//! Native transcript proof using captured OMP notice shapes; no provider turns.
fn main() {
    #[cfg(target_os = "macos")]
    macos::run();
    #[cfg(not(target_os = "macos"))]
    println!("job_notices: skipped; no headless renderer on this platform");
}

#[cfg(target_os = "macos")]
mod macos {
    use drostje::{
        app::{AppState, Bootstrap},
        harness::{ContentBlock, Message},
        model::{Project, ThreadKind},
        platform,
        store::Store,
        ui::{commands, theme, workspace::Workspace},
    };
    use gpui_kit::{
        AppContext, HeadlessAppContext, SharedString, WindowHandle,
        assets::AllAssets,
        component::{Root, Theme, ThemeMode},
        px, size,
        test::TestWindowExt,
    };
    use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};
    use uuid::Uuid;

    fn pump(cx: &mut HeadlessAppContext, window: WindowHandle<Root>) {
        cx.advance_clock(Duration::from_millis(40));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
    }

    pub fn run() {
        let directory =
            std::env::temp_dir().join(format!("drostje-job-notices-{}", Uuid::new_v4()));
        let repo = directory.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .arg(&repo)
                .status()
                .unwrap()
                .success()
        );
        let store = Store::open(directory.join("data")).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Notice rendering".into(),
            root: repo.clone(),
            archived: false,
            defaults: Default::default(),
        };
        smol::block_on(store.save_project(project.clone())).unwrap();
        let bootstrap = Bootstrap {
            data_dir: directory.join("data"),
            store: store.clone(),
            snapshot: smol::block_on(store.snapshot()).unwrap(),
            diagnostics: BTreeMap::new(),
            repositories: BTreeMap::from([(
                project.id,
                Ok(platform::inspect_repository(&repo).unwrap()),
            )]),
            browser_tools: None,
        };
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
        let state = cx.new(|cx| AppState::new(bootstrap, cx));
        let thread = state.update(&mut cx, |state, _| {
            state.select_project(project.id);
            state.new_thread(ThreadKind::Conversation).unwrap();
            state.selected_thread.unwrap()
        });
        let window = cx
            .open_window(size(px(1100.), px(800.)), |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .unwrap();
        pump(&mut cx, window);
        let batch = "<system-notice>\n2 background jobs have completed. Resume your work using the results below.\n\n── Job bg_16 (cargo run --features runtime-qa --example space_network_smoke 900) ──\nwarning: linker warning\nPASS: real GPUI renderer\n[raw output: artifact://53]\nWall time: 40.52 seconds\n── Job bg_17 (cargo test) ──\nerror: permission denied\nWall time: 20.23 seconds\nCommand exited with code 101\n</system-notice>";
        state.update(&mut cx, |state, cx| {
            let session = state.conversations.get_mut(&thread).unwrap();
            // Feed recorded transcript content into the production view, as browser QA does.
            session.messages = vec![Arc::new(Message {
                id: "jobs".into(),
                role: "custom".into(),
                content: vec![ContentBlock::Text(batch.into())],
                raw: None,
                tool_call_id: None,
                error: false,
                exit_code: None,
            })];
            session.generation = Uuid::new_v4();
            cx.notify();
        });
        let first = format!("job:{thread}:jobs:bg_16");
        let second = format!("job:{thread}:jobs:bg_17");
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            for width in [1100., 760.] {
                cx.update_window(window.into(), |_, window, cx| {
                    Theme::change(mode, Some(window), cx);
                    window.resize(size(px(width), px(800.)));
                })
                .unwrap();
                pump(&mut cx, window);
                cx.update_window(window.into(), |_, window, cx| {
                    assert!(
                        window
                            .find(SharedString::from(format!("{first}:card")))
                            .visible()
                    );
                    assert!(
                        window
                            .find(SharedString::from(format!("{second}:card")))
                            .visible()
                    );
                    assert!(
                        window
                            .try_find(SharedString::from(format!("{first}:body")))
                            .is_none()
                    );
                    window.click(SharedString::from(format!("{first}:header")), cx);
                })
                .unwrap();
                pump(&mut cx, window);
                if let Some(output) = std::env::var_os("DROSTJE_QA_SCREENSHOTS") {
                    std::fs::create_dir_all(&output).unwrap();
                    cx.capture_screenshot(window.into())
                        .unwrap()
                        .save(Path::new(&output).join(format!("job-output-{mode:?}-{width}.png")))
                        .unwrap();
                }
                cx.update_window(window.into(), |_, window, cx| {
                    assert!(
                        window
                            .find(SharedString::from(format!("{first}:body")))
                            .visible()
                    );
                    // Tab traversal must reach the same disclosure; Enter folds it again.
                    for _ in 0..80 {
                        if window.find(SharedString::from(first.clone())).focused() == Some(true) {
                            break;
                        }
                        window.press("tab", cx);
                        window.render_frame(cx);
                    }
                    assert_eq!(
                        window.find(SharedString::from(first.clone())).focused(),
                        Some(true)
                    );
                    window.press("enter", cx);
                    // Kit's press helper emits key-down only. Native buttons activate
                    // on key-up; send the matching release through GPUI as well.
                    window.dispatch_event(
                        gpui_kit::PlatformInput::KeyUp(gpui_kit::KeyUpEvent {
                            keystroke: gpui_kit::Keystroke::parse("enter").unwrap(),
                        }),
                        cx,
                    );
                })
                .unwrap();
                pump(&mut cx, window);
                cx.update_window(window.into(), |_, window, _| {
                    assert!(
                        window
                            .try_find(SharedString::from(format!("{first}:body")))
                            .is_none()
                    );
                })
                .unwrap();
                if let Some(output) = std::env::var_os("DROSTJE_QA_SCREENSHOTS") {
                    std::fs::create_dir_all(&output).unwrap();
                    cx.capture_screenshot(window.into())
                        .unwrap()
                        .save(Path::new(&output).join(format!("job-notices-{mode:?}-{width}.png")))
                        .unwrap();
                }
            }
        }
        state.update(&mut cx, |state, cx| {
            let session = state.conversations.get_mut(&thread).unwrap();
            session.messages = vec![Arc::new(Message {
                id: "notice-states".into(),
                role: "custom".into(),
                content: vec![
                    ContentBlock::Text("<system-notice>\nNew harness format: details remain available\n</system-notice>".into()),
                    ContentBlock::Text("<system-notice>\nBackground job Empty has completed.\n<task-result id=\"Empty\" status=\"completed\"><output></output></task-result>\n</system-notice>".into()),
                    ContentBlock::Text("<system-notice>\nBackground job Report has completed.\n<task-result id=\"Report\" agent=\"task\" status=\"completed\"><output>{\"summary\":\"Review ready\",\"changed\":[\"src/app.rs\"]}</output></task-result>\n</system-notice>".into()),
                ],
                raw: None, tool_call_id: None, error: false, exit_code: None,
            })];
            session.generation = Uuid::new_v4();
            cx.notify();
        });
        pump(&mut cx, window);
        let notice = format!("{thread}:notice-states:0");
        cx.update_window(window.into(), |_, window, cx| {
            assert!(
                window
                    .try_find(SharedString::from(format!("{notice}:notice-body")))
                    .is_none()
            );
            assert!(
                window
                    .try_find(SharedString::from(format!(
                        "job:{thread}:notice-states:Empty:body"
                    )))
                    .is_none()
            );
            assert!(
                window
                    .find(SharedString::from(format!(
                        "job:{thread}:notice-states:Report:body"
                    )))
                    .visible()
            );
            window.click(SharedString::from(notice.clone()), cx);
        })
        .unwrap();
        pump(&mut cx, window);
        cx.update_window(window.into(), |_, window, _| {
            assert!(
                window
                    .find(SharedString::from(format!("{notice}:notice-body")))
                    .visible()
            );
        })
        .unwrap();
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            cx.update_window(window.into(), |_, window, cx| {
                Theme::change(mode, Some(window), cx);
            })
            .unwrap();
            pump(&mut cx, window);
            cx.update_window(window.into(), |_, window, _| {
                assert!(
                    window
                        .find(SharedString::from(format!("{notice}:notice-body")))
                        .visible()
                );
                assert!(
                    window
                        .try_find(SharedString::from(format!(
                            "job:{thread}:notice-states:Empty"
                        )))
                        .is_none()
                );
            })
            .unwrap();
            if let Some(output) = std::env::var_os("DROSTJE_QA_SCREENSHOTS") {
                cx.capture_screenshot(window.into())
                    .unwrap()
                    .save(Path::new(&output).join(format!("notice-details-{mode:?}.png")))
                    .unwrap();
            }
        }
        state.update(&mut cx, |state, cx| state.begin_shutdown(false, cx));
        println!(
            "job_notices: native jobs, mouse and Tab/Enter disclosure, narrow layout, both themes, empty/structured output and unknown notices passed; no provider turns"
        );
    }
}
