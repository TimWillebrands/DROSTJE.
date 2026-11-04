use crate::clipboard::ClipboardApprovalCallback;
use crate::input_surface::{native_input_context, native_text_key};
use crate::native::{
    KeyAction, Modifiers as NativeModifiers, MouseButton as NativeMouseButton, MouseState,
    NativeSurface,
};
use gpui_kit::*;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::{
    cell::RefCell,
    ffi::{CString, OsString, c_void},
    ops::Range,
    os::unix::ffi::OsStrExt as _,
    path::PathBuf,
    ptr::NonNull,
    rc::Rc,
    sync::Arc,
};

/// An opaque terminal color. Native configuration receives packed RGB bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}
impl TerminalColor {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalTheme {
    pub background: TerminalColor,
    pub foreground: TerminalColor,
    pub palette: [TerminalColor; 16],
}
impl TerminalTheme {
    pub const fn new(
        background: TerminalColor,
        foreground: TerminalColor,
        palette: [TerminalColor; 16],
    ) -> Self {
        Self {
            background,
            foreground,
            palette,
        }
    }
    fn packed(self) -> [u8; 54] {
        let mut bytes = [0; 54];
        for (color, bytes) in [self.background, self.foreground]
            .iter()
            .chain(self.palette.iter())
            .zip(bytes.chunks_exact_mut(3))
        {
            bytes.copy_from_slice(&[color.r, color.g, color.b]);
        }
        bytes
    }
}

/// Commands are executable + argv, never shell source. With no program, Ghostty
/// resolves the account's default shell and starts its normal interactive session.
#[non_exhaustive]
pub struct TerminalOptions {
    pub working_directory: PathBuf,
    pub program: Option<PathBuf>,
    pub args: Vec<OsString>,
    pub focus_on_spawn: bool,
    pub theme: Option<TerminalTheme>,
    pub clipboard_approval: Option<ClipboardApprovalCallback>,
}
impl TerminalOptions {
    pub fn new(working_directory: impl Into<PathBuf>) -> Self {
        Self {
            working_directory: working_directory.into(),
            program: None,
            args: Vec::new(),
            focus_on_spawn: true,
            theme: None,
            clipboard_approval: None,
        }
    }
}

/// Exit means Ghostty observed the actual child exit, not a successful command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalStatus {
    Running,
    Exited,
    Closed,
}

type SurfaceOwner = Rc<RefCell<Option<NativeSurface>>>;

/// Retained by GPUI's frame state, not by Terminal. Losing the drawn element
/// drops this lease and occludes the backend even if tab owners retain entities.
struct FrameLease(SurfaceOwner);
impl Drop for FrameLease {
    fn drop(&mut self) {
        if let Some(surface) = self.0.borrow_mut().as_mut() {
            surface.set_focus(false);
            surface.set_visible(false);
        }
    }
}

/// Owns a real libghostty child and native GPU renderer. `close` takes the native
/// resource out of its shared cell, so frame leases and cloned entities cannot
/// postpone process teardown. The wakeup task holds only a WeakEntity.
pub struct Terminal {
    surface: SurfaceOwner,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    tick_task: Option<Task<()>>,
    visible: bool,
    window_focused: bool,
    composition: String,
    selection: Range<usize>,
    backend_error: Option<String>,
    _subscriptions: Vec<Subscription>,
    frame: Option<Arc<RenderImage>>,
    frame_dirty: bool,
    default_shell: bool,
}

