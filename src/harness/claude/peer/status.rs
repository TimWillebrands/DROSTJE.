//! Private, status-only return address for one owned Claude runtime. Native replies are
//! explicitly denied, never acknowledged as delivered or silently discarded.
use super::*;
use std::os::unix::{fs::PermissionsExt, net::UnixListener};

const CONNECTIONS: usize = 16;
const STATUS_FRAME: usize = 65_536;
const REPLY_REASON: &str = "This address receives delivery status only; reply with the network tool and sender Thread UUID";

struct Incoming {
    stream: UnixStream,
    bytes: Vec<u8>,
    deadline: Instant,
}

pub(super) struct StatusInbox {
    listener: UnixListener,
    path: PathBuf,
    pub target: PathBuf,
    address: String,
    pid: u32,
    session: Uuid,
    incoming: Vec<Incoming>,
    outgoing: Vec<WriteFrame>,
}

impl StatusInbox {
    pub fn bind(target: &Path, pid: u32, session: Uuid) -> Result<Self> {
        let directory = target
            .parent()
            .context("Claude peer socket has no parent")?;
        let metadata = fs::symlink_metadata(directory)?;
        // SAFETY: geteuid has no arguments or memory-safety preconditions.
        let uid = unsafe { libc::geteuid() };
        ensure!(
            metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o077 == 0,
            "Claude status socket requires an owner-only native socket directory"
        );
        let path = directory.join(format!("{:016x}.sock", Uuid::new_v4().as_u128() as u64));
        // The owner-only directory protects the bind-to-chmod interval without changing umask.
        let listener = UnixListener::bind(&path)?;
        if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .and_then(|_| listener.set_nonblocking(true))
        {
            let _ = fs::remove_file(&path);
            return Err(error.into());
        }
        let address = address(&path);
        Ok(Self {
            listener,
            path,
            target: target.into(),
            address,
            pid,
            session,
            incoming: Vec::new(),
            outgoing: Vec::new(),
        })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn tick(&mut self) -> Vec<Value> {
        let mut frames = Vec::new();
        for _ in 0..CONNECTIONS {
            if self.incoming.len() >= CONNECTIONS {
                break;
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    match peer_pid(&stream).and_then(|pid| {
                        ensure!(
                            pid == self.pid,
                            "Native status connection PID does not match the owned Claude runtime"
                        );
                        stream.set_nonblocking(true)?;
                        Ok(())
                    }) {
                        Ok(()) => self.incoming.push(Incoming {
                            stream,
                            bytes: Vec::new(),
                            deadline: Instant::now() + CONNECT_DEADLINE,
                        }),
                        Err(error) => frames.push(diagnostic(format!(
                            "Rejected unverified native status connection: {error:#}"
                        ))),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    frames.push(diagnostic(format!(
                        "Accept native status connection: {error}"
                    )));
                    break;
                }
            }
        }
        let mut index = 0;
        while index < self.incoming.len() {
            let connection = &mut self.incoming[index];
            let mut remove = Instant::now() >= connection.deadline;
            let mut buffer = [0; 8192];
            if !remove {
                match connection.stream.read(&mut buffer) {
                    Ok(0) => remove = true,
                    Ok(count) => {
                        if connection.bytes.len() + count > STATUS_FRAME {
                            frames.push(diagnostic("Native status frame exceeds 64 KiB".into()));
                            remove = true;
                        } else {
                            connection.bytes.extend_from_slice(&buffer[..count]);
                        }
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => {
                        frames.push(diagnostic(format!(
                            "Read native status connection: {error}"
                        )));
                        remove = true;
                    }
                }
            }
            // One protocol frame per connection is sufficient for receipts and replies. An
            // optional auth line is never needed on this Unix status-only endpoint; redact it.
            if let Some(end) = connection.bytes.iter().position(|byte| *byte == b'\n') {
                match serde_json::from_slice::<Value>(&connection.bytes[..end]) {
                    Ok(frame) if frame["type"] == "auth" => frames.push(diagnostic(
                        "Unexpected authentication frame at native status-only socket (redacted)"
                            .into(),
                    )),
                    Ok(frame) => frames.push(frame),
                    Err(error) => frames.push(diagnostic(format!(
                        "Malformed native status frame: {error}"
                    ))),
                }
                remove = true;
            } else if remove && !connection.bytes.is_empty() {
                frames.push(diagnostic(
                    "Native status connection ended without a complete frame".into(),
                ));
            }
            if remove {
                self.incoming.swap_remove(index);
            } else {
                index += 1;
            }
        }
        for frame in &frames {
            if frame["type"] == "user" {
                if let Err(error) = self.deny_reply(frame) {
                    // Preserve the original frame AND the explicit rejection-transport failure.
                    // We never issue a successful delivery receipt for a native reply.
                    return frames
                        .iter()
                        .cloned()
                        .chain(std::iter::once(diagnostic(format!(
                            "Could not return native reply denial: {error:#}"
                        ))))
                        .collect();
                }
            }
        }
        let mut index = 0;
        while index < self.outgoing.len() {
            let write = &mut self.outgoing[index];
            let mut remove = false;
            let result = (|| -> Result<()> {
                ensure!(
                    Instant::now() < write.deadline,
                    "Native reply denial transport timed out"
                );
                if !connected(&write.stream)? {
                    return Ok(());
                }
                match write.stream.write(&write.bytes[write.offset..]) {
                    Ok(0) => bail!("Native reply denial socket closed"),
                    Ok(count) => {
                        write.offset += count;
                        remove = write.offset == write.bytes.len();
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
                Ok(())
            })();
            if let Err(error) = result {
                frames.push(diagnostic(format!(
                    "Could not send native reply denial: {error:#}"
                )));
                remove = true;
            }
            if remove {
                self.outgoing.swap_remove(index);
            } else {
                index += 1;
            }
        }
        frames
    }

    fn deny_reply(&mut self, frame: &Value) -> Result<()> {
        ensure!(
            self.outgoing.len() < CONNECTIONS,
            "Native reply denial queue is full"
        );
        // Always send to the validated owned child socket, NEVER a sender-authored `from`.
        let stream = connect(&self.target)?;
        ensure!(
            peer_pid(&stream)? == self.pid,
            "Native reply denial target PID changed"
        );
        let mut bytes = serde_json::to_vec(&json!({
            "msgV":1,"msg_id":Uuid::new_v4(),"type":"control","action":"peer_message_status",
            "status":"denied","orig_msg_id":frame["msg_id"],"reason":REPLY_REASON,
            "from":self.address,"session_id":self.session,
        }))?;
        bytes.push(b'\n');
        self.outgoing.push(WriteFrame {
            stream,
            bytes,
            offset: 0,
            deadline: Instant::now() + CONNECT_DEADLINE,
        });
        Ok(())
    }
}

impl Drop for StatusInbox {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn diagnostic(reason: String) -> Value {
    json!({"type":"drostje_peer_status_diagnostic","reason":reason})
}

pub(super) fn address(path: &Path) -> String {
    let mut address = String::from("uds:");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'/' | b'.' | b'-') {
            address.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(address, "%{byte:02X}");
        }
    }
    address
}

fn peer_pid(stream: &UnixStream) -> Result<u32> {
    #[cfg(target_os = "macos")]
    {
        let mut pid: libc::pid_t = 0;
        let mut length = mem::size_of_val(&pid) as libc::socklen_t;
        // SAFETY: pid and length are initialized writable storage of the advertised sizes;
        // getsockopt reads kernel credentials from this live connected Unix socket.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut length,
            )
        };
        ensure!(
            result == 0 && length as usize == mem::size_of_val(&pid) && pid > 0,
            "Read native status peer PID: {}",
            std::io::Error::last_os_error()
        );
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: uid/gid are valid writable scalar outputs for this live socket.
        let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
        // SAFETY: geteuid has no arguments or memory-safety preconditions.
        ensure!(
            result == 0 && uid == unsafe { libc::geteuid() },
            "Native status peer is not the current user"
        );
        Ok(pid as u32)
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: zero is a valid initial representation of the kernel credential struct.
        let mut credentials: libc::ucred = unsafe { mem::zeroed() };
        let mut length = mem::size_of_val(&credentials) as libc::socklen_t;
        // SAFETY: credentials and length are writable outputs of the advertised sizes;
        // this reads kernel-generated credentials, never sender-authored frame fields.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        // SAFETY: geteuid has no arguments or memory-safety preconditions.
        ensure!(
            result == 0
                && length as usize == mem::size_of_val(&credentials)
                && credentials.pid > 0
                && credentials.uid == unsafe { libc::geteuid() },
            "Read native status peer credentials"
        );
        Ok(credentials.pid as u32)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = stream;
        bail!("Native peer status credentials are unsupported on this platform")
    }
}
