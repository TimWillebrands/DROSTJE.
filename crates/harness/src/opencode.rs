//! OpenCode over the Agent Client Protocol (`opencode acp`): JSON-RPC 2.0, one JSON
//! object per line on stdio. Only this module knows ACP wire fields.
//!
//! Lifecycle: `initialize`, then `session/load` for a saved session (OpenCode replays its
//! history as `session/update` notifications before answering) or `session/new`, then the
//! saved model, effort and mode are applied with `session/set_config_option`. Only then is
//! the conversation `Ready`. A `session/prompt` answers once, when its turn ends; a prompt
//! sent while a turn runs is folded into that turn by OpenCode.
use super::stdio::{Commands, StdioChild, sleep_for};
use super::*;
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use drostje_platform::IdleSleepAssertion;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

/// Largest outgoing frame, newline included. Images travel inline as base64.
const FRAME: usize = 1_048_576;
const TRANSCRIPT: &str = "conversation.jsonl";
/// OpenCode loads providers and plugins before answering `initialize` and `session/*`.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
/// OpenCode lists its commands right after a session opens; silence means none will come.
const COMMANDS_TIMEOUT: Duration = Duration::from_secs(10);
/// Session ids are 30 characters today; the size bound assumes more.
const SESSION_ID_BOUND: usize = 64;

fn prompt_frame(session: &str, prompt: Vec<Value>, id: u64) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"session/prompt",
        "params":{"sessionId":session,"prompt":prompt}})
}

/// Upper bound of the `session/prompt` frame for this text and these image byte lengths.
pub(super) fn submission_size_for_lengths(text: &str, images: &[(&str, u64)]) -> Result<u64> {
    let prompt = std::iter::once(json!({"type":"text","text":text}))
        .chain(
            images
                .iter()
                .map(|(mime, _)| json!({"type":"image","mimeType":mime,"data":""})),
        )
        .collect();
    let frame = prompt_frame(&"x".repeat(SESSION_ID_BOUND), prompt, u64::MAX);
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

pub(super) fn submission_size(text: &str, images: &[ImageAttachment]) -> Result<u64> {
    let lengths = images
        .iter()
        .map(|image| Ok((image.mime.as_str(), fs::metadata(&image.path)?.len())))
        .collect::<Result<Vec<_>>>()?;
    submission_size_for_lengths(text, &lengths)
}

/// The session a saved reference names, after checking it belongs to this project.
fn saved_session(reference: &str, cwd: &Path) -> Result<String> {
    let saved: Value = serde_json::from_str(reference).context(
        "Invalid saved OpenCode session reference; create a new conversation explicitly",
    )?;
    ensure!(
        saved["cwd"].as_str() == cwd.to_str(),
        "Saved OpenCode session belongs to a different project"
    );
    saved["session"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .context("Saved OpenCode session ID is missing")
}

fn session_reference(session: &str, cwd: &Path) -> Result<String> {
    Ok(serde_json::to_string(
        &json!({"session":session,"cwd":cwd}),
    )?)
}

fn prepare(config: &mut ConversationConfig) -> Result<(StdioChild, Option<String>)> {
    ensure!(
        config.harness == "opencode",
        "Unsupported conversation harness: {}",
        config.harness
    );
    config.cwd = fs::canonicalize(&config.cwd).context("Project root is unavailable")?;
    ensure!(config.cwd.is_dir(), "Project root is not a directory");
    fs::create_dir_all(&config.session_dir)?;
    config.session_dir = fs::canonicalize(&config.session_dir)?;
    if let Some(mode) = &config.permission_mode {
        ensure!(
            permission_modes("opencode")
                .iter()
                .any(|(value, _)| value == mode),
            "Unsupported OpenCode mode: {mode}"
        );
    }
    let mut command = Command::new(&config.executable);
    for key in &config.credential_env {
        command.env_remove(key);
    }
    command.arg("acp").current_dir(&config.cwd);
    let warning = drostje_browser_tools::configure_harness(
        &mut command,
        "opencode",
        &config.session_dir,
        config.browser_tools.as_ref(),
    );
    let child = StdioChild::spawn(command, "OpenCode", FRAME)?;
    Ok((child, warning))
}

pub(super) async fn open(mut config: ConversationConfig) -> Result<ConversationHandle> {
    let (commands, input) = async_channel::bounded(64);
    let (output, events) = async_channel::bounded(128);
    smol::unblock(move || -> Result<()> {
        let (child, warning) = prepare(&mut config)?;
        let mut actor = Actor::new(config, child, input, output)?;
        if let Some(text) = warning {
            actor.notice("warning", text);
        }
        std::thread::Builder::new()
            .name("drostje-opencode".into())
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
            .context("Start OpenCode actor")?;
        Ok(())
    })
    .await?;
    Ok(ConversationHandle { commands, events })
}

/// What an outstanding client request is for.
#[derive(Debug)]
enum Pending {
    Initialize,
    NewSession,
    LoadSession,
    /// A saved model, effort or mode applied before the conversation reports `Ready`.
    Setup,
    SelectModel(ModelKey),
    Prompt(Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Initializing,
    Opening,
    Setup,
    Ready,
}

struct UserTurn {
    id: Uuid,
    rpc: u64,
    /// OpenCode answered or streamed for this prompt, so it holds the text.
    accepted: bool,
}

struct PeerAttempt {
    attempt: Uuid,
    rpc: u64,
    messages: Vec<PeerMessage>,
    /// The attempt owns a turn: it was sent while idle, or outlived the user turn it joined.
    turn: bool,
    started: bool,
    accepted: bool,
}

/// One unanswered `session/request_permission`.
struct Permission {
    rpc: Value,
    /// `(optionId, name, kind)` as OpenCode offered them.
    options: Vec<(String, String, String)>,
}

/// A tool call assembled from `tool_call` and its `tool_call_update`s.
#[derive(Clone, Debug, Default)]
struct ToolState {
    name: String,
    intent: Option<String>,
    input: Value,
    status: String,
    result: Vec<ContentBlock>,
    exit_code: Option<i64>,
    raw: Value,
}

impl ToolState {
    fn complete(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "failed")
    }
    fn error(&self) -> bool {
        self.status == "failed" || self.exit_code.is_some_and(|code| code != 0)
    }
}

/// Local records that recognise replayed prompt text DROSTJE. sent.
#[derive(Default)]
struct Records {
    users: HashMap<String, Vec<ImageAttachment>>,
    peers: HashMap<String, (Uuid, Vec<PeerMessage>)>,
}

impl Records {
    fn observe(&mut self, record: &Value) {
        let Some(text) = record["text"].as_str() else {
            return;
        };
        match record["type"].as_str() {
            Some("drostje_user") => {
                let images = serde_json::from_value(record["images"].clone()).unwrap_or_default();
                self.users.entry(text.to_owned()).or_insert(images);
            }
            Some("drostje_peer") => {
                if let (Ok(id), Ok(messages)) = (
                    serde_json::from_value::<Uuid>(record["id"].clone()),
                    serde_json::from_value::<Vec<PeerMessage>>(record["messages"].clone()),
                ) {
                    self.peers.insert(text.to_owned(), (id, messages));
                }
            }
            _ => {}
        }
    }

    /// How a stored message is shown: replayed prompts DROSTJE. sent regain their images,
    /// and peer context is shown as the peer messages it carried.
    fn present(&self, message: &Message) -> Message {
        if message.role != "user" {
            return message.clone();
        }
        let text = message_text(message);
        if let Some((_, messages)) = self.peers.get(&text) {
            return Message {
                role: "peer".into(),
                content: messages
                    .iter()
                    .cloned()
                    .map(ContentBlock::PeerMessage)
                    .collect(),
                ..message.clone()
            };
        }
        if let Some(images) = self.users.get(&text).filter(|images| !images.is_empty())
            && !message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Image(_)))
        {
            let mut message = message.clone();
            message
                .content
                .extend(images.iter().cloned().map(ContentBlock::Image));
            return message;
        }
        message.clone()
    }
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn read_transcript(path: &Path) -> Result<Vec<Value>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    BufReader::new(file)
        .lines()
        .map(|line| serde_json::from_str(&line?).context("Malformed saved OpenCode transcript"))
        .collect()
}

struct Actor {
    config: ConversationConfig,
    child: StdioChild,
    commands: Commands,
    output: async_channel::Sender<ConversationEvent>,
    events: VecDeque<ConversationEvent>,
    state: ConversationState,
    phase: Phase,
    startup_deadline: Instant,
    closing: bool,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    session: Option<String>,
    session_ref: String,
    image_input: bool,
    config_options: Value,
    /// Config ids still to apply during setup, in order.
    setup: VecDeque<&'static str>,
    replaying: bool,
    transcript: File,
    records: Records,
    /// Conversation messages in order; `message_ix` finds one by id.
    messages: Vec<Message>,
    message_ix: HashMap<String, usize>,
    dirty: Vec<String>,
    tools: HashMap<String, ToolState>,
    dirty_tools: Vec<String>,
    images: ImageCache,
    user: Option<UserTurn>,
    peer: Option<PeerAttempt>,
    interrupting: bool,
    permissions: HashMap<String, Permission>,
    idle_sleep_assertion: Option<IdleSleepAssertion>,
    commands_listed: bool,
    commands_deadline: Option<Instant>,
}