impl Terminal {
    /// Must run on the owning platform thread outside render. NSView/Metal and
    /// Wayland/EGL construction require it. Ghostty's own font/default-shell
    /// discovery is synchronous here; application config/file I/O is not added.
    /// Unsupported window handles fail before any native process is started.
    pub fn spawn<T: 'static>(
        options: TerminalOptions,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Result<Entity<Self>, String> {
        let native_window = native_window(window)?;
        if !options.working_directory.is_absolute() {
            return Err("Terminal working directory must be absolute".into());
        }
        if options.program.is_none() && !options.args.is_empty() {
            return Err("Terminal arguments require an executable".into());
        }
        let cwd = CString::new(options.working_directory.as_os_str().as_bytes())
            .map_err(|_| "Terminal directory contains a NUL byte")?;
        let default_shell = options.program.is_none();
        let mut argv =
            Vec::with_capacity(options.args.len() + usize::from(options.program.is_some()));
        if let Some(program) = options.program {
            if program.as_os_str().is_empty() {
                return Err("Terminal executable is empty".into());
            }
            argv.push(
                CString::new(program.as_os_str().as_bytes())
                    .map_err(|_| "Terminal executable contains a NUL byte")?,
            );
            for arg in options.args {
                argv.push(
                    CString::new(arg.as_bytes())
                        .map_err(|_| "Terminal argument contains a NUL byte")?,
                );
            }
        }
        let theme = options.theme.map(TerminalTheme::packed);
        let surface = NativeSurface::new(
            native_window.display,
            native_window.surface,
            f64::from(window.scale_factor()),
            cwd,
            &argv,
            theme.as_ref(),
        )
        .map_err(|error| format!("Initialize Ghostty: {error}"))?;
        surface
            .wakeup()
            .init_clipboard_approval(options.clipboard_approval);
        let focus = cx.focus_handle();
        let terminal = cx.new(|cx| {
            let subscriptions = vec![
                cx.on_focus(&focus, window, |terminal: &mut Self, window, _| {
                    terminal.sync_focus(window)
                }),
                cx.on_blur(&focus, window, |terminal: &mut Self, window, _| {
                    terminal.sync_focus(window)
                }),
                cx.observe_window_activation(window, |terminal: &mut Self, window, _| {
                    terminal.sync_focus(window)
                }),
            ];
            let mut terminal = Self {
                surface: Rc::new(RefCell::new(Some(surface))),
                focus,
                bounds: Bounds::default(),
                tick_task: None,
                visible: true,
                window_focused: false,
                composition: String::new(),
                selection: 0..0,
                backend_error: None,
                _subscriptions: subscriptions,
                frame: None,
                frame_dirty: true,
                default_shell,
            };
            terminal.start_ticking(cx);
            terminal
        });
        if options.focus_on_spawn {
            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
        }
        Ok(terminal)
    }

