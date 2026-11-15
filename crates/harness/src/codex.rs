//! Codex through `codex app-server`: JSON-RPC over stdio (v2 protocol). One owned
//! app-server process serves exactly one Codex thread, so turn and request
//! correlation never has to consider another client.
use super::stdio::{Commands, StdioChild, sleep_for};
use super::*;
use anyhow::{Context, Result, bail, ensure};
use drostje_platform::IdleSleepAssertion;
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
    process::Command,
    time::{Duration, Instant, UNIX_EPOCH},
};

/// Prompts carry image paths, never bytes, so 1 MiB bounds text alone.
const INPUT_LIMIT: usize = 1_048_576;
const TRANSCRIPT: &str = "conversation.jsonl";
/// `initialize` plus `thread/start` includes MCP server startup.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// Live command output kept per tool card; the completed item carries the full output.
const LIVE_OUTPUT: usize = 262_144;
const COMPACT: &str = "compact";
const ALLOW: &str = "Allow";
const ALLOW_SESSION: &str = "Allow for this session";
const ALLOW_TURN: &str = "Allow for this turn";
const DENY: &str = "Deny";
const CONTINUE: &str = "Continue";
const DECLINE: &str = "Decline";

/// `(sandbox, approvalPolicy, approvalsReviewer)` for one of `permission_modes("codex")`.
/// `None` sends no override, so the user's Codex configuration decides.
fn permission_overrides(mode: Option<&str>) -> Result<Map<String, Value>> {
    let (sandbox, approval, reviewer) = match mode {
        None => return Ok(Map::new()),
        Some("read-only") => ("read-only", "on-request", "user"),
        Some("ask") => ("workspace-write", "on-request", "user"),
        Some("auto") => ("workspace-write", "on-request", "auto_review"),
        Some("full-access") => ("danger-full-access", "never", "user"),
        Some(mode) => bail!("Unsupported Codex permission mode: {mode}"),
    };
    let mut overrides = Map::new();
    overrides.insert("sandbox".into(), json!(sandbox));
    overrides.insert("approvalPolicy".into(), json!(approval));
    overrides.insert("approvalsReviewer".into(), json!(reviewer));
    Ok(overrides)
}

/// The saved reference names a Codex thread and the directory it belongs to.
fn saved_thread(config: &ConversationConfig) -> Result<Option<String>> {
    let Some(reference) = &config.session_ref else {
        return Ok(None);
    };
    let saved: Value = serde_json::from_str(reference)
        .context("Invalid saved Codex thread reference; create a new conversation explicitly")?;
    ensure!(
        saved["cwd"].as_str() == config.cwd.to_str(),
        "Saved Codex thread belongs to a different project directory"
    );
    let thread = saved["thread"]
        .as_str()
        .filter(|thread| !thread.is_empty())
        .context("Saved Codex thread ID is missing")?;
    Ok(Some(thread.to_owned()))
}

fn prepare(
    config: &mut ConversationConfig,
) -> Result<(StdioChild, Option<String>, Option<String>)> {
    ensure!(
        config.harness == "codex",
        "Unsupported conversation harness: {}",
        config.harness
    );
    config.cwd = fs::canonicalize(&config.cwd).context("Project root is unavailable")?;
    ensure!(config.cwd.is_dir(), "Project root is not a directory");
    fs::create_dir_all(&config.session_dir)?;
    config.session_dir = fs::canonicalize(&config.session_dir)?;
    permission_overrides(config.permission_mode.as_deref())?;
    let saved = saved_thread(config)?;
    let mut command = Command::new(&config.executable);
    for key in &config.credential_env {
        command.env_remove(key);
    }
    command.arg("app-server").current_dir(&config.cwd);
    let warning = drostje_browser_tools::configure_harness(
        &mut command,
        "codex",
        &config.session_dir,
        config.browser_tools.as_ref(),
    );
    Ok((
        StdioChild::spawn(command, "Codex", INPUT_LIMIT)?,
        saved,
        warning,
    ))
}

pub(super) async fn open(mut config: ConversationConfig) -> Result<ConversationHandle> {
    let (commands, input) = async_channel::bounded(64);
    let (output, events) = async_channel::bounded(128);
    smol::unblock(move || -> Result<()> {
        let (child, saved, warning) = prepare(&mut config)?;
        let mut actor = Actor::new(config, saved, child, input, output)?;
        if let Some(text) = warning {
            actor.notice("warning", text);
        }
        std::thread::Builder::new()
            .name("drostje-codex".into())
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
            .context("Start Codex actor")?;
        Ok(())
    })
    .await?;
    Ok(ConversationHandle { commands, events })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    User(Uuid),
    Peer(Uuid),
}

impl Origin {
    fn id(self) -> Uuid {
        match self {
            Self::User(id) | Self::Peer(id) => id,
        }
    }
}

/// Requests DROSTJE sent and whose answer it still expects.
#[derive(Debug)]
enum Rpc {
    Initialize,
    ThreadOpen { resumed: bool },
    Models,
    Skills,
    RateLimits,
    TurnStart { origin: Origin },
    Compact { request: Uuid },
    Steer { attempt: Uuid },
    Interrupt,
}

struct Turn {
    origin: Origin,
    /// Codex's turn ID, from the `turn/start` answer or the `turn/started` notification.
    native: Option<String>,
    interrupting: bool,
    interrupt_sent: bool,
    /// The local transcript record written once Codex admits the turn.
    record: Option<Value>,
}

/// What an answer to a pending server request must produce.
#[derive(Debug)]
enum Answerable {
    /// Command and file-change approvals; `extra` maps added option labels to decisions.
    Approval {
        extra: Vec<(String, Value)>,
    },
    Permissions {
        requested: Value,
    },
    Questions {
        questions: Vec<Value>,
        index: usize,
        answers: Map<String, Value>,
    },
    Elicitation,
}

#[derive(Debug)]
struct ServerRequest {
    rpc: Value,
    kind: UserRequestKind,
    answerable: Answerable,
}

enum Answer {
    Reply(Value),
    Next(UserRequest),
    Invalid,
}

#[derive(Clone, Debug)]
struct Skill {
    name: String,
    description: String,
    path: String,
}

struct Actor {
    config: ConversationConfig,
    child: StdioChild,
    commands: Commands,
    output: async_channel::Sender<ConversationEvent>,
    events: VecDeque<ConversationEvent>,
    state: ConversationState,
    closing: bool,
    started: Instant,
    saved: Option<String>,
    thread: Option<String>,
    provider: String,
    session_ref: String,
    opened: bool,
    models_done: bool,
    ready: bool,
    next_id: u64,
    rpcs: HashMap<u64, Rpc>,
    raw_models: Vec<Value>,
    models: Vec<ModelChoice>,
    /// The model passed to the next `turn/start` after the user picked one.
    selected_model: Option<String>,
    current_model: Option<String>,
    /// "Yes and switch to auto" routes later approvals to Codex's auto reviewer.
    auto_review: bool,
    skills: Vec<Skill>,
    turn: Option<Turn>,
    peer_attempts: HashSet<Uuid>,
    /// Peer records for `turn/steer` requests awaiting Codex's answer.
    pending_peer_records: HashMap<Uuid, Value>,
    requests: HashMap<String, ServerRequest>,
    messages: Vec<Message>,
    message_index: HashMap<String, usize>,
    dirty_messages: Vec<String>,
    tools: HashMap<String, ToolActivity>,
    dirty_tools: Vec<String>,
    images: ImageCache,
    transcript: File,
    /// Local `drostje_user` and `drostje_peer` records by the `clientUserMessageId` they sent.
    records: HashMap<String, Value>,
    rate_limits: Option<Value>,
    idle_sleep_assertion: Option<IdleSleepAssertion>,
    effort_warned: bool,
}

