//! Explicit runtime QA: GPUI actions with actual isolated Neovim and native OMP.
//! Requires DROST_RUNTIME_FIXTURE; provider turns additionally require DROST_PROVIDER_SMOKE=1.
use crate::{
    app::{AppState, Bootstrap},
    harness::{
        self, ContentBlock, ConversationCommand, ConversationEvent, ConversationState, TurnOutcome,
    },
    store::Store,
    ui::commands,
};
use crate::{
    model::ThreadId,
    neovim::DraftKey,
    platform,
    ui::workspace::{Region, Workspace},
};
use gpui_kit as gpui;
use gpui_kit::component::Root;
use gpui_kit::{
    App, AppContext, ClipboardItem, Context, Entity, TestAppContext, Window, WindowHandle,
};
use serde_json::json;
use std::path::PathBuf;
use std::{
    collections::BTreeMap,
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
    window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| update(workspace, window, cx))
        })
        .unwrap()
}

fn set_dialog(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    key: &str,
    value: String,
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
        field
            .input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    });
}

fn set_factory_field(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    key: &str,
    value: String,
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
        field
            .input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    });
    cx.run_until_parked();
}

async fn replace_and_send(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    state: &Entity<AppState>,
    id: ThreadId,
    text: &str,
) {
    let editor = cx.read(|cx| state.read(cx).editor.clone().unwrap());
    let draft = editor.snapshot(DraftKey::Thread(id)).await.unwrap();
    assert!(
        editor
            .replace_if_revision(draft.key.clone(), draft.revision(), text.into())
            .await
            .unwrap()
    );
    update_workspace(cx, window, workspace, |workspace, window, cx| {
        workspace.focus_composer(window, cx)
    });
    cx.dispatch_action(window.into(), commands::Submit);
}

async fn settled(cx: &mut TestAppContext, state: &Entity<AppState>, id: ThreadId) {
    until(
        cx,
        "provider turn settlement",
        Duration::from_secs(180),
        |cx| {
            let state = state.read(cx);
            state.conversations.get(&id).is_some_and(|session| {
                session.outcome.is_some()
                    && !session.preparing
                    && !state.active_threads.contains(&id)
            })
        },
    )
    .await;
    cx.read(|cx| {
        let session = &state.read(cx).conversations[&id];
        assert!(
            session.error.is_none(),
            "Real OMP failure: {:?}",
            session.error
        );
    });
}

