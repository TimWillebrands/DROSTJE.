//! Opt-in native component QA with isolated state; never submits provider turns.
use crate::{
    app::{AppState, Bootstrap},
    model::{DraftKey, ThreadId, ThreadKind},
    platform,
    store::Store,
    ui::{
        commands,
        sidebar::Row,
        workspace::{Region, Workspace},
    },
};
use gpui_kit::{
    App, AppContext, ClipboardItem, Context, Entity, Focusable, Image, ImageFormat, Modifiers,
    TestAppContext, VisualTestContext, Window, WindowHandle,
    component::{Root, Theme, ThemeMode, WindowExt},
    px, size,
    test::TestWindowExt,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
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
            .read(cx)
            .fields
            .iter()
            .find(|field| field.key == key)
            .unwrap()
            .input
            .clone();
        field.update(cx, |input, cx| {
            input.set_value(value.to_owned(), window, cx)
        });
        let focus = field.focus_handle(cx);
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
            .read(cx)
            .fields
            .iter()
            .find(|field| field.key == key)
            .unwrap()
            .input
            .clone();
        field.update(cx, |input, cx| {
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
        std::env::var_os("DROSTJE_RUNTIME_FIXTURE")
            .expect("Set DROSTJE_RUNTIME_FIXTURE to a disposable fixture"),
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
    let data = data.unwrap_or_else(|| fixture.join(format!("drostje-qa-{}", Uuid::new_v4())));
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
        cx.simulate_keystrokes(window.into(), "secondary-enter");
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
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; sidebar harness picker and clipboard paste against a real store"]
async fn project_plus_picks_harness_and_clipboard_image_attaches(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let project = cx.read(|cx| fixture.state.read(cx).snapshot.projects[0].id);
    let count = cx.read(|cx| fixture.state.read(cx).snapshot.threads.len());
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
    let selector: &'static str = Box::leak(format!("new-conversation-{project}").into_boxed_str());
    let plus = visual
        .debug_bounds(selector)
        .expect("project row shows its '+'");
    visual.simulate_click(plus.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).snapshot.threads.len()),
        count,
        "opening the picker must not create a conversation"
    );
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).selected_project),
        Some(project),
        "the clicked project owns the picker action"
    );
    cx.simulate_keystrokes(fixture.window.into(), "down down enter");
    until(
        cx,
        "picker created a Claude Code conversation",
        Duration::from_secs(5),
        |cx| {
            let state = fixture.state.read(cx);
            state.snapshot.threads.len() == count + 1
                && state
                    .conversation_thread()
                    .is_some_and(|thread| thread.project_id == project)
        },
    )
    .await;
    assert_eq!(
        cx.read(|cx| fixture
            .state
            .read(cx)
            .conversation_thread()
            .unwrap()
            .harness
            .clone()),
        "claude-code"
    );
    let conversation = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    assert!(cx.read(|cx| fixture.state.read(cx).selected_project == Some(project)));

    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([1, 2, 3, 255]),
    ))
    .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
    .unwrap();
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_image(&Image {
            format: ImageFormat::Png,
            bytes: png,
            id: 1,
        }))
    });
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.focus_region(Region::Thread, window, cx),
    );
    cx.simulate_keystrokes(fixture.window.into(), commands::PASTE_IMAGE_KEY);
    until(cx, "pasted image attached", Duration::from_secs(10), |cx| {
        fixture
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == conversation)
            .is_some_and(|thread| {
                thread.attachments.len() == 1
                    && thread.attachments[0].mime == "image/png"
                    && thread.attachments[0].width == 2
            })
    })
    .await;

    let notices = cx.read(|cx| fixture.state.read(cx).notices.len());
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("just text".into())));
    cx.dispatch_action(fixture.window.into(), commands::PasteImage);
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).notices.len()),
        notices + 1,
        "a text-only clipboard reports a warning instead of attaching"
    );
    assert_eq!(
        cx.read(|cx| fixture
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == conversation)
            .unwrap()
            .attachments
            .len()),
        1
    );
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; new thread tabs draw their own title"]
async fn new_thread_tab_draws_its_title(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let id = new_conversation(cx, &fixture).await;
    cx.run_until_parked();
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    })
    .unwrap();
    // A panel docked without its presentation handle falls back to the layout name "Thread".
    let selector: &'static str = Box::leak(format!("thread-tab-{id}").into_boxed_str());
    let mut visual = VisualTestContext::from_window(fixture.window.into(), cx);
    assert!(
        visual.debug_bounds(selector).is_some(),
        "the tab bar draws the thread's own title element"
    );
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; native sidebar hover actions"]
async fn thread_sidebar_hover_actions_rename_and_archive_immediately(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let id = new_conversation(cx, &fixture).await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.focus_composer(window, cx);
        },
    );
    cx.simulate_keystrokes(fixture.window.into(), "k e e p cmd-escape");
    until(cx, "archive draft", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).thread().unwrap().draft == "keep"
    })
    .await;
    let row = format!("thread-{id}");
    let actions = format!("thread-actions-{id}");
    let rename = format!("rename-thread-{id}");
    let archive = format!("archive-thread-{id}");

    cx.update_window(fixture.window.into(), |_, window, cx| {
        assert!(
            window.try_find(actions.clone()).is_none(),
            "thread actions stay out of the tree until the row is hovered"
        );
        window.hover(row.clone(), cx);
        assert!(
            window.find(actions.clone()).visible(),
            "hover paints the thread action lane"
        );
        assert!(window.find(rename.clone()).visible());
        assert!(window.find(archive.clone()).visible());
    })
    .unwrap();
    smol::Timer::after(Duration::from_secs(1)).await;
    cx.run_until_parked();
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find(actions.clone()).visible(),
            "thread actions remain painted after the row tooltip opens"
        );
        window.click(rename.clone(), cx);
    })
    .unwrap();

    assert!(cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| matches!(dialog.read(cx).kind, crate::ui::settings::DialogKind::RenameThread(target) if target == id))
    }));
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "name",
        "Manually renamed",
    );
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    until(cx, "manual thread rename", Duration::from_secs(5), |cx| {
        fixture
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .is_some_and(|thread| thread.title == "Manually renamed")
    })
    .await;

    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.hover(row.clone(), cx);
        assert!(window.find(archive.clone()).visible());
        window.click(archive.clone(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(cx.read(|cx| fixture.workspace.read(cx).dialog.is_none()));
    cx.read(|cx| {
        let state = fixture.state.read(cx);
        let thread = state.thread().unwrap();
        assert_eq!(state.selected_thread, Some(id));
        assert!(thread.archived, "one click archives without confirmation");
        assert_eq!(thread.title, "Manually renamed");
        assert_eq!(thread.draft, "keep");
        assert!(state.show_archived, "archived history remains accessible");
    });
    assert_eq!(
        update_workspace(
            cx,
            fixture.window,
            &fixture.workspace,
            |workspace, window, cx| { workspace.focused_region(window, cx) }
        ),
        Region::Projects
    );
    wait_for_saved_thread(cx, &fixture, id, |thread| {
        thread.archived && thread.title == "Manually renamed" && thread.draft == "keep"
    })
    .await;

    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; immediate archive shares action guards and palette dispatch"]
async fn thread_archive_action_guards_and_palette_are_immediate(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let id = new_conversation(cx, &fixture).await;
    fixture.state.update(cx, |state, cx| {
        state.active_threads.insert(id);
        cx.notify();
    });
    cx.dispatch_action(fixture.window.into(), commands::ArchiveThread);
    assert!(cx.read(|cx| !fixture.state.read(cx).thread().unwrap().archived));
    assert!(cx.read(|cx| fixture.workspace.read(cx).dialog.is_none()));
    fixture.state.update(cx, |state, cx| {
        state.active_threads.remove(&id);
        state.pending_factory.insert(id);
        cx.notify();
    });
    cx.dispatch_action(fixture.window.into(), commands::ArchiveThread);
    assert!(cx.read(|cx| !fixture.state.read(cx).thread().unwrap().archived));
    assert!(cx.read(|cx| fixture.workspace.read(cx).dialog.is_none()));
    fixture.state.update(cx, |state, cx| {
        state.pending_factory.remove(&id);
        cx.notify();
    });

    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(
        fixture.window.into(),
        "a r c h i v e space t h r e a d enter",
    );
    cx.run_until_parked();
    cx.read(|cx| {
        let workspace = fixture.workspace.read(cx);
        assert!(workspace.palette.is_none());
        assert!(workspace.dialog.is_none());
        let state = fixture.state.read(cx);
        assert_eq!(state.selected_thread, Some(id));
        assert!(state.thread().unwrap().archived);
    });
    wait_for_saved_thread(cx, &fixture, id, |thread| thread.archived).await;

    cx.dispatch_action(fixture.window.into(), commands::ArchiveProject);
    assert!(cx.read(|cx| !fixture.state.read(cx).project().unwrap().archived));
    assert!(cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| {
                matches!(
                    dialog.read(cx).kind,
                    crate::ui::settings::DialogKind::ArchiveProject(_)
                )
            })
    }));
    cx.dispatch_action(fixture.window.into(), commands::Dismiss);
    quit(cx, &fixture).await;
}

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

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; native live diff sidebar, no provider turn"]
async fn conversation_diff_sidebar_tracks_working_tree(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let conversation = new_conversation(cx, &fixture).await;
    let root = cx.read(|cx| fixture.state.read(cx).project().unwrap().root.clone());
    fs::write(root.join("notes.txt"), "after\n").unwrap();
    fs::write(root.join("sidebar-new.txt"), "visible\n").unwrap();

    cx.dispatch_action(fixture.window.into(), commands::RefreshConversationDiff);
    until(cx, "working tree diff", Duration::from_secs(5), |cx| {
        fixture
            .state
            .read(cx)
            .conversation_diff(conversation)
            .and_then(|diff| diff.result.as_ref())
            .is_some_and(|result| {
                result.as_ref().is_ok_and(|diff| {
                    diff.patch.contains("-before")
                        && diff.patch.contains("+after")
                        && diff.patch.contains("+visible")
                })
            })
    })
    .await;
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
    let composer = visual.debug_bounds("composer-row").unwrap();
    let sidebar = visual.debug_bounds("conversation-diff-sidebar").unwrap();
    assert!(
        sidebar.origin.y + sidebar.size.height <= composer.origin.y,
        "the changes sidebar stops above the composer"
    );
    assert!(
        composer.origin.x + composer.size.width >= sidebar.origin.x + sidebar.size.width,
        "the composer spans the full pane width beneath the sidebar"
    );
    assert!(visual.debug_bounds("conversation-diff-patch").is_some());

    let other = new_conversation(cx, &fixture).await;
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

    cx.dispatch_action(fixture.window.into(), commands::ToggleConversationDiffs);
    until(cx, "collapsed diff sidebar", Duration::from_secs(5), |cx| {
        !fixture.state.read(cx).thread_sidebar_open(conversation)
    })
    .await;
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
    assert!(visual.debug_bounds("conversation-diff-sidebar").is_none());
    assert!(visual.debug_bounds("toggle-conversation-diffs").is_some());

    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.state.update(cx, |state, cx| {
                state.select_thread(other);
                cx.notify();
            });
            workspace.focus_region(Region::Thread, window, cx);
        },
    );
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
    assert!(
        visual.debug_bounds("conversation-diff-sidebar").is_some(),
        "collapsing one Conversation must not collapse another"
    );

    fs::write(root.join("notes.txt"), "before\n").unwrap();
    fs::remove_file(root.join("sidebar-new.txt")).unwrap();
    quit(cx, &fixture).await;
}

