use super::*;

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; space isolation, native controls and draft safety without provider turns"]
async fn spaces_preserve_drafts_tabs_and_shared_projects(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let default = cx.read(|cx| fixture.state.read(cx).active_space);
    let project = cx.read(|cx| fixture.state.read(cx).selected_project.unwrap());
    let project_root = cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .snapshot
            .projects
            .iter()
            .find(|item| item.id == project)
            .unwrap()
            .root
            .clone()
    });
    let thread = new_conversation(cx, &fixture).await;
    let session = cx.read(|cx| fixture.state.read(cx).conversations[&thread].generation);
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.focus_composer(window, cx);
        },
    );
    cx.simulate_keystrokes(fixture.window.into(), "s a f e secondary-escape");
    wait_for_saved_thread(cx, &fixture, thread, |thread| thread.draft == "safe").await;
    cx.dispatch_action(fixture.window.into(), commands::ToggleProjects);
    cx.run_until_parked();

    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(fixture.window.into(), "n e w space s p a c e");
    cx.simulate_keystrokes(fixture.window.into(), "enter");
    cx.run_until_parked();
    until(cx, "new space form", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).dialog.is_some()
    })
    .await;
    set_dialog(cx, fixture.window, &fixture.workspace, "name", "Default");
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    cx.run_until_parked();
    assert!(cx.read(|cx| fixture.workspace.read(cx).dialog.is_some()));
    assert_eq!(cx.read(|cx| fixture.state.read(cx).spaces.len()), 1);
    set_dialog(cx, fixture.window, &fixture.workspace, "name", "Research");
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    until(cx, "new empty space", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).active_space != default
    })
    .await;
    let research = cx.read(|cx| fixture.state.read(cx).active_space);
    assert!(cx.read(|cx| {
        !fixture
            .workspace
            .read(cx)
            .dock
            .read(cx)
            .is_dock_open(gpui_kit::component::dock::DockPlacement::Left)
    }));
    cx.dispatch_action(fixture.window.into(), commands::ToggleProjects);
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| fixture
            .state
            .read(cx)
            .conversations
            .get(&thread)
            .map(|session| session.generation)),
        Some(session),
        "switching spaces must retain the original harness session"
    );
    assert!(cx.read(|cx| {
        let state = fixture.state.read(cx);
        state.open_threads.is_empty()
            && state.selected_thread.is_none()
            && state.selected_project.is_none()
            && crate::ui::workspace::projects::rows(state, "").is_empty()
    }));
    assert!(cx.read(|cx| {
        commands::Command::NewConversation
            .disabled_reason(fixture.state.read(cx))
            .is_some()
    }));

    cx.dispatch_action(fixture.window.into(), commands::AddProject);
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec![project_root.clone()])
    });
    until(
        cx,
        "existing project added to second space",
        Duration::from_secs(10),
        |cx| {
            let state = fixture.state.read(cx);
            state.project_in_active_space(project) && state.selected_project == Some(project)
        },
    )
    .await;
    assert!(cx.read(|cx| {
        commands::Command::NewConversation
            .disabled_reason(fixture.state.read(cx))
            .is_none()
    }));
    assert!(cx.read(|cx| {
        !crate::ui::workspace::projects::rows(fixture.state.read(cx), "")
            .contains(&Row::Thread(thread))
    }));
    let research_thread = new_conversation(cx, &fixture).await;
    assert!(cx.read(|cx| {
        let rows = crate::ui::workspace::projects::rows(fixture.state.read(cx), "");
        rows.contains(&Row::Thread(research_thread)) && !rows.contains(&Row::Thread(thread))
    }));

    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace
                .move_thread_to_space(research_thread, default, window, cx)
                .unwrap();
        },
    );
    until(
        cx,
        "thread moved between spaces",
        Duration::from_secs(10),
        |cx| {
            !fixture
                .state
                .read(cx)
                .thread_in_active_space(research_thread)
        },
    )
    .await;
    assert!(cx.read(|cx| {
        let state = fixture.state.read(cx);
        state.project_in_active_space(project)
            && !crate::ui::workspace::projects::rows(state, "")
                .contains(&Row::Thread(research_thread))
    }));

    for (mode, expected) in [("light", ThemeMode::Light), ("dark", ThemeMode::Dark)] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        until(cx, "space theme", Duration::from_secs(5), |cx| {
            Theme::global(cx).mode == expected
        })
        .await;
        for (width, height) in [(1440., 900.), (900., 600.)] {
            cx.simulate_window_resize(fixture.window.into(), size(px(width), px(height)));
            for _ in 0..3 {
                cx.run_until_parked();
                cx.update_window(fixture.window.into(), |_, window, cx| {
                    window.simulate_next_frame(cx);
                    window.refresh();
                    window.render_frame(cx);
                })
                .unwrap();
            }
            cx.update_window(fixture.window.into(), |_, window, _| {
                let switcher = window.find("space-switcher").bounds();
                let add = window.find("new-space").bounds();
                assert!(switcher.size.width > px(0.) && add.size.width > px(0.));
                assert!(switcher.origin.y >= px(0.));
                assert!(add.bottom_right().y <= px(height));
                assert!(add.bottom_right().x <= px(width));
            })
            .unwrap();
        }
    }

    cx.dispatch_action(fixture.window.into(), commands::RenameSpace);
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            let (target, _, input, _) = workspace.inline_rename().unwrap();
            assert_eq!(target, crate::ui::workspace::RenameTarget::Space(research));
            input.update(cx, |input, cx| {
                input.set_value("Investigation".to_owned(), window, cx)
            });
            input.focus_handle(cx).focus(window, cx);
        },
    );
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    until(cx, "space renamed", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).active_space_name() == "Investigation"
    })
    .await;
    cx.dispatch_action(fixture.window.into(), commands::PreviousSpace);
    until(
        cx,
        "return to original tabs",
        Duration::from_secs(5),
        |cx| {
            let state = fixture.state.read(cx);
            state.active_space == default && state.selected_thread == Some(thread)
        },
    )
    .await;
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).open_threads.clone()),
        vec![thread]
    );
    assert_eq!(
        cx.read(
            |cx| fixture.state.read(cx).drafts[&DraftKey::Thread(thread)]
                .text
                .clone()
        ),
        "safe"
    );
    assert!(cx.read(|cx| {
        commands::Command::DeleteSpace
            .disabled_reason(fixture.state.read(cx))
            .is_some()
    }));
    assert!(
        cx.read(|cx| {
            fixture
                .workspace
                .read(cx)
                .dock
                .read(cx)
                .dump(cx)
                .left_dock
                .as_ref()
                .is_some_and(|dock| dock.open())
        }),
        "returning to Default keeps the shared Projects pane open"
    );

    cx.dispatch_action(fixture.window.into(), commands::MoveProjectToSpace);
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
    cx.simulate_keystrokes(fixture.window.into(), "enter");
    until(
        cx,
        "project moved out of Default",
        Duration::from_secs(5),
        |cx| !fixture.state.read(cx).project_in_active_space(project),
    )
    .await;
    until(cx, "moved draft detached", Duration::from_secs(5), |cx| {
        fixture
            .workspace
            .read(cx)
            .editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).active_key().is_none())
    })
    .await;
    assert!(cx.read(|cx| {
        let state = fixture.state.read(cx);
        state.open_threads.is_empty() && crate::ui::workspace::projects::rows(state, "").is_empty()
    }));
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.switch_space(research, window, cx).unwrap(),
    );
    until(
        cx,
        "project visible in destination",
        Duration::from_secs(5),
        |cx| {
            let state = fixture.state.read(cx);
            state.active_space == research && state.project_in_active_space(project)
        },
    )
    .await;
    assert!(cx.read(|cx| {
        crate::ui::workspace::projects::rows(fixture.state.read(cx), "")
            .contains(&crate::ui::workspace::projects::Row::Thread(thread))
    }));
    fixture.state.update(cx, |state, cx| {
        state.select_thread(thread);
        cx.notify();
    });
    until(cx, "moved draft restored", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).editor_selection == Some(DraftKey::Thread(thread))
    })
    .await;
    assert_eq!(
        cx.read(
            |cx| fixture.state.read(cx).drafts[&DraftKey::Thread(thread)]
                .text
                .clone()
        ),
        "safe"
    );

    cx.dispatch_action(fixture.window.into(), commands::DeleteSpace);
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), research);
    assert_eq!(cx.read(|cx| fixture.state.read(cx).spaces.len()), 2);
    cx.dispatch_action(fixture.window.into(), commands::DeleteSpace);
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    cx.run_until_parked();
    until(
        cx,
        "space removed without deleting project",
        Duration::from_secs(5),
        |cx| {
            let state = fixture.state.read(cx);
            state.active_space == default
                && state.spaces.len() == 1
                && state.project_in_active_space(project)
        },
    )
    .await;
    assert!(cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .any(|saved| saved.id == thread && saved.draft == "safe")
    }));
    fixture.state.update(cx, |state, cx| {
        state.select_thread(thread);
        cx.notify();
    });
    cx.run_until_parked();
    cx.dispatch_action(fixture.window.into(), commands::NewSpace);
    set_dialog(cx, fixture.window, &fixture.workspace, "name", "Restart");
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    until(
        cx,
        "leave an empty space active for restart",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).active_space_name() == "Restart",
    )
    .await;
    quit(cx, &fixture).await;
    println!("DROSTJE_SPACES_QA_DATA={}", fixture.data.display());
}

