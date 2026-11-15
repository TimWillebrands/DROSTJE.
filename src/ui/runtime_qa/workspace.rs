//! Workspace shell: empty state, real-backend actions, Command, themes and the input workload.
use super::*;

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; native empty-state layout and keyboard"]
async fn empty_workspace_actions_stay_compact(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    for (mode, expected) in [("light", ThemeMode::Light), ("dark", ThemeMode::Dark)] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        until(cx, "empty workspace theme", Duration::from_secs(5), |cx| {
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
                    window.draw(cx).clear(cx);
                })
                .unwrap();
            }
            for action in ["empty-new-conversation", "empty-new-factory"] {
                let (thread, bounds) = cx
                    .update_window(fixture.window.into(), |_, window, _| {
                        (
                            window.find("Welcome").bounds(),
                            window.find(action).bounds(),
                        )
                    })
                    .unwrap();
                assert!(
                    bounds.size.width < thread.size.width * 0.5,
                    "{action} stretches across the thread pane: {bounds:?}"
                );
                assert!(thread.contains(&bounds.center()));
                assert!(bounds.center().y > thread.origin.y + thread.size.height * 0.3);
                assert!(bounds.center().y < thread.origin.y + thread.size.height * 0.7);
            }
        }
    }
    cx.simulate_keystrokes(fixture.window.into(), "secondary-n");
    until(
        cx,
        "empty-state conversation action",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).conversation_thread().is_some(),
    )
    .await;
    cx.simulate_keystrokes(fixture.window.into(), commands::CLOSE_THREAD_KEY);
    until(
        cx,
        "return to empty workspace",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).selected_thread.is_none(),
    )
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "secondary-shift-n");
    until(
        cx,
        "empty-state Factory action",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).factory_thread().is_some(),
    )
    .await;
    quit(cx, &fixture).await;
}

