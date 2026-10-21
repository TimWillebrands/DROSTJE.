//! Claude Code protocol-1 inbox transport. This is deliberately separate from stdin:
//! native inbound policy and the CLI's safe-boundary queue remain authoritative.
use super::*;
use std::{
    mem,
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt},
            net::UnixStream,
        },
    },
};

const REGISTRY_LIMIT: u64 = 65_536;
const CONNECT_DEADLINE: Duration = Duration::from_secs(5);
const RECORD_LIMIT: usize = 128;

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

    pub fn probe(&mut self) -> bool {
        if Instant::now() < self.probe_at {
            return false;
        }
        self.probe_at = Instant::now() + Duration::from_millis(500);
        let ready = self
            .discover()
            .and_then(|path| connect(&path))
            .and_then(|stream| {
                ensure!(connected(&stream)?, "Claude peer inbox is still connecting");
                Ok(())
            })
            .is_ok();
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
                    .is_some_and(|r| r.terminal || (r.accepted && r.shown))
            }) else {
                bail!("Claude peer admission tracking is full");
            };
            let id = self.order.remove(index).unwrap();
            self.records.remove(&id);
        }
        let body = super::super::peer_context(&delivery.messages)?;
        // No `from`: this is intentionally a one-way native peer envelope. Do not invent a
        // reply address or assert a Claude permission class / own-child authentication.
        let content = format!(
            "<cross-session-message from-name=\"Space network\">\n{body}\n</cross-session-message>"
        );
        let mut bytes = serde_json::to_vec(&json!({
            "msgV":1, "msg_id":delivery.attempt, "type":"user",
            "message":{"role":"user","content":content}, "priority":"next",
            "session_id":self.session, "uuid":delivery.attempt,
        }))?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= FRAME, "Claude peer frame exceeds 1 MiB");
        let path = self.discover()?;
        let stream = connect(&path)?;
        // Admission can be revoked while discovery/connect runs; no bytes have been written.
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
            } else if let Some(write) = &mut record.write {
                if Instant::now() >= write.deadline {
                    failure = Some(
                        "Claude peer socket did not accept the complete frame within 5 seconds"
                            .into(),
                    );
                } else {
                    match connected(&write.stream) {
                        Ok(false) => {}
                        Err(error) => failure = Some(format!("Claude peer socket: {error:#}")),
                        Ok(true) => match write.stream.write(&write.bytes[write.offset..]) {
                            Ok(0) => failure = Some("Claude peer socket closed".into()),
                            Ok(count) => {
                                write.offset += count;
                                if write.offset == write.bytes.len() {
                                    record.written = true;
                                }
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::WouldBlock
                                        | std::io::ErrorKind::Interrupted
                                ) => {}
                            Err(error) => failure = Some(format!("Claude peer socket: {error}")),
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
            } else if record.written {
                // Closing after a complete NDJSON frame is transport completion, NOT admission.
                record.write = None;
            }
        }
        updates
    }

    /// Only native stdout can confirm admission. Socket write completion never calls this.
    pub fn observe(&mut self, frame: &Value) -> (Vec<Update>, Vec<(Uuid, Vec<PeerMessage>)>) {
        let mut updates = Vec::new();
        let mut messages = Vec::new();
        let kind = frame["type"].as_str().unwrap_or("");
        let is_peer = frame["origin"]["kind"] == "peer";
        if is_peer
            && frame["origin"]["verifiedPeerPid"]
                .as_u64()
                .is_some_and(|pid| pid != u64::from(std::process::id()))
        {
            return (updates, messages);
        }
        let hold = kind == "system"
            && frame["subtype"] == "peer_message_hold"
            && frame["lane"] == "socket";
        let lifecycle = kind == "command_lifecycle";
        let ids = correlated_ids(frame);
        for id in ids {
            let Some(record) = self.records.get_mut(&id) else {
                continue;
            };
            // Unwritten attempts cannot have native evidence from this transport.
            if !record.written || record.terminal {
                continue;
            }
            let status = if hold {
                frame["state"].as_str()
            } else if lifecycle {
                frame["state"].as_str()
            } else {
                None
            };
            let accepted = matches!(status, Some("queued" | "started" | "released"))
                || (is_peer && matches!(kind, "user" | "assistant" | "result"))
                || (matches!(kind, "assistant" | "result")
                    && frame["user_message_uuids"].is_array());
            if accepted && !record.accepted {
                record.accepted = true;
                record.reported = true;
                updates.push(Update {
                    attempt: id,
                    outcome: PeerDeliveryOutcome::Accepted,
                });
            } else if !record.accepted {
                let outcome = match status {
                    Some("held") if !record.reported => {
                        Some(PeerDeliveryOutcome::Unknown(format!(
                            "Claude native inbound policy held this peer message ({})",
                            frame["cause"].as_str().unwrap_or("approval required")
                        )))
                    }
                    Some("dropped" | "refused" | "discarded") => {
                        Some(PeerDeliveryOutcome::Rejected(format!(
                            "Claude native inbound policy did not admit this peer message ({})",
                            frame["outcome"].as_str().unwrap_or(status.unwrap())
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
    for field in ["command_uuid", "message_uuid", "user_message_uuid"] {
        if let Some(id) = frame[field]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    if frame["type"] == "user" && frame["origin"]["kind"] == "peer" {
        if let Some(id) = frame["uuid"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
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
        metadata.is_file() && metadata.len() <= REGISTRY_LIMIT,
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
        socket_metadata.file_type().is_socket() && socket_metadata.uid() == metadata.uid(),
        "Claude peer path is not an owned Unix socket"
    );
    // SAFETY: geteuid takes no arguments and has no memory-safety preconditions.
    let uid = unsafe { libc::geteuid() };
    ensure!(
        metadata.uid() == uid,
        "Claude peer registry belongs to another user"
    );
    Ok(socket)
}

fn connect(path: &Path) -> Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: an all-zero sockaddr_un is a valid initial representation; family and path
    // are set below, and the checked length leaves a terminating NUL in sun_path.
    let mut address: libc::sockaddr_un = unsafe { mem::zeroed() };
    ensure!(
        bytes.len() < address.sun_path.len() && !bytes.contains(&0),
        "Claude peer socket path is invalid or too long"
    );
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = mem::size_of_val(&address) as u8;
    }
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    // SAFETY: socket creates a fresh descriptor with the specified Unix stream family.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    ensure!(
        fd >= 0,
        "Open Claude peer socket: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: successful socket returned a unique owned descriptor; OwnedFd closes it on every error path.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    nonblocking(owned.as_raw_fd())?;
    // SAFETY: F_SETFD operates on this live owned descriptor; FD_CLOEXEC prevents
    // unrelated subprocesses from retaining the private in-flight transport.
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } >= 0,
        "Set Claude peer socket close-on-exec: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: address points to initialized sockaddr_un storage for the supplied size;
    // the descriptor is live and nonblocking, so connect cannot stall the actor.
    let result = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            mem::size_of_val(&address) as libc::socklen_t,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        ensure!(
            matches!(error.raw_os_error(), Some(libc::EINPROGRESS | libc::EAGAIN)),
            "Connect Claude peer socket: {error}"
        );
    }
    Ok(UnixStream::from(owned))
}

fn connected(stream: &UnixStream) -> Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: descriptor is one initialized pollfd pointing at a live socket; timeout zero
    // makes this a readiness check, never a blocking wait in the conversation actor.
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if result == 0 {
        return Ok(false);
    }
    if let Some(error) = stream.take_error()? {
        return Err(error.into());
    }
    ensure!(
        descriptor.revents & (libc::POLLHUP | libc::POLLNVAL) == 0,
        "Claude peer socket disconnected"
    );
    Ok(descriptor.revents & libc::POLLOUT != 0)
}

#[cfg(test)]
mod tests;
