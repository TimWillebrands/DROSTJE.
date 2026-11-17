use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender, TryRecvError};
use drostje_ntfy::{Event, PublishError, Server};
use uuid::Uuid;

use crate::{
    CheckCode, DesktopPairing, Hello, PairReply, PairingOffer, Topic,
    link::{Connection, ConnectionState, LinkRecord, TICK, body, publish},
    messages::{Down, Up, open_up, seal_down},
    outbox::Outbox,
    pairing::IncomingPhone,
    view::PhoneView,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingEvent {
    Listening,
    Incoming { name: String, check: CheckCode },
    Paired { name: String, link: LinkRecord },
    Problem(String),
    Expired,
}

enum PairingCommand {
    Accept { desktop: String },
    Decline,
}

pub struct PairingSession {
    pub offer: PairingOffer,
    commands: Sender<PairingCommand>,
    pub events: Receiver<PairingEvent>,
}

impl PairingSession {
    pub fn start(server: Server, token: Option<String>, ttl: Duration) -> Self {
        let pairing = DesktopPairing::new(server.address().to_owned(), token);
        let offer = pairing.offer.clone();
        let (commands, command_queue) = async_channel::bounded(8);
        let (event_sender, events) = async_channel::bounded(32);
        std::thread::Builder::new()
            .name("phone-pairing".into())
            .spawn(move || run_pairing(server, pairing, ttl, command_queue, event_sender))
            .expect("spawning the phone pairing thread");
        Self {
            offer,
            commands,
            events,
        }
    }

    pub fn accept(&self, desktop: String) {
        let _ = self.commands.try_send(PairingCommand::Accept { desktop });
    }

    pub fn decline(&self) {
        let _ = self.commands.try_send(PairingCommand::Decline);
    }
}

impl Drop for PairingSession {
    fn drop(&mut self) {
        self.commands.close();
        self.events.close();
    }
}

fn run_pairing(
    server: Server,
    pairing: DesktopPairing,
    ttl: Duration,
    commands: Receiver<PairingCommand>,
    events: Sender<PairingEvent>,
) {
    let topic = pairing.offer.topic.clone();
    let subscription = server.subscribe(&[topic.as_str()], None);
    let deadline = Instant::now() + ttl;
    let mut asking: Option<IncomingPhone> = None;
    let mut listening = false;
    let emit = |event| events.try_send(event).is_ok() || !events.is_closed();
    loop {
        if asking.is_none() && Instant::now() >= deadline {
            emit(PairingEvent::Expired);
            return;
        }
        match commands.try_recv() {
            Ok(PairingCommand::Accept { desktop }) => {
                if let Some(incoming) = &asking {
                    let link = LinkRecord {
                        server: server.address().to_owned(),
                        down: Topic::random(),
                        up: Topic::random(),
                        key: incoming.handshake.key.clone(),
                    };
                    let reply = PairReply::Accepted {
                        desktop,
                        down: link.down.clone(),
                        up: link.up.clone(),
                    };
                    let sealed = pairing.seal_reply(&link.key, &reply);
                    match server.publish_text(topic.as_str(), &sealed) {
                        Ok(_) => {
                            emit(PairingEvent::Paired {
                                name: incoming.name.clone(),
                                link,
                            });
                            return;
                        }
                        Err(error) => {
                            emit(PairingEvent::Problem(format!(
                                "Could not answer the phone: {error}"
                            )));
                        }
                    }
                }
            }
            Ok(PairingCommand::Decline) => {
                if let Some(incoming) = asking.take() {
                    let sealed = pairing.seal_reply(&incoming.handshake.key, &PairReply::Declined);
                    let _ = server.publish_text(topic.as_str(), &sealed);
                }
            }
            Err(TryRecvError::Closed) => return,
            Err(TryRecvError::Empty) => {}
        }
        match subscription.events.try_recv() {
            Ok(Event::Connected) => {
                if !listening {
                    listening = true;
                    emit(PairingEvent::Listening);
                }
            }
            Ok(Event::Disconnected(reason)) => {
                listening = false;
                emit(PairingEvent::Problem(reason));
            }
            Ok(Event::Message(message)) if asking.is_none() => {
                let incoming = serde_json::from_str::<Hello>(&message.body)
                    .ok()
                    .and_then(|hello| pairing.open_hello(&hello).ok());
                if let Some(incoming) = incoming {
                    emit(PairingEvent::Incoming {
                        name: incoming.name.clone(),
                        check: incoming.handshake.check,
                    });
                    asking = Some(incoming);
                }
            }
            Ok(Event::Message(_)) | Err(TryRecvError::Empty) => std::thread::sleep(TICK),
            Err(TryRecvError::Closed) => {
                emit(PairingEvent::Problem("Subscription ended".into()));
                return;
            }
        }
        if events.is_closed() {
            return;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkEvent {
    Watch(Option<Uuid>),
    Position(String),
    Connection(Connection),
    Unpaired,
    Revoked,
    Closed,
}

enum LinkCommand {
    Revoke,
    Close,
}

pub struct DesktopLink {
    views: Sender<PhoneView>,
    commands: Sender<LinkCommand>,
    pub events: Receiver<LinkEvent>,
    finished: Receiver<()>,
}

impl DesktopLink {
    pub fn start(server: Server, link: LinkRecord, up_since: Option<String>) -> Self {
        let (views, view_queue) = async_channel::bounded(1);
        let (commands, command_queue) = async_channel::bounded(4);
        let (event_sender, events) = async_channel::bounded(64);
        let (done, finished) = async_channel::bounded::<()>(1);
        std::thread::Builder::new()
            .name("phone-link".into())
            .spawn(move || {
                let _done = done;
                run_link(
                    server,
                    link,
                    up_since,
                    view_queue,
                    command_queue,
                    event_sender,
                )
            })
            .expect("spawning the phone link thread");
        Self {
            views,
            commands,
            events,
            finished,
        }
    }

    pub fn finished(&self) -> Receiver<()> {
        self.finished.clone()
    }

    pub fn show(&self, view: PhoneView) {
        let _ = self.views.force_send(view);
    }

    pub fn revoke(&self) {
        let _ = self.commands.try_send(LinkCommand::Revoke);
    }

    pub fn close(&self) {
        let _ = self.commands.try_send(LinkCommand::Close);
    }
}

impl Drop for DesktopLink {
    fn drop(&mut self) {
        let _ = self.commands.try_send(LinkCommand::Close);
    }
}

fn send_down(server: &Server, link: &LinkRecord, down: &Down) -> Result<(), PublishError> {
    publish(server, &link.down, seal_down(&link.key, &link.down, down)).map(|_| ())
}

fn run_link(
    server: Server,
    link: LinkRecord,
    up_since: Option<String>,
    views: Receiver<PhoneView>,
    commands: Receiver<LinkCommand>,
    events: Sender<LinkEvent>,
) {
    let subscription = server.subscribe(&[link.up.as_str()], up_since);
    let mut outbox = Outbox::new(Instant::now());
    let mut current: Option<PhoneView> = None;
    let mut connection = ConnectionState::new();
    let emit = |event: LinkEvent| {
        let _ = events.send_blocking(event);
    };
    loop {
        if let Some(state) = connection.changed() {
            emit(LinkEvent::Connection(state));
        }
        match commands.try_recv() {
            Ok(LinkCommand::Revoke) => {
                let mut attempts = 0;
                while send_down(&server, &link, &Down::Revoked).is_err() && attempts < 2 {
                    attempts += 1;
                    std::thread::sleep(Duration::from_secs(1));
                }
                emit(LinkEvent::Revoked);
                return;
            }
            Ok(LinkCommand::Close) | Err(TryRecvError::Closed) => {
                if current.is_some() {
                    let _ = send_down(&server, &link, &outbox.farewell());
                }
                emit(LinkEvent::Closed);
                return;
            }
            Err(TryRecvError::Empty) => {}
        }
        while let Ok(view) = views.try_recv() {
            current = Some(view);
        }
        let mut idle = true;
        match subscription.events.try_recv() {
            Ok(Event::Connected) => {
                idle = false;
                connection.subscribed();
            }
            Ok(Event::Disconnected(reason)) => {
                idle = false;
                connection.subscription_lost(reason);
            }
            Ok(Event::Message(message)) => {
                idle = false;
                let up = body(&server, &message)
                    .ok()
                    .and_then(|body| open_up(&link.key, &link.up, body.received()).ok());
                emit(LinkEvent::Position(message.id));
                match up {
                    Some(Up::Sync { watch }) => {
                        outbox.request_reset();
                        emit(LinkEvent::Watch(watch));
                    }
                    Some(Up::Watch { thread }) => emit(LinkEvent::Watch(thread)),
                    Some(Up::Unpaired) => {
                        emit(LinkEvent::Unpaired);
                        return;
                    }
                    None => {}
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Closed) => {
                emit(LinkEvent::Closed);
                return;
            }
        }
        if let Some(view) = &current {
            let now = Instant::now();
            if let Some(prepared) = outbox.prepare(view, now) {
                idle = false;
                match send_down(&server, &link, &prepared.down) {
                    Ok(()) => {
                        outbox.delivered(prepared, Instant::now());
                        connection.published(Ok(()));
                    }
                    Err(PublishError::RateLimited { retry_after }) => {
                        outbox.rate_limited(prepared, retry_after, Instant::now());
                        connection
                            .published(Err("Server rate limit reached; updates slow down".into()));
                    }
                    Err(error) => {
                        outbox.failed(prepared, Instant::now());
                        connection.published(Err(error.to_string()));
                    }
                }
            }
        }
        if idle {
            std::thread::sleep(TICK);
        }
    }
}
