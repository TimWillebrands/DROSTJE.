use crate::{
    app::{AppState, Bootstrap, PhonePairingStatus},
    phone::{
        PairingOffer,
        phone_side::{PhonePairing, PhonePairingEvent},
    },
    store::Store,
    ui::{commands, commands::Command, workspace::Workspace},
};
use drostje_ntfy::test_server::TestServer;
use gpui_kit::{
    AnyWindowHandle, AppContext, Entity, TestAppContext,
    component::{Root, Theme, ThemeMode},
    test::TestWindowExt,
};
use std::time::{Duration, Instant};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(20);

fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<AppState>) {
    cx.dispatcher.allow_parking();
    let directory = std::env::temp_dir().join(format!("drostje-phones-ui-{}", Uuid::new_v4()));
    let store = Store::open(directory.join("ui")).unwrap();
    let snapshot = smol::block_on(store.snapshot()).unwrap();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::theme::initialize(None, Some(&directory), cx);
        commands::bind_keys(cx);
    });
    let state = cx.new(|cx| {
        AppState::new(
            Bootstrap {
                data_dir: directory.clone(),
                store,
                snapshot,
                diagnostics: Default::default(),
                repositories: Default::default(),
                browser_tools: None,
            },
            cx,
        )
    });
    let window = cx.add_window(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(state.clone(), window, cx));
        let content = cx.new(|_| crate::ui::workspace::WorkspaceRoot { workspace });
        Root::new(content, window, cx)
    });
    (window.into(), state)
}

fn status(cx: &mut TestAppContext, state: &Entity<AppState>) -> Option<PhonePairingStatus> {
    cx.read(|cx| {
        state
            .read(cx)
            .phones
            .pairing
            .as_ref()
            .map(|pairing| pairing.status.clone())
    })
}

fn wait(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn visible(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> bool {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some_and(|element| element.visible())
    })
    .unwrap()
}

fn both_themes(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        cx.update_window(window, |_, window, cx| {
            Theme::change(mode, Some(window), cx);
            window.render_frame(cx);
            assert!(window.find(id).visible(), "{id} is visible in {mode:?}");
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn a_phone_pairs_from_the_dialog_with_its_check_code(cx: &mut TestAppContext) {
    let (window, state) = open(cx);
    assert_eq!(
        cx.read(|cx| Command::PairPhone.disabled_reason(state.read(cx))),
        Some("Set a phone server in Settings › Phones first.".into())
    );
    let ntfy = TestServer::start();
    state.update(cx, |state, _| {
        state.save_phone_server(ntfy.address(), "").unwrap()
    });
    assert_eq!(
        cx.read(|cx| Command::PairPhone.disabled_reason(state.read(cx))),
        None
    );

    cx.dispatch_action(window, commands::PairPhone);
    wait(cx, window, "the QR code", |cx| {
        status(cx, &state) == Some(PhonePairingStatus::Showing)
    });
    both_themes(cx, window, "pair-phone-qr");
    assert!(visible(cx, window, "pair-phone-status"));

    let link = cx.read(|cx| state.read(cx).phones.pairing.as_ref().unwrap().link.clone());
    let phone = PhonePairing::start(PairingOffer::parse(&link).unwrap(), "Pixel 9".into());
    let deadline = Instant::now() + WAIT;
    let phone_check = loop {
        if let Ok(PhonePairingEvent::Sent { check }) = phone.events.try_recv() {
            break check.to_string();
        }
        assert!(Instant::now() < deadline, "the phone never sent its hello");
        std::thread::sleep(Duration::from_millis(10));
    };
    wait(cx, window, "the phone to ask", |cx| {
        matches!(
            status(cx, &state),
            Some(PhonePairingStatus::Incoming { .. })
        )
    });
    assert_eq!(
        status(cx, &state),
        Some(PhonePairingStatus::Incoming {
            name: "Pixel 9".into(),
            check: phone_check,
        })
    );
    both_themes(cx, window, "pair-phone-check");

    cx.update_window(window, |_, window, cx| window.click("dialog-confirm", cx))
        .unwrap();
    wait(cx, window, "the pairing to finish", |cx| {
        matches!(status(cx, &state), Some(PhonePairingStatus::Paired { .. }))
    });
    both_themes(cx, window, "pair-phone-paired");
    let paired = cx.read(|cx| state.read(cx).paired_phones());
    assert_eq!(paired.len(), 1);
    assert_eq!(paired[0].name, "Pixel 9");
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(PhonePairingEvent::Paired(desktop)) = phone.events.try_recv() {
            assert_eq!(desktop.link, paired[0].link);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the phone never learned it was paired"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    cx.update_window(window, |_, window, cx| window.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(status(cx, &state).is_none(), "Escape closes Pair phone");
    assert!(!visible(cx, window, "pair-phone-paired"));

    let id = paired[0].id;
    assert_eq!(cx.read(|cx| state.read(cx).remove_phone_reason(id)), None);
    state.update(cx, |state, _| state.remove_phone(id).unwrap());
    assert!(cx.read(|cx| state.read(cx).paired_phones().is_empty()));
    assert_eq!(
        cx.read(|cx| state.read(cx).remove_phone_reason(id)),
        Some("This phone is no longer paired".into())
    );
}

#[gpui_kit::test]
fn an_unreachable_server_offers_a_new_code(cx: &mut TestAppContext) {
    let (window, state) = open(cx);
    state.update(cx, |state, _| {
        state.save_phone_server("http://127.0.0.1:9", "").unwrap()
    });
    cx.dispatch_action(window, commands::PairPhone);
    wait(cx, window, "the connection problem", |cx| {
        matches!(status(cx, &state), Some(PhonePairingStatus::Problem(_)))
    });
    both_themes(cx, window, "pair-phone-unreachable");
    cx.update_window(window, |_, window, cx| window.click("dialog-confirm", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(matches!(
        status(cx, &state),
        Some(PhonePairingStatus::Connecting | PhonePairingStatus::Problem(_))
    ));
    cx.update_window(window, |_, window, cx| window.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(status(cx, &state).is_none());
}

#[gpui_kit::test]
fn the_phone_server_setting_is_validated(cx: &mut TestAppContext) {
    let (_, state) = open(cx);
    state.update(cx, |state, _| {
        assert!(
            state
                .save_phone_server("http://ntfy.example.org", "")
                .is_err()
        );
        assert!(
            state
                .save_phone_server("https://ntfy.example.org", "BAD NAME")
                .is_err()
        );
        state
            .save_phone_server("https://ntfy.example.org", "DROSTJE_PHONE_TOKEN_UNSET")
            .unwrap();
        assert_eq!(state.phone_server(), "https://ntfy.example.org");
        let reason = Command::PairPhone.disabled_reason(state).unwrap();
        assert!(reason.contains("DROSTJE_PHONE_TOKEN_UNSET"), "{reason}");
    });
}
