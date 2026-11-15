use super::*;
use std::{
    mem,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
};

mod status;

const REGISTRY_LIMIT: u64 = 65_536;
const CONNECT_DEADLINE: Duration = Duration::from_secs(5);
const RECORD_LIMIT: usize = 128;
const PROBE_INTERVAL: Duration = Duration::from_millis(500);
const READY_PROBE_INTERVAL: Duration = Duration::from_secs(5);

pub(super) struct Update {
    pub attempt: Uuid,
    pub outcome: PeerDeliveryOutcome,
}

struct WriteFrame {
    stream: UnixStream,
    bytes: Vec<u8>,
    offset: usize,
    deadline: Instant,
}

struct Record {
    delivery: PeerDelivery,
    write: Option<WriteFrame>,
    written: bool,
    reported: bool,
    accepted: bool,
    shown: bool,
    held: bool,
    started: bool,
    settled: bool,
    terminal: bool,
}

pub(super) struct Inbox {
    registry: Option<PathBuf>,
    pid: u32,
    session: Uuid,
    pub ready: bool,
    probe_at: Instant,
    records: HashMap<Uuid, Record>,
    order: VecDeque<Uuid>,
    status: Option<status::StatusInbox>,
}

impl Inbox {
    pub fn new(pid: u32, session: Uuid) -> Self {
        let directory = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")));
        Self {
            registry: directory.map(|dir| dir.join("sessions").join(format!("{pid}.json"))),
            pid,
            session,
            ready: false,
            probe_at: Instant::now(),
            records: HashMap::new(),
            order: VecDeque::new(),
            status: None,
        }
    }

    fn discover(&self) -> Result<PathBuf> {
        registry_socket(
            self.registry
                .as_deref()
                .context("Claude config directory is unavailable")?,
            self.pid,
            self.session,
        )
    }

    fn status_address(&mut self, target: &Path) -> Result<&str> {
        if let Some(status) = &self.status {
            ensure!(
                status.target == target,
                "Claude native peer socket changed during this runtime"
            );
        } else {
            self.status = Some(status::StatusInbox::bind(target, self.pid, self.session)?);
        }
        Ok(self
            .status
            .as_ref()
            .context("Claude status listener was not initialized")?
            .address())
    }

    pub fn status_frames(&mut self) -> Vec<Value> {
        self.status
            .as_mut()
            .map_or_else(Vec::new, status::StatusInbox::tick)
    }

    pub fn blocked(&self) -> bool {
        self.records
            .values()
            .any(|record| record.held && !record.accepted && !record.terminal)
    }

    pub fn busy(&self) -> bool {
        self.records.values().any(|record| {
            record
                .write
                .as_ref()
                .is_some_and(|write| write.offset < write.bytes.len())
        }) || self.status.as_ref().is_some_and(status::StatusInbox::busy)
    }

    pub fn probe(&mut self) -> bool {
        if Instant::now() < self.probe_at {
            return false;
        }
        let ready = self
            .discover()
            .and_then(|path| {
                self.status_address(&path)?;
                connect(&path)
            })
            .and_then(|stream| {
                ensure!(connected(&stream)?, "Claude peer inbox is still connecting");
                ensure!(
                    status::peer_pid(&stream)? == self.pid,
                    "Claude native peer socket PID mismatch"
                );
                Ok(())
            })
            .is_ok();
        self.probe_at = Instant::now()
            + if ready {
                READY_PROBE_INTERVAL
            } else {
                PROBE_INTERVAL
            };
        let changed = self.ready != ready;
        self.ready = ready;
        changed
    }