impl Actor {
    fn new(
        config: ConversationConfig,
        saved: Option<String>,
        child: StdioChild,
        commands: async_channel::Receiver<ConversationCommand>,
        output: async_channel::Sender<ConversationEvent>,
    ) -> Result<Self> {
        let transcript_path = config.session_dir.join(TRANSCRIPT);
        let records = read_records(&transcript_path)?;
        let transcript = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript_path)
            .context("Open Codex transcript")?;
        Ok(Self {
            config,
            child,
            commands: Commands::new(commands)?,
            output,
            events: VecDeque::from([ConversationEvent::StateChanged(ConversationState::Starting)]),
            state: ConversationState::Starting,
            closing: false,
            started: Instant::now(),
            saved,
            thread: None,
            provider: "openai".into(),
            session_ref: String::new(),
            opened: false,
            models_done: false,
            ready: false,
            next_id: 1,
            rpcs: HashMap::new(),
            raw_models: Vec::new(),
            models: Vec::new(),
            selected_model: None,
            current_model: None,
            auto_review: false,
            skills: Vec::new(),
            turn: None,
            peer_attempts: HashSet::new(),
            pending_peer_records: HashMap::new(),
            requests: HashMap::new(),
            messages: Vec::new(),
            message_index: HashMap::new(),
            dirty_messages: Vec::new(),
            tools: HashMap::new(),
            dirty_tools: Vec::new(),
            images: ImageCache::default(),
            transcript,
            records,
            rate_limits: None,
            idle_sleep_assertion: None,
            effort_warned: false,
        })
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
        if self.state != state {
            self.state = state.clone();
            self.emit(ConversationEvent::StateChanged(state));
        }
    }

    /// The state that follows from the open requests and the active turn.
    fn derive_state(&mut self) {
        let state = if !self.requests.is_empty() {
            ConversationState::WaitingForInput
        } else if let Some(turn) = &self.turn {
            if turn.interrupting {
                ConversationState::Interrupting
            } else {
                ConversationState::Running
            }
        } else if self.ready {
            ConversationState::Ready
        } else {
            ConversationState::Starting
        };
        self.state(state);
    }

    fn request(&mut self, method: &str, params: Value, rpc: Rpc) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.child
            .queue(&json!({"id": id, "method": method, "params": params}))?;
        self.rpcs.insert(id, rpc);
        Ok(id)
    }

    fn reply(&mut self, id: &Value, result: Value) -> Result<()> {
        self.child.queue(&json!({"id": id, "result": result}))
    }

    fn reply_error(&mut self, id: &Value, code: i64, message: &str) -> Result<()> {
        self.child
            .queue(&json!({"id": id, "error": {"code": code, "message": message}}))
    }

    fn append_record(&mut self, record: Value) -> Result<()> {
        serde_json::to_writer(&mut self.transcript, &record)?;
        self.transcript.write_all(b"\n")?;
        self.transcript.flush()?;
        if let Some(id) = record["id"].as_str() {
            self.records.insert(id.to_owned(), record);
        }
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "clientInfo": {"name": "drostje", "title": "DROSTJE.", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi": true, "requestAttestation": false},
            }),
            Rpc::Initialize,
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
                "Conversation display stopped consuming Codex events"
            );
            for _ in 0..16 {
                match self.commands.recv() {
                    Ok(command) => self.command(command)?,
                    Err(async_channel::TryRecvError::Empty) => break,
                    Err(async_channel::TryRecvError::Closed) => {
                        self.closing = true;
                        break;
                    }
                }
            }
            if !self.closing {
                self.child.flush()?;
            }
            if self.closing {
                self.unsettle("Codex runtime closed before confirming peer context");
                self.state(ConversationState::Disconnected);
                return Ok(());
            }
            ensure!(
                self.ready || self.started.elapsed() < START_TIMEOUT,
                "Codex did not open its thread within 60 seconds"
            );
            for _ in 0..64 {
                if self.closing || self.events.len() >= 256 {
                    break;
                }
                match self.child.read_frame()? {
                    Some(frame) => self.frame(frame)?,
                    None => break,
                }
            }
            self.flush_dirty();
            if self.closing {
                continue;
            }
            if let Some(status) = self.child.exited()? {
                bail!("Codex app-server exited: {status}");
            }
            if let Some(text) = self.child.read_stderr()? {
                self.notice("diagnostic", text);
            }
            let timeout = sleep_for(&self.events, &self.output, false);
            self.child.wait(&mut self.commands, timeout)?;
        }
    }

    /// Streaming deltas mark their message dirty; one update per loop pass reaches the UI.
    fn flush_dirty(&mut self) {
        for id in std::mem::take(&mut self.dirty_messages) {
            if let Some(message) = self
                .message_index
                .get(&id)
                .map(|ix| self.messages[*ix].clone())
            {
                self.emit(ConversationEvent::MessageUpdated(message));
            }
        }
        for id in std::mem::take(&mut self.dirty_tools) {
            if let Some(tool) = self.tools.get(&id).cloned() {
                self.emit(ConversationEvent::ToolUpdated(tool));
            }
        }
    }

    fn upsert(&mut self, message: Message) {
        let id = message.id.clone();
        match self.message_index.get(&id) {
            Some(ix) => self.messages[*ix] = message,
            None => {
                self.message_index.insert(id.clone(), self.messages.len());
                self.messages.push(message);
            }
        }
        if !self.dirty_messages.contains(&id) {
            self.dirty_messages.push(id);
        }
    }

    fn upsert_tool(&mut self, tool: ToolActivity) {
        let id = tool.id.clone();
        self.tools.insert(id.clone(), tool);
        if !self.dirty_tools.contains(&id) {
            self.dirty_tools.push(id);
        }
    }

    fn command(&mut self, command: ConversationCommand) -> Result<()> {
        match command {
            ConversationCommand::Close => self.closing = true,
            ConversationCommand::LoadHistory { cursor } => {
                if cursor.is_none() && self.ready {
                    self.emit_history();
                }
            }
            ConversationCommand::SelectModel(model) => self.select_model(model),
            ConversationCommand::Interrupt => self.interrupt()?,
            ConversationCommand::Respond { request_id, answer } => {
                self.respond(&request_id, &answer)?
            }
            ConversationCommand::DeliverPeer(delivery) => {
                let attempt = delivery.attempt;
                if let Err(error) = self.deliver_peer(delivery) {
                    self.emit(ConversationEvent::PeerDelivery {
                        attempt,
                        outcome: PeerDeliveryOutcome::Rejected(format!("{error:#}")),
                    });
                }
            }
            ConversationCommand::Submit {
                request_id,
                text,
                images,
            } => {
                if let Err(error) = self.submit(request_id, &text, &images) {
                    self.emit(ConversationEvent::Failed {
                        request_id: Some(request_id),
                        message: format!("{error:#}"),
                    });
                }
            }
        }
        Ok(())
    }

    fn select_model(&mut self, model: ModelKey) {
        let failure = if self.turn.is_some() {
            Some("Model selection requires an idle Codex conversation".to_owned())
        } else if !self.models.is_empty() && !self.models.iter().any(|m| m.key.id == model.id) {
            Some(format!("Codex does not offer the model {}", model.id))
        } else {
            None
        };
        match failure {
            Some(message) => self.emit(ConversationEvent::Failed {
                request_id: None,
                message,
            }),
            None => {
                // `turn/start` applies the model to that turn and every later one.
                self.selected_model = Some(model.id.clone());
                self.emit(ConversationEvent::ModelSelected(model));
            }
        }
    }

    fn interrupt(&mut self) -> Result<()> {
        let Some(turn) = self.turn.as_mut() else {
            return Ok(());
        };
        if turn.interrupting {
            return Ok(());
        }
        turn.interrupting = true;
        self.send_interrupt()?;
        self.derive_state();
        Ok(())
    }

    /// Sends `turn/interrupt` once the turn has a Codex ID; until then the intent is kept.
    fn send_interrupt(&mut self) -> Result<()> {
        let Some(thread) = self.thread.clone() else {
            return Ok(());
        };
        let Some(turn) = self.turn.as_mut() else {
            return Ok(());
        };
        if !turn.interrupting || turn.interrupt_sent {
            return Ok(());
        }
        let Some(native) = turn.native.clone() else {
            return Ok(());
        };
        turn.interrupt_sent = true;
        self.request(
            "turn/interrupt",
            json!({"threadId": thread, "turnId": native}),
            Rpc::Interrupt,
        )?;
        Ok(())
    }

    fn turn_overrides(&mut self, params: &mut Value) {
        if let Some(model) = &self.selected_model {
            params["model"] = json!(model);
        }
        if let Some(effort) = self.config.effort.clone() {
            let model = self
                .selected_model
                .clone()
                .or_else(|| self.current_model.clone())
                .or_else(|| self.config.model.as_ref().map(|model| model.id.clone()));
            let offered = self
                .models
                .iter()
                .find(|choice| Some(&choice.key.id) == model.as_ref())
                .map(|choice| choice.efforts.contains(&effort));
            if offered == Some(false) {
                if !self.effort_warned {
                    self.effort_warned = true;
                    self.notice(
                        "warning",
                        format!(
                            "The selected Codex model does not offer {effort} effort; it uses its default effort"
                        ),
                    );
                }
            } else {
                params["effort"] = json!(effort);
            }
        }
        if self.auto_review {
            params["approvalsReviewer"] = json!("auto_review");
        }
    }

    fn begin_turn(&mut self, origin: Origin, record: Option<Value>) {
        if self.idle_sleep_assertion.is_none() {
            match IdleSleepAssertion::acquire("DROSTJE. conversation turn") {
                Ok(assertion) => self.idle_sleep_assertion = Some(assertion),
                Err(error) => self.notice(
                    "warning",
                    format!(
                        "Could not prevent idle sleep during this conversation turn: {error:#}. Keep your Mac awake manually until the turn finishes."
                    ),
                ),
            }
        }
        self.turn = Some(Turn {
            origin,
            native: None,
            interrupting: false,
            interrupt_sent: false,
            record,
        });
    }

    fn submit(&mut self, request_id: Uuid, text: &str, images: &[ImageAttachment]) -> Result<()> {
        ensure!(self.ready, "Codex is still opening its thread");
        ensure!(
            self.turn.is_none(),
            "A conversation already has an active turn"
        );
        ensure!(
            !text.trim().is_empty() || !images.is_empty(),
            "A prompt needs text or images"
        );
        let thread = self.thread.clone().context("Codex thread is not open")?;
        let record = json!({"type":"drostje_user","id":request_id,"text":text,"images":images});
        if invocation(text).is_some_and(|(name, rest)| name == COMPACT && rest.is_empty()) {
            ensure!(images.is_empty(), "/compact takes no images");
            self.request(
                "thread/compact/start",
                json!({"threadId": thread}),
                Rpc::Compact {
                    request: request_id,
                },
            )?;
        } else {
            let input = prompt_input(&self.skills, text, images)?;
            let mut params = json!({
                "threadId": thread,
                "clientUserMessageId": request_id,
                "input": input,
            });
            self.turn_overrides(&mut params);
            self.request(
                "turn/start",
                params,
                Rpc::TurnStart {
                    origin: Origin::User(request_id),
                },
            )?;
        }
        self.begin_turn(Origin::User(request_id), Some(record));
        self.derive_state();
        Ok(())
    }

    fn deliver_peer(&mut self, delivery: PeerDelivery) -> Result<()> {
        ensure!(
            !self.closing && self.ready,
            "Codex runtime is not ready for peer context"
        );
        ensure!(
            self.requests.is_empty(),
            "Codex is waiting for your answer to a request"
        );
        ensure!(
            self.peer_attempts.insert(delivery.attempt),
            "Peer delivery attempt cannot be replayed"
        );
        ensure!(
            !self.rpcs.values().any(|rpc| matches!(
                rpc,
                Rpc::Steer { .. }
                    | Rpc::TurnStart {
                        origin: Origin::Peer(_)
                    }
            )),
            "Another peer delivery is still being admitted"
        );
        ensure!(
            !delivery.cancelled.is_closed() && delivery.cancelled.is_empty(),
            "Peer delivery cancelled before dispatch"
        );
        ensure!(
            !delivery.messages.is_empty() && delivery.messages.len() <= 128,
            "Invalid peer delivery message count"
        );
        let thread = self.thread.clone().context("Codex thread is not open")?;
        let text = peer_context(&delivery.messages)?;
        let input = json!([{"type":"text","text":text,"text_elements":[]}]);
        let attempt = delivery.attempt;
        let record = json!({"type":"drostje_peer","id":attempt,"messages":delivery.messages});
        match &self.turn {
            Some(turn) if turn.interrupting => bail!("Codex turn is being interrupted"),
            // `turn/steer` adds input to the running turn at its next model call.
            Some(turn) => {
                let native = turn
                    .native
                    .clone()
                    .context("The running Codex turn has not been admitted yet")?;
                self.request(
                    "turn/steer",
                    json!({"threadId":thread,"input":input,"expectedTurnId":native,"clientUserMessageId":attempt}),
                    Rpc::Steer { attempt },
                )?;
                self.pending_peer_records.insert(attempt, record);
            }
            None => {
                let mut params =
                    json!({"threadId":thread,"clientUserMessageId":attempt,"input":input});
                self.turn_overrides(&mut params);
                self.request(
                    "turn/start",
                    params,
                    Rpc::TurnStart {
                        origin: Origin::Peer(attempt),
                    },
                )?;
                self.begin_turn(Origin::Peer(attempt), Some(record));
            }
        }
        Ok(())
    }

    fn peer_accepted(&mut self, record: Value) -> Result<()> {
        let attempt = record["id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            .context("Peer record lost its attempt")?;
        self.append_record(record.clone())?;
        if let Some(message) = record_message(&record)? {
            self.upsert(message);
        }
        self.emit(ConversationEvent::PeerDelivery {
            attempt,
            outcome: PeerDeliveryOutcome::Accepted,
        });
        Ok(())
    }

    fn respond(&mut self, request_id: &str, answer: &UserAnswer) -> Result<()> {
        let Some(mut pending) = self.requests.remove(request_id) else {
            return Ok(());
        };
        if matches!(answer, UserAnswer::PermissionAuto)
            && pending.kind == UserRequestKind::Permission
        {
            self.auto_review = true;
            let others: Vec<String> = self
                .requests
                .iter()
                .filter(|(_, request)| request.kind == UserRequestKind::Permission)
                .map(|(id, _)| id.clone())
                .collect();
            for id in others {
                if let Some(mut other) = self.requests.remove(&id)
                    && let Answer::Reply(result) =
                        answer_request(&mut other, &UserAnswer::PermissionAuto)
                {
                    self.reply(&other.rpc, result)?;
                    self.emit(ConversationEvent::UserRequestDismissed { request_id: id });
                }
            }
        }
        match answer_request(&mut pending, answer) {
            Answer::Reply(result) => {
                self.reply(&pending.rpc, result)?;
                self.emit(ConversationEvent::UserRequestDismissed {
                    request_id: request_id.to_owned(),
                });
            }
            Answer::Next(request) => {
                self.emit(ConversationEvent::UserRequestDismissed {
                    request_id: request_id.to_owned(),
                });
                self.requests.insert(request.id.clone(), pending);
                self.emit(ConversationEvent::UserRequest(request));
            }
            Answer::Invalid => {
                self.requests.insert(request_id.to_owned(), pending);
                self.notice("error", "Codex request does not accept that answer");
            }
        }
        self.derive_state();
        Ok(())
    }

    fn frame(&mut self, frame: Value) -> Result<()> {
        match (frame["method"].as_str(), frame.get("id")) {
            (Some(method), Some(id)) if !id.is_null() => {
                let method = method.to_owned();
                self.server_request(&method, frame)
            }
            (Some(method), _) => {
                let method = method.to_owned();
                self.notification(&method, &frame["params"], &frame)
            }
            (None, Some(id)) => match id.as_u64() {
                Some(id) => self.response(id, &frame),
                None => {
                    self.emit(ConversationEvent::Diagnostic {
                        label: "Uncorrelated Codex response".into(),
                        raw: frame,
                    });
                    Ok(())
                }
            },
            (None, None) => {
                self.emit(ConversationEvent::Diagnostic {
                    label: "Codex frame".into(),
                    raw: frame,
                });
                Ok(())
            }
        }
    }

    fn response(&mut self, id: u64, frame: &Value) -> Result<()> {
        let Some(rpc) = self.rpcs.remove(&id) else {
            self.emit(ConversationEvent::Diagnostic {
                label: "Uncorrelated Codex response".into(),
                raw: frame.clone(),
            });
            return Ok(());
        };
        let result = match frame.get("error").filter(|error| !error.is_null()) {
            Some(error) => Err(error["message"]
                .as_str()
                .unwrap_or("Codex rejected the request")
                .to_owned()),
            None => Ok(frame["result"].clone()),
        };
        match rpc {
            Rpc::Initialize => {
                result.map_err(|error| anyhow::anyhow!("Codex initialize failed: {error}"))?;
                self.child.queue(&json!({"method":"initialized"}))?;
                self.open_thread()?;
                self.request(
                    "skills/list",
                    json!({"cwds":[self.config.cwd]}),
                    Rpc::Skills,
                )?;
                self.request("account/rateLimits/read", Value::Null, Rpc::RateLimits)?;
            }
            Rpc::ThreadOpen { resumed } => match result {
                Ok(answer) => self.opened(&answer)?,
                Err(error)
                    if resumed
                        && self.config.allow_empty_session
                        && error.contains("no rollout found") =>
                {
                    // Codex stores a thread at its first turn; an unsent draft's thread never was.
                    self.notice(
                        "info",
                        "The saved Codex thread was never started; starting a new thread",
                    );
                    self.saved = None;
                    self.open_thread()?;
                }
                Err(error) => bail!("Codex could not open the thread: {error}"),
            },
            Rpc::Models => match result {
                Ok(answer) => {
                    self.raw_models
                        .extend(answer["data"].as_array().cloned().unwrap_or_default());
                    match answer["nextCursor"]
                        .as_str()
                        .filter(|cursor| !cursor.is_empty())
                    {
                        Some(cursor) if self.raw_models.len() < 1024 => {
                            let cursor = cursor.to_owned();
                            self.request("model/list", json!({"cursor": cursor}), Rpc::Models)?;
                        }
                        _ => {
                            self.models_done = true;
                            self.maybe_ready()?;
                        }
                    }
                }
                Err(error) => {
                    self.notice("error", format!("Codex model discovery failed: {error}"));
                    self.models_done = true;
                    self.maybe_ready()?;
                }
            },
            Rpc::Skills => match result {
                Ok(answer) => {
                    self.skills = parse_skills(&answer);
                    let commands = commands(&self.skills);
                    self.emit(ConversationEvent::CommandsUpdated(commands));
                }
                Err(error) => self.emit(ConversationEvent::CommandsUnavailable(error)),
            },
            Rpc::RateLimits => match result {
                Ok(answer) => {
                    self.rate_limits = Some(answer["rateLimits"].clone());
                    self.emit_limits();
                }
                // API-key logins have no plan windows.
                Err(error) => self.emit(ConversationEvent::Diagnostic {
                    label: "Codex rate limits".into(),
                    raw: json!({ "error": error }),
                }),
            },
            Rpc::TurnStart { origin } => self.turn_admitted(origin, result)?,
            Rpc::Compact { request } => self.turn_admitted(Origin::User(request), result)?,
            Rpc::Steer { attempt } => {
                let record = self.pending_peer_records.remove(&attempt);
                match (result, record) {
                    (Ok(_), Some(record)) => self.peer_accepted(record)?,
                    (Ok(_), None) => self.emit(ConversationEvent::PeerDelivery {
                        attempt,
                        outcome: PeerDeliveryOutcome::Accepted,
                    }),
                    (Err(error), _) => self.emit(ConversationEvent::PeerDelivery {
                        attempt,
                        outcome: PeerDeliveryOutcome::Rejected(format!(
                            "Codex did not add peer context to the running turn: {error}"
                        )),
                    }),
                }
            }
            Rpc::Interrupt => {
                if let Err(error) = result {
                    self.notice(
                        "error",
                        format!("Codex did not interrupt the turn: {error}"),
                    );
                }
            }
        }
        Ok(())
    }

    fn open_thread(&mut self) -> Result<()> {
        let mut params = permission_overrides(self.config.permission_mode.as_deref())?;
        params.insert("cwd".into(), json!(self.config.cwd));
        if let Some(model) = &self.config.model {
            params.insert("model".into(), json!(model.id));
        }
        match self.saved.clone() {
            Some(thread) => {
                params.insert("threadId".into(), json!(thread));
                self.request(
                    "thread/resume",
                    Value::Object(params),
                    Rpc::ThreadOpen { resumed: true },
                )?;
            }
            None => {
                self.request(
                    "thread/start",
                    Value::Object(params),
                    Rpc::ThreadOpen { resumed: false },
                )?;
            }
        }
        Ok(())
    }

    fn opened(&mut self, answer: &Value) -> Result<()> {
        let thread = answer["thread"]["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("Codex opened a thread without an ID")?
            .to_owned();
        if let Some(saved) = &self.saved {
            ensure!(*saved == thread, "Codex resumed a different thread");
        }
        if let Some(provider) = answer["modelProvider"].as_str().filter(|p| !p.is_empty()) {
            self.provider = provider.to_owned();
        }
        self.current_model = answer["model"].as_str().map(str::to_owned);
        self.session_ref = serde_json::to_string(&json!({"thread":thread,"cwd":self.config.cwd}))?;
        self.thread = Some(thread);
        self.messages.clear();
        self.message_index.clear();
        for turn in answer["thread"]["turns"].as_array().into_iter().flatten() {
            for item in turn["items"].as_array().into_iter().flatten() {
                for message in self.history_item(item)? {
                    self.upsert(message);
                }
            }
        }
        self.dirty_messages.clear();
        self.opened = true;
        self.request("model/list", json!({}), Rpc::Models)?;
        self.maybe_ready()
    }

    fn maybe_ready(&mut self) -> Result<()> {
        if !self.opened || !self.models_done {
            return Ok(());
        }
        self.models = parse_models(&self.raw_models, &self.provider);
        if self.models.is_empty()
            && let Some(id) = self
                .config
                .model
                .as_ref()
                .map(|model| model.id.clone())
                .or_else(|| self.current_model.clone())
        {
            self.models.push(ModelChoice {
                key: ModelKey {
                    provider: self.provider.clone(),
                    id: id.clone(),
                },
                name: id,
                input: vec!["text".into(), "image".into()],
                efforts: Vec::new(),
            });
        }
        let first = !self.ready;
        self.emit(ConversationEvent::Ready {
            peer_delivery: true,
            session_ref: self.session_ref.clone(),
            models: self.models.clone(),
        });
        if first {
            self.ready = true;
            self.emit(ConversationEvent::PeerDeliveryAvailable {
                available: true,
                reason: None,
            });
            self.emit_history();
            self.derive_state();
        }
        Ok(())
    }

    fn emit_history(&mut self) {
        self.emit(ConversationEvent::HistoryReset);
        self.emit(ConversationEvent::History {
            messages: self.messages.clone(),
            next_cursor: None,
        });
    }

    /// A history item, with DROSTJE's own submissions restored from local records.
    fn history_item(&mut self, item: &Value) -> Result<Vec<Message>> {
        if item["type"] == "userMessage"
            && let Some(record) = item["clientId"]
                .as_str()
                .and_then(|id| self.records.get(id))
        {
            return Ok(record_message(record)?.into_iter().collect());
        }
        let (messages, _) = normalize_item(item, true, &self.config.session_dir, &mut self.images);
        Ok(messages)
    }

    fn turn_admitted(
        &mut self,
        origin: Origin,
        result: std::result::Result<Value, String>,
    ) -> Result<()> {
        let current = self.turn.as_ref().is_some_and(|turn| turn.origin == origin);
        match result {
            Ok(answer) => {
                if !current {
                    self.emit(ConversationEvent::Diagnostic {
                        label: "Codex admitted a turn DROSTJE no longer tracks".into(),
                        raw: answer,
                    });
                    return Ok(());
                }
                let record = self.turn.as_mut().and_then(|turn| {
                    if let Some(native) = answer["turn"]["id"].as_str() {
                        turn.native = Some(native.to_owned());
                    }
                    turn.record.take()
                });
                match origin {
                    Origin::User(request_id) => {
                        if let Some(record) = record {
                            self.append_record(record.clone())?;
                            if let Some(message) = record_message(&record)? {
                                self.upsert(message);
                            }
                        }
                        self.emit(ConversationEvent::SubmissionAccepted { request_id });
                    }
                    Origin::Peer(attempt) => {
                        self.emit(ConversationEvent::PeerTurnStarted {
                            request_id: attempt,
                        });
                        match record {
                            Some(record) => self.peer_accepted(record)?,
                            None => self.emit(ConversationEvent::PeerDelivery {
                                attempt,
                                outcome: PeerDeliveryOutcome::Accepted,
                            }),
                        }
                    }
                }
                self.send_interrupt()?;
                self.derive_state();
            }
            Err(error) => {
                if current {
                    self.turn = None;
                    self.idle_sleep_assertion = None;
                }
                match origin {
                    Origin::User(request_id) => self.emit(ConversationEvent::Failed {
                        request_id: Some(request_id),
                        message: format!("Codex did not start the turn: {error}"),
                    }),
                    Origin::Peer(attempt) => self.emit(ConversationEvent::PeerDelivery {
                        attempt,
                        outcome: PeerDeliveryOutcome::Rejected(format!(
                            "Codex did not start a peer turn: {error}"
                        )),
                    }),
                }
                self.derive_state();
            }
        }
        Ok(())
    }

    fn settle(&mut self, outcome: TurnOutcome) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        self.idle_sleep_assertion = None;
        for (request_id, _) in std::mem::take(&mut self.requests) {
            self.emit(ConversationEvent::UserRequestDismissed { request_id });
        }
        if let (Origin::Peer(attempt), Some(_)) = (turn.origin, turn.record) {
            // Never admitted: Codex may or may not have read the context.
            self.emit(ConversationEvent::PeerDelivery {
                attempt,
                outcome: PeerDeliveryOutcome::Unknown(
                    "Codex stopped before confirming the peer turn".into(),
                ),
            });
        }
        self.emit(ConversationEvent::TurnSettled {
            request_id: turn.origin.id(),
            outcome,
        });
        self.derive_state();
    }

    /// Ends tracking without native confirmation: unconfirmed peer context stays unknown.
    fn unsettle(&mut self, reason: &str) {
        let steering: Vec<Uuid> = self
            .rpcs
            .values()
            .filter_map(|rpc| match rpc {
                Rpc::Steer { attempt } => Some(*attempt),
                _ => None,
            })
            .collect();
        for attempt in steering {
            self.pending_peer_records.remove(&attempt);
            self.emit(ConversationEvent::PeerDelivery {
                attempt,
                outcome: PeerDeliveryOutcome::Unknown(reason.into()),
            });
        }
        self.rpcs.clear();
        self.settle(TurnOutcome::Unknown);
    }

    fn disconnect(&mut self, message: String) {
        self.unsettle("Codex runtime disconnected before confirming peer context");
        self.state(ConversationState::Failed);
        self.emit(ConversationEvent::Failed {
            request_id: None,
            message,
        });
    }

    fn emit_limits(&mut self) {
        if let Some(snapshot) = &self.rate_limits {
            let limits = parse_limits(snapshot);
            self.emit(ConversationEvent::LimitsUpdated(limits));
        }
    }

    fn notification(&mut self, method: &str, params: &Value, frame: &Value) -> Result<()> {
        if let Some(thread) = params["threadId"].as_str()
            && self.thread.as_deref().is_some_and(|own| own != thread)
        {
            // Sub-agent threads report through their own IDs; keep them visible but apart.
            self.emit(ConversationEvent::Diagnostic {
                label: format!("Codex {method} for another thread"),
                raw: frame.clone(),
            });
            return Ok(());
        }
        match method {
            "turn/started" => {
                let native = params["turn"]["id"].as_str().map(str::to_owned);
                match self.turn.as_mut() {
                    Some(turn) if turn.native.is_none() || turn.native == native => {
                        turn.native = native;
                        self.send_interrupt()?;
                    }
                    _ => self.emit(ConversationEvent::Diagnostic {
                        label: "Uncorrelated Codex turn".into(),
                        raw: frame.clone(),
                    }),
                }
            }
            "turn/completed" => {
                let native = params["turn"]["id"].as_str();
                if self
                    .turn
                    .as_ref()
                    .is_some_and(|turn| turn.native.as_deref() == native && native.is_some())
                {
                    let outcome = match params["turn"]["status"].as_str() {
                        Some("completed") => TurnOutcome::Completed,
                        Some("interrupted") => TurnOutcome::Interrupted,
                        Some("failed") => TurnOutcome::Failed,
                        _ => TurnOutcome::Unknown,
                    };
                    if outcome == TurnOutcome::Failed {
                        let message = params["turn"]["error"]["message"]
                            .as_str()
                            .unwrap_or("Codex turn failed");
                        self.notice("error", message);
                    }
                    self.flush_dirty();
                    self.settle(outcome);
                } else {
                    self.emit(ConversationEvent::Diagnostic {
                        label: "Uncorrelated Codex turn completion".into(),
                        raw: frame.clone(),
                    });
                }
            }
            "item/started" | "item/completed" => {
                self.live_item(&params["item"], method == "item/completed")?
            }
            "item/agentMessage/delta" | "item/plan/delta" => {
                self.append_text(params, false);
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                self.append_text(params, true);
            }
            "item/reasoning/summaryPartAdded" => {
                if let Some(id) = params["itemId"].as_str()
                    && let Some(ix) = self.message_index.get(id).copied()
                    && let Some(ContentBlock::Thinking(text)) =
                        self.messages[ix].content.first_mut()
                    && !text.is_empty()
                {
                    text.push_str("\n\n");
                }
            }
            "item/commandExecution/outputDelta" | "item/fileChange/outputDelta" => {
                if let Some(id) = params["itemId"].as_str()
                    && let Some(delta) = params["delta"].as_str()
                    && let Some(tool) = self.tools.get_mut(id)
                {
                    match tool.result.first_mut() {
                        Some(ContentBlock::Text(text)) => text.push_str(delta),
                        _ => tool.result = vec![ContentBlock::Text(delta.into())],
                    }
                    if let Some(ContentBlock::Text(text)) = tool.result.first_mut()
                        && text.len() > LIVE_OUTPUT
                    {
                        let cut = text.len() - LIVE_OUTPUT;
                        let cut = (cut..text.len())
                            .find(|ix| text.is_char_boundary(*ix))
                            .unwrap_or(text.len());
                        text.replace_range(..cut, "…");
                    }
                    let id = id.to_owned();
                    if !self.dirty_tools.contains(&id) {
                        self.dirty_tools.push(id);
                    }
                }
            }
            "thread/tokenUsage/updated" => {
                let usage = &params["tokenUsage"];
                if let (Some(used), Some(max)) = (
                    usage["last"]["totalTokens"].as_u64(),
                    usage["modelContextWindow"].as_u64(),
                ) {
                    self.emit(ConversationEvent::ContextUsageUpdated(ContextUsage {
                        used_tokens: used,
                        max_tokens: max,
                    }));
                }
            }
            "account/rateLimits/updated" => {
                let update = &params["rateLimits"];
                let snapshot = self.rate_limits.get_or_insert_with(|| json!({}));
                if snapshot["limitId"].is_null() || snapshot["limitId"] == update["limitId"] {
                    // Sparse update: present windows replace, absent ones keep their value.
                    for key in ["limitId", "primary", "secondary"] {
                        if !update[key].is_null() {
                            snapshot[key] = update[key].clone();
                        }
                    }
                    self.emit_limits();
                }
            }
            "thread/name/updated" => {
                if let Some(name) = params["threadName"]
                    .as_str()
                    .filter(|name| !name.trim().is_empty())
                {
                    self.emit(ConversationEvent::TitleChanged(name.to_owned()));
                }
            }
            "turn/plan/updated" => {
                let mut lines: Vec<String> = params["explanation"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned)
                    .into_iter()
                    .collect();
                for step in params["plan"].as_array().into_iter().flatten() {
                    let mark = match step["status"].as_str() {
                        Some("completed") => "✓",
                        Some("inProgress") => "▸",
                        _ => "○",
                    };
                    lines.push(format!("{mark} {}", step["step"].as_str().unwrap_or("")));
                }
                self.emit(ConversationEvent::WidgetUpdated {
                    key: "codex.plan".into(),
                    lines,
                });
            }
            "serverRequest/resolved" => {
                let resolved = params["requestId"].clone();
                let ids: Vec<String> = self
                    .requests
                    .iter()
                    .filter(|(_, request)| request.rpc == resolved)
                    .map(|(id, _)| id.clone())
                    .collect();
                for request_id in ids {
                    self.requests.remove(&request_id);
                    self.emit(ConversationEvent::UserRequestDismissed { request_id });
                }
                self.derive_state();
            }
            "error" => {
                let message = params["error"]["message"].as_str().unwrap_or("Codex error");
                if params["willRetry"].as_bool().unwrap_or(false) {
                    self.notice("warning", format!("{message}; Codex is retrying"));
                } else {
                    self.notice("error", message);
                }
            }
            "warning" | "configWarning" | "deprecationNotice" | "guardianWarning" => {
                let text = params["message"]
                    .as_str()
                    .or_else(|| params["summary"].as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| params.to_string());
                self.notice("warning", text);
            }
            "model/rerouted" => {
                let text = format!(
                    "Codex rerouted this turn from {} to {}",
                    params["fromModel"].as_str().unwrap_or("the selected model"),
                    params["toModel"].as_str().unwrap_or("another model"),
                );
                self.notice("info", text);
            }
            "mcpServer/startupStatus/updated" => {
                if params["status"] == "failed" {
                    let text = format!(
                        "Codex MCP server {} failed to start: {}",
                        params["name"].as_str().unwrap_or("unknown"),
                        params["error"]
                            .as_str()
                            .or_else(|| params["failureReason"].as_str())
                            .unwrap_or("no reason given"),
                    );
                    self.notice("warning", text);
                }
            }
            "thread/status/changed" => {
                if params["status"]["type"] == "systemError" {
                    self.notice("error", "Codex reported a system error for this thread");
                }
            }
            "skills/changed" => {
                self.request(
                    "skills/list",
                    json!({"cwds":[self.config.cwd],"forceReload":true}),
                    Rpc::Skills,
                )?;
            }
            // Lifecycle chatter with no DROSTJE surface; diffs come from the working tree.
            "thread/started"
            | "thread/settings/updated"
            | "thread/readState/changed"
            | "turn/diff/updated"
            | "turn/moderationMetadata"
            | "account/updated"
            | "remoteControl/status/changed"
            | "thread/goal/cleared"
            | "thread/goal/updated"
            | "item/fileChange/patchUpdated"
            | "rawResponseItem/completed"
            | "rawResponse/completed"
            | "item/mcpToolCall/progress"
            | "app/list/updated"
            | "thread/compacted" => {}
            _ => self.emit(ConversationEvent::Diagnostic {
                label: format!("Codex {method}"),
                raw: frame.clone(),
            }),
        }
        Ok(())
    }

    fn append_text(&mut self, params: &Value, thinking: bool) {
        let (Some(id), Some(delta)) = (params["itemId"].as_str(), params["delta"].as_str()) else {
            return;
        };
        if !self.message_index.contains_key(id) {
            let block = if thinking {
                ContentBlock::Thinking(String::new())
            } else {
                ContentBlock::Text(String::new())
            };
            self.upsert(Message {
                id: id.to_owned(),
                role: "assistant".into(),
                content: vec![block],
                raw: None,
                tool_call_id: None,
                error: false,
                exit_code: None,
            });
        }
        let ix = self.message_index[id];
        match self.messages[ix].content.first_mut() {
            Some(ContentBlock::Text(text) | ContentBlock::Thinking(text)) => text.push_str(delta),
            _ => self.messages[ix]
                .content
                .insert(0, ContentBlock::Text(delta.into())),
        }
        let id = id.to_owned();
        if !self.dirty_messages.contains(&id) {
            self.dirty_messages.push(id);
        }
    }

    fn live_item(&mut self, item: &Value, completed: bool) -> Result<()> {
        if item["type"] == "userMessage"
            && let Some(client) = item["clientId"].as_str()
        {
            // DROSTJE already shows what it sent, including turns it is still admitting.
            let pending = self
                .turn
                .as_ref()
                .is_some_and(|turn| turn.origin.id().to_string() == client)
                || self
                    .pending_peer_records
                    .keys()
                    .any(|id| id.to_string() == client);
            if pending || self.records.contains_key(client) {
                return Ok(());
            }
        }
        let (messages, tool) =
            normalize_item(item, completed, &self.config.session_dir, &mut self.images);
        // Streamed text already holds the deltas; an empty start must not erase them.
        for message in messages {
            let streamed = !completed
                && self.message_index.contains_key(&message.id)
                && message.content.iter().all(|block| {
                    matches!(block, ContentBlock::Text(text) | ContentBlock::Thinking(text) if text.is_empty())
                });
            if !streamed {
                self.upsert(message);
            }
        }
        if let Some(mut tool) = tool {
            if !completed
                && let Some(live) = self.tools.get(&tool.id)
                && tool.result.is_empty()
            {
                tool.result = live.result.clone();
            }
            self.upsert_tool(tool);
        }
        Ok(())
    }

    fn server_request(&mut self, method: &str, frame: Value) -> Result<()> {
        let rpc = frame["id"].clone();
        let params = &frame["params"];
        let ui = pending_ui(&rpc);
        let built = match method {
            "item/commandExecution/requestApproval" => Some(command_approval(&ui, params)),
            "item/fileChange/requestApproval" => Some(file_approval(&ui, params)),
            "item/permissions/requestApproval" => Some(permissions_approval(&ui, params)),
            "item/tool/requestUserInput" => {
                let questions = params["questions"].as_array().cloned().unwrap_or_default();
                if questions.is_empty() {
                    self.reply(&rpc, json!({"answers":{}}))?;
                    None
                } else {
                    let request = question_request(&ui, &questions, 0);
                    Some((
                        request,
                        Answerable::Questions {
                            questions,
                            index: 0,
                            answers: Map::new(),
                        },
                    ))
                }
            }
            "mcpServer/elicitation/request" if params["mode"] == "url" => {
                let url = params["url"].as_str().unwrap_or_default().to_owned();
                let message = params["message"].as_str().map(str::to_owned);
                self.emit(ConversationEvent::LinkOffered {
                    url: url.clone(),
                    instructions: message.clone(),
                });
                Some((
                    UserRequest {
                        id: ui.clone(),
                        kind: UserRequestKind::Select,
                        title: format!(
                            "{} needs you to open a link",
                            params["serverName"].as_str().unwrap_or("An MCP server")
                        ),
                        options: vec![
                            UserOption {
                                value: CONTINUE.into(),
                                description: Some("I finished in the browser".into()),
                            },
                            UserOption {
                                value: DECLINE.into(),
                                description: None,
                            },
                        ],
                        text: format!("{}\n{url}", message.unwrap_or_default())
                            .trim()
                            .to_owned(),
                        placeholder: None,
                        deadline: None,
                    },
                    Answerable::Elicitation,
                ))
            }
            "mcpServer/elicitation/request" => {
                self.notice(
                    "warning",
                    format!(
                        "Declined a form from MCP server {}: DROSTJE. does not present MCP forms",
                        params["serverName"].as_str().unwrap_or("unknown")
                    ),
                );
                self.reply(
                    &rpc,
                    json!({"action":"decline","content":null,"_meta":null}),
                )?;
                None
            }
            _ => {
                self.emit(ConversationEvent::Diagnostic {
                    label: format!("Unsupported Codex request {method}"),
                    raw: frame.clone(),
                });
                self.reply_error(&rpc, -32601, "DROSTJE. does not support this request")?;
                None
            }
        };
        if let Some((request, answerable)) = built {
            self.requests.insert(
                request.id.clone(),
                ServerRequest {
                    rpc,
                    kind: request.kind.clone(),
                    answerable,
                },
            );
            self.emit(ConversationEvent::UserRequest(request));
            self.derive_state();
        }
        Ok(())
    }
}