    fn with_surface<R>(&self, f: impl FnOnce(&mut NativeSurface) -> R) -> Option<R> {
        self.surface.borrow_mut().as_mut().map(f)
    }
    pub fn status(&self) -> TerminalStatus {
        self.with_surface(|surface| {
            if surface.is_alive() {
                TerminalStatus::Running
            } else {
                TerminalStatus::Exited
            }
        })
        .unwrap_or(TerminalStatus::Closed)
    }
    /// Snapshot native identities on the platform thread; inspect processes off it.
    /// Missing process information remains conservative, without requiring shell integration.
    pub fn needs_confirm_quit(&self, cx: &App) -> Task<bool> {
        if self.status() != TerminalStatus::Running {
            return Task::ready(false);
        }
        if self.backend_error.is_some() || !self.default_shell {
            return Task::ready(true);
        }
        let Some(processes) = self
            .with_surface(|surface| surface.process_info())
            .flatten()
        else {
            return Task::ready(true);
        };
        cx.background_spawn(async move { processes.needs_confirm_quit() })
    }
    pub fn backend_error(&self) -> Option<&str> {
        self.backend_error.as_deref()
    }
    pub fn focus<T>(&mut self, window: &mut Window, cx: &mut Context<T>) {
        self.set_visible(true);
        self.focus.focus(window, cx);
        self.sync_focus(window);
    }
    /// Hiding pauses native drawing, not the shell. Showing is completed by paint.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        if !visible {
            self.with_surface(|surface| {
                surface.set_focus(false);
                surface.set_visible(false);
            });
        }
    }
    fn sync_focus(&mut self, window: &Window) {
        self.window_focused = self.focus.is_focused(window) && window.is_window_active();
        self.with_surface(|surface| surface.set_focus(self.visible && self.window_focused));
    }
    pub fn update_theme(&mut self, theme: TerminalTheme) {
        self.with_surface(|surface| surface.update_theme(&theme.packed()));
    }
    /// Explicit, idempotent teardown; cloned Entity and frame references are inert.
    pub fn close(&mut self) {
        self.tick_task = None;
        self.frame = None;
        self.visible = false;
        self.composition.clear();
        self.surface.borrow_mut().take();
    }
    /// Read Ghostty's completed GPU frame without exposing native ownership.
    pub fn snapshot(&mut self) -> Result<Arc<RenderImage>, String> {
        self.capture_frame()?
            .ok_or_else(|| "Terminal frame is not ready".into())
    }
    fn capture_frame(&mut self) -> Result<Option<Arc<RenderImage>>, String> {
        let Some(snapshot) = self
            .with_surface(|surface| surface.snapshot())
            .ok_or("Terminal is closed")??
        else {
            return Ok(None);
        };
        let image = image::RgbaImage::from_raw(snapshot.width, snapshot.height, snapshot.bgra)
            .ok_or("Native terminal snapshot has invalid dimensions")?;
        Ok(Some(Arc::new(RenderImage::new(smallvec::smallvec![
            image::Frame::new(image)
        ]))))
    }
    fn start_ticking(&mut self, cx: &mut Context<Self>) {
        let Some(wakeup) = self.with_surface(|surface| surface.wakeup()) else {
            return;
        };
        let terminal = cx.entity().downgrade();
        self.tick_task = Some(cx.spawn(async move |_, cx| {
            loop {
                wakeup.wait().await;
                if terminal
                    .update(cx, |terminal, cx| {
                        terminal.with_surface(|surface| surface.tick());
                        terminal.service_clipboard(cx);
                        terminal.frame_dirty = true;
                        // The native GPU frame is composed by GPUI, so render wakeups
                        // invalidate this entity. Parents can observe actual exit here.
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }
    fn service_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(request) = self
            .with_surface(|surface| surface.take_clipboard_read())
            .flatten()
        {
            #[cfg(target_os = "linux")]
            let item = if request.selection {
                cx.read_from_primary()
            } else {
                cx.read_from_clipboard()
            };
            #[cfg(not(target_os = "linux"))]
            let item = cx.read_from_clipboard();
            let text = item
                .and_then(|item| item.text())
                .unwrap_or_default()
                .replace('\0', "�");
            if let Ok(text) = CString::new(text) {
                self.with_surface(|surface| surface.complete_clipboard_read(request, &text));
            }
        }
        while let Some(write) = self
            .with_surface(|surface| surface.take_clipboard_write())
            .flatten()
        {
            let item = ClipboardItem::new_string(write.text);
            #[cfg(target_os = "linux")]
            if write.selection {
                cx.write_to_primary(item);
            } else {
                cx.write_to_clipboard(item);
            }
            #[cfg(not(target_os = "linux"))]
            cx.write_to_clipboard(item);
        }
    }
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.frame_dirty |= self.bounds != bounds;
        self.bounds = bounds;
        let visible = self.visible && !bounds.intersect(&window.content_mask().bounds).is_empty();
        if !visible {
            self.with_surface(|surface| surface.set_visible(false));
            return;
        }
        let surface = self.surface.clone();
        window.with_global_id("native-terminal-frame".into(), |id, window| {
            window.with_element_state(id, |lease: Option<FrameLease>, _| {
                ((), lease.unwrap_or_else(|| FrameLease(surface)))
            });
        });
        self.with_surface(|surface| {
            surface.set_frame(
                f64::from(f32::from(bounds.origin.x)),
                f64::from(f32::from(bounds.origin.y)),
                f64::from(f32::from(bounds.size.width)),
                f64::from(f32::from(bounds.size.height)),
                f64::from(window.scale_factor()),
            );
            surface.set_visible(true);
            surface.set_focus(self.window_focused);
        });
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, cx.entity()),
            cx,
        );
        // GPUI's macOS surface import only supports YUV video, not BGRA; its
        // Linux renderer has no EGL import. Compose cached native-renderer
        // readbacks on both platforms so clipping and overlays remain correct.
        if self.frame_dirty {
            match self.capture_frame() {
                Ok(Some(frame)) => {
                    self.frame = Some(frame);
                    self.backend_error = None;
                    self.frame_dirty = false;
                }
                // Metal installs its first completed frame asynchronously.
                Ok(None) => {}
                Err(error) => self.backend_error = Some(error),
            }
        }
        if let Some(frame) = self.frame.as_ref()
            && let Err(error) =
                window.paint_image(bounds, bounds, Default::default(), frame.clone(), 0, false)
        {
            self.backend_error = Some(error.to_string());
        }
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Text/IME enters exclusively through EntityInputHandler. Special and
        // modified keys go through Ghostty's key protocol, after GPUI actions.
        if !self.composition.is_empty() {
            return;
        }
        let key = &event.keystroke;
        if native_text_key(key) {
            return;
        }
        if self.send_key(
            if event.is_held {
                KeyAction::Repeat
            } else {
                KeyAction::Press
            },
            key,
        ) {
            window.prevent_default();
            cx.stop_propagation();
        }
    }
    fn send_key(&mut self, action: KeyAction, keystroke: &Keystroke) -> bool {
        let Some(key) = native_key(&keystroke.key) else {
            return false;
        };
        let text = keystroke
            .key_char
            .as_deref()
            .and_then(|text| CString::new(text).ok());
        let (active, consumed) =
            key_modifiers(keystroke.modifiers, key.implied_shift, text.is_some());
        self.with_surface(|surface| {
            surface.key(
                action,
                active,
                consumed,
                key.keycode,
                text.as_deref(),
                key.unshifted_codepoint,
            )
        })
        .unwrap_or(false)
    }
    fn mouse_position(&mut self, position: Point<Pixels>, modifiers: gpui_kit::Modifiers) {
        self.with_surface(|surface| {
            surface.mouse_position(
                f64::from(f32::from(position.x - self.bounds.origin.x)),
                f64::from(f32::from(position.y - self.bounds.origin.y)),
                modifiers.into(),
            )
        });
    }
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        self.sync_focus(window);
        self.mouse_position(event.position, event.modifiers);
        self.with_surface(|surface| {
            surface.mouse_button(
                MouseState::Press,
                event.button.into(),
                event.modifiers.into(),
            )
        });
    }
    fn mouse_up(&mut self, event: &MouseUpEvent) {
        self.mouse_position(event.position, event.modifiers);
        self.with_surface(|surface| {
            surface.mouse_button(
                MouseState::Release,
                event.button.into(),
                event.modifiers.into(),
            )
        });
    }
    fn scroll(&mut self, event: &ScrollWheelEvent) {
        self.mouse_position(event.position, event.modifiers);
        let (x, y, precision) = match event.delta {
            ScrollDelta::Pixels(delta) => (
                f64::from(f32::from(delta.x)),
                f64::from(f32::from(delta.y)),
                true,
            ),
            ScrollDelta::Lines(delta) => (f64::from(delta.x), f64::from(delta.y), false),
        };
        self.with_surface(|surface| surface.mouse_scroll(x, y, precision));
    }
    fn clear_composition(&mut self) {
        self.composition.clear();
        self.selection = 0..0;
        self.with_surface(|surface| surface.preedit(c""));
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        self.close();
    }
}
impl Focusable for Terminal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for Terminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_focus(window);
        let terminal = cx.entity().downgrade();
        div()
            .id("terminal-native")
            .key_context(native_input_context("Terminal"))
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        let _ =
                            terminal.update(cx, |terminal, cx| terminal.paint(bounds, window, cx));
                    },
                )
                .absolute()
                .size_full(),
            )
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(|terminal, event: &KeyUpEvent, _, _| {
                if terminal.composition.is_empty() && !native_text_key(&event.keystroke) {
                    terminal.send_key(KeyAction::Release, &event.keystroke);
                }
            }))
            .on_mouse_move(cx.listener(|terminal, event: &MouseMoveEvent, _, _| {
                terminal.mouse_position(event.position, event.modifiers)
            }))
            .on_mouse_down(gpui_kit::MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(gpui_kit::MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(gpui_kit::MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_up(
                gpui_kit::MouseButton::Left,
                cx.listener(|terminal, event, _, _| terminal.mouse_up(event)),
            )
            .on_mouse_up(
                gpui_kit::MouseButton::Middle,
                cx.listener(|terminal, event, _, _| terminal.mouse_up(event)),
            )
            .on_mouse_up(
                gpui_kit::MouseButton::Right,
                cx.listener(|terminal, event, _, _| terminal.mouse_up(event)),
            )
            .on_scroll_wheel(cx.listener(|terminal, event, _, _| terminal.scroll(event)))
    }
}

// Terminals do not expose editable scrollback as a document. The only native
// text range is the uncommitted IME preedit; committing sends it exactly once.
impl EntityInputHandler for Terminal {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let units: Vec<u16> = self.composition.encode_utf16().collect();
        let start = range.start.min(units.len());
        let end = range.end.max(start).min(units.len());
        *adjusted = Some(start..end);
        String::from_utf16(&units[start..end]).ok()
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.selection.clone(),
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.composition.is_empty()).then(|| 0..self.composition.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.clear_composition();
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.visible || self.status() != TerminalStatus::Running {
            return;
        }
        let text = if let Some(range) = range {
            let mut units: Vec<u16> = self.composition.encode_utf16().collect();
            if range.start > range.end || range.end > units.len() {
                return;
            }
            units.splice(range, text.encode_utf16());
            let Ok(text) = String::from_utf16(&units) else {
                return;
            };
            CString::new(text)
        } else {
            CString::new(text)
        };
        let Ok(text) = text else {
            return;
        };
        self.clear_composition();
        self.with_surface(|surface| surface.text(&text));
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.visible || self.status() != TerminalStatus::Running {
            return;
        }
        let mut units: Vec<u16> = self.composition.encode_utf16().collect();
        let range = range.unwrap_or(0..units.len());
        if range.start > range.end || range.end > units.len() {
            return;
        }
        units.splice(range.clone(), text.encode_utf16());
        let Ok(composition) = String::from_utf16(&units) else {
            return;
        };
        let Ok(preedit) = CString::new(composition.as_str()) else {
            return;
        };
        let end = composition.encode_utf16().count();
        self.selection = selection
            .map(|selection| {
                (range.start + selection.start).min(end)..(range.start + selection.end).min(end)
            })
            .unwrap_or(end..end);
        self.composition = composition;
        self.with_surface(|surface| surface.preedit(&preedit));
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.with_surface(|surface| {
            let (x, y, w, h) = surface.ime_point();
            Bounds::new(
                bounds.origin + point(px(x as f32), px(y as f32)),
                size(px(w as f32), px(h as f32)),
            )
        })
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.selection.start)
    }
    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.visible && self.status() == TerminalStatus::Running
    }
}

