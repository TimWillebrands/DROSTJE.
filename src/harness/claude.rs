use super::*;
use crate::platform::IdleSleepAssertion;
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

const FRAME: usize = 1_048_576;
const OUTPUT_FRAME: usize = 67_108_864;
/// Claude Code (2.1.x, `--input-format stream-json`) emits its `system/init` frame only after
/// the first `user` frame, so this deadline is armed by the first prompt, not by the spawn.
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSCRIPT: &str = "conversation.jsonl";
const INITIALIZE_REQUEST: &str = "drostje-initialize";
const CONTEXT_USAGE_REQUEST: &str = "drostje-context-usage";
/// `get_usage` is marked experimental by Claude Code; an unexpected answer is kept as a diagnostic.
const USAGE_REQUEST: &str = "drostje-usage";

pub(super) fn submission_size_for_lengths(
    id: Uuid,
    text: &str,
    images: &[(&str, u64)],
) -> Result<u64> {
    let content = std::iter::once(json!({"type":"text","text":text}))
        .chain(images.iter().map(|(mime, _)| {
            json!({"type":"image","source":{"type":"base64","media_type":mime,"data":""}})
        }))
        .collect::<Vec<_>>();
    let frame = json!({
        "type":"user",
        "message":{"role":"user","content":content},
        "parent_tool_use_id":Value::Null,
        "session_id":id,
    });
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

struct OwnedChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    reaped: bool,
}

impl OwnedChild {
    fn close(&mut self) {
        self.stdin.take();
        if self.reaped {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.reaped = true;
                    return;
                }
                Ok(None) if Instant::now() < deadline => {
                    let mut discard = [0; 16_384];
                    let _ = self.stdout.read(&mut discard);
                    let _ = self.stderr.read(&mut discard);
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => break,
            }
        }
        // SAFETY: this PID is the unreaped child whose setsid call succeeded before exec.
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.reaped = true;
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // SAFETY: the same owned child process group is still alive and must not outlive the app.
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
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
    // SAFETY: fcntl only reads flags from this live pipe descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: the descriptor is owned by this process; adding O_NONBLOCK preserves its flags.
    let changed = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    ensure!(
        flags >= 0 && changed >= 0,
        "Set Claude Code pipe nonblocking: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[derive(Clone)]
struct Session {
    id: Uuid,
    reference: String,
    transcript_path: PathBuf,
    /// Claude Code stores a session at its first user frame, so a saved reference alone does not
    /// prove one exists; a line in our transcript does. Resuming an unstored ID is a fatal error.
    resume: bool,
}

fn session(config: &mut ConversationConfig) -> Result<Session> {
    ensure!(
        config.harness == "claude-code",
        "Unsupported conversation harness: {}",
        config.harness
    );
    config.cwd = fs::canonicalize(&config.cwd).context("Project root is unavailable")?;
    ensure!(config.cwd.is_dir(), "Project root is not a directory");
    fs::create_dir_all(&config.session_dir)?;
    config.session_dir = fs::canonicalize(&config.session_dir)?;

    let transcript_path = config.session_dir.join(TRANSCRIPT);
    let (id, resume) = if let Some(reference) = &config.session_ref {
        let saved: Value = serde_json::from_str(reference).context(
            "Invalid saved Claude Code session reference; create a new conversation explicitly",
        )?;
        ensure!(
            saved["cwd"].as_str() == config.cwd.to_str()
                && saved["session_dir"].as_str() == config.session_dir.to_str(),
            "Saved Claude Code session belongs to a different project or session directory"
        );
        let id = saved["id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            .context("Saved Claude Code session ID is missing or invalid")?;
        (id, !read_transcript(&transcript_path)?.is_empty())
    } else {
        (Uuid::new_v4(), false)
    };
    let reference = serde_json::to_string(&json!({
        "id":id,
        "cwd":config.cwd,
        "session_dir":config.session_dir,
    }))?;
    Ok(Session {
        id,
        reference,
        transcript_path,
        resume,
    })
}

fn prepare(config: &mut ConversationConfig) -> Result<(OwnedChild, Session, Option<String>)> {
    let session = session(config)?;
    let mut command = Command::new(&config.executable);
    // `--permission-prompts host` only names who answers; without `--permission-prompt-tool
    // stdio` the CLI never sends `can_use_tool` and auto-denies every prompt.
    command
        .args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-prompts",
            "host",
            "--permission-prompt-tool",
            "stdio",
        ])
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
    if session.resume {
        command.arg("--resume").arg(session.id.to_string());
    } else {
        command.arg("--session-id").arg(session.id.to_string());
    }
    if let Some(model) = &config.model {
        command.arg("--model").arg(&model.id);
    }
    if let Some(effort) = &config.effort {
        command.arg("--effort").arg(effort);
    }
    if let Some(mode) = &config.permission_mode {
        ensure!(
            super::permission_modes("claude-code")
                .iter()
                .any(|(value, _)| value == mode),
            "Unsupported Claude Code permission mode: {mode}"
        );
        command.arg("--permission-mode").arg(mode);
    }
    // SAFETY: pre_exec runs after fork and before exec; setsid has no Rust-managed inputs.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Start Claude Code at {}", config.executable.display()))?;
    let stdin = child.stdin.take().context("Open Claude Code stdin")?;
    let stdout = child.stdout.take().context("Open Claude Code stdout")?;
    let stderr = child.stderr.take().context("Open Claude Code stderr")?;
    let owned = OwnedChild {
        child,
        stdin: Some(stdin),
        stdout,
        stderr,
        reaped: false,
    };
    nonblocking(owned.stdin.as_ref().unwrap().as_raw_fd())?;
    nonblocking(owned.stdout.as_raw_fd())?;
    nonblocking(owned.stderr.as_raw_fd())?;
    Ok((owned, session, browser_warning))
}

pub(super) async fn open(mut config: ConversationConfig) -> Result<ConversationHandle> {
    let (commands, input) = async_channel::bounded(64);
    let (output, events) = async_channel::bounded(128);
    smol::unblock(move || -> Result<()> {
        let (child, session, browser_warning) = prepare(&mut config)?;
        if let Some(text) = browser_warning {
            let _ = output.try_send(ConversationEvent::Notice {
                level: "warning".into(),
                text,
            });
        }
        let actor = Actor::new(config, session, child, input, output)?;
        std::thread::Builder::new()
            .name("drostje-claude-code".into())
            .spawn(move || {
                let mut actor = actor;
                if let Err(error) = actor.run() {
                    actor.disconnect(format!("{error:#}"));
                }
                actor.child.close();
                for event in actor.events.drain(..) {
                    if actor.output.send_blocking(event).is_err() {
                        break;
                    }
                }
            })
            .context("Start Claude Code actor")?;
        Ok(())
    })
    .await?;
    Ok(ConversationHandle { commands, events })
}

struct PendingWrite {
    bytes: Vec<u8>,
    accepted: Option<(Uuid, Value)>,
}

#[derive(Default)]
struct Turn {
    id: Option<Uuid>,
    accepted: bool,
    interrupting: bool,
}

/// One unanswered `can_use_tool` request.
#[derive(Debug)]
struct Permission {
    input: Value,
    /// Labelled `permission_suggestions` from the CLI; the chosen one goes back verbatim as
    /// `updatedPermissions` so Claude Code stops asking.
    choices: Vec<(String, Value)>,
    /// `AskUserQuestion` travels through the same channel; `Some` names the question whose
    /// answer goes into `updatedInput`.
    question: Option<String>,
}

struct Actor {
    config: ConversationConfig,
    session: Session,
    child: OwnedChild,
    commands: async_channel::Receiver<ConversationCommand>,
    output: async_channel::Sender<ConversationEvent>,
    events: VecDeque<ConversationEvent>,
    state: ConversationState,
    init_deadline: Option<Instant>,
    initialized: bool,
    closing: bool,
    line: Vec<u8>,
    read_buffer: [u8; 16_384],
    read_start: usize,
    read_end: usize,
    writes: VecDeque<PendingWrite>,
    write_offset: usize,
    transcript: File,
    history: Vec<Value>,
    sequence: usize,
    turn: Turn,
    idle_sleep_assertion: Option<IdleSleepAssertion>,
    permissions: HashMap<String, Permission>,
    tools: HashMap<String, ToolActivity>,
    /// Tool result images already stored under the session directory.
    images: ImageCache,
    /// `set_model` control requests awaiting the CLI's answer.
    model_requests: HashMap<String, ModelKey>,
    /// The `initialize` answer carried the model catalog, so `init` need not report one.
    models_reported: bool,
    /// Usage control requests awaiting an answer, so refresh triggers never pile up.
    usage_requests: Vec<&'static str>,
    /// The last command list, kept raw so `init` can mark terminal-only commands later.
    slash_commands: Option<Value>,
    /// `terminal_slash_commands` from `init`: commands bound to the Claude Code terminal.
    terminal_commands: Vec<String>,
}

impl Actor {
    fn new(
        config: ConversationConfig,
        session: Session,
        child: OwnedChild,
        commands: async_channel::Receiver<ConversationCommand>,
        output: async_channel::Sender<ConversationEvent>,
    ) -> Result<Self> {
        let history = read_transcript(&session.transcript_path)?;
        let sequence = history.len();
        let transcript = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&session.transcript_path)
            .context("Open Claude Code transcript")?;
        Ok(Self {
            config,
            session,
            child,
            commands,
            output,
            events: VecDeque::from([ConversationEvent::StateChanged(ConversationState::Starting)]),
            state: ConversationState::Starting,
            init_deadline: None,
            initialized: false,
            closing: false,
            line: Vec::new(),
            read_buffer: [0; 16_384],
            read_start: 0,
            read_end: 0,
            writes: VecDeque::new(),
            write_offset: 0,
            transcript,
            history,
            sequence,
            turn: Turn::default(),
            idle_sleep_assertion: None,
            permissions: HashMap::new(),
            tools: HashMap::new(),
            images: ImageCache::default(),
            slash_commands: None,
            terminal_commands: Vec::new(),
            model_requests: HashMap::new(),
            models_reported: false,
            usage_requests: Vec::new(),
        })
    }