#[gpui_kit::test]
#[ignore = "requires DROSTJE_SPACES_QA_DATA from the space lifecycle scenario; restores inactive tabs after process restart"]
async fn spaces_restore_active_context_and_inactive_tabs(cx: &mut TestAppContext) {
    let data = PathBuf::from(std::env::var_os("DROSTJE_SPACES_QA_DATA").unwrap());
    let root = PathBuf::from(std::env::var_os("DROSTJE_RUNTIME_FIXTURE").unwrap());
    assert!(
        fs::canonicalize(&data)
            .unwrap()
            .starts_with(fs::canonicalize(root).unwrap())
    );
    let fixture = open_fixture(cx, Some(data)).await;
    assert!(cx.read(|cx| {
        let state = fixture.state.read(cx);
        state.active_space_name() == "Restart"
            && state.selected_thread.is_none()
            && state.open_threads.is_empty()
            && crate::ui::workspace::projects::rows(state, "").is_empty()
    }));
    cx.dispatch_action(fixture.window.into(), commands::SwitchSpace);
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
    cx.simulate_keystrokes(fixture.window.into(), "g g enter");
    until(
        cx,
        "inactive tabs restored after restart",
        Duration::from_secs(5),
        |cx| {
            let state = fixture.state.read(cx);
            state.active_space == state.default_space_id()
                && state.thread().is_some_and(|thread| thread.draft == "safe")
        },
    )
    .await;
    let thread = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).open_threads.clone()),
        vec![thread]
    );
    until(
        cx,
        "restored native editor",
        Duration::from_secs(15),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .editor
                .as_ref()
                .is_some_and(|editor| {
                    editor.read(cx).active_key() == Some(&DraftKey::Thread(thread))
                })
        },
    )
    .await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.focus_composer(window, cx);
        },
    );
    cx.simulate_keystrokes(fixture.window.into(), "escape :");
    let notices = cx.read(|cx| fixture.state.read(cx).notice_sequence);
    cx.dispatch_action(fixture.window.into(), commands::NextSpace);
    until(
        cx,
        "unfinished Vim command blocks switching",
        Duration::from_secs(15),
        |cx| fixture.state.read(cx).notice_sequence > notices,
    )
    .await;
    assert!(cx.read(|cx| {
        let state = fixture.state.read(cx);
        state.active_space == state.default_space_id() && state.selected_thread == Some(thread)
    }));
    cx.dispatch_action(fixture.window.into(), commands::CancelEditorCommand);
    cx.run_until_parked();
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable fixture; exercises gesture-linked Space motion and reduced motion"]
async fn space_swipes_track_reverse_cancel_and_settle(cx: &mut TestAppContext) {
    use gpui_kit::{Bounds, Pixels, PlatformInput, ScrollDelta, ScrollEvent, TouchPhase, point};

    struct Geometry {
        rows: Bounds<Pixels>,
        controls: [Bounds<Pixels>; 3],
        preview: Option<Bounds<Pixels>>,
    }

    fn geometry(cx: &mut TestAppContext, fixture: &Fixture, preview: bool) -> Geometry {
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.render_frame(cx);
            Geometry {
                rows: window.find("project-rows").bounds(),
                controls: ["Projects", "space-switcher", "new-space"]
                    .map(|id| window.find(id).bounds()),
                preview: preview.then(|| window.find("project-rows-preview").bounds()),
            }
        })
        .unwrap()
    }

    fn near(actual: Pixels, expected: Pixels) {
        assert!(
            (actual - expected).as_f32().abs() <= 0.5,
            "expected {expected:?}, got {actual:?}"
        );
    }

    fn stationary(actual: &Geometry, rest: &Geometry) {
        assert_eq!(
            actual.controls, rest.controls,
            "Space motion must not move or resize Projects or its controls"
        );
        near(actual.rows.origin.y, rest.rows.origin.y);
        near(actual.rows.size.width, rest.rows.size.width);
        near(actual.rows.size.height, rest.rows.size.height);
    }

    fn dragged(cx: &mut TestAppContext, fixture: &Fixture, rest: &Geometry, offset: f32) {
        let actual = geometry(cx, fixture, true);
        stationary(&actual, rest);
        near(actual.rows.origin.x, rest.rows.origin.x + px(offset));
        let preview = actual.preview.unwrap();
        near(
            preview.origin.x - actual.rows.origin.x,
            rest.rows.size.width * -offset.signum(),
        );
        near(preview.origin.y, actual.rows.origin.y);
        near(preview.size.width, rest.rows.size.width);
        near(preview.size.height, rest.rows.size.height);
    }

    fn scroll(cx: &mut TestAppContext, fixture: &Fixture, x: f32, y: f32, phase: TouchPhase) {
        cx.update_window(fixture.window.into(), |_, window, cx| {
            let position = window.find("space-switcher").bounds().center();
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position,
                    delta: ScrollDelta::Pixels(point(px(x), px(y))),
                    touch_phase: phase,
                    ..Default::default()
                }),
                cx,
            );
        })
        .unwrap();
    }

    async fn settle(cx: &mut TestAppContext, fixture: &Fixture, rest: &Geometry) {
        smol::Timer::after(Duration::from_millis(200)).await;
        cx.run_until_parked();
        let actual = geometry(cx, fixture, false);
        stationary(&actual, rest);
        near(actual.rows.origin.x, rest.rows.origin.x);
    }

    let fixture = open_fixture(cx, None).await;
    let original = cx.read(|cx| fixture.state.read(cx).active_space);
    let thread = new_conversation(cx, &fixture).await;
    let next = fixture.state.update(cx, |state, cx| {
        let id = state.create_space("Swipe destination").unwrap();
        cx.notify();
        id
    });
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace
                .move_thread_to_space(thread, next, window, cx)
                .unwrap();
        },
    );
    cx.dispatch_action(fixture.window.into(), commands::FocusProjects);
    cx.run_until_parked();
    for (mode, expected) in [("light", ThemeMode::Light), ("dark", ThemeMode::Dark)] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        until(cx, "swipe theme", Duration::from_secs(5), |cx| {
            Theme::global(cx).mode == expected
        })
        .await;
        for (width, height) in [(1440., 900.), (900., 600.)] {
            cx.simulate_window_resize(fixture.window.into(), size(px(width), px(height)));
            for _ in 0..3 {
                cx.run_until_parked();
                geometry(cx, &fixture, false);
            }
            let rest = geometry(cx, &fixture, false);
            assert!(rest.rows.size.width > px(80.));

            scroll(cx, &fixture, -10., 80., TouchPhase::Started);
            let vertical = geometry(cx, &fixture, false);
            stationary(&vertical, &rest);
            near(vertical.rows.origin.x, rest.rows.origin.x);
            scroll(cx, &fixture, 0., 0., TouchPhase::Ended);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            settle(cx, &fixture, &rest).await;
            scroll(cx, &fixture, -90., 46., TouchPhase::Started);
            scroll(cx, &fixture, 0., 0., TouchPhase::Ended);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            settle(cx, &fixture, &rest).await;

            scroll(cx, &fixture, -40.25, 0., TouchPhase::Started);
            dragged(cx, &fixture, &rest, -40.25);
            scroll(cx, &fixture, -44.5, 0., TouchPhase::Moved);
            dragged(cx, &fixture, &rest, -84.75);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            smol::Timer::after(Duration::from_millis(220)).await;
            cx.run_until_parked();
            dragged(cx, &fixture, &rest, -84.75);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            scroll(cx, &fixture, 65., 0., TouchPhase::Moved);
            dragged(cx, &fixture, &rest, -19.75);
            scroll(cx, &fixture, 0., 0., TouchPhase::Ended);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            scroll(cx, &fixture, -200., 0., TouchPhase::Moved);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            settle(cx, &fixture, &rest).await;

            scroll(cx, &fixture, -40.25, 0., TouchPhase::Started);
            scroll(cx, &fixture, -44.5, 0., TouchPhase::Moved);
            dragged(cx, &fixture, &rest, -84.75);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            scroll(cx, &fixture, 0., 0., TouchPhase::Cancelled);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            scroll(cx, &fixture, -200., 0., TouchPhase::Moved);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            settle(cx, &fixture, &rest).await;
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);

            scroll(cx, &fixture, -40., 0., TouchPhase::Started);
            scroll(cx, &fixture, -40., 0., TouchPhase::Moved);
            dragged(cx, &fixture, &rest, -80.);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            scroll(cx, &fixture, 0., 0., TouchPhase::Ended);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);
            let incoming = geometry(cx, &fixture, false);
            stationary(&incoming, &rest);
            assert!(
                incoming.rows.origin.x > rest.rows.origin.x,
                "destination enters from the adjacent preview's side"
            );
            scroll(cx, &fixture, -200., 0., TouchPhase::Moved);
            scroll(cx, &fixture, 200., 0., TouchPhase::Moved);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);
            settle(cx, &fixture, &rest).await;
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);

            cx.simulate_keystrokes(fixture.window.into(), "secondary-alt-[");
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            let keyboard = geometry(cx, &fixture, false);
            stationary(&keyboard, &rest);
            assert!(
                keyboard.rows.origin.x < rest.rows.origin.x,
                "keyboard switching also animates"
            );
            settle(cx, &fixture, &rest).await;

            let content_width = rest.rows.size.width.as_f32();
            scroll(cx, &fixture, -content_width - 70., 0., TouchPhase::Started);
            dragged(cx, &fixture, &rest, -content_width);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            scroll(cx, &fixture, 0., 0., TouchPhase::Ended);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);
            settle(cx, &fixture, &rest).await;
            cx.simulate_keystrokes(fixture.window.into(), "secondary-alt-[");
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            settle(cx, &fixture, &rest).await;

            scroll(cx, &fixture, -84.75, 0., TouchPhase::Moved);
            dragged(cx, &fixture, &rest, -84.75);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            scroll(cx, &fixture, 65., 0., TouchPhase::Moved);
            dragged(cx, &fixture, &rest, -19.75);
            smol::Timer::after(Duration::from_millis(220)).await;
            cx.run_until_parked();
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            settle(cx, &fixture, &rest).await;

            scroll(cx, &fixture, 40.25, 0., TouchPhase::Moved);
            scroll(cx, &fixture, 44.5, 0., TouchPhase::Moved);
            dragged(cx, &fixture, &rest, 84.75);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            smol::Timer::after(Duration::from_millis(220)).await;
            cx.run_until_parked();
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);
            settle(cx, &fixture, &rest).await;
            cx.simulate_keystrokes(fixture.window.into(), "secondary-alt-[");
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            settle(cx, &fixture, &rest).await;

            cx.update(|cx| cx.set_reduce_motion(true));
            scroll(cx, &fixture, -40., 0., TouchPhase::Started);
            scroll(cx, &fixture, -45., 0., TouchPhase::Moved);
            let reduced = geometry(cx, &fixture, false);
            stationary(&reduced, &rest);
            near(reduced.rows.origin.x, rest.rows.origin.x);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            smol::Timer::after(Duration::from_millis(220)).await;
            cx.run_until_parked();
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            let held = geometry(cx, &fixture, false);
            stationary(&held, &rest);
            near(held.rows.origin.x, rest.rows.origin.x);
            scroll(cx, &fixture, 0., 0., TouchPhase::Ended);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);
            let released = geometry(cx, &fixture, false);
            stationary(&released, &rest);
            near(released.rows.origin.x, rest.rows.origin.x);
            scroll(cx, &fixture, -200., 0., TouchPhase::Moved);
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);
            settle(cx, &fixture, &rest).await;
            cx.simulate_keystrokes(fixture.window.into(), "secondary-alt-[");
            assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
            let keyboard = geometry(cx, &fixture, false);
            stationary(&keyboard, &rest);
            near(keyboard.rows.origin.x, rest.rows.origin.x);
            cx.update(|cx| cx.set_reduce_motion(false));
        }
    }
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable fixture; shared sidebar geometry across Spaces and reopening"]
async fn spaces_share_sidebar_geometry_and_visibility(cx: &mut TestAppContext) {
    use gpui_kit::component::dock::DockPlacement;

    fn sidebar(cx: &mut TestAppContext, fixture: &Fixture) -> (f32, bool) {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.read(|cx| {
            let dock = fixture
                .workspace
                .read(cx)
                .dock
                .read(cx)
                .dump(cx)
                .left_dock
                .unwrap();
            (dock.size().as_f32(), dock.open())
        })
    }

    if let Some(data) = std::env::var_os("DROSTJE_SHARED_SIDEBAR_QA_DATA") {
        let data = PathBuf::from(data);
        let root = PathBuf::from(std::env::var_os("DROSTJE_RUNTIME_FIXTURE").unwrap());
        assert!(
            fs::canonicalize(&data)
                .unwrap()
                .starts_with(fs::canonicalize(root).unwrap())
        );
        let fixture = open_fixture(cx, Some(data)).await;
        let width = cx
            .update_window(fixture.window.into(), |_, window, _| {
                (window.rem_size() * 18.).as_f32()
            })
            .unwrap();
        let shared = (width, false);
        assert_eq!(
            sidebar(cx, &fixture),
            shared,
            "shared sidebar geometry survives reopening"
        );
        cx.dispatch_action(fixture.window.into(), commands::PreviousSpace);
        assert_eq!(
            sidebar(cx, &fixture),
            shared,
            "inactive Space geometry stays shared after reopening"
        );
        cx.dispatch_action(fixture.window.into(), commands::ToggleProjects);
        assert_eq!(sidebar(cx, &fixture), (width, true));
        quit(cx, &fixture).await;
        return;
    }

    let fixture = open_fixture(cx, None).await;
    let original = cx.read(|cx| fixture.state.read(cx).active_space);
    let next = fixture.state.update(cx, |state, cx| {
        let id = state.create_space("Shared sidebar").unwrap();
        cx.notify();
        id
    });
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.dock.update(cx, |dock, cx| {
                dock.set_dock_size(DockPlacement::Left, window.rem_size() * 20., window, cx);
            });
        },
    );
    let first = sidebar(cx, &fixture);
    cx.dispatch_action(fixture.window.into(), commands::NextSpace);
    assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);
    assert_eq!(
        sidebar(cx, &fixture),
        first,
        "a new Space must not reset the sidebar width"
    );

    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.dock.update(cx, |dock, cx| {
                dock.set_dock_size(DockPlacement::Left, window.rem_size() * 18., window, cx);
            });
        },
    );
    cx.dispatch_action(fixture.window.into(), commands::ToggleProjects);
    let shared = sidebar(cx, &fixture);
    assert!(!shared.1);
    cx.dispatch_action(fixture.window.into(), commands::PreviousSpace);
    assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
    assert_eq!(
        sidebar(cx, &fixture),
        shared,
        "an older Space must not restore stale sidebar geometry"
    );
    cx.dispatch_action(fixture.window.into(), commands::NextSpace);
    assert_eq!(
        sidebar(cx, &fixture),
        shared,
        "an empty Space must not reopen the shared sidebar"
    );

    quit(cx, &fixture).await;
    println!("DROSTJE_SHARED_SIDEBAR_QA_DATA={}", fixture.data.display());
}