impl From<gpui_kit::Modifiers> for NativeModifiers {
    fn from(value: gpui_kit::Modifiers) -> Self {
        modifiers(value)
    }
}

impl From<gpui_kit::MouseButton> for NativeMouseButton {
    fn from(value: gpui_kit::MouseButton) -> Self {
        match value {
            gpui_kit::MouseButton::Left => Self::Left,
            gpui_kit::MouseButton::Right => Self::Right,
            gpui_kit::MouseButton::Middle => Self::Middle,
            gpui_kit::MouseButton::Navigate(_) => Self::Unknown,
        }
    }
}

struct NativeWindow {
    display: Option<NonNull<c_void>>,
    surface: NonNull<c_void>,
}

fn native_window(window: &Window) -> Result<NativeWindow, String> {
    let handle = raw_window_handle::HasWindowHandle::window_handle(window)
        .map_err(|error| format!("read native window handle: {error}"))?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(handle) => Ok(NativeWindow {
            display: None,
            surface: handle.ns_view,
        }),
        RawWindowHandle::Wayland(handle) => {
            let display = raw_window_handle::HasDisplayHandle::display_handle(window)
                .map_err(|error| format!("read native display handle: {error}"))?;
            let RawDisplayHandle::Wayland(display) = display.as_raw() else {
                return Err(
                    "GPUI returned mismatched Wayland window and display handles".to_owned(),
                );
            };
            Ok(NativeWindow {
                display: Some(display.display),
                surface: handle.surface,
            })
        }
        _ => Err("libghostty native surfaces require macOS or Wayland".to_owned()),
    }
}

