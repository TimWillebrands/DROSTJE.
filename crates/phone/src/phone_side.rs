use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_channel::{Receiver, Sender, TryRecvError};
use drostje_ntfy::{Event, Server};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    CheckCode, PairReply, PairingOffer,
    link::{Connection, ConnectionState, LinkRecord, PAIRING_TTL, TICK, body, publish},
    messages::{Up, open_down, seal_up},
    mirror::{Applied, Mirror},
    pairing::{open_reply, phone_hello},
    view::PhoneView,
};

const SYNC_RETRY: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDesktop {
    pub desktop: String,
    pub token: Option<String>,
    pub link: LinkRecord,
    #[serde(default)]
    pub since: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhonePairingEvent {
    Sent { check: CheckCode },
    Paired(PairedDesktop),
    Declined,
    Failed(String),
}

pub struct PhonePairing {
    pub events: Receiver<PhonePairingEvent>,
}

impl PhonePairing {
    pub fn start(offer: PairingOffer, phone_name: String) -> Self {
        let (sender, events) = async_channel::bounded(8);
        std::thread::Builder::new()
            .name("phone-pairing".into())
            .spawn(move || {
                let result = pair(&offer, &phone_name, &sender);
                let event = match result {
                    Ok(event) => event,
                    Err(reason) => PhonePairingEvent::Failed(reason),
                };
                let _ = sender.try_send(event);
            })
            .expect("spawning the pairing thread");
        Self { events }
    }
}

impl Drop for PhonePairing {
    fn drop(&mut self) {
        self.events.close();
    }
}

fn pair(
    offer: &PairingOffer,
    phone_name: &str,
    events: &Sender<PhonePairingEvent>,
) -> Result<PhonePairingEvent, String> {
    let server = Server::new(&offer.server, offer.token.as_deref())
        .map_err(|error| format!("The pairing code names an unusable server: {error}"))?;
    let (hello, handshake) = phone_hello(offer, phone_name).map_err(|error| error.to_string())?;
    let text = serde_json::to_string(&hello).expect("a hello always serializes");
    let published = server
        .publish_text(offer.topic.as_str(), &text)
        .map_err(|error| format!("Could not reach the desktop's server: {error}"))?;
    if events
        .try_send(PhonePairingEvent::Sent {
            check: handshake.check,
        })
        .is_err()
    {
        return Err("Pairing was cancelled".into());
    }
    let subscription = server.subscribe(&[offer.topic.as_str()], Some(published.id));
    let deadline = Instant::now() + PAIRING_TTL;
    while Instant::now() < deadline {
        if events.is_closed() {
            return Err("Pairing was cancelled".into());
        }
        match subscription.events.try_recv() {
            Ok(Event::Message(message)) => match open_reply(offer, &handshake.key, &message.body) {
                Ok(PairReply::Accepted { desktop, down, up }) => {
                    return Ok(PhonePairingEvent::Paired(PairedDesktop {
                        desktop,
                        token: offer.token.clone(),
                        link: LinkRecord {
                            server: server.address().to_owned(),
                            down,
                            up,
                            key: handshake.key,
                        },
                        since: None,
                    }));
                }
                Ok(PairReply::Declined) => return Ok(PhonePairingEvent::Declined),
                Err(_) => {}
            },
            Ok(Event::Connected | Event::Disconnected(_)) => {}
            Err(TryRecvError::Empty) => std::thread::sleep(TICK),
            Err(TryRecvError::Closed) => return Err("Lost the connection to the server".into()),
        }
    }
    Err("The desktop did not answer in time; show a new code and scan again".into())
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub view: Arc<PhoneView>,
    pub synced: bool,
    pub desktop_online: Option<bool>,
    pub heard_at: Option<i64>,
}

#[derive(Clone, Debug)]
pub enum PhoneEvent {
    Snapshot(Snapshot),
    Position(String),
    Connection(Connection),
    Revoked,
    Closed,
}

enum PhoneCommand {
    Watch(Option<Uuid>),
    Unpair,
    Close,
}

pub struct PhoneLink {
    commands: Sender<PhoneCommand>,
    pub events: Receiver<PhoneEvent>,
}

impl PhoneLink {
    pub fn start(paired: PairedDesktop, cached: Option<PhoneView>) -> Result<Self, String> {
        let server = Server::new(&paired.link.server, paired.token.as_deref())
            .map_err(|error| error.to_string())?;
        let (commands, command_queue) = async_channel::bounded(16);
        let (sender, events) = async_channel::bounded(64);
        std::thread::Builder::new()
            .name("phone-link".into())
            .spawn(move || run(server, paired, cached, command_queue, sender))
            .expect("spawning the phone link thread");
        Ok(Self { commands, events })
    }

    pub fn watch(&self, thread: Option<Uuid>) {
        let _ = self.commands.try_send(PhoneCommand::Watch(thread));
    }

    pub fn unpair(&self) {
        let _ = self.commands.try_send(PhoneCommand::Unpair);
    }
}

impl Drop for PhoneLink {
    fn drop(&mut self) {
        let _ = self.commands.try_send(PhoneCommand::Close);
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

fn run(
    server: Server,
    paired: PairedDesktop,
    cached: Option<PhoneView>,
    commands: Receiver<PhoneCommand>,
    events: Sender<PhoneEvent>,
) {
    let link = paired.link;
    let subscription = server.subscribe(&[link.down.as_str()], paired.since);
    let mut mirror = cached.map(Mirror::from_cache).unwrap_or_default();
    let mut connection = ConnectionState::new();
    let mut watch: Option<Uuid> = None;
    let mut sync_sent: Option<Instant> = None;
    let mut heard_at: Option<i64> = None;
    let emit = |event: PhoneEvent| {
        let _ = events.send_blocking(event);
    };
    let send = |up: &Up, connection: &mut ConnectionState| {
        let result = publish(&server, &link.up, seal_up(&link.key, &link.up, up));
        connection.published(result.map(|_| ()).map_err(|error| error.to_string()));
    };
    let snapshot = |mirror: &Mirror, heard_at: Option<i64>| {
        PhoneEvent::Snapshot(Snapshot {
            view: Arc::new(mirror.view.clone()),
            synced: mirror.synced(),
            desktop_online: mirror.desktop_online,
            heard_at,
        })
    };
    emit(snapshot(&mirror, heard_at));
    loop {
        if let Some(state) = connection.changed() {
            emit(PhoneEvent::Connection(state));
        }
        match commands.try_recv() {
            Ok(PhoneCommand::Watch(thread)) => {
                watch = thread;
                send(&Up::Watch { thread }, &mut connection);
                continue;
            }
            Ok(PhoneCommand::Unpair) => {
                send(&Up::Unpaired, &mut connection);
                emit(PhoneEvent::Closed);
                return;
            }
            Ok(PhoneCommand::Close) | Err(TryRecvError::Closed) => {
                emit(PhoneEvent::Closed);
                return;
            }
            Err(TryRecvError::Empty) => {}
        }
        let needs_sync = !mirror.synced()
            && connection.is_subscribed()
            && sync_sent.is_none_or(|sent| sent.elapsed() >= SYNC_RETRY);
        if needs_sync {
            mirror.request_reset();
            sync_sent = Some(Instant::now());
            send(&Up::Sync { watch }, &mut connection);
            continue;
        }
        match subscription.events.try_recv() {
            Ok(Event::Connected) => connection.subscribed(),
            Ok(Event::Disconnected(reason)) => connection.subscription_lost(reason),
            Ok(Event::Message(message)) => {
                let down = body(&server, &message)
                    .ok()
                    .and_then(|body| open_down(&link.key, &link.down, body.received()).ok());
                emit(PhoneEvent::Position(message.id.clone()));
                let Some(down) = down else { continue };
                heard_at = Some(if message.time > 0 {
                    message.time
                } else {
                    now_unix()
                });
                match mirror.receive(down) {
                    Applied::Revoked => {
                        emit(PhoneEvent::Revoked);
                        return;
                    }
                    Applied::Reset => {
                        sync_sent = None;
                        let showing = mirror.view.transcript.as_ref().map(|t| t.thread);
                        if watch.is_some() && showing != watch {
                            send(&Up::Watch { thread: watch }, &mut connection);
                        }
                        emit(snapshot(&mirror, heard_at));
                    }
                    Applied::Changed => emit(snapshot(&mirror, heard_at)),
                    Applied::NeedsSync => {
                        sync_sent = Some(Instant::now());
                        send(&Up::Sync { watch }, &mut connection);
                    }
                    Applied::Unchanged => {}
                }
            }
            Err(TryRecvError::Empty) => std::thread::sleep(TICK),
            Err(TryRecvError::Closed) => {
                emit(PhoneEvent::Closed);
                return;
            }
        }
    }
}