async fn wait_for_saved_thread(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    id: ThreadId,
    matches: impl Fn(&crate::model::Thread) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        let saved = fixture.store.snapshot().await.unwrap();
        if saved
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .is_some_and(&matches)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "latest thread edit was not committed: {:?}",
            saved.threads.iter().find(|thread| thread.id == id)
        );
        smol::Timer::after(Duration::from_millis(20)).await;
    }
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; typing stays local until focus loss or quit"]
async fn typing_saves_on_focus_loss_and_quit_not_idle(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    cx.update_window(fixture.window.into(), |_, window, _| {
        window.activate_window()
    })
    .unwrap();
    cx.run_until_parked();
    let id = new_conversation(cx, &fixture).await;
    until(
        cx,
        "initial persistence and harness",
        Duration::from_secs(15),
        |cx| {
            let state = fixture.state.read(cx);
            state.pending_writes == 0
                && state.conversations.get(&id).is_some_and(|session| {
                    !matches!(session.state, crate::harness::ConversationState::Starting)
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
    cx.simulate_keystrokes(fixture.window.into(), "a b c");
    until(cx, "in-memory typing", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).drafts[&DraftKey::Thread(id)].text == "abc"
    })
    .await;
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("save-indicator").is_none(),
            "typing must not start saving or show an active-save indicator"
        );
    })
    .unwrap();
    smol::Timer::after(Duration::from_millis(1800)).await;
    cx.run_until_parked();
    let saved = fixture.store.snapshot().await.unwrap();
    assert_eq!(
        saved
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .unwrap()
            .draft,
        "",
        "pausing while focused must not save the draft"
    );

    // Include a final keystroke immediately before focus loss: the semantic
    // snapshot must include it even if its normal draft event arrives later.
    cx.simulate_keystrokes(fixture.window.into(), "d cmd-escape");
    wait_for_saved_thread(cx, &fixture, id, |thread| thread.draft == "abcd").await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.focus_composer(window, cx);
        },
    );
    cx.simulate_keystrokes(fixture.window.into(), "e");
    VisualTestContext::from_window(fixture.window.into(), cx).deactivate_window();
    wait_for_saved_thread(cx, &fixture, id, |thread| thread.draft == "abcde").await;
    cx.update_window(fixture.window.into(), |_, window, _| {
        window.activate_window()
    })
    .unwrap();
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window.into(), "f");
    until(
        cx,
        "latest unsaved revision",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).drafts[&DraftKey::Thread(id)].text == "abcdef",
    )
    .await;
    quit(cx, &fixture).await;
    let saved = fixture.store.snapshot().await.unwrap();
    assert_eq!(
        saved
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .unwrap()
            .draft,
        "abcdef"
    );
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; Factory input commits on blur and Return"]
async fn factory_typing_stays_local_until_focus_loss_or_commit(cx: &mut TestAppContext) {
    fn focus_field(cx: &mut TestAppContext, fixture: &Fixture, key: &str) {
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
                    .input(key)
                    .clone();
                input.focus_handle(cx).focus(window, cx);
            },
        );
        cx.run_until_parked();
    }
    let fixture = open_fixture(cx, None).await;
    cx.update_window(fixture.window.into(), |_, window, _| {
        window.activate_window()
    })
    .unwrap();
    cx.run_until_parked();
    cx.dispatch_action(fixture.window.into(), commands::NewFactoryThread);
    until(cx, "Factory form ready", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).factory_form.is_some()
            && fixture.state.read(cx).pending_writes == 0
    })
    .await;
    let id = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    focus_field(cx, &fixture, "source");
    cx.simulate_keystrokes(fixture.window.into(), "a b c");
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
        "abc"
    );
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("save-indicator").is_none(),
            "editing a Factory field must not show an active-save indicator"
        );
    })
    .unwrap();
    smol::Timer::after(Duration::from_millis(1800)).await;
    cx.run_until_parked();
    let saved = fixture.store.snapshot().await.unwrap();
    assert_eq!(
        saved
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .unwrap()
            .factory
            .as_ref()
            .unwrap()
            .source,
        ""
    );
    cx.simulate_keystrokes(fixture.window.into(), "d cmd-1");
    cx.update_window(fixture.window.into(), |_, window, cx| {
        let input = fixture
            .workspace
            .read(cx)
            .factory_form
            .as_ref()
            .unwrap()
            .read(cx)
            .input("source");
        assert!(
            !input.focus_handle(cx).is_focused(window),
            "source still focused: {:?}",
            window.context_stack()
        );
    })
    .unwrap();
    wait_for_saved_thread(cx, &fixture, id, |thread| {
        thread.factory.as_ref().unwrap().source == "abcd"
    })
    .await;
    focus_field(cx, &fixture, "model");
    cx.simulate_keystrokes(fixture.window.into(), "m enter");
    wait_for_saved_thread(cx, &fixture, id, |thread| {
        thread.factory.as_ref().unwrap().model == "m"
    })
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
        for (keys, expected) in [("ctrl-w l", Region::Thread), ("ctrl-w h", Region::Projects)] {
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
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE and installed Neovim; no provider turns"]
async fn editor_startup_failure_and_restart_preserve_draft(cx: &mut TestAppContext) {
    let fixture = open_fixture_with_editor(cx, None, Some("/nonexistent/drostje-qa-neovim")).await;
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
    cx.simulate_keystrokes(fixture.window.into(), "secondary-escape");
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
    cx.simulate_keystrokes(fixture.window.into(), "secondary-shift-p");
    until(cx, "palette opened", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).palette.is_some()
    })
    .await;
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
    }
}
#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; native palette command"]
async fn conversation_harness_commands_are_visible_and_actionable(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    assert_eq!(
        cx.read(|cx| {
            fixture
                .state
                .read(cx)
                .project()
                .unwrap()
                .defaults
                .interactive_harness
                .clone()
        }),
        "omp"
    );
    let before = cx.read(|cx| fixture.state.read(cx).snapshot.threads.len());
    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(
        fixture.window.into(),
        "n e w space p i space c o n v e r s a t i o n enter",
    );
    until(
        cx,
        "visible Pi conversation command",
        Duration::from_secs(5),
        |cx| {
            fixture.workspace.read(cx).palette.is_none()
                && fixture.state.read(cx).snapshot.threads.len() == before + 1
        },
    )
    .await;
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).thread().unwrap().harness.clone()),
        "pi"
    );
    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(
        fixture.window.into(),
        "n e w space c l a u d e space c o d e space c o n v e r s a t i o n enter",
    );
    until(
        cx,
        "visible Claude Code conversation command",
        Duration::from_secs(5),
        |cx| {
            fixture.workspace.read(cx).palette.is_none()
                && fixture.state.read(cx).snapshot.threads.len() == before + 2
        },
    )
    .await;
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).thread().unwrap().harness.clone()),
        "claude-code"
    );
    quit(cx, &fixture).await;
}

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
    let second = new_conversation(cx, &fixture).await;
    cx.update_window(fixture.window.into(), |_, window, _| {
        window.activate_window()
    })
    .unwrap();
    let first_row = gpui_kit::SharedString::from(format!("thread-{first}"));
    let second_row = gpui_kit::SharedString::from(format!("thread-{second}"));
    cx.simulate_keystrokes(fixture.window.into(), "secondary-2 i a b c");
    until(
        cx,
        "draft before pane navigation",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).drafts[&DraftKey::Thread(second)].text == "abc",
    )
    .await;

    for mode in ["light", "dark"] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        for (width, height) in [(1440., 900.), (900., 600.)] {
            cx.simulate_window_resize(fixture.window.into(), size(px(width), px(height)));
            cx.simulate_keystrokes(fixture.window.into(), "secondary-2 i");
            cx.run_until_parked();
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(has_focus_outline(window, cx, "prompt-editor"));
                assert!(!has_focus_outline(window, cx, second_row.clone()));
            })
            .unwrap();
            cx.simulate_keystrokes(fixture.window.into(), "ctrl-w l ctrl-w ctrl-l");
            cx.run_until_parked();
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(
                    window.find("prompt-editor").focused(),
                    Some(true),
                    "moving right at the edge must leave Neovim focused"
                );
            })
            .unwrap();

            cx.simulate_keystrokes(
                fixture.window.into(),
                if width == 900. {
                    "ctrl-w h"
                } else {
                    "secondary-escape ctrl-w h"
                },
            );
            cx.run_until_parked();
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("Projects").focused(), Some(true));
                assert!(!has_focus_outline(window, cx, "prompt-editor"));
                assert!(has_focus_outline(window, cx, second_row.clone()));
            })
            .unwrap();
            cx.simulate_keystrokes(fixture.window.into(), "k");
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

            // Native search owns its own focus cue, not the remembered list cursor.
            cx.simulate_keystrokes(fixture.window.into(), "/");
            cx.run_until_parked();
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(!has_focus_outline(window, cx, first_row.clone()));
                assert!(!has_focus_outline(window, cx, "prompt-editor"));
            })
            .unwrap();
            cx.simulate_keystrokes(fixture.window.into(), "escape");
        }
    }
    // Pane navigation must not type into the retained Neovim buffer or reset
    // insert mode; returning to the editor resumes at the same cursor.
    cx.simulate_keystrokes(fixture.window.into(), "secondary-2 i d");
    until(
        cx,
        "draft after pane navigation",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).drafts[&DraftKey::Thread(second)].text == "abcd",
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
    let mut threads = Vec::new();
    for _ in 0..3 {
        threads.push(new_conversation(cx, &fixture).await);
    }

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
    // At the right edge, neither moving right nor moving vertically may pick
    // Projects just because its full-height pane has a different centre.
    for keys in ["ctrl-w l", "ctrl-w ctrl-l", "ctrl-w k", "ctrl-w j"] {
        cx.simulate_keystrokes(fixture.window.into(), keys);
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
        cx.simulate_keystrokes(fixture.window.into(), "ctrl-w h");
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
        threads.last().copied().map(Row::Thread),
        "entering Projects anchors the visible keyboard cursor on the selected thread"
    );
    let rows = cx.read(|cx| crate::ui::sidebar::rows(fixture.state.read(cx), ""));
    for (keys, expected) in [
        ("home", rows.first()),
        ("end", rows.last()),
        ("pageup", rows.first()),
        ("pagedown", rows.last()),
    ] {
        cx.simulate_keystrokes(fixture.window.into(), keys);
        assert_eq!(
            cx.read(|cx| fixture.workspace.read(cx).cursor),
            expected.copied(),
            "{keys} must move the Projects cursor like its Vim motion"
        );
    }
    cx.simulate_keystrokes(fixture.window.into(), "k");
    assert_eq!(
        cx.read(|cx| fixture.workspace.read(cx).cursor),
        Some(Row::Thread(threads[1]))
    );
    cx.simulate_keystrokes(fixture.window.into(), "enter");
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
        ("ctrl-w l", Some(threads[2])),
        ("ctrl-w l", Some(threads[2])),
        ("ctrl-w h", Some(threads[1])),
        ("ctrl-w h", Some(threads[0])),
        ("ctrl-w h", None),
        ("ctrl-w l", Some(threads[0])),
    ] {
        cx.simulate_keystrokes(fixture.window.into(), keys);
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

    // Stacked threads sit beside the full-height Projects pane. Its centre
    // must not make it a vertical neighbour of any of those threads.
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
        ("ctrl-w k", Some(threads[0])),
        ("ctrl-w l", Some(threads[0])),
        ("ctrl-w j", Some(threads[1])),
        ("ctrl-w j", Some(threads[2])),
        ("ctrl-w j", Some(threads[2])),
        ("ctrl-w l", Some(threads[2])),
        ("ctrl-w h", None),
    ] {
        cx.simulate_keystrokes(fixture.window.into(), keys);
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
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "name",
        "Factory needle",
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
        window.within("settings-sidebar").click("0-0-2", cx);
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
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
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
        fixture.workspace.read(cx).dialog.is_none()
    })
    .await;
    assert_eq!(cx.read(|cx| Theme::global(cx).mode), ThemeMode::Dark);
    cx.simulate_keystrokes(fixture.window.into(), "secondary-,");
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
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; exercises orderly quit against a real SQLite write lock"]
async fn orderly_quit_flushes_before_platform_deadline(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    new_conversation(cx, &fixture).await;
    until(cx, "queued writes drained", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).pending_writes == 0
    })
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "secondary-,");
    until(
        cx,
        "native Settings before Quit",
        Duration::from_secs(5),
        |cx| fixture.workspace.read(cx).dialog.is_some(),
    )
    .await;
    let connection = rusqlite::Connection::open(fixture.data.join("drostje.sqlite3")).unwrap();
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
#[ignore = "requires persisted disposable DROSTJE_QA_DATA; observer only, no Factory dispatch"]
async fn layout_and_factory_runtime_recovery(cx: &mut TestAppContext) {
    let data = PathBuf::from(std::env::var_os("DROSTJE_QA_DATA").unwrap());
    let root = PathBuf::from(std::env::var_os("DROSTJE_RUNTIME_FIXTURE").unwrap());
    assert!(
        std::fs::canonicalize(&data)
            .unwrap()
            .starts_with(std::fs::canonicalize(root).unwrap())
    );
    let fixture = open_fixture(cx, Some(data)).await;
    let saved = fixture.store.snapshot().await.unwrap();
    let selected: ThreadId = serde_json::from_value(
        saved.settings["spaces"]["sessions"]
            [cx.read(|cx| fixture.state.read(cx).active_space.to_string())]["selected_thread"]
            .clone(),
    )
    .unwrap();
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
    cx.simulate_keystrokes(fixture.window.into(), "secondary-3");
    assert_eq!(
        update_workspace(
            cx,
            fixture.window,
            &fixture.workspace,
            |workspace, window, cx| workspace.focused_region(window, cx)
        ),
        Region::Thread
    );
    // Focus output opens the selected thread's sidebar on its Output tab.
    assert!(cx.read(|cx| {
        let state = fixture.state.read(cx);
        state.thread_sidebar_open(selected)
            && state.thread_sidebar_tab(selected) == crate::app::SidebarTab::Output
    }));
    cx.simulate_keystrokes(fixture.window.into(), "secondary-j cmd-1");
    assert!(cx.read(|cx| !fixture.state.read(cx).thread_sidebar_open(selected)));
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
    let blocker = rusqlite::Connection::open(fixture.data.join("drostje.sqlite3")).unwrap();
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
    cx.simulate_keystrokes(fixture.window.into(), "s a f e cmd-escape");
    wait_for_saved_thread(cx, &fixture, thread, |thread| thread.draft == "safe").await;
    cx.dispatch_action(fixture.window.into(), commands::ToggleProjects);
    cx.run_until_parked();

    // Discover creation through the real palette, not a test-only entry point.
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
            && crate::ui::sidebar::rows(state, "").is_empty()
    }));
    assert!(cx.read(|cx| {
        commands::Command::NewConversation
            .disabled_reason(fixture.state.read(cx))
            .is_some()
    }));

    // Re-adding a registered working tree attaches it to this Space instead of
    // leaving the empty Space without a usable project.
    cx.dispatch_action(fixture.window.into(), commands::AddProject);
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "path",
        &project_root.to_string_lossy(),
    );
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
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
        !crate::ui::sidebar::rows(fixture.state.read(cx), "").contains(&Row::Thread(thread))
    }));
    let research_thread = new_conversation(cx, &fixture).await;
    assert!(cx.read(|cx| {
        let rows = crate::ui::sidebar::rows(fixture.state.read(cx), "");
        rows.contains(&Row::Thread(research_thread)) && !rows.contains(&Row::Thread(thread))
    }));

    // Exercise the same Workspace transition used by the thread context-menu
    // destination. The destination gains the project, and the thread disappears
    // from this Space without being duplicated.
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
            && !crate::ui::sidebar::rows(state, "").contains(&Row::Thread(research_thread))
    }));

    // The switcher and add control remain usable in both themes at minimum size.
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
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "name",
        "Investigation",
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

    // Moving from the native picker moves the project and every thread, not its data.
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
        state.open_threads.is_empty() && crate::ui::sidebar::rows(state, "").is_empty()
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
        crate::ui::sidebar::rows(fixture.state.read(cx), "")
            .contains(&crate::ui::sidebar::Row::Thread(thread))
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
            && crate::ui::sidebar::rows(state, "").is_empty()
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