impl Actor {
    fn new(
        config: ConversationConfig,
        child: StdioChild,
        commands: async_channel::Receiver<ConversationCommand>,
        output: async_channel::Sender<ConversationEvent>,
    ) -> Result<Self> {
        let path = config.session_dir.join(TRANSCRIPT);
        let mut records = Records::default();
        for record in read_transcript(&path)? {
            records.observe(&record);
        }
        let transcript = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .context("Open OpenCode transcript")?;
        Ok(Self {
            config,
            child,
            commands: Commands::new(commands)?,
            output,
            events: VecDeque::from([ConversationEvent::StateChanged(ConversationState::Starting)]),
            state: ConversationState::Starting,
            phase: Phase::Initializing,
            startup_deadline: Instant::now() + STARTUP_TIMEOUT,
            closing: false,
            next_id: 1,
            pending: HashMap::new(),
            session: None,
            session_ref: String::new(),
            image_input: false,
            config_options: Value::Array(Vec::new()),
            setup: VecDeque::new(),
            replaying: false,
            transcript,
            records,
            messages: Vec::new(),
            message_ix: HashMap::new(),
            dirty: Vec::new(),
            tools: HashMap::new(),
            dirty_tools: Vec::new(),
            images: ImageCache::default(),
            user: None,
            peer: None,
            interrupting: false,
            permissions: HashMap::new(),
            idle_sleep_assertion: None,
            commands_listed: false,
            commands_deadline: None,
        })
    }

