use std::{
    ffi::{CStr, c_void},
    ptr::NonNull,
    sync::{Arc, OnceLock},
};

#[cfg(not(target_os = "linux"))]
use std::ffi::CString;

use crate::clipboard::{ClipboardApproval, ClipboardApprovalCallback, ClipboardOperation};
use async_channel::{Receiver, Sender};

#[derive(Clone, Debug)]
pub(crate) struct ProcessInfo {
    pub root: u32,
    pub foreground: u32,
    pub login_wrapper: bool,
    pub expected_program: Arc<CStr>,
}

unsafe extern "C" {
    fn gpui_ghostty_process_activity(
        root: u32,
        foreground: u32,
        login_wrapper: bool,
        expected_program: *const std::ffi::c_char,
    ) -> std::ffi::c_int;
}

impl ProcessInfo {
    fn from_raw(
        root: u64,
        foreground: u64,
        login_wrapper: bool,
        expected_program: &Arc<CStr>,
    ) -> Option<Self> {
        let root = u32::try_from(root).ok()?;
        let foreground = u32::try_from(foreground).ok()?;
        if root == 0
            || foreground == 0
            || root > i32::MAX as u32
            || foreground > i32::MAX as u32
            || expected_program.to_bytes().is_empty()
        {
            return None;
        }
        Some(Self {
            root,
            foreground,
            login_wrapper,
            expected_program: Arc::clone(expected_program),
        })
    }

    pub fn needs_confirm_quit(self) -> bool {
        // SAFETY: Process identities cross by value; the program is a valid
        // NUL-terminated string alive through the call. The shim retains no
        // pointers and returns zero only when it can establish an idle shell.
        unsafe {
            gpui_ghostty_process_activity(
                self.root,
                self.foreground,
                self.login_wrapper,
                self.expected_program.as_ptr(),
            ) != 0
        }
    }
}

struct NativeCallbacks {
    sender: Sender<()>,
    approval: OnceLock<ClipboardApprovalCallback>,
}

#[derive(Clone)]
pub struct NativeWakeup {
    callbacks: Arc<NativeCallbacks>,
    receiver: Receiver<()>,
}

impl NativeWakeup {
    fn new() -> Self {
        let (sender, receiver) = async_channel::bounded(1);
        Self {
            callbacks: Arc::new(NativeCallbacks {
                sender,
                approval: OnceLock::new(),
            }),
            receiver,
        }
    }

    pub async fn wait(&self) {
        let _ = self.receiver.recv().await;
    }

    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn signal(&self) {
        let _ = self.callbacks.sender.try_send(());
    }

    pub fn init_clipboard_approval(&self, callback: Option<ClipboardApprovalCallback>) {
        if let Some(callback) = callback {
            let _ = self.callbacks.approval.set(callback);
        }
    }

    fn userdata(&self) -> *mut c_void {
        Arc::as_ptr(&self.callbacks).cast_mut().cast()
    }
}

unsafe extern "C" fn native_wakeup(userdata: *mut c_void) {
    let Some(sender) = NonNull::new(userdata.cast::<NativeCallbacks>()) else {
        return;
    };
    // SAFETY: NativeSurface keeps the Arc allocation alive until native teardown completes.
    let _ = unsafe { sender.as_ref() }.sender.try_send(());
}

unsafe extern "C" fn native_clipboard_approval(
    userdata: *mut c_void,
    operation: i32,
    text: *const std::ffi::c_char,
) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(callbacks) = NonNull::new(userdata.cast::<NativeCallbacks>()) else {
            return false;
        };
        if text.is_null() {
            return false;
        }
        let operation = match operation {
            0 => ClipboardOperation::Paste,
            1 => ClipboardOperation::Read,
            2 => ClipboardOperation::Write,
            _ => return false,
        };
        // SAFETY: NativeSurface owns userdata through teardown; the shim provides
        // a NUL-terminated string valid for this callback only.
        let callbacks = unsafe { callbacks.as_ref() };
        let callback = callbacks.approval.get();
        let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
        callback.is_some_and(|callback| {
            callback(ClipboardApproval {
                operation,
                text: &text,
            })
        })
    }))
    .unwrap_or(false)
}

