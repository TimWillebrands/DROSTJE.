use super::*;

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

    cx.simulate_keystrokes(fixture.window.into(), "d secondary-escape");
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