    /// Coalesced transcript updates go out before any later event, so a settled turn is
    /// never reported ahead of its final text.
    fn emit(&mut self, event: ConversationEvent) {
        if !matches!(
            event,
            ConversationEvent::MessageUpdated(_) | ConversationEvent::ToolUpdated(_)
        ) && !(self.dirty.is_empty() && self.dirty_tools.is_empty())
        {
            self.flush_dirty();
        }
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

    fn request(&mut self, method: &str, params: Value, purpose: Pending) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.child
            .queue(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        self.pending.insert(id, purpose);
        Ok(id)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.child
            .queue(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    fn respond(&mut self, id: Value, result: Value) -> Result<()> {
        self.child
            .queue(&json!({"jsonrpc":"2.0","id":id,"result":result}))
    }

    fn record(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.transcript, value)?;
        self.transcript.write_all(b"\n")?;
        self.transcript.flush()?;
        self.records.observe(value);
        Ok(())
    }

    fn session_id(&self) -> Result<String> {
        self.session.clone().context("OpenCode session is not open")
    }

    fn run(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion":1,
                "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},
                "clientInfo":{"name":"drostje","title":"DROSTJE.","version":env!("CARGO_PKG_VERSION")},
            }),
            Pending::Initialize,
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
                "Conversation display stopped consuming OpenCode events"
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
            if self.closing {
                self.stop_peer(PeerDeliveryOutcome::Unknown(
                    "OpenCode closed before confirming the peer context".into(),
                ));
                self.settle_all(TurnOutcome::Unknown);
                self.state(ConversationState::Disconnected);
                return Ok(());
            }
            self.child.flush()?;
            ensure!(
                self.phase == Phase::Ready || Instant::now() < self.startup_deadline,
                "OpenCode did not open a session within 60 seconds"
            );
            if !self.commands_listed
                && self
                    .commands_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.commands_deadline = None;
                self.emit(ConversationEvent::CommandsUnavailable(
                    "OpenCode did not list its commands".into(),
                ));
            }
            for _ in 0..64 {
                if self.closing || self.events.len() >= 256 {
                    break;
                }
                let Some(frame) = self.child.read_frame()? else {
                    break;
                };
                self.frame(frame)?;
            }
            self.flush_dirty();
            if self.closing {
                continue;
            }
            if let Some(status) = self.child.exited()? {
                bail!("OpenCode process exited: {status}");
            }
            if let Some(text) = self.child.read_stderr()? {
                self.notice("diagnostic", text);
            }
            let timeout = sleep_for(&self.events, &self.output, false);
            self.child.wait(&mut self.commands, timeout)?;
        }
    }

    fn command(&mut self, command: ConversationCommand) -> Result<()> {
        if self.closing {
            if let ConversationCommand::DeliverPeer(delivery) = command {
                self.emit(ConversationEvent::PeerDelivery {
                    attempt: delivery.attempt,
                    outcome: PeerDeliveryOutcome::Rejected("OpenCode runtime is closing".into()),
                });
            }
            return Ok(());
        }
        match command {
            ConversationCommand::Close => self.closing = true,
            ConversationCommand::DeliverPeer(delivery) => {
                let attempt = delivery.attempt;
                if let Err(error) = self.deliver_peer(delivery) {
                    self.emit(ConversationEvent::PeerDelivery {
                        attempt,
                        outcome: PeerDeliveryOutcome::Rejected(format!("{error:#}")),
                    });
                }
            }
            ConversationCommand::LoadHistory { cursor } => {
                if cursor.is_none() && self.phase == Phase::Ready {
                    self.emit_history();
                }
            }
            ConversationCommand::SelectModel(model) => {
                let result = (|| -> Result<()> {
                    ensure!(
                        self.phase == Phase::Ready,
                        "OpenCode has not opened its session yet"
                    );
                    ensure!(
                        self.user.is_none() && self.peer.is_none(),
                        "Model selection requires an idle OpenCode conversation"
                    );
                    let session = self.session_id()?;
                    self.request(
                        "session/set_config_option",
                        json!({"sessionId":session,"configId":"model","value":model.id}),
                        Pending::SelectModel(model),
                    )?;
                    Ok(())
                })();
                if let Err(error) = result {
                    self.emit(ConversationEvent::Failed {
                        request_id: None,
                        message: format!("{error:#}"),
                    });
                }
            }
            ConversationCommand::Interrupt => {
                let running = self.user.is_some() || self.peer.is_some();
                if running && !self.interrupting {
                    let session = self.session_id()?;
                    self.notify("session/cancel", json!({"sessionId":session}))?;
                    self.interrupting = true;
                    // ACP: the client answers pending permission requests as cancelled.
                    for (request_id, permission) in std::mem::take(&mut self.permissions) {
                        self.respond(permission.rpc, json!({"outcome":{"outcome":"cancelled"}}))?;
                        self.emit(ConversationEvent::UserRequestDismissed { request_id });
                    }
                    self.state(ConversationState::Interrupting);
                }
            }
            ConversationCommand::Respond { request_id, answer } => {
                let Some(permission) = self.permissions.get(&request_id) else {
                    return Ok(());
                };
                let Some(outcome) = permission_outcome(&permission.options, &answer) else {
                    self.notice("error", "OpenCode request does not accept that answer");
                    return Ok(());
                };
                if let Some(permission) = self.permissions.remove(&request_id) {
                    self.respond(permission.rpc, json!({ "outcome": outcome }))?;
                }
                self.emit(ConversationEvent::UserRequestDismissed { request_id });
                if self.permissions.is_empty() && !self.interrupting {
                    self.state(ConversationState::Running);
                }
            }
            ConversationCommand::Submit {
                request_id,
                text,
                images,
            } => {
                if let Err(error) = self.submit(request_id, text, images) {
                    self.emit(ConversationEvent::Failed {
                        request_id: Some(request_id),
                        message: format!("{error:#}"),
                    });
                }
            }
        }
        Ok(())
    }

    fn submit(
        &mut self,
        request_id: Uuid,
        text: String,
        images: Vec<ImageAttachment>,
    ) -> Result<()> {
        ensure!(
            self.phase == Phase::Ready,
            "OpenCode has not opened its session yet"
        );
        ensure!(
            self.user.is_none() && self.peer.is_none(),
            "A conversation already has an active turn"
        );
        ensure!(
            !text.trim().is_empty() || !images.is_empty(),
            "A prompt needs text or images"
        );
        ensure!(
            images.is_empty() || self.image_input,
            "OpenCode does not accept image prompts"
        );
        ensure!(
            submission_size(&text, &images)? <= FRAME as u64,
            "Prompt exceeds 1 MiB; explicitly optimize images or reduce the draft"
        );
        let mut prompt = vec![json!({"type":"text","text":text})];
        for image in &images {
            let bytes = crate::attachments::read_original(&image.path)?;
            let (_, mime, _) = crate::attachments::decode(&bytes)?;
            ensure!(
                mime == image.mime,
                "Attachment bytes do not match its MIME type"
            );
            prompt.push(json!({"type":"image","mimeType":mime,"data":STANDARD.encode(bytes)}));
        }
        let session = self.session_id()?;
        let id = self.next_id;
        self.child.queue(&prompt_frame(&session, prompt, id))?;
        self.next_id += 1;
        self.pending.insert(id, Pending::Prompt(request_id));
        self.record(&json!({"type":"drostje_user","id":request_id,"text":text,"images":images}))?;
        match IdleSleepAssertion::acquire("DROSTJE. conversation turn") {
            Ok(assertion) => self.idle_sleep_assertion = Some(assertion),
            Err(error) => self.notice(
                "warning",
                format!(
                    "Could not prevent idle sleep during this conversation turn: {error:#}. Keep your Mac awake manually until the turn finishes."
                ),
            ),
        }
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(ContentBlock::Text(text));
        }
        content.extend(images.into_iter().map(ContentBlock::Image));
        self.upsert(Message {
            id: request_id.to_string(),
            role: "user".into(),
            content,
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        });
        self.user = Some(UserTurn {
            id: request_id,
            rpc: id,
            accepted: false,
        });
        self.state(ConversationState::Running);
        Ok(())
    }

    fn deliver_peer(&mut self, delivery: PeerDelivery) -> Result<()> {
        ensure!(
            self.phase == Phase::Ready,
            "OpenCode has not opened its session yet"
        );
        ensure!(
            self.permissions.is_empty() && !self.interrupting,
            "OpenCode peer delivery is blocked by a pending request or interrupt"
        );
        ensure!(
            self.peer.is_none(),
            "Another peer delivery is still unconfirmed"
        );
        ensure!(
            !delivery.cancelled.is_closed() && delivery.cancelled.is_empty(),
            "Peer delivery cancelled before dispatch"
        );
        ensure!(
            !delivery.messages.is_empty() && delivery.messages.len() <= 128,
            "Invalid peer delivery message count"
        );
        ensure!(self.child.is_running(), "OpenCode runtime has stopped");
        let text = super::peer_context(&delivery.messages)?;
        let session = self.session_id()?;
        let id = self.next_id;
        self.child.queue(&prompt_frame(
            &session,
            vec![json!({"type":"text","text":text})],
            id,
        ))?;
        self.next_id += 1;
        self.pending.insert(id, Pending::Prompt(delivery.attempt));
        self.record(&json!({"type":"drostje_peer","id":delivery.attempt,
            "messages":delivery.messages,"text":text}))?;
        self.peer = Some(PeerAttempt {
            attempt: delivery.attempt,
            rpc: id,
            messages: delivery.messages,
            turn: self.user.is_none(),
            started: false,
            accepted: false,
        });
        Ok(())
    }

    /// The peer attempt now runs as its own turn in OpenCode.
    fn start_peer(&mut self) {
        let Some(peer) = self.peer.as_mut().filter(|peer| peer.turn && !peer.started) else {
            return;
        };
        peer.started = true;
        let attempt = peer.attempt;
        if self.idle_sleep_assertion.is_none() {
            match IdleSleepAssertion::acquire("DROSTJE. peer conversation turn") {
                Ok(assertion) => self.idle_sleep_assertion = Some(assertion),
                Err(error) => self.notice(
                    "warning",
                    format!("Could not prevent idle sleep during peer turn: {error:#}"),
                ),
            }
        }
        self.emit(ConversationEvent::PeerTurnStarted {
            request_id: attempt,
        });
        self.state(ConversationState::Running);
        self.accept_peer();
    }

    fn accept_peer(&mut self) {
        let Some(peer) = self.peer.as_mut().filter(|peer| !peer.accepted) else {
            return;
        };
        peer.accepted = true;
        let attempt = peer.attempt;
        let messages = peer.messages.clone();
        self.emit(ConversationEvent::PeerDelivery {
            attempt,
            outcome: PeerDeliveryOutcome::Accepted,
        });
        self.upsert(Message {
            id: attempt.to_string(),
            role: "peer".into(),
            content: messages
                .into_iter()
                .map(ContentBlock::PeerMessage)
                .collect(),
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        });
    }

    /// Ends an unconfirmed peer attempt without a turn of its own.
    fn stop_peer(&mut self, outcome: PeerDeliveryOutcome) {
        let Some(peer) = self.peer.take() else {
            return;
        };
        if !peer.accepted {
            self.emit(ConversationEvent::PeerDelivery {
                attempt: peer.attempt,
                outcome,
            });
        }
        if peer.started {
            self.emit(ConversationEvent::TurnSettled {
                request_id: peer.attempt,
                outcome: TurnOutcome::Unknown,
            });
        }
    }

    fn settle_all(&mut self, outcome: TurnOutcome) {
        if let Some(user) = self.user.take() {
            self.emit(ConversationEvent::TurnSettled {
                request_id: user.id,
                outcome,
            });
        }
        self.idle_sleep_assertion = None;
        self.interrupting = false;
        self.permissions.clear();
    }

    /// Any update while a prompt is outstanding proves OpenCode is working on it.
    fn observe_activity(&mut self) {
        if self.replaying {
            return;
        }
        if let Some(user) = self.user.as_mut().filter(|user| !user.accepted) {
            user.accepted = true;
            let request_id = user.id;
            self.emit(ConversationEvent::SubmissionAccepted { request_id });
        }
        if self.user.is_none() {
            self.start_peer();
        }
    }

    fn disconnect(&mut self, message: String) {
        self.stop_peer(PeerDeliveryOutcome::Unknown(
            "OpenCode disconnected before confirming the peer context".into(),
        ));
        self.settle_all(TurnOutcome::Unknown);
        self.state(ConversationState::Failed);
        self.emit(ConversationEvent::Failed {
            request_id: None,
            message,
        });
    }

    fn upsert(&mut self, message: Message) {
        let id = message.id.clone();
        match self.message_ix.get(&id) {
            Some(&ix) => self.messages[ix] = message,
            None => {
                self.message_ix.insert(id.clone(), self.messages.len());
                self.messages.push(message);
            }
        }
        self.mark(id);
    }

    fn mark(&mut self, id: String) {
        if !self.replaying && !self.dirty.contains(&id) {
            self.dirty.push(id);
        }
    }

    fn flush_dirty(&mut self) {
        for id in std::mem::take(&mut self.dirty) {
            if let Some(&ix) = self.message_ix.get(&id) {
                let message = self.records.present(&self.messages[ix]);
                self.emit(ConversationEvent::MessageUpdated(message));
            }
        }
        for id in std::mem::take(&mut self.dirty_tools) {
            if let Some(tool) = self.tools.get(&id) {
                let activity = ToolActivity {
                    id: id.clone(),
                    name: tool.name.clone(),
                    intent: tool.intent.clone(),
                    input: tool.input.clone(),
                    result: tool.result.clone(),
                    error: tool.error(),
                    exit_code: tool.exit_code,
                    complete: tool.complete(),
                    raw: Some(tool.raw.clone()),
                };
                self.emit(ConversationEvent::ToolUpdated(activity));
            }
        }
    }

    fn emit_history(&mut self) {
        self.emit(ConversationEvent::HistoryReset);
        let messages = self
            .messages
            .iter()
            .map(|message| self.records.present(message))
            .collect();
        self.emit(ConversationEvent::History {
            messages,
            next_cursor: None,
        });
    }

    fn models(&self) -> Vec<ModelChoice> {
        parse_models(&self.config_options, self.image_input)
    }

    fn report_ready(&mut self) -> Result<()> {
        self.phase = Phase::Ready;
        let models = self.models();
        self.emit(ConversationEvent::Ready {
            peer_delivery: true,
            session_ref: self.session_ref.clone(),
            models,
        });
        self.emit(ConversationEvent::PeerDeliveryAvailable {
            available: true,
            reason: None,
        });
        self.emit_history();
        self.state(ConversationState::Ready);
        if !self.commands_listed {
            self.commands_deadline = Some(Instant::now() + COMMANDS_TIMEOUT);
        }
        Ok(())
    }

    fn session_opened(&mut self, result: &Value) -> Result<()> {
        if let Some(id) = result["sessionId"].as_str() {
            self.session = Some(id.to_owned());
        }
        let session = self.session_id()?;
        self.session_ref = session_reference(&session, &self.config.cwd)?;
        self.config_options = result["configOptions"].clone();
        self.replaying = false;
        self.phase = Phase::Setup;
        self.setup = VecDeque::from(["model", "effort", "mode"]);
        self.advance_setup()
    }

    /// Applies the next saved setting that differs from OpenCode's, or reports `Ready`.
    fn advance_setup(&mut self) -> Result<()> {
        while let Some(config_id) = self.setup.pop_front() {
            let wanted = match config_id {
                "model" => self.config.model.as_ref().map(|model| model.id.clone()),
                "effort" => self.config.effort.clone(),
                _ => self.config.permission_mode.clone(),
            };
            let Some(wanted) = wanted else {
                continue;
            };
            let option = config_option(&self.config_options, config_id).cloned();
            let Some(option) = option else {
                self.notice(
                    "warning",
                    format!("OpenCode does not offer a {config_id} setting here; using its default instead of {wanted}"),
                );
                continue;
            };
            if option["currentValue"].as_str() == Some(wanted.as_str()) {
                continue;
            }
            if !option_values(&option).any(|value| value == wanted) {
                self.notice(
                    "warning",
                    format!(
                        "OpenCode does not offer {config_id} {wanted}; using {}",
                        option["currentValue"].as_str().unwrap_or("its default")
                    ),
                );
                continue;
            }
            let session = self.session_id()?;
            self.request(
                "session/set_config_option",
                json!({"sessionId":session,"configId":config_id,"value":wanted}),
                Pending::Setup,
            )?;
            return Ok(());
        }
        self.report_ready()
    }

    fn frame(&mut self, frame: Value) -> Result<()> {
        match (frame.get("method").and_then(Value::as_str), frame.get("id")) {
            (Some(method), Some(id)) => {
                let (method, id) = (method.to_owned(), id.clone());
                self.server_request(&method, id, frame)
            }
            (Some(method), None) => {
                let method = method.to_owned();
                self.notification(&method, frame)
            }
            (None, Some(id)) => {
                let Some(id) = id.as_u64() else {
                    self.emit(ConversationEvent::Diagnostic {
                        label: "Uncorrelated OpenCode response".into(),
                        raw: frame,
                    });
                    return Ok(());
                };
                self.response(id, frame)
            }
            (None, None) => {
                self.emit(ConversationEvent::Diagnostic {
                    label: "OpenCode frame".into(),
                    raw: frame,
                });
                Ok(())
            }
        }
    }

    fn response(&mut self, id: u64, frame: Value) -> Result<()> {
        let Some(purpose) = self.pending.remove(&id) else {
            self.emit(ConversationEvent::Diagnostic {
                label: "Uncorrelated OpenCode response".into(),
                raw: frame,
            });
            return Ok(());
        };
        let error = frame.get("error").map(|error| {
            error["message"]
                .as_str()
                .unwrap_or("OpenCode rejected the request")
                .to_owned()
        });
        let result = &frame["result"];
        match purpose {
            Pending::Initialize => {
                if let Some(error) = error {
                    bail!("OpenCode initialize failed: {error}");
                }
                self.image_input =
                    result["agentCapabilities"]["promptCapabilities"]["image"] == true;
                self.phase = Phase::Opening;
                let saved = self
                    .config
                    .session_ref
                    .clone()
                    .map(|reference| saved_session(&reference, &self.config.cwd))
                    .transpose()?;
                let cwd = self.config.cwd.clone();
                match saved {
                    Some(session) => {
                        ensure!(
                            result["agentCapabilities"]["loadSession"] == true,
                            "This OpenCode cannot reopen saved sessions"
                        );
                        self.session = Some(session.clone());
                        self.replaying = true;
                        self.request(
                            "session/load",
                            json!({"sessionId":session,"cwd":cwd,"mcpServers":[]}),
                            Pending::LoadSession,
                        )?;
                    }
                    None => {
                        self.request(
                            "session/new",
                            json!({"cwd":cwd,"mcpServers":[]}),
                            Pending::NewSession,
                        )?;
                    }
                }
            }
            Pending::LoadSession => match error {
                None => self.session_opened(result)?,
                Some(error) if self.config.allow_empty_session => {
                    self.replaying = false;
                    self.messages.clear();
                    self.message_ix.clear();
                    self.tools.clear();
                    self.session = None;
                    self.notice(
                        "warning",
                        format!("OpenCode could not reopen the saved session ({error}); started a new session for this unsent Draft"),
                    );
                    let cwd = self.config.cwd.clone();
                    self.request(
                        "session/new",
                        json!({"cwd":cwd,"mcpServers":[]}),
                        Pending::NewSession,
                    )?;
                }
                Some(error) => bail!(
                    "OpenCode could not reopen the saved session: {error}. Start a new conversation explicitly."
                ),
            },
            Pending::NewSession => {
                if let Some(error) = error {
                    bail!("OpenCode could not start a session: {error}");
                }
                ensure!(
                    result["sessionId"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty()),
                    "OpenCode session/new omitted its session ID"
                );
                self.session_opened(result)?;
            }
            Pending::Setup => {
                match error {
                    Some(error) => self.notice(
                        "warning",
                        format!("OpenCode did not apply a saved setting: {error}"),
                    ),
                    None => self.update_options(result["configOptions"].clone()),
                }
                self.advance_setup()?;
            }
            Pending::SelectModel(model) => match error {
                Some(error) => self.emit(ConversationEvent::Failed {
                    request_id: None,
                    message: format!("OpenCode did not switch to {}: {error}", model.id),
                }),
                None => {
                    self.update_options(result["configOptions"].clone());
                    self.emit(ConversationEvent::ModelSelected(model));
                    let models = self.models();
                    self.emit(ConversationEvent::Ready {
                        peer_delivery: true,
                        session_ref: self.session_ref.clone(),
                        models,
                    });
                }
            },
            Pending::Prompt(owner) => self.prompt_settled(id, owner, error, result)?,
        }
        Ok(())
    }

    fn update_options(&mut self, options: Value) {
        if options.is_array() {
            self.config_options = options;
        }
    }

    fn prompt_settled(
        &mut self,
        rpc: u64,
        owner: Uuid,
        error: Option<String>,
        result: &Value,
    ) -> Result<()> {
        let stop = result["stopReason"].as_str().unwrap_or("");
        if let Some(text) = stop_notice(stop) {
            self.notice("warning", text);
        }
        if let Some(user) = self
            .user
            .take_if(|user| user.rpc == rpc && user.id == owner)
        {
            match &error {
                Some(error) if !user.accepted => {
                    self.emit(ConversationEvent::Failed {
                        request_id: Some(user.id),
                        message: format!("OpenCode rejected the prompt: {error}"),
                    });
                }
                _ => {
                    if !user.accepted {
                        self.emit(ConversationEvent::SubmissionAccepted {
                            request_id: user.id,
                        });
                    }
                    if let Some(error) = &error {
                        self.notice("error", format!("OpenCode turn failed: {error}"));
                    }
                    let outcome = match &error {
                        Some(_) => TurnOutcome::Failed,
                        None => turn_outcome(stop, self.interrupting),
                    };
                    self.emit(ConversationEvent::TurnSettled {
                        request_id: user.id,
                        outcome,
                    });
                }
            }
            self.permissions.clear();
            // A peer prompt that joined this turn and is still unanswered runs on alone.
            if let Some(peer) = self.peer.as_mut().filter(|peer| !peer.turn) {
                peer.turn = true;
                self.start_peer();
            }
        } else if self
            .peer
            .as_ref()
            .is_some_and(|peer| peer.rpc == rpc && peer.attempt == owner)
        {
            let accepted = self.peer.as_ref().is_some_and(|peer| peer.accepted);
            match &error {
                Some(error) if !accepted => {
                    let attempt = owner;
                    self.peer = None;
                    self.emit(ConversationEvent::PeerDelivery {
                        attempt,
                        outcome: PeerDeliveryOutcome::Rejected(format!(
                            "OpenCode rejected the peer context: {error}"
                        )),
                    });
                }
                _ if stop == "cancelled" && !accepted => {
                    // Cancelled before OpenCode showed any work on it.
                    self.stop_peer(PeerDeliveryOutcome::Unknown(
                        "Interrupted before OpenCode confirmed the peer context".into(),
                    ));
                }
                _ => {
                    self.start_peer();
                    self.accept_peer();
                    if let Some(peer) = self.peer.take().filter(|peer| peer.turn) {
                        if let Some(error) = &error {
                            self.notice("error", format!("OpenCode peer turn failed: {error}"));
                        }
                        let outcome = match &error {
                            Some(_) => TurnOutcome::Failed,
                            None => turn_outcome(stop, self.interrupting),
                        };
                        self.emit(ConversationEvent::TurnSettled {
                            request_id: peer.attempt,
                            outcome,
                        });
                    }
                }
            }
        } else {
            self.emit(ConversationEvent::Diagnostic {
                label: "Uncorrelated OpenCode prompt answer".into(),
                raw: result.clone(),
            });
        }
        if self.user.is_none() && self.peer.as_ref().is_none_or(|peer| !peer.started) {
            self.idle_sleep_assertion = None;
            self.interrupting = false;
            self.permissions.clear();
            if self.peer.is_none() {
                self.state(ConversationState::Ready);
            }
        }
        Ok(())
    }

    fn server_request(&mut self, method: &str, id: Value, frame: Value) -> Result<()> {
        if method == "session/request_permission"
            && frame["params"]["sessionId"].as_str() == self.session.as_deref()
        {
            let request_id = format!(
                "opencode-permission-{}",
                id.as_str().map_or_else(|| id.to_string(), str::to_owned)
            );
            let (request, options) = permission_request(request_id.clone(), &frame["params"]);
            if options.is_empty() {
                self.notice(
                    "warning",
                    "Declined an OpenCode permission request without options",
                );
                return self.respond(id, json!({"outcome":{"outcome":"cancelled"}}));
            }
            self.observe_activity();
            self.permissions
                .insert(request_id, Permission { rpc: id, options });
            self.emit(ConversationEvent::UserRequest(request));
            self.state(ConversationState::WaitingForInput);
            return Ok(());
        }
        self.emit(ConversationEvent::Diagnostic {
            label: format!("Unsupported OpenCode request {method}"),
            raw: frame,
        });
        self.child.queue(&json!({"jsonrpc":"2.0","id":id,
            "error":{"code":-32601,"message":format!("DROSTJE. does not support {method}")}}))
    }

    fn notification(&mut self, method: &str, mut frame: Value) -> Result<()> {
        let params = &frame["params"];
        if method != "session/update" || params["sessionId"].as_str() != self.session.as_deref() {
            self.emit(ConversationEvent::Diagnostic {
                label: format!("OpenCode {method}"),
                raw: frame,
            });
            return Ok(());
        }
        let update = &params["update"];
        let kind = update["sessionUpdate"].as_str().unwrap_or("");
        if matches!(
            kind,
            "agent_message_chunk"
                | "agent_thought_chunk"
                | "tool_call"
                | "tool_call_update"
                | "plan"
        ) {
            self.observe_activity();
        }
        match kind {
            "user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk" => {
                let role = if kind == "user_message_chunk" {
                    "user"
                } else {
                    "assistant"
                };
                let id = update["messageId"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| self.trailing_id(role));
                let block = self.chunk_block(&update["content"], kind == "agent_thought_chunk");
                // The frame is not read again; the chunk keeps its update as raw data.
                let raw = frame["params"]["update"].take();
                self.append_chunk(&id, role, block, raw);
            }
            "tool_call" | "tool_call_update" => self.tool_update(update)?,
            "usage_update" => {
                if let (Some(used), Some(size)) = (update["used"].as_u64(), update["size"].as_u64())
                    && size > 0
                {
                    self.emit(ConversationEvent::ContextUsageUpdated(ContextUsage {
                        used_tokens: used,
                        max_tokens: size,
                    }));
                }
            }
            "available_commands_update" => {
                self.commands_listed = true;
                self.commands_deadline = None;
                let commands = parse_commands(&update["availableCommands"]);
                self.emit(ConversationEvent::CommandsUpdated(commands));
            }
            "config_option_update" => {
                self.update_options(update["configOptions"].clone());
                if self.phase == Phase::Ready {
                    let models = self.models();
                    self.emit(ConversationEvent::Ready {
                        peer_delivery: true,
                        session_ref: self.session_ref.clone(),
                        models,
                    });
                }
            }
            "session_info_update" => {
                if let Some(title) = update["title"]
                    .as_str()
                    .map(str::trim)
                    .filter(|title| !title.is_empty() && !title.starts_with("New session - "))
                    && !self.replaying
                {
                    self.emit(ConversationEvent::TitleChanged(title.to_owned()));
                }
            }
            _ => self.emit(ConversationEvent::Diagnostic {
                label: format!("OpenCode {kind} update"),
                raw: frame,
            }),
        }
        Ok(())
    }

    /// The id of the last message when it has `role`, else a fresh one, for chunks
    /// without a `messageId`.
    fn trailing_id(&self, role: &str) -> String {
        match self.messages.last() {
            Some(message) if message.role == role && message.tool_call_id.is_none() => {
                message.id.clone()
            }
            _ => format!("opencode-{role}-{}", self.messages.len()),
        }
    }

    fn chunk_block(&mut self, content: &Value, thinking: bool) -> ContentBlock {
        match content["type"].as_str() {
            Some("text") => {
                let text = content["text"].as_str().unwrap_or_default().to_owned();
                if thinking {
                    ContentBlock::Thinking(text)
                } else {
                    ContentBlock::Text(text)
                }
            }
            Some("image") => {
                let directory = self.config.session_dir.clone();
                match image_block(content, &directory, &mut self.images) {
                    Ok(image) => ContentBlock::Image(image),
                    Err(error) => ContentBlock::ImageError(format!("{error:#}")),
                }
            }
            _ => ContentBlock::Text(content.to_string()),
        }
    }

    /// Appends in place; the message reaches the display once per loop pass via `mark`.
    fn append_chunk(&mut self, id: &str, role: &str, block: ContentBlock, raw: Value) {
        let ix = match self.message_ix.get(id) {
            Some(&ix) => ix,
            None => {
                self.message_ix.insert(id.to_owned(), self.messages.len());
                self.messages.push(Message {
                    id: id.to_owned(),
                    role: role.to_owned(),
                    content: Vec::new(),
                    raw: None,
                    tool_call_id: None,
                    error: false,
                    exit_code: None,
                });
                self.messages.len() - 1
            }
        };
        let message = &mut self.messages[ix];
        append_block(&mut message.content, block);
        message.raw = Some(raw);
        self.mark(id.to_owned());
    }

    fn tool_update(&mut self, update: &Value) -> Result<()> {
        let id = update["toolCallId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("OpenCode tool update omitted its toolCallId")?
            .to_owned();
        let mut tool = self.tools.remove(&id).unwrap_or_default();
        merge_tool(&mut tool, update);
        let directory = self.config.session_dir.clone();
        if let Some(content) = update["content"].as_array() {
            let blocks = tool_content(content, &directory, &mut self.images);
            if !blocks.is_empty() {
                tool.result = blocks;
            }
        }
        if tool.result.is_empty()
            && let Some(output) = update["rawOutput"]["output"].as_str()
        {
            tool.result = vec![ContentBlock::Text(output.to_owned())];
        }
        self.upsert(Message {
            id: format!("tool-call-{id}"),
            role: "assistant".into(),
            content: vec![ContentBlock::ToolCall {
                id: id.clone(),
                name: tool.name.clone(),
                intent: tool.intent.clone(),
                input: tool.input.clone(),
            }],
            raw: Some(tool.raw.clone()),
            tool_call_id: None,
            error: false,
            exit_code: None,
        });
        if tool.complete() {
            self.upsert(Message {
                id: format!("tool-result-{id}"),
                role: "toolResult".into(),
                content: tool.result.clone(),
                raw: Some(tool.raw.clone()),
                tool_call_id: Some(id.clone()),
                error: tool.error(),
                exit_code: tool.exit_code,
            });
        }
        if !self.replaying && !self.dirty_tools.contains(&id) {
            self.dirty_tools.push(id.clone());
        }
        self.tools.insert(id, tool);
        Ok(())
    }
}

