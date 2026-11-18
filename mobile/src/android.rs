use std::{
    ffi::{CStr, CString, c_char},
    path::PathBuf,
    sync::Mutex,
};

use drostje_phone::qr::{LumaFrame, decode_frame};
use gpui_mobile::android::{
    host::{GpuBackend, GpuOutcome, on_gpu_outcome, set_gpu_backend},
    jni::{activity, android_app, get_string, platform, with_env},
};
use jni::sys::{JNIEnv, jclass, jint, jobject, jstring};

use crate::{
    graphics::{self, Backend, StartOn},
    storage::Storage,
};

unsafe fn java_string(env: *mut JNIEnv, value: jstring) -> Option<String> {
    if env.is_null() || value.is_null() {
        return None;
    }
    // SAFETY: the JVM passes a valid JNIEnv for this thread and a live jstring local reference.
    let table = unsafe { &**env };
    // SAFETY: `value` is a non-null jstring; a null isCopy pointer is allowed by the JNI spec.
    let chars = unsafe { (table.v1_1.GetStringUTFChars)(env, value, std::ptr::null_mut()) };
    if chars.is_null() {
        return None;
    }
    // SAFETY: GetStringUTFChars returns a NUL-terminated modified UTF-8 buffer that stays valid until released.
    let text = unsafe { CStr::from_ptr(chars) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: `chars` came from GetStringUTFChars on the same string and is released exactly once.
    unsafe { (table.v1_1.ReleaseStringUTFChars)(env, value, chars) };
    Some(text)
}

/// # Safety
/// Called by the JVM with a valid JNIEnv and a String argument.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_app_drostje_phone_MainActivity_nativeLink(
    env: *mut JNIEnv,
    _class: jclass,
    link: jstring,
) {
    // SAFETY: forwarded unchanged from the JVM call frame.
    if let Some(link) = unsafe { java_string(env, link) } {
        crate::links::deliver(link);
    }
}

/// # Safety
/// Called by the JVM with a valid JNIEnv and a direct ByteBuffer holding the luma plane.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_app_drostje_phone_ScanActivity_nativeDecode(
    env: *mut JNIEnv,
    _class: jclass,
    buffer: jobject,
    width: jint,
    height: jint,
    row_stride: jint,
) -> jstring {
    if env.is_null() || buffer.is_null() || width <= 0 || height <= 0 || row_stride < width {
        return std::ptr::null_mut();
    }
    // SAFETY: the JVM passes a valid JNIEnv for this thread.
    let table = unsafe { &**env };
    // SAFETY: `buffer` is a live direct ByteBuffer local reference owned by the camera frame.
    let address = unsafe { (table.v1_4.GetDirectBufferAddress)(env, buffer) } as *const u8;
    // SAFETY: as above; capacity is reported in bytes for the same buffer.
    let capacity = unsafe { (table.v1_4.GetDirectBufferCapacity)(env, buffer) };
    if address.is_null() || capacity <= 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the buffer memory stays valid and unmodified until the frame is closed after this call returns.
    let pixels = unsafe { std::slice::from_raw_parts(address, capacity as usize) };
    let frame = LumaFrame {
        width: width as usize,
        height: height as usize,
        row_stride: row_stride as usize,
        pixels,
    };
    let Some(text) = decode_frame(&frame).into_iter().next() else {
        return std::ptr::null_mut();
    };
    let Ok(text) = CString::new(text) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `text` is a NUL-terminated buffer that outlives the call; the JVM copies it into a new String.
    unsafe { (table.v1_1.NewStringUTF)(env, text.as_ptr() as *const c_char) }
}

pub fn start_scanner() -> Result<(), String> {
    with_env(|env| {
        let activity = activity(env)?;
        env.call_method(
            &activity,
            jni::jni_str!("startScanner"),
            jni::jni_sig!("()V"),
            &[],
        )
        .map(|_| ())
        .map_err(|error| format!("Could not open the camera: {error}"))
    })
}

pub fn leave() {
    let _ = with_env(|env| {
        let activity = activity(env)?;
        env.call_method(&activity, jni::jni_str!("leave"), jni::jni_sig!("()V"), &[])
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
}

pub fn device_name() -> String {
    with_env(|env| {
        let activity = activity(env)?;
        let name = env
            .call_method(
                &activity,
                jni::jni_str!("deviceName"),
                jni::jni_sig!("()Ljava/lang/String;"),
                &[],
            )
            .and_then(|value| value.l())
            .map_err(|error| error.to_string())?;
        Ok(get_string(env, &name))
    })
    .ok()
    .filter(|name| !name.trim().is_empty())
    .unwrap_or_else(|| "Android phone".into())
}

static DRAWING_ON: Mutex<Option<Backend>> = Mutex::new(None);

pub fn choose_graphics(storage: &Storage) {
    set_gpu_backend(match graphics::start(storage) {
        StartOn::VulkanThenGl => GpuBackend::Auto,
        StartOn::Gl => GpuBackend::Gl,
    });
    on_gpu_outcome(|outcome| {
        let backend = match outcome {
            GpuOutcome::Vulkan => Backend::Vulkan,
            GpuOutcome::Gl | GpuOutcome::GlAfterVulkanFailed => Backend::Gl,
            GpuOutcome::Failed => return,
        };
        *DRAWING_ON.lock().expect("graphics backend lock poisoned") = Some(backend);
    });
}

pub fn frame_drawn(storage: &Storage) {
    let Some(backend) = *DRAWING_ON.lock().expect("graphics backend lock poisoned") else {
        return;
    };
    let storage = storage.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("phone-graphics".into())
        .spawn(move || graphics::drew(&storage, backend))
    {
        log::warn!("Drawing on {backend:?} not recorded: {error}");
    }
}

pub fn files_dir() -> PathBuf {
    android_app()
        .and_then(|app| app.internal_data_path())
        .unwrap_or_else(std::env::temp_dir)
}

pub fn insets() -> crate::device::Insets {
    platform()
        .and_then(|platform| platform.primary_window())
        .map(|window| {
            let insets = window.safe_area_insets_logical();
            crate::device::Insets {
                top: insets.top,
                bottom: insets.bottom,
                left: insets.left,
                right: insets.right,
            }
        })
        .unwrap_or_default()
}

pub fn system_bars(dark: bool) {
    gpui_mobile::set_system_chrome(&gpui_mobile::SystemChromeStyle {
        status_bar_color: None,
        status_bar_style: if dark {
            gpui_mobile::StatusBarContentStyle::Light
        } else {
            gpui_mobile::StatusBarContentStyle::Dark
        },
        navigation_bar_color: None,
    });
}