/// `/name` for a listed skill becomes Codex's structured skill input; images go by path.
fn prompt_input(skills: &[Skill], text: &str, images: &[ImageAttachment]) -> Result<Vec<Value>> {
    let mut input = Vec::new();
    let mut body = text;
    if let Some((name, rest)) = invocation(text)
        && let Some(skill) = skills.iter().find(|skill| skill.name == name)
    {
        input.push(json!({"type":"skill","name":skill.name,"path":skill.path}));
        body = rest;
    }
    if !body.is_empty() {
        input.push(json!({"type":"text","text":body,"text_elements":[]}));
    }
    for image in images {
        let bytes = crate::attachments::read_original(&image.path)?;
        let (_, mime, _) = crate::attachments::decode(&bytes)?;
        ensure!(
            mime == image.mime,
            "Attachment bytes do not match its MIME type"
        );
        input.push(json!({"type":"localImage","path":image.path}));
    }
    Ok(input)
}

fn read_records(path: &Path) -> Result<HashMap<String, Value>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error).context("Read Codex transcript"),
    };
    let mut records = HashMap::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(&line).context("Corrupt Codex transcript line")?;
        if let Some(id) = record["id"].as_str() {
            records.insert(id.to_owned(), record);
        }
    }
    Ok(records)
}

