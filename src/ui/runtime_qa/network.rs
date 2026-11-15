//! Opt-in real Store/editor/native-runtime QA. The runner must OS-deny IP egress.
//! Native acceptance is not a successful provider response or proof of reading.
use super::*;
use crate::{
    harness::ConversationState,
    network::{Action, DeliveryState},
};

fn require_blocked_ip() {
    assert_eq!(
        std::env::var("DROSTJE_NETWORK_QA_ISOLATED").as_deref(),
        Ok("1"),
        "Run only with the isolated native QA runner and scratch HOME/config"
    );
    for address in ["127.0.0.1:9", "[::1]:9"] {
        let error =
            std::net::TcpStream::connect_timeout(&address.parse().unwrap(), Duration::from_secs(1))
                .expect_err("IP networking must be OS-blocked before starting a native harness");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied,
            "No fail-closed IP sandbox at {address}: {error}"
        );
    }
}

async fn network_call(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    thread: Uuid,
    action: Action,
) -> Result<serde_json::Value, String> {
    let (reply, response) = std::sync::mpsc::channel();
    fixture.state.update(cx, |state, _| {
        let token = state.conversations[&thread].generation;
        state.network_tool(thread, token, action, reply);
    });
    let result = std::cell::RefCell::new(None);
    until(
        cx,
        "real network worker response",
        Duration::from_secs(15),
        |_| {
            if result.borrow().is_some() {
                return true;
            }
            if let Ok(response) = response.try_recv() {
                *result.borrow_mut() = Some(response);
            }
            result.borrow().is_some()
        },
    )
    .await;
    result.into_inner().unwrap()
}

async fn ready(cx: &mut TestAppContext, fixture: &Fixture, thread: Uuid) {
    until(
        cx,
        "real OMP peer bridge handshake",
        Duration::from_secs(25),
        |cx| {
            let state = fixture.state.read(cx);
            state.conversations.get(&thread).is_some_and(|session| {
                session.state == ConversationState::Ready
                    && session.persisted
                    && session.peer_delivery
            }) && fixture
                .workspace
                .read(cx)
                .editor
                .as_ref()
                .is_some_and(|editor| {
                    editor.read(cx).active_key() == Some(&DraftKey::Thread(thread))
                })
        },
    )
    .await;
}

async fn omp_thread(cx: &mut TestAppContext, fixture: &Fixture) -> Uuid {
    cx.dispatch_action(fixture.window.into(), commands::NewOmpConversation);
    let thread = cx.read(|cx| fixture.state.read(cx).selected_thread.unwrap());
    ready(cx, fixture, thread).await;
    thread
}

async fn palette(cx: &mut TestAppContext, fixture: &Fixture, keys: &str) {
    open_palette(cx, fixture).await;
    cx.simulate_keystrokes(fixture.window.into(), keys);
    until(
        cx,
        "network palette command dismissed",
        Duration::from_secs(5),
        |cx| fixture.workspace.read(cx).palette.is_none(),
    )
    .await;
}

async fn join(cx: &mut TestAppContext, fixture: &Fixture, thread: Uuid) {
    palette(cx, fixture, "j o i n space n e t w o r k enter").await;
    until(
        cx,
        "automatic consent committed",
        Duration::from_secs(5),
        |cx| fixture.state.read(cx).network_delivery_enabled(thread),
    )
    .await;
}

fn private_input(cx: &TestAppContext, fixture: &Fixture, thread: Uuid) -> serde_json::Value {
    cx.read(|cx| {
        let state = fixture.state.read(cx);
        let saved = state.snapshot.threads.iter().find(|candidate| candidate.id == thread).unwrap();
        json!({"draft": state.drafts[&DraftKey::Thread(thread)].text,
            "attachments": saved.attachments, "submission": saved.submission, "queued": saved.queued})
    })
}

fn disposition(cx: &gpui_kit::App, fixture: &Fixture, message: Uuid) -> Option<DeliveryState> {
    fixture
        .state
        .read(cx)
        .network
        .snapshot
        .messages
        .iter()
        .find(|candidate| candidate.id == message)
        .and_then(|message| message.deliveries.first())
        .map(|delivery| delivery.state.clone())
}

