//! SEINTJE framing is isolated here. Queued mutations never survive a lost attachment.
use std::{
    io::ErrorKind,
    net::{TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail, ensure};
use async_channel::{Receiver, Sender};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned, de::IgnoredAny};
use tungstenite::{
    Message, WebSocket, client::IntoClientRequest, protocol::WebSocketConfig,
    stream::MaybeTlsStream,
};
use uuid::Uuid;

use super::{config::validate_relay_url, wire::MAX_FRAME_BYTES};

const QUEUE: usize = 32;
const IO_TICK: Duration = Duration::from_millis(100);
/// Read timeout while traffic flows, so queued commands never wait behind an idle read.
const ACTIVE_TICK: Duration = Duration::from_millis(1);
/// Bytes written per pass before liveness and incoming traffic are checked again.
const PASS_BYTES: usize = 1024 * 1024;
/// How long a full receive queue may stall the attachment before it is closed.
const RECEIVE_WAIT: Duration = Duration::from_secs(1);
const HEARTBEAT: Duration = Duration::from_secs(5);
const LEASE: Duration = Duration::from_secs(15);
const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(60);
const PROTOCOL: &str = "seintje-relay/1";
/// Room left in every frame for the relay envelope around a body.
pub const BODY_BUDGET: usize = MAX_FRAME_BYTES - 4096;

#[derive(Debug)]
struct AttachmentRevoked(&'static str);
impl std::fmt::Display for AttachmentRevoked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for AttachmentRevoked {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Desktop,
    Host,
}
impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Host => "host",
        }
    }
}

/// One message body, serialized once by its producer and framed verbatim.
#[derive(Debug)]
pub struct Outgoing {
    body: String,
    /// The controller attachment a desktop request was built for; never read for hosts.
    epoch: Option<Uuid>,
}
#[derive(Debug)]
pub enum EncodeError {
    /// The body does not fit `BODY_BUDGET`; serialization stopped at the limit.
    Oversized,
    Serialize(String),
}
impl std::fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Oversized => formatter.write_str("Remote frame limit exceeded"),
            Self::Serialize(error) => formatter.write_str(error),
        }
    }
}
impl std::error::Error for EncodeError {}
impl Outgoing {
    pub fn encode(value: &impl Serialize, epoch: Option<Uuid>) -> Result<Self, EncodeError> {
        let mut writer = Bounded {
            bytes: Vec::new(),
            remaining: BODY_BUDGET,
            exceeded: false,
        };
        if let Err(error) = serde_json::to_writer(&mut writer, value) {
            return Err(if writer.exceeded {
                EncodeError::Oversized
            } else {
                EncodeError::Serialize(error.to_string())
            });
        }
        let body =
            String::from_utf8(writer.bytes).map_err(|e| EncodeError::Serialize(e.to_string()))?;
        Ok(Self { body, epoch })
    }
    /// A body this crate already serialized, such as a journaled event.
    pub(crate) fn serialized(body: String, epoch: Option<Uuid>) -> Result<Self, EncodeError> {
        if body.len() > BODY_BUDGET {
            return Err(EncodeError::Oversized);
        }
        Ok(Self { body, epoch })
    }
    pub fn body(&self) -> &str {
        &self.body
    }
}
/// Collects serialized bytes and stops the serializer as soon as the budget is spent.
struct Bounded {
    bytes: Vec<u8>,
    remaining: usize,
    exceeded: bool,
}
impl std::io::Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(remaining) = self.remaining.checked_sub(bytes.len()) else {
            self.exceeded = true;
            return Err(std::io::Error::other("Remote frame limit exceeded"));
        };
        self.remaining = remaining;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct Connection<T> {
    pub outgoing: Sender<Outgoing>,
    pub incoming: Receiver<TransportEvent<T>>,
    pub reconnect: Sender<()>,
}
#[derive(Debug)]
pub enum TransportEvent<T> {
    /// Controller attachment identity, authenticated by the relay on both endpoints.
    PeerConnected {
        epoch: Uuid,
    },
    PeerDisconnected,
    Payload(T),
    /// An authenticated message whose body is valid JSON but not a `T`.
    InvalidPayload(String),
    Disconnected(String),
}

/// Consecutive failed attempts wait twice as long each time, up to `MAX_RETRY`; an
/// authenticated attachment starts the sequence over.
struct Backoff {
    delay: Duration,
}
impl Default for Backoff {
    fn default() -> Self {
        Self { delay: FIRST_RETRY }
    }
}
impl Backoff {
    fn next(&mut self) -> Duration {
        let delay = self.delay;
        self.delay = (self.delay * 2).min(MAX_RETRY);
        delay
    }
    fn reset(&mut self) {
        self.delay = FIRST_RETRY;
    }
}

