use super::*;

fn terminal_tabs(value: &serde_json::Value) -> Vec<(String, String)> {
    let mut tabs = Vec::new();
    match value {
        serde_json::Value::Object(map) => {
            if map.get("panel_name") == Some(&json!("Terminal")) {
                let panel = &map["info"]["panel"];
                tabs.push((panel["terminal"].to_string(), panel["project"].to_string()));
            }
            map.values()
                .for_each(|child| tabs.extend(terminal_tabs(child)));
        }
        serde_json::Value::Array(items) => items
            .iter()
            .for_each(|child| tabs.extend(terminal_tabs(child))),
        _ => {}
    }
    tabs
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE; tests real unsupported-native-host error, mixed tabs and persistence without a provider turn"]
async fn terminal_tabs_keep_failure_local_and_save_tabs(cx: &mut TestAppContext) {
    let fixture = open_fixture_with_editor(cx, None, Some("/nonexistent/drostje-qa-neovim")).await;
    let project = cx.read(|cx| fixture.state.read(cx).snapshot.projects[0].id);
    cx.dispatch_action(fixture.window.into(), commands::NewFactoryThread);
    let factory = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    cx.dispatch_action(fixture.window.into(), commands::NewTerminal);
    until(
        cx,
        "terminal host failure is visible",
        Duration::from_secs(10),
        |cx| {
            fixture
                .workspace
                .read(cx)
                .active_terminal(cx)
                .is_some_and(|terminal| terminal.read(cx).error().is_some())
        },
    )
    .await;
    let press = |cx: &mut TestAppContext, key: &str| {
        cx.update_window(fixture.window.into(), |_, window, cx| window.press(key, cx))
            .unwrap();
        cx.run_until_parked();
    };
    assert_eq!(cx.read(|cx| fixture.state.read(cx).selected_thread), None);
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).open_threads.clone()),
        [factory]
    );
    assert_eq!(
        cx.read(|cx| fixture.state.read(cx).snapshot.threads.len()),
        1
    );
    assert!(cx.read(|cx| {
        commands::Command::RestartTerminal
            .disabled_reason_in_workspace(fixture.workspace.read(cx), cx)
            .is_none()
    }));

    for key in ["j", "k", "i", "secondary-enter"] {
        press(cx, key);
    }
    assert!(cx.read(|cx| fixture.state.read(cx).pending_factory.is_empty()));
    assert!(cx.read(|cx| fixture.state.read(cx).snapshot.launches.is_empty()));
    press(cx, "ctrl-shift-tab");
    until(cx, "cycle back to Factory", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).selected_thread == Some(factory)
    })
    .await;
    press(cx, "ctrl-tab");
    until(cx, "cycle back to terminal", Duration::from_secs(5), |cx| {
        fixture.workspace.read(cx).has_active_terminal(cx)
    })
    .await;

    open_palette(cx, &fixture).await;
    assert!(cx.read(|cx| {
        commands::Command::CloseThread
            .disabled_reason_in_workspace(fixture.workspace.read(cx), cx)
            .is_none()
    }));
    press(cx, "escape");
    cx.run_until_parked();
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, _, cx| {
            workspace.flush_layout(cx);
        },
    );
    fixture.store.flush().await.unwrap();
    let snapshot = fixture.store.snapshot().await.unwrap();
    let saved = terminal_tabs(&json!(snapshot.settings));
    assert_eq!(saved.len(), 1, "terminal tab is saved with the layout");
    assert_eq!(saved[0].1, json!(project).to_string());
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.threads[0].id, factory);

    quit(cx, &fixture).await;
    println!("DROSTJE_TERMINAL_QA_DATA={}", fixture.data.display());
}

#[gpui_kit::test]
#[ignore = "requires DROSTJE_TERMINAL_QA_DATA from terminal_tabs_keep_failure_local_and_save_tabs; reopens the tab after process restart"]
async fn terminal_tabs_reopen_after_restart(cx: &mut TestAppContext) {
    let data = PathBuf::from(std::env::var_os("DROSTJE_TERMINAL_QA_DATA").unwrap());
    let root = PathBuf::from(std::env::var_os("DROSTJE_RUNTIME_FIXTURE").unwrap());
    assert!(
        fs::canonicalize(&data)
            .unwrap()
            .starts_with(fs::canonicalize(root).unwrap())
    );
    let fixture =
        open_fixture_with_editor(cx, Some(data), Some("/nonexistent/drostje-qa-neovim")).await;
    let factory = cx.read(|cx| fixture.state.read(cx).open_threads.clone());
    assert_eq!(factory.len(), 1, "the Factory thread tab is restored too");
    let saved = terminal_tabs(
        cx.read(|cx| fixture.state.read(cx).space_layout().cloned())
            .as_ref()
            .unwrap(),
    );
    assert_eq!(saved.len(), 1, "first phase saved one terminal tab");
    until(cx, "terminal tab reopened", Duration::from_secs(10), |cx| {
        fixture.workspace.read(cx).terminal_count(cx) == 1
    })
    .await;
    let press = |cx: &mut TestAppContext, key: &str| {
        cx.update_window(fixture.window.into(), |_, window, cx| window.press(key, cx))
            .unwrap();
        cx.run_until_parked();
    };
    let live = cx.read(|cx| json!(fixture.workspace.read(cx).dock.read(cx).dump(cx)));
    assert_eq!(terminal_tabs(&live), saved, "the same tab reopened");
    let factory = factory[0];
    press(cx, "secondary-2");
    until(
        cx,
        "focus reopened terminal",
        Duration::from_secs(5),
        |cx| fixture.workspace.read(cx).has_active_terminal(cx),
    )
    .await;

    press(cx, commands::CLOSE_THREAD_KEY);
    until(
        cx,
        "close removes terminal but retains Factory",
        Duration::from_secs(5),
        |cx| {
            let workspace = fixture.workspace.read(cx);
            workspace.terminal_count(cx) == 0
                && fixture.state.read(cx).selected_thread == Some(factory)
        },
    )
    .await;
    quit(cx, &fixture).await;
}