    fn emit(&mut self, event: ConversationEvent) {
        self.events.push_back(event);
    }

    fn state(&mut self, state: ConversationState) {
        if self.state != state {
            self.state = state.clone();
            self.emit(ConversationEvent::StateChanged(state));
        }
    }

    fn queue(&mut self, value: Value, accepted: Option<(Uuid, Value)>) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= FRAME, "Claude Code input exceeds 1 MiB");
        ensure!(
            self.writes.len() < 128,
            "Claude Code input pipe is not accepting commands"
        );
        self.writes.push_back(PendingWrite { bytes, accepted });
        Ok(())
    }

    /// Asks for context and plan usage unless the same question is still unanswered.
    fn refresh_usage(&mut self, requests: &[&'static str]) -> Result<()> {
        for &request_id in requests {
            if self.usage_requests.contains(&request_id) {
                continue;
            }
            let subtype = if request_id == CONTEXT_USAGE_REQUEST {
                "get_context_usage"
            } else {
                "get_usage"
            };
            self.queue(
                json!({"type":"control_request","request_id":request_id,
                    "request":{"subtype":subtype}}),
                None,
            )?;
            self.usage_requests.push(request_id);
        }
        Ok(())
    }

    fn append_history(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.transcript, value)?;
        self.transcript.write_all(b"\n")?;
        self.transcript.flush()?;
        self.history.push(value.clone());
        self.sequence += 1;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        while let Some(write) = self.writes.front() {
            let Some(stdin) = self.child.stdin.as_mut() else {
                bail!("Claude Code input closed");
            };
            match stdin.write(&write.bytes[self.write_offset..]) {
                Ok(0) => bail!("Claude Code input closed"),
                Ok(count) => {
                    self.write_offset += count;
                    if self.write_offset == write.bytes.len() {
                        let completed = self.writes.pop_front().unwrap();
                        self.write_offset = 0;
                        if let Some((request_id, history)) = completed.accepted {
                            self.append_history(&history)?;
                            // Claude Code never echoes prompts; show one as soon as it is accepted.
                            for message in normalize_history(
                                &history,
                                self.sequence,
                                &self.config.session_dir,
                                &mut self.images,
                            )? {
                                self.emit(ConversationEvent::MessageUpdated(message));
                            }
                            if self.turn.id == Some(request_id) && !self.turn.accepted {
                                self.turn.accepted = true;
                                self.emit(ConversationEvent::SubmissionAccepted { request_id });
                            }
                        }
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
        // The process accepts prompts as soon as its stdin is open; `init` follows the first one.
        let models = self
            .config
            .model
            .iter()
            .map(|key| ModelChoice {
                key: key.clone(),
                name: key.id.clone(),
                input: vec!["text".into(), "image".into()],
                efforts: Vec::new(),
            })
            .collect();
        self.emit(ConversationEvent::Ready {
            session_ref: self.session.reference.clone(),
            models,
        });
        self.emit_history()?;
        self.state(ConversationState::Ready);
        // The `initialize` answer carries the model catalog before any prompt is sent.
        self.queue(
            json!({"type":"control_request","request_id":INITIALIZE_REQUEST,
                "request":{"subtype":"initialize"}}),
            None,
        )?;
        loop {
            while let Some(event) = self.events.pop_front() {
                match self.output.try_send(event) {
                    Ok(()) => {}
                    Err(async_channel::TrySendError::Full(event)) => {
                        self.events.push_front(event);
                        break;
                    }
                    Err(async_channel::TrySendError::Closed(_)) => {
                        self.closing = true;
                        self.events.clear();
                        break;
                    }
                }
            }
            ensure!(
                self.events.len() < 512,
                "Conversation display stopped consuming Claude Code events"
            );
            for _ in 0..16 {
                match self.commands.try_recv() {
                    Ok(command) => self.command(command)?,
                    Err(async_channel::TryRecvError::Empty) => break,
                    Err(async_channel::TryRecvError::Closed) => {
                        self.closing = true;
                        break;
                    }
                }
            }
            self.flush()?;
            if self.closing {
                self.child.stdin.take();
                self.settle(TurnOutcome::Unknown);
                self.state(ConversationState::Disconnected);
                return Ok(());
            }
            ensure!(
                self.initialized
                    || self
                        .init_deadline
                        .is_none_or(|deadline| Instant::now() < deadline),
                "Claude Code did not initialize within 30 seconds of the first prompt"
            );
            for _ in 0..16 {
                if self.events.len() >= 256 || !self.read_stdout()? {
                    break;
                }
            }
            if let Some(status) = self
                .child
                .child
                .try_wait()
                .context("Wait for Claude Code child")?
            {
                self.child.reaped = true;
                bail!("Claude Code process exited: {status}");
            }
            let mut error_bytes = [0; 4096];
            match self.child.stderr.read(&mut error_bytes) {
                Ok(n) if n > 0 => self.emit(ConversationEvent::Notice {
                    level: "diagnostic".into(),
                    text: String::from_utf8_lossy(&error_bytes[..n]).into_owned(),
                }),
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
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn read_stdout(&mut self) -> Result<bool> {
        if self.read_start == self.read_end {
            match self.child.stdout.read(&mut self.read_buffer) {
                Ok(0) => bail!("Claude Code disconnected (stdout EOF)"),
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
        ensure!(
            self.line.len() + length <= OUTPUT_FRAME,
            "Claude Code stdout frame exceeds 64 MiB"
        );
        self.line.extend_from_slice(&pending[..length]);
        self.read_start += length;
        if self.line.last() == Some(&b'\n') {
            let value: Value = serde_json::from_slice(&self.line)
                .context("Malformed Claude Code stdout JSON/UTF-8")?;
            ensure!(
                value.is_object(),
                "Claude Code stdout frame must be an object"
            );
            self.line.clear();
            self.frame(value)?;
        }
        Ok(true)
    }

    fn command(&mut self, command: ConversationCommand) -> Result<()> {
        if self.closing {
            return Ok(());
        }
        match command {
            ConversationCommand::Close => self.closing = true,
            ConversationCommand::LoadHistory { cursor } => {
                if cursor.is_none() {
                    self.emit_history()?;
                }
            }
            ConversationCommand::SelectModel(model) => {
                if self.turn.id.is_some() {
                    self.emit(ConversationEvent::Failed {
                        request_id: None,
                        message: "Model selection requires an idle Claude Code conversation".into(),
                    });
                } else {
                    let request_id = Uuid::new_v4().to_string();
                    self.queue(
                        json!({"type":"control_request","request_id":request_id,
                            "request":{"subtype":"set_model","model":model.id}}),
                        None,
                    )?;
                    self.model_requests.insert(request_id, model);
                }
            }
            ConversationCommand::Interrupt => {
                if self.turn.id.is_some() && !self.turn.interrupting {
                    self.turn.interrupting = true;
                    self.state(ConversationState::Interrupting);
                    let request_id = Uuid::new_v4().to_string();
                    self.queue(
                        json!({"type":"control_request","request_id":request_id,
                            "request":{"subtype":"interrupt"}}),
                        None,
                    )?;
                }
            }
            ConversationCommand::Respond { request_id, answer } => {
                if !self.permissions.contains_key(&request_id) {
                    return Ok(());
                }
                let Some(responses) =
                    permission_responses(&mut self.permissions, &request_id, &answer)
                else {
                    self.emit(ConversationEvent::Notice {
                        level: "error".into(),
                        text: "Claude Code request does not accept that answer".into(),
                    });
                    return Ok(());
                };
                for (request_id, response) in responses {
                    self.queue(
                        json!({"type":"control_response","response":{
                            "subtype":"success","request_id":request_id,"response":response
                        }}),
                        None,
                    )?;
                    self.emit(ConversationEvent::UserRequestDismissed { request_id });
                }
                self.state(if self.permissions.is_empty() {
                    ConversationState::Running
                } else {
                    ConversationState::WaitingForInput
                });
            }
            ConversationCommand::Submit {
                request_id,
                text,
                images,
            } => {
                let attempt = (|| -> Result<()> {
                    ensure!(
                        self.turn.id.is_none(),
                        "A conversation already has an active turn"
                    );
                    ensure!(
                        !text.trim().is_empty() || !images.is_empty(),
                        "A prompt needs text or images"
                    );
                    ensure!(
                        submission_size(request_id, &text, &images)? <= FRAME as u64,
                        "Prompt exceeds 1 MiB; explicitly optimize images or reduce the draft"
                    );
                    let mut content = vec![json!({"type":"text","text":text})];
                    for image in &images {
                        let bytes = crate::attachments::read_original(&image.path)?;
                        let (_, mime, _) = crate::attachments::decode(&bytes)?;
                        ensure!(
                            mime == image.mime,
                            "Attachment bytes do not match its MIME type"
                        );
                        content.push(json!({"type":"image","source":{
                            "type":"base64","media_type":mime,"data":STANDARD.encode(bytes)
                        }}));
                    }
                    let frame = json!({
                        "type":"user",
                        "message":{"role":"user","content":content},
                        "parent_tool_use_id":Value::Null,
                        "session_id":self.session.id,
                    });
                    let history = json!({
                        "type":"drostje_user",
                        "id":request_id,
                        "text":text,
                        "images":images,
                    });
                    self.queue(frame, Some((request_id, history)))?;
                    match IdleSleepAssertion::acquire("DROSTJE. conversation turn") {
                        Ok(assertion) => self.idle_sleep_assertion = Some(assertion),
                        Err(error) => self.emit(ConversationEvent::Notice {
                            level: "warning".into(),
                            text: format!(
                                "Could not prevent idle sleep during this conversation turn: {error:#}. Keep your Mac awake manually until the turn finishes."
                            ),
                        }),
                    }
                    if !self.initialized && self.init_deadline.is_none() {
                        self.init_deadline = Some(Instant::now() + READY_TIMEOUT);
                    }
                    self.turn = Turn {
                        id: Some(request_id),
                        accepted: false,
                        interrupting: false,
                    };
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

    fn emit_history(&mut self) -> Result<()> {
        self.emit(ConversationEvent::HistoryReset);
        let history = self.history.clone();
        let mut messages = Vec::new();
        for (index, frame) in history.iter().enumerate() {
            messages.extend(normalize_history(
                frame,
                index,
                &self.config.session_dir,
                &mut self.images,
            )?);
        }
        self.emit(ConversationEvent::History {
            messages,
            next_cursor: None,
        });
        Ok(())
    }

    fn frame(&mut self, frame: Value) -> Result<()> {
        match frame["type"].as_str().unwrap_or("") {
            "system" if frame["subtype"] == "init" => {
                let id = frame["session_id"]
                    .as_str()
                    .and_then(|id| Uuid::parse_str(id).ok())
                    .context("Claude Code init omitted its session ID")?;
                ensure!(
                    id == self.session.id,
                    "Claude Code initialized a different session"
                );
                self.initialized = true;
                let terminal: Vec<String> = frame["terminal_slash_commands"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                if terminal != self.terminal_commands {
                    self.terminal_commands = terminal;
                    if let Some(commands) = self.slash_commands.clone() {
                        self.commands(commands);
                    }
                }
                if !self.models_reported {
                    let models = frame["model"]
                        .as_str()
                        .filter(|model| !model.is_empty())
                        .map(|id| ModelChoice {
                            key: ModelKey {
                                provider: "anthropic".into(),
                                id: id.into(),
                            },
                            name: id.into(),
                            input: vec!["text".into(), "image".into()],
                            efforts: Vec::new(),
                        })
                        .into_iter()
                        .collect();
                    self.emit(ConversationEvent::Ready {
                        session_ref: self.session.reference.clone(),
                        models,
                    });
                }
            }
            "assistant" => {
                self.append_history(&frame)?;
                let message = normalize_assistant(&frame, self.sequence)?;
                for block in &message.content {
                    if let ContentBlock::ToolCall {
                        id,
                        name,
                        intent,
                        input,
                    } = block
                    {
                        let tool = ToolActivity {
                            id: id.clone(),
                            name: name.clone(),
                            intent: intent.clone(),
                            input: input.clone(),
                            result: Vec::new(),
                            error: false,
                            exit_code: None,
                            complete: false,
                            raw: Some(frame.clone()),
                        };
                        self.tools.insert(id.clone(), tool.clone());
                        self.emit(ConversationEvent::ToolUpdated(tool));
                    }
                }
                self.emit(ConversationEvent::MessageUpdated(message));
            }
            "user" => {
                self.append_history(&frame)?;
                for message in normalize_tool_results(
                    &frame,
                    self.sequence,
                    &self.config.session_dir,
                    &mut self.images,
                )? {
                    if let Some(id) = &message.tool_call_id {
                        let mut tool = self.tools.remove(id).unwrap_or_else(|| ToolActivity {
                            id: id.clone(),
                            name: "Tool".into(),
                            intent: None,
                            input: Value::Null,
                            result: Vec::new(),
                            error: false,
                            exit_code: None,
                            complete: false,
                            raw: None,
                        });
                        tool.result = message.content.clone();
                        tool.error = message.error;
                        tool.complete = true;
                        tool.raw = Some(frame.clone());
                        self.emit(ConversationEvent::ToolUpdated(tool));
                    }
                    self.emit(ConversationEvent::MessageUpdated(message));
                }
            }
            "control_request" if frame["request"]["subtype"] == "can_use_tool" => {
                let request_id = frame["request_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .context("Claude Code permission request omitted its ID")?
                    .to_owned();
                match permission_request(request_id.clone(), &frame["request"]) {
                    Ok((request, permission)) => {
                        self.permissions.insert(request_id, permission);
                        self.emit(ConversationEvent::UserRequest(request));
                        self.state(ConversationState::WaitingForInput);
                    }
                    Err(reason) => {
                        // A question shape DROSTJE cannot present is declined with the reason,
                        // visibly, so Claude Code can ask again in a supported form.
                        self.emit(ConversationEvent::Notice {
                            level: "warning".into(),
                            text: format!("Declined a Claude Code request: {reason}"),
                        });
                        self.queue(
                            json!({"type":"control_response","response":{
                                "subtype":"success","request_id":request_id,
                                "response":{"behavior":"deny","message":reason}
                            }}),
                            None,
                        )?;
                    }
                }
            }
            "result" => {
                let error = frame["is_error"].as_bool().unwrap_or(false)
                    || frame["subtype"]
                        .as_str()
                        .is_some_and(|kind| kind != "success");
                if error {
                    self.emit(ConversationEvent::Notice {
                        level: "error".into(),
                        text: result_error_text(&frame),
                    });
                    self.emit(ConversationEvent::Diagnostic {
                        label: "Claude Code result".into(),
                        raw: frame.clone(),
                    });
                }
                let outcome = if self.turn.interrupting {
                    TurnOutcome::Interrupted
                } else if error {
                    TurnOutcome::Failed
                } else {
                    TurnOutcome::Completed
                };
                self.settle(outcome);
                self.state(ConversationState::Ready);
                self.refresh_usage(&[CONTEXT_USAGE_REQUEST, USAGE_REQUEST])?;
            }
            // Claude Code pushes the whole list again whenever it changes.
            "system" if frame["subtype"] == "commands_changed" => {
                self.commands(frame["commands"].clone())
            }
            "control_response" => self.control_response(&frame)?,
            "rate_limit_event" => self.refresh_usage(&[USAGE_REQUEST])?,
            "keep_alive" => {}
            _ => self.emit(ConversationEvent::Diagnostic {
                label: "Claude Code event".into(),
                raw: frame,
            }),
        }
        Ok(())
    }

    fn control_response(&mut self, frame: &Value) -> Result<()> {
        let response = &frame["response"];
        let request_id = response["request_id"].as_str().unwrap_or("");
        if let Some(index) = self.usage_requests.iter().position(|id| *id == request_id) {
            let request_id = self.usage_requests.remove(index);
            let parsed = (response["subtype"] == "success")
                .then(|| &response["response"])
                .and_then(|answer| {
                    if request_id == CONTEXT_USAGE_REQUEST {
                        parse_context_usage(answer).map(ConversationEvent::ContextUsageUpdated)
                    } else {
                        parse_limits(answer).map(ConversationEvent::LimitsUpdated)
                    }
                });
            self.emit(parsed.unwrap_or_else(|| ConversationEvent::Diagnostic {
                label: "Claude Code usage".into(),
                raw: frame.clone(),
            }));
            return Ok(());
        }
        let error = (response["subtype"] != "success").then(|| {
            response["error"]
                .as_str()
                .unwrap_or("Claude Code rejected the request")
                .to_owned()
        });
        if request_id == INITIALIZE_REQUEST {
            match error {
                Some(error) => {
                    self.emit(ConversationEvent::Notice {
                        level: "error".into(),
                        text: format!("Claude Code model discovery failed: {error}"),
                    });
                    self.emit(ConversationEvent::CommandsUnavailable(error));
                }
                None => {
                    self.commands(response["response"]["commands"].clone());
                    let models = parse_models(&response["response"]["models"]);
                    if !models.is_empty() {
                        self.models_reported = true;
                        self.emit(ConversationEvent::Ready {
                            session_ref: self.session.reference.clone(),
                            models,
                        });
                    }
                    self.refresh_usage(&[CONTEXT_USAGE_REQUEST, USAGE_REQUEST])?;
                }
            }
        } else if let Some(model) = self.model_requests.remove(request_id) {
            match error {
                Some(error) => self.emit(ConversationEvent::Failed {
                    request_id: None,
                    message: format!("Claude Code did not switch to {}: {error}", model.id),
                }),
                None => {
                    self.emit(ConversationEvent::ModelSelected(model));
                    self.refresh_usage(&[CONTEXT_USAGE_REQUEST])?;
                }
            }
        }
        Ok(())
    }

    fn commands(&mut self, raw: Value) {
        let event = match parse_commands(&raw, &self.terminal_commands) {
            Some(commands) => ConversationEvent::CommandsUpdated(commands),
            None => ConversationEvent::CommandsUnavailable(
                "Claude Code did not list its commands".into(),
            ),
        };
        self.slash_commands = Some(raw);
        self.emit(event);
    }

    fn settle(&mut self, outcome: TurnOutcome) {
        self.idle_sleep_assertion = None;
        if let Some(request_id) = self.turn.id.take() {
            self.permissions.clear();
            self.emit(ConversationEvent::TurnSettled {
                request_id,
                outcome,
            });
        }
        self.turn = Turn::default();
    }

    fn disconnect(&mut self, message: String) {
        self.settle(TurnOutcome::Unknown);
        self.state(ConversationState::Failed);
        self.emit(ConversationEvent::Failed {
            request_id: None,
            message,
        });
    }
}

/// The `initialize` control response lists models as `{value, displayName, supportedEffortLevels?}`;
/// `value` is what `--model` and `set_model` accept.
fn parse_models(models: &Value) -> Vec<ModelChoice> {
    models
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model["value"].as_str().filter(|id| !id.is_empty())?;
            let name = model["displayName"]
                .as_str()
                .filter(|name| !name.is_empty())
                .unwrap_or(id);
            let efforts = model["supportedEffortLevels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            Some(ModelChoice {
                key: ModelKey {
                    provider: "anthropic".into(),
                    id: id.into(),
                },
                name: name.into(),
                input: vec!["text".into(), "image".into()],
                efforts,
            })
        })
        .collect()
}

/// Command entries are `{name, description, argumentHint, aliases?, builtin?}`. Plugin
/// commands are namespaced `plugin:name`; everything else is a skill or command file, which
/// Claude Code does not tell apart.
fn parse_commands(raw: &Value, terminal: &[String]) -> Option<Vec<SlashCommand>> {
    let commands = raw
        .as_array()?
        .iter()
        .filter_map(|entry| {
            let name = entry["name"].as_str().filter(|name| !name.is_empty())?;
            let aliases: Vec<String> = entry["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let builtin = entry["builtin"] == true;
            let source = if builtin {
                CommandSource::Builtin
            } else if name.contains(':') {
                CommandSource::Plugin
            } else {
                CommandSource::Custom
            };
            let blocked = if builtin && matches!(name, "clear" | "reset" | "new") {
                Some(super::commands::NEW_CONVERSATION)
            } else if builtin && name == "model" {
                Some(super::commands::MODEL_PICKER)
            } else if terminal.iter().any(|command| command == name) {
                Some("it only runs in the Claude Code terminal")
            } else {
                None
            };
            Some(SlashCommand {
                name: name.to_owned(),
                aliases,
                description: entry["description"].as_str().unwrap_or("").to_owned(),
                argument_hint: entry["argumentHint"]
                    .as_str()
                    .filter(|hint| !hint.is_empty())
                    .map(str::to_owned),
                source,
                blocked,
            })
        })
        .collect();
    Some(commands)
}

/// Error results carry their reason in `result`, `error` or (for execution errors) `errors`.
/// `get_context_usage` answer: `totalTokens` of `maxTokens`.
fn parse_context_usage(answer: &Value) -> Option<ContextUsage> {
    let used_tokens = answer["totalTokens"].as_u64()?;
    let max_tokens = answer["maxTokens"].as_u64().filter(|max| *max > 0)?;
    Some(ContextUsage {
        used_tokens,
        max_tokens,
    })
}

/// `get_usage` answer. `rate_limits_available: false` (API key, Bedrock, Vertex) means no windows;
/// a window without a numeric utilisation is left out rather than shown as zero.
fn parse_limits(answer: &Value) -> Option<Vec<UsageLimit>> {
    if answer["rate_limits_available"] == false {
        return Some(Vec::new());
    }
    let limits = answer["rate_limits"].as_object()?;
    Some(
        [
            ("five_hour", LimitWindow::FiveHour),
            ("seven_day", LimitWindow::SevenDay),
        ]
        .into_iter()
        .filter_map(|(key, window)| {
            let limit = limits.get(key)?;
            Some(UsageLimit {
                window,
                percent_used: limit["utilization"].as_f64()?.clamp(0., 100.) as f32,
                resets_at: limit["resets_at"].as_str().and_then(parse_timestamp),
            })
        })
        .collect(),
    )
}

/// RFC 3339 timestamp (`2026-09-16T18:00:00.5+02:00`) to `SystemTime`; `None` for anything else.
fn parse_timestamp(text: &str) -> Option<SystemTime> {
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = text.get(range)?;
        digits
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b' ')
    {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut rest = &text[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        rest = &fraction[digits..];
    }
    let offset = match rest.as_bytes() {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let hours: i64 = rest[1..3].parse().ok()?;
            let minutes: i64 = rest[4..6].parse().ok()?;
            let offset = hours * 3600 + minutes * 60;
            if *sign == b'-' { -offset } else { offset }
        }
        _ => return None,
    };
    // Days from civil date (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(seconds)
        .ok()
        .map(|seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
}

fn result_error_text(frame: &Value) -> String {
    if let Some(text) = frame["result"].as_str().or_else(|| frame["error"].as_str()) {
        return text.to_owned();
    }
    let errors = frame["errors"]
        .as_array()
        .map(|errors| {
            errors
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    if errors.is_empty() {
        "Claude Code turn failed".to_owned()
    } else {
        errors
    }
}

fn read_transcript(path: &Path) -> Result<Vec<Value>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    BufReader::new(file)
        .lines()
        .map(|line| {
            let line = line?;
            serde_json::from_str(&line).context("Malformed saved Claude Code transcript")
        })
        .collect()
}

fn permission_text(tool_name: &str, input: &Value) -> String {
    let detail = input["command"]
        .as_str()
        .or_else(|| input["file_path"].as_str())
        .or_else(|| input["path"].as_str());
    detail
        .map(|detail| format!("{tool_name}: {detail}"))
        .unwrap_or_else(|| format!("{tool_name}: {input}"))
}

const ALLOW: &str = "Allow";
const DENY: &str = "Deny";
const DENIED: &str = "The user denied this tool request.";

/// Maps a `can_use_tool` request to the choices shown to the user: allow once, one entry per
/// usable `permission_suggestions` item, deny. `AskUserQuestion` becomes a plain selection.
/// `Err` carries the reason a request cannot be presented here.
fn permission_request(
    request_id: String,
    request: &Value,
) -> std::result::Result<(UserRequest, Permission), String> {
    let tool_name = request["tool_name"].as_str().unwrap_or("tool");
    let input = request["input"].clone();
    if tool_name == "AskUserQuestion" {
        return question_request(request_id, input);
    }
    let display_name = request["display_name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .unwrap_or(tool_name);
    let choices: Vec<(String, Value)> = request["permission_suggestions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|suggestion| Some((describe_suggestion(suggestion)?, suggestion.clone())))
        .collect();
    let mut options = vec![UserOption {
        value: ALLOW.into(),
        description: Some("This time".into()),
    }];
    for (label, suggestion) in &choices {
        if options.iter().any(|option| option.value == *label) {
            continue;
        }
        options.push(UserOption {
            value: label.clone(),
            description: Some(destination_text(suggestion)),
        });
    }
    options.push(UserOption {
        value: DENY.into(),
        description: None,
    });
    let mut text = permission_text(tool_name, &input);
    if let Some(description) = request["description"]
        .as_str()
        .filter(|description| !description.is_empty())
    {
        text = format!("{description}\n{text}");
    }
    Ok((
        UserRequest {
            id: request_id,
            kind: UserRequestKind::Permission,
            title: format!("Allow {display_name}?"),
            options,
            text,
            placeholder: None,
            deadline: None,
        },
        Permission {
            input,
            choices,
            question: None,
        },
    ))
}

/// One single-choice question with options is a selection; anything else is declined so the
/// model asks again in that form rather than DROSTJE guessing an answer.
fn question_request(
    request_id: String,
    input: Value,
) -> std::result::Result<(UserRequest, Permission), String> {
    let questions = input["questions"].as_array().cloned().unwrap_or_default();
    let [question] = questions.as_slice() else {
        return Err(format!(
            "DROSTJE answers one question at a time; this request carried {}. \
             Ask again with one single-choice question per request.",
            questions.len()
        ));
    };
    if question["multiSelect"].as_bool().unwrap_or(false) {
        return Err(
            "DROSTJE answers single-choice questions only. Ask again without multiSelect.".into(),
        );
    }
    let options: Vec<UserOption> = question["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|option| {
            Some(UserOption {
                value: option["label"]
                    .as_str()
                    .filter(|label| !label.is_empty())?
                    .to_owned(),
                description: option["description"].as_str().map(str::to_owned),
            })
        })
        .collect();
    if options.is_empty() {
        return Err(
            "DROSTJE answers questions with labelled options only. Ask again with options.".into(),
        );
    }
    let text = question["question"].as_str().unwrap_or_default().to_owned();
    let title = question["header"]
        .as_str()
        .filter(|header| !header.is_empty())
        .unwrap_or("Question")
        .to_owned();
    Ok((
        UserRequest {
            id: request_id,
            kind: UserRequestKind::Question,
            title,
            options,
            text: text.clone(),
            placeholder: None,
            deadline: None,
        },
        Permission {
            input,
            choices: Vec::new(),
            question: Some(text),
        },
    ))
}

/// The `control_response` payload for an answer; `None` when it is not one of the offered choices.
fn permission_response(permission: &Permission, answer: &UserAnswer) -> Option<Value> {
    let deny = |message: &str| json!({"behavior":"deny","message":message});
    let allow = |suggestion: Option<&Value>| {
        let mut response = json!({"behavior":"allow","updatedInput":permission.input});
        if let Some(suggestion) = suggestion {
            response["updatedPermissions"] = json!([suggestion]);
        }
        response
    };
    match (answer, &permission.question) {
        (UserAnswer::Cancelled { .. }, Some(_)) => Some(deny("The user dismissed the question.")),
        (UserAnswer::Cancelled { .. } | UserAnswer::Confirmed(false), None) => Some(deny(DENIED)),
        (UserAnswer::Confirmed(true), None) => Some(allow(None)),
        (UserAnswer::Confirmed(_), Some(_)) => None,
        (UserAnswer::Value(value), Some(question)) => {
            let mut input = permission.input.clone();
            input["answers"] = json!({ question.as_str(): value });
            Some(json!({"behavior":"allow","updatedInput":input}))
        }
        (UserAnswer::PermissionAuto, None) => {
            let mut response = allow(None);
            response["updatedPermissions"] =
                json!([{"type":"setMode","mode":"auto","destination":"session"}]);
            Some(response)
        }
        (UserAnswer::Value(value), None) if value == ALLOW => Some(allow(None)),
        (UserAnswer::Value(value), None) if value == DENY => Some(deny(DENIED)),
        (UserAnswer::Value(value), None) => permission
            .choices
            .iter()
            .find(|(label, _)| label == value)
            .map(|(_, suggestion)| allow(Some(suggestion))),
        (UserAnswer::PermissionAuto, Some(_)) => None,
    }
}

/// Removes and answers one request. Auto approval also clears every other pending permission,
/// while AskUserQuestion entries remain for the user.
fn permission_responses(
    permissions: &mut HashMap<String, Permission>,
    request_id: &str,
    answer: &UserAnswer,
) -> Option<Vec<(String, Value)>> {
    let permission = permissions.remove(request_id)?;
    let Some(response) = permission_response(&permission, answer) else {
        permissions.insert(request_id.to_owned(), permission);
        return None;
    };
    let mut responses = vec![(request_id.to_owned(), response)];
    if matches!(answer, UserAnswer::PermissionAuto) {
        let mut pending: Vec<String> = permissions
            .iter()
            .filter(|(_, permission)| permission.question.is_none())
            .map(|(id, _)| id.clone())
            .collect();
        pending.sort();
        for id in pending {
            if let Some(permission) = permissions.remove(&id)
                && let Some(response) =
                    permission_response(&permission, &UserAnswer::Confirmed(true))
            {
                responses.push((id, response));
            }
        }
    }
    Some(responses)
}

/// Label for one `permission_suggestions` item, or `None` for kinds DROSTJE will not offer
/// (deny rules, `bypassPermissions`, `dontAsk`, unknown types).
fn describe_suggestion(suggestion: &Value) -> Option<String> {
    let scope = match suggestion["destination"].as_str().unwrap_or("session") {
        "session" => "this session",
        "userSettings" => "in every project",
        _ => "in this project",
    };
    Some(match suggestion["type"].as_str()? {
        "addRules" => {
            if suggestion["behavior"]
                .as_str()
                .is_some_and(|behavior| behavior != "allow")
            {
                return None;
            }
            let rules: Vec<String> = suggestion["rules"]
                .as_array()?
                .iter()
                .filter_map(|rule| {
                    let tool = rule["toolName"].as_str()?;
                    Some(match rule["ruleContent"].as_str() {
                        Some(content) => format!("{tool}({content})"),
                        None => tool.to_owned(),
                    })
                })
                .collect();
            if rules.is_empty() {
                return None;
            }
            format!("Allow and don't ask again for {} {scope}", rules.join(", "))
        }
        "setMode" => match suggestion["mode"].as_str()? {
            "acceptEdits" => format!("Allow and accept edits {scope}"),
            "plan" => format!("Allow and switch to plan mode {scope}"),
            "bypassPermissions" | "dontAsk" => return None,
            mode => format!("Allow and switch to {mode} mode {scope}"),
        },
        "addDirectories" => {
            let directories: Vec<&str> = suggestion["directories"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            if directories.is_empty() {
                return None;
            }
            format!(
                "Allow and grant access to {} {scope}",
                directories.join(", ")
            )
        }
        _ => return None,
    })
}

fn destination_text(suggestion: &Value) -> String {
    match suggestion["destination"].as_str().unwrap_or("session") {
        "session" => "Until this conversation closes".into(),
        "userSettings" => "Saved to ~/.claude/settings.json".into(),
        "projectSettings" => "Saved to .claude/settings.json".into(),
        "localSettings" => "Saved to .claude/settings.local.json".into(),
        other => format!("Saved to {other}"),
    }
}

fn normalize_history(
    frame: &Value,
    sequence: usize,
    directory: &Path,
    images: &mut ImageCache,
) -> Result<Vec<Message>> {
    match frame["type"].as_str().unwrap_or("") {
        "drostje_user" => {
            let images: Vec<ImageAttachment> = serde_json::from_value(frame["images"].clone())?;
            let mut content = Vec::new();
            if let Some(text) = frame["text"].as_str().filter(|text| !text.is_empty()) {
                content.push(ContentBlock::Text(text.into()));
            }
            content.extend(images.into_iter().map(ContentBlock::Image));
            Ok(vec![Message {
                id: frame["id"].as_str().unwrap_or("user").into(),
                role: "user".into(),
                content,
                raw: Some(frame.clone()),
                tool_call_id: None,
                error: false,
                exit_code: None,
            }])
        }
        "assistant" => Ok(vec![normalize_assistant(frame, sequence)?]),
        "user" => normalize_tool_results(frame, sequence, directory, images),
        _ => Ok(Vec::new()),
    }
}

fn normalize_assistant(frame: &Value, sequence: usize) -> Result<Message> {
    let message = frame
        .get("message")
        .context("Claude Code assistant event omitted message")?;
    let blocks = message["content"]
        .as_array()
        .context("Claude Code assistant content is not an array")?;
    let mut content = Vec::new();
    for block in blocks {
        match block["type"].as_str().unwrap_or("") {
            "text" => content.push(ContentBlock::Text(
                block["text"].as_str().unwrap_or_default().into(),
            )),
            "thinking" => content.push(ContentBlock::Thinking(
                block["thinking"].as_str().unwrap_or_default().into(),
            )),
            "tool_use" | "server_tool_use" => content.push(ContentBlock::ToolCall {
                id: block["id"].as_str().unwrap_or("tool").into(),
                name: block["name"].as_str().unwrap_or("Tool").into(),
                intent: block["input"]["description"]
                    .as_str()
                    .or_else(|| block["input"]["command"].as_str())
                    .map(str::to_owned),
                input: block["input"].clone(),
            }),
            _ => content.push(ContentBlock::Text(block.to_string())),
        }
    }
    Ok(Message {
        // One `assistant` frame per content block, all sharing the API `message.id`; only the
        // frame `uuid` is unique, and keying on the shared ID would replace earlier blocks.
        id: frame["uuid"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("claude-assistant-{sequence}")),
        // A local slash command (`/context`, `/reload-skills`) answers with a synthetic
        // assistant frame; like OMP's command output it is not a model reply.
        role: if frame.get("local_command_run").is_some() {
            "system"
        } else {
            "assistant"
        }
        .into(),
        content,
        raw: Some(frame.clone()),
        tool_call_id: None,
        error: false,
        exit_code: None,
    })
}

fn normalize_tool_results(
    frame: &Value,
    sequence: usize,
    directory: &Path,
    images: &mut ImageCache,
) -> Result<Vec<Message>> {
    let Some(blocks) = frame["message"]["content"].as_array() else {
        return Ok(Vec::new());
    };
    let mut messages = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        if block["type"] != "tool_result" {
            continue;
        }
        let content = match &block["content"] {
            Value::String(text) => vec![ContentBlock::Text(text.clone())],
            Value::Array(parts) => parts
                .iter()
                .map(
                    |part| match (part["type"].as_str(), part["text"].as_str()) {
                        (_, Some(text)) => ContentBlock::Text(text.into()),
                        // `Read` on an image answers with the image itself.
                        (Some("image"), None) => match tool_result_image(part, directory, images) {
                            Ok(image) => ContentBlock::Image(image),
                            Err(error) => ContentBlock::ImageError(format!("{error:#}")),
                        },
                        _ => ContentBlock::Text(part.to_string()),
                    },
                )
                .collect(),
            value => vec![ContentBlock::Text(value.to_string())],
        };
        let call_id = block["tool_use_id"].as_str().unwrap_or("tool").to_owned();
        messages.push(Message {
            id: frame["uuid"]
                .as_str()
                .map(|id| format!("{id}-{index}"))
                .unwrap_or_else(|| format!("claude-tool-result-{sequence}-{index}")),
            role: "toolResult".into(),
            content,
            raw: Some(frame.clone()),
            tool_call_id: Some(call_id),
            error: block["is_error"].as_bool().unwrap_or(false),
            exit_code: None,
        });
    }
    Ok(messages)
}

fn tool_result_image(
    part: &Value,
    directory: &Path,
    images: &mut ImageCache,
) -> Result<ImageAttachment> {
    let source = &part["source"];
    ensure!(
        source["type"] == "base64",
        "Unsupported tool result image source: {}",
        source["type"]
    );
    let mime = source["media_type"]
        .as_str()
        .context("Tool result image has no media type")?;
    let encoded = source["data"]
        .as_str()
        .context("Tool result image has no data")?;
    images.import(mime, encoded, directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_messages_preserve_text_tools_and_results() -> Result<()> {
        let assistant = json!({
            "type":"assistant",
            "uuid":"event-1",
            "message":{"id":"message-1","content":[
                {"type":"text","text":"Checking"},
                {"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"cargo check"}}
            ]}
        });
        let message = normalize_assistant(&assistant, 0)?;
        assert_eq!(message.id, "event-1");
        assert!(matches!(&message.content[0], ContentBlock::Text(text) if text == "Checking"));
        assert!(
            matches!(&message.content[1], ContentBlock::ToolCall { id, name, .. } if id == "tool-1" && name == "Bash")
        );

        let result = json!({
            "type":"user",
            "uuid":"event-2",
            "message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","is_error":true,"content":"failed"}]}
        });
        let messages = normalize_tool_results(
            &result,
            1,
            Path::new("/nonexistent"),
            &mut ImageCache::default(),
        )?;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("tool-1"));
        assert!(messages[0].error);
        Ok(())
    }

    #[test]
    fn assistant_frames_sharing_a_message_id_stay_separate() -> Result<()> {
        let frames = [
            json!({"type":"assistant","uuid":"event-1","message":{"id":"msg_1","content":[
                {"type":"text","text":"Let me check"}]}}),
            json!({"type":"assistant","uuid":"event-2","message":{"id":"msg_1","content":[
                {"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"ls"}}]}}),
            json!({"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"x"}]}}),
        ];
        let ids: Vec<String> = frames
            .iter()
            .enumerate()
            .map(|(index, frame)| normalize_assistant(frame, index).map(|message| message.id))
            .collect::<Result<_>>()?;
        assert_eq!(ids, ["event-1", "event-2", "claude-assistant-2"]);
        let mut images = ImageCache::default();
        let history: Vec<String> = frames
            .iter()
            .enumerate()
            .flat_map(|(index, frame)| {
                normalize_history(frame, index, Path::new("/nonexistent"), &mut images)
                    .unwrap_or_default()
            })
            .map(|message| message.id)
            .collect();
        assert_eq!(history, ids, "reloaded history keeps live message IDs");
        Ok(())
    }

    #[test]
    fn tool_result_images_become_viewable_images() -> Result<()> {
        let root = std::env::temp_dir().join(format!("drostje-claude-image-{}", Uuid::new_v4()));
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(4, 3)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        let result = json!({
            "type":"user",
            "uuid":"event-1",
            "message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","content":[
                {"type":"text","text":"Read 1 image"},
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":STANDARD.encode(&png)}},
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"not base64"}},
                {"type":"document","title":"kept raw"}
            ]}]}
        });
        let mut images = ImageCache::default();
        let messages = normalize_tool_results(&result, 0, &root, &mut images)?;
        let content = &messages[0].content;
        assert!(matches!(&content[0], ContentBlock::Text(text) if text == "Read 1 image"));
        let ContentBlock::Image(image) = &content[1] else {
            panic!("expected an image, got {:?}", content[1]);
        };
        assert_eq!((image.width, image.height), (4, 3));
        assert!(image.path.starts_with(root.join("native-images")));
        assert_eq!(fs::read(&image.path)?, png);
        assert!(matches!(&content[2], ContentBlock::ImageError(error) if error.contains("base64")));
        assert!(matches!(&content[3], ContentBlock::Text(text) if text.contains("kept raw")));

        let again = normalize_tool_results(&result, 0, &root, &mut images)?;
        assert!(
            matches!(&again[0].content[1], ContentBlock::Image(same) if same.path == image.path),
            "reloading history reuses the stored copy"
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn permission_requests_offer_suggestions_that_answers_echo_back() {
        let request = json!({"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash",
            "input":{"command":"mkdir -p sub","description":"Create sub directory"},
            "description":"Create sub directory",
            "permission_suggestions":[
                {"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"mkdir -p sub"}],
                 "behavior":"allow","destination":"localSettings"},
                {"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"rm -rf"}],
                 "behavior":"deny","destination":"localSettings"},
                {"type":"setMode","mode":"bypassPermissions","destination":"session"},
                {"type":"setMode","mode":"acceptEdits","destination":"session"},
                {"type":"unknownKind"}
            ],
            "tool_use_id":"tool-1"});
        let (shown, permission) =
            permission_request("permission-1".into(), &request).expect("presentable");
        assert_eq!(shown.kind, UserRequestKind::Permission);
        assert_eq!(shown.title, "Allow Bash?");
        assert_eq!(shown.text, "Create sub directory\nBash: mkdir -p sub");
        let labels: Vec<&str> = shown.options.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Allow",
                "Allow and don't ask again for Bash(mkdir -p sub) in this project",
                "Allow and accept edits this session",
                "Deny",
            ]
        );
        assert_eq!(
            shown.options[1].description.as_deref(),
            Some("Saved to .claude/settings.local.json")
        );

        let once = permission_response(&permission, &UserAnswer::Value("Allow".into())).unwrap();
        assert_eq!(once["behavior"], "allow");
        assert_eq!(once["updatedInput"]["command"], "mkdir -p sub");
        assert!(once.get("updatedPermissions").is_none());
        let always =
            permission_response(&permission, &UserAnswer::Value(labels[1].into())).unwrap();
        assert_eq!(always["behavior"], "allow");
        assert_eq!(
            always["updatedPermissions"],
            json!([request["permission_suggestions"][0]])
        );
        let denied = permission_response(&permission, &UserAnswer::Value("Deny".into())).unwrap();
        assert_eq!(denied["behavior"], "deny");
        assert_eq!(
            permission_response(&permission, &UserAnswer::Cancelled { timed_out: false }).unwrap()
                ["behavior"],
            "deny"
        );
        assert_eq!(
            permission_response(&permission, &UserAnswer::Confirmed(true)).unwrap()["behavior"],
            "allow"
        );
        assert!(permission_response(&permission, &UserAnswer::Value("Maybe".into())).is_none());
        let auto = permission_response(&permission, &UserAnswer::PermissionAuto).unwrap();
        assert_eq!(auto["behavior"], "allow");
        assert_eq!(
            auto["updatedPermissions"],
            json!([{"type":"setMode","mode":"auto","destination":"session"}])
        );
    }

    #[test]
    fn auto_permission_approves_pending_permissions_but_leaves_questions_open() {
        let mut permissions = HashMap::from([
            (
                "current".into(),
                Permission {
                    input: json!({"command":"cargo check"}),
                    choices: Vec::new(),
                    question: None,
                },
            ),
            (
                "other".into(),
                Permission {
                    input: json!({"command":"cargo test"}),
                    choices: Vec::new(),
                    question: None,
                },
            ),
            (
                "question".into(),
                Permission {
                    input: json!({"questions":[]}),
                    choices: Vec::new(),
                    question: Some("Which option?".into()),
                },
            ),
        ]);

        let responses =
            permission_responses(&mut permissions, "current", &UserAnswer::PermissionAuto).unwrap();

        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].0, "current");
        assert_eq!(
            responses[0].1["updatedPermissions"],
            json!([{"type":"setMode","mode":"auto","destination":"session"}])
        );
        assert_eq!(responses[1].0, "other");
        assert!(responses[1].1.get("updatedPermissions").is_none());
        assert_eq!(
            permissions.keys().map(String::as_str).collect::<Vec<_>>(),
            ["question"]
        );
    }

    #[test]
    fn single_choice_questions_accept_offered_and_custom_answers() {
        let question = |questions: Value| {
            json!({"subtype":"can_use_tool","tool_name":"AskUserQuestion",
                "input":{"questions":questions},"tool_use_id":"tool-1"})
        };
        let (shown, permission) = permission_request(
            "q-1".into(),
            &question(
                json!([{"question":"Which colour?","header":"Colour","multiSelect":false,
                "options":[{"label":"Red","description":"Warm"},{"label":"Blue"}]}]),
            ),
        )
        .expect("presentable");
        assert_eq!(shown.kind, UserRequestKind::Question);
        assert_eq!(shown.title, "Colour");
        assert_eq!(shown.text, "Which colour?");
        assert_eq!(shown.options.len(), 2);
        assert_eq!(shown.options[0].description.as_deref(), Some("Warm"));
        let answer = permission_response(&permission, &UserAnswer::Value("Blue".into())).unwrap();
        assert_eq!(answer["behavior"], "allow");
        assert_eq!(
            answer["updatedInput"]["answers"],
            json!({"Which colour?":"Blue"})
        );
        assert_eq!(
            answer["updatedInput"]["questions"].as_array().map(Vec::len),
            Some(1)
        );
        assert!(permission_response(&permission, &UserAnswer::PermissionAuto).is_none());
        let custom =
            permission_response(&permission, &UserAnswer::Value("Something else".into())).unwrap();
        assert_eq!(
            custom["updatedInput"]["answers"],
            json!({"Which colour?":"Something else"})
        );
        assert!(permission_response(&permission, &UserAnswer::Confirmed(true)).is_none());
        assert_eq!(
            permission_response(&permission, &UserAnswer::Cancelled { timed_out: true }).unwrap()["behavior"],
            "deny"
        );

        let two = question(json!([{"question":"A","options":[{"label":"1"}]},
            {"question":"B","options":[{"label":"2"}]}]));
        assert!(
            permission_request("q-2".into(), &two)
                .unwrap_err()
                .contains("carried 2")
        );
        let multi =
            question(json!([{"question":"A","multiSelect":true,"options":[{"label":"1"}]}]));
        assert!(
            permission_request("q-3".into(), &multi)
                .unwrap_err()
                .contains("multiSelect")
        );
        let free = question(json!([{"question":"A","options":[]}]));
        assert!(
            permission_request("q-4".into(), &free)
                .unwrap_err()
                .contains("options")
        );
    }

    #[test]
    fn outgoing_budget_counts_claude_image_envelope_and_base64() -> Result<()> {
        let id = Uuid::nil();
        let actual = json!({
            "type":"user",
            "message":{"role":"user","content":[
                {"type":"text","text":"日本語"},
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":STANDARD.encode([1,2,3,4])}}
            ]},
            "parent_tool_use_id":Value::Null,
            "session_id":id,
        });
        assert_eq!(
            submission_size_for_lengths(id, "日本語", &[("image/png", 4)])?,
            serde_json::to_vec(&actual)?.len() as u64 + 1
        );
        Ok(())
    }

    #[test]
    fn saved_transcript_reconstructs_user_and_tool_history() -> Result<()> {
        let user = json!({"type":"drostje_user","id":Uuid::nil(),"text":"Fix it","images":[]});
        let messages = normalize_history(
            &user,
            0,
            Path::new("/nonexistent"),
            &mut ImageCache::default(),
        )?;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert!(matches!(&messages[0].content[0], ContentBlock::Text(text) if text == "Fix it"));
        Ok(())
    }

    #[test]
    fn initialize_catalog_maps_values_names_and_effort_levels() {
        let models = parse_models(&json!([
            {"value":"default","resolvedModel":"claude-opus-5[1m]","displayName":"Default (recommended)",
             "supportsEffort":true,"supportedEffortLevels":["low","medium","high","xhigh","max"]},
            {"value":"haiku","resolvedModel":"claude-haiku-4-5-20251001","displayName":"Haiku"},
            {"displayName":"No value"},
        ]));
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].key.provider, "anthropic");
        assert_eq!(models[0].key.id, "default");
        assert_eq!(models[0].name, "Default (recommended)");
        assert_eq!(models[0].efforts.len(), 5);
        assert_eq!(models[1].key.id, "haiku");
        assert!(models[1].efforts.is_empty());
    }

    #[test]
    fn usage_answers_map_context_and_plan_windows() {
        assert_eq!(
            parse_context_usage(&json!({"totalTokens":84_000,"maxTokens":200_000,"percentage":42})),
            Some(ContextUsage {
                used_tokens: 84_000,
                max_tokens: 200_000
            })
        );
        assert_eq!(
            parse_context_usage(&json!({"totalTokens":1,"maxTokens":0})),
            None
        );
        assert_eq!(parse_context_usage(&json!({})), None);

        let limits = parse_limits(&json!({
            "rate_limits_available": true,
            "rate_limits": {
                "five_hour": {"utilization": 23.5, "resets_at": "2026-09-16T18:00:00.250+02:00"},
                "seven_day": {"utilization": null, "resets_at": null},
                "seven_day_opus": {"utilization": 90, "resets_at": null}
            }
        }))
        .expect("limits");
        assert_eq!(
            limits,
            [UsageLimit {
                window: LimitWindow::FiveHour,
                percent_used: 23.5,
                resets_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_789_574_400)),
            }]
        );
        assert_eq!(
            parse_limits(&json!({"rate_limits_available": false, "rate_limits": null})),
            Some(Vec::new())
        );
        assert_eq!(parse_limits(&json!({"rate_limits_available": true})), None);
    }

    #[test]
    fn timestamps_accept_rfc3339_only() {
        assert_eq!(
            parse_timestamp("1970-01-02T00:00:00Z"),
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(86_400))
        );
        assert_eq!(
            parse_timestamp("2000-03-01T00:00:00-01:00"),
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(951_868_800 + 3_600))
        );
        assert_eq!(parse_timestamp("2026-09-16"), None);
        assert_eq!(parse_timestamp("2026-13-16T18:00:00Z"), None);
        assert_eq!(parse_timestamp("2026-09-16T18:00:00"), None);
    }

    #[test]
    fn error_result_reports_execution_errors() {
        let frame = json!({"type":"result","subtype":"error_during_execution","is_error":true,
            "errors":["No conversation found with session ID: x","second"]});
        assert_eq!(
            result_error_text(&frame),
            "No conversation found with session ID: x; second"
        );
        assert_eq!(
            result_error_text(&json!({"type":"result","is_error":true})),
            "Claude Code turn failed"
        );
        assert_eq!(
            result_error_text(&json!({"type":"result","is_error":true,"result":"Denied"})),
            "Denied"
        );
    }

    #[test]
    fn saved_session_resumes_only_with_a_local_transcript() -> Result<()> {
        let root = std::env::temp_dir().join(format!("drostje-claude-{}", Uuid::new_v4()));
        let cwd = root.join("repo");
        let session_dir = root.join("session");
        fs::create_dir_all(&cwd)?;
        fs::create_dir_all(&session_dir)?;
        let id = Uuid::new_v4();
        let reference = serde_json::to_string(&json!({
            "id":id,
            "cwd":fs::canonicalize(&cwd)?,
            "session_dir":fs::canonicalize(&session_dir)?,
        }))?;
        let config = || ConversationConfig {
            harness: "claude-code".into(),
            executable: root.join("claude"),
            cwd: cwd.clone(),
            session_dir: session_dir.clone(),
            session_ref: Some(reference.clone()),
            model: None,
            effort: None,
            permission_mode: None,
            browser_tools: None,
            allow_empty_session: true,
        };

        let fresh = session(&mut config())?;
        assert_eq!(fresh.id, id);
        assert!(!fresh.resume, "an unstored session must not be resumed");

        fs::write(
            session_dir.join(TRANSCRIPT),
            format!(
                "{}\n",
                json!({"type":"drostje_user","id":Uuid::nil(),"text":"Hi","images":[]})
            ),
        )?;
        let stored = session(&mut config())?;
        assert_eq!(stored.id, id);
        assert!(stored.resume);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn fake_cli_exercises_session_prompt_permission_and_completion() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("drostje-claude-{}", Uuid::new_v4()));
        let cwd = root.join("repo");
        let session_dir = root.join("session");
        fs::create_dir_all(&cwd)?;
        let executable = root.join("claude");
        fs::write(
            &executable,
            r#"#!/bin/sh
session=
printf '%s\n' "$@" > "$(dirname "$0")/args.txt"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --session-id|--resume) session="$2"; shift 2 ;;
    *) shift ;;
  esac
done
while IFS= read -r line; do
  case "$line" in
    *'"subtype":"initialize"'*)
      printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"drostje-initialize","response":{"models":[{"value":"default","displayName":"Default (recommended)","supportedEffortLevels":["low","high"]},{"value":"haiku","displayName":"Haiku"}]}}}'
      ;;
    *'"subtype":"get_context_usage"'*)
      printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"drostje-context-usage","response":{"totalTokens":84000,"maxTokens":200000,"percentage":42}}}'
      ;;
    *'"subtype":"get_usage"'*)
      printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"drostje-usage","response":{"rate_limits_available":true,"rate_limits":{"five_hour":{"utilization":23,"resets_at":null},"seven_day":{"utilization":61,"resets_at":"2026-09-20T00:00:00Z"}}}}}'
      ;;
    *'"subtype":"set_model"'*)
      id=$(printf '%s' "$line" | sed 's/.*"request_id":"\([^"]*\)".*/\1/')
      case "$line" in
        *'"model":"haiku"'*) printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s"}}\n' "$id" ;;
        *) printf '{"type":"control_response","response":{"subtype":"error","request_id":"%s","error":"Model not found"}}\n' "$id" ;;
      esac
      ;;
    *'"type":"user"'*)
      printf '{"type":"system","subtype":"init","session_id":"%s","model":"claude-test"}\n' "$session"
      printf '%s\n' '{"type":"control_request","request_id":"permission-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo check"},"permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"cargo check"}],"behavior":"allow","destination":"localSettings"}]}}'
      ;;
    *'"updatedPermissions":[{"type":"addRules"'*)
      printf '%s\n' '{"type":"assistant","uuid":"event-1","message":{"id":"message-1","content":[{"type":"text","text":"Checking"}]}}'
      printf '%s\n' '{"type":"assistant","uuid":"event-2","message":{"id":"message-1","content":[{"type":"text","text":"Done"}]}}'
      printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"Done"}'
      ;;
    *'"type":"control_response"'*)
      printf '%s\n' '{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["permission answer lacked updatedPermissions"]}'
      ;;
  esac