/// Authentication is captured once; neither token nor HTTP errors containing it are logged.
pub fn connect<T: DeserializeOwned + Send + 'static>(
    relay_url: &str,
    token: &str,
    role: Role,
) -> Result<Connection<T>> {
    let mut url = validate_relay_url(relay_url)?;
    ensure!(
        !token.is_empty() && !token.chars().any(char::is_control),
        "Invalid relay credential"
    );
    let expected_host = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|host| !host.is_empty()))
        .ok_or_else(|| anyhow!("Relay URL must include its configured Host ID"))?
        .to_owned();
    url.set_path(&format!(
        "{}/{}",
        url.path().trim_end_matches('/'),
        role.name()
    ));
    let mut authorization = tungstenite::http::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| anyhow!("Invalid relay credential"))?;
    authorization.set_sensitive(true);
    let (outgoing, commands) = async_channel::bounded(QUEUE);
    let (events, incoming) = async_channel::bounded(QUEUE);
    let (reconnect, resets) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("seintje-websocket".into())
        .spawn(move || {
            let mut backoff = Backoff::default();
            loop {
                if commands.is_closed() || events.is_closed() {
                    break;
                }
                // Drain before every new attachment, including attempts made while disconnected.
                while commands.try_recv().is_ok() {}
                while resets.try_recv().is_ok() {}
                let mut authenticated = false;
                let result = socket(&url, &authorization).and_then(|mut socket| {
                    connected(
                        &mut socket,
                        role,
                        &expected_host,
                        &commands,
                        &events,
                        &resets,
                        &mut authenticated,
                    )
                });
                if authenticated {
                    backoff.reset();
                }
                while commands.try_recv().is_ok() {}
                if commands.is_closed() || events.is_closed() {
                    break;
                }
                let terminal = result
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.is::<AttachmentRevoked>());
                let reason = result
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "Relay attachment closed".into());
                if events
                    .try_send(TransportEvent::Disconnected(reason))
                    .is_err()
                {
                    break;
                }
                if terminal {
                    break;
                }
                let deadline = Instant::now() + backoff.next();
                loop {
                    if commands.is_closed() || events.is_closed() {
                        return;
                    }
                    while commands.try_recv().is_ok() {}
                    let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    if left.is_zero() {
                        break;
                    }
                    std::thread::sleep(left.min(IO_TICK));
                }
            }
            events.close();
        })?;
    Ok(Connection {
        outgoing,
        incoming,
        reconnect,
    })
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;
fn tcp(socket: &Socket) -> Result<&TcpStream> {
    Ok(match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => &stream.sock,
        _ => bail!("Unsupported relay TLS transport"),
    })
}
fn socket(url: &url::Url, authorization: &tungstenite::http::HeaderValue) -> Result<Socket> {
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("Relay hostname missing"))?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("Relay port missing"))?;
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|_| anyhow!("Relay hostname resolution failed"))?;
    let mut stream = None;
    for address in addresses.take(8) {
        if let Ok(connection) = TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
            stream = Some(connection);
            break;
        }
    }
    let stream = stream.ok_or_else(|| anyhow!("Relay TCP connection failed"))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| anyhow!("Invalid relay WebSocket request"))?;
    request.headers_mut().insert(
        tungstenite::http::header::AUTHORIZATION,
        authorization.clone(),
    );
    let config = WebSocketConfig::default()
        .max_frame_size(Some(MAX_FRAME_BYTES))
        .max_message_size(Some(MAX_FRAME_BYTES))
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME_BYTES + 1024);
    // Explicit TLS handshake has no redirect path; Bearer credentials never follow redirects.
    let (socket, _) = tungstenite::client_tls_with_config(request, stream, Some(config), None)
        .map_err(|_| anyhow!("Relay WebSocket authentication or TLS handshake failed"))?;
    let tcp = tcp(&socket)?;
    tcp.set_read_timeout(Some(IO_TICK))?;
    tcp.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(socket)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Peer {
    connected: bool,
    epoch: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    frame: usize,
    queue: usize,
    idle_timeout: u64,
}
enum Envelope<B> {
    Hello {
        protocol: String,
        instance: String,
        host: String,
        role: String,
        epoch: u64,
        peer: Peer,
        limits: Limits,
    },
    PeerConnected {
        peer_epoch: u64,
    },
    PeerDisconnected {
        peer_epoch: u64,
        _reason: String,
    },
    Message {
        id: String,
        from_epoch: u64,
        to_epoch: u64,
        body: B,
    },
    Rejected {
        _id: String,
        _reason: String,
        _peer_epoch: Option<u64>,
    },
}

impl<B> Envelope<B> {
    fn map_body<C>(self, body: impl FnOnce(B) -> C) -> Envelope<C> {
        match self {
            Self::Hello {
                protocol,
                instance,
                host,
                role,
                epoch,
                peer,
                limits,
            } => Envelope::Hello {
                protocol,
                instance,
                host,
                role,
                epoch,
                peer,
                limits,
            },
            Self::PeerConnected { peer_epoch } => Envelope::PeerConnected { peer_epoch },
            Self::PeerDisconnected {
                peer_epoch,
                _reason,
            } => Envelope::PeerDisconnected {
                peer_epoch,
                _reason,
            },
            Self::Message {
                id,
                from_epoch,
                to_epoch,
                body: payload,
            } => Envelope::Message {
                id,
                from_epoch,
                to_epoch,
                body: body(payload),
            },
            Self::Rejected {
                _id,
                _reason,
                _peer_epoch,
            } => Envelope::Rejected {
                _id,
                _reason,
                _peer_epoch,
            },
        }
    }
}

