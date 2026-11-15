use super::*;

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; verifies nonblocking tool details navigation"]
async fn tool_details_open_in_the_thread_inspector(cx: &mut TestAppContext) {
    let fixture = open_fixture(cx, None).await;
    let thread = new_conversation(cx, &fixture).await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| {
            workspace.open_details(thread, "tool-call".to_owned(), window, cx);
        },
    );
    cx.read(|cx| {
        let workspace = fixture.workspace.read(cx);
        assert!(workspace.dialog.is_none());
        assert!(workspace.detail_surface.is_none());
        let state = fixture.state.read(cx);
        assert!(state.thread_sidebar_open(thread));
        assert_eq!(
            state.thread_sidebar_tab(thread),
            crate::app::SidebarTab::Details
        );
    });
    cx.dispatch_action(fixture.window.into(), commands::ToggleConversationDiffs);
    assert!(!cx.read(|cx| fixture.state.read(cx).thread_sidebar_open(thread)));
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; verifies inline requests do not open a modal"]
async fn input_and_custom_answers_stay_inline_and_back_preserves_the_request(
    cx: &mut TestAppContext,
) {
    use crate::harness::{UserRequest, UserRequestKind};

    let fixture = open_fixture(cx, None).await;
    let thread = new_conversation(cx, &fixture).await;
    let input = UserRequest {
        id: "input-request".into(),
        kind: UserRequestKind::Input,
        title: "Enter a value".into(),
        options: Vec::new(),
        text: "draft value".into(),
        placeholder: Some("Value".into()),
        deadline: None,
    };
    fixture.state.update(cx, |state, cx| {
        state
            .conversations
            .get_mut(&thread)
            .unwrap()
            .requests
            .insert(input.id.clone(), input.clone());
        cx.notify();
    });
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.open_extension(thread, input.clone(), window, cx),
    );
    cx.read(|cx| {
        let workspace = fixture.workspace.read(cx);
        let form = workspace.extension.as_ref().unwrap();
        assert!(form.inline_answer);
        assert_eq!(form.input.as_ref().unwrap().read(cx).value(), "draft value");
        assert!(workspace.dialog.is_none());
        assert!(workspace.detail_surface.is_none());
    });
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.close_extension(false, window, cx),
    );

    let question = UserRequest {
        id: "question-request".into(),
        kind: UserRequestKind::Question,
        title: "Choose a direction".into(),
        options: vec![crate::harness::UserOption {
            value: "Existing option".into(),
            description: None,
        }],
        text: "Which direction?".into(),
        placeholder: None,
        deadline: None,
    };
    fixture.state.update(cx, |state, cx| {
        state
            .conversations
            .get_mut(&thread)
            .unwrap()
            .requests
            .insert(question.id.clone(), question.clone());
        cx.notify();
    });
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.open_custom_answer(thread, question.clone(), window, cx),
    );
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.back_from_custom_answer(thread, &question.id, window, cx),
    );
    cx.read(|cx| {
        let workspace = fixture.workspace.read(cx);
        assert!(workspace.extension.is_none());
        assert!(workspace.dialog.is_none());
        assert!(
            fixture.state.read(cx).conversations[&thread]
                .requests
                .contains_key(&question.id)
        );
    });
    quit(cx, &fixture).await;
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; retained drafts and inline answers never block the palette"]
async fn palette_opens_over_retained_drafts_and_inline_answers(cx: &mut TestAppContext) {
    use crate::harness::{UserRequest, UserRequestKind};

    let fixture = open_fixture(cx, None).await;
    let thread = new_conversation(cx, &fixture).await;
    let hosted = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            fixture
                .workspace
                .read(cx)
                .dialog
                .as_ref()
                .map(|dialog| dialog.read(cx).hosted())
        })
    };
    let close_palette = async |cx: &mut TestAppContext| {
        cx.simulate_keystrokes(fixture.window.into(), "escape");
        until(cx, "palette closed", Duration::from_secs(5), |cx| {
            fixture.workspace.read(cx).palette.is_none()
        })
        .await;
    };

    cx.dispatch_action(fixture.window.into(), commands::Settings);
    until(cx, "Settings opened", Duration::from_secs(5), |cx| {
        fixture
            .workspace
            .read(cx)
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).hosted())
    })
    .await;
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
    open_palette(cx, &fixture).await;
    close_palette(cx).await;

    cx.dispatch_action(fixture.window.into(), commands::Settings);
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
        |workspace, window, cx| workspace.dismiss(window, cx),
    );
    cx.run_until_parked();
    assert_eq!(hosted(cx), Some(false));
    open_palette(cx, &fixture).await;
    close_palette(cx).await;

    let input = UserRequest {
        id: "palette-input-request".into(),
        kind: UserRequestKind::Input,
        title: "Enter a value".into(),
        options: Vec::new(),
        text: "typed answer".into(),
        placeholder: None,
        deadline: None,
    };
    fixture.state.update(cx, |state, cx| {
        state
            .conversations
            .get_mut(&thread)
            .unwrap()
            .requests
            .insert(input.id.clone(), input.clone());
        cx.notify();
    });
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.open_extension(thread, input.clone(), window, cx),
    );
    assert!(cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .extension
            .as_ref()
            .is_some_and(|form| form.inline_answer)
    }));
    cx.dispatch_action(fixture.window.into(), commands::OpenPalette);
    until(
        cx,
        "palette over inline answer",
        Duration::from_secs(5),
        |cx| fixture.workspace.read(cx).palette.is_some(),
    )
    .await;
    close_palette(cx).await;
    cx.read(|cx| {
        let form = fixture.workspace.read(cx).extension.as_ref().unwrap();
        assert_eq!(form.request.id, input.id);
        assert_eq!(
            form.input.as_ref().unwrap().read(cx).value(),
            "typed answer"
        );
        assert!(
            fixture.state.read(cx).conversations[&thread]
                .requests
                .contains_key(&input.id),
            "opening the palette must not cancel the agent's request"
        );
    });
    quit(cx, &fixture).await;
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
    cx.simulate_keystrokes(fixture.window.into(), "secondary-n");
    until(
        cx,
        "new conversation on the last used harness",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).snapshot.threads.len() == before + 3,
    )
    .await;
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).thread().unwrap().harness.clone()),
        "claude-code"
    );
    let unstarted = cx.read(|cx| fixture.state.read(cx).thread().unwrap().id);
    cx.dispatch_action(fixture.window.into(), commands::SwitchHarness);
    until(cx, "harness picker", Duration::from_secs(5), |cx| {
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
    cx.simulate_keystrokes(fixture.window.into(), "g g j enter");
    until(cx, "harness switched", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).palette.is_none()
            && fixture
                .state
                .read(cx)
                .snapshot
                .threads
                .iter()
                .any(|thread| thread.id == unstarted && thread.harness == "pi")
    })
    .await;
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).snapshot.threads.len()),
        before + 3,
        "Switching replaces the harness in place"
    );
    quit(cx, &fixture).await;
}
