use super::*;

fn has_focus_outline(window: &Window, cx: &App, id: impl Into<gpui_kit::ElementId>) -> bool {
    let bounds = window.find(id).bounds().scale(window.scale_factor());
    window.painted_quads().iter().any(|quad| {
        quad.bounds == bounds
            && quad.border_color.a > 0.
            && quad.border_color != Theme::global(cx).ring
            && quad.border_widths.left > px(0.).scale(window.scale_factor())
    })
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; real Neovim and focus presentation"]
async fn keyboard_focus_moves_visibly_between_draft_and_projects(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let first = new_conversation(cx, &fixture).await;
    cx.dispatch_action(fixture.window.into(), commands::NewOmpConversation);
    let second = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    assert_ne!(first, second);
    until(
        cx,
        "second Conversation buffer",
        Duration::from_secs(15),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .editor
                .as_ref()
                .is_some_and(|editor| {
                    editor.read(cx).active_key() == Some(&DraftKey::Thread(second))
                })
        },
    )
    .await;
    let press = |cx: &mut TestAppContext, keys: &str| {
        for key in keys.split_whitespace() {
            cx.update_window(fixture.window.into(), |_, window, cx| window.press(key, cx))
                .unwrap();
            cx.run_until_parked();
        }
    };
    cx.update_window(fixture.window.into(), |_, window, _| {
        window.activate_window()
    })
    .unwrap();
    let first_row = gpui_kit::SharedString::from(format!("thread-{first}"));
    let second_row = gpui_kit::SharedString::from(format!("thread-{second}"));
    press(cx, "secondary-2 i a b c");
    until(
        cx,
        "draft before pane navigation",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).drafts[&DraftKey::Thread(second)].text == "abc",
    )
    .await;

    let editor = cx.read(|cx| fixture.state.read(cx).editor.clone().unwrap());
    let mut retained = None;
    for mode in ["light", "dark"] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        for (width, height) in [(1440., 900.), (900., 600.)] {
            cx.simulate_window_resize(fixture.window.into(), size(px(width), px(height)));
            press(cx, "secondary-2 i");
            cx.run_until_parked();
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(has_focus_outline(window, cx, "prompt-editor"));
                assert!(!has_focus_outline(window, cx, second_row.clone()));
            })
            .unwrap();
            press(cx, "escape ctrl-w l ctrl-w ctrl-l");
            cx.run_until_parked();
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(
                    window.find("prompt-editor").focused(),
                    Some(true),
                    "native Ctrl-W chords must stay with Neovim"
                );
            })
            .unwrap();
            retained = Some(editor.snapshot(DraftKey::Thread(second)).await.unwrap());

            press(cx, "secondary-alt-left");
            cx.run_until_parked();
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("Projects").focused(), Some(true));
                assert!(!has_focus_outline(window, cx, "prompt-editor"));
                assert!(has_focus_outline(window, cx, second_row.clone()));
            })
            .unwrap();
            let motion = cx.read(|cx| {
                let rows = crate::ui::workspace::projects::rows(fixture.state.read(cx), "");
                let from = rows
                    .iter()
                    .position(|row| *row == Row::Thread(second))
                    .unwrap();
                let to = rows
                    .iter()
                    .position(|row| *row == Row::Thread(first))
                    .unwrap();
                if to < from { "k" } else { "j" }
            });
            press(cx, motion);
            cx.run_until_parked();
            assert_eq!(
                cx.read(|cx| fixture.state.read(cx).selected_thread),
                Some(second)
            );
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(has_focus_outline(window, cx, first_row.clone()));
                assert!(!has_focus_outline(window, cx, second_row.clone()));
            })
            .unwrap();

            press(cx, "/");
            cx.run_until_parked();
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(!has_focus_outline(window, cx, first_row.clone()));
                assert!(!has_focus_outline(window, cx, "prompt-editor"));
            })
            .unwrap();
            press(cx, "escape");
        }
    }
    let retained = retained.unwrap();
    press(cx, "secondary-2 i");
    let returned = editor.snapshot(DraftKey::Thread(second)).await.unwrap();
    assert_eq!(returned.text, "abc");
    assert_eq!(returned.text, retained.text);
    assert_eq!(returned.cursor, retained.cursor);
    let mut expected = retained.text;
    expected.insert(retained.cursor.1, 'd');
    press(cx, "i d");
    until(
        cx,
        "draft after pane navigation",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).drafts[&DraftKey::Thread(second)].text == expected,
    )
    .await;
    VisualTestContext::from_window(fixture.window.into(), cx).deactivate_window();
    cx.run_until_parked();
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!has_focus_outline(window, cx, "prompt-editor"));
        assert!(window.try_find("pane-focus").is_none());
        window.activate_window();
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(has_focus_outline(window, cx, "prompt-editor"));
    })
    .unwrap();
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; split-pane navigation into Projects"]
async fn pane_navigation_reaches_projects_and_selects_a_thread(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let press = |cx: &mut TestAppContext, key: &str| {
        cx.update_window(fixture.window.into(), |_, window, cx| window.press(key, cx))
            .unwrap();
        cx.run_until_parked();
    };
    let mut threads = Vec::new();
    for _ in 0..3 {
        cx.dispatch_action(fixture.window.into(), commands::NewOmpConversation);
        let id = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
        until(
            cx,
            "split Conversation buffer",
            Duration::from_secs(15),
            |cx| {
                fixture
                    .workspace
                    .read(cx)
                    .editor
                    .as_ref()
                    .is_some_and(|editor| {
                        editor.read(cx).active_key() == Some(&DraftKey::Thread(id))
                    })
            },
        )
        .await;
        threads.push(id);
    }
    fixture.state.update(cx, |state, cx| {
        for &id in &threads {
            if state.thread_sidebar_open(id) {
                let tab = state.thread_sidebar_tab(id);
                state.toggle_thread_sidebar(id, tab);
            }
        }
        cx.notify();
    });

    let mut layout = update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, _, cx| serde_json::to_value(workspace.dock.read(cx).dump(cx)).unwrap(),
    );
    layout["center"] = json!({
        "panel_name": "StackPanel",
        "children": threads.iter().map(|thread| json!({
            "panel_name": "TabPanel",
            "children": [{
                "panel_name": "Thread",
                "children": [],
                "info": { "panel": { "thread": thread } }
            }],
            "info": { "tabs": { "active_index": 0 } }
        })).collect::<Vec<_>>(),
        "info": {
            "stack": {
                "sizes": [400.0, 400.0, 400.0],
                "axis": 0
            }
        }
    });
    let layout = serde_json::from_value(layout).unwrap();
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        move |workspace, window, cx| {
            workspace
                .dock
                .update(cx, |dock, cx| dock.load(layout, window, cx))
                .unwrap();
            workspace.focus_region(Region::Thread, window, cx);
        },
    );
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).selected_thread),
        threads.last().copied()
    );
    assert_eq!(
        update_workspace(
            cx,
            fixture.window,
            &fixture.workspace,
            |workspace, window, cx| { workspace.focused_thread(window, cx) }
        ),
        threads.last().copied(),
        "the layout fixture must focus the rightmost thread before navigation"
    );
    for keys in [
        "secondary-alt-l",
        "secondary-alt-right",
        "secondary-alt-k",
        "secondary-alt-j",
    ] {
        press(cx, keys);
        assert_eq!(
            update_workspace(
                cx,
                fixture.window,
                &fixture.workspace,
                |workspace, window, cx| { workspace.focused_thread(window, cx) }
            ),
            threads.last().copied(),
            "{keys} must stay at the edge, not jump to Projects"
        );
    }
    for expected in [Region::Thread, Region::Thread, Region::Projects] {
        press(cx, "secondary-alt-h");
        assert_eq!(
            update_workspace(
                cx,
                fixture.window,
                &fixture.workspace,
                |workspace, window, cx| workspace.focused_region(window, cx)
            ),
            expected
        );
    }

    assert_eq!(
        cx.read(|cx| fixture.workspace.read(cx).cursor),
        Some(Row::Thread(threads[0])),
        "entering Projects anchors the visible keyboard cursor on the selected thread"
    );
    let rows = cx.read(|cx| crate::ui::workspace::projects::rows(fixture.state.read(cx), ""));
    for (keys, expected) in [
        ("home", rows.first()),
        ("end", rows.last()),
        ("pageup", rows.first()),
        ("pagedown", rows.last()),
    ] {
        press(cx, keys);
        assert_eq!(
            cx.read(|cx| fixture.workspace.read(cx).cursor),
            expected.copied(),
            "{keys} must move the Projects cursor like its Vim motion"
        );
    }
    press(cx, "k");
    assert_eq!(
        cx.read(|cx| fixture.workspace.read(cx).cursor),
        rows.get(rows.len() - 2).copied()
    );
    let middle_row = rows
        .iter()
        .position(|row| *row == Row::Thread(threads[1]))
        .unwrap();
    press(cx, "home");
    for _ in 0..middle_row {
        press(cx, "j");
    }
    press(cx, "enter");
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).selected_thread),
        Some(threads[1])
    );
    assert_eq!(
        update_workspace(
            cx,
            fixture.window,
            &fixture.workspace,
            |workspace, window, cx| workspace.focused_region(window, cx)
        ),
        Region::Thread
    );
    for (keys, expected) in [
        ("secondary-alt-l", Some(threads[2])),
        ("secondary-alt-l", Some(threads[2])),
        ("secondary-alt-h", Some(threads[1])),
        ("secondary-alt-h", Some(threads[0])),
        ("secondary-alt-h", None),
        ("secondary-alt-l", Some(threads[0])),
    ] {
        press(cx, keys);
        assert_eq!(
            update_workspace(
                cx,
                fixture.window,
                &fixture.workspace,
                |workspace, window, cx| { workspace.focused_thread(window, cx) }
            ),
            expected,
            "{keys} must visit the adjacent pane without skipping or wrapping"
        );
    }

    let mut layout = update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, _, cx| serde_json::to_value(workspace.dock.read(cx).dump(cx)).unwrap(),
    );
    layout["center"]["info"]["stack"]["axis"] = json!(1);
    layout["center"]["info"]["stack"]["sizes"] = json!([220., 300., 220.]);
    let layout = serde_json::from_value(layout).unwrap();
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace
                .dock
                .update(cx, |dock, cx| dock.load(layout, window, cx))
                .unwrap();
            workspace.focus_region(Region::Thread, window, cx);
        },
    );
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
    for (keys, expected) in [
        ("secondary-alt-k", Some(threads[0])),
        ("secondary-alt-l", Some(threads[0])),
        ("secondary-alt-j", Some(threads[1])),
        ("secondary-alt-j", Some(threads[2])),
        ("secondary-alt-j", Some(threads[2])),
        ("secondary-alt-l", Some(threads[2])),
        ("secondary-alt-h", None),
    ] {
        press(cx, keys);
        assert_eq!(
            update_workspace(
                cx,
                fixture.window,
                &fixture.workspace,
                |workspace, window, cx| { workspace.focused_thread(window, cx) }
            ),
            expected,
            "{keys} must respect stacked pane edges"
        );
    }
    quit(cx, &fixture).await;
}
