use super::*;

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
    cx.simulate_keystrokes(fixture.window.into(), "down down down enter");
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
    let selector: &'static str = Box::leak(format!("thread-tab-{id}").into_boxed_str());
    let mut visual = VisualTestContext::from_window(fixture.window.into(), cx);
    assert!(
        visual.debug_bounds(selector).is_some(),
        "the tab bar draws the thread's own title element"
    );
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; opens a real harness session, no provider turn"]
async fn visible_thread_opens_its_conversation_without_selection(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let id = new_conversation(cx, &fixture).await;
    fixture.state.update(cx, |state, cx| {
        let project = state.selected_project.unwrap();
        state.select_terminal_project(project);
        if let Some(commands) = state.conversations.remove(&id).and_then(|s| s.commands) {
            let _ = commands.try_send(crate::harness::ConversationCommand::Close);
        }
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| fixture.state.read(cx).selected_thread), None);
    assert!(
        cx.read(|cx| fixture.state.read(cx).conversations.contains_key(&id)),
        "the visible tab opens its Conversation without a click"
    );
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; running turns mark their tab, row, composer and title bar"]
async fn running_turn_marks_its_thread(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let id = new_conversation(cx, &fixture).await;
    let leak = |name: String| -> &'static str { Box::leak(name.into_boxed_str()) };
    let (tab, tab_mark, tab_sweep, row_mark) = (
        leak(format!("thread-tab-{id}")),
        leak(format!("thread-tab-activity-{id}")),
        leak(format!("thread-tab-sweep-{id}")),
        leak(format!("thread-activity-rows-{id}")),
    );
    let set_running = |cx: &mut TestAppContext, running: bool| {
        fixture.state.update(cx, |state, cx| {
            match running {
                true => state.active_threads.insert(id),
                false => state.active_threads.remove(&id),
            };
            cx.emit(crate::app::PresentationChange::Thread(id));
            cx.emit(crate::app::PresentationChange::Projects);
            cx.notify();
        });
        cx.run_until_parked();
        cx.update_window(fixture.window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
    };

    set_running(cx, true);
    let mut visual = VisualTestContext::from_window(fixture.window.into(), cx);
    let title = visual.debug_bounds(tab).expect("thread tab");
    assert!(
        visual.debug_bounds(tab_mark).is_some(),
        "tab carries the mark"
    );
    let sweep = visual.debug_bounds(tab_sweep).expect("tab underline");
    assert_eq!((sweep.left(), sweep.right()), (title.left(), title.right()));
    assert!(
        sweep.top() >= title.bottom(),
        "the underline sits below the label"
    );
    eprintln!("tab title {title:?}, underline {sweep:?}");
    assert!(
        visual.debug_bounds(row_mark).is_some(),
        "Projects row carries the mark"
    );
    let composer = visual.debug_bounds("composer-row").expect("composer");
    let edge = visual
        .debug_bounds("composer-sweep")
        .expect("composer sweep");
    assert_eq!(edge.size.width, composer.size.width);
    assert!(
        edge.top() <= composer.top() + px(1.),
        "the sweep rides the top border"
    );
    assert!(
        visual.debug_bounds("running-turns").is_some(),
        "title bar counts it"
    );

    set_running(cx, false);
    let mut visual = VisualTestContext::from_window(fixture.window.into(), cx);
    for selector in [
        tab_mark,
        tab_sweep,
        row_mark,
        "composer-sweep",
        "running-turns",
    ] {
        assert!(visual.debug_bounds(selector).is_none(), "{selector} clears");
    }
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
    cx.simulate_keystrokes(fixture.window.into(), "k e e p secondary-escape");
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
        let workspace = fixture.workspace.read(cx);
        workspace.dialog.is_none()
            && workspace.inline_rename().is_some_and(|(target, _, _, _)| {
                target == crate::ui::workspace::RenameTarget::Thread(id)
            })
    }));
    let original_title = cx.read(|cx| fixture.state.read(cx).thread().unwrap().title.clone());
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace
                .inline_rename()
                .unwrap()
                .2
                .update(cx, |input, cx| {
                    input.set_value("Discarded rename".to_owned(), window, cx);
                });
        },
    );
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(fixture.workspace.read(cx).inline_rename().is_none());
        assert_eq!(
            fixture.state.read(cx).thread().unwrap().title,
            original_title
        );
    });
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.hover(row.clone(), cx);
        window.click(rename.clone(), cx);
    })
    .unwrap();
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            let input = workspace.inline_rename().unwrap().2;
            input.update(cx, |input, cx| {
                input.set_value("Manually renamed".to_owned(), window, cx)
            });
            input.focus_handle(cx).focus(window, cx);
        },
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
        let thread = state
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .unwrap();
        assert!(thread.archived, "one click archives without confirmation");
        assert_eq!(thread.title, "Manually renamed");
        assert_eq!(thread.draft, "keep");
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
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.focus_region(Region::Thread, window, cx),
    );
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
        assert!(
            state
                .snapshot
                .threads
                .iter()
                .any(|thread| thread.id == id && thread.archived)
        );
    });
    wait_for_saved_thread(cx, &fixture, id, |thread| thread.archived).await;

    let second = new_conversation(cx, &fixture).await;
    fixture.state.update(cx, |state, cx| {
        state.select_thread(id);
        cx.notify();
    });
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.focus_region(Region::Projects, window, cx);
        },
    );
    settle_frames(cx, &fixture);
    let tabs = cx.read(|cx| fixture.state.read(cx).open_threads.clone());
    let project = cx.read(|cx| fixture.state.read(cx).project().unwrap().id);
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.clear_notifications(cx);
    })
    .unwrap();
    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(
        fixture.window.into(),
        "a r c h i v e space p r o j e c t enter",
    );
    cx.run_until_parked();
    cx.read(|cx| {
        let state = fixture.state.read(cx);
        assert_eq!(state.project().unwrap().id, project);
        assert!(state.project().unwrap().archived);
        assert!(!state.open_threads.contains(&id));
        assert!(!state.open_threads.contains(&second));
        let workspace = fixture.workspace.read(cx);
        assert!(workspace.dialog.is_none());
    });
    settle_frames(cx, &fixture);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut previous = None;
    let mut stable_frames = 0;
    while stable_frames < 3 {
        cx.run_until_parked();
        let bounds = cx
            .update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                window.find("undo-project-archive").bounds()
            })
            .unwrap();
        stable_frames = if previous == Some(bounds) {
            stable_frames + 1
        } else {
            0
        };
        previous = Some(bounds);
        assert!(
            Instant::now() < deadline,
            "Undo notification did not settle"
        );
        smol::Timer::after(Duration::from_millis(20)).await;
    }
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("undo-project-archive", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let state = fixture.state.read(cx);
        assert!(!state.project().unwrap().archived);
        assert_eq!(state.open_threads, tabs, "Undo restores every project tab");
        assert_eq!(state.selected_thread, Some(id));
    });
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; creates and removes a real Git worktree, no provider turn"]
async fn worktree_conversation_is_created_inherited_and_removed(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let root = cx.read(|cx| fixture.state.read(cx).project().unwrap().root.clone());
    cx.dispatch_action(fixture.window.into(), commands::NewConversationInWorktree);
    cx.run_until_parked();
    assert!(cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| {
                matches!(
                    dialog.read(cx).kind,
                    crate::ui::settings::DialogKind::NewWorktreeConversation(_)
                )
            })
    }));
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    until(cx, "worktree conversation", Duration::from_secs(20), |cx| {
        fixture
            .state
            .read(cx)
            .conversation_thread()
            .is_some_and(|thread| thread.worktree_id.is_some())
    })
    .await;
    let (thread, worktree) = cx.read(|cx| {
        let state = fixture.state.read(cx);
        let thread = state.conversation_thread().unwrap().clone();
        let worktree = state.thread_worktree(&thread).unwrap().clone();
        (thread, worktree)
    });
    assert!(cx.read(|cx| fixture.workspace.read(cx).dialog.is_none()));
    assert!(
        worktree
            .path
            .starts_with(fs::canonicalize(&fixture.data).unwrap().join("worktrees"))
    );
    assert!(worktree.path.is_dir());
    assert!(worktree.branch.starts_with(crate::worktrees::BRANCH_PREFIX));
    assert_eq!(worktree.created_by, thread.id);
    assert_eq!(
        platform::worktree_head_branch(&worktree.path)
            .unwrap()
            .as_deref(),
        Some(worktree.branch.as_str())
    );

    cx.dispatch_action(fixture.window.into(), commands::NewOmpConversation);
    cx.run_until_parked();
    let sibling = cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .conversation_thread()
            .unwrap()
            .clone()
    });
    assert_ne!(sibling.id, thread.id);
    assert_eq!(sibling.worktree_id, Some(worktree.id));

    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("composer-workspace-picker", cx);
    })
    .unwrap();
    cx.simulate_keystrokes(fixture.window.into(), "down enter");
    cx.run_until_parked();
    assert!(cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| {
                matches!(
                    dialog.read(cx).kind,
                    crate::ui::settings::DialogKind::NewWorkspaceConversation(_)
                )
            })
    }));
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("workspace-choice").click("input", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window.into(), "up enter");
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    cx.run_until_parked();
    let checkout = cx.read(|cx| {
        let state = fixture.state.read(cx);
        let selected = state.conversation_thread().unwrap();
        assert_ne!(selected.id, sibling.id);
        assert_eq!(selected.worktree_id, None);
        assert_eq!(state.working_tree_of(selected).unwrap().1, root);
        assert_eq!(
            state
                .snapshot
                .threads
                .iter()
                .find(|t| t.id == sibling.id)
                .unwrap()
                .worktree_id,
            Some(worktree.id)
        );
        selected.id
    });
    cx.dispatch_action(fixture.window.into(), commands::NewConversationInWorkspace);
    cx.run_until_parked();
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("workspace-choice").click("input", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.simulate_keystrokes(fixture.window.into(), "down enter");
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    cx.run_until_parked();
    let shared = cx.read(|cx| {
        let state = fixture.state.read(cx);
        let selected = state.conversation_thread().unwrap();
        assert_ne!(selected.id, checkout);
        assert_eq!(selected.worktree_id, Some(worktree.id));
        selected.id
    });
    for (mode, expected) in [("light", ThemeMode::Light), ("dark", ThemeMode::Dark)] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        until(cx, "workspace strip theme", Duration::from_secs(5), |cx| {
            Theme::global(cx).mode == expected
        })
        .await;
        for (width, height) in [(1440., 900.), (900., 600.)] {
            cx.simulate_window_resize(fixture.window.into(), size(px(width), px(height)));
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                let workspace = window.find("composer-workspace-picker").bounds();
                let branch = window.find("composer-branch-picker").bounds();
                assert!(
                    workspace.right() <= branch.left(),
                    "workspace controls overlap"
                );
                assert!(window.find("composer-branch-picker").visible());
            })
            .unwrap();
        }
    }
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.click("composer-branch-picker", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).hosted())
    }));
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    cx.run_until_parked();
    assert!(cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| !dialog.read(cx).hosted())
    }));

    cx.dispatch_action(fixture.window.into(), commands::SwitchBranch);
    cx.run_until_parked();
    set_dialog(
        cx,
        fixture.window,
        &fixture.workspace,
        "new_branch",
        "feature-x",
    );
    let dirty = worktree.path.join("workspace-qa-uncommitted.txt");
    fs::write(
        &dirty,
        "Keep this change until the branch switch is refused\n",
    )
    .unwrap();
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    until(
        cx,
        "dirty branch switch refusal",
        Duration::from_secs(20),
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
    assert_eq!(
        platform::worktree_head_branch(&worktree.path)
            .unwrap()
            .as_deref(),
        Some(worktree.branch.as_str())
    );
    assert_eq!(
        fs::read_to_string(&dirty).unwrap(),
        "Keep this change until the branch switch is refused\n"
    );
    fs::remove_file(&dirty).unwrap();
    cx.simulate_keystrokes(fixture.window.into(), "secondary-enter");
    until(cx, "branch switch", Duration::from_secs(20), |cx| {
        let state = fixture.state.read(cx);
        state
            .worktree(worktree.id)
            .is_some_and(|w| w.branch == "feature-x" && w.named)
            && state.worktree_heads.get(&worktree.id) == Some(&Some("feature-x".into()))
    })
    .await;
    assert!(cx.read(|cx| fixture.workspace.read(cx).dialog.is_none()));
    assert_eq!(
        platform::worktree_head_branch(&worktree.path)
            .unwrap()
            .as_deref(),
        Some("feature-x")
    );

    for id in [sibling.id, thread.id, shared] {
        fixture.state.update(cx, |state, cx| {
            state.select_thread(id);
            cx.notify();
        });
        cx.dispatch_action(fixture.window.into(), commands::ArchiveThread);
        cx.run_until_parked();
    }
    until(
        cx,
        "automatic worktree removal",
        Duration::from_secs(20),
        |cx| {
            fixture
                .state
                .read(cx)
                .worktree(worktree.id)
                .is_some_and(|worktree| !worktree.is_live())
        },
    )
    .await;
    assert!(!worktree.path.exists());
    assert!(!platform::branch_exists(&root, "feature-x").unwrap());
    quit(cx, &fixture).await;
}

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
