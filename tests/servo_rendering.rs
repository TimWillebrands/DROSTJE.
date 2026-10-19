//! Opt-in real Servo + GPUI Metal rendering/input regression, without provider turns.
fn main() {
    #[cfg(target_os = "macos")]
    macos::run();
    #[cfg(not(target_os = "macos"))]
    println!("servo_rendering: GPUI offscreen renderer is unavailable on this platform");
}

#[cfg(target_os = "macos")]
mod macos {
    use drostje::{
        browser::{BrowserCommand, BrowserRuntime},
        ui::{browser::BrowserPane, commands, theme},
    };
    use gpui_kit::{
        assets::AllAssets,
        component::{Root, Theme, ThemeMode},
        test::TestWindowExt,
        *,
    };
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use uuid::Uuid;

    struct Sample {
        pane: Entity<BrowserPane>,
        width: f32,
    }
    impl Render for Sample {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(self.width)).h(px(520.)).child(self.pane.clone())
        }
    }
    fn pump(cx: &mut HeadlessAppContext, window: WindowHandle<Root>) {
        cx.advance_clock(Duration::from_millis(40));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, w, cx| w.render_frame(cx))
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    fn until(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        label: &str,
        predicate: impl Fn() -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(45);
        while !predicate() {
            assert!(Instant::now() < deadline, "Timed out: {label}");
            pump(cx, window);
        }
        pump(cx, window);
    }
    fn evaluate(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        runtime: &BrowserRuntime,
        id: Uuid,
        script: &str,
    ) -> serde_json::Value {
        let request_id = Uuid::new_v4().to_string();
        runtime.command(
            id,
            BrowserCommand::Evaluate {
                request_id: request_id.clone(),
                script: script.into(),
            },
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = runtime.evaluation(&request_id) {
                return result.unwrap();
            }
            assert!(Instant::now() < deadline, "Evaluation timed out: {script}");
            pump(cx, window);
        }
    }
    pub fn run() {
        let directory = std::env::temp_dir().join(format!("drostje-servo-ui-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("index.html"), "<!doctype html><title>Servo UI proof</title><style>html,body{margin:0;background:rgb(12,34,56);color:white}button{position:absolute;left:20px;top:20px;width:120px;height:60px}input{position:absolute;left:20px;top:120px}</style><button onclick='window.clicked++'>Click</button><input id=text><script>window.clicked=0</script>").unwrap();
        let url = url::Url::from_file_path(directory.join("index.html"))
            .unwrap()
            .to_string();
        let runtime = BrowserRuntime::new();
        let id = Uuid::new_v4();
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(AllAssets),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.allow_parking();
        cx.update(|cx| {
            gpui_kit::init(cx);
            theme::initialize(None, None, cx);
            commands::bind_keys(cx);
        });
        let mut sample = None;
        let window = cx
            .open_window(size(px(800.), px(600.)), |window, cx| {
                let pane =
                    cx.new(|cx| BrowserPane::new(id, url.clone(), runtime.clone(), window, cx));
                let view = cx.new(|_| Sample { pane, width: 760. });
                sample = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        let sample = sample.unwrap();
        until(&mut cx, window, "page and frame", || {
            let state = runtime.snapshot(id).unwrap();
            assert!(state.error.is_none(), "Servo failed: {:?}", state.error);
            !state.loading && state.title == "Servo UI proof" && state.frame.is_some()
        });
        for (mode, width) in [(ThemeMode::Light, 760.), (ThemeMode::Dark, 480.)] {
            cx.update_window(window.into(), |_, w, cx| {
                Theme::change(mode, Some(w), cx);
                sample.update(cx, |sample, cx| {
                    sample.width = width;
                    cx.notify();
                });
                w.render_frame(cx);
            })
            .unwrap();
            let (bounds, scale) = cx
                .update_window(window.into(), |_, w, _| {
                    (w.find("servo-surface").bounds(), w.scale_factor())
                })
                .unwrap();
            assert!(
                (bounds.size.width.as_f32() - width).abs() < 1.0,
                "Browser must follow its layout width: {bounds:?}"
            );
            let physical_width = (bounds.size.width.as_f32() * scale).round() as u32;
            until(&mut cx, window, "resized frame", || {
                runtime
                    .snapshot(id)
                    .unwrap()
                    .frame
                    .is_some_and(|frame| frame.width == physical_width)
            });
            // A resized framebuffer can precede the page's repaint and GPUI's
            // asynchronous image upload. Wait for the actual displayed pixels.
            let deadline = Instant::now() + Duration::from_secs(45);
            let screenshot = loop {
                let screenshot = cx.capture_screenshot(window.into()).unwrap();
                let pixel = screenshot
                    .get_pixel(
                        ((bounds.left().as_f32() + 300.) * scale) as u32,
                        ((bounds.top().as_f32() + 250.) * scale) as u32,
                    )
                    .0;
                if pixel == [12, 34, 56, 255] {
                    break screenshot;
                }
                assert!(
                    Instant::now() < deadline,
                    "Servo RGBA must survive GPUI BGRA upload in {mode:?}; last pixel {pixel:?}"
                );
                pump(&mut cx, window);
            };
            if let Some(output) = std::env::var_os("DROSTJE_QA_SCREENSHOTS") {
                std::fs::create_dir_all(&output).unwrap();
                screenshot
                    .save(std::path::Path::new(&output).join(format!("servo-{mode:?}.png")))
                    .unwrap();
            }
            cx.update_window(window.into(), |_, w, cx| {
                w.click_at("servo-surface", point(px(50.), px(40.)), cx)
            })
            .unwrap();
            let expected = if mode == ThemeMode::Light { 1.0 } else { 2.0 };
            let deadline = Instant::now() + Duration::from_secs(10);
            while evaluate(&mut cx, window, &runtime, id, "window.clicked").as_f64()
                != Some(expected)
            {
                assert!(Instant::now() < deadline);
            }
        }
        cx.update_window(window.into(), |_, w, cx| {
            w.click_at("servo-surface", point(px(50.), px(130.)), cx)
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while evaluate(&mut cx, window, &runtime, id, "document.activeElement.id") != "text" {
            assert!(Instant::now() < deadline);
        }
        cx.update_window(window.into(), |_, w, cx| w.input("Servo text ✓", cx))
            .unwrap();
        while evaluate(
            &mut cx,
            window,
            &runtime,
            id,
            "document.querySelector('input').value",
        ) != "Servo text ✓"
        {
            assert!(Instant::now() < deadline);
        }
        cx.update_window(window.into(), |_, w, cx| {
            w.press("cmd-l", cx);
            w.press("cmd-a", cx);
            w.input("about:blank", cx);
            w.press("enter", cx);
        })
        .unwrap();
        until(&mut cx, window, "address navigation", || {
            runtime
                .snapshot(id)
                .is_some_and(|s| s.url == "about:blank" && !s.loading && s.can_go_back)
        });
        cx.update_window(window.into(), |_, w, cx| w.click("browser-back", cx))
            .unwrap();
        until(&mut cx, window, "toolbar history", || {
            runtime
                .snapshot(id)
                .is_some_and(|s| s.url == url && !s.loading && s.can_go_forward)
        });
        runtime.command(
            id,
            BrowserCommand::Navigate {
                url: url::Url::from_file_path(directory.join("missing.html"))
                    .unwrap()
                    .to_string(),
            },
        );
        until(&mut cx, window, "visible navigation failure", || {
            runtime.snapshot(id).is_some_and(|s| s.error.is_some())
        });
        if let Some(output) = std::env::var_os("DROSTJE_QA_SCREENSHOTS") {
            cx.capture_screenshot(window.into())
                .unwrap()
                .save(std::path::Path::new(&output).join("servo-error.png"))
                .unwrap();
        }
        runtime.shutdown();
        drop(sample);
        drop(cx);
        std::fs::remove_dir_all(directory).unwrap();
        println!(
            "Servo GPUI proof: light/dark pixels, resize, pointer click, committed text, address focus/navigation, toolbar history, navigation failure"
        );
    }
}
