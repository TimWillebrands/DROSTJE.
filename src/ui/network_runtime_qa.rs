//! Native smoke with real harness startup and host-tool HTTP; never asks a model for a turn.
use super::*;
use crate::harness::ConversationState;
use std::io::{Read, Write};

fn post_network(
    endpoint: String,
    thread: Uuid,
    token: Uuid,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let endpoint = url::Url::parse(&endpoint).unwrap();
    let mut stream =
        std::net::TcpStream::connect((endpoint.host_str().unwrap(), endpoint.port().unwrap()))
            .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let body =
        json!({"thread":thread,"network_token":token,"name":"network","arguments":arguments})
            .to_string();
    write!(stream, "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", endpoint.path(), body.len(), body).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
}

async fn network_call(
    cx: &mut TestAppContext,
    endpoint: &str,
    thread: Uuid,
    token: Uuid,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let endpoint = endpoint.to_owned();
    let (sender, receiver) = async_channel::bounded(1);
    smol::spawn(async move {
        let response =
            smol::unblock(move || post_network(endpoint, thread, token, arguments)).await;
        let _ = sender.send(response).await;
    })
    .detach();
    until(
        cx,
        "real network tool response",
        Duration::from_secs(20),
        |_| !receiver.is_empty(),
    )
    .await;
    receiver.recv().await.unwrap()
}

async fn omp_thread(cx: &mut TestAppContext, fixture: &Fixture) -> (Uuid, Uuid) {
    cx.dispatch_action(fixture.window.into(), commands::NewOmpConversation);
    let thread = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    until(cx, "real OMP startup", Duration::from_secs(20), |cx| {
        let state = fixture.state.read(cx);
        state
            .conversations
            .get(&thread)
            .is_some_and(|session| session.state == ConversationState::Ready && session.persisted)
            && fixture
                .workspace
                .read(cx)
                .editor
                .as_ref()
                .is_some_and(|editor| {
                    editor.read(cx).active_key() == Some(&DraftKey::Thread(thread))
                })
    })
    .await;
    let token = cx.read(|cx| fixture.state.read(cx).conversations[&thread].generation);
    (thread, token)
}