    pub fn send(&mut self, delivery: PeerDelivery) -> Result<()> {
        ensure!(
            !delivery.cancelled.is_closed() && delivery.cancelled.is_empty(),
            "Peer delivery was cancelled before dispatch"
        );
        ensure!(
            !delivery.messages.is_empty(),
            "Peer delivery contains no messages"
        );
        ensure!(
            !self.records.contains_key(&delivery.attempt),
            "Peer delivery attempt was already dispatched"
        );
        while self.records.len() >= RECORD_LIMIT {
            let Some(index) = self.order.iter().position(|id| {
                self.records
                    .get(id)
                    .is_some_and(|r| r.terminal || (r.accepted && r.shown && r.settled))
            }) else {
                bail!("Claude peer admission tracking is full");
            };
            let id = self
                .order
                .remove(index)
                .context("Claude peer admission tracking order changed")?;
            self.records.remove(&id);
        }
        let body = super::super::peer_context(&delivery.messages)?;
        let path = self.discover()?;
        let from = self.status_address(&path)?.to_owned();
        let name = match delivery.messages.first().map(|message| message.origin) {
            Some(ContextOrigin::Artifact) => "DROSTJE artifact",
            Some(ContextOrigin::Peer) | None => "Space network",
        };
        let content = format!(
            "<cross-session-message from=\"{from}\" from-name=\"{name}\">\n{body}\n</cross-session-message>"
        );
        let mut bytes = serde_json::to_vec(&json!({
            "msgV":1, "msg_id":delivery.attempt, "type":"user",
            "message":{"role":"user","content":content}, "priority":"next",
            "session_id":self.session, "uuid":delivery.attempt,
            "from":from,
        }))?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= FRAME, "Claude peer frame exceeds 1 MiB");
        let stream = connect(&path)?;
        ensure!(
            !delivery.cancelled.is_closed() && delivery.cancelled.is_empty(),
            "Peer delivery was cancelled before dispatch"
        );
        let attempt = delivery.attempt;
        self.records.insert(
            attempt,
            Record {
                delivery,
                write: Some(WriteFrame {
                    stream,
                    bytes,
                    offset: 0,
                    deadline: Instant::now() + CONNECT_DEADLINE,
                }),
                written: false,
                reported: false,
                accepted: false,
                shown: false,
                held: false,
                started: false,
                settled: false,
                terminal: false,
            },
        );
        self.order.push_back(attempt);
        Ok(())
    }

    pub fn tick(&mut self) -> Vec<Update> {
        let mut updates = Vec::new();
        for (&attempt, record) in &mut self.records {
            if record.accepted || record.terminal {
                continue;
            }
            let cancelled =
                record.delivery.cancelled.is_closed() || !record.delivery.cancelled.is_empty();
            let mut failure = None;
            if cancelled {
                failure = Some("Peer delivery authorization was revoked".to_owned());
            } else if let Some(write) = record
                .write
                .as_mut()
                .filter(|write| write.offset < write.bytes.len())
            {
                if Instant::now() >= write.deadline {
                    failure = Some(
                        "Claude peer socket did not accept the complete frame within 5 seconds"
                            .into(),
                    );
                } else {
                    match connected(&write.stream) {
                        Ok(false) => {}
                        Err(error) => failure = Some(format!("Claude peer socket: {error:#}")),
                        Ok(true) => match status::peer_pid(&write.stream).and_then(|pid| {
                            ensure!(pid == self.pid, "Claude native peer socket PID mismatch");
                            Ok(write.stream.write(&write.bytes[write.offset..])?)
                        }) {
                            Ok(0) => failure = Some("Claude peer socket closed".into()),
                            Ok(count) => {
                                write.offset += count;
                                if write.offset == write.bytes.len() {
                                    record.written = true;
                                }
                            }
                            Err(error)
                                if error.downcast_ref::<std::io::Error>().is_some_and(
                                    |error| {
                                        matches!(
                                            error.kind(),
                                            std::io::ErrorKind::WouldBlock
                                                | std::io::ErrorKind::Interrupted
                                        )
                                    },
                                ) => {}
                            Err(error) => failure = Some(format!("Claude peer socket: {error:#}")),
                        },
                    }
                }
            }
            if let Some(reason) = failure {
                let uncertain =
                    record.written || record.write.as_ref().is_some_and(|write| write.offset != 0);
                record.write = None;
                record.terminal = !uncertain;
                if !record.reported {
                    record.reported = true;
                    updates.push(Update {
                        attempt,
                        outcome: if uncertain {
                            PeerDeliveryOutcome::Unknown(reason)
                        } else {
                            PeerDeliveryOutcome::Rejected(reason)
                        },
                    });
                }
            }
        }
        updates
    }

    pub fn observe(&mut self, frame: &Value) -> (Vec<Update>, Vec<(Uuid, Vec<PeerMessage>)>) {
        let mut updates = Vec::new();
        let mut messages = Vec::new();
        let kind = frame["type"].as_str().unwrap_or("");
        let is_peer = frame["origin"]["kind"] == "peer";
        if is_peer
            && frame["origin"]["verifiedPeerPid"].as_u64() != Some(u64::from(std::process::id()))
        {
            return (updates, messages);
        }
        let hold = kind == "system"
            && frame["subtype"] == "peer_message_hold"
            && frame["lane"] == "socket";
        let lifecycle = kind == "command_lifecycle";
        let receipt = kind == "control" && frame["action"] == "peer_message_status";
        let ids = correlated_ids(frame);
        for id in ids {
            let Some(record) = self.records.get_mut(&id) else {
                continue;
            };
            if !record.written || record.terminal {
                continue;
            }
            let status = if receipt {
                frame["status"].as_str()
            } else if hold || lifecycle {
                frame["state"].as_str()
            } else {
                None
            };
            let accepted = matches!(
                status,
                Some("queued" | "started" | "released" | "delivered")
            ) || (is_peer && matches!(kind, "user" | "assistant" | "result"))
                || (matches!(kind, "assistant" | "result")
                    && frame["user_message_uuids"].is_array());
            if status == Some("started") || is_context(frame) {
                record.started = true;
            }
            if kind == "result" || (lifecycle && matches!(status, Some("completed" | "cancelled")))
            {
                record.settled = true;
            }
            if status.is_some() || accepted {
                record.write = None;
            }
            if accepted && !record.accepted {
                record.accepted = true;
                record.held = false;
                record.reported = true;
                updates.push(Update {
                    attempt: id,
                    outcome: PeerDeliveryOutcome::Accepted,
                });
            } else if !record.accepted {
                if status == Some("held") {
                    record.held = true;
                }
                let outcome = match status {
                    Some("held") if !record.reported => {
                        Some(PeerDeliveryOutcome::Unknown(format!(
                            "Claude native inbound policy held this peer message ({})",
                            frame["reason"]
                                .as_str()
                                .or_else(|| frame["cause"].as_str())
                                .unwrap_or("approval required")
                        )))
                    }
                    Some(status @ ("dropped" | "refused" | "discarded" | "denied" | "expired")) => {
                        Some(PeerDeliveryOutcome::Rejected(format!(
                            "Claude native inbound policy did not admit this peer message ({}). Review crossSessionInbound in the native session; if its inbox is unavailable, reconnect the runtime before resuming Space delivery. Space never changes native policy.",
                            frame["reason"]
                                .as_str()
                                .or_else(|| frame["outcome"].as_str())
                                .or_else(|| frame["status_detail"].as_str())
                                .unwrap_or(status)
                        )))
                    }
                    Some("cancelled") if !record.reported => Some(PeerDeliveryOutcome::Unknown(
                        "Claude cancelled the native peer command".into(),
                    )),
                    _ => None,
                };
                if let Some(outcome) = outcome {
                    record.terminal = matches!(&outcome, PeerDeliveryOutcome::Rejected(_));
                    record.reported = true;
                    updates.push(Update {
                        attempt: id,
                        outcome,
                    });
                }
            }
            if record.accepted && !record.shown && is_context(frame) {
                record.shown = true;
                messages.push((id, mem::take(&mut record.delivery.messages)));
            }
        }
        (updates, messages)
    }

    pub fn written_unsettled(&self) -> Vec<Uuid> {
        self.records
            .iter()
            .filter_map(|(&id, record)| {
                (!record.terminal
                    && !record.settled
                    && (record.written
                        || record.write.as_ref().is_some_and(|write| write.offset != 0)))
                .then_some(id)
            })
            .collect()
    }

    pub fn stop_accounted(&self, id: &Uuid) -> bool {
        self.records
            .get(id)
            .is_some_and(|record| record.started || record.settled || record.terminal)
    }

    pub fn confirm_cancelled(&mut self, ids: &[Uuid]) -> Vec<Update> {
        let mut updates = Vec::new();
        for id in ids {
            if let Some(record) = self.records.get_mut(id) {
                record.write = None;
                record.settled = true;
                record.terminal = true;
                if !record.accepted {
                    record.reported = true;
                    updates.push(Update { attempt:*id, outcome:PeerDeliveryOutcome::Rejected(
                        "Claude native Stop cancelled this queued peer message before consumption".into()) });
                }
            }
        }
        updates
    }

    pub fn known(&self, id: &Uuid) -> bool {
        self.records.get(id).is_some_and(|record| record.accepted)
    }

    pub fn tracks(&self, id: &Uuid) -> bool {
        self.records.contains_key(id)
    }

    pub fn stop(&mut self, reason: &str) -> Vec<Update> {
        let mut updates = Vec::new();
        for (&attempt, record) in &mut self.records {
            if !record.accepted && !record.reported {
                let uncertain =
                    record.written || record.write.as_ref().is_some_and(|write| write.offset != 0);
                updates.push(Update {
                    attempt,
                    outcome: if uncertain {
                        PeerDeliveryOutcome::Unknown(reason.into())
                    } else {
                        PeerDeliveryOutcome::Rejected(reason.into())
                    },
                });
                record.reported = true;
                record.terminal = !uncertain;
            }
            record.write = None;
        }
        self.ready = false;
        updates
    }
}