#[derive(Clone, Copy, PartialEq)]
struct NativeFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale_factor: f64,
}

impl NativeFrame {
    fn new(x: f64, y: f64, width: f64, height: f64, scale_factor: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
            scale_factor,
        }
    }
}

struct NativeSurfaceState {
    frame: Option<NativeFrame>,
    visible: bool,
    requested_visible: bool,
}

impl Default for NativeSurfaceState {
    fn default() -> Self {
        Self {
            frame: None,
            visible: false,
            requested_visible: true,
        }
    }
}

pub(crate) struct NativeSnapshot {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bgra: Vec<u8>,
}

const SNAPSHOT_ATTEMPTS: usize = 3;

impl NativeSnapshot {
    /// # Safety
    ///
    /// `read` writes at most `capacity` bytes to `pixels`, and when it returns
    /// true with `0 < length <= capacity` it has initialized `length` bytes there.
    pub(crate) unsafe fn capture(
        capacity: &mut usize,
        bottom_up: bool,
        mut read: impl FnMut(*mut u8, usize, &mut u32, &mut u32, &mut usize) -> bool,
    ) -> Result<Option<Self>, String> {
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let mut bgra = Vec::<u8>::with_capacity(*capacity);
            let (mut width, mut height, mut length) = (0, 0, 0);
            if !read(
                bgra.as_mut_ptr(),
                bgra.capacity(),
                &mut width,
                &mut height,
                &mut length,
            ) {
                return Err("capture native terminal frame".to_owned());
            }
            if length == 0 {
                return Ok(None);
            }
            let row_length = snapshot_row_length(width, height, length)?;
            *capacity = length;
            if length > bgra.capacity() {
                continue;
            }
            // SAFETY: `read` succeeded with `length <= capacity`, so by this
            // function's contract it initialized the first `length` bytes.
            unsafe { bgra.set_len(length) };
            if bottom_up {
                flip_bgra_rows(&mut bgra, row_length);
            }
            return Ok(Some(Self {
                width,
                height,
                bgra,
            }));
        }
        Err("native terminal frame kept resizing during capture".to_owned())
    }
}

fn snapshot_row_length(width: u32, height: u32, length: usize) -> Result<usize, String> {
    let row_length = usize::try_from(width)
        .ok()
        .filter(|width| *width > 0)
        .and_then(|width| width.checked_mul(4));
    let expected = row_length.and_then(|row| {
        usize::try_from(height)
            .ok()
            .filter(|height| *height > 0)
            .and_then(|height| row.checked_mul(height))
    });
    match (row_length, expected) {
        (Some(row_length), Some(expected)) if expected == length => Ok(row_length),
        _ => Err("native terminal snapshot has invalid dimensions".to_owned()),
    }
}

fn flip_bgra_rows(pixels: &mut [u8], row_length: usize) {
    let rows = pixels.len() / row_length;
    for row in 0..rows / 2 {
        let opposite = rows - row - 1;
        let (before, after) = pixels.split_at_mut(opposite * row_length);
        before[row * row_length..(row + 1) * row_length].swap_with_slice(&mut after[..row_length]);
    }
}

impl NativeSurfaceState {
    fn frame_changed(&self, frame: NativeFrame) -> bool {
        self.frame != Some(frame)
    }

    fn commit_frame(&mut self, frame: NativeFrame) {
        self.frame = Some(frame);
    }

    fn update_visibility(&mut self, visible: bool) -> bool {
        self.requested_visible = visible;
        let visible = visible && self.frame.is_some();
        if self.visible == visible {
            return false;
        }
        self.visible = visible;
        true
    }
}