#[gpui_kit::test]
#[ignore = "requires disposable DROSTJE_RUNTIME_FIXTURE, OMP and Neovim; no model turns"]
async fn space_network_sidebar_and_real_mailbox(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    let root = PathBuf::from(std::env::var_os("DROSTJE_RUNTIME_FIXTURE").unwrap());
    let data = root.join(format!("network-qa-{}", Uuid::new_v4()));
    let store = Store::open(data.clone()).unwrap();
    store
        .save_project(crate::model::Project {
            id: Uuid::new_v4(),
            name: "Network QA".into(),
            root: std::fs::canonicalize(root.join("target-repo")).unwrap(),
            archived: false,
            defaults: Default::default(),
        })
        .await
        .unwrap();
    store.flush().await.unwrap();
    drop(store);
    let bridge = smol::unblock(crate::browser_tools::BrowserToolServer::start)
        .await
        .unwrap();
    let endpoint = bridge.endpoint();
    let fixture = open_fixture_with_tools(cx, Some(data), None, Some(bridge)).await;
    settle_frames(cx, &fixture);
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx)
    })
    .unwrap();
    let (first, first_token) = omp_thread(cx, &fixture).await;
    cx.dispatch_action(fixture.window.into(), commands::ShowNetworkMembers);
    settle_frames(cx, &fixture);
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        let scope = gpui_kit::SharedString::from(format!("space-network-sidebar-{first}"));
        assert!(window.find(scope.clone()).visible());
        window.within(scope).click("toggle-space-network", cx);
    })
    .unwrap();
    until(cx, "first opt-in committed", Duration::from_secs(5), |cx| {
        fixture.state.read(cx).network_joined(first)
    })
    .await;

    let (second, second_token) = omp_thread(cx, &fixture).await;
    let not_joined = network_call(
        cx,
        &endpoint,
        second,
        second_token,
        json!({"action":"members"}),
    )
    .await;
    assert!(
        not_joined["error"].is_string(),
        "tool exposure must not silently enroll a Thread"
    );
    open_palette(cx, &fixture).await;
    cx.simulate_keystrokes(fixture.window.into(), "j o i n space n e t w o r k enter");
    until(
        cx,
        "palette opt-in committed",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).network_joined(second),
    )
    .await;
    update_workspace(
        cx,
        fixture.window,
        &fixture.workspace,
        |workspace, window, cx| workspace.focus_composer(window, cx),
    );
    cx.simulate_keystrokes(fixture.window.into(), "s a f e");
    until(
        cx,
        "committed private Draft",
        Duration::from_secs(5),
        |cx| {
            fixture
                .state
                .read(cx)
                .drafts
                .get(&DraftKey::Thread(second))
                .is_some_and(|draft| draft.text == "safe")
        },
    )
    .await;
    cx.simulate_keystrokes(fixture.window.into(), "secondary-escape");

    let waiting_endpoint = endpoint.clone();
    let waiting = smol::spawn(async move {
        smol::unblock(move || {
            post_network(
                waiting_endpoint,
                second,
                second_token,
                json!({"action":"wait"}),
            )
        })
        .await
    });
    until(
        cx,
        "agent explicitly waiting",
        Duration::from_secs(5),
        |cx| {
            fixture
                .state
                .read(cx)
                .network
                .snapshot
                .waiting
                .contains(&second)
        },
    )
    .await;

    let direct = network_call(
        cx,
        &endpoint,
        first,
        first_token,
        json!({"action":"send","recipient":second,"body":"Is anyone else changing src/store.rs?"}),
    )
    .await;
    let direct_id: Uuid =
        serde_json::from_value(direct["result"]["message"]["id"].clone()).unwrap();
    let wake = waiting.await;
    assert_eq!(wake["result"]["ready"], true, "{wake}");
    assert_eq!(wake["result"]["pending"], 1, "{wake}");
    until(
        cx,
        "message woke the waiting agent",
        Duration::from_secs(5),
        |cx| {
            !fixture
                .state
                .read(cx)
                .network
                .snapshot
                .waiting
                .contains(&second)
        },
    )
    .await;
    let broadcast = network_call(cx, &endpoint, first, first_token, json!({"action":"broadcast","body":"I am changing persistence only; please message before editing the write worker."})).await;
    let broadcast_id: Uuid =
        serde_json::from_value(broadcast["result"]["message"]["id"].clone()).unwrap();
    until(
        cx,
        "queued messages visible",
        Duration::from_secs(5),
        |cx| {
            fixture
                .state
                .read(cx)
                .network
                .snapshot
                .messages
                .iter()
                .any(|message| message.id == broadcast_id)
        },
    )
    .await;
    assert!(cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .network
            .snapshot
            .messages
            .iter()
            .filter(|message| [direct_id, broadcast_id].contains(&message.id))
            .all(|message| {
                message
                    .deliveries
                    .iter()
                    .all(|delivery| !delivery.retrieved)
            })
    }));
    cx.dispatch_action(fixture.window.into(), commands::ShowNetworkMessages);
    cx.simulate_keystrokes(fixture.window.into(), "enter");
    settle_frames(cx, &fixture);
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        let scope = gpui_kit::SharedString::from(format!("space-network-sidebar-{second}"));
        assert_eq!(
            window
                .within(scope)
                .find("network-selection-detail")
                .focused(),
            Some(true)
        );
    })
    .unwrap();
    for mode in ["light", "dark"] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        for (width, height) in [(1440., 900.), (900., 600.)] {
            cx.simulate_window_resize(fixture.window.into(), size(px(width), px(height)));
            settle_frames(cx, &fixture);
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                let scope = gpui_kit::SharedString::from(format!("space-network-sidebar-{second}"));
                let panel = window.find(scope.clone()).bounds();
                let control = window
                    .within(scope.clone())
                    .find("toggle-space-network")
                    .bounds();
                assert!(panel.size.width > px(0.) && panel.size.height > px(0.));
                assert!(control.left() >= panel.left() && control.right() <= panel.right());
                assert!(
                    window
                        .within(scope)
                        .find("network-selection-detail")
                        .visible()
                );
            })
            .unwrap();
        }
    }
    cx.simulate_keystrokes(fixture.window.into(), "escape");
    settle_frames(cx, &fixture);
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        let scope = gpui_kit::SharedString::from(format!("space-network-sidebar-{second}"));
        assert_eq!(
            window
                .within(scope)
                .find("network-selection-detail")
                .focused(),
            Some(false)
        );
    })
    .unwrap();
    let inbox = network_call(
        cx,
        &endpoint,
        second,
        second_token,
        json!({"action":"inbox"}),
    )
    .await;
    let received = inbox["result"]["messages"].as_array().unwrap();
    assert_eq!(
        received
            .iter()
            .map(|message| message["id"].clone())
            .collect::<Vec<_>>(),
        vec![json!(direct_id), json!(broadcast_id)]
    );
    assert_eq!(received[0]["body"], "Is anyone else changing src/store.rs?");
    until(
        cx,
        "retrieval visible in sidebar",
        Duration::from_secs(5),
        |cx| {
            fixture
                .state
                .read(cx)
                .network
                .snapshot
                .messages
                .iter()
                .filter(|message| [direct_id, broadcast_id].contains(&message.id))
                .all(|message| message.deliveries.iter().all(|delivery| delivery.retrieved))
        },
    )
    .await;
    assert_eq!(
        cx.read(
            |cx| fixture.state.read(cx).drafts[&DraftKey::Thread(second)]
                .text
                .clone()
        ),
        "safe"
    );
    assert!(cx.read(|cx| {
        fixture
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .all(|thread| thread.submission.is_none())
    }));

    // List navigation opens the selected actual Thread rather than falling through to Projects.
    cx.dispatch_action(fixture.window.into(), commands::ShowNetworkMembers);
    let target = cx.read(|cx| fixture.state.read(cx).network.snapshot.members[0].thread);
    cx.simulate_keystrokes(fixture.window.into(), "g g enter");
    until(
        cx,
        "network member opens its Thread",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).selected_thread == Some(target),
    )
    .await;
    cx.dispatch_action(fixture.window.into(), commands::LeaveSpaceNetwork);
    until(cx, "leave committed", Duration::from_secs(5), |cx| {
        !fixture.state.read(cx).network_joined(target)
    })
    .await;
    let target_token = if target == first {
        first_token
    } else {
        second_token
    };
    let revoked = network_call(
        cx,
        &endpoint,
        target,
        target_token,
        json!({"action":"broadcast","body":"must not send after leave"}),
    )
    .await;
    assert!(revoked["error"].is_string());

    // The remaining participant can still send to itself; a real disk lock exercises failure UI.
    let remaining = if target == first { second } else { first };
    let remaining_token = if remaining == first {
        first_token
    } else {
        second_token
    };
    fixture.state.update(cx, |state, cx| {
        state.select_thread(remaining);
        cx.emit(crate::app::PresentationChange::Selection);
    });
    cx.dispatch_action(fixture.window.into(), commands::ShowNetworkMessages);
    until(
        cx,
        "pending writes drained before disk failure",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).pending_writes == 0,
    )
    .await;
    fixture.store.flush().await.unwrap();
    let blocker = rusqlite::Connection::open(fixture.data.join("drostje.sqlite3")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let failure = network_call(
        cx,
        &endpoint,
        remaining,
        remaining_token,
        json!({"action":"send","recipient":remaining,"body":"This write must fail visibly"}),
    )
    .await;
    assert!(failure["error"].is_string());
    until(
        cx,
        "network storage failure visible",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).network.error.is_some(),
    )
    .await;
    settle_frames(cx, &fixture);
    let membership_error = cx.read(|cx| fixture.state.read(cx).network.error.clone());
    cx.update_window(fixture.window.into(), |_, window, cx| {
        window.render_frame(cx);
        let scope = gpui_kit::SharedString::from(format!("space-network-sidebar-{remaining}"));
        window.within(scope).click("toggle-space-network", cx);
    })
    .unwrap();
    settle_frames(cx, &fixture);
    cx.read(|cx| {
        let state = fixture.state.read(cx);
        assert!(state.network_joined(remaining));
        assert!(!state.network.pending.contains(&remaining));
        assert_eq!(
            state.network.error, membership_error,
            "disabled activation must not dispatch a membership change"
        );
    });
    blocker.execute_batch("ROLLBACK").unwrap();
    println!(
        "Space messaging native smoke: opt-in, direct, broadcast, retrieval, keyboard, both themes, resize, Draft safety, leave and storage failure exercised; no model turns. data={}",
        fixture.data.display()
    );
    fixture
        .state
        .update(cx, |state, cx| state.begin_shutdown(true, cx));
    until(
        cx,
        "explicit forced quit after storage failure",
        Duration::from_secs(15),
        |cx| fixture.state.read(cx).shutdown_complete,
    )
    .await;
}