async fn send(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    sender: Uuid,
    recipient: Uuid,
    body: &str,
) -> Uuid {
    let response = network_call(
        cx,
        fixture,
        sender,
        Action::Send {
            recipient,
            body: body.into(),
        },
    )
    .await
    .unwrap();
    serde_json::from_value(response["message"]["id"].clone()).unwrap()
}

fn render_messages(cx: &mut TestAppContext, fixture: &Fixture, thread: Uuid) {
    cx.dispatch_action(fixture.window.into(), commands::ShowNetworkMessages);
    cx.simulate_keystrokes(fixture.window.into(), "g g enter");
    for mode in ["light", "dark"] {
        fixture.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(mode));
            cx.notify();
        });
        for (width, height) in [(1440., 900.), (900., 600.)] {
            cx.simulate_window_resize(fixture.window.into(), size(px(width), px(height)));
            settle_frames(cx, fixture);
            cx.update_window(fixture.window.into(), |_, window, cx| {
                window.render_frame(cx);
                let scope = gpui_kit::SharedString::from(format!("space-network-sidebar-{thread}"));
                let panel = window.find(scope.clone()).bounds();
                let control = window.within(scope.clone()).find("toggle-space-network").bounds();
                assert!(panel.size.width > px(0.) && panel.size.height > px(0.));
                assert!(control.left() >= panel.left() && control.right() <= panel.right());
                let detail = window.within(scope).find("network-selection-detail");
                assert!(detail.visible(),
                    "Message detail is clipped in {mode} at {width}x{height}: panel={panel:?}, control={control:?}, detail={detail:?}");
                let bounds = detail.bounds();
                assert!(bounds.top() >= panel.top() && bounds.bottom() <= panel.bottom(),
                    "Message detail escapes its sidebar in {mode} at {width}x{height}: panel={panel:?}, detail={detail:?}");
                assert_eq!(detail.focused(), Some(true),
                    "Keyboard-opened message loses focus in {mode} at {width}x{height}: {detail:?}");
            }).unwrap();
        }
    }
    cx.simulate_keystrokes(fixture.window.into(), "escape");
}