fn modifiers(value: gpui_kit::Modifiers) -> NativeModifiers {
    let mut result = NativeModifiers::empty();
    if value.shift {
        result.insert(NativeModifiers::SHIFT);
    }
    if value.control {
        result.insert(NativeModifiers::CONTROL);
    }
    if value.alt {
        result.insert(NativeModifiers::ALT);
    }
    if value.platform {
        result.insert(NativeModifiers::SUPER);
    }
    result
}

fn key_modifiers(
    mut value: gpui_kit::Modifiers,
    implied_shift: bool,
    has_text: bool,
) -> (NativeModifiers, NativeModifiers) {
    value.shift |= implied_shift;
    let active = modifiers(value);
    let mut consumed = NativeModifiers::empty();
    if has_text && value.shift {
        consumed.insert(NativeModifiers::SHIFT);
    }
    (active, consumed)
}

struct NativeKey {
    keycode: u32,
    unshifted_codepoint: u32,
    implied_shift: bool,
}

fn native_key(key: &str) -> Option<NativeKey> {
    let (key, implied_shift) = unshifted_key(key);
    let unshifted_codepoint = match key {
        "space" => u32::from(' '),
        _ => single_codepoint(key).map_or(0, u32::from),
    };
    Some(NativeKey {
        keycode: native_keycode(key)?,
        unshifted_codepoint,
        implied_shift,
    })
}