#[gpui::test]
#[ignore = "requires real Neovim/OMP and a disposable DROST_RUNTIME_FIXTURE; optional authenticated provider turns"]
async fn workspace_actions_with_real_backends(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    let fixture = PathBuf::from(
        std::env::var_os("DROST_RUNTIME_FIXTURE")
            .expect("Set DROST_RUNTIME_FIXTURE to the disposable fixture"),
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
    let data = fixture.join(format!("drost-qa-{}", Uuid::new_v4()));
    let store = Store::open(data.clone()).unwrap();
    store.set_setting("executables".into(), json!({"nvim":"/opt/homebrew/bin/nvim", "omp":"/opt/homebrew/bin/omp", "factory":"/Users/tim/.local/bin/factory"})).await.unwrap();
    store
        .set_setting(
            "factory_data_root".into(),
            json!(fixture.join("factory-data")),
        )
        .await
        .unwrap();
    let snapshot = store.snapshot().await.unwrap();
    let bootstrap = Bootstrap {
        data_dir: data.clone(),
        store: store.clone(),
        snapshot,
        diagnostics: BTreeMap::new(),
        repositories: BTreeMap::new(),
    };
    cx.update(|cx| {
        gpui_kit::init(cx);
        commands::bind_keys(cx);
    });
    let mut workspace = None;
    let window = cx.add_window(|window, cx| {
        let state = cx.new(|cx| AppState::new(bootstrap, cx));
        let view = cx.new(|cx| Workspace::new(state, window, cx));
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    let workspace = workspace.unwrap();
    let state = cx.read(|cx| workspace.read(cx).state.clone());
    assert!(cx.read(|cx| state.read(cx).snapshot.projects.is_empty()));
    cx.dispatch_action(window.into(), commands::AddProject);
    set_dialog(
        cx,
        window,
        &workspace,
        "path",
        repository.root.to_string_lossy().into_owned(),
    );
    cx.dispatch_action(window.into(), commands::Confirm);
    until(cx, "project registration", Duration::from_secs(10), |cx| {
        state.read(cx).snapshot.projects.len() == 1
    })
    .await;
    let project_id = cx.read(|cx| state.read(cx).selected_project.unwrap());
    let alias = fixture.join(format!("repo-alias-{}", Uuid::new_v4()));
    std::os::unix::fs::symlink(&repository.root, &alias).unwrap();
    cx.dispatch_action(window.into(), commands::AddProject);
    set_dialog(
        cx,
        window,
        &workspace,
        "path",
        alias.to_string_lossy().into_owned(),
    );
    cx.dispatch_action(window.into(), commands::Confirm);
    until(
        cx,
        "canonical project selection",
        Duration::from_secs(10),
        |cx| !state.read(cx).registering,
    )
    .await;
    assert_eq!(cx.read(|cx| state.read(cx).snapshot.projects.len()), 1);
    assert_eq!(
        cx.read(|cx| state.read(cx).selected_project),
        Some(project_id)
    );
    println!("Native action flow: empty workspace, Git registration, symlink deduplication");

    cx.dispatch_action(window.into(), commands::NewConversation);
    let first = cx.read(|cx| state.read(cx).selected_thread.unwrap());
    cx.dispatch_action(window.into(), commands::RenameConversation);
    set_dialog(cx, window, &workspace, "name", "Fixture Alpha".into());
    cx.dispatch_action(window.into(), commands::Confirm);
    until(cx, "first editor buffer", Duration::from_secs(15), |cx| {
        workspace
            .read(cx)
            .editor
            .as_ref()
            .is_some_and(|view| view.read(cx).active_key() == Some(&DraftKey::Thread(first)))
    })
    .await;
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.focus_composer(window, cx)
    });
    cx.simulate_keystrokes(
        window.into(),
        "a l p h a space b e t a escape 0 w c i w shift-g shift-a shift-m shift-m shift-a escape",
    );
    let editor = cx.read(|cx| state.read(cx).editor.clone().unwrap());
    assert_eq!(
        editor.snapshot(DraftKey::Thread(first)).await.unwrap().text,
        "alpha GAMMA"
    );
    cx.dispatch_action(window.into(), commands::NewConversation);
    let second = cx.read(|cx| state.read(cx).selected_thread.unwrap());
    assert_ne!(first, second);
    cx.dispatch_action(window.into(), commands::RenameConversation);
    set_dialog(cx, window, &workspace, "name", "Fixture Beta".into());
    cx.dispatch_action(window.into(), commands::Confirm);
    until(cx, "second editor buffer", Duration::from_secs(15), |cx| {
        workspace
            .read(cx)
            .editor
            .as_ref()
            .is_some_and(|view| view.read(cx).active_key() == Some(&DraftKey::Thread(second)))
    })
    .await;
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.focus_composer(window, cx)
    });
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(
            "first\n日本語 e\u{301} 😀".into(),
        ))
    });
    cx.simulate_keystrokes(window.into(), "cmd-v escape u ctrl-r");
    assert_eq!(
        editor
            .snapshot(DraftKey::Thread(second))
            .await
            .unwrap()
            .text,
        "first\n日本語 e\u{301} 😀"
    );
    for (key, expected) in [
        ("cmd-1", Region::Projects),
        ("cmd-2", Region::Conversation),
        ("cmd-3", Region::Factory),
    ] {
        cx.simulate_keystrokes(window.into(), key);
        assert_eq!(
            update_workspace(cx, window, &workspace, |workspace, window, cx| workspace
                .focused_region(window, cx)),
            expected
        );
    }
    cx.dispatch_action(window.into(), commands::OpenPalette);
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace
            .palette
            .as_ref()
            .unwrap()
            .input
            .update(cx, |input, cx| input.set_value("Fixture Alpha", window, cx));
    });
    cx.simulate_keystrokes(window.into(), "enter");
    until(
        cx,
        "palette thread activation",
        Duration::from_secs(10),
        |cx| {
            state.read(cx).selected_thread == Some(first)
                && workspace.read(cx).editor.as_ref().is_some_and(|view| {
                    view.read(cx).active_key() == Some(&DraftKey::Thread(first))
                })
        },
    )
    .await;
    assert_eq!(
        editor.snapshot(DraftKey::Thread(first)).await.unwrap().text,
        "alpha GAMMA"
    );
    println!(
        "Native GPUI key routing: real ciw, separate buffers, Unicode clipboard, undo/redo, Cmd-1/2/3, filtered palette"
    );

    let image_path = fixture.join(format!("qa-image-{}.png", Uuid::new_v4()));
    image::RgbaImage::from_pixel(32, 32, image::Rgba([230, 20, 30, 255]))
        .save(&image_path)
        .unwrap();
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.import_images(first, vec![image_path.clone()], window, cx)
    });
    until(
        cx,
        "decoded image attachment",
        Duration::from_secs(10),
        |cx| {
            state
                .read(cx)
                .thread()
                .is_some_and(|thread| !thread.attachments.is_empty())
        },
    )
    .await;
    let image = cx.read(|cx| state.read(cx).thread().unwrap().attachments[0].clone());
    assert_eq!(
        (image.width, image.height, image.mime.as_str()),
        (32, 32, "image/png")
    );
    assert_ne!(image.path, image_path);
    update_workspace(cx, window, &workspace, |workspace, _, cx| {
        workspace.image_preview = Some(image);
        cx.notify();
    });
    cx.refresh().unwrap();
    cx.run_until_parked();
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.dismiss(window, cx)
    });

    let paid = std::env::var("DROST_PROVIDER_SMOKE").as_deref() == Ok("1");
    let mut handoff = (0, "Append a line containing exactly drost-factory-smoke to notes.txt, then commit the change.".to_owned());
    if paid {
        until(
            cx,
            "authenticated OMP readiness",
            Duration::from_secs(75),
            |cx| {
                state
                    .read(cx)
                    .conversations
                    .get(&first)
                    .is_some_and(|session| {
                        session.persisted
                            && !session.models.is_empty()
                            && session.state == ConversationState::Ready
                    })
            },
        )
        .await;
        let model = cx.read(|cx| {
            let session = &state.read(cx).conversations[&first];
            session
                .models
                .iter()
                .find(|model| {
                    session.model.as_ref() == Some(&model.key)
                        && model.input.iter().any(|input| input == "image")
                })
                .or_else(|| {
                    session
                        .models
                        .iter()
                        .find(|model| model.input.iter().any(|input| input == "image"))
                })
                .expect("No real discovered image-capable model")
                .key
                .clone()
        });
        state.update(cx, |state, _| state.select_model(first, model.clone()));
        until(cx, "model selection", Duration::from_secs(35), |cx| {
            state.read(cx).conversations[&first].model.as_ref() == Some(&model)
        })
        .await;
        replace_and_send(cx, window, &workspace, &state, first, "Use the read tool to read notes.txt. Do not edit files or execute commands. Then describe the attached image's color and report the file's exact content. Add a 120-word explanation of why a disposable repository is useful for testing.").await;
        let deadline = Instant::now() + Duration::from_secs(180);
        let mut lengths = std::collections::BTreeSet::new();
        loop {
            cx.run_until_parked();
            let done = cx.read(|cx| {
                let state = state.read(cx);
                let session = &state.conversations[&first];
                let length = session
                    .messages
                    .iter()
                    .filter(|message| message.role == "assistant")
                    .flat_map(|message| &message.content)
                    .filter_map(|block| {
                        if let ContentBlock::Text(text) = block {
                            Some(text.len())
                        } else {
                            None
                        }
                    })
                    .sum::<usize>();
                if length > 0 {
                    lengths.insert(length);
                }
                session.outcome.is_some()
                    && !session.preparing
                    && !state.active_threads.contains(&first)
            });
            if done {
                break;
            }
            assert!(Instant::now() < deadline, "Provider response timeout");
            smol::Timer::after(Duration::from_millis(20)).await;
        }
        settled(cx, &state, first).await;
        cx.read(|cx| {
            let session = &state.read(cx).conversations[&first];
            assert_eq!(session.outcome, Some(TurnOutcome::Completed));
            assert!(
                session
                    .tools
                    .values()
                    .any(|tool| tool.complete && !tool.error),
                "No real completed tool activity observed"
            );
            assert!(
                session.messages.iter().any(|message| message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Image(_)))),
                "Native persisted image missing from history"
            );
            handoff = session
                .messages
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, message)| {
                    (message.role == "assistant").then(|| {
                        (
                            index,
                            message
                                .content
                                .iter()
                                .filter_map(|block| {
                                    if let ContentBlock::Text(text) = block {
                                        Some(text.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n\n"),
                        )
                    })
                })
                .unwrap();
        });
        assert!(lengths.len() > 1, "No incremental text snapshots observed");
        println!(
            "Authenticated native OMP: accepted PNG, completed real tool activity, observed {} incremental text lengths",
            lengths.len()
        );
        replace_and_send(cx, window, &workspace, &state, first, "Write a numbered list of 1000 detailed reasons to use disposable repositories. Do not use tools.").await;
        until(
            cx,
            "interruptible provider turn",
            Duration::from_secs(60),
            |cx| state.read(cx).conversations[&first].state == ConversationState::Running,
        )
        .await;
        cx.dispatch_action(window.into(), commands::Interrupt);
        settled(cx, &state, first).await;
        assert_eq!(
            cx.read(|cx| state.read(cx).conversations[&first].outcome.clone()),
            Some(TurnOutcome::Interrupted)
        );
        replace_and_send(
            cx,
            window,
            &workspace,
            &state,
            first,
            "Reply with exactly DROST-CONTINUED. Do not use tools.",
        )
        .await;
        settled(cx, &state, first).await;
        assert!(cx.read(|cx| state.read(cx).conversations[&first].messages.iter().any(|message| message.role == "assistant" && message.content.iter().any(|block| matches!(block, ContentBlock::Text(text) if text.contains("DROST-CONTINUED"))))));
        println!("Authenticated native OMP: interrupt confirmed, same session continued");
    }

    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.handoff_message = Some((first, handoff.0, handoff.1.clone()));
        workspace.execute(commands::Command::SendToFactory, window, cx);
    });
    until(
        cx,
        "selected-message Factory draft",
        Duration::from_secs(15),
        |cx| {
            workspace
                .read(cx)
                .factory_form
                .as_ref()
                .is_some_and(|form| !form.busy && form.pending_handoff.is_none())
                && state
                    .read(cx)
                    .drafts
                    .get(&DraftKey::Factory(project_id))
                    .is_some_and(|draft| draft.text == handoff.1)
        },
    )
    .await;
    set_factory_field(
        cx,
        window,
        &workspace,
        "title",
        "Reviewed fixture task".into(),
    );
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.review_factory_task(window, cx)
    });
    until(
        cx,
        "explicit Factory review",
        Duration::from_secs(10),
        |cx| {
            workspace
                .read(cx)
                .factory_form
                .as_ref()
                .is_some_and(|form| form.review.is_some())
        },
    )
    .await;
    cx.read(|cx| {
        let review = workspace
            .read(cx)
            .factory_form
            .as_ref()
            .unwrap()
            .review
            .as_ref()
            .unwrap();
        assert_eq!(review.spec.packet.as_ref().unwrap().task, handoff.1);
        assert_eq!(
            review.spec.origin_message.as_ref().unwrap().index,
            handoff.0
        );
    });
    assert!(
        store.snapshot().await.unwrap().launches.is_empty(),
        "Review unexpectedly launched work"
    );
    state.update(cx, |state, _| state.set_setting("executables", json!({"nvim":"/opt/homebrew/bin/nvim","omp":"/opt/homebrew/bin/omp","factory":fixture.join("not-a-factory-executable")})));
    update_workspace(cx, window, &workspace, |workspace, _, cx| {
        workspace.factory_form_changed(cx)
    });
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.review_factory_task(window, cx)
    });
    until(
        cx,
        "invalid executable review",
        Duration::from_secs(10),
        |cx| {
            workspace
                .read(cx)
                .factory_form
                .as_ref()
                .is_some_and(|form| form.review.is_some())
        },
    )
    .await;
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.dispatch_factory_review(window, cx)
    });
    until(
        cx,
        "visible preflight refusal",
        Duration::from_secs(15),
        |cx| {
            workspace
                .read(cx)
                .factory_form
                .as_ref()
                .is_some_and(|form| !form.busy && form.error.is_some())
        },
    )
    .await;
    assert_eq!(
        editor
            .snapshot(DraftKey::Factory(project_id))
            .await
            .unwrap()
            .text,
        handoff.1
    );
    assert!(cx.read(|cx| state.read(cx).owned_launches.is_empty()));
    println!(
        "Native Factory form: exact selected message, explicit review, real invalid-executable refusal; draft retained"
    );
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.close_factory_form(window, cx)
    });
    let empty_folder = fixture.join(format!("missing-prd-{}", Uuid::new_v4()));
    std::fs::create_dir(&empty_folder).unwrap();
    cx.dispatch_action(window.into(), commands::OpenFactoryFolder);
    set_factory_field(
        cx,
        window,
        &workspace,
        "folder",
        empty_folder.to_string_lossy().into_owned(),
    );
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.review_factory_task(window, cx)
    });
    until(cx, "missing PRD refusal", Duration::from_secs(10), |cx| {
        workspace
            .read(cx)
            .factory_form
            .as_ref()
            .is_some_and(|form| {
                !form.busy
                    && form
                        .error
                        .as_ref()
                        .is_some_and(|error| error.contains("PRD.md"))
            })
    })
    .await;
    assert!(cx.read(|cx| state.read(cx).owned_launches.is_empty()));
    update_workspace(cx, window, &workspace, |workspace, window, cx| {
        workspace.close_factory_form(window, cx)
    });

    let references = cx.read(|cx| state.read(cx).snapshot.threads.clone());
    let providers = cx.read(|cx| {
        state
            .read(cx)
            .conversations
            .values()
            .filter_map(|session| session.commands.clone())
            .collect::<Vec<_>>()
    });
    for commands in providers {
        let _ = commands.send(ConversationCommand::Close).await;
    }
    until(
        cx,
        "owned OMP child cleanup",
        Duration::from_secs(10),
        |cx| {
            state.read(cx).conversations.values().all(|session| {
                session
                    .commands
                    .as_ref()
                    .is_none_or(|commands| commands.is_closed())
            })
        },
    )
    .await;
    if paid {
        let thread = references.iter().find(|thread| thread.id == first).unwrap();
        let handle = harness::open(harness::ConversationConfig {
            harness: "omp".into(),
            executable: "/opt/homebrew/bin/omp".into(),
            cwd: repository.root.clone(),
            session_dir: data.join("omp-sessions").join(first.to_string()),
            session_ref: thread.session_ref.clone(),
            model: thread.model.clone(),
            allow_empty_session: false,
        })
        .await
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(75);
        let mut resumed = false;
        while !resumed {
            let event = smol::future::race(async { handle.events.recv().await.unwrap() }, async {
                smol::Timer::after(deadline.saturating_duration_since(Instant::now())).await;
                panic!("Resume timed out");
            })
            .await;
            match event {
                ConversationEvent::History { messages, .. } => resumed = messages.iter().any(|message| message.content.iter().any(|block| matches!(block, ContentBlock::Text(text) if text.contains("DROST-CONTINUED")))),
                ConversationEvent::SubmissionAccepted { .. } => panic!("Resume replayed a submission"),
                ConversationEvent::Failed { message, .. } => panic!("Resume failed: {message}"),
                _ => {},
            }
        }
        handle
            .commands
            .send(ConversationCommand::Close)
            .await
            .unwrap();
        while handle.events.recv().await.is_ok() {}
        println!("Real OMP reopen: exact durable session history restored without prompt replay");
    }
    for draft in editor.persistence_barrier().await.unwrap() {
        state.update(cx, |state, _| state.record_draft(draft));
    }
    until(cx, "durable UI writes", Duration::from_secs(10), |cx| {
        state.read(cx).pending_writes == 0
    })
    .await;
    store.flush().await.unwrap();
    let saved = store.snapshot().await.unwrap();
    assert_eq!(
        saved
            .threads
            .iter()
            .find(|thread| thread.id == second)
            .unwrap()
            .draft,
        "first\n日本語 e\u{301} 😀"
    );
    assert!(saved.settings.get("layout").is_some());
    assert_eq!(saved.settings.get("selected_thread"), Some(&json!(first)));
    editor.shutdown().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(repository.root.join("notes.txt")).unwrap(),
        "before\n"
    );
    println!("QA data retained for native inspection: {}", data.display());
    println!(
        "This is GPUI test-support action/protocol proof, not visual or OS-input verification."
    );
}