#[gpui_kit::test]
#[ignore = "requires isolated scratch HOME/config, OS-denied IP networking, installed OMP and Neovim"]
async fn space_network_automatic_delivery_preserves_private_input(cx: &mut TestAppContext) {
    require_blocked_ip();
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
    store.set_setting("executables".into(), json!({
        "omp": std::env::var("DROSTJE_NETWORK_QA_OMP").expect("Explicit installed OMP path"),
        "nvim": std::env::var("DROSTJE_NETWORK_QA_NVIM").expect("Explicit installed Neovim path")
    })).await.unwrap();
    store.flush().await.unwrap();
    drop(store);
    let bridge = smol::unblock(crate::browser_tools::BrowserToolServer::start)
        .await
        .unwrap();
    let fixture = open_fixture_with_tools(cx, Some(data.clone()), None, Some(bridge)).await;
    let first = omp_thread(cx, &fixture).await;
    assert!(
        network_call(cx, &fixture, first, Action::Members)
            .await
            .is_err(),
        "Tool exposure must not silently enroll a runtime"
    );
    join(cx, &fixture, first).await;
    let second = omp_thread(cx, &fixture).await;
    join(cx, &fixture, second).await;
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
    cx.dispatch_action(fixture.window.into(), commands::PasteImage);
    until(
        cx,
        "real pasted attachment saved",
        Duration::from_secs(10),
        |cx| {
            fixture
                .state
                .read(cx)
                .snapshot
                .threads
                .iter()
                .find(|thread| thread.id == second)
                .is_some_and(|thread| thread.attachments.len() == 1)
        },
    )
    .await;
    let private = private_input(cx, &fixture, second);
    let focus = cx
        .update_window(fixture.window.into(), |_, window, cx| window.focused(cx))
        .unwrap()
        .unwrap();
    let direct = send(
        cx,
        &fixture,
        first,
        second,
        "Peer observation: the QA repository contains notes.txt.",
    )
    .await;
    until(
        cx,
        "actual native receipt (not provider generation)",
        Duration::from_secs(25),
        |cx| disposition(cx, &fixture, direct) == Some(DeliveryState::Delivered),
    )
    .await;
    assert_eq!(private_input(cx, &fixture, second), private);
    cx.update_window(fixture.window.into(), |_, window, _| {
        assert!(focus.is_focused(window))
    })
    .unwrap();
    render_messages(cx, &fixture, second);
    until(
        cx,
        "peer admission settled without provider access",
        Duration::from_secs(25),
        |cx| !fixture.state.read(cx).active_threads.contains(&second),
    )
    .await;

    // Reconnect is a new runtime capability: saved membership is not runtime consent.
    let old = cx.read(|cx| fixture.state.read(cx).conversations[&second].generation);
    fixture
        .state
        .update(cx, |state, _| state.retry_conversation(second));
    ready(cx, &fixture, second).await;
    cx.read(|cx| {
        let state = fixture.state.read(cx);
        assert_ne!(state.conversations[&second].generation, old);
        assert!(state.network_joined(second));
        assert!(!state.network_delivery_enabled(second));
    });
    let queued = send(
        cx,
        &fixture,
        first,
        second,
        "Peer observation queued while the reconnected runtime is paused.",
    )
    .await;
    until(
        cx,
        "durably queued while paused",
        Duration::from_secs(5),
        |cx| disposition(cx, &fixture, queued) == Some(DeliveryState::Queued),
    )
    .await;
    render_messages(cx, &fixture, second);
    assert_eq!(
        cx.read(|cx| disposition(cx, &fixture, queued)),
        Some(DeliveryState::Queued)
    );
    palette(
        cx,
        &fixture,
        "r e s u m e space n e t w o r k space d e l i v e r y enter",
    )
    .await;
    until(
        cx,
        "explicit Resume native receipt",
        Duration::from_secs(25),
        |cx| disposition(cx, &fixture, queued) == Some(DeliveryState::Delivered),
    )
    .await;
    assert_eq!(private_input(cx, &fixture, second), private);
    until(
        cx,
        "resumed peer turn settled",
        Duration::from_secs(25),
        |cx| !fixture.state.read(cx).active_threads.contains(&second),
    )
    .await;
    cx.dispatch_action(fixture.window.into(), commands::CloseThread);
    until(cx, "closed native runtime", Duration::from_secs(10), |cx| {
        !fixture.state.read(cx).conversations.contains_key(&second)
    })
    .await;
    let closed = send(
        cx,
        &fixture,
        first,
        second,
        "This message must not restart a closed runtime.",
    )
    .await;
    until(cx, "closed recipient queue", Duration::from_secs(5), |cx| {
        disposition(cx, &fixture, closed) == Some(DeliveryState::Queued)
    })
    .await;
    settle_frames(cx, &fixture);
    assert!(!cx.read(|cx| fixture.state.read(cx).conversations.contains_key(&second)));
    palette(cx, &fixture, "l e a v e space n e t w o r k enter").await;
    until(
        cx,
        "keyboard Leave committed",
        Duration::from_secs(5),
        |cx| !fixture.state.read(cx).network_joined(first),
    )
    .await;
    assert!(
        network_call(cx, &fixture, first, Action::Members)
            .await
            .is_err()
    );
    quit(cx, &fixture).await;

    // An actual pre-automatic saved membership must not inherit new paid-turn consent.
    let saved = fixture.store.snapshot().await.unwrap();
    let saved_thread = saved
        .threads
        .iter()
        .find(|thread| thread.id == second)
        .unwrap();
    assert_eq!(
        json!({
            "draft": saved_thread.draft, "attachments": saved_thread.attachments,
            "submission": saved_thread.submission, "queued": saved_thread.queued,
        }),
        private,
        "Shutdown must persist the same private input"
    );
    let mut legacy = saved.settings[crate::network::SETTING].clone();
    for member in legacy["members"].as_array_mut().unwrap() {
        member.as_object_mut().unwrap().remove("automatic");
        member.as_object_mut().unwrap().remove("delivery_enabled");
    }
    fixture
        .store
        .set_setting(crate::network::SETTING.into(), legacy)
        .await
        .unwrap();
    fixture.store.flush().await.unwrap();
    println!(
        "Automatic network native QA: Join/Resume/Leave, native acceptance, queued reconnect/closed runtime, Draft/attachment/focus safety, both themes and sizes; OS-blocked provider generation. data={}",
        data.display()
    );
}