/// The Message a local `drostje_user` or `drostje_peer` record stands for.
fn record_message(record: &Value) -> Result<Option<Message>> {
    let id = record["id"].as_str().unwrap_or_default().to_owned();
    let (role, content) = match record["type"].as_str() {
        Some("drostje_user") => {
            let images: Vec<ImageAttachment> = serde_json::from_value(record["images"].clone())?;
            let mut content = Vec::new();
            if let Some(text) = record["text"].as_str().filter(|text| !text.is_empty()) {
                content.push(ContentBlock::Text(text.into()));
            }
            content.extend(images.into_iter().map(ContentBlock::Image));
            ("user", content)
        }
        Some("drostje_peer") => (
            "peer",
            serde_json::from_value::<Vec<PeerMessage>>(record["messages"].clone())?
                .into_iter()
                .map(ContentBlock::PeerMessage)
                .collect(),
        ),
        _ => return Ok(None),
    };
    Ok(Some(Message {
        id,
        role: role.into(),
        content,
        raw: Some(record.clone()),
        tool_call_id: None,
        error: false,
        exit_code: None,
    }))
}

fn parse_models(models: &[Value], provider: &str) -> Vec<ModelChoice> {
    models
        .iter()
        .filter(|model| !model["hidden"].as_bool().unwrap_or(false))
        .filter_map(|model| {
            let id = model["model"]
                .as_str()
                .or_else(|| model["id"].as_str())
                .filter(|id| !id.is_empty())?;
            Some(ModelChoice {
                key: ModelKey {
                    provider: provider.into(),
                    id: id.into(),
                },
                name: model["displayName"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .into(),
                input: model["inputModalities"]
                    .as_array()
                    .map(|modes| {
                        modes
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_else(|| vec!["text".into()]),
                efforts: model["supportedReasoningEfforts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|effort| effort["reasoningEffort"].as_str())
                    .map(str::to_owned)
                    .collect(),
            })
        })
        .collect()
}

/// Plan windows by duration: five hours and seven days are the windows DROSTJE shows.
fn parse_limits(snapshot: &Value) -> Vec<UsageLimit> {
    ["primary", "secondary"]
        .into_iter()
        .filter_map(|key| {
            let window = &snapshot[key];
            let window_kind = match window["windowDurationMins"].as_u64()? {
                300 => LimitWindow::FiveHour,
                10_080 => LimitWindow::SevenDay,
                _ => return None,
            };
            Some(UsageLimit {
                window: window_kind,
                percent_used: window["usedPercent"].as_f64()? as f32,
                resets_at: window["resetsAt"]
                    .as_u64()
                    .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds)),
            })
        })
        .collect()
}

fn parse_skills(answer: &Value) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    for entry in answer["data"].as_array().into_iter().flatten() {
        for skill in entry["skills"].as_array().into_iter().flatten() {
            if !skill["enabled"].as_bool().unwrap_or(true) {
                continue;
            }
            let (Some(name), Some(path)) = (skill["name"].as_str(), skill["path"].as_str()) else {
                continue;
            };
            if name == COMPACT || skills.iter().any(|known| known.name == name) {
                continue;
            }
            let description = skill["interface"]["shortDescription"]
                .as_str()
                .or_else(|| skill["shortDescription"].as_str())
                .or_else(|| skill["description"].as_str())
                .unwrap_or_default();
            skills.push(Skill {
                name: name.into(),
                description: description.into(),
                path: path.into(),
            });
        }
    }
    skills
}

/// Codex's slash commands are TUI-only; DROSTJE offers `/compact` and the enabled skills.
fn commands(skills: &[Skill]) -> Vec<SlashCommand> {
    std::iter::once(SlashCommand {
        name: COMPACT.into(),
        aliases: Vec::new(),
        description: "Summarize the conversation to free context".into(),
        argument_hint: None,
        source: CommandSource::Builtin,
        blocked: None,
    })
    .chain(skills.iter().map(|skill| SlashCommand {
        name: skill.name.clone(),
        aliases: Vec::new(),
        description: skill.description.clone(),
        argument_hint: None,
        source: CommandSource::Skill,
        blocked: None,
    }))
    .collect()
}

fn user_content(input: &Value, directory: &Path, images: &mut ImageCache) -> Vec<ContentBlock> {
    let mut content = Vec::new();
    for part in input.as_array().into_iter().flatten() {
        match part["type"].as_str().unwrap_or("") {
            "text" => content.push(ContentBlock::Text(
                part["text"].as_str().unwrap_or_default().into(),
            )),
            "skill" => content.push(ContentBlock::Text(format!(
                "/{}",
                part["name"].as_str().unwrap_or("skill")
            ))),
            "mention" => content.push(ContentBlock::Text(format!(
                "@{}",
                part["name"].as_str().unwrap_or("mention")
            ))),
            "image" => {
                let url = part["url"].as_str().unwrap_or_default();
                let inline = url
                    .strip_prefix("data:")
                    .and_then(|rest| rest.split_once(";base64,"));
                content.push(match inline {
                    Some((mime, data)) => match images.import(mime, data, directory) {
                        Ok(image) => ContentBlock::Image(image),
                        Err(error) => ContentBlock::ImageError(format!("{error:#}")),
                    },
                    None => ContentBlock::ImageError(format!("Image at {url}")),
                });
            }
            "localImage" => content.push(ContentBlock::ImageError(format!(
                "Image at {} was not attached through DROSTJE.",
                part["path"].as_str().unwrap_or("an unknown path")
            ))),
            _ => content.push(ContentBlock::Text(part.to_string())),
        }
    }
    content
}

fn mcp_content(result: &Value, directory: &Path, images: &mut ImageCache) -> Vec<ContentBlock> {
    result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(
            |part| match (part["type"].as_str(), part["text"].as_str()) {
                (_, Some(text)) => ContentBlock::Text(text.into()),
                (Some("image"), None) => match (part["mimeType"].as_str(), part["data"].as_str()) {
                    (Some(mime), Some(data)) => match images.import(mime, data, directory) {
                        Ok(image) => ContentBlock::Image(image),
                        Err(error) => ContentBlock::ImageError(format!("{error:#}")),
                    },
                    _ => ContentBlock::ImageError("MCP image without data".into()),
                },
                _ => ContentBlock::Text(part.to_string()),
            },
        )
        .collect()
}

/// One Codex item as Messages plus, for tool items, the live card. `complete` comes from
/// the lifecycle (`item/completed`, history); a status field can only add completion.
fn normalize_item(
    item: &Value,
    complete: bool,
    directory: &Path,
    images: &mut ImageCache,
) -> (Vec<Message>, Option<ToolActivity>) {
    let id = item["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("codex-{}", Uuid::new_v4()));
    let message = |role: &str, content: Vec<ContentBlock>| Message {
        id: id.clone(),
        role: role.into(),
        content,
        raw: Some(item.clone()),
        tool_call_id: None,
        error: false,
        exit_code: None,
    };
    let text = |key: &str| item[key].as_str().unwrap_or_default().to_owned();
    let lines = |key: &str| {
        item[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let status = item["status"].as_str().unwrap_or("");
    let done = complete || matches!(status, "completed" | "failed" | "declined");
    let failed = matches!(status, "failed" | "declined");
    let tool = |name: String,
                input: Value,
                intent: Option<String>,
                result: Vec<ContentBlock>,
                error: bool,
                exit_code: Option<i64>| {
        let activity = ToolActivity {
            id: id.clone(),
            name: name.clone(),
            intent: intent.clone(),
            input: input.clone(),
            result: if done { result.clone() } else { Vec::new() },
            error,
            exit_code,
            complete: done,
            raw: Some(item.clone()),
        };
        let mut messages = vec![Message {
            id: id.clone(),
            role: "assistant".into(),
            content: vec![ContentBlock::ToolCall {
                id: id.clone(),
                name,
                intent,
                input,
            }],
            raw: Some(item.clone()),
            tool_call_id: None,
            error: false,
            exit_code: None,
        }];
        if done {
            messages.push(Message {
                id: format!("{id}:result"),
                role: "toolResult".into(),
                content: result,
                raw: Some(item.clone()),
                tool_call_id: Some(id.clone()),
                error,
                exit_code,
            });
        }
        (messages, Some(activity))
    };
    match item["type"].as_str().unwrap_or("") {
        "userMessage" => (
            vec![message(
                "user",
                user_content(&item["content"], directory, images),
            )],
            None,
        ),
        "agentMessage" | "plan" => (
            vec![message("assistant", vec![ContentBlock::Text(text("text"))])],
            None,
        ),
        "reasoning" => {
            let summary = lines("summary");
            let thinking = if summary.is_empty() {
                lines("content")
            } else {
                summary
            };
            (
                vec![message("assistant", vec![ContentBlock::Thinking(thinking)])],
                None,
            )
        }
        "contextCompaction" => (
            vec![message(
                "system",
                vec![ContentBlock::Text("Context compacted".into())],
            )],
            None,
        ),
        "enteredReviewMode" => (
            vec![message(
                "system",
                vec![ContentBlock::Text(format!(
                    "Review started: {}",
                    text("review")
                ))],
            )],
            None,
        ),
        "exitedReviewMode" => (
            vec![message(
                "assistant",
                vec![ContentBlock::Text(text("review"))],
            )],
            None,
        ),
        "commandExecution" => {
            let exit_code = item["exitCode"].as_i64();
            tool(
                "bash".into(),
                json!({"command": item["command"], "cwd": item["cwd"]}),
                None,
                vec![ContentBlock::Text(text("aggregatedOutput"))],
                failed || exit_code.is_some_and(|code| code != 0),
                exit_code,
            )
        }
        "fileChange" => {
            let changes = item["changes"].as_array().cloned().unwrap_or_default();
            let paths: Vec<&str> = changes
                .iter()
                .filter_map(|change| change["path"].as_str())
                .collect();
            let diff = changes
                .iter()
                .filter_map(|change| change["diff"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            tool(
                "edit".into(),
                json!({"path": paths.join("\n"), "changes": changes}),
                None,
                vec![ContentBlock::Text(diff)],
                failed,
                None,
            )
        }
        "mcpToolCall" => {
            let mut result = mcp_content(&item["result"], directory, images);
            if let Some(error) = item["error"]["message"].as_str() {
                result.push(ContentBlock::Text(error.into()));
            }
            tool(
                format!(
                    "mcp__{}__{}",
                    item["server"].as_str().unwrap_or("mcp"),
                    item["tool"].as_str().unwrap_or("tool")
                ),
                item["arguments"].clone(),
                None,
                result,
                failed || !item["error"].is_null(),
                None,
            )
        }
        "dynamicToolCall" => {
            let result = item["contentItems"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|part| match part["text"].as_str() {
                    Some(text) => ContentBlock::Text(text.into()),
                    None => ContentBlock::Text(part.to_string()),
                })
                .collect();
            tool(
                text("tool"),
                item["arguments"].clone(),
                None,
                result,
                failed || item["success"] == false,
                None,
            )
        }
        "collabAgentToolCall" => tool(
            "task".into(),
            json!({"description": item["prompt"], "tool": item["tool"], "model": item["model"]}),
            item["tool"].as_str().map(str::to_owned),
            vec![ContentBlock::Text(item["agentsStates"].to_string())],
            failed,
            None,
        ),
        kind @ ("webSearch" | "imageView" | "imageGeneration" | "sleep" | "functionCallOutput") => {
            let mut input = item.clone();
            if let Some(fields) = input.as_object_mut() {
                fields.remove("type");
                fields.remove("id");
            }
            let result = match kind {
                "functionCallOutput" => vec![ContentBlock::Text(item["output"].to_string())],
                _ => Vec::new(),
            };
            let name = match kind {
                "webSearch" => "web_search",
                "imageView" => "view_image",
                "imageGeneration" => "image_generation",
                "sleep" => "sleep",
                _ => item["name"].as_str().unwrap_or("function"),
            };
            tool(name.into(), input, None, result, failed, None)
        }
        "subAgentActivity" => (
            vec![message(
                "system",
                vec![ContentBlock::Text(format!(
                    "Subagent {}: {}",
                    item["kind"].as_str().unwrap_or("activity"),
                    item["agentPath"].as_str().unwrap_or_default()
                ))],
            )],
            None,
        ),
        "hookPrompt" => (
            vec![message(
                "system",
                item["fragments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|fragment| {
                        ContentBlock::Text(
                            fragment["text"]
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| fragment.to_string()),
                        )
                    })
                    .collect(),
            )],
            None,
        ),
        // Unknown items stay visible with their raw payload rather than being dropped.
        other => (
            vec![message(
                "system",
                vec![ContentBlock::Text(format!("Codex {other} item"))],
            )],
            None,
        ),
    }
}

fn option(value: &str, description: Option<&str>) -> UserOption {
    UserOption {
        value: value.into(),
        description: description.map(str::to_owned),
    }
}

fn command_approval(ui: &str, params: &Value) -> (UserRequest, Answerable) {
    let mut extra = Vec::new();
    let mut options = vec![
        option(ALLOW, Some("This time")),
        option(
            ALLOW_SESSION,
            Some("Do not ask again for this command this session"),
        ),
    ];
    if let Some(amendment) = params["proposedExecpolicyAmendment"]
        .as_array()
        .filter(|prefix| !prefix.is_empty())
    {
        let prefix = amendment
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        let label = format!("Always allow commands starting with {prefix}");
        options.push(option(&label, Some("Adds an exec policy rule")));
        extra.push((
            label,
            json!({"acceptWithExecpolicyAmendment":{"execpolicy_amendment":amendment}}),
        ));
    }
    for amendment in params["proposedNetworkPolicyAmendments"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let host = amendment["host"].as_str().unwrap_or("this host");
        let label = match amendment["action"].as_str() {
            Some("deny") => format!("Always deny {host}"),
            _ => format!("Always allow {host}"),
        };
        options.push(option(&label, Some("Adds a network policy rule")));
        extra.push((
            label,
            json!({"applyNetworkPolicyAmendment":{"network_policy_amendment":amendment}}),
        ));
    }
    options.push(option(DENY, None));
    let network = params["networkApprovalContext"]["host"].as_str();
    let title = match network {
        Some(host) => format!("Allow network access to {host}?"),
        None => "Allow Codex to run this command?".into(),
    };
    let text = [
        params["reason"].as_str().map(str::to_owned),
        params["command"]
            .as_str()
            .map(|command| format!("$ {command}")),
        params["cwd"].as_str().map(|cwd| format!("in {cwd}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    (
        UserRequest {
            id: ui.into(),
            kind: UserRequestKind::Permission,
            title,
            options,
            text,
            placeholder: None,
            deadline: None,
        },
        Answerable::Approval { extra },
    )
}

fn file_approval(ui: &str, params: &Value) -> (UserRequest, Answerable) {
    let text = [
        params["reason"].as_str().map(str::to_owned),
        params["grantRoot"]
            .as_str()
            .map(|root| format!("Write access under {root} for this session")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    (
        UserRequest {
            id: ui.into(),
            kind: UserRequestKind::Permission,
            title: "Allow Codex to change files?".into(),
            options: vec![
                option(ALLOW, Some("This time")),
                option(
                    ALLOW_SESSION,
                    Some("Do not ask again for these files this session"),
                ),
                option(DENY, None),
            ],
            text,
            placeholder: None,
            deadline: None,
        },
        Answerable::Approval { extra: Vec::new() },
    )
}

fn permissions_approval(ui: &str, params: &Value) -> (UserRequest, Answerable) {
    let requested = &params["permissions"];
    let mut lines: Vec<String> = params["reason"]
        .as_str()
        .map(str::to_owned)
        .into_iter()
        .collect();
    if requested["network"]["enabled"] == true {
        lines.push("Network access".into());
    }
    for (key, label) in [("read", "Read"), ("write", "Write")] {
        for path in requested["fileSystem"][key]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(path) = path.as_str() {
                lines.push(format!("{label} {path}"));
            }
        }
    }
    let mut granted = Map::new();
    for key in ["network", "fileSystem"] {
        if !requested[key].is_null() {
            granted.insert(key.into(), requested[key].clone());
        }
    }
    (
        UserRequest {
            id: ui.into(),
            kind: UserRequestKind::Permission,
            title: "Grant Codex additional permissions?".into(),
            options: vec![
                option(ALLOW_TURN, None),
                option(ALLOW_SESSION, None),
                option(DENY, None),
            ],
            text: lines.join("\n"),
            placeholder: None,
            deadline: None,
        },
        Answerable::Permissions {
            requested: Value::Object(granted),
        },
    )
}

/// One question of a `requestUserInput` batch; DROSTJE asks them in order.
fn question_request(ui: &str, questions: &[Value], index: usize) -> UserRequest {
    let question = &questions[index];
    let options: Vec<UserOption> = question["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|choice| {
            Some(UserOption {
                value: choice["label"]
                    .as_str()
                    .filter(|label| !label.is_empty())?
                    .into(),
                description: choice["description"].as_str().map(str::to_owned),
            })
        })
        .collect();
    let kind = match (
        options.is_empty(),
        question["isOther"].as_bool().unwrap_or(false),
    ) {
        (true, _) => UserRequestKind::Input,
        (false, true) => UserRequestKind::Question,
        (false, false) => UserRequestKind::Select,
    };
    let mut title = question["header"]
        .as_str()
        .filter(|header| !header.is_empty())
        .unwrap_or("Question")
        .to_owned();
    if questions.len() > 1 {
        title = format!("{title} ({} of {})", index + 1, questions.len());
    }
    UserRequest {
        id: format!("{ui}-{index}"),
        kind,
        title,
        options,
        text: question["question"].as_str().unwrap_or_default().into(),
        placeholder: question["isSecret"]
            .as_bool()
            .unwrap_or(false)
            .then(|| "Secret answer".into()),
        deadline: None,
    }
}

/// The JSON-RPC result for an answer, the next question of a batch, or `Invalid`.
fn answer_request(pending: &mut ServerRequest, answer: &UserAnswer) -> Answer {
    match &mut pending.answerable {
        Answerable::Approval { extra } => {
            let decision = match answer {
                UserAnswer::PermissionAuto | UserAnswer::Confirmed(true) => json!("accept"),
                UserAnswer::Confirmed(false) | UserAnswer::Cancelled { .. } => json!("decline"),
                UserAnswer::Value(value) if value == ALLOW => json!("accept"),
                UserAnswer::Value(value) if value == ALLOW_SESSION => json!("acceptForSession"),
                UserAnswer::Value(value) if value == DENY => json!("decline"),
                UserAnswer::Value(value) => match extra.iter().find(|(label, _)| label == value) {
                    Some((_, decision)) => decision.clone(),
                    None => return Answer::Invalid,
                },
            };
            Answer::Reply(json!({ "decision": decision }))
        }
        Answerable::Permissions { requested } => {
            let grant = |scope: &str, permissions: Value| {
                Answer::Reply(json!({"permissions": permissions, "scope": scope}))
            };
            match answer {
                UserAnswer::PermissionAuto | UserAnswer::Confirmed(true) => {
                    grant("turn", requested.clone())
                }
                UserAnswer::Value(value) if value == ALLOW_TURN => grant("turn", requested.clone()),
                UserAnswer::Value(value) if value == ALLOW_SESSION => {
                    grant("session", requested.clone())
                }
                UserAnswer::Value(value) if value == DENY => grant("turn", json!({})),
                UserAnswer::Confirmed(false) | UserAnswer::Cancelled { .. } => {
                    grant("turn", json!({}))
                }
                UserAnswer::Value(_) => Answer::Invalid,
            }
        }
        Answerable::Elicitation => {
            let action = match answer {
                UserAnswer::Value(value) if value == CONTINUE => "accept",
                UserAnswer::Confirmed(true) => "accept",
                UserAnswer::Value(value) if value == DECLINE => "decline",
                UserAnswer::Confirmed(false) => "decline",
                UserAnswer::Cancelled { .. } => "cancel",
                UserAnswer::Value(_) | UserAnswer::PermissionAuto => return Answer::Invalid,
            };
            Answer::Reply(json!({"action": action, "content": null, "_meta": null}))
        }
        Answerable::Questions {
            questions,
            index,
            answers,
        } => {
            match answer {
                UserAnswer::Value(value) => {
                    let id = questions[*index]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    answers.insert(id, json!({"answers": [value]}));
                }
                // A dismissed batch answers what it has; Codex decides how to continue.
                UserAnswer::Cancelled { .. } => {
                    return Answer::Reply(json!({"answers": answers.clone()}));
                }
                UserAnswer::Confirmed(_) | UserAnswer::PermissionAuto => return Answer::Invalid,
            }
            *index += 1;
            if *index < questions.len() {
                let ui = pending_ui(&pending.rpc);
                return Answer::Next(question_request(&ui, questions, *index));
            }
            Answer::Reply(json!({"answers": answers.clone()}))
        }
    }
}

fn pending_ui(rpc: &Value) -> String {
    format!(
        "codex-{}",
        match rpc {
            Value::String(id) => id.clone(),
            other => other.to_string(),
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_parts(item: Value, complete: bool) -> (Vec<Message>, Option<ToolActivity>) {
        let directory = std::env::temp_dir().join(format!("drostje-codex-{}", Uuid::new_v4()));
        normalize_item(&item, complete, &directory, &mut ImageCache::default())
    }

    #[test]
    fn permission_modes_map_to_sandbox_approval_and_reviewer() -> Result<()> {
        assert!(permission_overrides(None)?.is_empty());
        for (value, _) in permission_modes("codex") {
            let overrides = permission_overrides(Some(value))?;
            assert_eq!(overrides.len(), 3, "{value}");
        }
        let auto = permission_overrides(Some("auto"))?;
        assert_eq!(auto["approvalsReviewer"], "auto_review");
        assert_eq!(auto["sandbox"], "workspace-write");
        let full = permission_overrides(Some("full-access"))?;
        assert_eq!(full["sandbox"], "danger-full-access");
        assert_eq!(full["approvalPolicy"], "never");
        assert_eq!(
            permission_overrides(Some("read-only"))?["sandbox"],
            "read-only"
        );
        assert!(permission_overrides(Some("bypassPermissions")).is_err());
        Ok(())
    }

    #[test]
    fn command_items_become_bash_cards_with_results_only_when_complete() {
        let running = json!({"type":"commandExecution","id":"cmd-1","command":"cargo test",
            "cwd":"/repo","status":"inProgress","aggregatedOutput":null,"exitCode":null});
        let (messages, tool) = item_parts(running, false);
        let tool = tool.expect("tool card");
        assert_eq!(tool.name, "bash");
        assert_eq!(tool.input["command"], "cargo test");
        assert!(!tool.complete && tool.result.is_empty());
        assert_eq!(messages.len(), 1);
        assert!(
            matches!(&messages[0].content[0], ContentBlock::ToolCall { id, .. } if id == "cmd-1")
        );

        let failed = json!({"type":"commandExecution","id":"cmd-1","command":"cargo test",
            "cwd":"/repo","status":"completed","aggregatedOutput":"1 failed","exitCode":101});
        let (messages, tool) = item_parts(failed, true);
        let tool = tool.expect("tool card");
        assert!(tool.complete && tool.error);
        assert_eq!(tool.exit_code, Some(101));
        assert_eq!(messages[1].role, "toolResult");
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("cmd-1"));
        assert!(matches!(&messages[1].content[0], ContentBlock::Text(text) if text == "1 failed"));
    }

    #[test]
    fn file_changes_mcp_calls_and_unknown_items_stay_visible() -> Result<()> {
        let change = json!({"type":"fileChange","id":"patch-1","status":"completed","changes":[
            {"path":"src/a.rs","kind":{"type":"update","move_path":null},"diff":"-a\n+b"}]});
        let (_, tool) = item_parts(change, true);
        let tool = tool.expect("edit card");
        assert_eq!(
            (tool.name.as_str(), tool.input["path"].as_str()),
            ("edit", Some("src/a.rs"))
        );

        let png = {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(1, 1).write_to(&mut bytes, image::ImageFormat::Png)?;
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                bytes.into_inner(),
            )
        };
        let mcp = json!({"type":"mcpToolCall","id":"mcp-1","server":"drostje","tool":"show_artifact",
            "status":"completed","arguments":{"path":"/tmp/a.html"},"error":null,
            "result":{"content":[{"type":"text","text":"shown"},{"type":"image","mimeType":"image/png","data":png}]}});
        let (_, tool) = item_parts(mcp, true);
        let tool = tool.expect("mcp card");
        assert_eq!(tool.name, "mcp__drostje__show_artifact");
        assert!(matches!(
            tool.result.as_slice(),
            [ContentBlock::Text(_), ContentBlock::Image(_)]
        ));

        let (messages, tool) = item_parts(json!({"type":"teleport","id":"x-1","to":"mars"}), true);
        assert!(tool.is_none());
        assert_eq!(messages[0].role, "system");
        assert_eq!(
            messages[0].raw.as_ref().map(|raw| raw["to"].clone()),
            Some(json!("mars"))
        );
        Ok(())
    }

    #[test]
    fn models_limits_and_skills_map_from_catalog_answers() {
        let models = parse_models(
            &[
                json!({"id":"a","model":"gpt-a","displayName":"GPT A","hidden":false,"inputModalities":["text","image"],
                    "supportedReasoningEfforts":[{"reasoningEffort":"low","description":""},{"reasoningEffort":"high","description":""}]}),
                json!({"id":"b","model":"gpt-hidden","displayName":"Hidden","hidden":true}),
            ],
            "openai",
        );
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].key,
            ModelKey {
                provider: "openai".into(),
                id: "gpt-a".into()
            }
        );
        assert_eq!(models[0].efforts, ["low", "high"]);
        assert_eq!(models[0].input, ["text", "image"]);

        let limits = parse_limits(&json!({
            "primary":{"usedPercent":23,"windowDurationMins":300,"resetsAt":1_800_000_000u64},
            "secondary":{"usedPercent":61,"windowDurationMins":10080,"resetsAt":null},
        }));
        assert_eq!(
            limits
                .iter()
                .map(|limit| (limit.window, limit.percent_used))
                .collect::<Vec<_>>(),
            [(LimitWindow::FiveHour, 23.), (LimitWindow::SevenDay, 61.)]
        );
        assert!(limits[0].resets_at.is_some() && limits[1].resets_at.is_none());
        assert!(
            parse_limits(&json!({"primary":{"usedPercent":5,"windowDurationMins":60}})).is_empty()
        );

        let skills = parse_skills(&json!({"data":[{"skills":[
            {"name":"tdd","description":"Long","shortDescription":"Test first","path":"/s/tdd/SKILL.md","enabled":true},
            {"name":"off","description":"","path":"/s/off/SKILL.md","enabled":false},
            {"name":"compact","description":"","path":"/s/compact/SKILL.md","enabled":true}]}]}));
        assert_eq!(skills.len(), 1);
        let commands = commands(&skills);
        assert_eq!(commands[0].name, "compact");
        assert_eq!(commands[0].source, CommandSource::Builtin);
        assert_eq!(
            (commands[1].name.as_str(), commands[1].description.as_str()),
            ("tdd", "Test first")
        );
        assert_eq!(commands[1].source, CommandSource::Skill);
    }

    #[test]
    fn skill_invocations_become_structured_input() -> Result<()> {
        let skills = [Skill {
            name: "tdd".into(),
            description: String::new(),
            path: "/s/tdd/SKILL.md".into(),
        }];
        assert_eq!(
            prompt_input(&skills, "/tdd fix the parser", &[])?,
            [
                json!({"type":"skill","name":"tdd","path":"/s/tdd/SKILL.md"}),
                json!({"type":"text","text":"fix the parser","text_elements":[]})
            ]
        );
        assert_eq!(
            prompt_input(&skills, "/other thing", &[])?,
            [json!({"type":"text","text":"/other thing","text_elements":[]})]
        );
        Ok(())
    }

    #[test]
    fn approvals_offer_session_and_policy_choices_and_decline_on_dismiss() {
        let (request, answerable) = command_approval(
            "codex-7",
            &json!({"command":"cargo check","cwd":"/repo","reason":"Needs the build",
                "proposedExecpolicyAmendment":["cargo","check"]}),
        );
        assert_eq!(request.kind, UserRequestKind::Permission);
        assert!(request.text.contains("$ cargo check"));
        let mut pending = ServerRequest {
            rpc: json!(7),
            kind: request.kind.clone(),
            answerable,
        };
        let reply = |pending: &mut ServerRequest, answer: UserAnswer| match answer_request(
            pending, &answer,
        ) {
            Answer::Reply(value) => Some(value),
            _ => None,
        };
        assert_eq!(
            reply(&mut pending, UserAnswer::Value(ALLOW.into())),
            Some(json!({"decision":"accept"}))
        );
        assert_eq!(
            reply(&mut pending, UserAnswer::Value(ALLOW_SESSION.into())),
            Some(json!({"decision":"acceptForSession"}))
        );
        assert_eq!(
            reply(
                &mut pending,
                UserAnswer::Value("Always allow commands starting with cargo check".into())
            ),
            Some(
                json!({"decision":{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["cargo","check"]}}})
            )
        );
        assert_eq!(
            reply(&mut pending, UserAnswer::Cancelled { timed_out: false }),
            Some(json!({"decision":"decline"}))
        );
        assert_eq!(
            reply(&mut pending, UserAnswer::PermissionAuto),
            Some(json!({"decision":"accept"}))
        );
        assert!(reply(&mut pending, UserAnswer::Value("Something else".into())).is_none());

        let (_, answerable) = permissions_approval(
            "codex-8",
            &json!({"reason":"Fetch crates","permissions":{"network":{"enabled":true},"fileSystem":null}}),
        );
        let mut pending = ServerRequest {
            rpc: json!(8),
            kind: UserRequestKind::Permission,
            answerable,
        };
        assert_eq!(
            reply(&mut pending, UserAnswer::Value(ALLOW_SESSION.into())),
            Some(json!({"permissions":{"network":{"enabled":true}},"scope":"session"}))
        );
        assert_eq!(
            reply(&mut pending, UserAnswer::Value(DENY.into())),
            Some(json!({"permissions":{},"scope":"turn"}))
        );
    }

    #[test]
    fn question_batches_are_asked_in_order_and_answered_together() {
        let questions = vec![
            json!({"id":"q1","header":"Scope","question":"Which crate?","isOther":true,"isSecret":false,
                "options":[{"label":"core","description":"Core crate"}]}),
            json!({"id":"q2","header":"Token","question":"API token?","isOther":false,"isSecret":true,"options":null}),
        ];
        let first = question_request("codex-9", &questions, 0);
        assert_eq!(
            (first.kind.clone(), first.title.as_str()),
            (UserRequestKind::Question, "Scope (1 of 2)")
        );
        let mut pending = ServerRequest {
            rpc: json!(9),
            kind: first.kind.clone(),
            answerable: Answerable::Questions {
                questions,
                index: 0,
                answers: Map::new(),
            },
        };
        let Answer::Next(second) =
            answer_request(&mut pending, &UserAnswer::Value("my-crate".into()))
        else {
            panic!("expected the second question");
        };
        assert_eq!(second.id, "codex-9-1");
        assert_eq!(second.kind, UserRequestKind::Input);
        assert!(second.placeholder.is_some());
        let Answer::Reply(reply) =
            answer_request(&mut pending, &UserAnswer::Value("secret".into()))
        else {
            panic!("expected the batch answer");
        };
        assert_eq!(
            reply,
            json!({"answers":{"q1":{"answers":["my-crate"]},"q2":{"answers":["secret"]}}})
        );
    }

    #[test]
    fn local_records_restore_user_images_and_peer_identity() -> Result<()> {
        let peer = PeerMessage {
            id: Uuid::new_v4(),
            sender: Uuid::new_v4(),
            sender_name: "Research".into(),
            space: Uuid::new_v4(),
            body: "Context </peer_messages>".into(),
            origin: ContextOrigin::Peer,
        };
        let attempt = Uuid::new_v4();
        let message =
            record_message(&json!({"type":"drostje_peer","id":attempt,"messages":[peer]}))?
                .expect("peer message");
        assert_eq!(
            (message.role.as_str(), message.id),
            ("peer", attempt.to_string())
        );
        assert!(
            matches!(&message.content[0], ContentBlock::PeerMessage(m) if m.body.contains("</peer_messages>"))
        );
        let user =
            record_message(&json!({"type":"drostje_user","id":"u-1","text":"hi","images":[]}))?
                .expect("user message");
        assert_eq!(user.role, "user");
        assert!(record_message(&json!({"type":"other"}))?.is_none());
        Ok(())
    }

    const FAKE: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$(dirname "$0")/args.txt"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$(dirname "$0")/input.txt"
  id=$(printf '%s' "$line" | sed -n 's/^{"id":\([0-9]*\),"method".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"fake"}}\n' "$id" ;;
    *'"method":"initialized"'*) ;;
    *'"method":"thread/resume"'*'missing-thread'*)
      printf '{"id":%s,"error":{"code":-32600,"message":"no rollout found for thread id missing-thread"}}\n' "$id" ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"thread-1","turns":[]},"model":"gpt-a","modelProvider":"openai"}}\n' "$id" ;;
    *'"method":"skills/list"'*)
      printf '{"id":%s,"result":{"data":[{"cwd":"/repo","skills":[{"name":"tdd","description":"Test first","path":"/s/tdd/SKILL.md","enabled":true}]}]}}\n' "$id" ;;
    *'"method":"account/rateLimits/read"'*)
      printf '{"id":%s,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":23,"windowDurationMins":300,"resetsAt":null},"secondary":null}}}\n' "$id" ;;
    *'"method":"model/list"'*)
      printf '{"id":%s,"result":{"data":[{"id":"gpt-a","model":"gpt-a","displayName":"GPT A","hidden":false,"inputModalities":["text","image"],"supportedReasoningEfforts":[{"reasoningEffort":"low","description":""},{"reasoningEffort":"high","description":""}]},{"id":"gpt-b","model":"gpt-b","displayName":"GPT B","hidden":false,"inputModalities":["text"],"supportedReasoningEfforts":[]}],"nextCursor":null}}\n' "$id" ;;
    *'"method":"turn/start"'*'peer_messages'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn-p","status":"inProgress","items":[]}}}\n' "$id"
      printf '%s\n' '{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-p","status":"inProgress","items":[]}}}'
      printf '%s\n' '{"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-p","item":{"type":"agentMessage","id":"msg-p","text":"Noted"}}}'
      printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-p","status":"completed","items":[]}}}' ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"turn-1","status":"inProgress","items":[]}}}\n' "$id"
      printf '%s\n' '{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"inProgress","items":[]}}}'
      printf '%s\n' '{"method":"item/started","params":{"threadId":"thread-1","turnId":"turn-1","item":{"type":"agentMessage","id":"msg-1","text":""}}}'
      printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"msg-1","delta":"Hel"}}'
      printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"msg-1","delta":"lo"}}'
      printf '%s\n' '{"id":"approval-1","method":"item/commandExecution/requestApproval","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"cmd-1","command":"cargo check","cwd":"/repo","proposedExecpolicyAmendment":["cargo","check"]}}' ;;
    *'"id":"approval-1","result":{"decision":{"acceptWithExecpolicyAmendment"'*)
      printf '%s\n' '{"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","item":{"type":"commandExecution","id":"cmd-1","command":"cargo check","cwd":"/repo","status":"completed","aggregatedOutput":"ok","exitCode":0}}}'
      printf '%s\n' '{"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","item":{"type":"agentMessage","id":"msg-1","text":"Hello"}}}'
      printf '%s\n' '{"method":"thread/tokenUsage/updated","params":{"threadId":"thread-1","turnId":"turn-1","tokenUsage":{"last":{"totalTokens":1200},"modelContextWindow":200000}}}'
      printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[]}}}' ;;
    *'"id":"approval-1"'*)
      printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"failed","error":{"message":"wrong decision"},"items":[]}}}' ;;
    *'"method":"thread/compact/start"'*)
      printf '{"id":%s,"result":{}}\n' "$id"
      printf '%s\n' '{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-c","status":"inProgress","items":[]}}}' ;;
    *'"method":"turn/interrupt"'*'"turnId":"turn-c"'*)
      printf '{"id":%s,"result":{}}\n' "$id"
      printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-c","status":"interrupted","items":[]}}}' ;;
  esac