#[gpui::test]
#[ignore = "requires real Neovim; exercises orderly quit against a real SQLite write lock"]
async fn orderly_quit_flushes_before_platform_deadline(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    let data = std::env::temp_dir().join(format!("drost-quit-{}", Uuid::new_v4()));
    let store = Store::open(data.clone()).unwrap();
    store
        .set_setting(
            "executables".into(),
            json!({"nvim":"/opt/homebrew/bin/nvim"}),
        )
        .await
        .unwrap();
    let bootstrap = Bootstrap {
        data_dir: data.clone(),
        store: store.clone(),
        snapshot: store.snapshot().await.unwrap(),
        diagnostics: BTreeMap::new(),
        repositories: BTreeMap::new(),
    };
    cx.update(|cx| {
        gpui_kit::init(cx);
        commands::bind_keys(cx);
    });
    let mut workspace = None;
    let window = cx.add_window(|window, cx| {
        let state = cx.new(|cx| AppState::new(bootstrap, cx));
        let view = cx.new(|cx| Workspace::new(state, window, cx));
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    let workspace = workspace.unwrap();
    let state = cx.read(|cx| workspace.read(cx).state.clone());
    until(cx, "real editor startup", Duration::from_secs(10), |cx| {
        workspace.read(cx).editor.is_some()
    })
    .await;
    let connection = rusqlite::Connection::open(data.join("drost.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    state.update(cx, |state, _| {
        state.set_setting("quit_marker", json!("durable-before-platform-quit"))
    });
    cx.dispatch_action(window.into(), commands::Quit);
    until(
        cx,
        "orderly cleanup starts before platform quit",
        Duration::from_secs(1),
        |cx| state.read(cx).shutting_down,
    )
    .await;
    smol::Timer::after(Duration::from_millis(350)).await;
    cx.run_until_parked();
    assert!(
        !cx.read(|cx| state.read(cx).shutdown_complete),
        "Quit bypassed the blocked durable write"
    );
    connection.execute_batch("COMMIT").unwrap();
    until(
        cx,
        "orderly cleanup completes",
        Duration::from_secs(10),
        |cx| state.read(cx).shutdown_complete,
    )
    .await;
    assert_eq!(
        store.snapshot().await.unwrap().settings.get("quit_marker"),
        Some(&json!("durable-before-platform-quit"))
    );
    assert!(cx.read(|cx| state.read(cx).editor.as_ref().unwrap().commands.is_closed()));
    println!(
        "Orderly quit waited beyond GPUI's 200ms deadline, flushed SQLite, and shut down real Neovim before platform quit"
    );
}

#[gpui::test]
#[ignore = "requires persisted disposable DROST_QA_DATA; DROST_FACTORY_SMOKE=1 explicitly authorizes a real Factory run"]
async fn layout_and_factory_runtime_recovery(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    let fixture = PathBuf::from(std::env::var_os("DROST_RUNTIME_FIXTURE").unwrap());
    let data = PathBuf::from(std::env::var_os("DROST_QA_DATA").unwrap());
    assert!(
        std::fs::canonicalize(&data)
            .unwrap()
            .starts_with(std::fs::canonicalize(&fixture).unwrap())
    );
    let store = Store::open(data.clone()).unwrap();
    store.set_setting("executables".into(), json!({"nvim":"/opt/homebrew/bin/nvim","omp":"/opt/homebrew/bin/omp","factory":"/Users/tim/.local/bin/factory"})).await.unwrap();
    let snapshot = store.snapshot().await.unwrap();
    let selected = snapshot.settings["selected_thread"].clone();
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
    let bootstrap = Bootstrap {
        data_dir: data.clone(),
        store: store.clone(),
        snapshot,
        diagnostics: BTreeMap::new(),
        repositories,
    };
    cx.update(|cx| {
        gpui_kit::init(cx);
        commands::bind_keys(cx);
    });
    let mut workspace = None;
    let window = cx.add_window(|window, cx| {
        let state = cx.new(|cx| AppState::new(bootstrap, cx));
        let view = cx.new(|cx| Workspace::new(state, window, cx));
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    let workspace = workspace.unwrap();
    let state = cx.read(|cx| workspace.read(cx).state.clone());
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let (left, right) = cx.read(|cx| {
        let dock = workspace.read(cx).dock.read(cx);
        (
            dock.dock_size(gpui_kit::component::dock::DockPlacement::Left)
                .unwrap(),
            dock.dock_size(gpui_kit::component::dock::DockPlacement::Right),
        )
    });
    let left = f32::from(left);
    visual.simulate_mouse_down(
        gpui::point(gpui::px(left - 1.5), gpui::px(300.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    visual.simulate_mouse_move(
        gpui::point(gpui::px(left + 6.), gpui::px(300.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    visual.simulate_mouse_move(
        gpui::point(gpui::px(left + 52.), gpui::px(300.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    visual.simulate_mouse_up(
        gpui::point(gpui::px(left + 52.), gpui::px(300.)),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    let resized = cx.read(|cx| {
        let dock = workspace.read(cx).dock.read(cx);
        assert_eq!(
            dock.dock_size(gpui_kit::component::dock::DockPlacement::Right),
            right
        );
        dock.dock_size(gpui_kit::component::dock::DockPlacement::Left)
            .unwrap()
    });
    assert_eq!(resized, gpui::px(left + 52.));
    let mut launched = None;
    if std::env::var("DROST_FACTORY_SMOKE").as_deref() == Ok("1") {
        cx.dispatch_action(window.into(), commands::SendToFactory);
        let project_id = cx.read(|cx| state.read(cx).selected_project.unwrap());
        until(
            cx,
            "Factory editor activation",
            Duration::from_secs(15),
            |cx| {
                workspace.read(cx).editor.as_ref().is_some_and(|view| {
                    view.read(cx).active_key() == Some(&DraftKey::Factory(project_id))
                })
            },
        )
        .await;
        let editor = cx.read(|cx| state.read(cx).editor.clone().unwrap());
        let draft = editor
            .snapshot(DraftKey::Factory(project_id))
            .await
            .unwrap();
        let task = "Append a line containing exactly drost-factory-smoke to notes.txt, then commit the change.";
        assert!(
            editor
                .replace_if_revision(draft.key.clone(), draft.revision(), task.into())
                .await
                .unwrap()
        );
        set_factory_field(
            cx,
            window,
            &workspace,
            "title",
            "Factory smoke fixture".into(),
        );
        set_factory_field(cx, window, &workspace, "target_branch", "main".into());
        update_workspace(cx, window, &workspace, |workspace, window, cx| {
            workspace
                .factory_form
                .as_ref()
                .unwrap()
                .harness
                .update(cx, |harness, cx| {
                    harness.set_selected_value(&"opencode".into(), window, cx)
                });
        });
        cx.run_until_parked();
        update_workspace(cx, window, &workspace, |workspace, window, cx| {
            workspace.review_factory_task(window, cx)
        });
        until(cx, "Factory smoke review", Duration::from_secs(10), |cx| {
            workspace
                .read(cx)
                .factory_form
                .as_ref()
                .is_some_and(|form| form.review.is_some())
        })
        .await;
        let launch_id = cx.read(|cx| {
            workspace
                .read(cx)
                .factory_form
                .as_ref()
                .unwrap()
                .review
                .as_ref()
                .unwrap()
                .spec
                .id
        });
        update_workspace(cx, window, &workspace, |workspace, window, cx| {
            workspace.dispatch_factory_review(window, cx)
        });
        until(
            cx,
            "Factory doctor and dispatch result",
            Duration::from_secs(120),
            |cx| {
                workspace
                    .read(cx)
                    .factory_form
                    .as_ref()
                    .is_none_or(|form| !form.busy && form.error.is_some())
            },
        )
        .await;
        let error = cx.read(|cx| {
            workspace
                .read(cx)
                .factory_form
                .as_ref()
                .and_then(|form| form.error.clone())
        });
        if let Some(error) = error {
            assert_eq!(
                editor
                    .snapshot(DraftKey::Factory(project_id))
                    .await
                    .unwrap()
                    .text,
                task
            );
            assert!(cx.read(|cx| state.read(cx).owned_launches.is_empty()));
            println!("FACTORY_PREREQUISITE_BLOCKER: {error}");
            update_workspace(cx, window, &workspace, |workspace, window, cx| {
                workspace.close_factory_form(window, cx)
            });
        } else {
            until(
                cx,
                "tracked Factory launch",
                Duration::from_secs(10),
                |cx| state.read(cx).factory_runs.contains_key(&launch_id),
            )
            .await;
            let launch = store
                .snapshot()
                .await
                .unwrap()
                .launches
                .into_iter()
                .find(|launch| launch.id == launch_id)
                .unwrap();
            assert_eq!(launch.project_id, project_id);
            assert_eq!(
                launch.thread_id,
                cx.read(|cx| state.read(cx).selected_thread)
            );
            launched = Some(launch_id);
            println!("Real Factory accepted launch {launch_id}; quitting the GUI while it runs");
        }
    }
    cx.dispatch_action(window.into(), commands::Quit);
    until(cx, "orderly GUI shutdown", Duration::from_secs(15), |cx| {
        state.read(cx).shutdown_complete
    })
    .await;
    assert!(cx.read(|cx| state.read(cx).factory.is_none()));
    window
        .update(cx, |_, window, _| window.remove_window())
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
    let bootstrap = Bootstrap {
        data_dir: data.clone(),
        store: store.clone(),
        snapshot,
        diagnostics: BTreeMap::new(),
        repositories,
    };
    let mut reopened = None;
    let new_window = cx.add_window(|window, cx| {
        let state = cx.new(|cx| AppState::new(bootstrap, cx));
        let view = cx.new(|cx| Workspace::new(state, window, cx));
        reopened = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reopened = reopened.unwrap();
    let recovered = cx.read(|cx| reopened.read(cx).state.clone());
    assert_eq!(
        cx.read(|cx| reopened
            .read(cx)
            .dock
            .read(cx)
            .dock_size(gpui_kit::component::dock::DockPlacement::Left)),
        Some(resized)
    );
    assert_eq!(
        cx.read(|cx| json!(recovered.read(cx).selected_thread)),
        selected
    );
    println!(
        "Native dock mouse-drag and orderly quit/reconstruction retained layout and selection"
    );
    if let Some(id) = launched {
        until(
            cx,
            "Factory replay after GUI reopening",
            Duration::from_secs(15),
            |cx| recovered.read(cx).factory_runs.contains_key(&id),
        )
        .await;
        cx.read(|cx| {
            let snapshot = &recovered.read(cx).factory_runs[&id];
            if !snapshot.reported_state.terminal() && snapshot.launch.process_exit.is_none() {
                assert_eq!(snapshot.liveness, crate::factory::Liveness::Unverified);
            }
        });
        until(
            cx,
            "real Factory terminal event",
            Duration::from_secs(3000),
            |cx| {
                recovered.read(cx).factory_runs[&id]
                    .reported_state
                    .terminal()
            },
        )
        .await;
        cx.read(|cx| {
            let snapshot = &recovered.read(cx).factory_runs[&id];
            println!(
                "REAL_FACTORY_TERMINAL: {:?}; work item {:?}; attempt {:?}; event path {:?}",
                snapshot.reported_state,
                snapshot.launch.work_item_id,
                snapshot.launch.attempt_id,
                snapshot.launch.event_path
            );
            assert_eq!(
                snapshot.reported_state,
                crate::factory::ReportedState::Completed,
                "Real Factory run did not complete: {:?} {:?}",
                snapshot.diagnostics,
                snapshot.notes
            );
        });
    }
    cx.dispatch_action(new_window.into(), commands::Quit);
    until(
        cx,
        "reopened GUI orderly shutdown",
        Duration::from_secs(15),
        |cx| recovered.read(cx).shutdown_complete,
    )
    .await;
    println!("Recovered QA data: {}", data.display());
}