/// Every field any relay envelope carries; `type` decides which must and may appear.
/// Parsed flat instead of as an internally tagged enum, so a message body deserializes
/// straight into its typed form without first being buffered as a generic tree. Each
/// field is `None` only when absent, so `null` is never mistaken for a missing field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, bound(deserialize = "B: Deserialize<'de>"))]
struct RawEnvelope<B> {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "present")]
    protocol: Option<String>,
    #[serde(default, deserialize_with = "present")]
    instance: Option<String>,
    #[serde(default, deserialize_with = "present")]
    host: Option<String>,
    #[serde(default, deserialize_with = "present")]
    role: Option<String>,
    #[serde(default, deserialize_with = "present")]
    epoch: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    peer: Option<Peer>,
    #[serde(default, deserialize_with = "present")]
    limits: Option<Limits>,
    /// `Some(None)` is an explicit `null`, which only `rejected` accepts.
    #[serde(default, deserialize_with = "present")]
    peer_epoch: Option<Option<u64>>,
    #[serde(default, deserialize_with = "present")]
    reason: Option<String>,
    #[serde(default, deserialize_with = "present")]
    id: Option<String>,
    #[serde(default, deserialize_with = "present")]
    from_epoch: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    to_epoch: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    body: Option<B>,
}
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}
impl<B> RawEnvelope<B> {
    /// The envelope this `type` describes, or `None` when a field is missing or foreign to it.
    fn into_envelope(self) -> Option<Envelope<B>> {
        let Self {
            kind,
            protocol,
            instance,
            host,
            role,
            epoch,
            peer,
            limits,
            peer_epoch,
            reason,
            id,
            from_epoch,
            to_epoch,
            body,
        } = self;
        let hello = protocol.is_some()
            || instance.is_some()
            || host.is_some()
            || role.is_some()
            || epoch.is_some()
            || peer.is_some()
            || limits.is_some();
        let routed = from_epoch.is_some() || to_epoch.is_some() || body.is_some();
        Some(match kind.as_str() {
            "hello" if peer_epoch.is_none() && reason.is_none() && id.is_none() && !routed => {
                Envelope::Hello {
                    protocol: protocol?,
                    instance: instance?,
                    host: host?,
                    role: role?,
                    epoch: epoch?,
                    peer: peer?,
                    limits: limits?,
                }
            }
            "peer_connected" if !hello && reason.is_none() && id.is_none() && !routed => {
                Envelope::PeerConnected {
                    peer_epoch: peer_epoch??,
                }
            }
            "peer_disconnected" if !hello && id.is_none() && !routed => {
                Envelope::PeerDisconnected {
                    peer_epoch: peer_epoch??,
                    _reason: reason?,
                }
            }
            "message" if !hello && peer_epoch.is_none() && reason.is_none() => Envelope::Message {
                id: id?,
                from_epoch: from_epoch?,
                to_epoch: to_epoch?,
                body: body?,
            },
            "rejected" if !hello && !routed => Envelope::Rejected {
                _id: id?,
                _reason: reason?,
                _peer_epoch: peer_epoch.flatten(),
            },
            _ => return None,
        })
    }
}
/// A message body that is valid JSON but not a `T` stays a per-message error for the
/// consumer to judge; anything else malformed is an invalid relay envelope.
fn parse_envelope<T: DeserializeOwned>(
    text: &str,
) -> Result<Envelope<std::result::Result<T, String>>> {
    let envelope = match serde_json::from_str::<RawEnvelope<T>>(text) {
        Ok(raw) => raw.into_envelope().map(|envelope| envelope.map_body(Ok)),
        // Only the body differs between the two parses, so a valid untyped message
        // means the body alone was wrong.
        Err(error) => serde_json::from_str::<RawEnvelope<IgnoredAny>>(text)
            .ok()
            .and_then(RawEnvelope::into_envelope)
            .filter(|envelope| matches!(envelope, Envelope::Message { .. }))
            .map(|envelope| envelope.map_body(|_| Err(error.to_string()))),
    };
    envelope.ok_or_else(|| anyhow!("Invalid SEINTJE relay envelope"))
}

struct Attachment {
    namespace: Option<Uuid>,
    own: Option<u64>,
    peer: Option<u64>,
    frame_limit: usize,
    heartbeat: Duration,
    lease: Duration,
}
impl Default for Attachment {
    fn default() -> Self {
        Self {
            namespace: None,
            own: None,
            peer: None,
            frame_limit: MAX_FRAME_BYTES,
            heartbeat: HEARTBEAT,
            lease: LEASE,
        }
    }
}
impl Attachment {
    fn controller_epoch(&self, role: Role) -> Result<Uuid> {
        let namespace = self
            .namespace
            .ok_or_else(|| anyhow!("Relay has not authenticated the attachment"))?;
        let epoch = match role {
            Role::Desktop => self.own,
            Role::Host => self.peer,
        }
        .ok_or_else(|| anyhow!("Remote desktop is not attached"))?;
        Ok(Uuid::new_v5(&namespace, &epoch.to_be_bytes()))
    }