#[cfg(target_os = "macos")]
use std::{
    ffi::{c_char, c_int},
    marker::PhantomData,
    rc::Rc,
};

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    #[repr(C)]
    struct RawSurface {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        fn gpui_ghostty_surface_new(
            parent_view: *mut c_void,
            working_directory: *const c_char,
            argv: *const *const c_char,
            argc: usize,
            theme: *const u8,
            wakeup_userdata: *mut c_void,
            wakeup: unsafe extern "C" fn(*mut c_void),
            approve_clipboard: unsafe extern "C" fn(*mut c_void, i32, *const c_char) -> bool,
        ) -> *mut RawSurface;
        fn gpui_ghostty_surface_free(surface: *mut RawSurface);
        fn gpui_ghostty_surface_tick(surface: *mut RawSurface);
        fn gpui_ghostty_surface_process_info(
            surface: *const RawSurface,
            root: *mut u64,
            foreground: *mut u64,
            login_wrapper: *mut bool,
        ) -> bool;
        fn gpui_ghostty_surface_expected_program(surface: *const RawSurface) -> *const c_char;
        fn gpui_ghostty_surface_is_alive(surface: *const RawSurface) -> bool;
        fn gpui_ghostty_surface_snapshot(
            surface: *mut RawSurface,
            pixels: *mut u8,
            capacity: usize,
            width: *mut u32,
            height: *mut u32,
            length: *mut usize,
        ) -> bool;
        fn gpui_ghostty_surface_set_frame(
            surface: *mut RawSurface,
            x: f64,
            y: f64,
            width: f64,
            height: f64,
        );
        fn gpui_ghostty_surface_set_visible(surface: *mut RawSurface, visible: bool);
        fn gpui_ghostty_surface_set_focus(surface: *mut RawSurface, focused: bool);
        fn gpui_ghostty_surface_key(
            surface: *mut RawSurface,
            action: c_int,
            modifiers: c_int,
            consumed_modifiers: c_int,
            keycode: u32,
            text: *const c_char,
            unshifted_codepoint: u32,
        ) -> bool;
        fn gpui_ghostty_surface_text(surface: *mut RawSurface, text: *const c_char, length: usize);
        fn gpui_ghostty_surface_preedit(
            surface: *mut RawSurface,
            text: *const c_char,
            length: usize,
        );
        fn gpui_ghostty_surface_ime_point(
            surface: *mut RawSurface,
            x: *mut f64,
            y: *mut f64,
            w: *mut f64,
            h: *mut f64,
        );
        fn gpui_ghostty_surface_theme(surface: *mut RawSurface, theme: *const u8);
        fn gpui_ghostty_surface_mouse_position(
            surface: *mut RawSurface,
            x: f64,
            y: f64,
            modifiers: c_int,
        );
        fn gpui_ghostty_surface_mouse_button(
            surface: *mut RawSurface,
            state: c_int,
            button: c_int,
            modifiers: c_int,
        );
        fn gpui_ghostty_surface_mouse_scroll(
            surface: *mut RawSurface,
            x: f64,
            y: f64,
            modifiers: c_int,
        );
    }

    pub struct NativeSurface {
        raw: NonNull<RawSurface>,
        wakeup: NativeWakeup,
        state: NativeSurfaceState,
        expected_program: Option<Arc<CStr>>,
        snapshot_capacity: usize,
        _main_thread: PhantomData<Rc<()>>,
    }

    impl NativeSurface {
        pub fn new(
            _display: Option<NonNull<c_void>>,
            parent_view: NonNull<c_void>,
            _scale_factor: f64,
            working_directory: CString,
            argv: &[CString],
            theme: Option<&[u8; 54]>,
        ) -> Result<Self, &'static str> {
            let wakeup = NativeWakeup::new();
            let argv = argv.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
            // SAFETY: The C shim validates creation failures. The parent pointer and
            // main-thread lifetime requirements are this method's documented boundary.
            let raw = unsafe {
                gpui_ghostty_surface_new(
                    parent_view.as_ptr(),
                    working_directory.as_ptr(),
                    argv.as_ptr(),
                    argv.len(),
                    theme.map_or(std::ptr::null(), |theme| theme.as_ptr()),
                    wakeup.userdata(),
                    native_wakeup,
                    native_clipboard_approval,
                )
            };
            let raw = NonNull::new(raw).ok_or("libghostty could not create a terminal surface")?;
            // SAFETY: The getter borrows an immutable NUL-terminated path from
            // this live surface; copy it before the surface can be destroyed.
            let expected_program = unsafe {
                let program = gpui_ghostty_surface_expected_program(raw.as_ptr());
                if program.is_null() {
                    None
                } else {
                    Some(Arc::<CStr>::from(CStr::from_ptr(program)))
                }
            };
            Ok(Self {
                raw,
                wakeup,
                state: NativeSurfaceState::default(),
                expected_program,
                snapshot_capacity: 0,
                _main_thread: PhantomData,
            })
        }

        pub fn wakeup(&self) -> NativeWakeup {
            self.wakeup.clone()
        }

        pub fn tick(&mut self) {
            // SAFETY: `raw` is owned by this value and calls stay on the main thread.
            unsafe { gpui_ghostty_surface_tick(self.raw.as_ptr()) }
        }

        pub fn is_alive(&self) -> bool {
            // SAFETY: `raw` remains valid for this value's lifetime.
            unsafe { gpui_ghostty_surface_is_alive(self.raw.as_ptr()) }
        }
        pub fn process_info(&self) -> Option<ProcessInfo> {
            let (mut root, mut foreground) = (0, 0);
            let mut login_wrapper = false;
            // SAFETY: `raw` remains live on the main thread, and all output
            // pointers refer to writable stack values for this call only.
            let available = unsafe {
                gpui_ghostty_surface_process_info(
                    self.raw.as_ptr(),
                    &mut root,
                    &mut foreground,
                    &mut login_wrapper,
                )
            };
            if !available {
                return None;
            }
            ProcessInfo::from_raw(
                root,
                foreground,
                login_wrapper,
                self.expected_program.as_ref()?,
            )
        }

        pub fn snapshot(&mut self) -> Result<Option<NativeSnapshot>, String> {
            let raw = self.raw;
            let read = |pixels: *mut u8,
                        capacity: usize,
                        width: &mut u32,
                        height: &mut u32,
                        length: &mut usize| {
                // SAFETY: `raw` is live on the AppKit main thread, the outputs are
                // writable for this call and `pixels` has `capacity` writable bytes.
                unsafe {
                    gpui_ghostty_surface_snapshot(
                        raw.as_ptr(),
                        pixels,
                        capacity,
                        width,
                        height,
                        length,
                    )
                }
            };
            // SAFETY: The shim writes at most `capacity` bytes and copies the whole
            // `length`-byte frame whenever it succeeds with a buffer that fits.
            unsafe { NativeSnapshot::capture(&mut self.snapshot_capacity, false, read) }
        }

        pub fn set_frame(&mut self, x: f64, y: f64, width: f64, height: f64, scale_factor: f64) {
            let frame = NativeFrame::new(x, y, width, height, scale_factor);
            if !self.state.frame_changed(frame) {
                return;
            }
            // SAFETY: `raw` is valid and geometry values cross the C boundary by value.
            unsafe { gpui_ghostty_surface_set_frame(self.raw.as_ptr(), x, y, width, height) }
            self.state.commit_frame(frame);
            self.set_visible(self.state.requested_visible);
        }

        pub fn set_visible(&mut self, visible: bool) {
            if self.state.update_visibility(visible) {
                // SAFETY: `raw` is valid and this is called from the AppKit main thread.
                unsafe { gpui_ghostty_surface_set_visible(self.raw.as_ptr(), self.state.visible) }
            }
        }

        pub fn set_focus(&mut self, focused: bool) {
            // SAFETY: `raw` is valid and this is called from the AppKit main thread.
            unsafe { gpui_ghostty_surface_set_focus(self.raw.as_ptr(), focused) }
        }

        pub fn key(
            &mut self,
            action: KeyAction,
            modifiers: Modifiers,
            consumed_modifiers: Modifiers,
            keycode: u32,
            text: Option<&CStr>,
            unshifted_codepoint: u32,
        ) -> bool {
            // SAFETY: Optional text remains valid for the duration of the call and
            // all integer values match the C shim's stable adapter ABI.
            unsafe {
                gpui_ghostty_surface_key(
                    self.raw.as_ptr(),
                    action as c_int,
                    modifiers.bits(),
                    consumed_modifiers.bits(),
                    keycode,
                    text.map_or(std::ptr::null(), CStr::as_ptr),
                    unshifted_codepoint,
                )
            }
        }

        pub fn text(&mut self, text: &CStr) {
            // SAFETY: The bytes remain valid for the duration of the call.
            unsafe {
                gpui_ghostty_surface_text(self.raw.as_ptr(), text.as_ptr(), text.to_bytes().len())
            }
        }

        pub fn preedit(&mut self, text: &CStr) {
            // SAFETY: Text is borrowed only for this call; the surface is live.
            unsafe {
                gpui_ghostty_surface_preedit(
                    self.raw.as_ptr(),
                    text.as_ptr(),
                    text.to_bytes().len(),
                )
            }
        }

        pub fn ime_point(&self) -> (f64, f64, f64, f64) {
            let (mut x, mut y, mut w, mut h) = (0.0, 0.0, 1.0, 1.0);
            // SAFETY: The shim writes four valid stack-owned output values.
            unsafe {
                gpui_ghostty_surface_ime_point(self.raw.as_ptr(), &mut x, &mut y, &mut w, &mut h)
            }
            (x, y, w, h)
        }

        pub fn update_theme(&mut self, theme: &[u8; 54]) {
            // SAFETY: The fixed array contains exactly the RGB values expected by the shim.
            unsafe { gpui_ghostty_surface_theme(self.raw.as_ptr(), theme.as_ptr()) }
        }

        pub fn mouse_position(&mut self, x: f64, y: f64, modifiers: Modifiers) {
            // SAFETY: Values cross the adapter ABI by value.
            unsafe {
                gpui_ghostty_surface_mouse_position(self.raw.as_ptr(), x, y, modifiers.bits())
            }
        }

        pub fn mouse_button(
            &mut self,
            state: MouseState,
            button: MouseButton,
            modifiers: Modifiers,
        ) {
            // SAFETY: Values cross the adapter ABI by value.
            unsafe {
                gpui_ghostty_surface_mouse_button(
                    self.raw.as_ptr(),
                    state as c_int,
                    button as c_int,
                    modifiers.bits(),
                )
            }
        }

        pub fn take_clipboard_read(&mut self) -> Option<ClipboardRead> {
            None
        }

        pub fn complete_clipboard_read(&mut self, _request: ClipboardRead, _text: &CStr) {}

        pub fn take_clipboard_write(&mut self) -> Option<ClipboardWrite> {
            None
        }

        pub fn mouse_scroll(&mut self, x: f64, y: f64, precision: bool) {
            let scroll_flags = i32::from(precision);
            // SAFETY: Values cross the adapter ABI by value.
            unsafe { gpui_ghostty_surface_mouse_scroll(self.raw.as_ptr(), x, y, scroll_flags) }
        }
    }

    impl Drop for NativeSurface {
        fn drop(&mut self) {
            // SAFETY: This value uniquely owns `raw` and drops on the AppKit main thread.
            unsafe { gpui_ghostty_surface_free(self.raw.as_ptr()) }
        }
    }
}

