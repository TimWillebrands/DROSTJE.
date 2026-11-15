//! Factory form input, layout recovery and persisted-configuration guard.
use super::*;

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
    cx.simulate_keystrokes(fixture.window.into(), "d secondary-1");
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
    cx.simulate_keystrokes(fixture.window.into(), "secondary-j secondary-1");
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
