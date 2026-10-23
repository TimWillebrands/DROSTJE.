//! SEINTJE framing is isolated here. Queued mutations never survive a lost attachment.
use std::{
    io::ErrorKind,
    net::{TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail, ensure};
use async_channel::{Receiver, Sender};
use serde::Deserialize;
use serde_json::{Value, json};
use tungstenite::{
    Message, WebSocket, client::IntoClientRequest, protocol::WebSocketConfig,
    stream::MaybeTlsStream,
};
use uuid::Uuid;

use super::{config::validate_relay_url, wire::MAX_FRAME_BYTES};

const QUEUE: usize = 32;
const IO_TICK: Duration = Duration::from_millis(100);
const HEARTBEAT: Duration = Duration::from_secs(5);
const LEASE: Duration = Duration::from_secs(15);
const PROTOCOL: &str = "seintje-relay/1";

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
pub struct Connection {
    pub outgoing: Sender<Value>,
    pub incoming: Receiver<TransportEvent>,
    pub reconnect: Sender<()>,
}
#[derive(Debug)]
pub enum TransportEvent {
    /// Controller attachment identity, authenticated by the relay on both endpoints.
    PeerConnected {
        epoch: Uuid,
    },
    PeerDisconnected,
    Payload(Value),
    Disconnected(String),
}

/// Authentication is captured once; neither token nor HTTP errors containing it are logged.
pub fn connect(relay_url: &str, token: &str, role: Role) -> Result<Connection> {
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
            loop {
                if commands.is_closed() || events.is_closed() {
                    break;
                }
                // Drain before every new attachment, including attempts made while disconnected.
                while commands.try_recv().is_ok() {}
                while resets.try_recv().is_ok() {}
                let result = socket(&url, &authorization).and_then(|mut socket| {
                    connected(
                        &mut socket,
                        role,
                        &expected_host,
                        &commands,
                        &events,
                        &resets,
                    )
                });
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
                for _ in 0..10 {
                    if commands.is_closed() || events.is_closed() {
                        return;
                    }
                    while commands.try_recv().is_ok() {}
                    std::thread::sleep(IO_TICK);
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
    let tcp = match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => &stream.sock,
        _ => bail!("Unsupported relay TLS transport"),
    };
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
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Envelope {
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
        #[serde(rename = "reason")]
        _reason: String,
    },
    Message {
        id: String,
        from_epoch: u64,
        to_epoch: u64,
        body: Value,
    },
    Rejected {
        #[serde(rename = "id")]
        _id: String,
        #[serde(rename = "reason")]
        _reason: String,
        #[serde(rename = "peer_epoch")]
        _peer_epoch: Option<u64>,
    },
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

    fn accept(
        &mut self,
        envelope: Envelope,
        role: Role,
        expected_host: &str,
    ) -> Result<Option<TransportEvent>> {
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
                Some(TransportEvent::Payload(body))
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

    fn frame(&self, payload: Value) -> Result<String> {
        let peer = self
            .peer
            .ok_or_else(|| anyhow!("Remote counterpart is not attached; command discarded"))?;
        let frame = serde_json::to_string(
            &json!({"type":"send", "id":Uuid::new_v4().to_string(), "to_epoch":peer, "body":payload}),
        )?;
        // Forwarded message envelopes add from_epoch and a longer type discriminator.
        ensure!(
            frame.len() <= self.frame_limit.saturating_sub(128),
            "Relay frame exceeds its advertised size limit"
        );
        Ok(frame)
    }
}

fn connected(
    socket: &mut Socket,
    role: Role,
    expected_host: &str,
    commands: &Receiver<Value>,
    events: &Sender<TransportEvent>,
    resets: &Receiver<()>,
) -> Result<()> {
    let mut attachment = Attachment::default();
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    let ready_since = Instant::now();
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
        // Process at most a bounded burst before checking liveness and incoming traffic.
        for _ in 0..8 {
            let Ok(payload) = commands.try_recv() else {
                break;
            };
            if role == Role::Desktop
                && payload
                    .get("epoch")
                    .and_then(Value::as_str)
                    .and_then(|value| Uuid::parse_str(value).ok())
                    != attachment.controller_epoch(role).ok()
            {
                // A producer can race the disconnect notification. Old requests must not
                // acquire the destination identity of a new attachment.
                continue;
            }
            let frame = attachment.frame(payload)?;
            socket.send(Message::Text(frame.into())).map_err(|_| {
                anyhow!("Relay write failed; outcome uncertain and frame was not replayed")
            })?;
        }
        match socket.read() {
            Ok(message) => {
                last_rx = Instant::now();
                match message {
                    Message::Text(text) => {
                        ensure!(
                            text.len() <= attachment.frame_limit,
                            "Relay frame exceeds its advertised size limit"
                        );
                        let envelope: Envelope = serde_json::from_str(text.as_str())
                            .map_err(|_| anyhow!("Invalid SEINTJE relay envelope"))?;
                        let disconnected = matches!(envelope, Envelope::PeerDisconnected { .. });
                        let hello = matches!(envelope, Envelope::Hello { .. });
                        let event = attachment.accept(envelope, role, expected_host)?;
                        if hello {
                            socket.set_config(|config| {
                                config.max_frame_size = Some(attachment.frame_limit);
                                config.max_message_size = Some(attachment.frame_limit);
                            });
                        }
                        if disconnected {
                            while commands.try_recv().is_ok() {}
                        }
                        if let Some(event) = event {
                            events.try_send(event).map_err(|_| anyhow!("Relay receive queue exhausted; attachment closed without dropping observations"))?;
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

    fn hello(role: Role, instance: &str, own: u64, peer: Option<u64>) -> Envelope {
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
    fn message(from_epoch: u64, to_epoch: u64) -> Envelope {
        Envelope::Message {
            id: Uuid::new_v4().to_string(),
            from_epoch,
            to_epoch,
            body: Value::Null,
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
                    Envelope::PeerConnected { peer_epoch: 2 },
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
                Envelope::PeerDisconnected {
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
        assert!(serde_json::from_value::<Envelope>(missing_limits).is_err());
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
        let small: Value =
            serde_json::from_str(&attachment.frame(json!({"command":"once"})).unwrap()).unwrap();
        assert_eq!(small["type"], "send");
        assert_eq!(small["to_epoch"], 2);
        assert!(attachment.frame(Value::String("x".repeat(1024))).is_err());
        assert!(
            attachment
                .accept(
                    Envelope::Rejected {
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
}
