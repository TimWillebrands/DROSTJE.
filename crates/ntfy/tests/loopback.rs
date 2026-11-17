use std::time::{Duration, Instant};

use drostje_ntfy::{Event, Message, PublishError, Server, test_server::TestServer};

fn next_message(events: &async_channel::Receiver<Event>) -> Message {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(Event::Message(message)) => return message,
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("no message arrived within 10 seconds")
}

fn wait_connected(events: &async_channel::Receiver<Event>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(Event::Connected) => return,
            Ok(other) => panic!("unexpected event before connecting: {other:?}"),
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("subscription did not connect")
}

#[test]
fn published_text_reaches_a_live_subscriber() {
    let ntfy = TestServer::start();
    let server = Server::new(ntfy.address(), None).unwrap();
    let subscription = server.subscribe(&["down"], None);
    wait_connected(&subscription.events);
    let published = server.publish_text("down", "hello").unwrap();
    let message = next_message(&subscription.events);
    assert_eq!(message.id, published.id);
    assert_eq!(message.body, "hello");
    assert_eq!(message.topic, "down");
}

#[test]
fn a_subscription_replays_messages_after_the_last_seen_id() {
    let ntfy = TestServer::start();
    let server = Server::new(ntfy.address(), None).unwrap();
    let first = server.publish_text("t", "one").unwrap();
    server.publish_text("t", "two").unwrap();
    server.publish_text("other", "skip").unwrap();
    server.publish_text("t", "three").unwrap();
    let subscription = server.subscribe(&["t"], Some(first.id));
    wait_connected(&subscription.events);
    assert_eq!(next_message(&subscription.events).body, "two");
    assert_eq!(next_message(&subscription.events).body, "three");
}

#[test]
fn attachments_upload_and_download_from_the_same_server() {
    let ntfy = TestServer::start();
    let server = Server::new(ntfy.address(), None).unwrap();
    let subscription = server.subscribe(&["big"], None);
    wait_connected(&subscription.events);
    let bytes: Vec<u8> = (0..20_000u32).map(|n| n as u8).collect();
    server.publish_attachment("big", &bytes).unwrap();
    let message = next_message(&subscription.events);
    let attachment = message.attachment.expect("attachment metadata");
    assert_eq!(server.download(&attachment, 1 << 20).unwrap(), bytes);
    assert_eq!(
        server.download(&attachment, 100).unwrap_err(),
        PublishError::TooLarge
    );
    assert!(ntfy.has_attachment("big"));
}

#[test]
fn rate_limits_report_the_servers_retry_after() {
    let ntfy = TestServer::start();
    let server = Server::new(ntfy.address(), None).unwrap();
    ntfy.set_rate_limited(true);
    assert_eq!(
        server.publish_text("t", "x").unwrap_err(),
        PublishError::RateLimited {
            retry_after: Some(Duration::from_secs(7))
        }
    );
    assert_eq!(ntfy.publishes(), 0);
}

#[test]
fn tokens_are_sent_to_publish_and_subscribe() {
    let ntfy = TestServer::start();
    ntfy.require_token("tk_phone");
    let anonymous = Server::new(ntfy.address(), None).unwrap();
    assert!(matches!(
        anonymous.publish_text("t", "x"),
        Err(PublishError::Refused { status: 403, .. })
    ));
    let refused = anonymous.subscribe(&["t"], None);
    let deadline = Instant::now() + Duration::from_secs(10);
    let reason = loop {
        if let Ok(Event::Disconnected(reason)) = refused.events.try_recv() {
            break reason;
        }
        assert!(Instant::now() < deadline, "no refusal reported");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(reason.contains("token"), "{reason}");
    let authorized = Server::new(ntfy.address(), Some("tk_phone")).unwrap();
    let subscription = authorized.subscribe(&["t"], None);
    wait_connected(&subscription.events);
    authorized.publish_text("t", "ok").unwrap();
    assert_eq!(next_message(&subscription.events).body, "ok");
}

#[test]
fn stopping_a_subscription_closes_its_events() {
    let ntfy = TestServer::start();
    let server = Server::new(ntfy.address(), None).unwrap();
    let subscription = server.subscribe(&["t"], None);
    wait_connected(&subscription.events);
    subscription.stop();
    assert!(subscription.events.is_closed());
}