pub(super) fn is_context(frame: &Value) -> bool {
    frame["type"] == "user"
        && frame["origin"]["kind"] == "peer"
        && !frame["message"]["content"]
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "tool_result"))
}

pub(super) fn correlated_ids(frame: &Value) -> Vec<Uuid> {
    let mut ids = Vec::new();
    if frame["type"] == "control" && frame["action"] == "peer_message_status" {
        if let Some(id) = frame["orig_msg_id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            ids.push(id);
        }
        return ids;
    }
    if frame["origin"]["kind"] == "peer"
        && let Some(id) = frame["origin"]["msg_id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
    {
        ids.push(id);
    }
    for field in ["command_uuid", "message_uuid", "user_message_uuid"] {
        if let Some(id) = frame[field]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    if frame["type"] == "user"
        && frame["origin"]["kind"] == "peer"
        && let Some(id) = frame["uuid"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
        && !ids.contains(&id)
    {
        ids.push(id);
    }
    for id in frame["user_message_uuids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|id| Uuid::parse_str(id).ok())
    {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

fn registry_socket(path: &Path, pid: u32, session: Uuid) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= REGISTRY_LIMIT && metadata.mode() & 0o022 == 0,
        "Invalid Claude peer registry file"
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(REGISTRY_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= REGISTRY_LIMIT,
        "Claude peer registry exceeds 64 KiB"
    );
    let registry: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        registry.get("pid").is_none() || registry["pid"].as_u64() == Some(u64::from(pid)),
        "Claude peer registry PID mismatch"
    );
    ensure!(
        registry["sessionId"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            == Some(session),
        "Claude peer registry session mismatch"
    );
    ensure!(
        registry["peerProtocol"] == 1,
        "Claude runtime does not advertise peer protocol 1"
    );
    let socket = PathBuf::from(
        registry["messagingSocketPath"]
            .as_str()
            .context("Claude peer registry has no socket")?,
    );
    ensure!(
        socket.is_absolute(),
        "Claude peer socket path must be absolute"
    );
    let socket_metadata = fs::symlink_metadata(&socket)?;
    ensure!(
        socket_metadata.file_type().is_socket()
            && socket_metadata.uid() == metadata.uid()
            && socket_metadata.mode() & 0o077 == 0,
        "Claude peer path is not an owned Unix socket"
    );
    let uid = drostje_platform::effective_uid();
    ensure!(
        metadata.uid() == uid,
        "Claude peer registry belongs to another user"
    );
    Ok(socket)
}

fn connect(path: &Path) -> Result<UnixStream> {
    drostje_platform::connect_unix_nonblocking(path).context("Connect Claude peer socket")
}

fn connected(stream: &UnixStream) -> Result<bool> {
    let Some(readiness) = drostje_platform::poll_writable(stream.as_fd())? else {
        return Ok(false);
    };
    if let Some(error) = stream.take_error()? {
        return Err(error.into());
    }
    ensure!(!readiness.hung_up, "Claude peer socket disconnected");
    Ok(readiness.writable)
}

#[cfg(test)]
mod tests;
