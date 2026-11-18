#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

#[cfg(target_os = "android")]
pub use crate::android::{device_name, frame_drawn, insets, leave, start_scanner, system_bars};

#[cfg(not(target_os = "android"))]
pub fn start_scanner() -> Result<(), String> {
    Err("Scanning uses the phone camera. On a computer, open the pairing link instead".into())
}

#[cfg(not(target_os = "android"))]
pub fn leave() {}

#[cfg(not(target_os = "android"))]
pub fn device_name() -> String {
    "DROSTJE. phone app".into()
}

#[cfg(not(target_os = "android"))]
pub fn insets() -> Insets {
    Insets::default()
}

#[cfg(not(target_os = "android"))]
pub fn system_bars(_dark: bool) {}

#[cfg(not(target_os = "android"))]
pub fn frame_drawn(_storage: &crate::storage::Storage) {}