#[cfg(target_os = "macos")]
pub use platform::NativeSurface;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
pub use linux::NativeSurface;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("ghostty-terminal supports macOS and Linux Wayland only");

pub struct ClipboardRead {
    #[cfg(target_os = "linux")]
    pub selection: bool,
    #[cfg(target_os = "linux")]
    pub request: NonNull<c_void>,
}

pub struct ClipboardWrite {
    #[cfg(target_os = "linux")]
    pub selection: bool,
    pub text: String,
}

#[derive(Clone, Copy)]
#[repr(i32)]
pub enum KeyAction {
    Release = 0,
    Press = 1,
    Repeat = 2,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Modifiers(i32);

impl Modifiers {
    pub const SHIFT: Self = Self(1 << 0);
    pub const CONTROL: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);
    pub const SUPER: Self = Self(1 << 3);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    const fn bits(self) -> i32 {
        self.0
    }
}

#[derive(Clone, Copy)]
#[repr(i32)]
pub enum MouseState {
    Release = 0,
    Press = 1,
}

#[derive(Clone, Copy)]
#[repr(i32)]
pub enum MouseButton {
    Unknown = 0,
    Left = 1,
    Right = 2,
    Middle = 3,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_info_rejects_unknown_and_out_of_range_identities() {
        let expected_program = Arc::<CStr>::from(c"/bin/sh");
        let info = ProcessInfo::from_raw(1, i32::MAX as u64, false, &expected_program).unwrap();
        assert_eq!(info.root, 1);
        assert_eq!(info.foreground, i32::MAX as u32);
        assert!(!info.login_wrapper);
        assert!(
            ProcessInfo::from_raw(1, 2, true, &expected_program)
                .unwrap()
                .login_wrapper
        );
        assert!(Arc::ptr_eq(&info.expected_program, &expected_program));
        assert!(ProcessInfo::from_raw(1, 2, false, &Arc::<CStr>::from(c"")).is_none());
        for (root, foreground) in [
            (0, 1),
            (1, 0),
            (i32::MAX as u64 + 1, 1),
            (1, i32::MAX as u64 + 1),
            (u64::MAX, 1),
            (1, u64::MAX),
        ] {
            assert!(ProcessInfo::from_raw(root, foreground, false, &expected_program).is_none());
        }
    }

