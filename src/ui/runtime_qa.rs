//! Opt-in native component QA with isolated state; never submits provider turns.
use crate::{
    app::{AppState, Bootstrap},
    model::{DraftKey, ThreadId, ThreadKind},
    platform,
    store::Store,
    ui::{
        commands,
        workspace::{Region, Workspace},
    },
};
use gpui_kit::{
    App, AppContext, Context, Entity, Focusable, TestAppContext, VisualTestContext, Window,
    WindowHandle,
    component::{Root, Theme, ThemeMode, WindowExt},
    px, size,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};
use uuid::Uuid;

async fn until(
    cx: &mut TestAppContext,
    label: &str,
    timeout: Duration,
    predicate: impl Fn(&App) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        cx.run_until_parked();
        if cx.read(&predicate) {
            return;
        }
        assert!(Instant::now() < deadline, "Timed out: {label}");
        smol::Timer::after(Duration::from_millis(20)).await;
    }
}

fn update_workspace<R>(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    update: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
) -> R {
    cx.update_window(window.into(), |_, window, cx| {
        workspace.update(cx, |workspace, cx| update(workspace, window, cx))
    })
    .unwrap()
}

fn set_dialog(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    key: &str,
    value: &str,
) {
    update_workspace(cx, window, workspace, |workspace, window, cx| {
        let field = workspace
            .dialog
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .find(|field| field.key == key)
            .unwrap();
        field.input.update(cx, |input, cx| {
            input.set_value(value.to_owned(), window, cx)
        });
        let focus = field.input.focus_handle(cx);
        focus.focus(window, cx);
    });
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
}

fn set_factory_field(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    key: &str,
    value: &str,
) {
    update_workspace(cx, window, workspace, |workspace, window, cx| {
        let field = workspace
            .factory_form
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .find(|field| field.key == key)
            .unwrap();
        field.input.update(cx, |input, cx| {
            input.set_value(value.to_owned(), window, cx)
        });
    });
    cx.run_until_parked();
}

struct Fixture {
    window: WindowHandle<Root>,
    workspace: Entity<Workspace>,
    state: Entity<AppState>,
    store: Store,
    data: PathBuf,
}

async fn open_fixture(cx: &mut TestAppContext, data: Option<PathBuf>) -> Fixture {
    open_fixture_with_editor(cx, data, None).await
}

async fn open_fixture_with_editor(
    cx: &mut TestAppContext,
    data: Option<PathBuf>,
    executable: Option<&str>,
) -> Fixture {
    cx.dispatcher.allow_parking();
    let fixture = PathBuf::from(
        std::env::var_os("DROST_RUNTIME_FIXTURE")
            .expect("Set DROST_RUNTIME_FIXTURE to a disposable fixture"),
    );
    let repository = platform::inspect_repository(&fixture.join("target-repo")).unwrap();
    assert!(
        repository
            .root
            .starts_with(std::fs::canonicalize(&fixture).unwrap())
    );
    assert_eq!(
        std::fs::read_to_string(repository.root.join("notes.txt")).unwrap(),
        "before\n"
    );
    let data = data.unwrap_or_else(|| fixture.join(format!("drost-qa-{}", Uuid::new_v4())));
    let store = Store::open(data.clone()).unwrap();
    if let Some(executable) = executable {
        store
            .set_setting("executables".into(), json!({"nvim": executable}))
            .await
            .unwrap();
    }
    store
        .set_setting(
            "factory_data_root".into(),
            json!(fixture.join("factory-data")),
        )
        .await
        .unwrap();
    let snapshot = store.snapshot().await.unwrap();
    let repositories = snapshot
        .projects
        .iter()
        .map(|project| {
            (
                project.id,
                platform::inspect_repository(&project.root).map_err(|error| format!("{error:#}")),
            )
        })
        .collect();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::theme::initialize(Some(&snapshot.settings), Some(&data), cx);
        commands::bind_keys(cx);
    });
    let bootstrap = Bootstrap {
        data_dir: data.clone(),
        store: store.clone(),
        snapshot,
        diagnostics: BTreeMap::new(),
        repositories,
    };
    let mut view = None;
    let window = cx.add_window(|window, cx| {
        let state = cx.new(|cx| AppState::new(bootstrap, cx));
        let workspace = cx.new(|cx| Workspace::new(state, window, cx));
        view = Some(workspace.clone());
        let content = cx.new(|_| crate::ui::workspace::WorkspaceRoot { workspace });
        Root::new(content, window, cx)
    });
    let workspace = view.unwrap();
    let state = cx.read(|cx| workspace.read(cx).state.clone());
    cx.run_until_parked();
    if cx.read(|cx| state.read(cx).snapshot.projects.is_empty()) {
        cx.dispatch_action(window.into(), commands::AddProject);
        set_dialog(
            cx,
            window,
            &workspace,
            "path",
            &repository.root.to_string_lossy(),
        );
        cx.simulate_keystrokes(window.into(), "cmd-enter");
        until(cx, "project registration", Duration::from_secs(10), |cx| {
            state.read(cx).snapshot.projects.len() == 1
        })
        .await;
    }
    Fixture {
        window,
        workspace,
        state,
        store,
        data,
    }
}

