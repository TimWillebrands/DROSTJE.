use crate::{
    app::{AppEvent, AppState, Bootstrap, BranchListing, FetchStatus},
    model::{Project, ProjectDefaults, ThreadKind},
    platform,
    store::Store,
    ui::{
        commands::{self, Palette},
        workspace::{Workspace, WorkspaceRoot},
    },
};
use gpui_kit::{
    AnyWindowHandle, AppContext, Entity, TestAppContext,
    component::{Root, Theme, ThemeMode},
    test::TestWindowExt,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Fixture {
    dir: PathBuf,
    root: PathBuf,
    window: AnyWindowHandle,
    workspace: Entity<Workspace>,
    state: Entity<AppState>,
    project: Uuid,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn clone(dir: &Path, origin: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    git(
        dir,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    );
    git(&path, &["config", "user.name", "DROSTJE."]);
    git(&path, &["config", "user.email", "drostje@example.invalid"]);
    path
}

fn open(cx: &mut TestAppContext) -> Fixture {
    cx.dispatcher.allow_parking();
    let dir = std::env::temp_dir().join(format!("drostje-branch-ui-{}", Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let dir = fs::canonicalize(dir).unwrap();
    let origin = dir.join("origin.git");
    git(
        &dir,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    let root = clone(&dir, &origin, "root");
    git(&root, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git(&root, &["push", "-q", "-u", "origin", "main"]);
    git(&root, &["branch", "topic"]);
    let other = clone(&dir, &origin, "other");
    git(&other, &["switch", "-q", "-c", "theirs"]);
    git(&other, &["commit", "-q", "--allow-empty", "-m", "theirs"]);
    git(&other, &["push", "-q", "origin", "theirs"]);

    let store = Store::open(dir.join("data")).unwrap();
    let project = Project {
        id: Uuid::new_v4(),
        name: "Branches".into(),
        root: root.clone(),
        archived: false,
        defaults: ProjectDefaults::default(),
    };
    let project_id = project.id;
    smol::block_on(store.save_project(project)).unwrap();
    let snapshot = smol::block_on(store.snapshot()).unwrap();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::theme::initialize(None, Some(&dir), cx);
        commands::bind_keys(cx);
    });
    let repositories = BTreeMap::from([(
        project_id,
        platform::inspect_repository(&root).map_err(|error| format!("{error:#}")),
    )]);
    let state = cx.new(|cx| {
        AppState::new(
            Bootstrap {
                data_dir: dir.join("data"),
                store,
                snapshot,
                diagnostics: Default::default(),
                repositories,
                browser_tools: None,
            },
            cx,
        )
    });
    let missing = dir.join("no-harness-in-tests").display().to_string();
    state.update(cx, |state, _| {
        state.set_setting(
            "executables",
            json!({
                "omp": missing, "pi": missing, "claude": missing,
                "codex": missing, "opencode": missing,
            }),
        );
        state.select_project(project_id);
        state.new_thread(ThreadKind::Conversation).unwrap();
    });
    let mut workspace = None;
    let window = cx.add_window(|window, cx| {
        let view = cx.new(|cx| Workspace::new(state.clone(), window, cx));
        workspace = Some(view.clone());
        let content = cx.new(|_| WorkspaceRoot::new(view));
        Root::new(content, window, cx)
    });
    Fixture {
        dir,
        root,
        window: window.into(),
        workspace: workspace.unwrap(),
        state,
        project: project_id,
    }
}

fn wait(cx: &mut TestAppContext, fixture: &Fixture, what: &str, done: impl Fn(&AppState) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        cx.update_window(fixture.window, |_, window, cx| {
            window.simulate_next_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        if cx.read(|cx| done(fixture.state.read(cx))) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn visible(cx: &mut TestAppContext, fixture: &Fixture, id: &'static str) -> bool {
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some_and(|element| element.visible())
    })
    .unwrap()
}

fn picker_open(cx: &mut TestAppContext, fixture: &Fixture) -> bool {
    cx.read(|cx| fixture.workspace.read(cx).palette.is_some())
}

fn fetched(fixture: &Fixture) -> impl Fn(&AppState) -> bool {
    let project = fixture.project;
    move |state| {
        state
            .branch_list(project)
            .is_some_and(|list| list.fetch == Some(FetchStatus::Fetched))
            && state.conversation_thread().is_some_and(|thread| {
                state
                    .branch_choices(thread.id)
                    .and_then(Result::ok)
                    .is_some_and(|choices| choices.iter().any(|c| c.name == "theirs"))
            })
    }
}

fn choose(cx: &mut TestAppContext, fixture: &Fixture, query: &str) {
    cx.dispatch_action(fixture.window, commands::SwitchBranch);
    wait(cx, fixture, "the branch list", fetched(fixture));
    assert!(picker_open(cx, fixture));
    cx.simulate_input(fixture.window, query);
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window, "enter");
}

fn head(root: &Path) -> String {
    git(root, &["symbolic-ref", "--short", "HEAD"])
}

#[gpui_kit::test]
fn the_branch_picker_fetches_searches_switches_and_creates_from_the_keyboard(
    cx: &mut TestAppContext,
) {
    let fixture = open(cx);
    let thread = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    cx.dispatch_action(fixture.window, commands::FocusThread);
    wait(cx, &fixture, "the conversation", |_| true);

    cx.update_window(fixture.window, |_, window, cx| {
        let palette = Palette::open_branches(
            fixture.workspace.downgrade(),
            fixture.state.clone(),
            thread,
            window,
            cx,
        );
        fixture
            .workspace
            .update(cx, |workspace, _| workspace.palette = Some(palette));
    })
    .unwrap();
    assert!(
        visible(cx, &fixture, "branch-picker-empty"),
        "an unlisted repository reads as loading"
    );
    cx.simulate_keystrokes(fixture.window, "escape");
    cx.run_until_parked();
    assert!(!picker_open(cx, &fixture));

    cx.dispatch_action(fixture.window, commands::SwitchBranch);
    wait(cx, &fixture, "the fetched remote branch", fetched(&fixture));
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        cx.update_window(fixture.window, |_, window, cx| {
            Theme::change(mode, Some(window), cx);
        })
        .unwrap();
        for id in [
            "branch-picker",
            "branch-picker-status",
            "branch-picker-fetch",
        ] {
            assert!(visible(cx, &fixture, id), "{id} is visible in {mode:?}");
        }
    }
    cx.simulate_input(fixture.window, "theirs");
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window, "enter");
    wait(cx, &fixture, "the remote branch checked out", |state| {
        state.repositories[&fixture.project]
            .as_ref()
            .is_ok_and(|repository| repository.branch.as_deref() == Some("theirs"))
            && !state.worktree_request_pending()
    });
    assert_eq!(head(&fixture.root), "theirs");
    assert!(!picker_open(cx, &fixture));
    assert_eq!(
        git(
            &fixture.root,
            &["rev-parse", "--abbrev-ref", "theirs@{upstream}"]
        ),
        "origin/theirs",
        "a remote branch becomes a local tracking branch"
    );

    choose(cx, &fixture, "brand-new");
    wait(cx, &fixture, "the new branch", |state| {
        state.repositories[&fixture.project]
            .as_ref()
            .is_ok_and(|repository| repository.branch.as_deref() == Some("brand-new"))
            && !state.worktree_request_pending()
    });
    assert_eq!(head(&fixture.root), "brand-new");

    let dirty = fixture.root.join("uncommitted.txt");
    fs::write(&dirty, "keep me\n").unwrap();
    choose(cx, &fixture, "topic");
    wait(cx, &fixture, "the refusal", |state| {
        state
            .notices
            .iter()
            .any(|notice| notice.text.starts_with("Branch unchanged: "))
    });
    assert_eq!(head(&fixture.root), "brand-new");
    assert_eq!(fs::read_to_string(&dirty).unwrap(), "keep me\n");
    fs::remove_file(&dirty).unwrap();

    cx.dispatch_action(fixture.window, commands::SwitchBranch);
    wait(cx, &fixture, "the branch list", fetched(&fixture));
    let events = cx.read(|cx| fixture.state.read(cx).events.clone());
    smol::block_on(events.send(AppEvent::Branches(BranchListing::Listed {
        project: fixture.project,
        started: Instant::now(),
        result: Err("Git for-each-ref: unreadable".into()),
    })))
    .unwrap();
    wait(cx, &fixture, "the listing error", |state| {
        state
            .conversation_thread()
            .and_then(|thread| state.branch_choices(thread.id))
            .is_some_and(|choices| choices.is_err())
    });
    assert!(visible(cx, &fixture, "branch-picker-empty"));
    smol::block_on(events.send(AppEvent::Branches(BranchListing::Fetched {
        project: fixture.project,
        result: Err("Git fetch: could not read from remote repository".into()),
    })))
    .unwrap();
    wait(cx, &fixture, "the fetch failure", |state| {
        state
            .branch_list(fixture.project)
            .is_some_and(|list| matches!(list.fetch, Some(FetchStatus::Failed(_))))
    });
    assert!(visible(cx, &fixture, "branch-picker-fetch"));
    cx.simulate_keystrokes(fixture.window, "escape");
    cx.run_until_parked();
    assert!(!picker_open(cx, &fixture));
}

#[gpui_kit::test]
fn the_workspace_menu_starts_conversations_in_existing_worktrees(cx: &mut TestAppContext) {
    let fixture = open(cx);
    let first = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    fixture.state.update(cx, |state, _| {
        state
            .create_worktree_conversation(fixture.project, "main")
            .unwrap();
    });
    wait(cx, &fixture, "the worktree conversation", |state| {
        state
            .conversation_thread()
            .is_some_and(|thread| thread.worktree_id.is_some())
    });
    let (in_worktree, worktree) = cx.read(|cx| {
        let thread = fixture.state.read(cx).conversation_thread().unwrap();
        (thread.id, thread.worktree_id.unwrap())
    });

    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        window.click("composer-workspace-picker", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window, "down enter");
    wait(cx, &fixture, "a conversation in the checkout", |state| {
        state.conversation_thread().is_some_and(|thread| {
            ![first, in_worktree].contains(&thread.id) && thread.worktree_id.is_none()
        })
    });
    assert!(cx.read(|cx| fixture.workspace.read(cx).dialog.is_none()));
    let in_checkout = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());

    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        window.click("composer-workspace-picker", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window, "down enter");
    wait(
        cx,
        &fixture,
        "a conversation in the existing worktree",
        |state| {
            state.conversation_thread().is_some_and(|thread| {
                ![first, in_worktree, in_checkout].contains(&thread.id)
                    && thread.worktree_id == Some(worktree)
            })
        },
    );
}

#[gpui_kit::test]
fn the_workspace_menu_offers_removing_any_worktree(cx: &mut TestAppContext) {
    let fixture = open(cx);
    fixture.state.update(cx, |state, _| {
        state
            .create_worktree_conversation(fixture.project, "main")
            .unwrap();
    });
    wait(cx, &fixture, "the worktree conversation", |state| {
        state
            .conversation_thread()
            .is_some_and(|thread| thread.worktree_id.is_some())
    });
    let worktree = cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .conversation_thread()
            .unwrap()
            .worktree_id
            .unwrap()
    });
    cx.update_window(fixture.window, |_, window, cx| {
        window.render_frame(cx);
        window.click("composer-workspace-picker", cx);
    })
    .unwrap();
    wait(cx, &fixture, "the workspace menu", |_| true);
    for _ in 0..8 {
        if visible(cx, &fixture, "submenu") {
            break;
        }
        cx.simulate_keystrokes(fixture.window, "up");
        wait(cx, &fixture, "the next menu item", |_| true);
    }
    assert!(
        visible(cx, &fixture, "submenu"),
        "Remove worktree is reachable with up"
    );
    cx.simulate_keystrokes(fixture.window, "enter");
    wait(cx, &fixture, "the worktrees to remove", |_| true);
    cx.simulate_keystrokes(fixture.window, "enter");
    wait(cx, &fixture, "the removal dialog", |_| true);
    assert!(cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| {
                dialog.read(cx).kind == crate::ui::settings::DialogKind::RemoveWorktree(worktree)
            })
    }));
}
