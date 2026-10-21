use super::*;
use crate::platform::IdleSleepAssertion;
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

const FRAME: usize = 1_048_576;
const LOGICAL: usize = 67_108_864;
const CHUNK: usize = 262_144;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const MODEL_TIMEOUT: Duration = Duration::from_secs(60);

mod peer;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    Omp,
    Pi,
}
impl Dialect {
    fn for_harness(harness: &str) -> Result<Self> {
        match harness {
            "omp" => Ok(Self::Omp),
            "pi" => Ok(Self::Pi),
            _ => bail!("Unsupported Pi-family conversation harness: {harness}"),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Omp => "OMP",
            Self::Pi => "Pi",
        }
    }
}

pub(super) fn submission_size_for_lengths(
    id: Uuid,
    text: &str,
    images: &[(&str, u64)],
) -> Result<u64> {
    let mut frame = json!({"id":id,"type":"prompt","message":text});
    if !images.is_empty() {
        frame["images"] = images
            .iter()
            .map(|(mime, _)| json!({"type":"image","mimeType":mime,"data":""}))
            .collect();
    }
    let mut size = serde_json::to_vec(&frame)?.len() as u64 + 1;
    for (_, length) in images {
        size = size
            .checked_add(
                length
                    .div_ceil(3)
                    .checked_mul(4)
                    .context("Image size overflow")?,
            )
            .context("Request size overflow")?;
    }
    Ok(size)
}
pub(super) fn submission_size(id: Uuid, text: &str, images: &[ImageAttachment]) -> Result<u64> {
    let lengths = images
        .iter()
        .map(|image| Ok((image.mime.as_str(), fs::metadata(&image.path)?.len())))
        .collect::<Result<Vec<_>>>()?;
    submission_size_for_lengths(id, text, &lengths)
}

#[derive(Default)]
struct FrameDecoder {
    pending: Option<Chunks>,
}
struct Chunks {
    id: String,
    count: usize,
    length: usize,
    next: usize,
    bytes: Vec<u8>,
}
impl FrameDecoder {
    fn push(&mut self, value: Value) -> Result<Option<Value>> {
        if value["type"] != "rpc_chunk" {
            ensure!(self.pending.is_none(), "RPC chunk sequence interrupted");
            ensure!(value.is_object(), "RPC frame must be an object");
            return Ok(Some(value));
        }
        let id = value["chunkId"].as_str().context("Invalid RPC chunk ID")?;
        let number = |key: &str| -> Result<usize> {
            usize::try_from(value[key].as_u64().context("Invalid RPC chunk metadata")?)
                .context("RPC integer overflow")
        };
        let index = number("index")?;
        let count = number("count")?;
        let length = number("byteLength")?;
        ensure!(
            !id.is_empty()
                && id.encode_utf16().count() <= 128
                && (2..=LOGICAL / CHUNK).contains(&count)
                && index < count
                && (FRAME..=LOGICAL).contains(&length),
            "Invalid RPC chunk metadata"
        );
        let encoded = value["data"].as_str().context("Invalid RPC chunk data")?;
        ensure!(
            !encoded.is_empty() && encoded.len() <= CHUNK.div_ceil(3) * 4,
            "Invalid RPC chunk data length"
        );
        let bytes = STANDARD
            .decode(encoded)
            .context("Invalid RPC chunk base64")?;
        ensure!(
            STANDARD.encode(&bytes) == encoded && bytes.len() <= CHUNK,
            "Invalid RPC chunk data"
        );
        if self.pending.is_none() {
            ensure!(index == 0, "RPC chunk sequence must start at zero");
            self.pending = Some(Chunks {
                id: id.into(),
                count,
                length,
                next: 0,
                bytes: Vec::new(),
            });
        }
        let pending = self.pending.as_mut().unwrap();
        ensure!(
            pending.id == id
                && pending.count == count
                && pending.length == length
                && pending.next == index,
            "RPC chunk sequence mismatch"
        );
        ensure!(
            pending.bytes.len() + bytes.len() <= length,
            "RPC chunk exceeds declared length"
        );
        pending.bytes.extend_from_slice(&bytes);
        pending.next += 1;
        if pending.next != count {
            return Ok(None);
        }
        let pending = self.pending.take().unwrap();
        ensure!(pending.bytes.len() == length, "RPC chunk length mismatch");
        let text = std::str::from_utf8(&pending.bytes).context("Invalid RPC chunk UTF-8")?;
        let decoded: Value = serde_json::from_str(text)?;
        ensure!(decoded.is_object(), "RPC frame must be an object");
        Ok(Some(decoded))
    }
    fn eof(&self) -> Result<()> {
        ensure!(self.pending.is_none(), "Truncated RPC chunk sequence");
        Ok(())
    }
}

