//! Settings › Hosts: setup, native pickers and remote failure UI.
use super::*;

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; Host setup, native pickers and remote failure UI without provider credentials or turns"]
async fn remote_host_settings_selection_and_failure_are_keyboard_safe(cx: &mut TestAppContext) {
    fn select_hosts_page(cx: &mut TestAppContext, fixture: &Fixture) {
        settle_frames(cx, fixture);
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.within("settings-sidebar").click("0-1", cx);
        })
        .unwrap();
        settle_frames(cx, fixture);
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.render_frame(cx);
            let host = window.find("remote_host").bounds();
            assert!(
                host.size.width > px(0.) && host.origin.y < window.viewport_size().height,
                "Hosts navigation must show its actual form, not another Settings group's fields"
            );
        })
        .unwrap();
    }
    let fixture = open_fixture_with_editor(cx, None, Some("/nonexistent/drostje-qa-neovim")).await;
    let window = fixture.window.into();
    let token_env = format!("DROSTJE_QA_MISSING_REMOTE_TOKEN_{}", std::process::id());
    assert!(
        std::env::var_os(&token_env).is_none(),
        "QA requires an unset credential reference"
    );
    cx.dispatch_action(window, commands::NewConversation);
    let thread = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    let project = cx.read(|cx| fixture.state.read(cx).selected_project.unwrap());

    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(window, "s e t t i n g s enter");
    until(cx, "Host settings opened", Duration::from_secs(5), |cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).hosted())
    })
    .await;
    select_hosts_page(cx, &fixture);
    for (key, value) in [
        ("remote_host_id", "qa-host"),
        ("remote_host_name", "QA Host"),
        ("remote_relay_url", "not-a-relay"),
        ("remote_token_env", token_env.as_str()),
        ("remote_project_key", "workspace"),
    ] {
        set_dialog(cx, fixture.window, &fixture.workspace, key, value);
    }
    // set_dialog updates retained inputs, including fields virtualized below the
    // viewport. Their focus handle need not be mounted; use the visible Settings
    // footer, as the other Settings validation scenarios do.
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("dialog-confirm", cx);
    })
    .unwrap();
    until(
        cx,
        "invalid Host stays editable",
        Duration::from_secs(5),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .dialog
                .as_ref()
                .is_some_and(|dialog| dialog.read(cx).error.is_some())
        },
    )
    .await;
    assert!(cx.read(|cx| fixture.state.read(cx).remote_hosts().is_empty()));
    assert_eq!(
        cx.read(|cx| fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .value("remote_project_key", cx)),
        "workspace"
    );
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "remote_relay_url",
        "wss://127.0.0.1:1/remote",
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("dialog-confirm", cx);
    })
    .unwrap();
    until(
        cx,
        "Host saved from Settings",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).remote_hosts().len() == 1,
    )
    .await;
    assert_eq!(
        cx.read(
            |cx| fixture.state.read(cx).remote_hosts()[0].projects[&project.to_string()].clone()
        ),
        "workspace"
    );

    // Editing a saved Host loads all fields, and changing the selected Host cannot discard edits.
    cx.dispatch_action(window, commands::Settings);
    select_hosts_page(cx, &fixture);
    cx.update_window(window, |_, window, cx| {
        let select = fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .select("remote_host")
            .unwrap()
            .clone();
        select.update(cx, |select, cx| {
            select.set_selected_value(&"qa-host".to_owned(), window, cx);
            cx.emit(gpui_kit::component::select::SelectEvent::Confirm(Some(
                "qa-host".to_owned(),
            )));
        });
    })
    .unwrap();
    until(cx, "saved Host loaded", Duration::from_secs(5), |cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .value("remote_host_name", cx)
            == "QA Host"
    })
    .await;
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "remote_host_name",
        "QA Host renamed",
    );
    cx.update_window(window, |_, window, cx| {
        let select = fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .select("remote_host")
            .unwrap()
            .clone();
        select.update(cx, |select, cx| {
            select.set_selected_value(&String::new(), window, cx);
            cx.emit(gpui_kit::component::select::SelectEvent::Confirm(Some(
                String::new(),
            )));
        });
    })
    .unwrap();
    settle_frames(cx, &fixture);
    assert_eq!(
        cx.read(|cx| fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .value("remote_host_name", cx)),
        "QA Host renamed"
    );
    assert_eq!(
        cx.read(|cx| fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .value("remote_host", cx)),
        "qa-host"
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("dialog-confirm", cx);
    })
    .unwrap();
    until(cx, "Host edit saved", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).remote_hosts()[0].name == "QA Host renamed"
    })
    .await;

    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(window, "s w i t c h space e x e c u t i o n enter");
    settle_frames(cx, &fixture);
    cx.simulate_keystrokes(window, "g g j enter");
    until(
        cx,
        "Host selected by Vim picker",
        Duration::from_secs(5),
        |cx| {
            fixture
                .state
                .read(cx)
                .thread()
                .unwrap()
                .remote
                .as_ref()
                .is_some_and(|remote| remote.host == "qa-host")
        },
    )
    .await;
    until(
        cx,
        "missing Host credentials visible",
        Duration::from_secs(10),
        |cx| {
            fixture
                .state
                .read(cx)
                .conversations
                .get(&thread)
                .is_some_and(|session| session.error.is_some())
        },
    )
    .await;
    assert!(cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .working_tree_of(fixture.state.read(cx).thread().unwrap())
            .is_none()
    }));
    assert!(cx.read(|cx| {
        commands::Command::NewNeovim
            .disabled_reason(fixture.state.read(cx))
            .is_some()
    }));
    assert!(cx.read(|cx| {
        commands::Command::CommitChanges
            .disabled_reason(fixture.state.read(cx))
            .is_some()
    }));
    cx.dispatch_action(window, commands::RetryConversation);
    until(
        cx,
        "explicit reconnect reports missing credential",
        Duration::from_secs(10),
        |cx| {
            fixture
                .state
                .read(cx)
                .conversations
                .get(&thread)
                .is_some_and(|session| session.error.is_some())
        },
    )
    .await;

    // A failed remote stream leaves the Host's active turn outcome unknown. Reconnect
    // must remain available for reconciliation, unlike restarting an active local harness.
    fixture.state.update(cx, |state, cx| {
        state.active_threads.insert(thread);
        assert!(
            commands::Command::RetryConversation
                .disabled_reason(state)
                .is_none(),
            "failed remote streams must be reconnectable while their turn remains active"
        );
        let location = state
            .snapshot
            .threads
            .iter_mut()
            .find(|candidate| candidate.id == thread)
            .unwrap()
            .remote
            .take();
        assert!(
            commands::Command::RetryConversation
                .disabled_reason(state)
                .is_some(),
            "an active local harness must still refuse reconnect"
        );
        state
            .snapshot
            .threads
            .iter_mut()
            .find(|candidate| candidate.id == thread)
            .unwrap()
            .remote = location;
        state.active_threads.remove(&thread);
        cx.notify();
    });

    for (mode, expected) in [("light", ThemeMode::Light), ("dark", ThemeMode::Dark)] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        until(cx, "Host theme", Duration::from_secs(5), |cx| {
            Theme::global(cx).mode == expected
        })
        .await;
        for (width, height) in [(1440., 900.), (900., 600.)] {
            cx.simulate_window_resize(window, size(px(width), px(height)));
            settle_frames(cx, &fixture);
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert!(
                    window
                        .find("conversation-execution-location")
                        .bounds()
                        .size
                        .width
                        > px(0.)
                );
                assert!(window.find("composer-remote-workspace").bounds().size.width > px(0.));
            })
            .unwrap();
            cx.dispatch_action(window, commands::SwitchExecutionHost);
            settle_frames(cx, &fixture);
            cx.simulate_keystrokes(window, "shift-g g g escape");
            settle_frames(cx, &fixture);
            assert!(cx.read(|cx| fixture.workspace.read(cx).palette.is_none()));
            assert!(
                cx.update_window(window, |_, window, cx| window.focused(cx).is_some())
                    .unwrap()
            );
        }
    }

    // A file that exists locally must still fail at the Host, never launch a local editor.
    cx.dispatch_action(
        window,
        commands::OpenFileReference {
            reference: crate::file_refs::FileReference {
                path: "notes.txt".into(),
                line: None,
                column: None,
            },
            thread: Some(thread),
        },
    );
    until(
        cx,
        "remote file setup error",
        Duration::from_secs(10),
        |cx| {
            fixture
                .state
                .read(cx)
                .remote_file
                .as_ref()
                .is_some_and(|file| file.error.is_some())
        },
    )
    .await;
    settle_frames(cx, &fixture);
    assert!(cx.read(|cx| fixture.workspace.read(cx).neovim_tabs(cx).is_empty()));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("remote-file-view").bounds().size.width > px(0.));
    })
    .unwrap();
    cx.simulate_keystrokes(window, "escape");
    until(cx, "remote file dismissed", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).remote_file.is_none()
    })
    .await;
    assert!(
        cx.read(|cx| fixture
            .state
            .read(cx)
            .remove_remote_host_reason("qa-host")
            .is_some()),
        "a bound Conversation prevents removing its Host"
    );

    cx.dispatch_action(window, commands::SwitchExecutionHost);
    settle_frames(cx, &fixture);
    cx.simulate_keystrokes(window, "g g enter");
    until(
        cx,
        "return to local before first turn",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).thread().unwrap().remote.is_none(),
    )
    .await;

    // The same removal command is discoverable in the palette and used by Settings.
    cx.dispatch_action(window, commands::Settings);
    select_hosts_page(cx, &fixture);
    cx.update_window(window, |_, window, cx| {
        let select = fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .select("remote_host")
            .unwrap()
            .clone();
        select.update(cx, |select, cx| {
            select.set_selected_value(&"qa-host".to_owned(), window, cx);
            cx.emit(gpui_kit::component::select::SelectEvent::Confirm(Some(
                "qa-host".to_owned(),
            )));
        });
    })
    .unwrap();
    until(cx, "removal Host selected", Duration::from_secs(5), |cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .value("remote_host", cx)
            == "qa-host"
    })
    .await;
    cx.simulate_keystrokes(window, "escape");
    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(
        window,
        "r e m o v e space c o n f i g u r e d space h o s t enter",
    );
    settle_frames(cx, &fixture);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("remove-host-confirmation").bounds().size.width > px(0.));
        assert!(
            !window.has_active_sheet(cx),
            "removal never stacks over Settings"
        );
    })
    .unwrap();
    cx.simulate_keystrokes(window, "escape");
    until(
        cx,
        "cancel restores Settings",
        Duration::from_secs(5),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .dialog
                .as_ref()
                .is_some_and(|dialog| dialog.read(cx).hosted())
        },
    )
    .await;
    assert_eq!(cx.read(|cx| fixture.state.read(cx).remote_hosts().len()), 1);
    let retained = cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .value("nvim", cx)
    });
    settle_frames(cx, &fixture);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.has_active_sheet(cx),
            "Cancel must restore the actual Settings host"
        );
        assert!(
            !window.has_active_dialog(cx),
            "the confirmation must close before Settings reopens"
        );
        assert!(
            window.focused(cx).is_some(),
            "restored Settings must regain keyboard focus"
        );
    })
    .unwrap();
    // Reopening Settings can reset its virtualized group to Appearance. Use the
    // shared palette Action rather than assuming the Host button is still mounted.
    cx.simulate_keystrokes(window, "escape");
    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(
        window,
        "r e m o v e space c o n f i g u r e d space h o s t enter",
    );
    settle_frames(cx, &fixture);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    until(cx, "unused Host removed", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).remote_hosts().is_empty()
    })
    .await;
    until(
        cx,
        "removal restores Settings",
        Duration::from_secs(5),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .dialog
                .as_ref()
                .is_some_and(|dialog| dialog.read(cx).hosted())
        },
    )
    .await;
    settle_frames(cx, &fixture);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.has_active_sheet(cx),
            "removal must restore the actual Settings host"
        );
        assert!(
            !window.has_active_dialog(cx),
            "confirmation cleanup must not remain above Settings"
        );
        assert!(
            window.focused(cx).is_some(),
            "Settings must regain focus after removal"
        );
    })
    .unwrap();
    assert_eq!(
        cx.read(|cx| fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .value("nvim", cx)),
        retained
    );
    assert!(cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .value("remote_host_id", cx)
            .is_empty()
    }));
    cx.simulate_keystrokes(window, "escape");
    settle_frames(cx, &fixture);
    fixture.state.update(cx, |state, cx| {
        state
            .snapshot
            .threads
            .iter_mut()
            .find(|candidate| candidate.id == thread)
            .unwrap()
            .execution_started = true;
        cx.notify();
    });
    assert!(cx.read(|cx| {
        commands::Command::SwitchExecutionHost
            .disabled_reason(fixture.state.read(cx))
            .is_some()
    }));
    cx.dispatch_action(window, commands::SwitchExecutionHost);
    assert!(cx.read(|cx| fixture.workspace.read(cx).palette.is_none()));
    cx.dispatch_action(window, commands::NewFactoryThread);
    assert!(cx.read(|cx| {
        commands::Command::SwitchExecutionHost
            .disabled_reason(fixture.state.read(cx))
            .is_some()
    }));
    quit(cx, &fixture).await;
}
