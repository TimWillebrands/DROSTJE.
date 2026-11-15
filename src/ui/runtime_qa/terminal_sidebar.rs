use super::*;

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; retained real TerminalPane lifecycle without provider turns"]
async fn terminal_sidebar_retains_shell_and_worktree_until_shutdown(cx: &mut TestAppContext) {
    use crate::app::SidebarTab;
    let fixture = open_fixture(cx, None).await;
    for conversation in [false, true] {
        fixture.state.update(cx, |state, cx| {
            if conversation {
                let project = state.project().unwrap().id;
                let base = state.repositories[&project].as_ref().unwrap().branches[0].clone();
                state.create_worktree_conversation(project, &base).unwrap();
            } else {
                state.new_thread(ThreadKind::Factory).unwrap();
            }
            cx.notify();
        });
        if conversation {
            until(
                cx,
                "real worktree Conversation",
                Duration::from_secs(20),
                |cx| {
                    fixture
                        .state
                        .read(cx)
                        .conversation_thread()
                        .is_some_and(|thread| thread.worktree_id.is_some())
                },
            )
            .await;
        }
        let (thread, root, worktree) = cx.read(|cx| {
            let state = fixture.state.read(cx);
            let thread = state.thread().unwrap();
            (
                thread.id,
                state.working_tree_of(thread).unwrap().1,
                thread.worktree_id,
            )
        });
        cx.run_until_parked();
        for _ in 0..3 {
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.simulate_next_frame(cx);
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
        }
        update_workspace(
            cx,
            fixture.window,
            &fixture.workspace,
            |workspace, window, cx| {
                workspace.focus_region(Region::Thread, window, cx);
            },
        );
        assert!(cx.read(|cx| {
            commands::Command::ToggleTerminal
                .disabled_reason_in_workspace(fixture.workspace.read(cx), cx)
                .is_none()
        }));
        cx.dispatch_action(fixture.window.into(), commands::ToggleTerminal);
        until(cx, "thread shell created", Duration::from_secs(10), |cx| {
            fixture
                .workspace
                .read(cx)
                .sidebar_terminal(thread)
                .is_some()
        })
        .await;
        let pane = cx.read(|cx| fixture.workspace.read(cx).sidebar_terminal(thread).unwrap());
        assert!(cx.read(|cx| pane.read(cx).root == root && pane.read(cx).worktree == worktree));
        until(
            cx,
            "terminal start settled",
            Duration::from_secs(10),
            |cx| pane.read(cx).native().is_some() || pane.read(cx).error().is_some(),
        )
        .await;
        let native = cx.read(|cx| pane.read(cx).native());
        for (left, right) in [
            ("secondary-alt-h", "secondary-alt-l"),
            ("secondary-alt-left", "secondary-alt-right"),
        ] {
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                pane.update(cx, |pane, cx| pane.focus(window, cx));
                assert!(pane.read(cx).focus_handle(cx).contains_focused(window, cx));
                window.press(left, cx);
                assert!(!pane.read(cx).focus_handle(cx).contains_focused(window, cx));
                assert_ne!(window.find("Projects").focused(), Some(true));
                window.press(left, cx);
                assert_eq!(window.find("Projects").focused(), Some(true));
                window.press(right, cx);
                assert!(!pane.read(cx).focus_handle(cx).contains_focused(window, cx));
                window.press(right, cx);
                assert!(pane.read(cx).focus_handle(cx).contains_focused(window, cx));
            })
            .unwrap();
        }
        cx.dispatch_action(fixture.window.into(), commands::FocusOutput);
        cx.run_until_parked();
        cx.dispatch_action(fixture.window.into(), commands::ToggleTerminal);
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| fixture.workspace.read(cx).sidebar_terminal(thread)),
            Some(pane.clone())
        );
        assert_eq!(cx.read(|cx| pane.read(cx).native()), native);
        assert!(cx.read(|cx| {
            let state = fixture.state.read(cx);
            state.thread_sidebar_open(thread)
                && state.thread_sidebar_tab(thread) == SidebarTab::Terminal
                && state.snapshot.launches.is_empty()
        }));
        cx.dispatch_action(fixture.window.into(), commands::ToggleTerminal);
        cx.run_until_parked();
        assert!(!cx.read(|cx| fixture.state.read(cx).thread_sidebar_open(thread)));
        assert_eq!(cx.read(|cx| pane.read(cx).native()), native);
    }
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, _, cx| workspace.close_all_terminals(cx),
    );
    assert!(cx.read(|cx| fixture.workspace.read(cx).thread_terminals.is_empty()));
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.focus_region(Region::Projects, window, cx);
        },
    );
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.simulate_next_frame(cx);
        window.render_frame(cx);
    })
    .unwrap();
    quit(cx, &fixture).await;
}