/// Appends a streamed chunk, joining it to the previous block of the same kind.
fn append_block(content: &mut Vec<ContentBlock>, block: ContentBlock) {
    match (content.last_mut(), block) {
        (Some(ContentBlock::Text(previous)), ContentBlock::Text(text)) => previous.push_str(&text),
        (Some(ContentBlock::Thinking(previous)), ContentBlock::Thinking(text)) => {
            previous.push_str(&text)
        }
        (_, block) => content.push(block),
    }
}

/// Folds one `tool_call` or `tool_call_update` into the call's state. The first title is
/// OpenCode's tool name; a later, different title describes this call.
fn merge_tool(tool: &mut ToolState, update: &Value) {
    if let Some(title) = update["title"].as_str().filter(|title| !title.is_empty()) {
        if tool.name.is_empty() {
            tool.name = title.to_ascii_lowercase();
        } else if !title.eq_ignore_ascii_case(&tool.name) {
            tool.intent = Some(title.to_owned());
        }
    }
    if tool.name.is_empty() {
        tool.name = update["kind"].as_str().unwrap_or("tool").to_owned();
    }
    if let Some(input) = update.get("rawInput").filter(|input| input.is_object()) {
        tool.input = normalize_input(input);
    }
    if let Some(status) = update["status"].as_str() {
        tool.status = status.to_owned();
    }
    if let Some(exit) = update["rawOutput"]["metadata"]["exit"].as_i64() {
        tool.exit_code = Some(exit);
    }
    tool.raw = update.clone();
}