    fn accept<T>(
        &mut self,
        envelope: Envelope<std::result::Result<T, String>>,
        role: Role,
        expected_host: &str,
    ) -> Result<Option<TransportEvent<T>>> {
        Ok(match envelope {
            Envelope::Hello {
                protocol,
                instance,
                host,
                role: authenticated_role,
                epoch,
                peer,
                limits,
            } => {
                ensure!(self.own.is_none(), "Duplicate relay hello frame");
                ensure!(protocol == PROTOCOL, "Unsupported SEINTJE relay protocol");
                ensure!(
                    host == expected_host && authenticated_role == role.name(),
                    "Relay authenticated an unexpected Host or role"
                );
                ensure!(
                    !instance.is_empty() && instance.len() <= 256 && epoch != 0,
                    "Invalid relay instance or epoch"
                );
                ensure!(
                    limits.frame >= 1024 && limits.queue > 0 && limits.idle_timeout >= 3,
                    "Invalid advertised relay limits"
                );
                ensure!(
                    peer.connected == peer.epoch.is_some() && peer.epoch != Some(0),
                    "Invalid relay counterpart snapshot"
                );
                let protocol_namespace = Uuid::new_v5(&Uuid::NAMESPACE_URL, PROTOCOL.as_bytes());
                self.namespace = Some(Uuid::new_v5(&protocol_namespace, instance.as_bytes()));
                self.own = Some(epoch);
                self.peer = peer.epoch;
                self.frame_limit = limits.frame.min(MAX_FRAME_BYTES);
                self.lease = Duration::from_secs(limits.idle_timeout).min(LEASE);
                self.heartbeat = (Duration::from_secs(limits.idle_timeout) / 3).min(HEARTBEAT);
                if self.peer.is_some() {
                    Some(TransportEvent::PeerConnected {
                        epoch: self.controller_epoch(role)?,
                    })
                } else {
                    None
                }
            }
            Envelope::PeerConnected { peer_epoch } => {
                ensure!(
                    self.own.is_some(),
                    "Relay counterpart arrived before authentication"
                );
                ensure!(
                    self.peer.is_none() && peer_epoch != 0,
                    "Relay counterpart replaced without disconnect"
                );
                self.peer = Some(peer_epoch);
                Some(TransportEvent::PeerConnected {
                    epoch: self.controller_epoch(role)?,
                })
            }
            Envelope::PeerDisconnected { peer_epoch, .. } => {
                ensure!(
                    self.peer == Some(peer_epoch),
                    "Stale relay disconnect identity"
                );
                self.peer = None;
                Some(TransportEvent::PeerDisconnected)
            }
            Envelope::Message {
                id,
                from_epoch,
                to_epoch,
                body,
            } => {
                ensure!(
                    !id.is_empty() && id.len() <= 128,
                    "Invalid relay message identity"
                );
                ensure!(
                    self.own == Some(to_epoch) && self.peer == Some(from_epoch),
                    "Unattached or stale relay payload"
                );
                Some(match body {
                    Ok(body) => TransportEvent::Payload(body),
                    Err(error) => TransportEvent::InvalidPayload(error),
                })
            }
            Envelope::Rejected { _reason, .. } => {
                let reason = match _reason.as_str() {
                    "superseded" => "attachment superseded",
                    "peer_offline" => "counterpart offline",
                    "stale_peer" => "stale counterpart identity",
                    "peer_busy" => "counterpart queue full",
                    _ => "unknown rejection",
                };
                bail!("Relay rejected a frame ({reason}); outcome was not replayed")
            }
        })
    }

    /// The `send` envelope around an already serialized body, without parsing it again.
    fn frame(&self, payload: &Outgoing) -> Result<String> {
        let peer = self
            .peer
            .ok_or_else(|| anyhow!("Remote counterpart is not attached; command discarded"))?;
        let frame = format!(
            r#"{{"type":"send","id":"{}","to_epoch":{peer},"body":{}}}"#,
            Uuid::new_v4(),
            payload.body
        );
        // Forwarded message envelopes add from_epoch and a longer type discriminator.
        ensure!(
            frame.len() <= self.frame_limit.saturating_sub(128),
            "Relay frame exceeds its advertised size limit"
        );
        Ok(frame)
    }
}