struct OwnedChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    reaped: bool,
    deadline: Option<Instant>,
}
impl OwnedChild {
    fn close(&mut self) {
        self.stdin.take();
        if self.reaped {
            return;
        }
        let deadline = self
            .deadline
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(5));
        let stdout_nonblocking = nonblocking(self.stdout.as_raw_fd()).is_ok();
        let stderr_nonblocking = nonblocking(self.stderr.as_raw_fd()).is_ok();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.reaped = true;
                    return;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => break,
            }
            // Drain pipes so orderly shutdown cannot deadlock on unread output.
            let mut discard = [0u8; 16384];
            if stdout_nonblocking {
                let _ = self.stdout.read(&mut discard);
            }
            if stderr_nonblocking {
                let _ = self.stderr.read(&mut discard);
            }
        }
        // This PID belongs to the unreaped child whose setsid succeeded at spawn.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.reaped = true;
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
        self.reaped = true;
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.close();
    }
}
fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "Set OMP pipe nonblocking: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
fn prepare(
    config: &mut ConversationConfig,
    dialect: Dialect,
) -> Result<(OwnedChild, Option<String>)> {
    ensure!(
        Dialect::for_harness(&config.harness)? == dialect,
        "Conversation harness changed while opening"
    );
    config.cwd = fs::canonicalize(&config.cwd).context("Project root is unavailable")?;
    ensure!(config.cwd.is_dir(), "Project root is not a directory");
    fs::create_dir_all(&config.session_dir)?;
    config.session_dir = fs::canonicalize(&config.session_dir)?;
    let mut resume = None;
    if let Some(reference) = &config.session_ref {
        let saved: Value = serde_json::from_str(reference).with_context(|| {
            format!(
                "Invalid saved {} session reference; create a new conversation explicitly",
                dialect.name()
            )
        })?;
        ensure!(
            saved["cwd"].as_str() == config.cwd.to_str()
                && saved["session_dir"].as_str() == config.session_dir.to_str(),
            "Saved {} session belongs to a different project or session directory",
            dialect.name()
        );
        let id = saved["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .with_context(|| format!("Saved {} session ID is missing", dialect.name()))?;
        if let Some(file) = saved["file"].as_str() {
            let file = fs::canonicalize(file).with_context(|| {
                format!(
                    "Saved {} session file is missing; create a new conversation explicitly",
                    dialect.name()
                )
            })?;
            ensure!(
                file.starts_with(&config.session_dir) && file.is_file(),
                "Saved {} session file is outside this conversation",
                dialect.name()
            );
            fs::File::open(&file)
                .with_context(|| format!("Saved {} session file is unreadable", dialect.name()))?;
            resume = Some(if dialect == Dialect::Pi {
                file.into_os_string()
            } else {
                id.into()
            });
        } else {
            ensure!(
                saved["file"].is_null() && config.allow_empty_session,
                "Saved {} session has no durable file and a submission may exist; inspect recovery data or create a new conversation explicitly",
                dialect.name()
            );
            config.session_ref = None;
        }
    }
    let mut command = Command::new(&config.executable);
    command
        .args(["--mode", "rpc"])
        .arg("--session-dir")
        .arg(&config.session_dir)
        .current_dir(&config.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let browser_warning = crate::browser_tools::configure_harness(
        &mut command,
        &config.harness,
        &config.session_dir,
        config.browser_tools.as_ref(),
    );
    if dialect == Dialect::Omp {
        command.arg("--cwd").arg(&config.cwd);
        command.env("PI_RPC_EMIT_TITLE", "1");
    } else if let Ok(node) = crate::platform::resolve_executable("node", None)
        && let Some(directory) = node.parent()
    {
        let mut paths = vec![directory.to_path_buf()];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        if let Ok(path) = std::env::join_paths(paths) {
            command.env("PATH", path);
        }
    }
    if let Some(reference) = resume {
        command
            .arg(if dialect == Dialect::Pi {
                "--session"
            } else {
                "--resume"
            })
            .arg(reference);
    }
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command.spawn().with_context(|| {
        format!(
            "Start {} at {}",
            dialect.name(),
            config.executable.display()
        )
    })?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let owned = OwnedChild {
        child,
        stdin: Some(stdin),
        stdout,
        stderr,
        reaped: false,
        deadline: None,
    };
    nonblocking(owned.stdin.as_ref().unwrap().as_raw_fd())?;
    nonblocking(owned.stdout.as_raw_fd())?;
    nonblocking(owned.stderr.as_raw_fd())?;
    Ok((owned, browser_warning))
}

pub(super) async fn open(mut config: ConversationConfig) -> Result<ConversationHandle> {
    let dialect = Dialect::for_harness(&config.harness)?;
    let (commands, input) = async_channel::bounded(64);
    let (output, events) = async_channel::bounded(128);
    smol::unblock(move || -> Result<()> {
        let (child, browser_warning) = prepare(&mut config, dialect)?;
        if let Some(text) = browser_warning {
            let _ = output.try_send(ConversationEvent::Notice {
                level: "warning".into(),
                text,
            });
        }
        std::thread::Builder::new()
            .name(format!("drostje-{}", config.harness))
            .spawn(move || {
                let mut actor = Actor::new(config, child, input, output, dialect);
                if let Err(error) = actor.initialize().and_then(|()| actor.run()) {
                    actor.disconnect(format!("{error:#}"));
                }
                actor.child.close();
                // Final lifecycle notifications are delivered after shutdown, never blocking control.
                for event in actor.events.drain(..) {
                    if actor.output.send_blocking(event).is_err() {
                        break;
                    }
                }
            })
            .with_context(|| format!("Start {} actor", dialect.name()))?;
        Ok(())
    })
    .await?;
    Ok(ConversationHandle { commands, events })
}

#[derive(Clone, Debug)]
enum RequestKind {
    Negotiate,
    State,
    Models,
    Model(ModelKey),
    History {
        cursor: Option<String>,
        generation: u64,
    },
    Prompt(Uuid),
    Abort,
    Commands,
    Peer(Option<Uuid>),
}
struct Pending {
    kind: RequestKind,
    command: String,
    deadline: Instant,
}
#[derive(Default)]
struct Requests {
    pending: HashMap<String, Pending>,
}
impl Requests {
    fn take(&mut self, frame: &Value) -> Result<Option<Pending>> {
        let Some(id) = frame["id"].as_str() else {
            return Ok(None);
        };
        if let Some(pending) = self.pending.get(id) {
            ensure!(
                frame["command"].as_str() == Some(pending.command.as_str()),
                "RPC response command does not match its ID"
            );
        }
        Ok(self.pending.remove(id))
    }
}
#[derive(Default)]
struct Turn {
    id: Option<Uuid>,
    accepted: bool,
    interrupting: bool,
    failed: bool,
    terminal: Option<TurnOutcome>,
}
impl Turn {
    fn begin(&mut self, id: Uuid) {
        *self = Self {
            id: Some(id),
            ..Self::default()
        };
    }
    fn accept(&mut self, id: Uuid) -> Option<ConversationEvent> {
        if self.id == Some(id) && !self.accepted {
            self.accepted = true;
            Some(ConversationEvent::SubmissionAccepted { request_id: id })
        } else {
            None
        }
    }
    fn settle(&mut self, outcome: TurnOutcome) -> Option<ConversationEvent> {
        let id = self.id?;
        if !self.accepted && matches!(outcome, TurnOutcome::Completed | TurnOutcome::Interrupted) {
            self.terminal = Some(outcome);
            return None;
        }
        *self = Self::default();
        Some(ConversationEvent::TurnSettled {
            request_id: id,
            outcome,
        })
    }
    fn agent_end(&self, frame: &Value) -> Option<TurnOutcome> {
        if frame["isTerminal"] == false {
            None
        } else if self.interrupting {
            Some(TurnOutcome::Interrupted)
        } else if self.failed {
            Some(TurnOutcome::Failed)
        } else {
            Some(TurnOutcome::Completed)
        }
    }
    fn prompt_result(&self, frame: &Value) -> Option<TurnOutcome> {
        if frame["agentInvoked"] == false
            && frame["id"].as_str().and_then(|id| Uuid::parse_str(id).ok()) == self.id
            && self.id.is_some()
        {
            Some(if self.interrupting {
                TurnOutcome::Interrupted
            } else {
                TurnOutcome::Completed
            })
        } else {
            None
        }
    }
    fn late_failure(&mut self, frame: &Value) -> Option<ConversationEvent> {
        if frame["command"] == "prompt"
            && frame["success"] == false
            && frame["id"].as_str().and_then(|id| Uuid::parse_str(id).ok()) == self.id
        {
            self.settle(TurnOutcome::Failed)
        } else {
            None
        }
    }
}
struct Dialog {
    request: UserRequest,
    deadline: Option<Instant>,
}
struct Actor {
    config: ConversationConfig,
    dialect: Dialect,
    child: OwnedChild,
    commands: async_channel::Receiver<ConversationCommand>,
    output: async_channel::Sender<ConversationEvent>,
    events: VecDeque<ConversationEvent>,
    decoder: FrameDecoder,
    line: Vec<u8>,
    writes: VecDeque<Vec<u8>>,
    write_offset: usize,
    requests: Requests,
    dialogs: HashMap<String, Dialog>,
    tools: HashMap<String, ToolActivity>,
    turn: Turn,
    peers: peer::Peers,
    idle_sleep_assertion: Option<IdleSleepAssertion>,
    state: ConversationState,
    ready_deadline: Instant,
    negotiated: bool,
    initialized: bool,
    session_ref: Option<String>,
    models: Option<Vec<ModelChoice>>,
    model: Option<ModelKey>,
    initial_model: Option<ModelKey>,
    history_generation: u64,
    history_index: usize,
    history_wait: bool,
    next_poll: Instant,
    current_message: Option<String>,
    sequence: u64,
    closing: bool,
    eof: bool,
    read_buffer: [u8; 16384],
    read_start: usize,
    read_end: usize,
    submissions: VecDeque<Uuid>,
    images: ImageCache,
    local_outputs: VecDeque<Message>,
}
impl Actor {
    fn new(
        config: ConversationConfig,
        child: OwnedChild,
        commands: async_channel::Receiver<ConversationCommand>,
        output: async_channel::Sender<ConversationEvent>,
        dialect: Dialect,
    ) -> Self {
        let initial_model = config.model.clone();
        Self {
            config,
            dialect,
            child,
            commands,
            output,
            events: VecDeque::from([ConversationEvent::StateChanged(ConversationState::Starting)]),
            decoder: FrameDecoder::default(),
            line: Vec::new(),
            writes: VecDeque::new(),
            write_offset: 0,
            requests: Requests::default(),
            dialogs: HashMap::new(),
            tools: HashMap::new(),
            turn: Turn::default(),
            peers: peer::Peers::default(),
            idle_sleep_assertion: None,
            state: ConversationState::Starting,
            ready_deadline: Instant::now() + CONTROL_TIMEOUT,
            negotiated: dialect == Dialect::Pi,
            initialized: false,
            session_ref: None,
            models: None,
            model: None,
            initial_model,
            history_generation: 0,
            history_index: 0,
            history_wait: false,
            next_poll: Instant::now(),
            current_message: None,
            sequence: 0,
            closing: false,
            eof: false,
            read_buffer: [0; 16384],
            read_start: 0,
            read_end: 0,
            submissions: VecDeque::new(),
            images: ImageCache::default(),
            local_outputs: VecDeque::new(),
        }
    }
    fn initialize(&mut self) -> Result<()> {
        if self.dialect == Dialect::Pi {
            self.request(json!({"type":"get_state"}), RequestKind::State)?;
            self.request(json!({"type":"get_available_models"}), RequestKind::Models)?;
            self.request_commands()?;
        }
        Ok(())
    }
    /// OMP also pushes `available_commands_update` whenever its list changes; Pi has no such
    /// event, so its list is fetched again after every turn.
    fn request_commands(&mut self) -> Result<()> {
        if self
            .requests
            .pending
            .values()
            .any(|pending| matches!(pending.kind, RequestKind::Commands))
        {
            return Ok(());
        }
        let command = match self.dialect {
            Dialect::Omp => "get_available_commands",
            Dialect::Pi => "get_commands",
        };
        self.request(json!({ "type": command }), RequestKind::Commands)
    }
    fn commands(&mut self, raw: &Value) {
        self.peer_commands(raw);
        match parse_commands(self.dialect, raw) {
            Ok((commands, rejected)) => {
                if !rejected.is_empty() {
                    self.emit(ConversationEvent::Diagnostic {
                        label: "Commands without a name".into(),
                        raw: Value::Array(rejected),
                    });
                }
                self.emit(ConversationEvent::CommandsUpdated(commands));
            }
            Err(error) => self.emit(ConversationEvent::CommandsUnavailable(format!("{error:#}"))),
        }
    }
    fn emit(&mut self, event: ConversationEvent) {
        self.events.push_back(event);
    }
    fn notice(&mut self, level: &str, text: impl Into<String>) {
        self.emit(ConversationEvent::Notice {
            level: level.into(),
            text: text.into(),
        });
    }
    fn state(&mut self, state: ConversationState) {
        let state = if state == ConversationState::Ready && !self.peers.turns.is_empty() {
            ConversationState::Running
        } else {
            state
        };
        // Ready can precede prompt acceptance. Keep protection until that turn settles.
        if state == ConversationState::Disconnected
            || (state == ConversationState::Ready && self.turn.id.is_none())
        {
            self.idle_sleep_assertion = None;
        }
        if self.state != state {
            self.state = state.clone();
            self.emit(ConversationEvent::StateChanged(state));
        }
    }
    fn active(&self) -> bool {
        self.turn.id.is_some()
            || self.peers.idle.is_some()
            || !self.peers.turns.is_empty()
            || matches!(
                self.state,
                ConversationState::Running
                    | ConversationState::WaitingForInput
                    | ConversationState::Interrupting
            )
    }
    fn queue(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        ensure!(
            bytes.len() <= FRAME,
            "{} request exceeds 1 MiB; optimize images explicitly or reduce the draft",
            self.dialect.name()
        );
        ensure!(
            self.writes.len() < 128,
            "{} command pipe is not accepting input",
            self.dialect.name()
        );
        self.writes.push_back(bytes);
        Ok(())
    }
    fn request(&mut self, mut value: Value, kind: RequestKind) -> Result<()> {
        let id = value["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        value["id"] = json!(id);
        let command = value["type"]
            .as_str()
            .context("Missing command type")?
            .to_owned();
        let timeout = if matches!(kind, RequestKind::Models) {
            MODEL_TIMEOUT
        } else {
            CONTROL_TIMEOUT
        };
        self.queue(value)?;
        self.requests.pending.insert(
            id,
            Pending {
                kind,
                command,
                deadline: Instant::now() + timeout,
            },
        );
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        while let Some(bytes) = self.writes.front() {
            let Some(stdin) = self.child.stdin.as_mut() else {
                bail!("{} input closed", self.dialect.name());
            };
            match stdin.write(&bytes[self.write_offset..]) {
                Ok(0) => bail!("{} input closed", self.dialect.name()),
                Ok(count) => {
                    self.write_offset += count;
                    if self.write_offset == bytes.len() {
                        self.writes.pop_front();
                        self.write_offset = 0;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    fn run(&mut self) -> Result<()> {
        loop {
            while let Some(event) = self.events.pop_front() {
                match self.output.try_send(event) {
                    Ok(()) => {}
                    Err(async_channel::TrySendError::Full(event)) => {
                        self.events.push_front(event);
                        break;
                    }
                    Err(async_channel::TrySendError::Closed(_)) => {
                        self.close()?;
                        self.events.clear();
                        break;
                    }
                }
            }
            ensure!(
                self.events.len() < 512,
                "Conversation display stopped consuming lifecycle events"
            );
            for _ in 0..16 {
                match self.commands.try_recv() {
                    Ok(command) => self.command(command)?,
                    Err(async_channel::TryRecvError::Empty) => break,
                    Err(async_channel::TryRecvError::Closed) => {
                        self.close()?;
                        break;
                    }
                }
            }
            self.cancel_peers(
                false,
                "Peer delivery cancelled after possible dispatch; never replay",
            )?;
            self.flush()?;
            if self.closing {
                if self.writes.is_empty() {
                    self.child.stdin.take();
                }
                if self.eof || Instant::now() >= self.ready_deadline {
                    if let Some(event) = self.turn.settle(TurnOutcome::Unknown) {
                        self.emit(event);
                    }
                    self.state(ConversationState::Disconnected);
                    return Ok(());
                }
            } else {
                self.tick()?;
            }
            let mut drained = false;
            for _ in 0..16 {
                if self.events.len() >= 256 {
                    break;
                }
                if !self.read_stdout()? {
                    drained = true;
                    break;
                }
            }
            if drained {
                if let Some(status) = self
                    .child
                    .child
                    .try_wait()
                    .with_context(|| format!("Wait for owned {} child", self.dialect.name()))?
                {
                    self.child.reaped = true;
                    self.decoder.eof()?;
                    ensure!(
                        self.line.is_empty(),
                        "{} exited with a truncated physical frame",
                        self.dialect.name()
                    );
                    if self.closing {
                        self.eof = true;
                    } else {
                        bail!("{} process exited: {status}", self.dialect.name());
                    }
                }
            }
            let mut error_bytes = [0u8; 4096];
            match self.child.stderr.read(&mut error_bytes) {
                Ok(n) if n > 0 && self.events.len() < 256 => self.notice(
                    "diagnostic",
                    String::from_utf8_lossy(&error_bytes[..n]).into_owned(),
                ),
                Err(error)
                    if !matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    return Err(error.into());
                }
                _ => {}
            }
            // stdout EOF is authoritative: drain buffered terminal frames before reaping.
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn read_stdout(&mut self) -> Result<bool> {
        if self.read_start == self.read_end {
            match self.child.stdout.read(&mut self.read_buffer) {
                Ok(0) => {
                    self.decoder.eof()?;
                    ensure!(self.line.is_empty(), "Truncated RPC physical frame");
                    if self.closing {
                        self.eof = true;
                        return Ok(false);
                    }
                    bail!("{} disconnected (stdout EOF)", self.dialect.name());
                }
                Ok(count) => {
                    self.read_start = 0;
                    self.read_end = count;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(false);
                }
                Err(error) => return Err(error.into()),
            }
        }
        let pending = &self.read_buffer[self.read_start..self.read_end];
        let length = pending
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(pending.len());
        let frame_limit = if self.dialect == Dialect::Pi {
            LOGICAL
        } else {
            FRAME
        };
        ensure!(
            self.line.len() + length <= frame_limit,
            "{} physical stdout frame exceeds {} MiB",
            self.dialect.name(),
            frame_limit / 1_048_576
        );
        self.line.extend_from_slice(&pending[..length]);
        self.read_start += length;
        if self.line.last() == Some(&b'\n') {
            let value: Value = serde_json::from_slice(&self.line)
                .with_context(|| format!("Malformed {} stdout JSON/UTF-8", self.dialect.name()))?;
            ensure!(
                self.negotiated || value["type"] != "rpc_chunk",
                "{} sent protocol v2 chunks before negotiation succeeded",
                self.dialect.name()
            );
            self.line.clear();
            if let Some(value) = self.decoder.push(value)? {
                self.frame(value)?;
            }
        }
        Ok(true)
    }
    fn command(&mut self, command: ConversationCommand) -> Result<()> {
        if self.closing {
            match command {
                ConversationCommand::Submit { request_id, .. } => {
                    self.emit(ConversationEvent::Failed {
                        request_id: Some(request_id),
                        message: "Conversation is closing; prompt was not sent".into(),
                    })
                }
                ConversationCommand::DeliverPeer(delivery) => self.deliver_peer(delivery),
                _ => {}
            }
            return Ok(());
        }
        match command {
            ConversationCommand::Close => self.close()?,
            ConversationCommand::DeliverPeer(delivery) => self.deliver_peer(delivery),
            ConversationCommand::Interrupt => {
                self.cancel_peers(
                    true,
                    "Peer delivery interrupted after possible dispatch; never replay",
                )?;
                if self.active() {
                    self.turn.interrupting = true;
                    self.peers.interrupted = true;
                    self.state(ConversationState::Interrupting);
                    self.cancel_dialogs(false)?;
                    self.request(json!({"type":"abort"}), RequestKind::Abort)?;
                }
            }
            ConversationCommand::Respond { request_id, answer } => {
                if let Some(dialog) = self.dialogs.get(&request_id) {
                    let valid = match (&dialog.request.kind, &answer) {
                        (_, UserAnswer::Cancelled { .. }) => true,
                        (UserRequestKind::Confirm, UserAnswer::Confirmed(_)) => true,
                        (
                            UserRequestKind::Input | UserRequestKind::Editor,
                            UserAnswer::Value(_),
                        ) => true,
                        (UserRequestKind::Select, UserAnswer::Value(value)) => dialog
                            .request
                            .options
                            .iter()
                            .any(|option| option.value == *value),
                        (UserRequestKind::Permission | UserRequestKind::Question, _)
                        | (_, UserAnswer::PermissionAuto) => false,
                        _ => false,
                    };
                    if valid {
                        self.answer(&request_id, answer)?;
                    } else {
                        self.notice("error", "That answer does not match the pending request");
                    }
                }
            }
            ConversationCommand::SelectModel(model) => {
                if !self.initialized
                    || self.active()
                    || self
                        .requests
                        .pending
                        .values()
                        .any(|request| matches!(request.kind, RequestKind::Model(_)))
                {
                    self.emit(ConversationEvent::Failed {
                        request_id: None,
                        message: "Model selection requires an idle, initialized conversation"
                            .into(),
                    });
                } else {
                    self.request(
                        json!({"type":"set_model","provider":model.provider,"modelId":model.id}),
                        RequestKind::Model(model),
                    )?;
                }
            }
            ConversationCommand::LoadHistory { cursor } => {
                if self.negotiated {
                    if self.models.as_ref().is_none_or(Vec::is_empty)
                        && !self
                            .requests
                            .pending
                            .values()
                            .any(|p| matches!(p.kind, RequestKind::Models))
                    {
                        self.request(json!({"type":"get_available_models"}), RequestKind::Models)?;
                    }
                    self.history(cursor)?;
                }
            }
            ConversationCommand::Submit {
                request_id,
                text,
                images,
            } => {
                let attempt = (|| -> Result<()> {
                    ensure!(self.initialized, "OMP has not initialized");
                    ensure!(
                        text.trim_start().split_whitespace().next() != Some("/__drostje_peer"),
                        "The peer transport command is private"
                    );
                    ensure!(
                        !self.submissions.contains(&request_id),
                        "This submission ID was already used; input is never replayed"
                    );
                    ensure!(!self.active(), "A conversation already has an active turn");
                    ensure!(
                        !self
                            .requests
                            .pending
                            .values()
                            .any(|p| matches!(p.kind, RequestKind::Model(_))),
                        "Wait for model selection to finish"
                    );
                    ensure!(
                        !text.trim().is_empty() || !images.is_empty(),
                        "A prompt needs text or images"
                    );
                    if !images.is_empty() {
                        ensure!(
                            self.models.as_ref().is_some_and(|models| models
                                .iter()
                                .any(|choice| Some(&choice.key) == self.model.as_ref()
                                    && choice.input.iter().any(|input| input == "image"))),
                            "Selected model does not advertise image input"
                        );
                    }
                    ensure!(
                        submission_size(request_id, &text, &images)? <= FRAME as u64,
                        "Prompt exceeds 1 MiB; explicitly optimize images or reduce the draft"
                    );
                    let mut frame = json!({"id":request_id,"type":"prompt","message":text});
                    if !images.is_empty() {
                        let blocks = images.iter().map(|attachment| -> Result<Value> {
                            let bytes = crate::attachments::read_original(&attachment.path)?;
                            let (_, mime, _) = crate::attachments::decode(&bytes)?;
                            ensure!(mime == attachment.mime, "Attachment bytes do not match its MIME type");
                            Ok(json!({"type":"image","mimeType":mime,"data":STANDARD.encode(bytes)}))
                        }).collect::<Result<Vec<_>>>()?;
                        frame["images"] = Value::Array(blocks);
                    }
                    self.request(frame, RequestKind::Prompt(request_id))?;
                    if self.submissions.len() == 128 {
                        self.submissions.pop_front();
                    }
                    self.submissions.push_back(request_id);
                    match IdleSleepAssertion::acquire("DROSTJE. conversation turn") {
                        Ok(assertion) => self.idle_sleep_assertion = Some(assertion),
                        Err(error) => self.notice(
                            "warning",
                            format!(
                                "Could not prevent idle sleep during this conversation turn: {error:#}. Keep your Mac awake manually until the turn finishes."
                            ),
                        ),
                    }
                    self.turn.begin(request_id);
                    self.state(ConversationState::Running);
                    Ok(())
                })();
                if let Err(error) = attempt {
                    self.emit(ConversationEvent::Failed {
                        request_id: Some(request_id),
                        message: format!("{error:#}"),
                    });
                }
            }
        }
        Ok(())
    }
    fn history(&mut self, cursor: Option<String>) -> Result<()> {
        if self.closing {
            return Ok(());
        }
        if cursor.is_none() {
            self.history_generation += 1;
            self.history_index = 0;
            self.emit(ConversationEvent::HistoryReset);
        }
        if self.active() {
            self.history_wait = true;
            return Ok(());
        }
        let mut value = if self.dialect == Dialect::Omp {
            json!({"type":"get_messages_page","limit":128})
        } else {
            json!({"type":"get_messages"})
        };
        if self.dialect == Dialect::Omp
            && let Some(cursor) = &cursor
        {
            value["cursor"] = json!(cursor);
        }
        self.history_wait = false;
        self.request(
            value,
            RequestKind::History {
                cursor,
                generation: self.history_generation,
            },
        )
    }
    fn finish(&mut self, outcome: TurnOutcome) -> Result<()> {
        if let Some(event) = self.turn.settle(outcome) {
            self.emit(event);
        }
        if self.turn.id.is_none() {
            self.turn = Turn::default();
        }
        self.cancel_dialogs(false)?;
        self.state(ConversationState::Ready);
        if self.closing {
            return Ok(());
        }
        if !self
            .requests
            .pending
            .values()
            .any(|request| matches!(request.kind, RequestKind::State))
        {
            self.request(json!({"type":"get_state"}), RequestKind::State)?;
        }
        if self.dialect == Dialect::Pi {
            self.request_commands()?;
        }
        self.history(None)
    }
    fn close(&mut self) -> Result<()> {
        if self.closing {
            return Ok(());
        }
        self.cancel_peers(
            true,
            "Conversation closing after possible peer dispatch; never replay",
        )?;
        self.cancel_dialogs(false)?;
        if self.active() {
            self.turn.interrupting = true;
            self.peers.interrupted = true;
            self.state(ConversationState::Interrupting);
            self.request(json!({"type":"abort"}), RequestKind::Abort)?;
        }
        self.closing = true;
        self.ready_deadline = Instant::now() + Duration::from_secs(5);
        self.child.deadline = Some(self.ready_deadline);
        Ok(())
    }
    fn disconnect(&mut self, message: String) {
        self.peer_disconnected(&message);
        let ids: Vec<_> = self.dialogs.keys().cloned().collect();
        self.emit(ConversationEvent::PeerDeliveryAvailable {
            available: false,
            reason: Some(message.clone()),
        });
        self.dialogs.clear();
        for request_id in ids {
            self.emit(ConversationEvent::UserRequestDismissed { request_id });
        }
        if let Some(event) = self.turn.settle(TurnOutcome::Unknown) {
            self.emit(event);
        }
        let pending = std::mem::take(&mut self.requests.pending);
        for (_, request) in pending {
            if matches!(request.kind, RequestKind::Peer(_)) {
                continue;
            }
            self.emit(ConversationEvent::Failed {
                request_id: match request.kind {
                    RequestKind::Prompt(id) => Some(id),
                    _ => None,
                },
                message: format!("{}: {message}", request.command),
            });
        }
        self.emit(ConversationEvent::Failed {
            request_id: None,
            message,
        });
        self.state(ConversationState::Disconnected);
    }
    fn tick(&mut self) -> Result<()> {
        self.peer_tick()?;
        let now = Instant::now();
        ensure!(
            self.negotiated || now < self.ready_deadline,
            "{} did not negotiate protocol v2 within 30 seconds",
            self.dialect.name()
        );
        let expired: Vec<_> = self
            .requests
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now && !matches!(p.kind, RequestKind::Peer(Some(_))))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            let pending = self.requests.pending.remove(&id).unwrap();
            match pending.kind {
                RequestKind::Prompt(request_id) => {
                    self.emit(ConversationEvent::Failed { request_id:Some(request_id), message:"Prompt acknowledgement timed out; outcome unknown. Do not resend automatically.".into() });
                    if let Some(event) = self.turn.settle(TurnOutcome::Unknown) {
                        self.emit(event);
                    }
                    bail!("OMP prompt acceptance is unknown; reopen to inspect durable history");
                }
                RequestKind::Negotiate | RequestKind::State if !self.initialized => {
                    bail!("OMP {} timed out", pending.command)
                }
                RequestKind::Models => {
                    self.models = Some(Vec::new());
                    self.emit(ConversationEvent::Failed {
                        request_id: None,
                        message: "Model discovery timed out; reload history to retry discovery"
                            .into(),
                    });
                    self.maybe_ready()?;
                }
                RequestKind::Commands => self.emit(ConversationEvent::CommandsUnavailable(
                    format!("{} did not list its commands in time", self.dialect.name()),
                )),
                RequestKind::Peer(None) => self.emit(ConversationEvent::PeerDeliveryAvailable {
                    available: false,
                    reason: Some("Native peer delivery handshake timed out".into()),
                }),
                _ => {
                    self.emit(ConversationEvent::Failed {
                        request_id: None,
                        message: format!("OMP {} timed out", pending.command),
                    });
                    self.maybe_ready()?;
                }
            }
        }
        let expired: Vec<_> = self
            .dialogs
            .iter()
            .filter(|(_, d)| d.deadline.is_some_and(|deadline| deadline <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.answer(&id, UserAnswer::Cancelled { timed_out: true })?;
        }
        if (self.history_wait || self.turn.interrupting)
            && now >= self.next_poll
            && !self
                .requests
                .pending
                .values()
                .any(|p| matches!(p.kind, RequestKind::State))
        {
            self.next_poll = now + Duration::from_secs(1);
            self.request(json!({"type":"get_state"}), RequestKind::State)?;
        }
        Ok(())
    }
    fn frame(&mut self, value: Value) -> Result<()> {
        match value["type"].as_str().unwrap_or("") {
            "ready" => {
                ensure!(
                    self.dialect == Dialect::Omp,
                    "Pi unexpectedly requested OMP protocol negotiation"
                );
                if self.closing {
                    return Ok(());
                }
                ensure!(
                    !self.negotiated
                        && !self
                            .requests
                            .pending
                            .values()
                            .any(|p| matches!(p.kind, RequestKind::Negotiate)),
                    "Duplicate OMP ready frame"
                );
                ensure!(
                    value["supportedProtocolVersions"]
                        .as_array()
                        .is_some_and(|versions| versions.iter().any(|v| v.as_u64() == Some(2))),
                    "OMP does not support lossless protocol v2"
                );
                ensure!(
                    value["maxFrameBytes"].as_u64() == Some(FRAME as u64)
                        && value["maxReassembledFrameBytes"].as_u64() == Some(LOGICAL as u64),
                    "OMP advertised incompatible transport budgets"
                );
                self.request(
                    json!({"type":"negotiate_protocol","protocolVersion":2}),
                    RequestKind::Negotiate,
                )?;
            }
            "response" => self.response(value)?,
            "agent_start" | "turn_start" => {
                if self.turn.interrupting {
                    self.state(ConversationState::Interrupting);
                } else if self.dialogs.is_empty() {
                    self.state(ConversationState::Running);
                }
            }
            "agent_end" => {
                if let Some(messages) = value["messages"].as_array() {
                    for raw in messages {
                        self.sequence += 1;
                        let message = normalize_message(
                            raw,
                            &format!("terminal-{}", self.sequence),
                            &self.config.session_dir,
                            &mut self.images,
                        );
                        if raw["stopReason"] == "error" {
                            self.turn.failed = true;
                            self.peers.failed = true;
                        }
                        if raw["stopReason"] == "aborted" {
                            self.turn.interrupting = true;
                            self.peers.interrupted = true;
                        }
                        self.emit(ConversationEvent::MessageUpdated(message));
                    }
                }
                if self.dialect == Dialect::Omp
                    && self.turn.id.is_some()
                    && let Some(outcome) = self.turn.agent_end(&value)
                {
                    self.finish(outcome)?;
                }
            }
            "agent_settled" if self.dialect == Dialect::Pi => {
                let peer_turn = self.peer_settled();
                let outcome = if self.turn.interrupting {
                    TurnOutcome::Interrupted
                } else if self.turn.failed {
                    TurnOutcome::Failed
                } else {
                    TurnOutcome::Completed
                };
                if self.turn.id.is_some() || peer_turn {
                    self.finish(outcome)?;
                }
            }
            "session_settled" if self.dialect == Dialect::Omp => {
                if self.peer_settled() {
                    let outcome = if self.turn.interrupting {
                        TurnOutcome::Interrupted
                    } else if self.turn.failed {
                        TurnOutcome::Failed
                    } else {
                        TurnOutcome::Completed
                    };
                    self.finish(outcome)?;
                }
            }
            "prompt_result" => {
                if let Some(outcome) = self.turn.prompt_result(&value) {
                    self.finish(outcome)?;
                }
            }
            "message_start" | "message_update" | "message_end" => {
                let kind = value["type"].as_str().unwrap();
                let raw = value
                    .get("message")
                    .or_else(|| value["assistantMessageEvent"].get("partial"));
                if let Some(raw) = raw {
                    if kind == "message_start" || kind == "message_end" {
                        self.peer_message(raw);
                    }
                    if peer::messages(raw).is_some() {
                        self.emit(ConversationEvent::Diagnostic {
                            label: "Native peer message lifecycle".into(),
                            raw: value.clone(),
                        });
                    }
                    if kind == "message_start" || self.current_message.is_none() {
                        self.sequence += 1;
                        self.current_message =
                            Some(message_id(raw, &format!("live-{}", self.sequence)));
                    }
                    let message = normalize_message(
                        raw,
                        self.current_message.as_deref().unwrap(),
                        &self.config.session_dir,
                        &mut self.images,
                    );
                    if raw["stopReason"] == "error" {
                        self.turn.failed = true;
                        self.peers.failed = true;
                    }
                    if raw["stopReason"] == "aborted" {
                        self.turn.interrupting = true;
                        self.peers.interrupted = true;
                    }
                    self.emit(ConversationEvent::MessageUpdated(message));
                    if kind == "message_end" {
                        self.current_message = None;
                    }
                } else if !(self.dialect == Dialect::Pi && kind == "message_update") {
                    self.emit(ConversationEvent::Diagnostic {
                        label: "Message event without canonical snapshot".into(),
                        raw: value,
                    });
                }
            }
            "tool_execution_start" | "tool_execution_update" | "tool_execution_end" => {
                self.tool(&value)
            }
            "extension_ui_request" => self.extension(value)?,
            "command_output" => {
                self.sequence += 1;
                let message = Message {
                    id: format!("command-{}", self.sequence),
                    role: "system".into(),
                    content: vec![ContentBlock::Text(string(&value, "text"))],
                    raw: None,
                    tool_call_id: None,
                    error: false,
                    exit_code: None,
                };
                if self.local_outputs.len() == 128 {
                    self.local_outputs.pop_front();
                }
                self.local_outputs.push_back(message.clone());
                self.emit(ConversationEvent::MessageUpdated(message));
            }
            "model_changed" => {
                if let Ok(model) = model_key(&value["model"]) {
                    self.model = Some(model.clone());
                    self.emit(ConversationEvent::ModelSelected(model));
                } else {
                    self.emit(ConversationEvent::Diagnostic {
                        label: "Model change".into(),
                        raw: value,
                    });
                }
            }
            "available_commands_update" if self.dialect == Dialect::Omp => {
                self.commands(&value["commands"])
            }
            "session_info_update" => {
                self.request(json!({"type":"get_state"}), RequestKind::State)?;
            }
            "notice" => self.notice(
                value["level"].as_str().unwrap_or("info"),
                value["message"]
                    .as_str()
                    .or(value["text"].as_str())
                    .unwrap_or_else(|| self.dialect.name()),
            ),
            "extension_error" => {
                if value["event"] == "send_message" {
                    self.peer_dispatch_error(&string(&value, "error"));
                    self.notice("warning", "Native custom-message dispatch failed; unconfirmed peer outcomes are unknown and will not be replayed");
                }
                self.notice(
                    "error",
                    format!(
                        "Extension {}: {}",
                        string(&value, "extensionPath"),
                        string(&value, "error")
                    ),
                );
                self.emit(ConversationEvent::Diagnostic {
                    label: "Extension error".into(),
                    raw: value,
                });
            }
            "rpc_frame_error" => bail!(
                "{} could not transmit a complete frame: {}",
                self.dialect.name(),
                string(&value, "error")
            ),
            _ => self.emit(ConversationEvent::Diagnostic {
                label: value["type"]
                    .as_str()
                    .unwrap_or("Unknown Pi-family payload")
                    .into(),
                raw: value,
            }),
        }
        Ok(())
    }
    fn response(&mut self, value: Value) -> Result<()> {
        let pending = self.requests.take(&value)?;
        let id = value["id"].as_str().and_then(|id| Uuid::parse_str(id).ok());
        // Prompt scheduling may fail after its successful ID-correlated acknowledgement.
        if pending.is_none() {
            if let Some(event) = self.turn.late_failure(&value) {
                self.emit(ConversationEvent::Failed {
                    request_id: id,
                    message: string(&value, "error"),
                });
                self.emit(event);
                return self.finish(TurnOutcome::Failed);
            }
        }
        if pending.is_none()
            && value["command"] == "prompt"
            && value["success"] == false
            && id.is_some_and(|id| self.submissions.contains(&id))
        {
            self.emit(ConversationEvent::Failed {
                request_id: id,
                message: string(&value, "error"),
            });
            if id == self.turn.id {
                return self.finish(TurnOutcome::Failed);
            }
            self.emit(ConversationEvent::TurnSettled {
                request_id: id.unwrap(),
                outcome: TurnOutcome::Failed,
            });
            return Ok(());
        }
        let Some(pending) = pending else {
            self.emit(ConversationEvent::Diagnostic {
                label: "Unmatched RPC response".into(),
                raw: value,
            });
            return Ok(());
        };
        ensure!(
            value["success"].is_boolean(),
            "OMP response has no success flag"
        );
        if value["success"] == false {
            match pending.kind {
                RequestKind::Peer(attempt) => {
                    if let Some(attempt) = attempt {
                        self.peer_outcome(
                            attempt,
                            PeerDeliveryOutcome::Unknown(string(&value, "error")),
                        );
                        if self.peers.idle == Some(attempt) {
                            self.peers.idle = None;
                        }
                    } else {
                        self.emit(ConversationEvent::PeerDeliveryAvailable {
                            available: false,
                            reason: Some(format!(
                                "Native peer bridge failed: {}",
                                string(&value, "error")
                            )),
                        });
                    }
                }
                RequestKind::History { generation, .. }
                    if generation != self.history_generation => {}
                RequestKind::History { generation, .. }
                    if generation == self.history_generation
                        && (value["code"] == "session_busy" || value["code"] == "stale_cursor") =>
                {
                    self.history_generation += 1;
                    self.history_index = 0;
                    self.history_wait = true;
                    self.emit(ConversationEvent::HistoryReset);
                    self.next_poll = Instant::now() + Duration::from_millis(500);
                }
                RequestKind::Prompt(request_id) => {
                    self.emit(ConversationEvent::Failed {
                        request_id: Some(request_id),
                        message: string(&value, "error"),
                    });
                    if self.turn.id == Some(request_id) {
                        self.finish(TurnOutcome::Failed)?;
                    }
                }
                RequestKind::Negotiate => {
                    bail!("OMP refused protocol v2: {}", string(&value, "error"))
                }
                RequestKind::Commands => self.emit(ConversationEvent::CommandsUnavailable(string(
                    &value, "error",
                ))),
                RequestKind::State if !self.initialized => {
                    bail!("OMP initialization failed: {}", string(&value, "error"))
                }
                RequestKind::Models => {
                    self.models = Some(Vec::new());
                    self.emit(ConversationEvent::Failed {
                        request_id: None,
                        message: format!(
                            "Model discovery: {}. Reload history to retry.",
                            string(&value, "error")
                        ),
                    });
                    self.maybe_ready()?;
                }
                _ => {
                    self.emit(ConversationEvent::Failed {
                        request_id: None,
                        message: format!("{}: {}", pending.command, string(&value, "error")),
                    });
                    self.maybe_ready()?;
                }
            }
            return Ok(());
        }
        let data = &value["data"];
        match pending.kind {
            RequestKind::Negotiate => {
                ensure!(
                    data["protocolVersion"] == 2,
                    "OMP negotiated an unsupported protocol"
                );
                self.negotiated = true;
                if self.closing {
                    return Ok(());
                }
                self.request(json!({"type":"get_state"}), RequestKind::State)?;
                self.request(json!({"type":"get_available_models"}), RequestKind::Models)?;
                self.request_commands()?;
            }
            RequestKind::State => {
                let session_id = data["sessionId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .with_context(|| format!("{} returned no session ID", self.dialect.name()))?;
                if let Some(saved) = &self.config.session_ref {
                    let saved: Value = serde_json::from_str(saved)?;
                    ensure!(
                        saved["id"] == session_id,
                        "{} resumed a different session; refusing to replace saved history",
                        self.dialect.name()
                    );
                }
                self.session_ref = Some(serde_json::to_string(
                    &json!({"id":session_id,"file":data.get("sessionFile"),"cwd":self.config.cwd,"session_dir":self.config.session_dir}),
                )?);
                if let Ok(model) = model_key(&data["model"]) {
                    if self.model.as_ref() != Some(&model) {
                        self.model = Some(model.clone());
                        self.emit(ConversationEvent::ModelSelected(model));
                    }
                }
                let busy = data["isStreaming"] == true || data["isCompacting"] == true;
                if busy && !self.active() {
                    self.state(ConversationState::Running);
                }
                if !busy && self.turn.interrupting {
                    self.finish(TurnOutcome::Interrupted)?;
                } else if !busy && self.history_wait && self.turn.id.is_none() {
                    self.state(ConversationState::Ready);
                    self.history(None)?;
                }
                self.maybe_ready()?;
            }
            RequestKind::Models => {
                let values = data["models"].as_array().with_context(|| {
                    format!(
                        "{} model discovery returned no catalog",
                        self.dialect.name()
                    )
                })?;
                self.models = Some(
                    values
                        .iter()
                        .map(|value| {
                            Ok(ModelChoice {
                                key: model_key(value)?,
                                name: string(value, "name"),
                                efforts: Vec::new(),
                                input: value["input"]
                                    .as_array()
                                    .map(|a| {
                                        a.iter()
                                            .filter_map(Value::as_str)
                                            .map(str::to_owned)
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                );
                self.maybe_ready()?;
            }
            RequestKind::Model(expected) => {
                let model = model_key(data)?;
                ensure!(
                    model == expected,
                    "OMP selected a different model than requested"
                );
                self.model = Some(model.clone());
                self.emit(ConversationEvent::ModelSelected(model));
                self.maybe_ready()?;
            }
            RequestKind::History { generation, cursor } => {
                if generation == self.history_generation {
                    let values = data["messages"].as_array().with_context(|| {
                        format!("{} history has no messages", self.dialect.name())
                    })?;
                    let mut messages: Vec<_> = values
                        .iter()
                        .enumerate()
                        .map(|(index, value)| {
                            normalize_message(
                                value,
                                &format!("history-{}", self.history_index + index),
                                &self.config.session_dir,
                                &mut self.images,
                            )
                        })
                        .collect();
                    self.history_index += values.len();
                    let next_cursor = (self.dialect == Dialect::Omp)
                        .then(|| data["nextCursor"].as_str().map(str::to_owned))
                        .flatten();
                    ensure!(
                        next_cursor.is_none() || next_cursor != cursor,
                        "{} history cursor did not advance",
                        self.dialect.name()
                    );
                    if next_cursor.is_none() {
                        messages.extend(self.local_outputs.iter().cloned());
                    }
                    self.emit(ConversationEvent::History {
                        messages,
                        next_cursor,
                    });
                }
            }
            RequestKind::Prompt(request_id) => {
                if let Some(event) = self.turn.accept(request_id) {
                    self.emit(event);
                }
                if self.turn.id == Some(request_id) {
                    if let Some(outcome) = self.turn.terminal.take() {
                        self.finish(outcome)?;
                    }
                }
                if data["agentInvoked"] == false && self.turn.id == Some(request_id) {
                    self.finish(TurnOutcome::Completed)?;
                }
            }
            RequestKind::Commands => self.commands(&data["commands"]),
            RequestKind::Peer(_) => {} // RPC command handling is not a context receipt.
            RequestKind::Abort => {
                // Abort acknowledgement alone does not prove that the agent stopped.
                self.next_poll = Instant::now();
                self.history_wait = true;
            }
        }
        Ok(())
    }
    fn maybe_ready(&mut self) -> Result<()> {
        if self.closing {
            return Ok(());
        }
        if self.session_ref.is_none() || self.models.is_none() {
            return Ok(());
        }
        if let Some(model) = self.initial_model.take() {
            if !self.active() {
                self.request(
                    json!({"type":"set_model","provider":model.provider,"modelId":model.id}),
                    RequestKind::Model(model),
                )?;
                return Ok(());
            }
        }
        if self
            .requests
            .pending
            .values()
            .any(|pending| matches!(pending.kind, RequestKind::Model(_)))
        {
            return Ok(());
        }
        self.emit(ConversationEvent::Ready {
            session_ref: self.session_ref.clone().unwrap(),
            models: self.models.clone().unwrap(),
            peer_delivery: self.peers.ready,
        });
        if !self.initialized {
            self.initialized = true;
            self.notice(
                "info",
                format!(
                    "{} runs with your local user privileges and configured tool policy, not Factory's Docker sandbox.",
                    self.dialect.name()
                ),
            );
            if !self.active() {
                self.state(ConversationState::Ready);
            }
            self.history(None)?;
        }
        Ok(())
    }
    fn tool(&mut self, value: &Value) {
        let id = string(value, "toolCallId");
        if id.is_empty() {
            self.emit(ConversationEvent::Diagnostic {
                label: "Tool event without ID".into(),
                raw: value.clone(),
            });
            return;
        }
        let existing = self.tools.remove(&id);
        let mut tool = existing.unwrap_or_else(|| ToolActivity {
            id: id.clone(),
            name: string(value, "toolName"),
            intent: None,
            input: Value::Null,
            result: Vec::new(),
            error: false,
            exit_code: None,
            complete: false,
            raw: None,
        });
        if let Some(args) = value.get("args") {
            tool.input = args.clone();
            tool.intent = intent(value, args);
        }
        if let Some(name) = value["toolName"].as_str() {
            tool.name = name.into();
        }
        if let Some(result) = value.get("result").or_else(|| value.get("partialResult")) {
            let (content, raw) = normalize_content(
                result.get("content").unwrap_or(result),
                &self.config.session_dir,
                &mut self.images,
            );
            tool.result = content;
            tool.raw = raw;
            if let Some(details) = result.get("details") {
                tool.exit_code = details["exitCode"].as_i64();
                tool.raw = Some(json!({"unrecognizedContent":tool.raw,"details":details}));
            }
        }
        tool.error = value["isError"] == true;
        tool.complete = value["type"] == "tool_execution_end";
        if !tool.complete {
            self.tools.insert(id, tool.clone());
        }
        self.emit(ConversationEvent::ToolUpdated(tool));
    }
    fn extension(&mut self, value: Value) -> Result<()> {
        if self.peer_status(&value)? {
            return Ok(());
        }
        if self.closing {
            return Ok(());
        }
        let id = string(&value, "id");
        let method = string(&value, "method");
        match method.as_str() {
            "cancel" => {
                let target = string(&value, "targetId");
                if let Some(event) = cancel_target(&mut self.dialogs, &target) {
                    self.emit(event);
                    self.dialog_state();
                }
            }
            "confirm" | "select" | "input" | "editor" => {
                ensure!(!id.is_empty(), "OMP dialog has no request ID");
                if self.dialogs.contains_key(&id) {
                    self.answer(&id, UserAnswer::Cancelled { timed_out: false })?;
                }
                let duration = value["timeout"]
                    .as_f64()
                    .filter(|ms| ms.is_finite() && *ms >= 0.0)
                    .map(|ms| Duration::from_secs_f64((ms / 1000.0).min(315_360_000.0)));
                let deadline = duration.and_then(|duration| Instant::now().checked_add(duration));
                let request = UserRequest {
                    id: id.clone(),
                    kind: match method.as_str() {
                        "confirm" => UserRequestKind::Confirm,
                        "select" => UserRequestKind::Select,
                        "input" => UserRequestKind::Input,
                        _ => UserRequestKind::Editor,
                    },
                    title: string(&value, "title"),
                    options: value["options"]
                        .as_array()
                        .map(|options| {
                            options
                                .iter()
                                .enumerate()
                                .filter_map(|(index, option)| {
                                    option.as_str().map(|label| UserOption {
                                        value: label.into(),
                                        description: value["optionDetails"][index]["description"]
                                            .as_str()
                                            .map(str::to_owned),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    text: value["message"]
                        .as_str()
                        .or(value["prefill"].as_str())
                        .unwrap_or("")
                        .into(),
                    placeholder: value["placeholder"].as_str().map(str::to_owned),
                    deadline: duration.and_then(|duration| SystemTime::now().checked_add(duration)),
                };
                self.dialogs.insert(
                    id.clone(),
                    Dialog {
                        request: request.clone(),
                        deadline,
                    },
                );
                if request.kind == UserRequestKind::Select && request.options.is_empty() {
                    self.notice(
                        "warning",
                        "OMP requested a selection without options; request cancelled",
                    );
                    self.answer(&id, UserAnswer::Cancelled { timed_out: false })?;
                } else {
                    self.emit(ConversationEvent::UserRequest(request));
                    self.state(ConversationState::WaitingForInput);
                }
            }
            "notify" => self.notice(
                value["notifyType"].as_str().unwrap_or("info"),
                string(&value, "message"),
            ),
            "setStatus" => self.emit(ConversationEvent::StatusUpdated {
                key: string(&value, "statusKey"),
                text: value["statusText"].as_str().map(str::to_owned),
            }),
            "setWidget" => self.emit(ConversationEvent::WidgetUpdated {
                key: string(&value, "widgetKey"),
                lines: value["widgetLines"]
                    .as_array()
                    .map(|lines| {
                        lines
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
            "setTitle" => self.emit(ConversationEvent::TitleChanged(string(&value, "title"))),
            "set_editor_text" => self.emit(ConversationEvent::EditorTextRequested(string(
                &value, "text",
            ))),
            "open_url" => {
                let url = string(&value, "url");
                if safe_web_url(&url) {
                    self.emit(ConversationEvent::LinkOffered {
                        url,
                        instructions: value["instructions"].as_str().map(str::to_owned),
                    });
                } else {
                    self.notice(
                        "warning",
                        "OMP offered an unsupported URL scheme; nothing was opened",
                    );
                    self.emit(ConversationEvent::Diagnostic {
                        label: "Unsupported URL".into(),
                        raw: value,
                    });
                }
            }
            _ => {
                self.notice(
                    "warning",
                    format!("Unsupported OMP interactive method {method}; cancelled"),
                );
                if !id.is_empty() {
                    self.queue(answer_frame(
                        &id,
                        UserAnswer::Cancelled { timed_out: false },
                    ))?;
                }
                self.emit(ConversationEvent::Diagnostic {
                    label: "Unsupported extension request".into(),
                    raw: value,
                });
            }
        }
        Ok(())
    }
    fn dialog_state(&mut self) {
        if self.dialogs.is_empty() {
            if self.turn.interrupting {
                self.state(ConversationState::Interrupting);
            } else if self.turn.id.is_some() {
                self.state(ConversationState::Running);
            } else if self.initialized {
                self.state(ConversationState::Ready);
            }
        }
    }
    fn answer(&mut self, id: &str, answer: UserAnswer) -> Result<()> {
        if let Some(dialog) = self.dialogs.remove(id) {
            let answer = deadline_answer(answer, dialog.deadline, Instant::now());
            self.queue(answer_frame(id, answer))?;
            self.emit(ConversationEvent::UserRequestDismissed {
                request_id: id.into(),
            });
            self.dialog_state();
        }
        Ok(())
    }
    fn cancel_dialogs(&mut self, timed_out: bool) -> Result<()> {
        let ids: Vec<_> = self.dialogs.keys().cloned().collect();
        for id in ids {
            self.answer(&id, UserAnswer::Cancelled { timed_out })?;
        }
        Ok(())
    }
}

fn answer_frame(id: &str, answer: UserAnswer) -> Value {
    match answer {
        UserAnswer::Value(value) => json!({"type":"extension_ui_response","id":id,"value":value}),
        UserAnswer::Confirmed(confirmed) => {
            json!({"type":"extension_ui_response","id":id,"confirmed":confirmed})
        }
        // Claude-only permission semantics must never approve an OMP dialog.
        UserAnswer::PermissionAuto => {
            json!({"type":"extension_ui_response","id":id,"cancelled":true})
        }
        UserAnswer::Cancelled { timed_out: false } => {
            json!({"type":"extension_ui_response","id":id,"cancelled":true})
        }
        UserAnswer::Cancelled { timed_out: true } => {
            json!({"type":"extension_ui_response","id":id,"cancelled":true,"timedOut":true})
        }
    }
}
fn safe_web_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && !url.chars().any(char::is_control)
        && url
            .split_once("://")
            .is_some_and(|(_, rest)| !rest.is_empty() && !rest.starts_with('/'))
}
/// OMP entries are `{name, aliases?, description, input?: {hint}, source}`; Pi entries are
/// `{name, description?, source}` plus location details DROSTJE. does not need. Entries
/// without a name are returned separately so they can be shown as a diagnostic.
fn parse_commands(dialect: Dialect, raw: &Value) -> Result<(Vec<SlashCommand>, Vec<Value>)> {
    let entries = raw
        .as_array()
        .with_context(|| format!("{} returned no command list", dialect.name()))?;
    let mut commands = Vec::new();
    let mut rejected = Vec::new();
    for entry in entries {
        let Some(name) = entry["name"]
            .as_str()
            .map(|name| name.trim_start_matches('/'))
            .filter(|name| !name.is_empty())
        else {
            rejected.push(entry.clone());
            continue;
        };
        if name == peer::COMMAND {
            continue;
        }
        let source = match entry["source"].as_str().unwrap_or("") {
            "builtin" => CommandSource::Builtin,
            "skill" => CommandSource::Skill,
            "extension" => CommandSource::Extension,
            "custom" => CommandSource::Custom,
            "prompt" => CommandSource::Template,
            "file" => CommandSource::File,
            "mcp_prompt" => CommandSource::McpPrompt,
            other => CommandSource::Other(other.to_owned()),
        };
        let blocked = (dialect == Dialect::Omp && source == CommandSource::Builtin)
            .then(|| omp_blocked(name))
            .flatten();
        commands.push(SlashCommand {
            name: name.to_owned(),
            aliases: entry["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            description: string(entry, "description"),
            argument_hint: entry["input"]["hint"]
                .as_str()
                .filter(|hint| !hint.is_empty())
                .map(str::to_owned),
            source,
            blocked,
        });
    }
    Ok((commands, rejected))
}
/// OMP builtins that replace the model, session or directory DROSTJE. tracks.
fn omp_blocked(name: &str) -> Option<&'static str> {
    match name {
        "model" | "switch" => Some(super::commands::MODEL_PICKER),
        "session" => Some("session management belongs to the Conversation"),
        "move" | "wt" | "add-dir" | "remove-dir" => {
            Some("DROSTJE. owns the Conversation's directory and worktree")
        }
        _ => None,
    }
}
fn string(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_owned()
}
fn model_key(value: &Value) -> Result<ModelKey> {
    Ok(ModelKey {
        provider: value["provider"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("OMP model has no provider")?
            .into(),
        id: value["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("OMP model has no ID")?
            .into(),
    })
}
fn message_id(value: &Value, fallback: &str) -> String {
    if let Some(timestamp) = value
        .get("timestamp")
        .filter(|value| value.is_number() || value.is_string())
    {
        format!(
            "{}:{}:{}",
            string(value, "role"),
            timestamp,
            value["toolCallId"].as_str().unwrap_or("")
        )
    } else {
        fallback.into()
    }
}
fn normalize_message(
    value: &Value,
    fallback: &str,
    directory: &std::path::Path,
    images: &mut ImageCache,
) -> Message {
    if let Some((_, messages)) = peer::messages(value) {
        return Message {
            id: message_id(value, fallback),
            role: "peer".into(),
            content: messages
                .into_iter()
                .map(ContentBlock::PeerMessage)
                .collect(),
            raw: Some(value.clone()),
            tool_call_id: None,
            error: false,
            exit_code: None,
        };
    }
    let (mut content, mut raw) = normalize_content(&value["content"], directory, images);
    if value["customType"] == "drostje-peer" {
        raw = Some(value.clone());
    }
    if let Some(error) = value["errorMessage"].as_str() {
        content.push(ContentBlock::Text(format!("Provider error: {error}")));
    }
    let role = value["role"].as_str().unwrap_or("unknown").to_owned();
    if !matches!(
        role.as_str(),
        "user" | "assistant" | "toolResult" | "system" | "custom"
    ) {
        raw = Some(value.clone());
    }
    let tool_call_id = value["toolCallId"]
        .as_str()
        .filter(|id| !id.is_empty() && role == "toolResult")
        .map(str::to_owned);
    Message {
        id: message_id(value, fallback),
        role,
        content,
        raw,
        tool_call_id,
        error: value["isError"] == true,
        exit_code: value["details"]["exitCode"].as_i64(),
    }
}
/// OMP sends the purpose line as `intent` and, for most tools, also as the `i` argument.
fn intent(block: &Value, arguments: &Value) -> Option<String> {
    block["intent"]
        .as_str()
        .or_else(|| arguments["i"].as_str())
        .filter(|intent| !intent.trim().is_empty())
        .map(str::to_owned)
}
fn normalize_content(
    value: &Value,
    directory: &std::path::Path,
    images: &mut ImageCache,
) -> (Vec<ContentBlock>, Option<Value>) {
    if let Some(text) = value.as_str() {
        return (vec![ContentBlock::Text(text.into())], None);
    }
    let Some(blocks) = value.as_array() else {
        return (Vec::new(), (!value.is_null()).then(|| value.clone()));
    };
    let mut content = Vec::with_capacity(blocks.len());
    let mut unknown = Vec::new();
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => content.push(ContentBlock::Text(string(block, "text"))),
            Some("thinking") => content.push(ContentBlock::Thinking(string(block, "thinking"))),
            Some("toolCall") if block["id"].as_str().is_some_and(|id| !id.is_empty()) => {
                let arguments = block.get("arguments").cloned().unwrap_or(Value::Null);
                content.push(ContentBlock::ToolCall {
                    id: string(block, "id"),
                    name: string(block, "name"),
                    intent: intent(block, &arguments),
                    input: arguments,
                });
            }
            Some("image") => {
                let image = block["mimeType"]
                    .as_str()
                    .context("Provider image has no MIME type")
                    .and_then(|mime| {
                        let encoded = block["data"]
                            .as_str()
                            .context("Provider image has no resolved byte data")?;
                        images.import(mime, encoded, directory)
                    });
                match image {
                    Ok(image) => content.push(ContentBlock::Image(image)),
                    Err(error) => content.push(ContentBlock::ImageError(format!("{error:#}"))),
                }
            }
            _ => unknown.push(block.clone()),
        }
    }
    (
        content,
        (!unknown.is_empty()).then_some(Value::Array(unknown)),
    )
}

fn deadline_answer(answer: UserAnswer, deadline: Option<Instant>, now: Instant) -> UserAnswer {
    if deadline.is_some_and(|deadline| deadline <= now) {
        UserAnswer::Cancelled { timed_out: true }
    } else {
        answer
    }
}

fn cancel_target(dialogs: &mut HashMap<String, Dialog>, target: &str) -> Option<ConversationEvent> {
    dialogs
        .remove(target)
        .map(|_| ConversationEvent::UserRequestDismissed {
            request_id: target.into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(bytes: &[u8]) -> Vec<Value> {
        let count = bytes.len().div_ceil(CHUNK);
        bytes.chunks(CHUNK).enumerate().map(|(index,part)| json!({
            "type":"rpc_chunk","chunkId":"test","index":index,"count":count,"byteLength":bytes.len(),"data":STANDARD.encode(part)
        })).collect()
    }
    fn large_frame() -> Vec<u8> {
        serde_json::to_vec(&json!({"type":"message_end","text":"界".repeat(FRAME / 3 + 32)}))
            .unwrap()
    }
    fn pending(kind: RequestKind, command: &str) -> Pending {
        Pending {
            kind,
            command: command.into(),
            deadline: Instant::now() + CONTROL_TIMEOUT,
        }
    }

    fn next_event(events: &async_channel::Receiver<ConversationEvent>) -> ConversationEvent {
        smol::block_on(smol::future::race(
            async {
                events
                    .recv()
                    .await
                    .map_err(|_| anyhow::anyhow!("Pi fixture event channel closed"))
            },
            async {
                smol::Timer::after(Duration::from_secs(3)).await;
                bail!("Pi fixture timed out")
            },
        ))
        .unwrap()
    }

    #[test]
    fn normal_pi_uses_unextended_rpc_and_settles_after_agent_settled() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("drostje-normal-pi-{}", Uuid::new_v4()));
        let cwd = root.join("repo");
        let session_dir = root.join("sessions");
        fs::create_dir_all(&cwd)?;
        let executable = root.join("pi");
        fs::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$@" > "$(dirname "$0")/args"
session_dir=
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--session-dir" ]; then session_dir=$2; shift 2; else shift; fi
done
mkdir -p "$session_dir"
session_file="$session_dir/session.jsonl"
: > "$session_file"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed 's/.*"id":"\([^"]*\)".*/\1/')
  case "$line" in
    *'"type":"get_state"'*)
      printf '{"id":"%s","type":"response","command":"get_state","success":true,"data":{"sessionId":"pi-session","sessionFile":"%s","model":{"provider":"test","id":"model"},"isStreaming":false,"isCompacting":false}}\n' "$id" "$session_file"
      ;;
    *'"type":"get_available_models"'*)
      printf '{"id":"%s","type":"response","command":"get_available_models","success":true,"data":{"models":[{"provider":"test","id":"model","name":"Test model","input":["text"]}]}}\n' "$id"
      ;;
    *'"type":"get_messages"'*)
      printf '{"id":"%s","type":"response","command":"get_messages","success":true,"data":{"messages":[]}}\n' "$id"
      ;;
    *'"type":"prompt"'*)
      printf '{"id":"%s","type":"response","command":"prompt","success":true}\n' "$id"
      printf '{"type":"message_start","message":{"role":"assistant","timestamp":1,"content":[]}}\n'
      printf '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"Hello"}}\n'
      printf '{"type":"message_end","message":{"role":"assistant","timestamp":1,"content":[{"type":"text","text":"Hello from Pi"}],"stopReason":"stop"}}\n'
      printf '{"type":"agent_end","messages":[],"willRetry":false}\n'
      printf '{"type":"agent_settled"}\n'
      ;;
    *'"type":"abort"'*)
      printf '{"id":"%s","type":"response","command":"abort","success":true}\n' "$id"
      ;;
  esac
done
"#,
        )?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
        let handle = smol::block_on(open(ConversationConfig {
            harness: "pi".into(),
            executable,
            cwd,
            session_dir,
            session_ref: None,
            model: None,
            effort: None,
            permission_mode: None,
            browser_tools: None,
            allow_empty_session: true,
        }))?;

        let mut ready = false;
        for _ in 0..12 {
            if matches!(next_event(&handle.events), ConversationEvent::Ready { .. }) {
                ready = true;
                break;
            }
        }
        ensure!(ready, "Pi fixture did not become ready");

        let request_id = Uuid::new_v4();
        handle.commands.send_blocking(ConversationCommand::Submit {
            request_id,
            text: "Hello".into(),
            images: Vec::new(),
        })?;
        let mut accepted = false;
        let mut message = false;
        let mut settled = false;
        for _ in 0..20 {
            match next_event(&handle.events) {
                ConversationEvent::SubmissionAccepted { request_id: id } if id == request_id => {
                    accepted = true
                }
                ConversationEvent::MessageUpdated(message_value)
                    if matches!(
                        &message_value.content[..],
                        [ContentBlock::Text(text)] if text == "Hello from Pi"
                    ) =>
                {
                    message = true
                }
                ConversationEvent::TurnSettled {
                    request_id: id,
                    outcome: TurnOutcome::Completed,
                } if id == request_id => {
                    settled = true;
                    break;
                }
                _ => {}
            }
        }
        handle.commands.send_blocking(ConversationCommand::Close)?;
        let args = fs::read_to_string(root.join("args"))?;
        assert!(args.contains("--mode\nrpc\n"));
        assert!(!args.contains("negotiate_protocol"));
        assert!(accepted && message && settled);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn idle_sleep_protection_follows_owned_turn_lifecycle() -> Result<()> {
        const CHILD: &str = "DROSTJE_OMP_POWER_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "harness::omp::tests::idle_sleep_protection_follows_owned_turn_lifecycle",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()?;
            ensure!(
                output.status.success(),
                "Isolated OMP power test failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }

        fn assertions() -> Result<Vec<String>> {
            let output = Command::new("/usr/bin/pmset")
                .args(["-g", "assertions"])
                .output()?;
            ensure!(
                output.status.success(),
                "Could not inspect power assertions"
            );
            let pid = format!("pid {}(", std::process::id());
            String::from_utf8(output.stdout)?
                .lines()
                .filter(|line| {
                    line.trim_start().starts_with(&pid)
                        && line.contains("PreventUserIdleSystemSleep")
                        && line.contains("named: \"DROSTJE. conversation turn\"")
                })
                .map(|line| {
                    line.split_whitespace()
                        .find(|part| part.starts_with("[0x"))
                        .map(str::to_owned)
                        .context("Power assertion omitted its identifier")
                })
                .collect()
        }

        let root = std::env::temp_dir().join(format!("drostje-omp-power-{}", Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        let mut config = ConversationConfig {
            harness: "omp".into(),
            executable: "/usr/bin/true".into(),
            cwd: root.clone(),
            session_dir: root.join("session"),
            session_ref: None,
            model: None,
            effort: None,
            permission_mode: None,
            browser_tools: None,
            allow_empty_session: true,
        };
        // Drive the actual actor handlers; this child requires no external agent or credentials.
        let (child, _) = prepare(&mut config, Dialect::Omp)?;
        let (_commands, input) = async_channel::bounded(64);
        let (output, _events) = async_channel::bounded(128);
        let mut actor = Actor::new(config, child, input, output, Dialect::Omp);
        actor.initialized = true;
        actor.negotiated = true;
        actor.state(ConversationState::Ready);
        assert!(assertions()?.is_empty());

        let submit = |actor: &mut Actor| -> Result<Uuid> {
            let request_id = Uuid::new_v4();
            actor.command(ConversationCommand::Submit {
                request_id,
                text: "Check lifecycle".into(),
                images: Vec::new(),
            })?;
            Ok(request_id)
        };
        let id = submit(&mut actor)?;
        let active = assertions()?;
        assert_eq!(active.len(), 1);
        actor.frame(json!({"type":"agent_end","isTerminal":false}))?;
        assert_eq!(
            assertions()?,
            active,
            "Continuation must retain the same assertion"
        );
        actor.frame(json!({"type":"agent_end","isTerminal":true}))?;
        assert_eq!(
            assertions()?,
            active,
            "Terminal-before-acceptance is not settled yet"
        );
        actor.frame(json!({"id":id,"type":"response","command":"prompt","success":true}))?;
        assert!(
            assertions()?.is_empty(),
            "Accepted terminal turn must release protection"
        );

        let id = submit(&mut actor)?;
        actor.frame(json!({"id":id,"type":"response","command":"prompt","success":true}))?;
        let active = assertions()?;
        assert_eq!(active.len(), 1);
        actor.extension(json!({"id":"confirm","method":"confirm","title":"Continue?"}))?;
        assert_eq!(
            assertions()?,
            active,
            "Waiting for input must retain protection"
        );
        actor.command(ConversationCommand::Interrupt)?;
        assert_eq!(assertions()?, active, "Interrupt request is not terminal");
        actor.frame(json!({"type":"agent_end","isTerminal":true}))?;
        assert!(
            assertions()?.is_empty(),
            "Interrupted terminal turn must release protection"
        );

        submit(&mut actor)?;
        assert_eq!(assertions()?.len(), 1);
        actor.disconnect("Fixture EOF".into());
        assert!(
            assertions()?.is_empty(),
            "Disconnect must release protection"
        );
        drop(actor);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn responses_are_correlated_by_id_even_when_reordered() {
        let mut requests = Requests::default();
        requests
            .pending
            .insert("state".into(), pending(RequestKind::State, "get_state"));
        requests.pending.insert(
            "models".into(),
            pending(RequestKind::Models, "get_available_models"),
        );
        let response = json!({"id":"models","command":"get_available_models","type":"response","success":true});
        assert!(matches!(
            requests.take(&response).unwrap().unwrap().kind,
            RequestKind::Models
        ));
        let response = json!({"id":"state","command":"get_state","type":"response","success":true});
        assert!(matches!(
            requests.take(&response).unwrap().unwrap().kind,
            RequestKind::State
        ));
        assert!(requests.take(&response).unwrap().is_none());
        requests
            .pending
            .insert("state".into(), pending(RequestKind::State, "get_state"));
        assert!(
            requests
                .take(&json!({"id":"state","command":"prompt"}))
                .is_err()
        );
    }

    #[test]
    fn chunk_reassembly_preserves_utf8_split_between_chunks() {
        let bytes = large_frame();
        let mut decoder = FrameDecoder::default();
        let parts = chunks(&bytes);
        let last = parts.len() - 1;
        for (index, part) in parts.into_iter().enumerate() {
            let frame = decoder.push(part).unwrap();
            if index == last {
                assert_eq!(
                    frame.unwrap(),
                    serde_json::from_slice::<Value>(&bytes).unwrap()
                );
            } else {
                assert!(frame.is_none());
            }
        }
        decoder.eof().unwrap();
    }

    #[test]
    fn malformed_interleaved_and_truncated_chunks_never_complete() {
        let parts = chunks(&large_frame());
        let mut decoder = FrameDecoder::default();
        decoder.push(parts[0].clone()).unwrap();
        assert!(decoder.push(json!({"type":"agent_end"})).is_err());
        assert!(decoder.eof().is_err());
        let mut decoder = FrameDecoder::default();
        assert!(decoder.push(parts[1].clone()).is_err());
        let mut decoder = FrameDecoder::default();
        decoder.push(parts[0].clone()).unwrap();
        let mut mismatch = parts[1].clone();
        mismatch["chunkId"] = json!("other");
        assert!(decoder.push(mismatch).is_err());
        let mut invalid = parts[0].clone();
        invalid["data"] = json!("Zh==");
        assert!(FrameDecoder::default().push(invalid).is_err());
        let mut invalid = parts[0].clone();
        invalid["byteLength"] = json!(LOGICAL + 1);
        assert!(FrameDecoder::default().push(invalid).is_err());
        let mut bytes = large_frame();
        bytes[100] = 0xff;
        let mut decoder = FrameDecoder::default();
        let mut error = false;
        for part in chunks(&bytes) {
            if decoder.push(part).is_err() {
                error = true;
                break;
            }
        }
        assert!(error, "Invalid UTF-8 cannot become a completed message");
    }

    #[test]
    fn continuation_and_acceptance_are_not_completion() {
        let id = Uuid::new_v4();
        let mut turn = Turn::default();
        turn.begin(id);
        assert!(
            matches!(turn.accept(id),Some(ConversationEvent::SubmissionAccepted { request_id }) if request_id==id)
        );
        assert_eq!(
            turn.agent_end(&json!({"type":"agent_end","isTerminal":false})),
            None
        );
        assert_eq!(turn.id, Some(id));
        let outcome = turn.agent_end(&json!({"type":"agent_end"})).unwrap();
        assert!(
            matches!(turn.settle(outcome),Some(ConversationEvent::TurnSettled { request_id, outcome:TurnOutcome::Completed }) if request_id==id)
        );
    }

    #[test]
    fn terminal_before_ack_waits_for_durable_acceptance() {
        let id = Uuid::new_v4();
        let mut turn = Turn::default();
        turn.begin(id);
        assert!(turn.settle(TurnOutcome::Completed).is_none());
        assert_eq!(turn.id, Some(id));
        assert!(matches!(
            turn.accept(id),
            Some(ConversationEvent::SubmissionAccepted { .. })
        ));
        let outcome = turn.terminal.take().unwrap();
        assert!(matches!(
            turn.settle(outcome),
            Some(ConversationEvent::TurnSettled {
                outcome: TurnOutcome::Completed,
                ..
            })
        ));
    }

    #[test]
    fn late_same_id_failure_retains_recovery_identity_after_ack() {
        let id = Uuid::new_v4();
        let mut turn = Turn::default();
        turn.begin(id);
        let mut requests = Requests::default();
        requests
            .pending
            .insert(id.to_string(), pending(RequestKind::Prompt(id), "prompt"));
        let ack = json!({"type":"response","command":"prompt","id":id,"success":true});
        requests.take(&ack).unwrap().unwrap();
        turn.accept(id).unwrap();
        let failure = json!({"type":"response","command":"prompt","id":id,"success":false,"error":"Missing provider credentials"});
        assert!(requests.take(&failure).unwrap().is_none());
        assert!(
            matches!(turn.late_failure(&failure),Some(ConversationEvent::TurnSettled { request_id,outcome:TurnOutcome::Failed }) if request_id==id)
        );
        // The emitted UUID is the durable submission key; no replacement prompt or
        // new UUID is synthesized, so the app can offer its immutable text/images.
    }

    #[test]
    fn local_command_result_settles_only_its_own_submission() {
        let id = Uuid::new_v4();
        let mut turn = Turn::default();
        turn.begin(id);
        turn.accept(id);
        assert_eq!(
            turn.prompt_result(&json!({"id":Uuid::new_v4(),"agentInvoked":false})),
            None
        );
        assert_eq!(
            turn.prompt_result(&json!({"id":id,"agentInvoked":true})),
            None
        );
        let outcome = turn
            .prompt_result(&json!({"type":"prompt_result","id":id,"agentInvoked":false}))
            .unwrap();
        assert!(matches!(
            turn.settle(outcome),
            Some(ConversationEvent::TurnSettled {
                outcome: TurnOutcome::Completed,
                ..
            })
        ));
    }

    #[test]
    fn timeout_and_cancellation_never_approve_and_target_one_dialog() {
        let now = Instant::now();
        let answer = deadline_answer(UserAnswer::Confirmed(true), Some(now), now);
        let frame = answer_frame("a", answer);
        assert_eq!(frame["cancelled"], true);
        assert_eq!(frame["timedOut"], true);
        assert!(frame.get("confirmed").is_none());
        let frame = answer_frame("a", UserAnswer::Cancelled { timed_out: false });
        assert_eq!(frame["cancelled"], true);
        assert!(frame.get("confirmed").is_none());
        assert!(frame.get("value").is_none());
        let mut dialogs = HashMap::new();
        for id in ["a", "b"] {
            dialogs.insert(
                id.into(),
                Dialog {
                    request: UserRequest {
                        id: id.into(),
                        kind: UserRequestKind::Confirm,
                        title: "Confirm".into(),
                        options: Vec::new(),
                        text: String::new(),
                        placeholder: None,
                        deadline: None,
                    },
                    deadline: None,
                },
            );
        }
        assert!(
            matches!(cancel_target(&mut dialogs,"a"),Some(ConversationEvent::UserRequestDismissed { request_id }) if request_id=="a")
        );
        assert!(dialogs.contains_key("b"));
        assert!(cancel_target(&mut dialogs, "a").is_none());
    }

    #[test]
    fn disconnect_does_not_claim_confirmed_interruption() {
        let id = Uuid::new_v4();
        let mut turn = Turn::default();
        turn.begin(id);
        turn.accept(id);
        turn.interrupting = true;
        assert!(matches!(
            turn.settle(TurnOutcome::Unknown),
            Some(ConversationEvent::TurnSettled {
                outcome: TurnOutcome::Unknown,
                ..
            })
        ));
    }

    #[test]
    fn corrupt_image_does_not_discard_surrounding_message() {
        let message = normalize_message(
            &json!({"role":"assistant","timestamp":1,"content":[
                {"type":"text","text":"Before"},{"type":"image","mimeType":"image/png","data":"not base64"},{"type":"text","text":"After"}
            ]}),
            "fallback",
            std::path::Path::new("/unused"),
            &mut ImageCache::default(),
        );
        assert!(
            matches!(&message.content[..],[ContentBlock::Text(before),ContentBlock::ImageError(_),ContentBlock::Text(after)] if before=="Before" && after=="After")
        );
    }

    #[test]
    fn tool_call_block_keeps_id_name_intent_and_arguments() {
        let message = normalize_message(
            &json!({"role":"assistant","timestamp":2,"content":[
                {"type":"text","text":"Looking"},
                {"type":"toolCall","id":"call_1","name":"bash","intent":"List files","arguments":{"command":"ls","i":"List files"}},
                {"type":"toolCall","id":"call_2","name":"read","arguments":{"path":"a.rs","i":"Read a"}},
                {"type":"toolCall","name":"orphan","arguments":{}}
            ]}),
            "fallback",
            std::path::Path::new("/unused"),
            &mut ImageCache::default(),
        );
        assert!(message.tool_call_id.is_none());
        assert!(!message.error);
        match &message.content[..] {
            [
                ContentBlock::Text(text),
                ContentBlock::ToolCall {
                    id,
                    name,
                    intent,
                    input,
                },
                ContentBlock::ToolCall {
                    id: second,
                    intent: second_intent,
                    ..
                },
            ] => {
                assert_eq!(text, "Looking");
                assert_eq!(id, "call_1");
                assert_eq!(name, "bash");
                assert_eq!(intent.as_deref(), Some("List files"));
                assert_eq!(input["command"], "ls");
                assert_eq!(second, "call_2");
                assert_eq!(second_intent.as_deref(), Some("Read a"));
            }
            other => panic!("unexpected blocks: {other:?}"),
        }
        // A tool call without an ID cannot be answered; it stays visible as raw data.
        assert!(message.raw.is_some());
    }

    #[test]
    fn tool_result_message_keeps_call_reference_error_and_exit_code() {
        let message = normalize_message(
            &json!({"role":"toolResult","timestamp":3,"toolCallId":"call_1","toolName":"bash",
                "isError":true,"details":{"exitCode":1,"wallTimeMs":5},
                "content":[{"type":"text","text":"boom"}]}),
            "fallback",
            std::path::Path::new("/unused"),
            &mut ImageCache::default(),
        );
        assert_eq!(message.tool_call_id.as_deref(), Some("call_1"));
        assert!(message.error);
        assert_eq!(message.exit_code, Some(1));
        assert!(matches!(&message.content[..], [ContentBlock::Text(text)] if text == "boom"));
        let user = normalize_message(
            &json!({"role":"user","timestamp":4,"toolCallId":"stray","content":[]}),
            "fallback",
            std::path::Path::new("/unused"),
            &mut ImageCache::default(),
        );
        assert!(user.tool_call_id.is_none());
    }

    #[test]
    fn outgoing_budget_counts_json_escaping_base64_padding_and_newline() {
        let id = Uuid::new_v4();
        let text = "日本語\n\"quoted\"";
        let bytes = [1, 2, 3, 4];
        let actual = json!({"id":id,"type":"prompt","message":text,"images":[{"type":"image","mimeType":"image/png","data":STANDARD.encode(bytes)}]});
        assert_eq!(
            submission_size_for_lengths(id, text, &[("image/png", 4)]).unwrap(),
            serde_json::to_vec(&actual).unwrap().len() as u64 + 1
        );
        let overhead = submission_size_for_lengths(id, "", &[]).unwrap();
        let text = "x".repeat(FRAME - overhead as usize);
        assert_eq!(
            submission_size_for_lengths(id, &text, &[]).unwrap(),
            FRAME as u64
        );
        assert_eq!(
            submission_size_for_lengths(id, &format!("{text}x"), &[]).unwrap(),
            FRAME as u64 + 1
        );
    }

    fn test_actor(dialect: Dialect) -> Result<(Actor, std::path::PathBuf)> {
        let root = std::env::temp_dir().join(format!("drostje-omp-commands-{}", Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        let mut config = ConversationConfig {
            harness: match dialect {
                Dialect::Omp => "omp",
                Dialect::Pi => "pi",
            }
            .into(),
            executable: "/usr/bin/true".into(),
            cwd: root.clone(),
            session_dir: root.join("session"),
            session_ref: None,
            model: None,
            effort: None,
            permission_mode: None,
            browser_tools: None,
            allow_empty_session: true,
        };
        let (child, _) = prepare(&mut config, dialect)?;
        let (_commands, input) = async_channel::bounded(64);
        let (output, _events) = async_channel::bounded(128);
        let mut actor = Actor::new(config, child, input, output, dialect);
        actor.initialized = true;
        actor.negotiated = true;
        actor.events.clear();
        Ok((actor, root))
    }

    fn written(actor: &Actor) -> Vec<Value> {
        actor
            .writes
            .iter()
            .map(|bytes| serde_json::from_slice(bytes).unwrap())
            .collect()
    }

    #[test]
    fn omp_and_pi_command_lists_parse_into_one_shape() -> Result<()> {
        let (commands, rejected) = parse_commands(
            Dialect::Omp,
            &json!([
                {"name":"model","aliases":["models"],"description":"Show current model selection","source":"builtin"},
                {"name":"compact","description":"Compact the conversation","input":{"hint":"[soft|remote|snapcompact] [focus]"},"subcommands":[{"name":"soft","description":"Summarize locally"}],"source":"builtin"},
                {"name":"skill:tdd","description":"Test-driven development.","input":{"hint":"arguments"},"source":"skill"},
                {"name":"green","description":"Iterate on CI","source":"custom"},
                {"name":"later","description":"From a newer OMP","source":"workflow"},
                {"description":"No name"}
            ]),
        )?;
        assert_eq!(rejected, vec![json!({"description":"No name"})]);
        assert_eq!(commands.len(), 5);
        assert_eq!(commands[0].aliases, ["models"]);
        assert_eq!(
            commands[0].blocked,
            Some(super::super::commands::MODEL_PICKER)
        );
        assert_eq!(
            commands[1].argument_hint.as_deref(),
            Some("[soft|remote|snapcompact] [focus]")
        );
        assert_eq!(commands[1].blocked, None);
        assert_eq!(commands[2].source, CommandSource::Skill);
        assert_eq!(commands[3].source, CommandSource::Custom);
        assert_eq!(commands[4].source, CommandSource::Other("workflow".into()));

        // Pi's documented and shipped shapes differ only in location fields.
        let (commands, rejected) = parse_commands(
            Dialect::Pi,
            &json!([
                {"name":"skill:review","description":"Review code","source":"skill","location":"user","path":"/home/u/.pi/agent/skills/review/SKILL.md"},
                {"name":"fix","source":"prompt","sourceInfo":{"path":"/p/.pi/prompts/fix.md","source":"local","scope":"project","origin":"top-level"}},
                {"name":"model","description":"An extension's own model command","source":"extension"}
            ]),
        )?;
        assert!(rejected.is_empty());
        assert_eq!(commands[0].source, CommandSource::Skill);
        assert_eq!(commands[1].source, CommandSource::Template);
        assert_eq!(commands[1].description, "");
        assert_eq!(
            commands[2].blocked, None,
            "only OMP's own builtins are blocked"
        );
        assert!(parse_commands(Dialect::Pi, &json!({"commands":[]})).is_err());
        Ok(())
    }

    #[test]
    fn omp_requests_commands_after_negotiation_and_applies_pushed_updates() -> Result<()> {
        let (mut actor, root) = test_actor(Dialect::Omp)?;
        actor.negotiated = false;
        actor.request(
            json!({"id":"negotiate","type":"negotiate_protocol","protocolVersion":2}),
            RequestKind::Negotiate,
        )?;
        actor.frame(json!({"id":"negotiate","type":"response","command":"negotiate_protocol","success":true,"data":{"protocolVersion":2}}))?;
        let request = written(&actor)
            .into_iter()
            .find(|frame| frame["type"] == "get_available_commands")
            .context("OMP command list was not requested")?;

        actor.frame(json!({"id":request["id"],"type":"response","command":"get_available_commands","success":true,
            "data":{"commands":[{"name":"compact","description":"Compact the conversation","source":"builtin"}]}}))?;
        assert!(actor.events.iter().any(|event| matches!(event,
            ConversationEvent::CommandsUpdated(commands) if commands.len() == 1 && commands[0].name == "compact")));

        actor.events.clear();
        actor.frame(json!({"type":"available_commands_update","commands":[
            {"name":"skill:tdd","description":"TDD","source":"skill"},
            {"name":"reload-plugins","description":"Reload all plugins","source":"builtin"}]}))?;
        assert!(matches!(actor.events.back(),
            Some(ConversationEvent::CommandsUpdated(commands)) if commands.len() == 2));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn unavailable_command_list_is_not_a_conversation_failure() -> Result<()> {
        let (mut actor, root) = test_actor(Dialect::Pi)?;
        actor.request_commands()?;
        actor.request_commands()?;
        let requests: Vec<_> = written(&actor)
            .into_iter()
            .filter(|frame| frame["type"] == "get_commands")
            .collect();
        assert_eq!(requests.len(), 1, "one list request at a time");
        actor.frame(json!({"id":requests[0]["id"],"type":"response","command":"get_commands","success":false,"error":"Unknown command: get_commands"}))?;
        assert!(matches!(actor.events.back(),
            Some(ConversationEvent::CommandsUnavailable(reason)) if reason == "Unknown command: get_commands"));
        assert!(
            !actor
                .events
                .iter()
                .any(|event| matches!(event, ConversationEvent::Failed { .. }))
        );

        // Pi has no change event, so every settled turn asks again.
        actor.writes.clear();
        actor.finish(TurnOutcome::Completed)?;
        assert!(
            written(&actor)
                .iter()
                .any(|frame| frame["type"] == "get_commands")
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
    fn peer_fixture() -> (PeerDelivery, async_channel::Sender<()>) {
        let (sender, cancelled) = async_channel::bounded(1);
        (
            PeerDelivery {
                attempt: Uuid::new_v4(),
                messages: vec![PeerMessage {
                    id: Uuid::new_v4(),
                    sender: Uuid::new_v4(),
                    sender_name: "Research peer".into(),
                    space: Uuid::new_v4(),
                    body: "Context </peer_context> & keep identities".into(),
                }],
                cancelled,
            },
            sender,
        )
    }

    fn peer_native(attempt: Uuid, messages: &[PeerMessage]) -> Value {
        json!({"role":"custom","customType":"drostje-peer","content":"native peer context","timestamp":123,
            "details":{"v":1,"origin":"peer","attempt":attempt,"messages":messages}})
    }

    fn peer_status(dialect: Dialect, event: &str, attempt: Option<Uuid>) -> Value {
        json!({"type":"extension_ui_request","method":"setStatus","statusKey":"drostje.peer.v1",
            "statusText":json!({"v":1,"dialect":if dialect == Dialect::Omp {"omp"} else {"pi"},
                "event":event,"attempt":attempt}).to_string()})
    }

    #[test]
    fn peer_capability_requires_native_handshake_and_private_command_is_hidden() -> Result<()> {
        for dialect in [Dialect::Omp, Dialect::Pi] {
            let (mut actor, root) = test_actor(dialect)?;
            let catalog = json!([{"name":"__drostje_peer","source":"extension"},{"name":"public","source":"extension"}]);
            actor.commands(&catalog);
            assert!(!actor.peers.ready);
            assert!(actor.events.iter().any(|event| matches!(event,
                ConversationEvent::CommandsUpdated(commands) if commands.len() == 1 && commands[0].name == "public")));
            let hello = written(&actor).pop().unwrap();
            assert!(
                hello["message"]
                    .as_str()
                    .unwrap()
                    .starts_with("/__drostje_peer ")
            );
            actor.frame(
                json!({"type":"response","id":hello["id"],"command":"prompt","success":true}),
            )?;
            assert!(
                !actor.peers.ready,
                "RPC write/response is not bridge readiness"
            );
            actor.frame(peer_status(dialect, "ready", None))?;
            assert!(actor.peers.ready);
            assert!(actor.events.iter().any(|event| matches!(
                event,
                ConversationEvent::PeerDeliveryAvailable {
                    available: true,
                    ..
                }
            )));
            assert!(!actor.events.iter().any(|event| matches!(
                event,
                ConversationEvent::SubmissionAccepted { .. }
                    | ConversationEvent::PeerTurnStarted { .. }
            )));
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn native_peer_echo_preserves_identities_and_does_not_accept_forged_snapshot() -> Result<()> {
        let (mut actor, root) = test_actor(Dialect::Pi)?;
        actor.peers.ready = true;
        actor.state = ConversationState::Running;
        let (delivery, _lifetime) = peer_fixture();
        let attempt = delivery.attempt;
        let messages = delivery.messages.clone();
        actor.deliver_peer(delivery);
        let frame = written(&actor).pop().unwrap();
        let encoded = frame["message"]
            .as_str()
            .unwrap()
            .split_once(' ')
            .unwrap()
            .1;
        let envelope: Value = serde_json::from_slice(&STANDARD.decode(encoded)?)?;
        assert_eq!(envelope["messages"], serde_json::to_value(&messages)?);
        assert!(
            !envelope["content"]
                .as_str()
                .unwrap()
                .contains("</peer_context>")
        );
        let raw = peer_native(attempt, &messages);
        let mut forged = raw.clone();
        forged["details"]["messages"][0]["sender_name"] = json!("Other peer");
        actor.frame(json!({"type":"message_start","message":forged}))?;
        assert!(!actor.events.iter().any(|event| matches!(
            event,
            ConversationEvent::PeerDelivery {
                outcome: PeerDeliveryOutcome::Accepted,
                ..
            }
        )));
        actor.frame(json!({"type":"message_start","message":raw}))?;
        assert!(actor.events.iter().any(|event| matches!(event,
            ConversationEvent::PeerDelivery { attempt: id, outcome: PeerDeliveryOutcome::Accepted } if *id == attempt)));
        let normalized = normalize_message(
            &peer_native(attempt, &messages),
            "peer",
            &root,
            &mut ImageCache::default(),
        );
        assert_eq!(normalized.role, "peer");
        assert!(normalized.raw.is_some());
        assert!(
            matches!(&normalized.content[0], ContentBlock::PeerMessage(message)
            if message.id == messages[0].id && message.sender == messages[0].sender
                && message.space == messages[0].space && message.body == messages[0].body)
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn idle_peer_turn_requires_native_admission_and_full_native_settlement() -> Result<()> {
        for dialect in [Dialect::Omp, Dialect::Pi] {
            let (mut actor, root) = test_actor(dialect)?;
            actor.state = ConversationState::Ready;
            actor.peers.ready = true;
            let (delivery, _lifetime) = peer_fixture();
            let attempt = delivery.attempt;
            actor.deliver_peer(delivery);
            let frame = written(&actor).pop().unwrap();
            actor.frame(json!({"type":"response","command":"prompt","id":frame["id"],"success":true,"data":{"agentInvoked":false}}))?;
            assert!(actor.turn.id.is_none());
            assert!(
                actor.active(),
                "pending native admission reserves idle turn"
            );
            assert!(!actor.events.iter().any(|event| matches!(
                event,
                ConversationEvent::PeerDelivery {
                    outcome: PeerDeliveryOutcome::Accepted,
                    ..
                } | ConversationEvent::SubmissionAccepted { .. }
            )));
            actor.frame(peer_status(dialect, "turn_started", Some(attempt)))?;
            assert!(
                actor.turn.id.is_none(),
                "peer turns never occupy the user submission slot"
            );
            assert_eq!(actor.peers.turns, vec![attempt]);
            actor.frame(json!({"type":"agent_end","messages":[],"isTerminal":true}))?;
            assert_eq!(
                actor.peers.turns,
                vec![attempt],
                "low-level loop end is not session settlement"
            );
            actor.frame(json!({"type":if dialect == Dialect::Omp {"session_settled"} else {"agent_settled"}}))?;
            assert!(actor.turn.id.is_none());
            assert!(actor.peers.turns.is_empty());
            assert!(actor.events.iter().any(|event| matches!(event,
                ConversationEvent::TurnSettled { request_id, outcome: TurnOutcome::Completed } if *request_id == attempt)));
            assert!(
                !actor
                    .events
                    .iter()
                    .any(|event| matches!(event, ConversationEvent::SubmissionAccepted { .. }))
            );
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn busy_peer_receipt_has_no_ordinary_control_deadline() -> Result<()> {
        let (mut actor, root) = test_actor(Dialect::Omp)?;
        actor.peers.ready = true;
        actor.turn.begin(Uuid::new_v4());
        let (delivery, _lifetime) = peer_fixture();
        let attempt = delivery.attempt;
        let messages = delivery.messages.clone();
        actor.deliver_peer(delivery);
        for pending in actor.requests.pending.values_mut() {
            pending.deadline = Instant::now() - CONTROL_TIMEOUT;
        }
        actor.tick()?;
        assert!(!actor.events.iter().any(|event| matches!(
            event,
            ConversationEvent::PeerDelivery { .. } | ConversationEvent::Failed { .. }
        )));
        actor.frame(json!({"type":"message_end","message":peer_native(attempt, &messages)}))?;
        assert!(actor.events.iter().any(|event| matches!(
            event,
            ConversationEvent::PeerDelivery {
                outcome: PeerDeliveryOutcome::Accepted,
                ..
            }
        )));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn peer_cancellation_distinguishes_unsent_from_uncertain_and_allows_late_receipt() -> Result<()>
    {
        let (mut actor, root) = test_actor(Dialect::Pi)?;
        actor.peers.ready = true;
        actor.state = ConversationState::Ready;
        let (delivery, lifetime) = peer_fixture();
        let unsent = delivery.attempt;
        actor.deliver_peer(delivery);
        drop(lifetime);
        actor.cancel_peers(false, "cancelled")?;
        assert!(actor.writes.is_empty());
        assert!(actor.events.iter().any(|event| matches!(event,
            ConversationEvent::PeerDelivery { attempt, outcome: PeerDeliveryOutcome::Rejected(_) } if *attempt == unsent)));
        let (delivery, lifetime) = peer_fixture();
        let attempt = delivery.attempt;
        let messages = delivery.messages.clone();
        actor.deliver_peer(delivery);
        actor.writes.clear(); // transport has consumed the write; acceptance is not known
        drop(lifetime);
        actor.cancel_peers(false, "cancelled after dispatch")?;
        assert!(actor.events.iter().any(|event| matches!(event,
            ConversationEvent::PeerDelivery { attempt: id, outcome: PeerDeliveryOutcome::Unknown(_) } if *id == attempt)));
        actor.peer_message(&peer_native(attempt, &messages));
        assert!(
            matches!(actor.events.back(), Some(ConversationEvent::PeerDelivery {
            attempt: id, outcome: PeerDeliveryOutcome::Accepted }) if *id == attempt)
        );
        actor.disconnect("transport lost".into());
        assert!(!actor.peers.ready);
        assert!(actor.turn.id.is_none());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