/// OpenCode names files `filePath`; the conversation view reads `path`.
fn normalize_input(input: &Value) -> Value {
    let mut input = input.clone();
    if input.get("path").is_none()
        && let Some(path) = input.get("filePath").cloned()
    {
        input["path"] = path;
    }
    input
}

fn tool_content(content: &[Value], directory: &Path, images: &mut ImageCache) -> Vec<ContentBlock> {
    content
        .iter()
        .filter_map(|item| match item["type"].as_str() {
            Some("content") => {
                let inner = &item["content"];
                match inner["type"].as_str() {
                    Some("text") => inner["text"]
                        .as_str()
                        .map(|text| ContentBlock::Text(text.into())),
                    Some("image") => Some(match image_block(inner, directory, images) {
                        Ok(image) => ContentBlock::Image(image),
                        Err(error) => ContentBlock::ImageError(format!("{error:#}")),
                    }),
                    _ => Some(ContentBlock::Text(inner.to_string())),
                }
            }
            Some("diff") => Some(ContentBlock::Text(format!(
                "{}\n--- old\n{}\n+++ new\n{}",
                item["path"].as_str().unwrap_or(""),
                item["oldText"].as_str().unwrap_or(""),
                item["newText"].as_str().unwrap_or("")
            ))),
            _ => Some(ContentBlock::Text(item.to_string())),
        })
        .collect()
}