#[gpui_kit::test]
#[ignore = "requires OS-denied IP networking and DROSTJE_NETWORK_QA_RESTORE_DATA from the automatic-delivery scenario in a previous process"]
async fn space_network_restore_requires_fresh_automatic_consent(cx: &mut TestAppContext) {
    require_blocked_ip();
    cx.dispatcher.allow_parking();
    let root = std::fs::canonicalize(std::env::var_os("DROSTJE_RUNTIME_FIXTURE").unwrap()).unwrap();
    let data = std::fs::canonicalize(std::env::var_os("DROSTJE_NETWORK_QA_RESTORE_DATA").unwrap())
        .unwrap();
    assert!(
        data.starts_with(&root) && data != root,
        "Restore only a disposable QA Store"
    );
    let store = Store::open(data.clone()).unwrap();
    let saved = store.snapshot().await.unwrap();
    let network: crate::network::NetworkSnapshot =
        serde_json::from_value(saved.settings[crate::network::SETTING].clone()).unwrap();
    let (closed, second) = network
        .messages
        .iter()
        .find_map(|message| {
            message
                .deliveries
                .iter()
                .find(|delivery| delivery.state == DeliveryState::Queued)
                .map(|delivery| (message.id, delivery.recipient))
        })
        .expect("Previous process must leave a real queued native message");
    let thread = saved
        .threads
        .iter()
        .find(|thread| thread.id == second)
        .unwrap();
    let private = json!({
        "draft": thread.draft, "attachments": thread.attachments,
        "submission": thread.submission, "queued": thread.queued,
    });
    drop(store);
    let lock = std::fs::File::options()
        .read(true)
        .write(true)
        .open(data.join("instance.lock"))
        .unwrap();
    until(
        cx,
        "snapshot reader released its Store lock",
        Duration::from_secs(10),
        |_| match lock.try_lock() {
            Ok(()) => true,
            Err(std::fs::TryLockError::WouldBlock) => false,
            Err(error) => panic!("Inspect Store lock: {error}"),
        },
    )
    .await;
    drop(lock);
    let bridge = smol::unblock(crate::browser_tools::BrowserToolServer::start)
        .await
        .unwrap();
    let restored = open_fixture_with_tools(cx, Some(data.clone()), None, Some(bridge)).await;
    restored.state.update(cx, |state, cx| {
        state.select_thread(second);
        cx.emit(crate::app::PresentationChange::Selection);
    });
    ready(cx, &restored, second).await;
    cx.read(|cx| {
        let state = restored.state.read(cx);
        assert!(state.network_joined(second));
        assert!(
            !state
                .network
                .snapshot
                .members
                .iter()
                .find(|member| member.thread == second)
                .unwrap()
                .automatic
        );
        assert!(!state.network_delivery_enabled(second));
    });
    render_messages(cx, &restored, second);
    assert_eq!(
        cx.read(|cx| disposition(cx, &restored, closed)),
        Some(DeliveryState::Queued)
    );
    palette(
        cx,
        &restored,
        "r e s u m e space n e t w o r k space d e l i v e r y enter",
    )
    .await;
    until(
        cx,
        "legacy explicit Resume native receipt",
        Duration::from_secs(25),
        |cx| disposition(cx, &restored, closed) == Some(DeliveryState::Delivered),
    )
    .await;
    assert_eq!(private_input(cx, &restored, second), private);
    render_messages(cx, &restored, second);
    quit(cx, &restored).await;
    println!(
        "Automatic network restart QA: real saved legacy consent remained paused until explicit Resume; queued native delivery and private input preserved. data={}",
        data.display()
    );
}