fn single_codepoint(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let first = chars.next()?;
    chars.next().is_none().then_some(first)
}

fn unshifted_key(key: &str) -> (&str, bool) {
    match key {
        "!" => ("1", true),
        "@" => ("2", true),
        "#" => ("3", true),
        "$" => ("4", true),
        "%" => ("5", true),
        "^" => ("6", true),
        "&" => ("7", true),
        "*" => ("8", true),
        "(" => ("9", true),
        ")" => ("0", true),
        "_" => ("-", true),
        "+" => ("=", true),
        "{" => ("[", true),
        "}" => ("]", true),
        "|" => ("\\", true),
        ":" => (";", true),
        "\"" => ("'", true),
        "<" => (",", true),
        ">" => (".", true),
        "?" => ("/", true),
        "~" => ("`", true),
        _ => (key, false),
    }
}

#[cfg(target_os = "macos")]
fn native_keycode(key: &str) -> Option<u32> {
    // Native values mirror Ghostty's pinned macOS keycode table. GPUI does
    // not expose NSEvent.keyCode, so keys it collapses (notably the keypad)
    // cannot be distinguished here.
    Some(match key {
        // ANSI printable keys, ordered by macOS virtual keycode.
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "=" => 24,
        "9" => 25,
        "7" => 26,
        "-" => 27,
        "8" => 28,
        "0" => 29,
        "]" => 30,
        "o" => 31,
        "u" => 32,
        "[" => 33,
        "i" => 34,
        "p" => 35,
        "l" => 37,
        "j" => 38,
        "'" => 39,
        "k" => 40,
        ";" => 41,
        "\\" => 42,
        "," => 43,
        "/" => 44,
        "n" => 45,
        "m" => 46,
        "." => 47,
        "`" => 50,

        // Editing and navigation keys emitted by GPUI.
        "enter" | "return" => 36,
        "tab" => 48,
        "space" => 49,
        "backspace" => 51,
        "escape" => 53,
        "insert" => 114,
        "home" => 115,
        "pageup" | "page_up" | "page-up" => 116,
        "delete" => 117,
        "end" => 119,
        "pagedown" | "page_down" | "page-down" => 121,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,

        // Function keys available in Ghostty's macOS keycode table.
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        "f13" => 105,
        "f14" => 107,
        "f15" => 113,
        "f16" => 106,
        "f17" => 64,
        "f18" => 79,
        "f19" => 80,
        "f20" => 90,
        _ => return None,
    })
}

