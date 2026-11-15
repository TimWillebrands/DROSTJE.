//! Embedded Neovim prompt editor: startup failure, slash commands and file references.
use super::*;

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

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE and OMP; slash commands reach the real draft, never a provider turn"]
async fn slash_command_picker_writes_into_the_draft_and_refuses_blocked_commands(
    cx: &mut TestAppContext,
) {
    let fixture = open_fixture(cx, None).await;
    cx.update_window(fixture.window.into(), |_, window, _| {
        window.activate_window()
    })
    .unwrap();
    let id = new_conversation(cx, &fixture).await;
    until(
        cx,
        "OMP lists its commands",
        Duration::from_secs(30),
        |cx| {
            fixture
                .state
                .read(cx)
                .conversations
                .get(&id)
                .is_some_and(|session| {
                    session.state == crate::harness::ConversationState::Ready
                        && session
                            .slash_commands
                            .commands()
                            .iter()
                            .any(|command| command.name == "compact")
                })
        },
    )
    .await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.focus_composer(window, cx),
    );
    cx.simulate_keystrokes(fixture.window.into(), "k e e p space t e s t s");
    until(cx, "typed arguments", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).drafts[&DraftKey::Thread(id)].text == "keep tests"
    })
    .await;

    cx.dispatch_action(fixture.window.into(), commands::InsertSlashCommand);
    until(cx, "command picker opened", Duration::from_secs(5), |cx| {
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
    cx.simulate_keystrokes(fixture.window.into(), "c o m p a c t enter");
    until(
        cx,
        "command inserted before the arguments",
        Duration::from_secs(5),
        |cx| {
            fixture.workspace.read(cx).palette.is_none()
                && fixture.state.read(cx).drafts[&DraftKey::Thread(id)].text
                    == "/compact keep tests"
        },
    )
    .await;
    // The cursor sits after the command, still in insert mode, so typing continues there.
    cx.simulate_keystrokes(fixture.window.into(), "s o f t space");
    until(
        cx,
        "typing continues after the command",
        Duration::from_secs(5),
        |cx| {
            fixture.state.read(cx).drafts[&DraftKey::Thread(id)].text == "/compact soft keep tests"
        },
    )
    .await;
    assert!(
        cx.read(|cx| fixture
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .find(|t| t.id == id)
            .unwrap()
            .submission
            .is_none()),
        "inserting a command never sends it"
    );

    // A blocked command typed by hand is refused before it reaches OMP.
    cx.simulate_keystrokes(fixture.window.into(), "escape g g d G i / m o d e l");
    until(cx, "blocked command typed", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).drafts[&DraftKey::Thread(id)].text == "/model"
    })
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    until(
        cx,
        "blocked command refused",
        Duration::from_secs(5),
        |cx| {
            fixture
                .state
                .read(cx)
                .conversations
                .get(&id)
                .and_then(|session| session.error.as_deref())
                .is_some_and(|error| error.starts_with("/model is not available"))
        },
    )
    .await;
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).drafts[&DraftKey::Thread(id)]
            .text
            .clone()),
        "/model",
        "a refused command stays in the draft"
    );
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE and OMP; caret popup completes against the real draft"]
async fn slash_command_popup_completes_at_the_caret(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    cx.update_window(fixture.window.into(), |_, window, _| {
        window.activate_window()
    })
    .unwrap();
    let id = new_conversation(cx, &fixture).await;
    until(
        cx,
        "OMP lists its commands",
        Duration::from_secs(30),
        |cx| {
            fixture
                .state
                .read(cx)
                .conversations
                .get(&id)
                .is_some_and(|session| {
                    session
                        .slash_commands
                        .commands()
                        .iter()
                        .any(|command| command.name == "compact")
                })
        },
    )
    .await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.focus_composer(window, cx),
    );
    let draft = |cx: &App| {
        fixture.state.read(cx).drafts[&DraftKey::Thread(id)]
            .text
            .clone()
    };
    let popup_shown = |cx: &mut TestAppContext| {
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.refresh();
            window.render_frame(cx);
            window.try_find("slash-completion").is_some()
        })
        .unwrap()
    };
    let selection = |cx: &App| {
        fixture
            .workspace
            .read(cx)
            .editor
            .as_ref()
            .and_then(|editor| editor.read(cx).completion_selection(cx))
    };

    cx.simulate_keystrokes(fixture.window.into(), "/ c o m p a c");
    until(cx, "partial command typed", Duration::from_secs(5), |cx| {
        draft(cx) == "/compac" && selection(cx).as_deref() == Some("compact")
    })
    .await;
    assert!(popup_shown(cx), "a partial command opens the popup");

    cx.simulate_keystrokes(fixture.window.into(), "tab");
    until(cx, "command completed", Duration::from_secs(5), |cx| {
        draft(cx) == "/compact "
    })
    .await;
    assert!(
        !popup_shown(cx),
        "the popup closes once the cursor leaves the command"
    );
    let note = |cx: &App| {
        fixture
            .workspace
            .read(cx)
            .editor
            .as_ref()
            .and_then(|editor| editor.read(cx).command_note_text().map(str::to_owned))
    };
    until(cx, "argument syntax shown", Duration::from_secs(5), |cx| {
        note(cx).is_some_and(|note| note.starts_with("/compact ["))
    })
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "s o f t");
    until(
        cx,
        "arguments typed in insert mode",
        Duration::from_secs(5),
        |cx| draft(cx) == "/compact soft",
    )
    .await;
    until(
        cx,
        "argument syntax hidden once typed",
        Duration::from_secs(5),
        |cx| note(cx).is_none(),
    )
    .await;

    // Escape closes the popup without leaving insert mode; typing on reopens it.
    cx.simulate_keystrokes(fixture.window.into(), "escape g g d G i / c o");
    until(cx, "second partial command", Duration::from_secs(5), |cx| {
        draft(cx) == "/co"
    })
    .await;
    assert!(popup_shown(cx));
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    cx.run_until_parked();
    assert!(!popup_shown(cx), "Escape closes the popup first");
    cx.simulate_keystrokes(fixture.window.into(), "m");
    until(cx, "still in insert mode", Duration::from_secs(5), |cx| {
        draft(cx) == "/com"
    })
    .await;
    assert!(popup_shown(cx), "a changed query reopens the popup");

    // Down and Ctrl-P move the selection; Return inserts it.
    let first = cx.read(selection).unwrap();
    cx.simulate_keystrokes(fixture.window.into(), "down");
    cx.run_until_parked();
    let second = cx.read(selection).unwrap();
    assert_ne!(first, second);
    cx.simulate_keystrokes(fixture.window.into(), "ctrl-p");
    cx.run_until_parked();
    assert_eq!(cx.read(selection).unwrap(), first);
    cx.simulate_keystrokes(fixture.window.into(), "enter");
    let expected = format!("/{first} ");
    until(
        cx,
        "Return inserts the selection",
        Duration::from_secs(5),
        |cx| draft(cx) == expected,
    )
    .await;
    assert!(
        cx.read(|cx| fixture
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .find(|t| t.id == id)
            .unwrap()
            .submission
            .is_none()),
        "completion never sends"
    );
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; opens Conversation file references as Neovim tabs without a provider turn"]
async fn conversation_file_references_open_neovim_tabs(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let thread = new_conversation(cx, &fixture).await;
    let root = cx.read(|cx| fixture.state.read(cx).snapshot.projects[0].root.clone());
    let open = |cx: &mut TestAppContext, path: &str, line| {
        cx.dispatch_action(
            fixture.window.into(),
            commands::OpenFileReference {
                reference: crate::file_refs::FileReference {
                    path: path.into(),
                    line,
                    column: None,
                },
                thread: Some(thread),
            },
        );
    };

    // A relative path resolves against the Conversation's working tree, at its line.
    open(cx, "notes.txt", Some(1));
    until(cx, "Neovim tab opened", Duration::from_secs(10), |cx| {
        fixture.workspace.read(cx).neovim_tabs(cx).len() == 1
    })
    .await;
    let (title, target) = cx.read(|cx| fixture.workspace.read(cx).neovim_tabs(cx))[0].clone();
    assert_eq!(title, "Neovim — notes.txt");
    assert_eq!(
        target.file.as_deref(),
        Some(fs::canonicalize(root.join("notes.txt")).unwrap().as_path())
    );
    assert_eq!((target.line, target.column), (Some(1), None));
    assert!(cx.read(|cx| fixture.workspace.read(cx).has_active_terminal(cx)));

    // A missing file is a visible notice, never an empty tab.
    let notices = cx.read(|cx| fixture.state.read(cx).notice_sequence);
    open(cx, "missing/file.rs", Some(3));
    until(cx, "missing file notice", Duration::from_secs(10), |cx| {
        fixture.state.read(cx).notice_sequence > notices
    })
    .await;
    assert!(cx.read(|cx| {
        fixture.state.read(cx).notices.iter().any(|notice| {
            notice.text.starts_with("Could not open missing/file.rs:3:")
                && notice.text.ends_with("does not exist")
        })
    }));
    assert_eq!(
        cx.read(|cx| fixture.workspace.read(cx).neovim_tabs(cx).len()),
        1
    );

    // The tab is saved with the layout, file and line included.
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, _, cx| workspace.flush_layout(cx),
    );
    fixture.store.flush().await.unwrap();
    let snapshot = fixture.store.snapshot().await.unwrap();
    assert!(
        json!(snapshot.settings).to_string().contains("\"line\":1"),
        "Neovim target is saved with the tab"
    );
    quit(cx, &fixture).await;
}