    #[test]
    fn explicit_visibility_survives_layout_and_resize() {
        let mut state = NativeSurfaceState::default();
        assert!(!state.update_visibility(false));
        let first = NativeFrame::new(0.0, 0.0, 100.0, 100.0, 1.0);
        state.commit_frame(first);
        assert!(!state.update_visibility(state.requested_visible));
        assert!(!state.visible);
        assert!(!state.frame_changed(first));
        state.commit_frame(NativeFrame::new(10.0, 10.0, 200.0, 200.0, 2.0));
        assert!(!state.update_visibility(state.requested_visible));
        assert!(!state.visible);
        assert!(state.update_visibility(true));
        assert!(state.visible);
        assert!(state.update_visibility(false));
        assert!(!state.visible);
    }

    #[test]
    fn native_surface_is_only_revealed_after_its_first_frame() {
        let mut state = NativeSurfaceState::default();
        assert!(!state.update_visibility(true));
        assert!(!state.visible);
        state.commit_frame(NativeFrame::new(0.0, 0.0, 100.0, 100.0, 1.0));
        assert!(state.update_visibility(state.requested_visible));
        assert!(state.visible);
    }

    fn capture_2x2(
        capacity: &mut usize,
        reads: &mut usize,
    ) -> Result<Option<NativeSnapshot>, String> {
        let rows: [[u8; 8]; 2] = [[1; 8], [2; 8]];
        let read = |pixels: *mut u8,
                    room: usize,
                    width: &mut u32,
                    height: &mut u32,
                    length: &mut usize| {
            *reads += 1;
            (*width, *height, *length) = (2, 2, 16);
            if room >= 16 {
                // SAFETY: `pixels` has `room >= 16` writable bytes.
                let out = unsafe { std::slice::from_raw_parts_mut(pixels, 16) };
                out.copy_from_slice(rows.as_flattened());
            }
            true
        };
        // SAFETY: `read` writes 16 bytes only when 16 fit, and then all of them.
        unsafe { NativeSnapshot::capture(capacity, true, read) }
    }