async fn new_conversation(cx: &mut TestAppContext, fixture: &Fixture) -> ThreadId {
    cx.dispatch_action(fixture.window.into(), commands::NewConversation);
    let id = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    until(
        cx,
        "real Conversation buffer",
        Duration::from_secs(15),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .editor
                .as_ref()
                .is_some_and(|view| view.read(cx).active_key() == Some(&DraftKey::Thread(id)))
        },
    )
    .await;
    id
}

async fn quit(cx: &mut TestAppContext, fixture: &Fixture) {
    cx.dispatch_action(fixture.window.into(), commands::Quit);
    until(cx, "orderly shutdown", Duration::from_secs(15), |cx| {
        fixture.state.read(cx).shutdown_complete
    })
    .await;
    fixture.store.flush().await.unwrap();
}

#[gpui_kit::test]
#[ignore = "requires disposable DROST_RUNTIME_FIXTURE; native empty-state layout and keyboard"]
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
            let mut visual = VisualTestContext::from_window(fixture.window.into(), cx);
            let thread = visual.debug_bounds("Thread").unwrap();
            for action in ["empty-new-conversation", "empty-new-factory"] {
                let bounds = visual.debug_bounds(action).unwrap();
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
    cx.simulate_keystrokes(fixture.window.into(), "cmd-n");
    until(
        cx,
        "empty-state conversation action",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).conversation_thread().is_some(),
    )
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "cmd-w");
    until(
        cx,
        "return to empty workspace",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).selected_thread.is_none(),
    )
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "cmd-shift-n");
    until(
        cx,
        "empty-state Factory action",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).factory_thread().is_some(),
    )
    .await;
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROST_RUNTIME_FIXTURE; no provider or Factory turns"]
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
    cx.simulate_keystrokes(fixture.window.into(), "cmd-w");
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
    println!("DROST_QA_DATA={}", fixture.data.display());
    assert_eq!(
        fixture.store.snapshot().await.unwrap().settings["open_threads"],
        json!([factory, first])
    );
}

#[gpui_kit::test]
#[ignore = "requires disposable DROST_RUNTIME_FIXTURE and installed Neovim; no provider turns"]
async fn editor_startup_failure_and_restart_preserve_draft(cx: &mut TestAppContext) {
    let fixture = open_fixture_with_editor(cx, None, Some("/nonexistent/drost-qa-neovim")).await;
    cx.dispatch_action(fixture.window.into(), commands::NewConversation);
    let id = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    until(
        cx,
        "missing Neovim setup error",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).editor_error.is_some(),
    )
    .await;
    assert!(cx.read(|cx| fixture.state.read(cx).editor.is_none()));
    let executable = platform::resolve_executable("nvim", None).unwrap();
    fixture.state.update(cx, |state, cx| {
        state.set_setting("executables", json!({"nvim": executable}));
        cx.notify();
    });
    cx.dispatch_action(fixture.window.into(), commands::RestartEditor);
    until(
        cx,
        "configured Neovim activation",
        Duration::from_secs(5),
        |cx| {
            fixture.workspace.read(cx).editor_selection == Some(DraftKey::Thread(id))
                && fixture.state.read(cx).editor_error.is_none()
        },
    )
    .await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.focus_composer(window, cx),
    );
    cx.simulate_keystrokes(fixture.window.into(), "s a f e escape");
    until(cx, "committed Neovim draft", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).drafts[&DraftKey::Thread(id)].text == "safe"
    })
    .await;
    let editor = cx.read(|cx| fixture.state.read(cx).editor.clone().unwrap());
    editor
        .try_command(crate::neovim::NvimCommand::Input(":qa!<CR>".into()))
        .unwrap();
    until(cx, "Neovim process failure", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).editor_error.is_some()
    })
    .await;
    assert!(editor.snapshot(DraftKey::Thread(id)).await.is_err());
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).drafts[&DraftKey::Thread(id)]
            .text
            .clone()),
        "safe"
    );
    cx.dispatch_action(fixture.window.into(), commands::RestartEditor);
    until(cx, "Neovim recovery", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).editor_error.is_none()
            && fixture.workspace.read(cx).editor_selection == Some(DraftKey::Thread(id))
    })
    .await;
    assert_eq!(
        editor.snapshot(DraftKey::Thread(id)).await.unwrap().text,
        "safe"
    );
    cx.dispatch_action(fixture.window.into(), commands::NewConversation);
    let second = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.sync_session(window, cx);
        },
    );
    cx.dispatch_action(fixture.window.into(), commands::RestartEditor);
    until(
        cx,
        "restart during draft activation",
        Duration::from_secs(5),
        |cx| {
            fixture.state.read(cx).editor_error.is_none()
                && fixture.workspace.read(cx).editor_selection == Some(DraftKey::Thread(second))
        },
    )
    .await;
    assert_eq!(
        editor.snapshot(DraftKey::Thread(id)).await.unwrap().text,
        "safe"
    );
    assert_eq!(
        editor
            .snapshot(DraftKey::Thread(second))
            .await
            .unwrap()
            .text,
        ""
    );
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.focus_composer(window, cx),
    );
    cx.simulate_keystrokes(fixture.window.into(), "cmd-escape");
    cx.update_window(fixture.window.into(), |_, window, cx| {
        let focus = fixture
            .workspace
            .read(cx)
            .editor
            .as_ref()
            .unwrap()
            .focus_handle(cx);
        assert!(!focus.is_focused(window));
    })
    .unwrap();
    quit(cx, &fixture).await;
    assert_eq!(
        fixture
            .store
            .snapshot()
            .await
            .unwrap()
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .unwrap()
            .draft,
        "safe"
    );
}