fn image_block(
    content: &Value,
    directory: &Path,
    images: &mut ImageCache,
) -> Result<ImageAttachment> {
    let mime = content["mimeType"]
        .as_str()
        .context("OpenCode image has no MIME type")?;
    let data = content["data"]
        .as_str()
        .context("OpenCode image has no data")?;
    images.import(mime, data, directory)
}

fn config_option<'a>(options: &'a Value, id: &str) -> Option<&'a Value> {
    let category = match id {
        "effort" => "thought_level",
        other => other,
    };
    let options = options.as_array()?;
    options
        .iter()
        .find(|option| option["id"] == id)
        .or_else(|| options.iter().find(|option| option["category"] == category))
}

fn option_values(option: &Value) -> impl Iterator<Item = &str> {
    option["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|choice| choice["value"].as_str())
}

/// The model catalog from the `model` config option. Effort levels are only known for the
/// current model: OpenCode lists them once that model is selected.
fn parse_models(options: &Value, image: bool) -> Vec<ModelChoice> {
    let Some(model) = config_option(options, "model") else {
        return Vec::new();
    };
    let current = model["currentValue"].as_str();
    let efforts: Vec<String> = config_option(options, "effort")
        .map(|effort| option_values(effort).map(str::to_owned).collect())
        .unwrap_or_default();
    let input: Vec<String> = if image {
        vec!["text".into(), "image".into()]
    } else {
        vec!["text".into()]
    };
    model["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|choice| {
            let id = choice["value"].as_str().filter(|id| !id.is_empty())?;
            Some(ModelChoice {
                key: model_key(id),
                name: choice["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .to_owned(),
                input: input.clone(),
                efforts: if Some(id) == current {
                    efforts.clone()
                } else {
                    Vec::new()
                },
            })
        })
        .collect()
}

fn model_key(id: &str) -> ModelKey {
    ModelKey {
        provider: id
            .split_once('/')
            .map_or(id, |(provider, _)| provider)
            .to_owned(),
        id: id.to_owned(),
    }
}

fn parse_commands(raw: &Value) -> Vec<SlashCommand> {
    raw.as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let name = entry["name"]
                .as_str()
                .map(|name| name.trim_start_matches('/'))
                .filter(|name| !name.is_empty())?;
            let blocked = match name {
                "new" | "clear" | "sessions" | "session" => Some(super::commands::NEW_CONVERSATION),
                "models" | "model" => Some(super::commands::MODEL_PICKER),
                "exit" | "quit" => Some("it only runs in the OpenCode terminal"),
                _ => None,
            };
            Some(SlashCommand {
                name: name.to_owned(),
                aliases: Vec::new(),
                description: entry["description"].as_str().unwrap_or("").to_owned(),
                argument_hint: entry["input"]["hint"]
                    .as_str()
                    .filter(|hint| !hint.is_empty())
                    .map(str::to_owned),
                source: if name == "compact" {
                    CommandSource::Builtin
                } else {
                    CommandSource::Custom
                },
                blocked: blocked.map(Into::into),
            })
        })
        .collect()
}

fn turn_outcome(stop: &str, interrupting: bool) -> TurnOutcome {
    match stop {
        "end_turn" | "max_tokens" | "max_turn_requests" => TurnOutcome::Completed,
        "cancelled" => TurnOutcome::Interrupted,
        "refusal" => TurnOutcome::Failed,
        _ if interrupting => TurnOutcome::Interrupted,
        _ => TurnOutcome::Unknown,
    }
}

fn stop_notice(stop: &str) -> Option<&'static str> {
    match stop {
        "max_tokens" => Some("OpenCode stopped at the model's output token limit"),
        "max_turn_requests" => Some("OpenCode stopped at its request limit for one turn"),
        "refusal" => Some("The model refused to continue this turn"),
        _ => None,
    }
}

/// Presents a permission request with OpenCode's own options, in its order.
fn permission_request(
    request_id: String,
    params: &Value,
) -> (UserRequest, Vec<(String, String, String)>) {
    let call = &params["toolCall"];
    let options: Vec<(String, String, String)> = params["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|option| {
            Some((
                option["optionId"].as_str()?.to_owned(),
                option["name"].as_str()?.to_owned(),
                option["kind"].as_str().unwrap_or("").to_owned(),
            ))
        })
        .collect();
    let tool = call["title"].as_str().unwrap_or("a tool");
    let input = &call["rawInput"];
    let detail = input["command"]
        .as_str()
        .or_else(|| input["filePath"].as_str())
        .or_else(|| input["path"].as_str())
        .or_else(|| input["pattern"].as_str())
        .map(str::to_owned)
        .or_else(|| (!input.is_null()).then(|| input.to_string()));
    let request = UserRequest {
        id: request_id,
        kind: UserRequestKind::Select,
        title: format!("Allow {tool}?"),
        options: options
            .iter()
            .map(|(_, name, kind)| UserOption {
                value: name.clone(),
                description: match kind.as_str() {
                    "allow_once" => Some("Allow this call only".into()),
                    "allow_always" => Some("Allow similar calls without asking".into()),
                    "reject_once" => Some("Decline this call".into()),
                    "reject_always" => Some("Decline similar calls without asking".into()),
                    _ => None,
                },
            })
            .collect(),
        text: detail.unwrap_or_default(),
        placeholder: None,
        deadline: None,
    };
    (request, options)
}