done
"#;

    struct Fake {
        root: PathBuf,
        handle: ConversationHandle,
    }

    impl Fake {
        fn open(session_ref: Option<String>, effort: Option<&str>) -> Result<Self> {
            use std::os::unix::fs::PermissionsExt;
            let root = std::env::temp_dir().join(format!("drostje-codex-{}", Uuid::new_v4()));
            let cwd = root.join("repo");
            fs::create_dir_all(&cwd)?;
            let executable = root.join("codex");
            fs::write(&executable, FAKE)?;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
            let session_ref = session_ref.map(|thread| {
                json!({"thread": thread, "cwd": fs::canonicalize(&cwd).unwrap_or(cwd.clone())})
                    .to_string()
            });
            let handle = smol::block_on(open(ConversationConfig {
                credential_env: Vec::new(),
                harness: "codex".into(),
                executable,
                cwd,
                session_dir: root.join("session"),
                session_ref,
                model: None,
                effort: effort.map(str::to_owned),
                permission_mode: Some("ask".into()),
                browser_tools: None,
                allow_empty_session: true,
            }))?;
            Ok(Self { root, handle })
        }

        fn next(&self) -> Result<ConversationEvent> {
            smol::block_on(smol::future::race(
                async { self.handle.events.recv().await.ok() },
                async {
                    smol::Timer::after(Duration::from_secs(5)).await;
                    None
                },
            ))
            .context("Timed out waiting for fake Codex event")
        }

        /// Events until `stop` returns true for one, which is returned last.
        fn until(
            &self,
            mut stop: impl FnMut(&ConversationEvent) -> bool,
        ) -> Result<Vec<ConversationEvent>> {
            let mut seen = Vec::new();
            loop {
                let event = self.next()?;
                let done = stop(&event);
                seen.push(event);
                if done {
                    return Ok(seen);
                }
            }
        }

        fn input(&self) -> Result<String> {
            Ok(fs::read_to_string(self.root.join("input.txt"))?)
        }

        fn send(&self, command: ConversationCommand) -> Result<()> {
            self.handle.commands.send_blocking(command)?;
            Ok(())
        }
    }

    #[test]
    fn fake_app_server_runs_a_turn_with_streaming_approval_and_usage() -> Result<()> {
        let fake = Fake::open(None, Some("high"))?;
        let mut models = Vec::new();
        let mut commands_seen = Vec::new();
        let mut limits = None;
        let mut session_ref = None;
        fake.until(|event| {
            match event {
                ConversationEvent::Ready {
                    models: list,
                    session_ref: reference,
                    peer_delivery,
                } => {
                    assert!(*peer_delivery);
                    models = list.clone();
                    session_ref = Some(reference.clone());
                }
                ConversationEvent::CommandsUpdated(list) => commands_seen = list.clone(),
                ConversationEvent::LimitsUpdated(list) => limits = Some(list.clone()),
                _ => {}
            }
            !models.is_empty() && !commands_seen.is_empty() && limits.is_some()
        })?;
        assert_eq!(
            models.iter().map(|m| m.key.id.as_str()).collect::<Vec<_>>(),
            ["gpt-a", "gpt-b"]
        );
        assert!(session_ref.is_some_and(|reference| reference.contains("thread-1")));
        assert!(commands_seen.iter().any(|command| command.name == "tdd"));
        assert_eq!(limits.unwrap_or_default()[0].window, LimitWindow::FiveHour);
        let args = fs::read_to_string(fake.root.join("args.txt"))?;
        assert_eq!(args.lines().next(), Some("app-server"));

        let request_id = Uuid::new_v4();
        fake.send(ConversationCommand::Submit {
            request_id,
            text: "Check it".into(),
            images: Vec::new(),
        })?;
        let events = fake.until(|event| matches!(event, ConversationEvent::UserRequest(_)))?;
        assert!(events.iter().any(|e| matches!(e, ConversationEvent::SubmissionAccepted { request_id: id } if *id == request_id)));
        let ConversationEvent::UserRequest(request) = events.last().expect("request") else {
            unreachable!()
        };
        assert_eq!(request.kind, UserRequestKind::Permission);
        let start = fake.input()?;
        assert!(start.contains(r#""effort":"high""#), "{start}");
        assert!(start.contains(&format!(r#""clientUserMessageId":"{request_id}""#)));
        assert!(start.contains(r#""approvalPolicy":"on-request""#));

        fake.send(ConversationCommand::Respond {
            request_id: request.id.clone(),
            answer: UserAnswer::Value("Always allow commands starting with cargo check".into()),
        })?;
        let events = fake.until(|event| matches!(event, ConversationEvent::TurnSettled { .. }))?;
        assert!(
            matches!(events.last(), Some(ConversationEvent::TurnSettled { request_id: id, outcome: TurnOutcome::Completed }) if *id == request_id)
        );
        assert!(events.iter().any(|event| matches!(event,
            ConversationEvent::MessageUpdated(m) if m.id == "msg-1" && matches!(&m.content[0], ContentBlock::Text(t) if t == "Hello"))));
        assert!(events.iter().any(|event| matches!(event,
            ConversationEvent::ToolUpdated(tool) if tool.id == "cmd-1" && tool.complete && tool.exit_code == Some(0))));
        assert!(events.iter().any(|event| matches!(
            event,
            ConversationEvent::ContextUsageUpdated(ContextUsage {
                used_tokens: 1200,
                max_tokens: 200_000
            })
        )));

        fake.send(ConversationCommand::SelectModel(ModelKey {
            provider: "openai".into(),
            id: "gpt-b".into(),
        }))?;
        fake.until(
            |event| matches!(event, ConversationEvent::ModelSelected(model) if model.id == "gpt-b"),
        )?;
        fake.send(ConversationCommand::SelectModel(ModelKey {
            provider: "openai".into(),
            id: "gpt-z".into(),
        }))?;
        fake.until(|event| {
            matches!(
                event,
                ConversationEvent::Failed {
                    request_id: None,
                    ..
                }
            )
        })?;
        fake.send(ConversationCommand::Close)?;
        fake.until(|event| {
            matches!(
                event,
                ConversationEvent::StateChanged(ConversationState::Disconnected)
            )
        })?;
        Ok(())
    }

    #[test]
    fn fake_app_server_replaces_an_unstarted_thread_admits_peers_and_interrupts() -> Result<()> {
        let fake = Fake::open(Some("missing-thread".into()), None)?;
        let events = fake.until(|event| {
            matches!(
                event,
                ConversationEvent::StateChanged(ConversationState::Ready)
            )
        })?;
        assert!(events.iter().any(|event| matches!(event, ConversationEvent::Notice { text, .. } if text.contains("never started"))));
        assert!(events.iter().any(|event| matches!(event, ConversationEvent::Ready { session_ref, .. } if session_ref.contains("thread-1"))));

        let (lifetime, cancelled) = async_channel::bounded(1);
        let attempt = Uuid::new_v4();
        let message = PeerMessage {
            id: Uuid::new_v4(),
            sender: Uuid::new_v4(),
            sender_name: "Research".into(),
            space: Uuid::new_v4(),
            body: "The API moved".into(),
            origin: ContextOrigin::Peer,
        };
        fake.send(ConversationCommand::DeliverPeer(PeerDelivery {
            attempt,
            messages: vec![message],
            cancelled,
        }))?;
        let events = fake.until(|event| matches!(event, ConversationEvent::TurnSettled { .. }))?;
        let order: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                ConversationEvent::PeerTurnStarted { request_id } if *request_id == attempt => {
                    Some("started")
                }
                ConversationEvent::PeerDelivery {
                    attempt: id,
                    outcome: PeerDeliveryOutcome::Accepted,
                } if *id == attempt => Some("accepted"),
                ConversationEvent::TurnSettled {
                    request_id,
                    outcome: TurnOutcome::Completed,
                } if *request_id == attempt => Some("settled"),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["started", "accepted", "settled"]);
        assert!(events.iter().any(
            |event| matches!(event, ConversationEvent::MessageUpdated(m) if m.role == "peer")
        ));
        drop(lifetime);

        let request_id = Uuid::new_v4();
        fake.send(ConversationCommand::Submit {
            request_id,
            text: "/compact".into(),
            images: Vec::new(),
        })?;
        fake.until(|event| matches!(event, ConversationEvent::SubmissionAccepted { .. }))?;
        fake.send(ConversationCommand::Interrupt)?;
        let events = fake.until(|event| matches!(event, ConversationEvent::TurnSettled { .. }))?;
        assert!(
            matches!(events.last(), Some(ConversationEvent::TurnSettled { request_id: id, outcome: TurnOutcome::Interrupted }) if *id == request_id)
        );
        assert!(fake.input()?.contains(r#""method":"thread/compact/start""#));
        fake.send(ConversationCommand::Close)?;
        Ok(())
    }
}