async fn open_palette(cx: &mut TestAppContext, fixture: &Fixture) {
    cx.simulate_keystrokes(fixture.window.into(), "cmd-shift-p");
    until(cx, "palette opened", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).palette.is_some()
    })
    .await;
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
}

#[gpui_kit::test]
#[ignore = "requires disposable DROST_RUNTIME_FIXTURE; native Command/focus/theme integration"]
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
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "name",
        "Factory needle",
    );
    cx.simulate_keystrokes(fixture.window.into(), "cmd-enter");
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
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "factory_data_root",
        "relative/path",
    );
    cx.simulate_keystrokes(fixture.window.into(), "cmd-enter");
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
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    until(cx, "Settings dismissed", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).dialog.is_none()
    })
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
        ("cmd-1", Region::Projects),
        ("cmd-2", Region::Thread),
        ("cmd-3", Region::Output),
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
    cx.simulate_keystrokes(fixture.window.into(), "cmd-,");
    until(
        cx,
        "Settings appearance controls",
        Duration::from_secs(5),
        |cx| fixture.workspace.read(cx).dialog.is_some(),
    )
    .await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace
                .dialog
                .as_ref()
                .unwrap()
                .select("theme_mode")
                .unwrap()
                .update(cx, |select, cx| {
                    select.set_selected_value(&"light".to_owned(), window, cx)
                });
        },
    );
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| Theme::global(cx).mode), ThemeMode::Dark);
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    until(cx, "appearance cancelled", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).dialog.is_none()
    })
    .await;
    assert_eq!(cx.read(|cx| Theme::global(cx).mode), ThemeMode::Dark);
    cx.simulate_keystrokes(fixture.window.into(), "cmd-,");
    until(cx, "Settings reopened", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).dialog.is_some()
    })
    .await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace
                .dialog
                .as_ref()
                .unwrap()
                .select("theme_mode")
                .unwrap()
                .update(cx, |select, cx| {
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
    cx.simulate_keystrokes(fixture.window.into(), "cmd-enter");
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
            let mut visual = VisualTestContext::from_window(fixture.window.into(), cx);
            let thread = visual.debug_bounds("Thread").unwrap();
            let composer = visual.debug_bounds("composer-content").unwrap();
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
    cx.simulate_keystrokes(fixture.window.into(), "cmd-1");
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
        ("ctrl-w l", Region::Thread),
        ("ctrl-w h", Region::Projects),
        ("ctrl-w h", Region::Projects),
        ("ctrl-w l", Region::Thread),
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
                && f32::from(Theme::global(cx).font_size) == 16.
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

#[gpui_kit::test]
#[ignore = "requires disposable DROST_RUNTIME_FIXTURE; exercises orderly quit against a real SQLite write lock"]
async fn orderly_quit_flushes_before_platform_deadline(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    new_conversation(cx, &fixture).await;
    until(cx, "queued writes drained", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).pending_writes == 0
    })
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "cmd-,");
    until(
        cx,
        "native Settings before Quit",
        Duration::from_secs(5),
        |cx| fixture.workspace.read(cx).dialog.is_some(),
    )
    .await;
    let connection = rusqlite::Connection::open(fixture.data.join("drost.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    fixture.state.update(cx, |state, _| {
        state.set_setting("quit_marker", json!("durable-before-platform-quit"))
    });
    cx.dispatch_action(fixture.window.into(), commands::Quit);
    until(
        cx,
        "cleanup before platform quit",
        Duration::from_secs(1),
        |cx| fixture.state.read(cx).shutting_down,
    )
    .await;
    smol::Timer::after(Duration::from_millis(350)).await;
    cx.run_until_parked();
    assert!(!cx.read(|cx| fixture.state.read(cx).shutdown_complete));
    connection.execute_batch("COMMIT").unwrap();
    until(cx, "cleanup completion", Duration::from_secs(10), |cx| {
        fixture.state.read(cx).shutdown_complete
    })
    .await;
    assert_eq!(
        fixture
            .store
            .snapshot()
            .await
            .unwrap()
            .settings
            .get("quit_marker"),
        Some(&json!("durable-before-platform-quit"))
    );
}