    #[test]
    fn capture_reads_into_an_exact_fit_buffer_and_flips_bottom_up_rows() {
        let (mut capacity, mut reads) = (0, 0);
        let first = capture_2x2(&mut capacity, &mut reads).unwrap().unwrap();
        assert_eq!((first.width, first.height), (2, 2));
        assert_eq!(first.bgra, [[2; 8], [1; 8]].as_flattened());
        assert_eq!((capacity, reads), (16, 2), "first capture learns the size");

        let second = capture_2x2(&mut capacity, &mut reads).unwrap().unwrap();
        assert_eq!(second.bgra, first.bgra);
        assert_eq!(reads, 3, "steady state reads once");
    }

    #[test]
    fn capture_reports_pending_failed_invalid_and_unsettled_frames() {
        let mut capacity = 0;
        let mut capture = |width: u32, length: usize, ok: bool| {
            let read = |_: *mut u8, _: usize, w: &mut u32, h: &mut u32, l: &mut usize| {
                (*w, *h, *l) = (width, 1, length);
                ok
            };
            // SAFETY: `read` never reports a frame that fits, so it writes nothing.
            unsafe { NativeSnapshot::capture(&mut capacity, false, read) }
        };
        assert!(
            capture(0, 0, true).unwrap().is_none(),
            "first frame pending"
        );
        assert!(capture(1, 4, false).is_err(), "native failure");
        assert!(capture(2, 4, true).is_err(), "length disagrees with size");

        let mut capacity = 0;
        let growing = |_: *mut u8, room: usize, w: &mut u32, h: &mut u32, l: &mut usize| {
            *w = u32::try_from(room / 4 + 1).unwrap();
            (*h, *l) = (1, *w as usize * 4);
            true
        };
        // SAFETY: `growing` always reports more than `room`, so it writes nothing.
        let result = unsafe { NativeSnapshot::capture(&mut capacity, false, growing) };
        assert!(result.is_err());
    }