#[cfg(target_os = "linux")]
fn native_keycode(key: &str) -> Option<u32> {
    // XKB keycodes used by Ghostty's Linux key table (evdev codes plus 8).
    Some(match key {
        "escape" => 9,
        "1" => 10,
        "2" => 11,
        "3" => 12,
        "4" => 13,
        "5" => 14,
        "6" => 15,
        "7" => 16,
        "8" => 17,
        "9" => 18,
        "0" => 19,
        "-" => 20,
        "=" => 21,
        "backspace" => 22,
        "tab" => 23,
        "q" => 24,
        "w" => 25,
        "e" => 26,
        "r" => 27,
        "t" => 28,
        "y" => 29,
        "u" => 30,
        "i" => 31,
        "o" => 32,
        "p" => 33,
        "[" => 34,
        "]" => 35,
        "enter" | "return" => 36,
        "a" => 38,
        "s" => 39,
        "d" => 40,
        "f" => 41,
        "g" => 42,
        "h" => 43,
        "j" => 44,
        "k" => 45,
        "l" => 46,
        ";" => 47,
        "'" => 48,
        "`" => 49,
        "\\" => 51,
        "z" => 52,
        "x" => 53,
        "c" => 54,
        "v" => 55,
        "b" => 56,
        "n" => 57,
        "m" => 58,
        "," => 59,
        "." => 60,
        "/" => 61,
        "space" => 65,
        "f1" => 67,
        "f2" => 68,
        "f3" => 69,
        "f4" => 70,
        "f5" => 71,
        "f6" => 72,
        "f7" => 73,
        "f8" => 74,
        "f9" => 75,
        "f10" => 76,
        "f11" => 95,
        "f12" => 96,
        "f13" => 191,
        "f14" => 192,
        "f15" => 193,
        "f16" => 194,
        "f17" => 195,
        "f18" => 196,
        "f19" => 197,
        "f20" => 198,
        "home" => 110,
        "up" => 111,
        "pageup" | "page_up" | "page-up" => 112,
        "left" => 113,
        "right" => 114,
        "end" => 115,
        "down" => 116,
        "pagedown" | "page_down" | "page-down" => 117,
        "insert" => 118,
        "delete" => 119,
        _ => return None,
    })
}

#[cfg(test)]
mod input_tests {
    use crate::input_surface::native_text_key;
    use gpui_kit::Keystroke;

    #[test]
    fn return_with_native_text_remains_a_terminal_key_not_a_paste() {
        let enter = Keystroke::parse("enter").unwrap().with_simulated_ime();
        assert_eq!(enter.key_char.as_deref(), Some("\n"));
        assert!(!native_text_key(&enter));
        assert!(native_text_key(
            &Keystroke::parse("é").unwrap().with_simulated_ime()
        ));
        assert!(!native_text_key(&Keystroke::parse("ctrl-c").unwrap()));
    }
}
