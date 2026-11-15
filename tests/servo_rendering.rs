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
    fn wait_for_script(
        cx: &mut HeadlessAppContext,
        window: WindowHandle<Root>,
        runtime: &BrowserRuntime,
        id: Uuid,
        script: &str,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while evaluate(cx, window, runtime, id, script) != true {
            assert!(Instant::now() < deadline, "Condition timed out: {script}");
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
        evaluate(
            &mut cx,
            window,
            &runtime,
            id,
            "window.pointerEvents=0;window.wheelEvents=0;\
             document.addEventListener('mousedown',e=>{window.pointerEvents++;window.lastDown=[e.clientX,e.clientY]});\
             document.addEventListener('mousemove',e=>window.lastMove=[e.clientX,e.clientY]);\
             document.addEventListener('wheel',e=>{e.preventDefault();window.wheelEvents++;window.lastWheel=[e.clientX,e.clientY,e.deltaY]}, {passive:false});\
             const corner=document.createElement('div');corner.style='position:fixed;right:0;bottom:0;width:20px;height:20px;background:rgb(31,221,71)';document.body.append(corner);true",
        );
        let pane = cx.update(|cx| sample.read(cx).pane.clone());
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                for invalid in [
                    (0, 100, 1.),
                    (100, 100, f32::NAN),
                    (100, 100, 0.24),
                    (100, 100, 8.01),
                    (8193, 1, 1.),
                    (8192, 8192, 1.),
                ] {
                    assert!(pane.set_viewport(Some(invalid), cx).is_err());
                }
            })
        });
        for (css_width, css_height, device_scale) in [(960, 640, 2.), (320, 240, 1.25)] {
            cx.update(|cx| {
                pane.update(cx, |pane, cx| {
                    pane.set_viewport(Some((css_width, css_height, device_scale)), cx)
                        .unwrap()
                })
            });
            for width in [760., 480.] {
                cx.update_window(window.into(), |_, w, cx| {
                    sample.update(cx, |sample, cx| {
                        sample.width = width;
                        cx.notify();
                    });
                    w.render_frame(cx);
                })
                .unwrap();
                until(&mut cx, window, "fixed viewport frame", || {
                    runtime.snapshot(id).unwrap().frame.is_some_and(|frame| {
                        frame.width == (css_width as f32 * device_scale).round() as u32
                            && frame.height == (css_height as f32 * device_scale).round() as u32
                            && frame.scale == device_scale
                    })
                });
                wait_for_script(
                    &mut cx,
                    window,
                    &runtime,
                    id,
                    &format!("innerWidth === {css_width} && innerHeight === {css_height}"),
                );
                let (bounds, native_scale) = cx
                    .update_window(window.into(), |_, w, _| {
                        (w.find("servo-surface").bounds(), w.scale_factor())
                    })
                    .unwrap();
                let fit = (bounds.size.width.as_f32() / css_width as f32)
                    .min(bounds.size.height.as_f32() / css_height as f32);
                let left = (bounds.size.width.as_f32() - css_width as f32 * fit) / 2.;
                let top = (bounds.size.height.as_f32() - css_height as f32 * fit) / 2.;
                let deadline = Instant::now() + Duration::from_secs(45);
                loop {
                    let screenshot = cx.capture_screenshot(window.into()).unwrap();
                    let pixel = screenshot
                        .get_pixel(
                            ((bounds.left().as_f32() + left + (css_width as f32 - 10.) * fit)
                                * native_scale) as u32,
                            ((bounds.top().as_f32() + top + (css_height as f32 - 10.) * fit)
                                * native_scale) as u32,
                        )
                        .0;
                    if pixel == [31, 221, 71, 255] {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "Fixed viewport must fit the pane: {pixel:?}"
                    );
                    pump(&mut cx, window);
                }
                let clicks = evaluate(&mut cx, window, &runtime, id, "window.clicked")
                    .as_f64()
                    .unwrap();
                cx.update_window(window.into(), |_, w, cx| {
                    w.click_at(
                        "servo-surface",
                        point(px(left + 50. * fit), px(top + 40. * fit)),
                        cx,
                    );
                })
                .unwrap();
                wait_for_script(
                    &mut cx,
                    window,
                    &runtime,
                    id,
                    &format!(
                        "window.clicked === {} && Math.abs(lastDown[0]-50)<2 && Math.abs(lastDown[1]-40)<2",
                        clicks + 1.
                    ),
                );
                cx.update_window(window.into(), |_, w, cx| {
                    w.click_at(
                        "servo-surface",
                        point(px(left + 50. * fit), px(top + 130. * fit)),
                        cx,
                    );
                })
                .unwrap();
                wait_for_script(
                    &mut cx,
                    window,
                    &runtime,
                    id,
                    "document.activeElement.id === 'text'",
                );
                cx.update_window(window.into(), |_, w, cx| {
                    w.press("cmd-a", cx);
                    w.input("Fixed viewport ✓", cx);
                })
                .unwrap();
                wait_for_script(
                    &mut cx,
                    window,
                    &runtime,
                    id,
                    "document.getElementById('text').value === 'Fixed viewport ✓'",
                );
                cx.update_window(window.into(), |_, w, cx| {
                    w.hover("servo-surface", cx);
                    w.scroll(
                        "servo-surface",
                        ScrollDelta::Pixels(point(px(0.), px(-30.))),
                        cx,
                    );
                })
                .unwrap();
                wait_for_script(
                    &mut cx,
                    window,
                    &runtime,
                    id,
                    &format!(
                        "window.lastWheel && Math.abs(lastMove[0]-{})<2 && Math.abs(lastMove[1]-{})<2 && Math.abs(lastWheel[0]-{})<2 && Math.abs(lastWheel[1]-{})<2 && Math.abs(lastWheel[2]-{})<2",
                        css_width / 2,
                        css_height / 2,
                        css_width / 2,
                        css_height / 2,
                        30. / fit * device_scale,
                    ),
                );
                let counts = evaluate(
                    &mut cx,
                    window,
                    &runtime,
                    id,
                    "[window.pointerEvents,window.wheelEvents]",
                );
                cx.update_window(window.into(), |_, w, cx| {
                    w.click_at("servo-surface", point(px(1.), px(1.)), cx);
                    w.dispatch_event(
                        PlatformInput::ScrollWheel(ScrollWheelEvent {
                            position: bounds.origin + point(px(1.), px(1.)),
                            delta: ScrollDelta::Pixels(point(px(0.), px(-30.))),
                            ..Default::default()
                        }),
                        cx,
                    );
                })
                .unwrap();
                assert_eq!(
                    evaluate(
                        &mut cx,
                        window,
                        &runtime,
                        id,
                        "[window.pointerEvents,window.wheelEvents]"
                    ),
                    counts,
                    "Letterboxing must not forward native input",
                );
            }
        }
        cx.update(|cx| pane.update(cx, |pane, cx| pane.set_viewport(None, cx).unwrap()));
        for width in [760., 480.] {
            let (bounds, scale) = cx
                .update_window(window.into(), |_, w, cx| {
                    sample.update(cx, |sample, cx| {
                        sample.width = width;
                        cx.notify();
                    });
                    w.render_frame(cx);
                    (w.find("servo-surface").bounds(), w.scale_factor())
                })
                .unwrap();
            until(&mut cx, window, "restored automatic viewport", || {
                runtime.snapshot(id).unwrap().frame.is_some_and(|frame| {
                    frame.width == (bounds.size.width.as_f32() * scale).round() as u32
                        && frame.height == (bounds.size.height.as_f32() * scale).round() as u32
                        && frame.scale == scale
                })
            });
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
        drop(pane);
        drop(sample);
        drop(cx);
        std::fs::remove_dir_all(directory).unwrap();
        println!(
            "Servo GPUI proof: light/dark pixels, resize, fixed viewport fitting and restore, scaled native click/move/wheel, letterbox exclusion, committed text, address focus/navigation, toolbar history, navigation failure"
        );
    }
}