#[gpui_kit::test]
#[ignore = "requires persisted disposable DROST_QA_DATA; observer only, no Factory dispatch"]
async fn layout_and_factory_runtime_recovery(cx: &mut TestAppContext) {
    let data = PathBuf::from(std::env::var_os("DROST_QA_DATA").unwrap());
    let root = PathBuf::from(std::env::var_os("DROST_RUNTIME_FIXTURE").unwrap());
    assert!(
        std::fs::canonicalize(&data)
            .unwrap()
            .starts_with(std::fs::canonicalize(root).unwrap())
    );
    let fixture = open_fixture(cx, Some(data)).await;
    let saved = fixture.store.snapshot().await.unwrap();
    let selected: ThreadId =
        serde_json::from_value(saved.settings["selected_thread"].clone()).unwrap();
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).selected_thread),
        Some(selected)
    );
    assert!(cx.read(|cx| fixture.state.read(cx).thread().unwrap().kind == ThreadKind::Factory));
    assert!(cx.read(|cx| fixture.state.read(cx).conversations.is_empty()));
    for launch in &saved.launches {
        until(
            cx,
            "recorded Factory observation",
            Duration::from_secs(5),
            |cx| fixture.state.read(cx).factory_runs.contains_key(&launch.id),
        )
        .await;
        fixture
            .state
            .update(cx, |state, cx| state.select_factory_launch(launch.id, cx));
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
        })
        .unwrap();
        assert_eq!(
            cx.read(|cx| fixture.state.read(cx).selected_thread),
            Some(launch.factory_thread_id)
        );
        assert!(cx.read(|cx| fixture.workspace.read(cx).factory_form.is_none()));
        assert!(cx.read(|cx| fixture.state.read(cx).conversations.is_empty()));
    }
    fixture.state.update(cx, |state, cx| {
        state.select_thread(selected);
        cx.notify();
    });
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
    cx.simulate_keystrokes(fixture.window.into(), "cmd-3");
    assert_eq!(
        update_workspace(
            cx,
            fixture.window,
            &fixture.workspace,
            |workspace, window, cx| workspace.focused_region(window, cx)
        ),
        Region::Output
    );
    cx.simulate_keystrokes(fixture.window.into(), "cmd-j cmd-1");
    assert_eq!(
        update_workspace(
            cx,
            fixture.window,
            &fixture.workspace,
            |workspace, window, cx| workspace.focused_region(window, cx)
        ),
        Region::Projects
    );
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable fixture; proves failed final persistence never starts Factory"]
async fn factory_start_requires_persisted_configuration(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let fixture = open_fixture(cx, None).await;
    let executable = fixture.data.join("factory-fixture");
    let invoked = fixture.data.join("factory-invoked");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 1\n",
            invoked.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    fixture.state.update(cx, |state, cx| {
        state.set_setting("executables", json!({"factory":executable}));
        cx.notify();
    });
    cx.dispatch_action(fixture.window.into(), commands::NewFactoryThread);
    until(cx, "editable Factory", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).factory_form.is_some()
    })
    .await;
    set_factory_field(cx, fixture.window, &fixture.workspace, "source", "42");
    until(
        cx,
        "configuration persisted",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).pending_writes == 0,
    )
    .await;
    let blocker = rusqlite::Connection::open(fixture.data.join("drost.sqlite3")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    cx.dispatch_action(fixture.window.into(), commands::Submit);
    until(cx, "final write refused", Duration::from_secs(10), |cx| {
        fixture.state.read(cx).storage_error.is_some()
    })
    .await;
    until(cx, "start released", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).pending_factory.is_empty()
    })
    .await;
    assert!(
        !invoked.exists(),
        "Factory ran before its final configuration was durable"
    );
    assert!(fixture.store.snapshot().await.unwrap().launches.is_empty());
    blocker.execute_batch("ROLLBACK").unwrap();
    fixture
        .state
        .update(cx, |state, cx| state.begin_shutdown(true, cx));
    until(
        cx,
        "explicit loss-approved shutdown",
        Duration::from_secs(10),
        |cx| fixture.state.read(cx).shutdown_complete,
    )
    .await;
}
