//! Shared fixture: opens a real Workspace window on the disposable DROSTJE_RUNTIME_FIXTURE.
use super::*;

pub(super) async fn until(
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

pub(super) fn update_workspace<R>(
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

pub(super) fn set_dialog(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    key: &str,
    value: &str,
) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        })
        .unwrap();
    }
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

pub(super) fn set_factory_field(
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

pub(super) struct Fixture {
    pub(super) window: WindowHandle<Root>,
    pub(super) workspace: Entity<Workspace>,
    pub(super) state: Entity<AppState>,
    pub(super) store: Store,
    pub(super) data: PathBuf,
}

pub(super) async fn open_fixture(cx: &mut TestAppContext, data: Option<PathBuf>) -> Fixture {
    open_fixture_with_editor(cx, data, None).await
}

pub(super) async fn open_fixture_with_editor(
    cx: &mut TestAppContext,
    data: Option<PathBuf>,
    executable: Option<&str>,
) -> Fixture {
    open_fixture_with_tools(cx, data, executable, None).await
}

pub(super) async fn open_fixture_with_tools(
    cx: &mut TestAppContext,
    data: Option<PathBuf>,
    executable: Option<&str>,
    browser_tools: Option<crate::browser_tools::BrowserToolServer>,
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
        browser_tools,
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
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|options| {
            assert!(options.directories && !options.files && !options.multiple);
            Some(vec![repository.root.clone()])
        });
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

pub(super) async fn new_conversation(cx: &mut TestAppContext, fixture: &Fixture) -> ThreadId {
    // New conversation from an untouched Conversation opens its harness picker
    // instead of stacking another blank one (`Workspace::execute`). Scenarios that
    // need several Conversations take the command's create branch directly.
    let untouched = cx.read(|cx| {
        let state = fixture.state.read(cx);
        state.conversation_thread().is_some_and(|thread| {
            state.is_untouched(thread) && state.switch_harness_reason().is_none()
        })
    });
    if untouched {
        update_workspace(
            cx,
            fixture.window,
            &fixture.workspace,
            |workspace, window, cx| {
                workspace.state.update(cx, |state, cx| {
                    state.new_thread(ThreadKind::Conversation).unwrap();
                    cx.notify();
                });
                workspace.focus_region(Region::Thread, window, cx);
            },
        );
    } else {
        cx.dispatch_action(fixture.window.into(), commands::NewConversation);
    }
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

pub(super) async fn quit(cx: &mut TestAppContext, fixture: &Fixture) {
    cx.dispatch_action(fixture.window.into(), commands::Quit);
    until(cx, "orderly shutdown", Duration::from_secs(15), |cx| {
        fixture.state.read(cx).shutdown_complete
    })
    .await;
    fixture.store.flush().await.unwrap();
}

pub(super) async fn wait_for_saved_thread(
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

pub(super) async fn open_palette(cx: &mut TestAppContext, fixture: &Fixture) {
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

/// Draws a few frames and lets dialog open/close animations, and the focus restoration
/// that waits on them, finish.
pub(super) fn settle_frames(cx: &mut TestAppContext, fixture: &Fixture) {
    for _ in 0..3 {
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
        })
        .unwrap();
    }
    cx.run_until_parked();
}