/// Opt-in comparative workload, not a wall-clock assertion in ordinary CI.
#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; measures real input and navigation without provider turns"]
async fn interaction_performance_workload(cx: &mut TestAppContext) {
    fn report(label: &str, mut samples: Vec<f64>) {
        samples.sort_by(f64::total_cmp);
        eprintln!(
            "PERF {label}: median={:.2}ms p95={:.2}ms max={:.2}ms",
            samples[samples.len() / 2],
            samples[samples.len() * 95 / 100],
            samples[samples.len() - 1]
        );
    }
    fn measure_keys(cx: &mut TestAppContext, window: WindowHandle<Root>, keys: &str) -> Vec<f64> {
        (0..40)
            .map(|_| {
                let start = Instant::now();
                cx.simulate_keystrokes(window.into(), keys);
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect()
    }
    let fixture = open_fixture(cx, None).await;
    let hidden_threads: usize = std::env::var("DROSTJE_PERF_HIDDEN_THREADS")
        .map(|value| {
            value
                .parse()
                .expect("DROSTJE_PERF_HIDDEN_THREADS must be an integer")
        })
        .unwrap_or(0);
    for _ in 0..hidden_threads {
        new_conversation(cx, &fixture).await;
    }
    eprintln!("PERF hidden conversations: {hidden_threads}");
    let first = new_conversation(cx, &fixture).await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.focus_composer(window, cx),
    );
    report("composer", measure_keys(cx, fixture.window, "x"));
    until(cx, "workload draft", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).drafts[&DraftKey::Thread(first)].text == "x".repeat(40)
    })
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "secondary-escape");
    cx.dispatch_action(fixture.window.into(), commands::FocusProjects);
    let mut pane_samples = Vec::new();
    for _ in 0..40 {
        for (keys, expected) in [
            ("secondary-alt-l", Region::Thread),
            ("secondary-alt-h", Region::Projects),
        ] {
            let start = Instant::now();
            cx.simulate_keystrokes(fixture.window.into(), keys);
            let actual = update_workspace(
                cx,
                fixture.window,
                &fixture.workspace,
                |workspace, window, cx| workspace.focused_region(window, cx),
            );
            pane_samples.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(actual, expected, "pane chord must take effect immediately");
        }
    }
    report("pane-navigation", pane_samples);
    cx.dispatch_action(fixture.window.into(), commands::NewFactoryThread);
    until(cx, "workload Factory form", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).factory_form.is_some()
    })
    .await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            let input = workspace
                .factory_form
                .as_ref()
                .unwrap()
                .read(cx)
                .fields
                .iter()
                .find(|field| field.key == "source")
                .unwrap()
                .input
                .clone();
            input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
            input.focus_handle(cx).focus(window, cx);
        },
    );
    report("factory-input", measure_keys(cx, fixture.window, "x"));
    assert_eq!(
        cx.read(|cx| fixture
            .state
            .read(cx)
            .thread()
            .unwrap()
            .factory
            .as_ref()
            .unwrap()
            .source
            .clone()),
        "x".repeat(40)
    );
    cx.simulate_keystrokes(fixture.window.into(), "secondary-,");
    until(cx, "workload Settings", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).dialog.is_some()
    })
    .await;
    set_dialog(cx, fixture.window, &fixture.workspace, "nvim", "");
    report("settings-input", measure_keys(cx, fixture.window, "x"));
    assert_eq!(
        cx.read(|cx| fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .unwrap()
            .read(cx)
            .fields
            .iter()
            .find(|field| field.key == "nvim")
            .unwrap()
            .input
            .read(cx)
            .value()
            .to_string()),
        "x".repeat(40)
    );
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    report(
        "tab-navigation",
        measure_keys(cx, fixture.window, "ctrl-tab"),
    );
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; no provider or Factory turns"]
async fn workspace_actions_with_real_backends(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let first = new_conversation(cx, &fixture).await;
    let before_typing =
        cx.read(|cx| fixture.state.read(cx).drafts[&DraftKey::Thread(first)].revision());
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.focus_composer(window, cx),
    );
    cx.simulate_keystrokes(
        fixture.window.into(),
        "a l p h a space b e t a escape 0 w c i w shift-g shift-a shift-m shift-m shift-a enter",
    );
    let editor = cx.read(|cx| fixture.state.read(cx).editor.clone().unwrap());
    assert!(
        !editor
            .replace_if_revision(DraftKey::Thread(first), before_typing, String::new())
            .await
            .unwrap()
    );
    until(cx, "Neovim committed edit", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).drafts[&DraftKey::Thread(first)].text == "alpha GAMMA\n"
    })
    .await;
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).drafts[&DraftKey::Thread(first)]
            .text
            .clone()),
        "alpha GAMMA\n"
    );

    cx.dispatch_action(fixture.window.into(), commands::NewFactoryThread);
    until(cx, "Factory form", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).factory_form.is_some()
    })
    .await;
    let factory = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    assert_ne!(factory, first);
    set_factory_field(
        cx,
        fixture.window,
        &fixture.workspace,
        "source",
        "not yet an ID 日本語",
    );
    cx.dispatch_action(fixture.window.into(), commands::Submit);
    cx.run_until_parked();
    assert!(fixture.store.snapshot().await.unwrap().launches.is_empty());
    assert!(cx.read(|cx| !fixture.state.read(cx).conversations.contains_key(&factory)));
    assert!(cx.read(|cx| {
        !fixture
            .state
            .read(cx)
            .drafts
            .contains_key(&DraftKey::Thread(factory))
    }));
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.state.update(cx, |state, cx| {
                state.select_thread(first);
                cx.notify();
            });
            workspace.focus_region(Region::Thread, window, cx);
        },
    );
    until(
        cx,
        "Factory input persistence",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).pending_writes == 0,
    )
    .await;
    let saved = fixture.store.snapshot().await.unwrap();
    let factory_row = saved
        .threads
        .iter()
        .find(|thread| thread.id == factory)
        .unwrap();
    assert_eq!(factory_row.kind, ThreadKind::Factory);
    assert_eq!(
        factory_row.factory.as_ref().unwrap().source,
        "not yet an ID 日本語"
    );
    assert!(factory_row.draft.is_empty() && factory_row.session_ref.is_none());
    assert_eq!(
        saved
            .threads
            .iter()
            .find(|thread| thread.id == first)
            .unwrap()
            .draft,
        "alpha GAMMA\n"
    );

    until(cx, "Conversation restored", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).editor_selection == Some(DraftKey::Thread(first))
    })
    .await;
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).drafts[&DraftKey::Thread(first)]
            .text
            .clone()),
        "alpha GAMMA\n"
    );
    // The dock's thread tabs retain both kinds; cycling never rewrites either draft.
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).open_threads.clone()),
        vec![first, factory]
    );
    cx.simulate_keystrokes(fixture.window.into(), "ctrl-tab");
    until(cx, "next thread tab", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).selected_thread == Some(factory)
    })
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "ctrl-shift-tab");
    until(cx, "previous thread tab", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).selected_thread == Some(first)
    })
    .await;
    cx.simulate_keystrokes(fixture.window.into(), commands::CLOSE_THREAD_KEY);
    until(
        cx,
        "closing selects adjacent tab",
        Duration::from_secs(5),
        |cx| {
            fixture.state.read(cx).selected_thread == Some(factory)
                && fixture.state.read(cx).open_threads == vec![factory]
        },
    )
    .await;
    fixture.state.update(cx, |state, cx| {
        state.select_thread(first);
        state.select_thread(first);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).open_threads.clone()),
        vec![factory, first]
    );
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).drafts[&DraftKey::Thread(first)]
            .text
            .clone()),
        "alpha GAMMA\n"
    );
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, _, cx| {
            workspace.state.update(cx, |state, cx| {
                state.select_thread(factory);
                cx.notify();
            })
        },
    );
    quit(cx, &fixture).await;
    println!("DROSTJE_QA_DATA={}", fixture.data.display());
    assert_eq!(
        fixture.store.snapshot().await.unwrap().settings["spaces"]["sessions"]
            [cx.read(|cx| fixture.state.read(cx).active_space.to_string())]["open_threads"],
        json!([factory, first])
    );
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; native Command/focus/theme integration"]
async fn thread_workspace_keyboard_and_themes(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let conversation = new_conversation(cx, &fixture).await;
    cx.dispatch_action(fixture.window.into(), commands::NewFactoryThread);
    until(cx, "Factory form ready", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).factory_form.is_some()
    })
    .await;
    let factory = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    cx.dispatch_action(fixture.window.into(), commands::RenameThread);
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            let (target, _, input, _) = workspace.inline_rename().unwrap();
            assert_eq!(target, crate::ui::workspace::RenameTarget::Thread(factory));
            input.update(cx, |input, cx| {
                input.set_value("Factory needle".to_owned(), window, cx)
            });
            input.focus_handle(cx).focus(window, cx);
        },
    );
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.state.update(cx, |state, cx| {
                state.select_thread(conversation);
                cx.notify();
            });
            workspace.focus_region(Region::Thread, window, cx);
        },
    );
    open_palette(cx, &fixture).await;
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window.into(), "n e e d l e enter");
    until(
        cx,
        "filtered Factory selection",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).selected_thread == Some(factory),
    )
    .await;
    assert!(cx.read(|cx| !fixture.state.read(cx).conversations.contains_key(&factory)));
    assert!(cx.read(|cx| fixture.workspace.read(cx).palette.is_none()));

    let count = cx.read(|cx| fixture.state.read(cx).snapshot.threads.len());
    open_palette(cx, &fixture).await;
    cx.run_until_parked();
    cx.simulate_keystrokes(
        fixture.window.into(),
        "n e w space c o n v e r s a t i o n enter",
    );
    until(cx, "one palette command", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).snapshot.threads.len() == count + 1
    })
    .await;
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).snapshot.threads.len()),
        count + 1
    );

    open_palette(cx, &fixture).await;
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window.into(), "s e t t i n g s enter");
    until(
        cx,
        "Settings remains open after palette",
        Duration::from_secs(5),
        |cx| {
            fixture.workspace.read(cx).dialog.is_some()
                && fixture.workspace.read(cx).palette.is_none()
        },
    )
    .await;
    assert!(
        cx.update_window(fixture.window.into(), |_, window, cx| window
            .has_active_sheet(cx))
            .unwrap()
    );
    let saved_root =
        cx.read(|cx| fixture.state.read(cx).snapshot.settings["factory_data_root"].clone());
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("settings-sidebar").click("0-0-3", cx);
    })
    .unwrap();
    cx.run_until_parked();
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "factory_data_root",
        "relative/path",
    );
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("dialog-confirm", cx);
    })
    .unwrap();
    until(
        cx,
        "Settings validation retains input",
        Duration::from_secs(5),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .dialog
                .as_ref()
                .is_some_and(|dialog| {
                    let dialog = dialog.read(cx);
                    dialog.error.is_some()
                        && dialog.value("factory_data_root", cx) == "relative/path"
                })
        },
    )
    .await;
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).snapshot.settings["factory_data_root"].clone()),
        saved_root
    );
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.click("dialog-cancel", cx);
    })
    .unwrap();
    until(
        cx,
        "Settings draft retained after dismissal",
        Duration::from_secs(5),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .dialog
                .as_ref()
                .is_some_and(|dialog| {
                    !dialog.read(cx).hosted()
                        && dialog.read(cx).error.is_some()
                        && dialog.read(cx).value("factory_data_root", cx) == "relative/path"
                })
        },
    )
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "secondary-,");
    until(
        cx,
        "Settings draft reopened",
        Duration::from_secs(5),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .dialog
                .as_ref()
                .is_some_and(|dialog| {
                    dialog.read(cx).hosted()
                        && dialog.read(cx).error.is_some()
                        && dialog.read(cx).value("factory_data_root", cx) == "relative/path"
                })
        },
    )
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    until(
        cx,
        "Settings draft hidden again",
        Duration::from_secs(5),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .dialog
                .as_ref()
                .is_some_and(|dialog| !dialog.read(cx).hosted())
        },
    )
    .await;

    fixture.state.update(cx, |state, cx| {
        let mut project = state.project().unwrap().clone();
        project.archived = true;
        state.save_project(project);
        cx.notify();
    });
    open_palette(cx, &fixture).await;
    cx.run_until_parked();
    cx.simulate_keystrokes(
        fixture.window.into(),
        "n e w space c o n v e r s a t i o n enter",
    );
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).snapshot.threads.len()),
        count + 1
    );
    assert!(cx.read(|cx| fixture.workspace.read(cx).palette.is_some()));
    cx.simulate_keystrokes(fixture.window.into(), "escape escape");
    fixture.state.update(cx, |state, cx| {
        let mut project = state.project().unwrap().clone();
        project.archived = false;
        state.save_project(project);
        cx.notify();
    });

    open_palette(cx, &fixture).await;
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window.into(), "z z z escape");
    assert!(cx.read(|cx| fixture.workspace.read(cx).palette.is_some()));
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    until(cx, "palette cancelled", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).palette.is_none()
    })
    .await;
    for (key, region) in [
        ("secondary-1", Region::Projects),
        ("secondary-2", Region::Thread),
    ] {
        cx.simulate_keystrokes(fixture.window.into(), key);
        assert_eq!(
            update_workspace(
                cx,
                fixture.window,
                &fixture.workspace,
                |workspace, window, cx| workspace.focused_region(window, cx)
            ),
            region
        );
    }
    for (mode, expected) in [("light", ThemeMode::Light), ("dark", ThemeMode::Dark)] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        until(cx, "theme mode applied", Duration::from_secs(5), |cx| {
            Theme::global(cx).mode == expected
        })
        .await;
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
        })
        .unwrap();
    }
    // Editing appearance is transactional: neither a choice nor Cancel previews it.
    cx.simulate_keystrokes(fixture.window.into(), "secondary-,");
    until(
        cx,
        "Settings appearance controls",
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
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            let root = workspace
                .dialog
                .as_ref()
                .unwrap()
                .read(cx)
                .fields
                .iter()
                .find(|field| field.key == "factory_data_root")
                .unwrap()
                .input
                .clone();
            root.update(cx, |input, cx| {
                input.set_value(saved_root.as_str().unwrap().to_owned(), window, cx);
            });
            workspace
                .dialog
                .as_ref()
                .unwrap()
                .read(cx)
                .select("theme_mode")
                .unwrap()
                .clone()
                .update(cx, |select, cx| {
                    select.set_selected_value(&"light".to_owned(), window, cx)
                });
        },
    );
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| Theme::global(cx).mode), ThemeMode::Dark);
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    until(cx, "appearance cancelled", Duration::from_secs(5), |cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| !dialog.read(cx).hosted())
    })
    .await;
    assert_eq!(cx.read(|cx| Theme::global(cx).mode), ThemeMode::Dark);
    cx.simulate_keystrokes(fixture.window.into(), "secondary-,");
    until(cx, "Settings reopened", Duration::from_secs(5), |cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).hosted())
    })
    .await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            let select = workspace
                .dialog
                .as_ref()
                .unwrap()
                .read(cx)
                .select("theme_mode")
                .unwrap()
                .clone();
            assert_eq!(
                select.read(cx).selected_value().map(String::as_str),
                Some("light")
            );
            select.update(cx, |select, cx| {
                select.set_selected_value(&"light".to_owned(), window, cx)
            });
        },
    );
    cx.run_until_parked();
    for _ in 0..3 {
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    cx.run_until_parked();
    until(cx, "appearance saved", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).dialog.is_none() && Theme::global(cx).mode == ThemeMode::Light
    })
    .await;
    fixture.store.flush().await.unwrap();
    assert_eq!(
        fixture.store.snapshot().await.unwrap().settings["theme_mode"],
        json!("light")
    );
    fixture.state.update(cx, |state, cx| {
        state.set_setting("theme_mode", json!("dark"));
        cx.notify();
    });

    until(
        cx,
        "initial theme load",
        Duration::from_secs(5),
        crate::ui::theme::initial_load_finished,
    )
    .await;
    let custom = fixture.data.join("themes/runtime.json");
    for font_size in [16., 20.] {
        std::fs::write(&custom, serde_json::to_vec(&json!({
            "name": "Runtime QA",
            "themes": [{"name": "Runtime dark", "mode": "dark", "font.size": font_size, "mono_font.size": font_size - 3.}]
        })).unwrap()).unwrap();
        until(
            cx,
            "custom theme discovered",
            Duration::from_secs(5),
            |cx| {
                crate::ui::theme::theme_names(ThemeMode::Dark, cx)
                    .iter()
                    .any(|name| name == "Runtime dark")
            },
        )
        .await;
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_dark", json!("Runtime dark"));
            cx.notify();
        });
        until(
            cx,
            "custom typography reloaded",
            Duration::from_secs(5),
            |cx| f32::from(Theme::global(cx).font_size) == font_size,
        )
        .await;
        for (width, height) in [(1440., 900.), (900., 600.)] {
            cx.simulate_window_resize(fixture.window.into(), size(px(width), px(height)));
            for _ in 0..3 {
                cx.run_until_parked();
                cx.update_window(fixture.window.into(), |_, window, cx| {
                    window.simulate_next_frame(cx);
                    window.refresh();
                    window.draw(cx).clear(cx);
                })
                .unwrap();
            }
            let thread_id = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
            let (thread, composer) = cx
                .update_window(fixture.window.into(), |_, window, _| {
                    (
                        window
                            .find(gpui_kit::SharedString::from(format!("Thread:{thread_id}")))
                            .bounds(),
                        window.find("composer-content").bounds(),
                    )
                })
                .unwrap();
            assert!(
                f32::from(thread.size.width) >= font_size * 27.,
                "{width}×{height} at {font_size}: {thread:?}"
            );
            assert!(
                f32::from(thread.size.height) >= font_size * 14.,
                "{width}×{height} at {font_size}: {thread:?}"
            );
            assert!(composer.origin.x >= thread.origin.x && composer.origin.y >= thread.origin.y);
            assert!(
                composer.origin.x + composer.size.width
                    <= thread.origin.x + thread.size.width + px(1.)
            );
            assert!(
                composer.origin.y + composer.size.height
                    <= thread.origin.y + thread.size.height + px(1.)
            );
        }
    }
    cx.simulate_keystrokes(fixture.window.into(), "secondary-1");
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
    }
    for (keys, expected) in [
        ("secondary-alt-l", Region::Thread),
        ("secondary-alt-h", Region::Projects),
        ("secondary-alt-h", Region::Projects),
        ("secondary-alt-l", Region::Thread),
    ] {
        cx.simulate_keystrokes(fixture.window.into(), keys);
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
    std::fs::remove_file(&custom).unwrap();
    until(
        cx,
        "removed theme falls back",
        Duration::from_secs(5),
        |cx| {
            !crate::ui::theme::theme_names(ThemeMode::Dark, cx)
                .iter()
                .any(|name| name == "Runtime dark")
                && f32::from(Theme::global(cx).font_size) == 15.
        },
    )
    .await;
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).snapshot.settings["theme_dark"].clone()),
        json!("Runtime dark")
    );
    assert!(cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .notices
            .iter()
            .any(|notice| notice.text.contains("Runtime dark"))
    }));
    fixture.state.update(cx, |state, cx| {
        state.set_setting("theme_mode", json!("system"));
        cx.notify();
    });
    quit(cx, &fixture).await;
}