    fn approve(wakeup: &NativeWakeup, operation: i32, text: &std::ffi::CStr) -> bool {
        // SAFETY: Both userdata and text remain live for this synchronous call.
        unsafe { native_clipboard_approval(wakeup.userdata(), operation, text.as_ptr()) }
    }

    #[test]
    fn clipboard_policy_receives_operation_and_text() {
        for (code, operation) in [
            (0, ClipboardOperation::Paste),
            (1, ClipboardOperation::Read),
            (2, ClipboardOperation::Write),
        ] {
            let wakeup = NativeWakeup::new();
            wakeup.init_clipboard_approval(Some(Arc::new(move |request| {
                request.operation == operation && request.text == "approved"
            })));
            assert!(approve(&wakeup, code, c"approved"));
            assert!(!approve(&wakeup, code, c"secret"));
            assert!(!approve(&wakeup, 99, c"approved"));
        }
    }

    #[test]
    fn missing_or_panicking_clipboard_policy_denies_access() {
        let wakeup = NativeWakeup::new();
        assert!(!approve(&wakeup, 1, c"secret"));
        wakeup.init_clipboard_approval(Some(Arc::new(|_| panic!("policy failure"))));
        assert!(!approve(&wakeup, 1, c"secret"));
    }

    #[test]
    fn native_wakeup_coalesces_duplicate_signals() {
        let wakeup = NativeWakeup::new();
        wakeup.signal();
        wakeup.signal();

        assert_eq!(wakeup.receiver.try_recv(), Ok(()));
        assert_eq!(
            wakeup.receiver.try_recv(),
            Err(async_channel::TryRecvError::Empty)
        );
    }
}
