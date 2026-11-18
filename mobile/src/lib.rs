pub mod device;
pub mod graphics;
pub mod links;
pub mod look;
pub mod state;
pub mod storage;
pub mod time;
pub mod ui;

#[cfg(target_os = "android")]
mod android;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("drostje"),
    );
    gpui_mobile::android::jni::install_panic_hook();
    let _platform = gpui_mobile::android::jni::init_platform(&app);
    let Some(shared) = gpui_mobile::android::jni::shared_platform() else {
        log::error!("The Android platform did not start");
        return;
    };
    let storage = storage::Storage::new(android::files_dir());
    android::choose_graphics(&storage);
    gpui_kit::Application::with_platform(shared.into_rc())
        .with_assets(gpui_kit::assets::AllAssets::new(""))
        .run(move |cx| {
            ui::open(
                storage,
                gpui_kit::WindowOptions {
                    window_bounds: None,
                    ..Default::default()
                },
                cx,
            )
        });
}