/// The ACP outcome for an answer, or `None` when OpenCode offered nothing that matches.
fn permission_outcome(options: &[(String, String, String)], answer: &UserAnswer) -> Option<Value> {
    let by_kind = |kind: &str| {
        options
            .iter()
            .find(|(_, _, option_kind)| option_kind == kind)
            .map(|(id, _, _)| json!({"outcome":"selected","optionId":id}))
    };
    match answer {
        UserAnswer::Value(value) => options
            .iter()
            .find(|(id, name, _)| name == value || id == value)
            .map(|(id, _, _)| json!({"outcome":"selected","optionId":id})),
        UserAnswer::Confirmed(true) => by_kind("allow_once"),
        UserAnswer::Confirmed(false) => by_kind("reject_once"),
        UserAnswer::Cancelled { .. } => Some(json!({"outcome":"cancelled"})),
        UserAnswer::PermissionAuto => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Value {
        json!([
            {"id":"model","category":"model","type":"select","currentValue":"anthropic/claude-opus",
             "options":[{"value":"opencode/big-pickle","name":"Big Pickle"},
                        {"value":"anthropic/claude-opus","name":"Claude Opus"}]},
            {"id":"effort","category":"thought_level","type":"select","currentValue":"low",
             "options":[{"value":"low","name":"low"},{"value":"high","name":"high"}]},
            {"id":"mode","category":"mode","type":"select","currentValue":"build",
             "options":[{"value":"build","name":"build"},{"value":"plan","name":"plan"}]}
        ])
    }

    #[test]
    fn models_come_from_the_model_option_with_efforts_for_the_current_one() {
        let models = parse_models(&options(), true);
        assert_eq!(models.len(), 2);
        assert_eq!(
            models[0].key,
            ModelKey {
                provider: "opencode".into(),
                id: "opencode/big-pickle".into()
            }
        );
        assert_eq!(models[0].name, "Big Pickle");
        assert!(models[0].efforts.is_empty());
        assert_eq!(models[1].efforts, ["low", "high"]);
        assert_eq!(models[1].input, ["text", "image"]);
        assert_eq!(parse_models(&options(), false)[0].input, ["text"]);
        assert!(parse_models(&json!([]), true).is_empty());
        assert_eq!(model_key("plain").provider, "plain");
    }

    #[test]
    fn tool_updates_keep_the_tool_name_and_normalise_paths() {
        let mut tool = ToolState::default();
        merge_tool(
            &mut tool,
            &json!({"toolCallId":"c1","title":"Read","kind":"read","status":"pending",
                "rawInput":{"filePath":"src/main.rs"}}),
        );
        assert_eq!(tool.name, "read");
        assert_eq!(tool.input["path"], "src/main.rs");
        assert!(!tool.complete());
        merge_tool(
            &mut tool,
            &json!({"toolCallId":"c1","title":"src/main.rs","status":"completed",
                "rawOutput":{"output":"fn main() {}","metadata":{"exit":0}}}),
        );
        assert_eq!(tool.name, "read");
        assert_eq!(tool.intent.as_deref(), Some("src/main.rs"));
        assert!(tool.complete() && !tool.error());
        merge_tool(
            &mut tool,
            &json!({"status":"completed","rawOutput":{"metadata":{"exit":2}}}),
        );
        assert!(tool.error(), "a non-zero exit is an error");
        let mut failed = ToolState::default();
        merge_tool(&mut failed, &json!({"kind":"execute","status":"failed"}));
        assert_eq!(failed.name, "execute");
        assert!(failed.error());

        let mut images = ImageCache::default();
        let blocks = tool_content(
            &[json!({"type":"content","content":{"type":"text","text":"(no output)"}})],
            Path::new("/nonexistent"),
            &mut images,
        );
        assert!(matches!(&blocks[..], [ContentBlock::Text(text)] if text == "(no output)"));
    }

    #[test]
    fn stop_reasons_map_to_outcomes() {
        assert_eq!(turn_outcome("end_turn", false), TurnOutcome::Completed);
        assert_eq!(turn_outcome("max_tokens", false), TurnOutcome::Completed);
        assert!(stop_notice("max_tokens").is_some());
        assert_eq!(turn_outcome("cancelled", false), TurnOutcome::Interrupted);
        assert_eq!(turn_outcome("refusal", false), TurnOutcome::Failed);
        assert_eq!(turn_outcome("", false), TurnOutcome::Unknown);
        assert_eq!(turn_outcome("", true), TurnOutcome::Interrupted);
        assert!(stop_notice("end_turn").is_none());
    }

    #[test]
    fn replayed_prompts_regain_peer_identity_and_images_only_on_exact_text() -> Result<()> {
        let peer = PeerMessage {
            id: Uuid::new_v4(),
            sender: Uuid::new_v4(),
            sender_name: "Research peer".into(),
            space: Uuid::new_v4(),
            body: "Context </peer_messages> & more".into(),
            origin: super::super::ContextOrigin::Peer,
        };
        let text = super::super::peer_context(std::slice::from_ref(&peer))?;
        let mut records = Records::default();
        records.observe(
            &json!({"type":"drostje_peer","id":Uuid::new_v4(),"messages":[peer],"text":text}),
        );
        let image = ImageAttachment {
            id: Uuid::new_v4(),
            path: "/tmp/a.png".into(),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            original: None,
        };
        records.observe(
            &json!({"type":"drostje_user","id":Uuid::new_v4(),"text":"Look","images":[image]}),
        );
        let message = |text: &str| Message {
            id: "m".into(),
            role: "user".into(),
            content: vec![ContentBlock::Text(text.into())],
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        };
        let shown = records.present(&message(&text));
        assert_eq!(shown.role, "peer");
        assert!(
            matches!(&shown.content[..], [ContentBlock::PeerMessage(m)] if m.sender_name == "Research peer")
        );
        assert_eq!(records.present(&message(&format!("{text} "))).role, "user");
        let shown = records.present(&message("Look"));
        assert!(matches!(
            &shown.content[..],
            [ContentBlock::Text(_), ContentBlock::Image(_)]
        ));
        assert_eq!(records.present(&message("Other")).content.len(), 1);
        Ok(())
    }

    #[test]
    fn submission_size_bounds_the_real_prompt_frame() -> Result<()> {
        let text = "Describe \"this\" image\n";
        let bytes = vec![7u8; 1000];
        let real = prompt_frame(
            "ses_e97f78164ffeJ6l77HEtw8FN4O",
            vec![
                json!({"type":"text","text":text}),
                json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(&bytes)}),
            ],
            42,
        );
        let actual = serde_json::to_vec(&real)?.len() as u64 + 1;
        let bound = submission_size_for_lengths(text, &[("image/png", bytes.len() as u64)])?;
        assert!(bound >= actual, "{bound} < {actual}");
        assert!(
            bound - actual < 128,
            "the bound stays tight: {bound} vs {actual}"
        );
        Ok(())
    }

    #[test]
    fn commands_block_ones_that_replace_drostje_state() {
        let commands = parse_commands(&json!([
            {"name":"compact","description":"Compact the session"},
            {"name":"review","description":"Review changes","input":{"hint":"[commit]"}},
            {"name":"new","description":"New session"},
            {"name":"models"}
        ]));
        assert_eq!(commands[0].source, CommandSource::Builtin);
        assert_eq!(commands[1].argument_hint.as_deref(), Some("[commit]"));
        assert_eq!(commands[1].blocked, None);
        assert_eq!(
            commands[2].blocked.as_deref(),
            Some(super::super::commands::NEW_CONVERSATION)
        );
        assert_eq!(
            commands[3].blocked.as_deref(),
            Some(super::super::commands::MODEL_PICKER)
        );
    }

    #[test]
    fn permission_answers_select_offered_options() {
        let (request, options) = permission_request(
            "p".into(),
            &json!({"sessionId":"s","toolCall":{"toolCallId":"c","title":"bash","rawInput":{"command":"rm -rf build"}},
                "options":[{"optionId":"once","name":"Allow once","kind":"allow_once"},
                           {"optionId":"always","name":"Always allow","kind":"allow_always"},
                           {"optionId":"reject","name":"Reject","kind":"reject_once"}]}),
        );
        assert_eq!(request.kind, UserRequestKind::Select);
        assert_eq!(request.text, "rm -rf build");
        assert_eq!(request.options.len(), 3);
        assert_eq!(
            permission_outcome(&options, &UserAnswer::Value("Always allow".into())),
            Some(json!({"outcome":"selected","optionId":"always"}))
        );
        assert_eq!(
            permission_outcome(&options, &UserAnswer::Confirmed(false)),
            Some(json!({"outcome":"selected","optionId":"reject"}))
        );
        assert_eq!(
            permission_outcome(&options, &UserAnswer::Cancelled { timed_out: true }),
            Some(json!({"outcome":"cancelled"}))
        );
        assert_eq!(
            permission_outcome(&options, &UserAnswer::PermissionAuto),
            None
        );
        assert_eq!(
            permission_outcome(&options, &UserAnswer::Value("Bogus".into())),
            None
        );
    }

    #[test]
    fn saved_reference_must_name_this_project() -> Result<()> {
        let cwd = Path::new("/tmp/project");
        let reference = session_reference("ses_1", cwd)?;
        assert_eq!(saved_session(&reference, cwd)?, "ses_1");
        assert!(saved_session(&reference, Path::new("/tmp/other")).is_err());
        assert!(saved_session("not json", cwd).is_err());
        Ok(())
    }

    /// A fake `opencode acp`: a shell script answering each request line with canned frames.
    const FAKE_AGENT: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$(dirname "$0")/args.txt"
sid=ses_fake
reply() { printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$1" "$2"; }
update() { printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"%s","update":%s}}\n' "$sid" "$1"; }
options='[{"id":"model","category":"model","type":"select","currentValue":"opencode/big-pickle","options":[{"value":"opencode/big-pickle","name":"Big Pickle"},{"value":"anthropic/opus","name":"Opus"}]},{"id":"mode","category":"mode","type":"select","currentValue":"build","options":[{"value":"build","name":"build"},{"value":"plan","name":"plan"}]}]'
selected='[{"id":"model","category":"model","type":"select","currentValue":"anthropic/opus","options":[{"value":"opencode/big-pickle","name":"Big Pickle"},{"value":"anthropic/opus","name":"Opus"}]},{"id":"effort","category":"thought_level","type":"select","currentValue":"low","options":[{"value":"low","name":"low"},{"value":"high","name":"high"}]},{"id":"mode","category":"mode","type":"select","currentValue":"plan","options":[{"value":"build","name":"build"},{"value":"plan","name":"plan"}]}]'
prompt=
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      reply "$id" '{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":true}}}' ;;
    *'"method":"session/new"'*)
      reply "$id" "{\"sessionId\":\"$sid\",\"configOptions\":$options}"
      update '{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"review","description":"Review"}]}' ;;
    *'"method":"session/set_config_option"'*)
      printf '%s\n' "$line" >> "$(dirname "$0")/config.txt"
      reply "$id" "{\"configOptions\":$selected}" ;;
    *'"method":"session/cancel"'*)
      reply "$prompt" '{"stopReason":"cancelled"}' ;;
    *'"text":"Check it"'*)
      prompt=$id
      update '{"sessionUpdate":"agent_thought_chunk","messageId":"m1","content":{"type":"text","text":"Thinking"}}'
      update '{"sessionUpdate":"tool_call","toolCallId":"c1","title":"bash","kind":"execute","status":"pending","rawInput":{}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":"perm-1","method":"session/request_permission","params":{"sessionId":"ses_fake","toolCall":{"toolCallId":"c1","title":"bash","rawInput":{"command":"cargo check"}},"options":[{"optionId":"once","name":"Allow once","kind":"allow_once"},{"optionId":"reject","name":"Reject","kind":"reject_once"}]}}' ;;
    *'"optionId":"once"'*)
      update '{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed","title":"cargo check","rawInput":{"command":"cargo check"},"content":[{"type":"content","content":{"type":"text","text":"ok"}}],"rawOutput":{"output":"ok","metadata":{"exit":0}}}'
      update '{"sessionUpdate":"agent_message_chunk","messageId":"m2","content":{"type":"text","text":"Do"}}'
      update '{"sessionUpdate":"agent_message_chunk","messageId":"m2","content":{"type":"text","text":"ne"}}'
      update '{"sessionUpdate":"usage_update","used":1200,"size":200000}'
      reply "$prompt" '{"stopReason":"end_turn"}' ;;
    *'reject-me'*)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32603,"message":"busy"}}\n' "$id" ;;
    *'peer_messages'*)
      update '{"sessionUpdate":"agent_message_chunk","messageId":"m4","content":{"type":"text","text":"Noted"}}'
      reply "$id" '{"stopReason":"end_turn"}' ;;
    *'"text":"Wait"'*)
      prompt=$id
      update '{"sessionUpdate":"agent_message_chunk","messageId":"m3","content":{"type":"text","text":"Working"}}' ;;
  esac
