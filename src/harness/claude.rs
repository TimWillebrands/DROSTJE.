use super::*;
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

fn prepare(config: &mut ConversationConfig) -> Result<(OwnedChild, Session)> {
    let session = session(config)?;
    let mut command = Command::new(&config.executable);
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
        ])
        .current_dir(&config.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    Ok((owned, session))
}

pub(super) async fn open(mut config: ConversationConfig) -> Result<ConversationHandle> {
    let (commands, input) = async_channel::bounded(64);
    let (output, events) = async_channel::bounded(128);
    smol::unblock(move || -> Result<()> {
        let (child, session) = prepare(&mut config)?;
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

struct Permission {
    input: Value,
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
    permissions: HashMap<String, Permission>,
    tools: HashMap<String, ToolActivity>,
    /// `set_model` control requests awaiting the CLI's answer.
    model_requests: HashMap<String, ModelKey>,
    /// The `initialize` answer carried the model catalog, so `init` need not report one.
    models_reported: bool,
    /// Usage control requests awaiting an answer, so refresh triggers never pile up.
    usage_requests: Vec<&'static str>,
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
            permissions: HashMap::new(),
            tools: HashMap::new(),
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
                if let Some(permission) = self.permissions.remove(&request_id) {
                    let response = match answer {
                        UserAnswer::Confirmed(true) => json!({
                            "behavior":"allow",
                            "updatedInput":permission.input,
                        }),
                        UserAnswer::Confirmed(false) | UserAnswer::Cancelled { .. } => json!({
                            "behavior":"deny",
                            "message":"The user denied this tool request.",
                        }),
                        UserAnswer::Value(_) => {
                            self.permissions.insert(request_id, permission);
                            self.emit(ConversationEvent::Notice {
                                level: "error".into(),
                                text: "Claude Code permission requests require Allow or Deny"
                                    .into(),
                            });
                            return Ok(());
                        }
                    };
                    self.queue(
                        json!({"type":"control_response","response":{
                            "subtype":"success","request_id":request_id,"response":response
                        }}),
                        None,
                    )?;
                    self.emit(ConversationEvent::UserRequestDismissed { request_id });
                    self.state(ConversationState::Running);
                }
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
            messages.extend(normalize_history(frame, index)?);
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
                for message in normalize_tool_results(&frame, self.sequence)? {
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
                let tool_name = frame["request"]["tool_name"]
                    .as_str()
                    .unwrap_or("tool")
                    .to_owned();
                let input = frame["request"]["input"].clone();
                self.permissions.insert(
                    request_id.clone(),
                    Permission {
                        input: input.clone(),
                    },
                );
                self.emit(ConversationEvent::UserRequest(UserRequest {
                    id: request_id,
                    kind: UserRequestKind::Confirm,
                    title: format!("Allow {tool_name}?"),
                    options: Vec::new(),
                    text: permission_text(&tool_name, &input),
                    placeholder: None,
                    deadline: None,
                }));
                self.state(ConversationState::WaitingForInput);
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
                Some(error) => self.emit(ConversationEvent::Notice {
                    level: "error".into(),
                    text: format!("Claude Code model discovery failed: {error}"),
                }),
                None => {
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

    fn settle(&mut self, outcome: TurnOutcome) {
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

fn normalize_history(frame: &Value, sequence: usize) -> Result<Vec<Message>> {
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
        "user" => normalize_tool_results(frame, sequence),
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
        id: message["id"]
            .as_str()
            .or_else(|| frame["uuid"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("claude-assistant-{sequence}")),
        role: "assistant".into(),
        content,
        raw: Some(frame.clone()),
        tool_call_id: None,
        error: false,
        exit_code: None,
    })
}

fn normalize_tool_results(frame: &Value, sequence: usize) -> Result<Vec<Message>> {
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
                .map(|part| {
                    ContentBlock::Text(
                        part["text"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| part.to_string()),
                    )
                })
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
        assert_eq!(message.id, "message-1");
        assert!(matches!(&message.content[0], ContentBlock::Text(text) if text == "Checking"));
        assert!(
            matches!(&message.content[1], ContentBlock::ToolCall { id, name, .. } if id == "tool-1" && name == "Bash")
        );

        let result = json!({
            "type":"user",
            "uuid":"event-2",
            "message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","is_error":true,"content":"failed"}]}
        });
        let messages = normalize_tool_results(&result, 1)?;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("tool-1"));
        assert!(messages[0].error);
        Ok(())
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
        let messages = normalize_history(&user, 0)?;
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
      printf '%s\n' '{"type":"control_request","request_id":"permission-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo check"}}}'
      ;;
    *'"type":"control_response"'*)
      printf '%s\n' '{"type":"assistant","uuid":"event-1","message":{"id":"message-1","content":[{"type":"text","text":"Done"}]}}'
      printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"Done"}'
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
        let mut permission = false;
        while !permission {
            match receive()? {
                ConversationEvent::SubmissionAccepted { request_id: seen } => {
                    accepted = seen == request_id;
                }
                ConversationEvent::UserRequest(request) => {
                    assert_eq!(request.id, "permission-1");
                    assert_eq!(request.kind, UserRequestKind::Confirm);
                    permission = true;
                }
                _ => {}
            }
        }
        assert!(accepted);
        handle
            .commands
            .send_blocking(ConversationCommand::Respond {
                request_id: "permission-1".into(),
                answer: UserAnswer::Confirmed(true),
            })?;

        let mut assistant = false;
        let mut completed = false;
        while !completed {
            match receive()? {
                ConversationEvent::MessageUpdated(message) => {
                    assistant = matches!(
                        message.content.first(),
                        Some(ContentBlock::Text(text)) if text == "Done"
                    );
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
        assert!(assistant);
        handle.commands.send_blocking(ConversationCommand::Close)?;

        let history = read_transcript(&session_dir.join(TRANSCRIPT))?;
        assert!(history.iter().any(|frame| frame["type"] == "drostje_user"));
        assert!(history.iter().any(|frame| frame["type"] == "assistant"));
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