done
"#,
        )?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;

        let handle = smol::block_on(open(ConversationConfig {
            harness: "claude-code".into(),
            executable: executable.clone(),
            cwd,
            session_dir: session_dir.clone(),
            session_ref: None,
            model: None,
            effort: Some("low".into()),
            permission_mode: Some("acceptEdits".into()),
            browser_tools: None,
            allow_empty_session: true,
        }))?;
        let receive = || {
            smol::block_on(smol::future::race(
                async { handle.events.recv().await.ok() },
                async {
                    smol::Timer::after(Duration::from_secs(5)).await;
                    None
                },
            ))
            .context("Timed out waiting for fake Claude Code event")
        };

        let mut catalog = Vec::new();
        let mut context = None;
        let mut limits = None;
        while catalog.len() != 2 || context.is_none() || limits.is_none() {
            match receive()? {
                ConversationEvent::Ready { models, .. } => catalog = models,
                ConversationEvent::ContextUsageUpdated(usage) => context = Some(usage),
                ConversationEvent::LimitsUpdated(windows) => limits = Some(windows),
                _ => {}
            }
        }
        assert_eq!(
            context,
            Some(ContextUsage {
                used_tokens: 84_000,
                max_tokens: 200_000
            })
        );
        let limits = limits.unwrap_or_default();
        assert_eq!(
            limits
                .iter()
                .map(|limit| (limit.window, limit.percent_used))
                .collect::<Vec<_>>(),
            [(LimitWindow::FiveHour, 23.), (LimitWindow::SevenDay, 61.)]
        );
        assert!(limits[1].resets_at.is_some());
        assert_eq!(catalog[0].name, "Default (recommended)");
        assert_eq!(catalog[0].efforts, ["low", "high"]);
        assert_eq!(catalog[1].key.id, "haiku");
        assert!(catalog[1].efforts.is_empty());
        let args = fs::read_to_string(root.join("args.txt"))?;
        assert!(args.contains("--effort\nlow\n"), "argv: {args}");
        assert!(
            args.contains("--permission-prompt-tool\nstdio\n"),
            "argv: {args}"
        );
        assert!(
            args.contains("--permission-mode\nacceptEdits\n"),
            "argv: {args}"
        );

        let haiku = catalog[1].key.clone();
        handle
            .commands
            .send_blocking(ConversationCommand::SelectModel(haiku.clone()))?;
        loop {
            match receive()? {
                ConversationEvent::ModelSelected(model) => {
                    assert_eq!(model, haiku);
                    break;
                }
                ConversationEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        handle
            .commands
            .send_blocking(ConversationCommand::SelectModel(ModelKey {
                provider: "anthropic".into(),
                id: "bogus".into(),
            }))?;
        loop {
            match receive()? {
                ConversationEvent::Failed { message, .. } => {
                    assert!(message.contains("Model not found"), "{message}");
                    break;
                }
                ConversationEvent::ModelSelected(model) => panic!("selected {model:?}"),
                _ => {}
            }
        }
        let request_id = Uuid::new_v4();
        handle.commands.send_blocking(ConversationCommand::Submit {
            request_id,
            text: "Check it".into(),
            images: Vec::new(),
        })?;

        let mut accepted = false;
        let mut echoed = false;
        let mut permission = None;
        while permission.is_none() {
            match receive()? {
                ConversationEvent::SubmissionAccepted { request_id: seen } => {
                    accepted = seen == request_id;
                }
                ConversationEvent::MessageUpdated(message) => {
                    echoed = message.role == "user"
                        && message.id == request_id.to_string()
                        && matches!(
                            message.content.first(),
                            Some(ContentBlock::Text(text)) if text == "Check it"
                        );
                }
                ConversationEvent::UserRequest(request) => {
                    assert_eq!(request.id, "permission-1");
                    assert_eq!(request.kind, UserRequestKind::Permission);
                    permission = Some(request);
                }
                _ => {}
            }
        }
        assert!(accepted);
        assert!(echoed, "the accepted prompt is shown before the reply");
        let permission = permission.unwrap();
        assert_eq!(permission.options.len(), 3, "{:?}", permission.options);
        handle
            .commands
            .send_blocking(ConversationCommand::Respond {
                request_id: "permission-1".into(),
                answer: UserAnswer::Value(permission.options[1].value.clone()),
            })?;

        let mut assistant = Vec::new();
        let mut completed = false;
        while !completed {
            match receive()? {
                ConversationEvent::MessageUpdated(message) if message.role == "assistant" => {
                    assistant.push(message.id.clone());
                }
                ConversationEvent::TurnSettled {
                    request_id: seen,
                    outcome,
                } => {
                    assert_eq!(seen, request_id);
                    assert_eq!(outcome, TurnOutcome::Completed);
                    completed = true;
                }
                _ => {}
            }
        }
        assert_eq!(
            assistant,
            ["event-1", "event-2"],
            "both blocks of one API message reach the display"
        );
        handle.commands.send_blocking(ConversationCommand::Close)?;

        let history = read_transcript(&session_dir.join(TRANSCRIPT))?;
        assert!(history.iter().any(|frame| frame["type"] == "drostje_user"));
        assert!(history.iter().any(|frame| frame["type"] == "assistant"));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn command_list_marks_commands_that_would_replace_drostje_state() {
        let raw = json!([
            {"name":"compact","description":"Free up context","argumentHint":"<optional custom summarization instructions>","builtin":true},
            {"name":"clear","description":"Start a new session","argumentHint":"[name]","aliases":["reset","new"],"builtin":true},
            {"name":"model","description":"Set the AI model","argumentHint":"[model]","builtin":true},
            {"name":"doctor","description":"Diagnose","argumentHint":"","builtin":true},
            {"name":"caveman:caveman-commit","description":"(caveman) Commit messages","argumentHint":"","aliases":["caveman-commit"]},
            {"name":"probecmd","description":"Probe command (project)","argumentHint":"foo"},
            {"description":"nameless"}
        ]);
        let commands = parse_commands(&raw, &["doctor".to_owned()]).unwrap();
        assert_eq!(commands.len(), 6);
        assert_eq!(
            commands[0].argument_hint.as_deref(),
            Some("<optional custom summarization instructions>")
        );
        assert_eq!(commands[0].blocked, None);
        assert_eq!(commands[1].aliases, ["reset", "new"]);
        assert!(commands[1].blocked.is_some());
        assert!(commands[2].blocked.is_some());
        assert!(commands[3].blocked.is_some(), "terminal-only");
        assert_eq!(commands[4].source, CommandSource::Plugin);
        assert_eq!(commands[4].argument_hint, None);
        assert_eq!(commands[5].source, CommandSource::Custom);
        assert_eq!(commands[5].blocked, None);
        assert!(parse_commands(&json!(null), &[]).is_none());
    }

    #[test]
    fn local_command_output_is_not_a_model_reply() {
        let local = json!({"type":"assistant","uuid":"u1",
            "message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Reloaded skills: 220 skills available (no changes)"}]},
            "local_command_run":{"command":"reload-skills","args":""}});
        let message = normalize_assistant(&local, 0).unwrap();
        assert_eq!(message.role, "system");
        assert!(
            matches!(&message.content[..], [ContentBlock::Text(text)] if text.starts_with("Reloaded skills"))
        );
        let reply = json!({"type":"assistant","uuid":"u2",
            "message":{"role":"assistant","content":[{"type":"text","text":"Done"}]}});
        assert_eq!(normalize_assistant(&reply, 1).unwrap().role, "assistant");
    }
}