done
"#;

    #[test]
    fn fake_agent_exercises_session_prompt_permission_completion_and_cancel() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("drostje-opencode-{}", Uuid::new_v4()));
        let cwd = root.join("repo");
        let session_dir = root.join("session");
        fs::create_dir_all(&cwd)?;
        let executable = root.join("opencode");
        fs::write(&executable, FAKE_AGENT)?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;

        let handle = smol::block_on(open(ConversationConfig {
            credential_env: Vec::new(),
            harness: "opencode".into(),
            executable,
            cwd: cwd.clone(),
            session_dir: session_dir.clone(),
            session_ref: None,
            model: Some(model_key("anthropic/opus")),
            effort: Some("high".into()),
            permission_mode: Some("plan".into()),
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
            .context("Timed out waiting for fake OpenCode event")
        };

        let (models, session_ref) = loop {
            match receive()? {
                ConversationEvent::Ready {
                    models,
                    session_ref,
                    peer_delivery,
                } => {
                    assert!(peer_delivery);
                    break (models, session_ref);
                }
                ConversationEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        };
        assert_eq!(models.len(), 2);
        assert_eq!(
            models[1].efforts,
            ["low", "high"],
            "efforts of the applied model"
        );
        assert_eq!(
            saved_session(&session_ref, &fs::canonicalize(&cwd)?)?,
            "ses_fake"
        );
        let args = fs::read_to_string(root.join("args.txt"))?;
        assert_eq!(args.trim(), "acp");
        let config = fs::read_to_string(root.join("config.txt"))?;
        let applied: Vec<&str> = config
            .lines()
            .map(|line| {
                if line.contains(r#""configId":"model""#) {
                    "model"
                } else if line.contains(r#""configId":"effort""#) {
                    "effort"
                } else {
                    "mode"
                }
            })
            .collect();
        assert_eq!(
            applied,
            ["model", "effort"],
            "mode already matched after the switch"
        );
        loop {
            match receive()? {
                ConversationEvent::StateChanged(ConversationState::Ready) => break,
                ConversationEvent::CommandsUpdated(commands) => {
                    assert_eq!(commands[0].name, "review")
                }
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
        let permission = loop {
            match receive()? {
                ConversationEvent::SubmissionAccepted { request_id: seen } => {
                    accepted = seen == request_id
                }
                ConversationEvent::MessageUpdated(message) if message.role == "user" => {
                    echoed = message.id == request_id.to_string()
                }
                ConversationEvent::UserRequest(request) => break request,
                ConversationEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        };
        assert!(accepted && echoed);
        assert_eq!(permission.text, "cargo check");
        handle
            .commands
            .send_blocking(ConversationCommand::Respond {
                request_id: permission.id.clone(),
                answer: UserAnswer::Value("Allow once".into()),
            })?;
        let mut reply = None;
        let mut tool = None;
        let mut context = None;
        loop {
            match receive()? {
                ConversationEvent::MessageUpdated(message) if message.id == "m2" => {
                    reply = Some((message_text(&message), message.raw))
                }
                ConversationEvent::ToolUpdated(activity) => tool = Some(activity),
                ConversationEvent::ContextUsageUpdated(usage) => context = Some(usage),
                ConversationEvent::TurnSettled {
                    request_id: seen,
                    outcome,
                } => {
                    assert_eq!(seen, request_id);
                    assert_eq!(outcome, TurnOutcome::Completed);
                    break;
                }
                _ => {}
            }
        }
        let (reply, raw) = reply.context("assistant reply")?;
        assert_eq!(reply, "Done", "chunks join into one message");
        assert_eq!(
            raw.map(|raw| raw["content"]["text"].clone()),
            Some(json!("ne")),
            "the message keeps its newest chunk as raw data"
        );
        let tool = tool.context("tool activity")?;
        assert_eq!(tool.name, "bash");
        assert_eq!(tool.intent.as_deref(), Some("cargo check"));
        assert!(tool.complete && !tool.error);
        assert_eq!(tool.exit_code, Some(0));
        assert_eq!(context.map(|usage| usage.used_tokens), Some(1200));

        let second = Uuid::new_v4();
        handle.commands.send_blocking(ConversationCommand::Submit {
            request_id: second,
            text: "Wait".into(),
            images: Vec::new(),
        })?;
        loop {
            if let ConversationEvent::MessageUpdated(message) = receive()?
                && message.id == "m3"
            {
                break;
            }
        }
        handle
            .commands
            .send_blocking(ConversationCommand::Interrupt)?;
        loop {
            if let ConversationEvent::TurnSettled {
                request_id,
                outcome,
            } = receive()?
            {
                assert_eq!(request_id, second);
                assert_eq!(outcome, TurnOutcome::Interrupted);
                break;
            }
        }
        handle.commands.send_blocking(ConversationCommand::Close)?;
        let history = read_transcript(&session_dir.join(TRANSCRIPT))?;
        assert_eq!(
            history
                .iter()
                .filter(|record| record["type"] == "drostje_user")
                .count(),
            2
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn idle_peer_delivery_runs_its_own_turn_and_errors_reject() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("drostje-opencode-{}", Uuid::new_v4()));
        let cwd = root.join("repo");
        fs::create_dir_all(&cwd)?;
        let executable = root.join("opencode");
        fs::write(&executable, FAKE_AGENT)?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
        let handle = smol::block_on(open(ConversationConfig {
            credential_env: Vec::new(),
            harness: "opencode".into(),
            executable,
            cwd,
            session_dir: root.join("session"),
            session_ref: None,
            model: None,
            effort: None,
            permission_mode: None,
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
            .context("Timed out waiting for fake OpenCode event")
        };
        while !matches!(
            receive()?,
            ConversationEvent::StateChanged(ConversationState::Ready)
        ) {}
        let delivery = |body: &str| {
            let (_lifetime, cancelled) = async_channel::bounded::<()>(1);
            (
                PeerDelivery {
                    attempt: Uuid::new_v4(),
                    messages: vec![PeerMessage {
                        id: Uuid::new_v4(),
                        sender: Uuid::new_v4(),
                        sender_name: "Peer".into(),
                        space: Uuid::new_v4(),
                        body: body.into(),
                        origin: super::super::ContextOrigin::Peer,
                    }],
                    cancelled,
                },
                _lifetime,
            )
        };

        let (peer, _lifetime) = delivery("Context for you");
        let attempt = peer.attempt;
        handle
            .commands
            .send_blocking(ConversationCommand::DeliverPeer(peer))?;
        let mut seen = Vec::new();
        loop {
            match receive()? {
                ConversationEvent::PeerTurnStarted { request_id } => {
                    assert_eq!(request_id, attempt);
                    seen.push("started");
                }
                ConversationEvent::PeerDelivery {
                    attempt: seen_attempt,
                    outcome,
                } => {
                    assert_eq!(seen_attempt, attempt);
                    assert!(
                        matches!(outcome, PeerDeliveryOutcome::Accepted),
                        "{outcome:?}"
                    );
                    seen.push("accepted");
                }
                ConversationEvent::MessageUpdated(message) if message.role == "peer" => {
                    seen.push("shown")
                }
                ConversationEvent::TurnSettled {
                    request_id,
                    outcome,
                } => {
                    assert_eq!(request_id, attempt);
                    assert_eq!(outcome, TurnOutcome::Completed);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(seen, ["started", "accepted", "shown"]);

        let (peer, _lifetime) = delivery("reject-me");
        let attempt = peer.attempt;
        handle
            .commands
            .send_blocking(ConversationCommand::DeliverPeer(peer))?;
        loop {
            match receive()? {
                ConversationEvent::PeerDelivery {
                    attempt: seen_attempt,
                    outcome,
                } => {
                    assert_eq!(seen_attempt, attempt);
                    assert!(
                        matches!(&outcome, PeerDeliveryOutcome::Rejected(reason) if reason.contains("busy")),
                        "{outcome:?}"
                    );
                    break;
                }
                ConversationEvent::PeerTurnStarted { .. } => panic!("a rejected peer has no turn"),
                _ => {}
            }
        }
        handle.commands.send_blocking(ConversationCommand::Close)?;
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