/// Wait briefly for a busy consumer instead of failing on the first full queue; a
/// consumer that stays stuck still closes the attachment rather than losing events.
fn deliver<T>(events: &Sender<TransportEvent<T>>, event: TransportEvent<T>) -> Result<()> {
    let event = match events.try_send(event) {
        Ok(()) => return Ok(()),
        Err(async_channel::TrySendError::Full(event)) => event,
        Err(async_channel::TrySendError::Closed(_)) => bail!("Relay receive queue closed"),
    };
    let delivered = smol::block_on(smol::future::or(
        async { events.send(event).await.is_ok() },
        async {
            smol::Timer::after(RECEIVE_WAIT).await;
            false
        },
    ));
    ensure!(
        delivered,
        "Relay receive queue exhausted; attachment closed without dropping observations"
    );
    Ok(())
}

/// Write every queued payload, up to `PASS_BYTES`, and return how many frames went out.
fn drain(
    socket: &mut Socket,
    role: Role,
    attachment: &Attachment,
    commands: &Receiver<Outgoing>,
) -> Result<usize> {
    let (mut sent, mut written) = (0, 0);
    while written < PASS_BYTES {
        let Ok(payload) = commands.try_recv() else {
            break;
        };
        if role == Role::Desktop && payload.epoch != attachment.controller_epoch(role).ok() {
            // A producer can race the disconnect notification. Old requests must not
            // acquire the destination identity of a new attachment.
            continue;
        }
        let frame = attachment.frame(&payload)?;
        written += frame.len();
        socket.send(Message::Text(frame.into())).map_err(|_| {
            anyhow!("Relay write failed; outcome uncertain and frame was not replayed")
        })?;
        sent += 1;
    }
    Ok(sent)
}

/// Short reads while commands wait or traffic just flowed; back to `IO_TICK` when idle.
fn read_timeout(queued: bool, idle_passes: u32) -> Duration {
    if queued {
        return ACTIVE_TICK;
    }
    ACTIVE_TICK
        .saturating_mul(1 << idle_passes.min(16))
        .min(IO_TICK)
}