/// The thread each tab group displays, read from the dock's own layout dump.
fn displayed_threads(layout: &serde_json::Value) -> Vec<String> {
    let mut displayed = Vec::new();
    let mut stack = vec![layout];
    while let Some(node) = stack.pop() {
        match node {
            serde_json::Value::Object(map) => {
                if map.get("panel_name").and_then(|name| name.as_str()) == Some("TabPanel") {
                    let active = map["info"]["tabs"]["active_index"].as_u64().unwrap_or(0);
                    if let Some(thread) =
                        map["children"][active as usize]["info"]["panel"]["thread"].as_str()
                    {
                        displayed.push(thread.to_owned());
                    }
                }
                stack.extend(map.values());
            }
            serde_json::Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    displayed
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; opening a background tab from Projects"]
async fn projects_selection_displays_the_threads_tab(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let mut threads = Vec::new();
    for _ in 0..3 {
        threads.push(new_conversation(cx, &fixture).await);
    }
    let displayed = |cx: &mut TestAppContext| {
        let layout = update_workspace(
            cx,
            fixture.window,
            &fixture.workspace,
            |workspace, _, cx| serde_json::to_value(workspace.dock.read(cx).dump(cx)).unwrap(),
        );
        displayed_threads(&layout["center"])
    };
    assert_eq!(displayed(cx), vec![threads[2].to_string()]);

    // The first tab sits behind the other two; a click on its Projects row shows it.
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.select_row(Row::Thread(threads[0]), true, window, cx),
    );
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).selected_thread),
        Some(threads[0])
    );
    assert_eq!(displayed(cx), vec![threads[0].to_string()]);
    assert_eq!(
        update_workspace(
            cx,
            fixture.window,
            &fixture.workspace,
            |workspace, window, cx| workspace.focused_region(window, cx)
        ),
        Region::Projects,
        "selecting from Projects keeps keyboard focus in the list"
    );

    // A selection made by other means is shown too, from Projects again.
    cx.simulate_keystrokes(fixture.window.into(), "ctrl-tab");
    cx.run_until_parked();
    assert_eq!(displayed(cx), vec![threads[1].to_string()]);
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.select_row(Row::Thread(threads[2]), true, window, cx),
    );
    cx.run_until_parked();
    assert_eq!(displayed(cx), vec![threads[2].to_string()]);
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; tests real unsupported-native-host error, mixed tabs and persistence without a provider turn"]
async fn terminal_tabs_keep_failure_local_and_do_not_persist_sessions(cx: &mut TestAppContext) {
    let fixture = open_fixture_with_editor(cx, None, Some("/nonexistent/drostje-qa-neovim")).await;
    cx.dispatch_action(fixture.window.into(), commands::NewFactoryThread);
    let factory = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    cx.dispatch_action(fixture.window.into(), commands::NewTerminal);
    until(
        cx,
        "terminal host failure is visible",
        Duration::from_secs(10),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .active_terminal(cx)
                .is_some_and(|terminal| terminal.read(cx).error().is_some())
        },
    )
    .await;
    let press = |cx: &mut TestAppContext, key: &str| {
        cx.update_window(fixture.window.into(), |_, window, cx| window.press(key, cx))
            .unwrap();
        cx.run_until_parked();
    };
    assert_eq!(cx.read(|cx| fixture.state.read(cx).selected_thread), None);
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).open_threads.clone()),
        [factory]
    );
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).snapshot.threads.len()),
        1
    );
    assert!(cx.read(|cx| {
        commands::Command::RestartTerminal
            .disabled_reason_in_workspace(fixture.workspace.read(cx), cx)
            .is_none()
    }));

    // Typing on a terminal pane must never start a Factory run or enter the composer.
    for key in ["j", "k", "i", "secondary-enter"] {
        press(cx, key);
    }
    assert!(cx.read(|cx| fixture.state.read(cx).pending_factory.is_empty()));
    assert!(cx.read(|cx| fixture.state.read(cx).snapshot.launches.is_empty()));
    press(cx, "ctrl-shift-tab");
    until(cx, "cycle back to Factory", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).selected_thread == Some(factory)
    })
    .await;
    press(cx, "ctrl-tab");
    until(cx, "cycle back to terminal", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).has_active_terminal(cx)
    })
    .await;

    // A failed native surface still owns a real tab; palette close stays enabled.
    open_palette(cx, &fixture).await;
    assert!(cx.read(|cx| {
        commands::Command::CloseThread
            .disabled_reason_in_workspace(fixture.workspace.read(cx), cx)
            .is_none()
    }));
    press(cx, "escape");
    cx.run_until_parked();
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, _, cx| {
            workspace.flush_layout(cx);
        },
    );
    fixture.store.flush().await.unwrap();
    let snapshot = fixture.store.snapshot().await.unwrap();
    let saved = serde_json::to_string(&snapshot.settings).unwrap();
    assert!(
        !saved.contains("\"Terminal\""),
        "ephemeral terminal leaf leaked into saved layout"
    );
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.threads[0].id, factory);

    press(cx, commands::CLOSE_THREAD_KEY);
    until(
        cx,
        "close removes terminal but retains Factory",
        Duration::from_secs(5),
        |cx| {
            let workspace = fixture.workspace.read(cx);
            workspace.terminal_count(cx) == 0
                && fixture.state.read(cx).selected_thread == Some(factory)
        },
    )
    .await;
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable fixture; exercises gesture-linked Space motion and reduced motion"]
async fn space_swipes_track_reverse_cancel_and_settle(cx: &mut TestAppContext) {
    use gpui_kit::{PlatformInput, ScrollDelta, ScrollWheelEvent, TouchPhase, point};

    fn position(cx: &mut TestAppContext, fixture: &Fixture) -> f32 {
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.render_frame(cx);
            assert_eq!(
                window.find("Projects").bounds().origin.x,
                px(0.),
                "Space motion must not translate the sidebar frame or workspace shell"
            );
            window.find("project-rows").bounds().origin.x.as_f32()
        })
        .unwrap()
    }

    fn scroll(cx: &mut TestAppContext, fixture: &Fixture, x: f32, y: f32, phase: TouchPhase) {
        cx.update_window(fixture.window.into(), |_, window, cx| {
            let position = window.find("space-switcher").bounds().center();
            window.dispatch_event(
                PlatformInput::ScrollWheel(ScrollWheelEvent {
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

    async fn settle(cx: &mut TestAppContext, fixture: &Fixture, rest: f32) {
        smol::Timer::after(Duration::from_millis(200)).await;
        cx.run_until_parked();
        assert!((position(cx, fixture) - rest).abs() < 0.5);
    }

    let fixture = open_fixture(cx, None).await;
    let original = cx.read(|cx| fixture.state.read(cx).active_space);
    let next = fixture.state.update(cx, |state, cx| {
        let id = state.create_space("Swipe destination").unwrap();
        cx.notify();
        id
    });
    cx.run_until_parked();
    for (mode, width, height) in [("light", 1440., 900.), ("dark", 900., 600.)] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        cx.simulate_window_resize(fixture.window.into(), size(px(width), px(height)));
        let rest = position(cx, &fixture);
        // Vertical scroll does not move the Space; horizontal travel follows and reverses.
        scroll(cx, &fixture, -10., 80., TouchPhase::Started);
        assert_eq!(position(cx, &fixture), rest);
        scroll(cx, &fixture, -40., 0., TouchPhase::Started);
        let dragged = position(cx, &fixture);
        assert!(
            dragged < rest - 5.,
            "Space must follow the fingers before committing"
        );
        assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
        scroll(cx, &fixture, 20., 0., TouchPhase::Moved);
        let reversed = position(cx, &fixture);
        assert!(reversed > dragged && reversed < rest);
        scroll(cx, &fixture, 0., 0., TouchPhase::Ended);
        scroll(cx, &fixture, -100., 0., TouchPhase::Moved);
        assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
        settle(cx, &fixture, rest).await;

        // A platform without touch phases also releases a short abandoned drag.
        scroll(cx, &fixture, -30., 0., TouchPhase::Moved);
        assert!(position(cx, &fixture) < rest);
        smol::Timer::after(Duration::from_millis(170)).await;
        cx.run_until_parked();
        settle(cx, &fixture, rest).await;
        assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);

        scroll(cx, &fixture, -40., 0., TouchPhase::Started);
        scroll(cx, &fixture, -45., 0., TouchPhase::Moved);
        assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);
        assert!(
            position(cx, &fixture) > rest,
            "destination enters from the other side"
        );
        scroll(cx, &fixture, -200., 0., TouchPhase::Moved);
        assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);
        settle(cx, &fixture, rest).await;

        cx.dispatch_action(fixture.window.into(), commands::PreviousSpace);
        assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), original);
        assert!(
            position(cx, &fixture) < rest,
            "keyboard switching also animates"
        );
        settle(cx, &fixture, rest).await;
    }
    cx.update(|cx| cx.set_reduce_motion(true));
    let rest = position(cx, &fixture);
    scroll(cx, &fixture, -40., 0., TouchPhase::Started);
    assert_eq!(position(cx, &fixture), rest);
    scroll(cx, &fixture, -45., 0., TouchPhase::Moved);
    assert_eq!(cx.read(|cx| fixture.state.read(cx).active_space), next);
    assert_eq!(position(cx, &fixture), rest);
    cx.update(|cx| cx.set_reduce_motion(false));
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
