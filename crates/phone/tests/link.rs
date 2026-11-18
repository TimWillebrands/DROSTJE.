use std::time::{Duration, Instant};

use drostje_ntfy::test_server::TestServer;
use drostje_phone::{
    PairingOffer, Server,
    desktop_side::{DesktopLink, LinkEvent, PairingEvent, PairingSession},
    link::Connection,
    phone_side::PromptPosted,
    phone_side::{PairedDesktop, PhoneEvent, PhoneLink, PhonePairing, PhonePairingEvent, Snapshot},
    view::{
        ItemKind, PhoneView, ProjectView, PromptOutcome, PromptReceipt, Prompting, SpaceView,
        ThreadKind, ThreadStatus, ThreadView, Transcript, TranscriptItem,
    },
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(15);

fn wait_for<T, E>(events: &async_channel::Receiver<E>, mut pick: impl FnMut(E) -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(event) => {
                if let Some(found) = pick(event) {
                    return found;
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("expected event did not arrive")
}

fn thread_id() -> Uuid {
    Uuid::from_u128(7)
}

fn desktop_view(status: ThreadStatus, transcript: Option<Transcript>) -> PhoneView {
    PhoneView {
        desktop: "Studio Mac".into(),
        spaces: vec![SpaceView {
            id: Uuid::from_u128(1),
            name: "Default".into(),
            projects: vec![ProjectView {
                id: Uuid::from_u128(2),
                name: "drost".into(),
                threads: vec![thread_id()],
            }],
        }],
        threads: [(
            thread_id(),
            ThreadView {
                id: thread_id(),
                title: "Fix the login bug".into(),
                kind: ThreadKind::Conversation {
                    harness: "claude-code".into(),
                },
                status,
                unseen: false,
                updated_at: "2026-11-17T10:00:00Z".into(),
            },
        )]
        .into(),
        transcript,
        theme: None,
        receipts: Vec::new(),
    }
}

fn transcript(text: &str) -> Transcript {
    Transcript {
        thread: thread_id(),
        earlier: false,
        items: vec![TranscriptItem {
            id: "m1".into(),
            kind: ItemKind::Assistant,
            text: text.into(),
            clipped: false,
            run: None,
        }],
        prompting: Prompting::Open,
    }
}

fn pair(ntfy: &TestServer) -> (PairedDesktop, drostje_phone::link::LinkRecord) {
    let server = Server::new(ntfy.address(), None).unwrap();
    let session = PairingSession::start(server, None, Duration::from_secs(60));
    wait_for(&session.events, |event| {
        (event == PairingEvent::Listening).then_some(())
    });
    let offer = PairingOffer::parse(&session.offer.to_link()).unwrap();
    let phone = PhonePairing::start(offer, "Pixel 9".into());
    let phone_check = wait_for(&phone.events, |event| match event {
        PhonePairingEvent::Sent { check } => Some(check),
        _ => None,
    });
    let (name, desktop_check) = wait_for(&session.events, |event| match event {
        PairingEvent::Incoming { name, check } => Some((name, check)),
        _ => None,
    });
    assert_eq!(name, "Pixel 9");
    assert_eq!(phone_check, desktop_check);
    session.accept("Studio Mac".into());
    let link = wait_for(&session.events, |event| match event {
        PairingEvent::Paired { link, .. } => Some(link),
        _ => None,
    });
    let paired = wait_for(&phone.events, |event| match event {
        PhonePairingEvent::Paired(paired) => Some(paired),
        _ => None,
    });
    assert_eq!(paired.desktop, "Studio Mac");
    assert_eq!(paired.link, link);
    (paired, link)
}

fn snapshot(
    events: &async_channel::Receiver<PhoneEvent>,
    check: impl Fn(&Snapshot) -> bool,
) -> Snapshot {
    wait_for(events, |event| match event {
        PhoneEvent::Snapshot(snapshot) if check(&snapshot) => Some(snapshot),
        _ => None,
    })
}

#[test]
fn a_paired_phone_mirrors_threads_and_follows_the_watched_transcript() {
    let ntfy = TestServer::start();
    let (paired, link) = pair(&ntfy);
    let down = link.down.clone();
    let server = Server::new(ntfy.address(), None).unwrap();
    let desktop = DesktopLink::start(server, link, None);
    desktop.show(desktop_view(ThreadStatus::Running, None));
    let phone = PhoneLink::start(paired, None).unwrap();
    let synced = snapshot(&phone.events, |snapshot| snapshot.synced);
    assert_eq!(
        synced.view.threads[&thread_id()].status,
        ThreadStatus::Running
    );
    assert_eq!(synced.desktop_online, Some(true));

    phone.watch(Some(thread_id()));
    let watched = wait_for(&desktop.events, |event| match event {
        LinkEvent::Watch(Some(thread)) => Some(thread),
        _ => None,
    });
    assert_eq!(watched, thread_id());
    desktop.show(desktop_view(
        ThreadStatus::Running,
        Some(transcript("Looking")),
    ));
    snapshot(&phone.events, |snapshot| snapshot.view.transcript.is_some());
    desktop.show(desktop_view(
        ThreadStatus::Waiting,
        Some(transcript("Looking at the login form")),
    ));
    let latest = snapshot(&phone.events, |snapshot| {
        snapshot.view.threads[&thread_id()].status == ThreadStatus::Waiting
    });
    assert_eq!(
        latest.view.transcript.as_ref().unwrap().items[0].text,
        "Looking at the login form"
    );
    for message in ntfy.messages(down.as_str()) {
        assert!(!message.contains("login"), "plaintext leaked: {message}");
    }

    desktop.revoke();
    wait_for(&phone.events, |event| {
        matches!(event, PhoneEvent::Revoked).then_some(())
    });
}

#[test]
fn a_prompt_from_the_phone_reaches_the_desktop_sealed_and_its_receipt_comes_back() {
    let ntfy = TestServer::start();
    let (paired, link) = pair(&ntfy);
    let up = link.up.clone();
    let server = Server::new(ntfy.address(), None).unwrap();
    let desktop = DesktopLink::start(server, link, None);
    desktop.show(desktop_view(ThreadStatus::Idle, Some(transcript("Done"))));
    let phone = PhoneLink::start(paired, None).unwrap();
    snapshot(&phone.events, |snapshot| snapshot.synced);

    let id = Uuid::from_u128(42);
    phone
        .prompt(id, thread_id(), "Also cover the logout redirect".into())
        .unwrap();
    let posted = wait_for(&phone.events, |event| match event {
        PhoneEvent::Prompt { id: sent, posted } if sent == id => Some(posted),
        _ => None,
    });
    assert_eq!(posted, PromptPosted::Published);
    let (thread, text, sent_at) = wait_for(&desktop.events, |event| match event {
        LinkEvent::Prompt {
            id: received,
            thread,
            text,
            sent_at,
        } if received == id => Some((thread, text, sent_at)),
        _ => None,
    });
    assert_eq!(thread, thread_id());
    assert_eq!(text, "Also cover the logout redirect");
    assert!(sent_at > 0, "the server time travels with the prompt");
    for message in ntfy.messages(up.as_str()) {
        assert!(!message.contains("logout"), "plaintext leaked: {message}");
    }

    desktop.show(PhoneView {
        receipts: vec![PromptReceipt {
            id,
            outcome: PromptOutcome::Started,
        }],
        ..desktop_view(ThreadStatus::Running, Some(transcript("Done")))
    });
    snapshot(&phone.events, |snapshot| {
        snapshot
            .view
            .receipts
            .iter()
            .any(|receipt| receipt.id == id)
    });
}

#[test]
fn a_prompt_the_server_cannot_take_is_reported_as_unknown() {
    let server = Server::new("http://127.0.0.1:9", None).unwrap();
    let paired = PairedDesktop {
        desktop: "Studio Mac".into(),
        token: None,
        link: drostje_phone::link::LinkRecord {
            server: server.address().into(),
            down: drostje_phone::Topic::random(),
            up: drostje_phone::Topic::random(),
            key: drostje_phone::LinkKey::from_bytes([1; 32]),
        },
        since: None,
    };
    let phone = PhoneLink::start(paired, None).unwrap();
    phone
        .prompt(Uuid::from_u128(1), thread_id(), "Hello".into())
        .unwrap();
    let posted = wait_for(&phone.events, |event| match event {
        PhoneEvent::Prompt { posted, .. } => Some(posted),
        _ => None,
    });
    assert!(
        matches!(&posted, PromptPosted::Unknown(reason) if reason.contains("unreachable")),
        "{posted:?}"
    );
}

#[test]
fn large_views_travel_as_attachments_and_nothing_readable_reaches_the_server() {
    let ntfy = TestServer::start();
    let (paired, link) = pair(&ntfy);
    let down = link.down.clone();
    let server = Server::new(ntfy.address(), None).unwrap();
    let desktop = DesktopLink::start(server, link, None);
    let long = "secret transcript text ".repeat(400);
    desktop.show(desktop_view(ThreadStatus::Idle, Some(transcript(&long))));
    let phone = PhoneLink::start(paired, None).unwrap();
    let synced = snapshot(&phone.events, |snapshot| snapshot.synced);
    assert_eq!(synced.view.transcript.as_ref().unwrap().items[0].text, long);
    assert!(ntfy.has_attachment(down.as_str()));
    for message in ntfy.messages(down.as_str()) {
        assert!(!message.contains("secret"), "plaintext leaked: {message}");
    }
}

#[test]
fn a_restarted_desktop_resets_the_phone_and_gets_the_watch_back() {
    let ntfy = TestServer::start();
    let (paired, link) = pair(&ntfy);
    let server = Server::new(ntfy.address(), None).unwrap();
    let first = DesktopLink::start(server.clone(), link.clone(), None);
    first.show(desktop_view(ThreadStatus::Idle, None));
    let phone = PhoneLink::start(paired, None).unwrap();
    snapshot(&phone.events, |snapshot| snapshot.synced);
    phone.watch(Some(thread_id()));
    wait_for(&first.events, |event| {
        matches!(event, LinkEvent::Watch(Some(_))).then_some(())
    });
    let position = wait_for(&first.events, |event| match event {
        LinkEvent::Position(id) => Some(id),
        _ => None,
    });
    drop(first);
    let offline = snapshot(&phone.events, |snapshot| {
        snapshot.desktop_online == Some(false)
    });
    assert!(offline.synced);
    let second = DesktopLink::start(server, link, Some(position));
    second.show(desktop_view(ThreadStatus::Running, None));
    snapshot(&phone.events, |snapshot| {
        snapshot.desktop_online == Some(true)
            && snapshot.view.threads[&thread_id()].status == ThreadStatus::Running
    });
    let watch = wait_for(&second.events, |event| match event {
        LinkEvent::Watch(Some(thread)) => Some(thread),
        _ => None,
    });
    assert_eq!(watch, thread_id());
}

#[test]
fn a_declined_phone_is_told_so() {
    let ntfy = TestServer::start();
    let server = Server::new(ntfy.address(), None).unwrap();
    let session = PairingSession::start(server, None, Duration::from_secs(60));
    wait_for(&session.events, |event| {
        (event == PairingEvent::Listening).then_some(())
    });
    let phone = PhonePairing::start(session.offer.clone(), "Unknown".into());
    wait_for(&session.events, |event| {
        matches!(event, PairingEvent::Incoming { .. }).then_some(())
    });
    session.decline();
    wait_for(&phone.events, |event| {
        (event == PhonePairingEvent::Declined).then_some(())
    });
}

#[test]
fn an_unanswered_pairing_code_expires() {
    let ntfy = TestServer::start();
    let server = Server::new(ntfy.address(), None).unwrap();
    let session = PairingSession::start(server, None, Duration::from_millis(300));
    wait_for(&session.events, |event| {
        (event == PairingEvent::Expired).then_some(())
    });
}

#[test]
fn an_unpairing_phone_tells_the_desktop() {
    let ntfy = TestServer::start();
    let (paired, link) = pair(&ntfy);
    let server = Server::new(ntfy.address(), None).unwrap();
    let desktop = DesktopLink::start(server, link, None);
    desktop.show(desktop_view(ThreadStatus::Idle, None));
    let phone = PhoneLink::start(paired, None).unwrap();
    snapshot(&phone.events, |snapshot| snapshot.synced);
    phone.unpair();
    wait_for(&desktop.events, |event| {
        (event == LinkEvent::Unpaired).then_some(())
    });
}

#[test]
fn an_unreachable_server_is_reported_as_a_connection_problem() {
    let server = Server::new("http://127.0.0.1:9", None).unwrap();
    let link = drostje_phone::link::LinkRecord {
        server: server.address().into(),
        down: drostje_phone::Topic::random(),
        up: drostje_phone::Topic::random(),
        key: drostje_phone::LinkKey::from_bytes([1; 32]),
    };
    let desktop = DesktopLink::start(server, link, None);
    let problem = wait_for(&desktop.events, |event| match event {
        LinkEvent::Connection(Connection::Problem(reason)) => Some(reason),
        _ => None,
    });
    assert!(problem.contains("127.0.0.1"), "{problem}");
}