fn connected<T: DeserializeOwned>(
    socket: &mut Socket,
    role: Role,
    expected_host: &str,
    commands: &Receiver<Outgoing>,
    events: &Sender<TransportEvent<T>>,
    resets: &Receiver<()>,
    authenticated: &mut bool,
) -> Result<()> {
    let mut attachment = Attachment::default();
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    let ready_since = Instant::now();
    let mut timeout = IO_TICK;
    let mut idle_passes = u32::MAX;
    let mut received = false;
    loop {
        if commands.is_closed() || events.is_closed() {
            let _ = socket.close(None);
            return Ok(());
        }
        ensure!(
            resets.try_recv().is_err(),
            "Relay attachment reset requested"
        );
        ensure!(
            last_rx.elapsed() < attachment.lease,
            "Relay heartbeat expired"
        );
        ensure!(
            attachment.own.is_some() || ready_since.elapsed() < LEASE,
            "Relay did not authenticate the attachment"
        );
        if last_ping.elapsed() >= attachment.heartbeat {
            socket
                .send(Message::Ping(Vec::new().into()))
                .map_err(|_| anyhow!("Relay heartbeat write failed"))?;
            last_ping = Instant::now();
        }
        let sent = drain(socket, role, &attachment, commands)?;
        idle_passes = if sent > 0 || received {
            0
        } else {
            idle_passes.saturating_add(1)
        };
        let wanted = read_timeout(!commands.is_empty(), idle_passes);
        if wanted != timeout {
            tcp(socket)?.set_read_timeout(Some(wanted))?;
            timeout = wanted;
        }
        received = false;
        match socket.read() {
            Ok(message) => {
                last_rx = Instant::now();
                received = true;
                match message {
                    Message::Text(text) => {
                        ensure!(
                            text.len() <= attachment.frame_limit,
                            "Relay frame exceeds its advertised size limit"
                        );
                        let envelope = parse_envelope::<T>(text.as_str())?;
                        let disconnected = matches!(envelope, Envelope::PeerDisconnected { .. });
                        let hello = matches!(envelope, Envelope::Hello { .. });
                        let event = attachment.accept(envelope, role, expected_host)?;
                        if hello {
                            *authenticated = true;
                            socket.set_config(|config| {
                                config.max_frame_size = Some(attachment.frame_limit);
                                config.max_message_size = Some(attachment.frame_limit);
                            });
                        }
                        if disconnected {
                            while commands.try_recv().is_ok() {}
                        }
                        if let Some(event) = event {
                            deliver(events, event)?;
                        }
                        if disconnected {
                            bail!("Remote counterpart detached; rotating the attachment identity");
                        }
                    }
                    Message::Ping(_) | Message::Pong(_) => {
                        socket
                            .flush()
                            .map_err(|_| anyhow!("Relay heartbeat flush failed"))?;
                    }
                    Message::Close(Some(frame)) if matches!(u16::from(frame.code), 4001 | 4401) => {
                        return Err(AttachmentRevoked("Relay attachment was superseded or its credential revoked; explicit Reconnect is required").into());
                    }
                    Message::Close(_) => bail!("Relay closed the attachment"),
                    Message::Binary(_) | Message::Frame(_) => {
                        bail!("Unsupported binary relay frame")
                    }
                }
            }
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => bail!("Relay read failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    type Parsed = Envelope<Result<Value, String>>;

    fn hello(
        role: Role,
        instance: &str,
        own: u64,
        peer: Option<u64>,
    ) -> Envelope<Result<Value, String>> {
        Envelope::Hello {
            protocol: PROTOCOL.into(),
            instance: instance.into(),
            host: "worker".into(),
            role: role.name().into(),
            epoch: own,
            peer: Peer {
                connected: peer.is_some(),
                epoch: peer,
            },
            limits: Limits {
                frame: MAX_FRAME_BYTES,
                queue: 32,
                idle_timeout: 60,
            },
        }
    }
    fn message(from_epoch: u64, to_epoch: u64) -> Envelope<Result<Value, String>> {
        Envelope::Message {
            id: Uuid::new_v4().to_string(),
            from_epoch,
            to_epoch,
            body: Ok(Value::Null),
        }
    }
    #[test]
    fn payload_requires_authenticated_live_counterpart_and_destination() {
        let mut attachment = Attachment::default();
        assert!(
            attachment
                .accept(message(2, 1), Role::Desktop, "worker")
                .is_err()
        );
        attachment
            .accept(
                hello(Role::Desktop, "server", 1, None),
                Role::Desktop,
                "worker",
            )
            .unwrap();
        assert!(matches!(
            attachment
                .accept(
                    Parsed::PeerConnected { peer_epoch: 2 },
                    Role::Desktop,
                    "worker"
                )
                .unwrap(),
            Some(TransportEvent::PeerConnected { .. })
        ));
        assert!(
            attachment
                .accept(message(3, 1), Role::Desktop, "worker")
                .is_err()
        );
        assert!(
            attachment
                .accept(message(2, 3), Role::Desktop, "worker")
                .is_err()
        );
        assert!(matches!(
            attachment
                .accept(message(2, 1), Role::Desktop, "worker")
                .unwrap(),
            Some(TransportEvent::Payload(Value::Null))
        ));
        attachment
            .accept(
                Parsed::PeerDisconnected {
                    peer_epoch: 2,
                    _reason: "closed".into(),
                },
                Role::Desktop,
                "worker",
            )
            .unwrap();
        assert!(
            attachment
                .accept(message(2, 1), Role::Desktop, "worker")
                .is_err()
        );
    }
    #[test]
    fn host_and_desktop_agree_on_controller_epoch_and_restart_changes_it() {
        let mut desktop = Attachment::default();
        let mut host = Attachment::default();
        desktop
            .accept(
                hello(Role::Desktop, "server", 1, Some(2)),
                Role::Desktop,
                "worker",
            )
            .unwrap();
        host.accept(
            hello(Role::Host, "server", 2, Some(1)),
            Role::Host,
            "worker",
        )
        .unwrap();
        assert_eq!(
            desktop.controller_epoch(Role::Desktop).unwrap(),
            host.controller_epoch(Role::Host).unwrap()
        );
        let mut restarted = Attachment::default();
        restarted
            .accept(
                hello(Role::Host, "restarted", 2, Some(1)),
                Role::Host,
                "worker",
            )
            .unwrap();
        assert_ne!(
            desktop.controller_epoch(Role::Desktop).unwrap(),
            restarted.controller_epoch(Role::Host).unwrap()
        );
    }
    #[test]
    fn hello_requires_protocol_identity_and_advertised_limits() {
        let mut attachment = Attachment::default();
        assert!(
            attachment
                .accept(
                    hello(Role::Host, "server", 1, None),
                    Role::Desktop,
                    "worker"
                )
                .is_err()
        );
        assert!(
            attachment
                .accept(
                    hello(Role::Desktop, "server", 1, None),
                    Role::Desktop,
                    "other-host"
                )
                .is_err()
        );
        let missing_limits = json!({"type":"hello","protocol":PROTOCOL,"instance":"server","host":"worker","role":"desktop","epoch":1,"peer":{"connected":false}});
        assert!(parse_envelope::<Value>(&missing_limits.to_string()).is_err());
        let mut wrong_protocol = hello(Role::Desktop, "server", 1, None);
        if let Envelope::Hello { protocol, .. } = &mut wrong_protocol {
            *protocol = "other/1".into();
        }
        assert!(
            attachment
                .accept(wrong_protocol, Role::Desktop, "worker")
                .is_err()
        );
    }
    #[test]
    fn rejected_frames_are_terminal_and_advertised_frame_limit_is_enforced() {
        let mut attachment = Attachment::default();
        let mut handshake = hello(Role::Desktop, "server", 1, Some(2));
        if let Envelope::Hello { limits, .. } = &mut handshake {
            limits.frame = 1024;
        }
        attachment
            .accept(handshake, Role::Desktop, "worker")
            .unwrap();
        let once = Outgoing::encode(&json!({"command":"once"}), None).unwrap();
        let small: Value = serde_json::from_str(&attachment.frame(&once).unwrap()).unwrap();
        assert_eq!(small["type"], "send");
        assert_eq!(small["to_epoch"], 2);
        assert_eq!(small["body"], json!({"command":"once"}));
        assert!(Uuid::parse_str(small["id"].as_str().unwrap()).is_ok());
        let large = Outgoing::encode(&Value::String("x".repeat(1024)), None).unwrap();
        assert!(attachment.frame(&large).is_err());
        assert!(
            attachment
                .accept(
                    Parsed::Rejected {
                        _id: Uuid::new_v4().to_string(),
                        _reason: "peer_busy".into(),
                        _peer_epoch: Some(2)
                    },
                    Role::Desktop,
                    "worker"
                )
                .is_err()
        );
    }

    #[test]
    fn bodies_are_framed_verbatim_and_oversized_bodies_stop_at_the_budget() {
        let body = Outgoing::encode(&json!({"text":"é\n\"quoted\""}), None).unwrap();
        let mut attachment = Attachment::default();
        attachment
            .accept(
                hello(Role::Host, "server", 1, Some(2)),
                Role::Host,
                "worker",
            )
            .unwrap();
        let frame = attachment.frame(&body).unwrap();
        assert!(frame.ends_with(&format!(r#""body":{}}}"#, body.body())));
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["body"]["text"], "é\n\"quoted\"");
        assert!(matches!(
            Outgoing::encode(&"x".repeat(BODY_BUDGET), None),
            Err(EncodeError::Oversized)
        ));
        assert!(Outgoing::encode(&"x".repeat(BODY_BUDGET - 2), None).is_ok());
        assert!(matches!(
            Outgoing::serialized("x".repeat(BODY_BUDGET + 1), None),
            Err(EncodeError::Oversized)
        ));
    }

    #[test]
    fn body_budget_counts_escaped_bytes() {
        let mut budget = Bounded {
            bytes: Vec::new(),
            remaining: 5,
            exceeded: false,
        };
        assert!(serde_json::to_writer(&mut budget, &"\n\n").is_err());
        assert!(budget.exceeded);
        let mut budget = Bounded {
            bytes: Vec::new(),
            remaining: 6,
            exceeded: false,
        };
        serde_json::to_writer(&mut budget, &"\n\n").unwrap();
        assert_eq!(
            (budget.remaining, budget.bytes.as_slice()),
            (0, &br#""\n\n""#[..])
        );
    }

    #[test]
    fn envelopes_keep_per_type_field_rules_and_type_message_bodies_directly() {
        #[derive(Debug, Deserialize, PartialEq)]
        #[serde(deny_unknown_fields)]
        struct Body {
            n: u32,
        }
        let message = |body: Value| {
            json!({"type":"message","id":"m","from_epoch":2,"to_epoch":1,"body":body}).to_string()
        };
        assert!(matches!(
            parse_envelope::<Body>(&message(json!({"n":7}))).unwrap(),
            Envelope::Message {
                body: Ok(Body { n: 7 }),
                ..
            }
        ));
        // A well-formed message with the wrong body is the consumer's protocol error.
        assert!(matches!(
            parse_envelope::<Body>(&message(json!({"n":"seven"}))).unwrap(),
            Envelope::Message { body: Err(_), .. }
        ));
        assert!(matches!(
            parse_envelope::<Body>(&message(Value::Null)).unwrap(),
            Envelope::Message { body: Err(_), .. }
        ));
        let invalid = [
            json!({"type":"message","id":"m","from_epoch":2,"to_epoch":1}),
            json!({"type":"message","id":"m","from_epoch":2,"to_epoch":1,"body":{"n":1},"reason":"x"}),
            json!({"type":"message","id":"m","from_epoch":2,"to_epoch":1,"body":{"n":1},"other":1}),
            json!({"type":"message","id":null,"from_epoch":2,"to_epoch":1,"body":{"n":1}}),
            json!({"type":"peer_connected","peer_epoch":2,"id":"m"}),
            json!({"type":"peer_connected","peer_epoch":null}),
            json!({"type":"peer_disconnected","peer_epoch":2}),
            json!({"type":"rejected","id":"m","reason":"peer_busy","body":{"n":1}}),
            json!({"type":"rejected","id":"m","reason":"peer_busy","epoch":1}),
            json!({"type":"unknown"}),
            json!({"id":"m"}),
        ];
        for envelope in invalid {
            assert!(
                parse_envelope::<Body>(&envelope.to_string()).is_err(),
                "{envelope}"
            );
        }
        for envelope in [
            json!({"type":"rejected","id":"m","reason":"peer_busy"}),
            json!({"type":"rejected","id":"m","reason":"peer_busy","peer_epoch":null}),
            json!({"type":"rejected","id":"m","reason":"peer_busy","peer_epoch":2}),
        ] {
            assert!(matches!(
                parse_envelope::<Body>(&envelope.to_string()).unwrap(),
                Envelope::Rejected { .. }
            ));
        }
        let hello = json!({"type":"hello","protocol":PROTOCOL,"instance":"server","host":"worker",
            "role":"desktop","epoch":1,"peer":{"connected":false},
            "limits":{"frame":1024,"queue":1,"idle_timeout":3}});
        assert!(matches!(
            parse_envelope::<Body>(&hello.to_string()).unwrap(),
            Envelope::Hello { .. }
        ));
        let mut foreign = hello;
        foreign["to_epoch"] = json!(1);
        assert!(parse_envelope::<Body>(&foreign.to_string()).is_err());
    }

    #[test]
    fn reconnect_backoff_doubles_to_a_minute_and_restarts_after_authentication() {
        let mut backoff = Backoff::default();
        let delays: Vec<u64> = (0..9).map(|_| backoff.next().as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        backoff.reset();
        assert_eq!(backoff.next(), FIRST_RETRY);
    }

    #[test]
    fn reads_stay_short_while_traffic_flows_and_relax_to_the_idle_tick() {
        assert_eq!(read_timeout(true, u32::MAX), ACTIVE_TICK);
        assert_eq!(read_timeout(false, 0), ACTIVE_TICK);
        assert_eq!(read_timeout(false, 3), ACTIVE_TICK * 8);
        assert_eq!(read_timeout(false, 7), IO_TICK);
        assert_eq!(read_timeout(false, u32::MAX), IO_TICK);
    }

    fn receive<T>(incoming: &Receiver<TransportEvent<T>>) -> TransportEvent<T> {
        smol::block_on(smol::future::or(
            async { incoming.recv().await.expect("transport stopped") },
            async {
                smol::Timer::after(Duration::from_secs(10)).await;
                panic!("no transport event within 10 s")
            },
        ))
    }

    /// A loopback relay that authenticates one host and one desktop, then forwards
    /// every host `send` to the desktop as a `message`.
    #[expect(
        clippy::result_large_err,
        reason = "tungstenite's handshake callback returns its own HTTP error response"
    )]
    fn relay() -> String {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "ws://127.0.0.1:{}/v1/relay/worker",
            listener.local_addr().unwrap().port()
        );
        std::thread::spawn(move || {
            let mut sockets = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let mut path = String::new();
                let socket = tungstenite::accept_hdr(
                    stream,
                    |request: &tungstenite::handshake::server::Request, response| {
                        path = request.uri().path().to_owned();
                        Ok(response)
                    },
                )
                .unwrap();
                sockets.push((path, socket));
            }
            sockets.sort_by(|a, b| a.0.cmp(&b.0));
            let (_, mut host) = sockets.pop().unwrap();
            let (_, mut desktop) = sockets.pop().unwrap();
            for (socket, role, own, peer) in
                [(&mut desktop, "desktop", 2, 1), (&mut host, "host", 1, 2)]
            {
                let hello = json!({"type":"hello","protocol":PROTOCOL,"instance":"loopback",
                    "host":"worker","role":role,"epoch":own,"peer":{"connected":true,"epoch":peer},
                    "limits":{"frame":MAX_FRAME_BYTES,"queue":64,"idle_timeout":60}});
                socket
                    .send(Message::Text(hello.to_string().into()))
                    .unwrap();
            }
            loop {
                match host.read() {
                    Ok(Message::Text(text)) => {
                        let send: Value = serde_json::from_str(text.as_str()).unwrap();
                        assert_eq!(
                            (send["type"].as_str(), send["to_epoch"].as_u64()),
                            (Some("send"), Some(2))
                        );
                        let message = json!({"type":"message","id":send["id"],"from_epoch":1,
                            "to_epoch":2,"body":send["body"]});
                        if desktop
                            .send(Message::Text(message.to_string().into()))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(_) => {
                        let _ = host.flush();
                    }
                    Err(_) => return,
                }
            }
        });
        url
    }

    #[test]
    fn queued_frames_reach_the_counterpart_in_order_without_a_per_pass_cap() {
        let url = relay();
        let host = connect::<Value>(&url, "host-token", Role::Host).unwrap();
        let desktop = connect::<Value>(&url, "desktop-token", Role::Desktop).unwrap();
        assert!(matches!(
            receive(&host.incoming),
            TransportEvent::PeerConnected { .. }
        ));
        assert!(matches!(
            receive(&desktop.incoming),
            TransportEvent::PeerConnected { .. }
        ));
        const FRAMES: u64 = 2000;
        let started = Instant::now();
        let outgoing = host.outgoing.clone();
        let producer = std::thread::spawn(move || {
            for n in 0..FRAMES {
                let body = Outgoing::encode(&json!({"n": n}), None).unwrap();
                outgoing.send_blocking(body).unwrap();
            }
        });
        for expected in 0..FRAMES {
            match receive(&desktop.incoming) {
                TransportEvent::Payload(body) => assert_eq!(body["n"], expected),
                other => panic!("unexpected transport event {other:?}"),
            }
        }
        producer.join().unwrap();
        // Eight frames per 100 ms read tick needed 25 s for this burst.
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    }
}
