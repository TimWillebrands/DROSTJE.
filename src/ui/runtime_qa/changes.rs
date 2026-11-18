use super::*;

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

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; opens a Neovim tab from the Changes list, no provider turn"]
async fn changes_list_is_keyboard_driven_and_opens_neovim(cx: &mut TestAppContext) {
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
            .is_some_and(|result| result.as_ref().is_ok_and(|diff| diff.changed_files == 2))
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
    assert!(visual.debug_bounds("conversation-diff-patch").is_some());
    assert!(visual.debug_bounds("toggle-changes-layout").is_some());
    assert!(visual.debug_bounds("open-change-in-neovim").is_some());

    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.focus_region(Region::Thread, window, cx),
    );
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window.into(), "secondary-alt-l");
    cx.run_until_parked();
    let cursor = |cx: &mut TestAppContext| {
        cx.update_window(fixture.window.into(), |_, window, cx| {
            fixture
                .workspace
                .read(cx)
                .changes_cursor(conversation, window, cx)
        })
        .unwrap()
    };
    assert_eq!(
        cursor(cx),
        (None, true),
        "Cmd-Alt-l reaches the Changes list"
    );
    cx.simulate_keystrokes(fixture.window.into(), "shift-g");
    cx.run_until_parked();
    let (last, _) = cursor(cx);
    cx.simulate_keystrokes(fixture.window.into(), "g g j j j");
    cx.run_until_parked();
    assert_eq!(cursor(cx), (Some(3), true));
    assert!(last.is_some_and(|last| last > 3));
    cx.simulate_keystrokes(fixture.window.into(), "s");
    until(cx, "side-by-side layout", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).changes_layout() == crate::app::ChangesLayout::SideBySide
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
    assert!(
        VisualTestContext::from_window(fixture.window.into(), cx)
            .debug_bounds("conversation-diff-patch")
            .is_some()
    );
    assert!(cursor(cx).1, "switching layout keeps focus in the list");
    cx.simulate_keystrokes(fixture.window.into(), "g g j j");
    cx.run_until_parked();
    assert_eq!(cursor(cx).0, Some(2));
    cx.simulate_keystrokes(fixture.window.into(), "enter");
    until(
        cx,
        "Neovim tab on the change",
        Duration::from_secs(10),
        |cx| fixture.workspace.read(cx).neovim_tabs(cx).len() == 1,
    )
    .await;
    let (_, target) = cx.read(|cx| fixture.workspace.read(cx).neovim_tabs(cx))[0].clone();
    assert_eq!(
        target.file.as_deref(),
        Some(fs::canonicalize(root.join("notes.txt")).unwrap().as_path())
    );
    assert_eq!(target.line, Some(1));
    cx.simulate_keystrokes(fixture.window.into(), "ctrl-shift-tab");
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
    }
    cx.simulate_keystrokes(fixture.window.into(), "secondary-alt-l");
    cx.run_until_parked();
    assert_eq!(
        cursor(cx),
        (Some(2), true),
        "the list keeps its cursor and takes focus again"
    );
    cx.simulate_keystrokes(fixture.window.into(), "s");
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).changes_layout()),
        crate::app::ChangesLayout::Inline
    );

    fs::write(root.join("notes.txt"), "before\n").unwrap();
    fs::remove_file(root.join("sidebar-new.txt")).unwrap();
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; commits in the fixture repository and rewinds it, no provider turn"]
async fn commit_panel_commits_the_working_tree(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let conversation = new_conversation(cx, &fixture).await;
    let root = cx.read(|cx| fixture.state.read(cx).project().unwrap().root.clone());
    let head = platform::git(&root, &["rev-parse", "HEAD"]).unwrap();
    fs::write(root.join("notes.txt"), "after\n").unwrap();
    fs::write(root.join("commit-new.txt"), "added\n").unwrap();

    cx.dispatch_action(fixture.window.into(), commands::RefreshConversationDiff);
    until(cx, "working tree diff", Duration::from_secs(5), |cx| {
        fixture
            .state
            .read(cx)
            .conversation_diff(conversation)
            .and_then(|diff| diff.result.as_ref())
            .is_some_and(|result| result.as_ref().is_ok_and(|diff| diff.changed_files == 2))
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
    assert!(visual.debug_bounds("conversation-diff-patch").is_some());
    assert!(visual.debug_bounds("commit-changes").is_some());
    cx.dispatch_action(fixture.window.into(), commands::Settings);
    until(cx, "Settings draft opened", Duration::from_secs(5), |cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).hosted())
    })
    .await;
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "nvim",
        "retained-editor",
    );
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    until(cx, "Settings draft hidden", Duration::from_secs(5), |cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| !dialog.read(cx).hosted())
    })
    .await;

    cx.dispatch_action(fixture.window.into(), commands::CommitChanges);
    cx.run_until_parked();
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            let dialog = workspace.dialog.as_ref().unwrap().clone();
            assert!(matches!(
                dialog.read(cx).kind,
                crate::ui::settings::DialogKind::Commit(id) if id == conversation
            ));
            assert!(!dialog.read(cx).hosted());
            assert_eq!(workspace.commit_panel_thread, Some(conversation));
            assert_eq!(
                workspace.state.read(cx).thread_sidebar_tab(conversation),
                crate::app::SidebarTab::Changes
            );
            let message = dialog.read(cx).commit_message().unwrap();
            message.update(cx, |message, cx| {
                message.set_value("Record fixture notes", window, cx)
            });
            message.focus_handle(cx).focus(window, cx);
        },
    );
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        })
        .unwrap();
    }
    assert!(
        VisualTestContext::from_window(fixture.window.into(), cx)
            .debug_bounds("commit-changes-panel")
            .is_some()
    );
    cx.dispatch_action(fixture.window.into(), commands::Dismiss);
    until(
        cx,
        "commit composer dismissed",
        Duration::from_secs(5),
        |cx| {
            let workspace = fixture.workspace.read(cx);
            workspace.commit_panel_thread.is_none()
                && workspace.dialog.as_ref().is_some_and(|dialog| {
                    let dialog = dialog.read(cx);
                    !dialog.hosted()
                        && dialog.commit_message().is_some_and(|message| {
                            message.read(cx).value() == "Record fixture notes"
                        })
                })
        },
    )
    .await;
    cx.dispatch_action(fixture.window.into(), commands::Settings);
    until(
        cx,
        "Settings draft survives the commit panel",
        Duration::from_secs(5),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .dialog
                .as_ref()
                .is_some_and(|dialog| {
                    let dialog = dialog.read(cx);
                    dialog.hosted() && dialog.value("nvim", cx) == "retained-editor"
                })
        },
    )
    .await;
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
    })
    .unwrap();
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
    cx.dispatch_action(fixture.window.into(), commands::CommitChanges);
    until(
        cx,
        "commit composer reopened",
        Duration::from_secs(5),
        |cx| {
            let workspace = fixture.workspace.read(cx);
            workspace.commit_panel_thread == Some(conversation)
                && workspace.dialog.as_ref().is_some_and(|dialog| {
                    let dialog = dialog.read(cx);
                    !dialog.hosted()
                        && dialog.commit_message().is_some_and(|message| {
                            message.read(cx).value() == "Record fixture notes"
                        })
                })
        },
    )
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    until(cx, "commit", Duration::from_secs(20), |cx| {
        fixture.workspace.read(cx).dialog.is_none()
            && fixture
                .state
                .read(cx)
                .conversation_diff(conversation)
                .and_then(|diff| diff.result.as_ref())
                .is_some_and(|result| result.as_ref().is_ok_and(|diff| diff.changed_files == 0))
    })
    .await;
    assert_eq!(
        platform::git(&root, &["log", "-1", "--format=%s"]).unwrap(),
        "Record fixture notes"
    );
    assert_eq!(
        platform::git(&root, &["rev-parse", "HEAD~1"]).unwrap(),
        head
    );

    platform::git(&root, &["reset", "--hard", &head]).unwrap();
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; real Git failure after closing Commit, no provider turn"]
async fn dismissed_commit_reports_git_failure_and_retains_the_form(cx: &mut TestAppContext) {
    struct RestoreFiles {
        notes: PathBuf,
        original: Vec<u8>,
        index_lock: PathBuf,
    }
    impl Drop for RestoreFiles {
        fn drop(&mut self) {
            let _ = fs::write(&self.notes, &self.original);
            let _ = fs::remove_file(&self.index_lock);
        }
    }

    let fixture = open_fixture(cx, None).await;
    let conversation = new_conversation(cx, &fixture).await;
    let root = cx.read(|cx| fixture.state.read(cx).project().unwrap().root.clone());
    let head = platform::git(&root, &["rev-parse", "HEAD"]).unwrap();
    let notes = root.join("notes.txt");
    let original = fs::read(&notes).unwrap();
    let index_lock = PathBuf::from(
        platform::git(
            &root,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "index.lock",
            ],
        )
        .unwrap(),
    );
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&index_lock)
        .unwrap();
    let restore = RestoreFiles {
        notes,
        original,
        index_lock,
    };
    fs::write(&restore.notes, "pending commit\n").unwrap();
    cx.dispatch_action(fixture.window.into(), commands::RefreshConversationDiff);
    until(cx, "commit failure diff", Duration::from_secs(5), |cx| {
        fixture
            .state
            .read(cx)
            .conversation_diff(conversation)
            .and_then(|diff| diff.result.as_ref())
            .is_some_and(|result| result.as_ref().is_ok_and(|diff| diff.changed_files == 1))
    })
    .await;

    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(
        fixture.window.into(),
        "c o m m i t space c h a n g e s enter",
    );
    cx.run_until_parked();
    let form = cx.read(|cx| fixture.workspace.read(cx).dialog.as_ref().unwrap().clone());
    update_workspace(cx, fixture.window, &fixture.workspace, |_, window, cx| {
        let message = form.read(cx).commit_message().unwrap();
        message.update(cx, |message, cx| {
            message.set_value("Keep this failed commit draft", window, cx)
        });
        message.focus_handle(cx).focus(window, cx);
    });
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.clear_notifications(cx);
    })
    .unwrap();
    settle_frames(cx, &fixture);

    let (request, progress) = cx
        .update_window(fixture.window.into(), |_, window, cx| {
            window.click("dialog-confirm", cx);
            let request = form.read(cx).request.expect("real Git request started");
            window.render_frame(cx);
            window.click("dialog-cancel", cx);
            assert!(fixture.workspace.read(cx).commit_panel_thread.is_none());
            assert_eq!(form.read(cx).request, Some(request));
            window.render_frame(cx);
            assert!(window.try_find("commit-changes-panel").is_none());
            assert!(
                window.find("notification").visible(),
                "background progress is visible"
            );
            let notifications = window.notifications(cx);
            assert_eq!(notifications.len(), 1, "closing shows one progress toast");
            (request, notifications[0].clone())
        })
        .unwrap();

    until(cx, "dismissed Git failure", Duration::from_secs(10), |cx| {
        fixture.state.read(cx).worktree_request.as_ref().is_some_and(|pending| {
            pending.id == request
                && matches!(&pending.status, crate::app::WorktreeRequestStatus::Failed(error) if error.contains("index.lock"))
        }) && form.read(cx).request.is_none()
            && form.read(cx).error.as_ref().is_some_and(|error| error.contains("index.lock"))
    })
    .await;
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("commit-changes-panel").is_none());
        let notifications = window.notifications(cx);
        assert_eq!(notifications.len(), 1, "the outcome replaces progress");
        assert_ne!(
            notifications[0], progress,
            "a settled outcome replaces the progress entity"
        );
        assert!(
            window.find("notification").visible(),
            "dismissed failure stays visible"
        );
    })
    .unwrap();

    cx.dispatch_action(fixture.window.into(), commands::CommitChanges);
    cx.run_until_parked();
    cx.read(|cx| {
        let workspace = fixture.workspace.read(cx);
        assert_eq!(workspace.commit_panel_thread, Some(conversation));
        assert_eq!(workspace.dialog.as_ref(), Some(&form));
        let form = form.read(cx);
        assert!(
            form.error
                .as_ref()
                .is_some_and(|error| error.contains("index.lock"))
        );
        assert_eq!(
            form.commit_message().unwrap().read(cx).value(),
            "Keep this failed commit draft"
        );
    });
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit-changes-panel").visible());
        assert!(
            window.find("commit-validation").visible(),
            "the retained Git error is painted"
        );
    })
    .unwrap();
    assert_eq!(platform::git(&root, &["rev-parse", "HEAD"]).unwrap(), head);
    drop(restore);
    quit(cx, &fixture).await;
}
