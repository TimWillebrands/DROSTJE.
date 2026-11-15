//! Single-owner durable execution service. Relay loss revokes admission, not subprocesses.
use super::{
    config::RemoteHostConfig,
    transport::{self, EncodeError, Outgoing, Role, TransportEvent},
    wire::*,
};
use anyhow::{Context, Result, bail, ensure};
use drostje_browser_tools::{
    BrowserToolConfig, BrowserToolOperation, BrowserToolRequest, BrowserToolServer,
};
use drostje_harness::{
    self as harness, ConversationCommand, ConversationConfig, ConversationEvent,
    ConversationHandle, PeerDelivery,
};
use drostje_platform as platform;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};
use uuid::Uuid;

const LEASE: Duration = Duration::from_secs(15);
const EVENT_BUDGET: i64 = 64 * 1024 * 1024;
const MAX_RECEIPTS: i64 = 100_000;
const MAX_SESSIONS: usize = 256;
const ATTACHMENT_BUDGET: u64 = 512 * 1024 * 1024;
/// Journaled bytes loaded per replay page; a page holds at least one event.
const REPLAY_PAGE_BYTES: usize = 4 * 1024 * 1024;
/// Queued updates merged into one before any other input is handled.
const COALESCE_LIMIT: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub relay: RemoteHostConfig,
    pub data_dir: PathBuf,
    pub projects: BTreeMap<String, PathBuf>,
    pub executables: BTreeMap<String, PathBuf>,
}
impl HostConfig {
    pub fn load(path: &Path) -> Result<Self> {
        serde_json::from_reader(File::open(path).context("Open host configuration")?)
            .context("Parse host configuration")
    }
    fn validate(&mut self) -> Result<()> {
        self.relay.validate()?;
        ensure!(
            self.data_dir.is_absolute(),
            "Host data_dir must be absolute"
        );
        ensure!(
            !self.projects.is_empty(),
            "Configure at least one mounted project"
        );
        for (key, root) in &mut self.projects {
            ensure!(
                !key.is_empty() && root.is_absolute(),
                "Project keys must name absolute mounted roots"
            );
            *root = fs::canonicalize(&root).with_context(|| format!("Resolve project {key}"))?;
            ensure!(root.is_dir(), "Project {key} is not a directory");
        }
        for (harness, executable) in &mut self.executables {
            harness::executable_name(harness)?;
            ensure!(
                executable.is_absolute(),
                "Configured harness executable must be absolute"
            );
            *executable = fs::canonicalize(&executable)
                .with_context(|| format!("Resolve installed {harness} executable"))?;
            let metadata = fs::metadata(&executable)?;
            ensure!(
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
                "Configured {harness} executable is not executable"
            );
        }
        ensure!(
            !self.executables.is_empty(),
            "Configure an actually installed conversation harness"
        );
        Ok(())
    }
    fn project(&self, key: &str) -> Result<PathBuf> {
        let root = self
            .projects
            .get(key)
            .context("Project is not configured on this host")?;
        ensure!(
            fs::canonicalize(root)? == *root,
            "Configured project root changed; restart the host after reviewing the mount"
        );
        Ok(root.clone())
    }
}

struct Runtime {
    generation: Uuid,
    project: String,
    handle: ConversationHandle,
    attached: bool,
    closing: bool,
    peer_cancellations: BTreeMap<Uuid, async_channel::Sender<()>>,
    state: harness::ConversationState,
    ready: Option<ConversationEvent>,
    peer_availability: Option<ConversationEvent>,
    controls: BTreeMap<String, ConversationEvent>,
    /// Images whose bytes the attached desktop received since it last opened this
    /// Conversation; cleared on every Open, Detach and desktop loss.
    sent_images: HashSet<Uuid>,
}
struct PendingNetwork {
    thread: Uuid,
    generation: Uuid,
    deadline: Instant,
    reply: mpsc::Sender<std::result::Result<Value, String>>,
}
struct PendingOpen {
    id: Uuid,
    epoch: Uuid,
    project: String,
    config: RemoteConversationConfig,
}
enum Input {
    Transport(TransportEvent<Request>),
    Tool(BrowserToolRequest),
    Harness(Uuid, Uuid, ConversationEvent),
    HarnessClosed(Uuid, Uuid),
}

/// Runs until the transport worker terminates or persistence fails. Never retries a mutation.
pub fn run(mut config: HostConfig) -> Result<()> {
    config.validate()?;
    let token = config.relay.token()?;
    fs::create_dir_all(&config.data_dir)?;
    fs::set_permissions(&config.data_dir, fs::Permissions::from_mode(0o700))?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.data_dir.join("host.lock"))?;
    // An exclusive flock that never waits on a different host, released when `lock` drops.
    ensure!(
        lock.try_lock().is_ok(),
        "Another host owns this data directory"
    );
    let journal = Journal::open(&config.data_dir.join("host.sqlite3"))?;
    let attachment_bytes = attachment_storage_bytes(&config.data_dir.join("attachments"))?;
    let tools = BrowserToolServer::start_remote()?;
    let connection = transport::connect(&config.relay.relay_url, &token, Role::Host)?;
    let (input, incoming) = mpsc::sync_channel(256);
    let relay_input = input.clone();
    std::thread::Builder::new().name("host-relay-input".into()).spawn(move || {
        while let Ok(event) = connection.incoming.recv_blocking() {
            if relay_input.send(Input::Transport(event)).is_err() { break; }
        }
        // Revocation/supersession permanently ends the transport worker. Revoke
        // admission immediately, but leave the host actor and its active turns alive.
        let _ = relay_input.send(Input::Transport(TransportEvent::Disconnected(
            "Relay transport terminated; restart the host explicitly after reviewing its token/configuration".into(),
        )));
    })?;
    let tool_input = input.clone();
    let requests = tools.requests();
    std::thread::Builder::new()
        .name("host-tool-input".into())
        .spawn(move || {
            while let Ok(request) = requests.recv_blocking() {
                if tool_input.send(Input::Tool(request)).is_err() {
                    break;
                }
            }
        })?;
    let mut host = Host {
        config,
        journal,
        tools,
        outgoing: connection.outgoing,
        reconnect: connection.reconnect,
        input,
        runtimes: BTreeMap::new(),
        pending_opens: BTreeMap::new(),
        attachment_bytes,
        network: BTreeMap::new(),
        epoch: None,
        last_seen: Instant::now(),
        desktop: Capabilities::default(),
    };
    let mut deferred = None;
    loop {
        host.expire();
        host.step(&incoming, &mut deferred, Duration::from_millis(100))?;
    }
}

/// A queued update followed by updates of the same message or tool is replaced by the
/// latest of them; desktops replace both by ID, so the transcript ends up the same. The
/// first other input stops the merge and is returned to be handled next, in order.
fn coalesce(incoming: &mpsc::Receiver<Input>, input: Input) -> (Input, Option<Input>) {
    let Input::Harness(thread, generation, mut event) = input else {
        return (input, None);
    };
    if !matches!(
        event,
        ConversationEvent::MessageUpdated(_) | ConversationEvent::ToolUpdated(_)
    ) {
        return (Input::Harness(thread, generation, event), None);
    }
    let mut next = None;
    for _ in 0..COALESCE_LIMIT {
        match incoming.try_recv() {
            Ok(Input::Harness(other_thread, other_generation, update))
                if other_thread == thread
                    && other_generation == generation
                    && same_update(&event, &update) =>
            {
                event = update;
            }
            Ok(other) => {
                next = Some(other);
                break;
            }
            Err(_) => break,
        }
    }
    (Input::Harness(thread, generation, event), next)
}
fn same_update(current: &ConversationEvent, next: &ConversationEvent) -> bool {
    match (current, next) {
        (ConversationEvent::MessageUpdated(current), ConversationEvent::MessageUpdated(next)) => {
            current.id == next.id
        }
        (ConversationEvent::ToolUpdated(current), ConversationEvent::ToolUpdated(next)) => {
            current.id == next.id
        }
        _ => false,
    }
}

struct Host {
    config: HostConfig,
    journal: Journal,
    tools: BrowserToolServer,
    outgoing: async_channel::Sender<Outgoing>,
    reconnect: async_channel::Sender<()>,
    input: mpsc::SyncSender<Input>,
    runtimes: BTreeMap<Uuid, Runtime>,
    pending_opens: BTreeMap<Uuid, PendingOpen>,
    attachment_bytes: u64,
    network: BTreeMap<Uuid, PendingNetwork>,
    epoch: Option<Uuid>,
    last_seen: Instant,
    /// What the attached desktop declared in its latest request.
    desktop: Capabilities,
}
impl Host {
    /// Handle the next input, after merging the queued updates that follow it. Returns
    /// whether an input arrived within `wait`.
    fn step(
        &mut self,
        incoming: &mpsc::Receiver<Input>,
        deferred: &mut Option<Input>,
        wait: Duration,
    ) -> Result<bool> {
        let input = match deferred.take() {
            Some(input) => input,
            None => match incoming.recv_timeout(wait) {
                Ok(input) => input,
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(false),
                Err(mpsc::RecvTimeoutError::Disconnected) => bail!("Host input workers stopped"),
            },
        };
        let (input, next) = coalesce(incoming, input);
        *deferred = next;
        self.handle(input)?;
        Ok(true)
    }
    fn handle(&mut self, input: Input) -> Result<()> {
        match input {
            Input::Transport(TransportEvent::PeerConnected { epoch }) => {
                self.disconnect("Desktop attachment was replaced");
                self.epoch = Some(epoch);
                self.last_seen = Instant::now();
            }
            Input::Transport(TransportEvent::PeerDisconnected) => {
                self.disconnect("Desktop disconnected")
            }
            Input::Transport(TransportEvent::Disconnected(error)) => {
                eprintln!("Relay disconnected: {error}");
                self.disconnect("Relay disconnected");
            }
            // The transport bounds every frame, and so every body, by MAX_FRAME_BYTES.
            Input::Transport(TransportEvent::Payload(request)) => {
                let id = request.id;
                let thread = request.thread;
                if let Err(error) = self.request(request) {
                    self.send(Response::Rejected {
                        id,
                        thread,
                        message: format!("{error:#}"),
                    });
                }
            }
            Input::Transport(TransportEvent::InvalidPayload(error)) => {
                eprintln!("Invalid remote request: {error}");
                self.disconnect("Invalid remote protocol");
            }
            Input::Tool(request) => self.tool(request),
            Input::Harness(thread, generation, event) => self.event(thread, generation, event)?,
            Input::HarnessClosed(thread, generation) => self.closed(thread, generation)?,
        }
        Ok(())
    }
    fn send(&mut self, response: Response) {
        if self.epoch.is_none() {
            return;
        }
        self.deliver(Outgoing::encode(&response, None));
    }
    /// Send a journaled event exactly as stored, minus the bytes of images the attached
    /// desktop already received when it accepts that.
    fn send_journaled(&mut self, thread: Uuid, journaled: Journaled) {
        if self.epoch.is_none() {
            return;
        }
        let body = match self.runtimes.get_mut(&thread) {
            Some(runtime) if self.desktop.image_reuse => {
                reuse_images(&mut runtime.sent_images, journaled)
            }
            _ => Outgoing::serialized(journaled.payload, None),
        };
        self.deliver(body);
    }
    fn deliver(&mut self, body: std::result::Result<Outgoing, EncodeError>) {
        let body = match body {
            Ok(body) => Some(body),
            Err(EncodeError::Serialize(error)) => {
                eprintln!("Serialize remote response: {error}");
                self.disconnect("Response serialization failed");
                return;
            }
            Err(EncodeError::Oversized) => None,
        };
        let sent = body.is_some_and(|body| {
            smol::block_on(smol::future::or(
                async { self.outgoing.send(body).await.is_ok() },
                async {
                    smol::Timer::after(Duration::from_secs(1)).await;
                    false
                },
            ))
        });
        if !sent {
            // Events remain durable. Reattachment either replays them or reports an explicit gap.
            self.disconnect("Remote response queue unavailable; reattach to reconcile");
            let _ = self.reconnect.try_send(());
        }
    }
    fn fail_network(&mut self, thread: Option<Uuid>, reason: &str) {
        self.network.retain(|_, call| {
            if thread.is_none_or(|thread| thread == call.thread) {
                let _ = call.reply.send(Err(reason.into()));
                false
            } else {
                true
            }
        });
    }
    fn disconnect(&mut self, reason: &str) {
        self.epoch = None;
        self.desktop = Capabilities::default();
        self.pending_opens.clear();
        for runtime in self.runtimes.values_mut() {
            runtime.attached = false;
            runtime.peer_cancellations.clear();
            runtime.sent_images.clear();
        }
        self.fail_network(None, reason);
    }
    fn expire(&mut self) {
        if self.epoch.is_some() && self.last_seen.elapsed() > LEASE {
            self.disconnect("Desktop heartbeat expired");
            let _ = self.reconnect.try_send(());
        }
        self.network.retain(|_, pending| {
            if Instant::now() >= pending.deadline {
                let _ = pending.reply.send(Err(
                    "Desktop network response timed out; outcome unknown".into(),
                ));
                false
            } else {
                true
            }
        });
    }
    fn request(&mut self, request: Request) -> Result<()> {
        ensure!(
            request.version == VERSION,
            "Unsupported remote protocol version"
        );
        ensure!(
            self.epoch == Some(request.epoch) && self.last_seen.elapsed() <= LEASE,
            "Desktop attachment is stale; reconnect before issuing commands"
        );
        self.last_seen = Instant::now();
        self.desktop = request.capabilities;
        let Request {
            id,
            thread,
            operation,
            ..
        } = request;
        match operation {
            Operation::Heartbeat => {}
            Operation::Open {
                project,
                config,
                after,
                generation,
                resume,
            } => self.open(id, thread, project, config, after, generation, resume)?,
            Operation::Command(command) => self.command(id, thread, command)?,
            Operation::Detach => {
                if let Some(runtime) = self.runtimes.get_mut(&thread) {
                    runtime.attached = false;
                    runtime.peer_cancellations.clear();
                    runtime.sent_images.clear();
                }
                self.pending_opens.remove(&thread);
                self.fail_network(Some(thread), "Conversation detached from desktop");
                self.send(Response::Receipt {
                    id,
                    thread,
                    status: ReceiptStatus::Admitted,
                });
            }
            Operation::CancelPeer { attempt } => {
                if let Some(runtime) = self.runtimes.get_mut(&thread) {
                    runtime.peer_cancellations.remove(&attempt);
                }
            }
            Operation::Ack { through } => {
                let latest = self.journal.latest(thread)?;
                ensure!(
                    through <= latest,
                    "Acknowledgement exceeds host event cursor"
                );
                self.journal.db.execute(
                    "UPDATE sessions SET acknowledged=MAX(acknowledged,?2) WHERE thread=?1",
                    params![thread.to_string(), sql_cursor(through)?],
                )?;
            }
            Operation::NetworkResponse { call, result } => {
                if let Some(pending) = self.network.get(&call) {
                    ensure!(
                        pending.thread == thread
                            && self
                                .runtimes
                                .get(&thread)
                                .is_some_and(|runtime| runtime.attached
                                    && runtime.generation == pending.generation),
                        "Network response capability is stale"
                    );
                    if let Some(pending) = self.network.remove(&call) {
                        let _ = pending.reply.send(result);
                    }
                }
            }
            Operation::ReadDiff { path } => {
                let result = self
                    .read_diff(thread, path.as_deref())
                    .map_err(|error| format!("{error:#}"));
                self.send(Response::Data { id, thread, result });
            }
            Operation::ReadFile { path } => {
                let result = self
                    .read_file(thread, &path)
                    .map_err(|error| format!("{error:#}"));
                self.send(Response::Data { id, thread, result });
            }
        }
        Ok(())
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "Mirrors the authenticated Open frame with independent replay and runtime identities"
    )]
    fn open(
        &mut self,
        id: Uuid,
        thread: Uuid,
        project: String,
        mut config: RemoteConversationConfig,
        after: u64,
        generation: Option<Uuid>,
        resume: bool,
    ) -> Result<()> {
        if self
            .pending_opens
            .get(&thread)
            .is_some_and(|pending| pending.id == id)
        {
            return Ok(());
        }
        ensure!(
            !self
                .runtimes
                .get(&thread)
                .is_some_and(|runtime| runtime.closing),
            "Previous empty runtime is still closing; wait for termination before reattaching"
        );
        let root = self.config.project(&project)?;
        let mut stored = self.journal.session(thread)?;
        let mut replacing_empty = false;
        if let Some(previous) = &stored {
            ensure!(
                previous.project == project,
                "A remote Conversation cannot change host project"
            );
            ensure!(
                previous.root == root,
                "Configured project now points to a different checkout; existing remote sessions cannot be remapped"
            );
            let effects: i64 = self.journal.db.query_row(
                "SELECT COUNT(*) FROM effects WHERE thread=?1",
                [thread.to_string()],
                |row| row.get(0),
            )?;
            // PermissionAuto is executed on this host. The desktop deliberately does
            // not speculate about its outcome, so a later attach may carry the old mode.
            if effects > 0 {
                config.permission_mode = previous.config.permission_mode.clone();
            }
            let changed = previous.config.harness != config.harness
                || previous.config.effort != config.effort
                || previous.config.permission_mode != config.permission_mode;
            if changed {
                ensure!(
                    effects == 0,
                    "Remote launch settings are immutable after the first admitted turn"
                );
                if let Some(runtime) = self.runtimes.get_mut(&thread) {
                    ensure!(
                        matches!(
                            runtime.state,
                            harness::ConversationState::Starting
                                | harness::ConversationState::Ready
                        ),
                        "Wait for the empty remote Conversation to become idle before changing launch settings"
                    );
                    runtime
                        .handle
                        .commands
                        .try_send(ConversationCommand::Close)
                        .context("Close empty runtime before reconfiguration")?;
                    runtime.closing = true;
                    runtime.attached = false;
                    runtime.peer_cancellations.clear();
                    config.session_ref = None;
                    self.pending_opens.insert(
                        thread,
                        PendingOpen {
                            id,
                            epoch: self.epoch.context("Desktop attachment disappeared")?,
                            project,
                            config,
                        },
                    );
                    self.fail_network(Some(thread), "Empty Conversation launch settings changed");
                    return Ok(());
                }
                ensure!(
                    resume,
                    "Explicit Resume is required before replacing a lost runtime"
                );
                stored = None;
                config.session_ref = None;
                replacing_empty = true;
            }
        }
        if !self.runtimes.contains_key(&thread) {
            ensure!(
                self.runtimes.len() < MAX_SESSIONS,
                "Host runtime limit reached"
            );
            if let Some(stored) = stored {
                ensure!(
                    resume,
                    "Host runtime is no longer live. Explicit Resume is required; pending submissions will not be replayed"
                );
                let effects: i64 = self.journal.db.query_row(
                    "SELECT COUNT(*) FROM effects WHERE thread=?1",
                    [thread.to_string()],
                    |row| row.get(0),
                )?;
                ensure!(
                    stored.session_ref.is_some() || effects == 0,
                    "Host did not obtain a durable provider session before stopping; start a new Conversation, never replay the uncertain submission"
                );
                config = stored.config;
                config.session_ref = stored.session_ref;
                config.allow_empty_session = effects == 0;
            } else {
                let sessions: i64 =
                    self.journal
                        .db
                        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
                ensure!(
                    replacing_empty || (sessions >= 0 && usize::try_from(sessions)? < MAX_SESSIONS),
                    "Host durable session limit reached"
                );
                ensure!(
                    config.session_ref.is_none(),
                    "New remote Conversations cannot import desktop session paths"
                );
                ensure!(
                    replacing_empty || (generation.is_none() && after == 0),
                    "Unknown remote Conversation; cannot restore this runtime cursor"
                );
            }
            let executable = self
                .config
                .executables
                .get(&config.harness)
                .context("Harness is not installed/configured on this host")?
                .clone();
            let runtime_generation = Uuid::new_v4();
            let directory = self
                .config
                .data_dir
                .join("sessions")
                .join(thread.to_string())
                .join(runtime_generation.to_string());
            fs::create_dir_all(&directory)?;
            let handle = smol::block_on(harness::open(ConversationConfig {
                harness: config.harness.clone(),
                executable,
                cwd: root.clone(),
                session_dir: directory,
                credential_env: vec![self.config.relay.token_env.clone()],
                session_ref: config.session_ref.clone(),
                model: config.model.clone(),
                effort: config.effort.clone(),
                permission_mode: config.permission_mode.clone(),
                allow_empty_session: config.allow_empty_session,
                browser_tools: Some(BrowserToolConfig {
                    endpoint: self.tools.endpoint(),
                    thread,
                    network_token: runtime_generation,
                    remote: true,
                }),
            }))?;
            // Persist identity before consuming provider events or admitting commands.
            self.journal
                .start(thread, runtime_generation, &project, &root, &config)?;
            let events = handle.events.clone();
            let input = self.input.clone();
            std::thread::Builder::new()
                .name(format!("host-session-{thread}"))
                .spawn(move || {
                    while let Ok(event) = events.recv_blocking() {
                        if input
                            .send(Input::Harness(thread, runtime_generation, event))
                            .is_err()
                        {
                            return;
                        }
                    }
                    let _ = input.send(Input::HarnessClosed(thread, runtime_generation));
                })?;
            self.runtimes.insert(
                thread,
                Runtime {
                    generation: runtime_generation,
                    project,
                    handle,
                    attached: false,
                    closing: false,
                    peer_cancellations: BTreeMap::new(),
                    state: harness::ConversationState::Starting,
                    ready: None,
                    peer_availability: None,
                    controls: BTreeMap::new(),
                    sent_images: HashSet::new(),
                },
            );
        }
        let runtime = self
            .runtimes
            .get(&thread)
            .context("Runtime was not opened")?;
        let runtime_generation = runtime.generation;
        let mut cursor = if replacing_empty || (resume && generation != Some(runtime_generation)) {
            0
        } else {
            ensure!(
                generation.is_none_or(|generation| generation == runtime_generation),
                "Runtime generation changed; explicit Resume required"
            );
            after
        };
        let snapshots: Vec<ConversationEvent> = [
            runtime.ready.clone(),
            Some(ConversationEvent::StateChanged(runtime.state.clone())),
            runtime.peer_availability.clone(),
        ]
        .into_iter()
        .flatten()
        .chain(runtime.controls.values().cloned())
        .collect();
        let (first, mut latest) = self.journal.bounds(thread)?;
        ensure!(
            cursor <= latest,
            "Desktop event cursor is ahead of the host"
        );
        let gap = first != 0 && cursor.saturating_add(1) < first;
        ensure!(
            !gap || resume,
            "Remote replay gap: retained events begin at {first}; explicitly Resume with a fresh history view"
        );
        if gap {
            cursor = latest;
        }
        let session = self
            .journal
            .session(thread)?
            .context("Missing durable session")?;
        // This Open starts a new desktop view of the Conversation: it receives every
        // image's bytes again.
        self.runtimes
            .get_mut(&thread)
            .context("Runtime disappeared")?
            .sent_images
            .clear();
        self.send(Response::Opened {
            id,
            thread,
            generation: runtime_generation,
            session_ref: session.session_ref,
            latest_sequence: latest,
            replay_after: cursor,
        });
        // Page through the journal so a long transcript is never held in memory at once.
        let mut after = cursor;
        loop {
            let page = self.journal.replay_page(thread, after)?;
            let Some((last, _)) = page.last() else {
                break;
            };
            after = *last;
            for (_, journaled) in page {
                self.send_journaled(thread, journaled);
            }
        }
        for (id, status) in self.journal.receipts(thread)? {
            self.send(Response::Receipt { id, thread, status });
        }
        if gap {
            let journaled = self.journal.append(
                thread,
                runtime_generation,
                ConversationEvent::HistoryReset,
                Vec::new(),
            )?;
            self.send_journaled(thread, journaled);
            self.runtimes
                .get(&thread)
                .context("Runtime disappeared")?
                .handle
                .commands
                .try_send(ConversationCommand::LoadHistory { cursor: None })
                .context("Load authoritative history after replay gap")?;
        }
        for event in snapshots {
            let journaled = self
                .journal
                .append(thread, runtime_generation, event, Vec::new())?;
            if let Response::Event { sequence, .. } = &journaled.response {
                latest = *sequence;
            }
            self.send_journaled(thread, journaled);
        }
        self.send(Response::Recovered {
            thread,
            generation: runtime_generation,
            through: latest,
        });
        // A saturated transport cannot accidentally admit turns following partial reconciliation.
        if self.epoch.is_some() {
            self.runtimes
                .get_mut(&thread)
                .context("Runtime disappeared during recovery")?
                .attached = true;
        }
        Ok(())
    }
    fn closed(&mut self, thread: Uuid, generation: Uuid) -> Result<()> {
        if !self
            .runtimes
            .get(&thread)
            .is_some_and(|runtime| runtime.generation == generation)
        {
            return Ok(());
        }
        self.event(
            thread,
            generation,
            ConversationEvent::StateChanged(harness::ConversationState::Disconnected),
        )?;
        self.runtimes.remove(&thread);
        self.fail_network(Some(thread), "Harness runtime stopped");
        if let Some(pending) = self.pending_opens.remove(&thread) {
            if self.epoch != Some(pending.epoch) || self.last_seen.elapsed() > LEASE {
                return Ok(());
            }
            // Adapter event channels close after their child owner has reaped the process.
            // Only now may an empty Conversation release its old durable identity.
            let transaction = self.journal.db.transaction()?;
            transaction.execute("DELETE FROM events WHERE thread=?1", [thread.to_string()])?;
            transaction.execute("DELETE FROM sessions WHERE thread=?1", [thread.to_string()])?;
            transaction.commit()?;
            self.journal.retained.remove(&thread);
            if let Err(error) = self.open(
                pending.id,
                thread,
                pending.project,
                pending.config,
                0,
                None,
                false,
            ) {
                self.send(Response::Rejected {
                    id: pending.id,
                    thread,
                    message: format!("{error:#}"),
                });
            }
        }
        Ok(())
    }

    fn command(&mut self, id: Uuid, thread: Uuid, command: WireCommand) -> Result<()> {
        let runtime = self
            .runtimes
            .get(&thread)
            .context("Open the remote Conversation before issuing commands")?;
        ensure!(
            runtime.attached && !runtime.closing,
            "Reattach and reconcile the remote Conversation before issuing commands"
        );
        if let WireCommand::LoadHistory { cursor } = command {
            runtime
                .handle
                .commands
                .try_send(ConversationCommand::LoadHistory { cursor })
                .context("Request remote history")?;
            self.send(Response::Receipt {
                id,
                thread,
                status: ReceiptStatus::Admitted,
            });
            return Ok(());
        }
        if let Some(status) = self.journal.receipt(id, thread)? {
            self.send(Response::Receipt { id, thread, status });
            return Ok(());
        }
        let effect = match &command {
            WireCommand::Submit { request_id, .. } => Some(*request_id),
            WireCommand::DeliverPeer { attempt, .. } => Some(*attempt),
            _ => None,
        };
        if let Some(effect) = effect
            && let Some(status) = self.journal.effect(effect, thread)?
        {
            self.journal.record(id, thread, Some(effect), &status)?;
            self.send(Response::Receipt { id, thread, status });
            return Ok(());
        }
        if effect.is_some() {
            ensure!(
                matches!(
                    self.runtimes
                        .get(&thread)
                        .context("Runtime disappeared")?
                        .state,
                    harness::ConversationState::Ready
                ),
                "Remote runtime is not idle; no subsequent turn may queue behind an active turn"
            );
        }
        let mut staged_images = None;
        let command = match command {
            WireCommand::Submit {
                request_id,
                text,
                images,
            } => {
                let directory = self
                    .config
                    .data_dir
                    .join("attachments")
                    .join(thread.to_string());
                let mut staged = stage_submission_images(
                    request_id,
                    &text,
                    &images,
                    &directory,
                    ATTACHMENT_BUDGET.saturating_sub(self.attachment_bytes),
                )?;
                let images = staged.take_images();
                staged_images = Some(staged);
                ConversationCommand::Submit {
                    request_id,
                    text,
                    images,
                }
            }
            WireCommand::DeliverPeer { attempt, messages } => {
                ensure!(
                    messages.len() <= 32
                        && messages
                            .iter()
                            .all(|message| message.body.len() <= drostje_network::MAX_BODY_BYTES),
                    "Peer delivery exceeds host limits"
                );
                let (cancel, cancelled) = async_channel::bounded(1);
                self.runtimes
                    .get_mut(&thread)
                    .context("Runtime disappeared")?
                    .peer_cancellations
                    .insert(attempt, cancel);
                ConversationCommand::DeliverPeer(PeerDelivery {
                    attempt,
                    messages,
                    cancelled,
                })
            }
            WireCommand::Interrupt => {
                self.runtimes
                    .get_mut(&thread)
                    .context("Runtime disappeared")?
                    .peer_cancellations
                    .clear();
                ConversationCommand::Interrupt
            }
            WireCommand::SelectModel(model) => ConversationCommand::SelectModel(model),
            WireCommand::LoadHistory { cursor } => ConversationCommand::LoadHistory { cursor },
            WireCommand::Respond { request_id, answer } => {
                if matches!(answer, harness::UserAnswer::PermissionAuto)
                    && let Some(mut session) = self.journal.session(thread)?
                {
                    session.config.permission_mode =
                        harness::auto_permission_mode(&session.config.harness).map(str::to_owned);
                    self.journal.db.execute(
                        "UPDATE sessions SET config=?2 WHERE thread=?1",
                        params![thread.to_string(), serde_json::to_string(&session.config)?],
                    )?;
                }
                ConversationCommand::Respond { request_id, answer }
            }
        };
        // FULL synchronous SQLite commit precedes native queue admission. A crash in this
        // window leaves Unknown forever; no code path executes an existing receipt again.
        let unknown = ReceiptStatus::Unknown(
            "Host stopped before native admission could be confirmed".into(),
        );
        self.journal.record(id, thread, effect, &unknown)?;
        let status = match self
            .runtimes
            .get(&thread)
            .context("Runtime disappeared")?
            .handle
            .commands
            .try_send(command)
        {
            Ok(()) => {
                if let Some(staged) = staged_images.as_mut() {
                    self.attachment_bytes += staged.commit();
                }
                if effect.is_some() {
                    self.runtimes
                        .get_mut(&thread)
                        .context("Runtime disappeared")?
                        .state = harness::ConversationState::Running;
                }
                ReceiptStatus::Admitted
            }
            Err(error) => {
                ReceiptStatus::Rejected(format!("Harness command queue unavailable: {error}"))
            }
        };
        if let Err(error) = self.journal.update_receipt(id, effect, &status) {
            // Queue admission already happened; a persistence failure cannot be reported
            // as a known rejection. The pre-dispatch Unknown receipt remains authoritative.
            self.send(Response::Receipt { id, thread, status: ReceiptStatus::Unknown(format!("Native command may have been admitted, but its receipt could not be updated: {error:#}")) });
            if let Some(runtime) = self.runtimes.get_mut(&thread) {
                runtime.attached = false;
            }
            return Ok(());
        }
        self.send(Response::Receipt { id, thread, status });
        Ok(())
    }
    fn event(
        &mut self,
        thread: Uuid,
        generation: Uuid,
        mut event: ConversationEvent,
    ) -> Result<()> {
        if !self
            .runtimes
            .get(&thread)
            .is_some_and(|runtime| runtime.generation == generation)
        {
            return Ok(());
        }
        let runtime = self
            .runtimes
            .get_mut(&thread)
            .context("Runtime disappeared")?;
        match &event {
            ConversationEvent::Ready { session_ref, .. } => {
                runtime.ready = Some(event.clone());
                self.journal.db.execute(
                    "UPDATE sessions SET session_ref=?2 WHERE thread=?1",
                    params![thread.to_string(), session_ref],
                )?;
            }
            ConversationEvent::StateChanged(state) => runtime.state = state.clone(),
            ConversationEvent::PeerDeliveryAvailable { .. } => {
                runtime.peer_availability = Some(event.clone())
            }
            ConversationEvent::UserRequest(request) => {
                ensure!(
                    runtime.controls.len() < 256,
                    "Remote pending control limit exceeded"
                );
                runtime
                    .controls
                    .insert(format!("request:{}", request.id), event.clone());
            }
            ConversationEvent::UserRequestDismissed { request_id } => {
                runtime.controls.remove(&format!("request:{request_id}"));
            }
            ConversationEvent::CommandsUpdated(_) | ConversationEvent::CommandsUnavailable(_) => {
                runtime.controls.insert("commands".into(), event.clone());
            }
            ConversationEvent::ModelSelected(model) => {
                runtime.controls.insert("model".into(), event.clone());
                if let Some(mut session) = self.journal.session(thread)? {
                    session.config.model = Some(model.clone());
                    self.journal.db.execute(
                        "UPDATE sessions SET config=?2 WHERE thread=?1",
                        params![thread.to_string(), serde_json::to_string(&session.config)?],
                    )?;
                }
            }
            ConversationEvent::ContextUsageUpdated(_) => {
                runtime
                    .controls
                    .insert("context_usage".into(), event.clone());
            }
            ConversationEvent::LimitsUpdated(_) => {
                runtime.controls.insert("limits".into(), event.clone());
            }
            ConversationEvent::PeerDelivery { attempt, outcome } => {
                runtime.peer_cancellations.remove(attempt);
                if !matches!(outcome, harness::PeerDeliveryOutcome::Accepted) {
                    runtime.state = harness::ConversationState::Ready;
                }
            }
            ConversationEvent::Failed {
                request_id: Some(_),
                ..
            } => runtime.state = harness::ConversationState::Ready,
            _ => {}
        }
        let images = match event_images(&mut event) {
            Ok(images) => images,
            Err(error) => {
                event = ConversationEvent::Notice {
                    level: "error".into(),
                    text: format!("Remote image transfer failed: {error:#}"),
                };
                Vec::new()
            }
        };
        let journaled = self.journal.append(thread, generation, event, images)?;
        if self
            .runtimes
            .get(&thread)
            .is_some_and(|runtime| runtime.attached)
        {
            self.send_journaled(thread, journaled);
        }
        Ok(())
    }
    fn tool(&mut self, request: BrowserToolRequest) {
        let BrowserToolRequest {
            thread,
            operation,
            reply,
        } = request;
        if let BrowserToolOperation::ShowRemoteArtifact {
            name,
            title,
            html,
            placement,
        } = operation
        {
            return self.artifact(thread, name, title, html, placement, reply);
        }
        if let BrowserToolOperation::CompleteArtifact { id } = operation {
            return self.complete_artifact(thread, id, reply);
        }
        let result = (|| -> Result<(Uuid, Value)> {
            let BrowserToolOperation::Network { token, action } = operation else {
                bail!(
                    "The browser tool is unavailable on the remote host; local paths cannot be opened on the desktop"
                )
            };
            let runtime = self
                .runtimes
                .get(&thread)
                .context("Remote runtime is not live")?;
            ensure!(
                runtime.generation == token
                    && runtime.attached
                    && self.epoch.is_some()
                    && self.last_seen.elapsed() <= LEASE,
                "Desktop Network coordinator is disconnected or this runtime capability was revoked"
            );
            ensure!(
                self.network.len() < 64,
                "Remote network request queue is full"
            );
            let action = match action {
                drostje_network::Action::Members => json!({"action":"members"}),
                drostje_network::Action::Send { recipient, body } => {
                    json!({"action":"send","recipient":recipient,"body":body})
                }
                drostje_network::Action::Broadcast { body } => {
                    json!({"action":"broadcast","body":body})
                }
            };
            Ok((runtime.generation, action))
        })();
        match result {
            Ok((generation, action)) => {
                let call = Uuid::new_v4();
                self.network.insert(
                    call,
                    PendingNetwork {
                        thread,
                        generation,
                        reply,
                        deadline: Instant::now() + Duration::from_secs(25),
                    },
                );
                self.send(Response::NetworkRequest {
                    thread,
                    generation,
                    call,
                    action,
                });
            }
            Err(error) => {
                let _ = reply.send(Err(format!("{error:#}")));
            }
        }
    }
    /// Forward an artifact's HTML to the attached desktop, which shows a local copy.
    fn artifact(
        &mut self,
        thread: Uuid,
        name: String,
        title: Option<String>,
        html: String,
        placement: drostje_browser_tools::ArtifactPlacement,
        reply: std::sync::mpsc::Sender<std::result::Result<Value, String>>,
    ) {
        let supported = self.desktop.artifacts;
        self.desktop_call(
            thread,
            supported,
            "The attached desktop DROSTJE cannot show remote artifacts; update DROSTJE on the desktop, or ask in plain text",
            reply,
            |generation, call| Response::ArtifactRequest {
                thread,
                generation,
                call,
                name,
                title,
                html,
                placement,
            },
        );
    }
    /// Forward an artifact completion to the attached desktop, which owns the page.
    fn complete_artifact(
        &mut self,
        thread: Uuid,
        browser: Uuid,
        reply: std::sync::mpsc::Sender<std::result::Result<Value, String>>,
    ) {
        let supported = self.desktop.artifact_completion;
        self.desktop_call(
            thread,
            supported,
            "The attached desktop DROSTJE cannot complete artifacts; update DROSTJE on the desktop",
            reply,
            |generation, call| Response::ArtifactComplete {
                thread,
                generation,
                call,
                browser,
            },
        );
    }
    /// A desktop-side artifact operation for this runtime, answered like a Network call.
    fn desktop_call(
        &mut self,
        thread: Uuid,
        supported: bool,
        unsupported: &str,
        reply: std::sync::mpsc::Sender<std::result::Result<Value, String>>,
        request: impl FnOnce(Uuid, Uuid) -> Response,
    ) {
        let result = (|| -> Result<Uuid> {
            let runtime = self
                .runtimes
                .get(&thread)
                .context("Remote runtime is not live")?;
            ensure!(
                runtime.attached && self.epoch.is_some() && self.last_seen.elapsed() <= LEASE,
                "The desktop is disconnected; artifacts work only while it is attached"
            );
            ensure!(supported, "{unsupported}");
            ensure!(
                self.network.len() < 64,
                "Remote artifact request queue is full"
            );
            Ok(runtime.generation)
        })();
        match result {
            Ok(generation) => {
                let call = Uuid::new_v4();
                self.network.insert(
                    call,
                    PendingNetwork {
                        thread,
                        generation,
                        reply,
                        deadline: Instant::now() + Duration::from_secs(25),
                    },
                );
                self.send(request(generation, call));
            }
            Err(error) => {
                let _ = reply.send(Err(format!("{error:#}")));
            }
        }
    }
    fn root(&self, thread: Uuid) -> Result<PathBuf> {
        if let Some(runtime) = self.runtimes.get(&thread) {
            return self.config.project(&runtime.project);
        }
        let stored = self
            .journal
            .session(thread)?
            .context("Unknown remote Conversation")?;
        let root = self.config.project(&stored.project)?;
        ensure!(
            root == stored.root,
            "Configured project now points to a different checkout"
        );
        Ok(root)
    }
    fn read_file(&self, thread: Uuid, path: &str) -> Result<Value> {
        let path = confined_path(&self.root(thread)?, path)?;
        let mut file = File::open(&path)?;
        ensure!(
            file.metadata()?.is_file(),
            "Remote file is not a regular file"
        );
        let mut bytes = Vec::new();
        (&mut file).take(512 * 1024 + 1).read_to_end(&mut bytes)?;
        let truncated = bytes.len() > 512 * 1024;
        bytes.truncate(512 * 1024);
        let content = String::from_utf8(bytes)
            .context("Remote file is binary or has invalid UTF-8 at the read boundary")?;
        Ok(json!({"content":content,"truncated":truncated}))
    }
    fn read_diff(&self, thread: Uuid, path: Option<&str>) -> Result<Value> {
        let root = self.root(thread)?;
        let selection = path.map(|path| confined_path(&root, path)).transpose()?;
        // The existing diff engine bounds git/untracked output and computes exact summaries.
        // Only the patch text crosses the relay, so display rows are never parsed here.
        let mut diff = platform::working_tree_patch(&root)?;
        if let Some(path) = selection.filter(|path| path != &root) {
            let relative = path
                .strip_prefix(&root)?
                .to_str()
                .context("Remote diff path is not valid UTF-8")?;
            let directory = path.is_dir();
            let mut selected = false;
            let mut patch = String::new();
            for line in diff.patch.split_inclusive('\n') {
                if line.starts_with("diff --git ") {
                    let header = drostje_diff::parse(line);
                    selected = header.rows.first().is_some_and(|row| {
                        let name: &str = row.text.as_ref();
                        name == relative || (directory && Path::new(name).starts_with(relative))
                    });
                }
                if selected {
                    patch.push_str(line);
                }
            }
            let rows = drostje_diff::parse(&patch);
            diff.changed_files = rows
                .rows
                .iter()
                .filter(|row| matches!(row.kind, drostje_diff::RowKind::File(_)))
                .count();
            diff.additions = rows
                .rows
                .iter()
                .filter(|row| matches!(row.kind, drostje_diff::RowKind::Added))
                .count();
            diff.deletions = rows
                .rows
                .iter()
                .filter(|row| matches!(row.kind, drostje_diff::RowKind::Removed))
                .count();
            diff.patch = patch;
        }
        Ok(
            json!({"head_branch":diff.head_branch,"patch":diff.patch,"changed_files":diff.changed_files,"additions":diff.additions,"deletions":diff.deletions,"truncated":diff.truncated}),
        )
    }
}
/// Strip from `journaled` the bytes of images `sent` already holds, and record the rest.
/// Unchanged events go out as their stored JSON, without serializing them again.
fn reuse_images(
    sent: &mut HashSet<Uuid>,
    journaled: Journaled,
) -> std::result::Result<Outgoing, EncodeError> {
    let Journaled {
        mut response,
        payload,
    } = journaled;
    let Response::Event { images, .. } = &mut response else {
        return Outgoing::serialized(payload, None);
    };
    if images.iter().all(|image| !sent.contains(&image.id)) {
        sent.extend(images.iter().map(|image| image.id));
        return Outgoing::serialized(payload, None);
    }
    images.retain(|image| sent.insert(image.id));
    Outgoing::encode(&response, None)
}
fn sql_cursor(value: u64) -> Result<i64> {
    i64::try_from(value).context("Remote cursor exceeds SQLite's nonnegative integer range")
}
fn wire_cursor(value: i64) -> Result<u64> {
    u64::try_from(value).context("Durable remote cursor is negative")
}
fn attachment_storage_bytes(root: &Path) -> Result<u64> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
        Ok(metadata) => ensure!(
            metadata.file_type().is_dir(),
            "Host attachment root must be an owned directory"
        ),
    }
    let mut directories = vec![root.to_path_buf()];
    let mut bytes = 0u64;
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(entry.path());
            } else {
                ensure!(
                    kind.is_file(),
                    "Host attachment storage contains a non-regular file"
                );
                bytes = bytes
                    .checked_add(metadata.len())
                    .context("Host attachment byte count overflow")?;
                ensure!(
                    bytes <= ATTACHMENT_BUDGET,
                    "Host attachment storage exceeds its durable 512 MiB budget"
                );
            }
        }
    }
    Ok(bytes)
}

fn confined_path(root: &Path, path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    ensure!(!path.as_os_str().is_empty(), "Remote path is empty");
    let resolved = fs::canonicalize(if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    })?;
    ensure!(
        resolved.starts_with(root),
        "Remote path escapes the configured project root"
    );
    Ok(resolved)
}

struct SavedSession {
    project: String,
    root: PathBuf,
    config: RemoteConversationConfig,
    session_ref: Option<String>,
}
#[derive(Clone, Copy, Default)]
struct RetainedEvents {
    count: i64,
    bytes: i64,
}
struct Journal {
    db: Connection,
    retained: BTreeMap<Uuid, RetainedEvents>,
}
/// A journaled response and the exact JSON stored for it.
struct Journaled {
    response: Response,
    payload: String,
}
impl Journal {
    fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS sessions(thread TEXT PRIMARY KEY, generation TEXT NOT NULL, project TEXT NOT NULL, root TEXT NOT NULL, config TEXT NOT NULL, session_ref TEXT, sequence INTEGER NOT NULL DEFAULT 0, acknowledged INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS events(thread TEXT NOT NULL, sequence INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(thread,sequence));
            CREATE TABLE IF NOT EXISTS receipts(id TEXT PRIMARY KEY, thread TEXT NOT NULL, effect TEXT, status TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS effects(id TEXT PRIMARY KEY, thread TEXT NOT NULL, status TEXT NOT NULL, settled INTEGER NOT NULL DEFAULT 0);")?;
        let unknown = serde_json::to_string(&ReceiptStatus::Unknown("Host process restarted; prior native admission/turn outcome may be unknown. Submission will not be replayed".into()))?;
        db.execute("UPDATE receipts SET status=?1 WHERE effect IN (SELECT id FROM effects WHERE settled=0)", [&unknown])?;
        db.execute("UPDATE effects SET status=?1 WHERE settled=0", [&unknown])?;
        // Recover exact totals once; token events must never rescan retained transcripts.
        let retained = {
            let mut statement = db.prepare("SELECT thread,COUNT(*),COALESCE(SUM(LENGTH(CAST(payload AS BLOB))),0) FROM events GROUP BY thread")?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            rows.map(|row| {
                let (thread, count, bytes) = row?;
                ensure!(
                    count >= 0 && bytes >= 0,
                    "Invalid durable event retention totals"
                );
                Ok((Uuid::parse_str(&thread)?, RetainedEvents { count, bytes }))
            })
            .collect::<Result<BTreeMap<_, _>>>()?
        };
        Ok(Self { db, retained })
    }
    fn session(&self, thread: Uuid) -> Result<Option<SavedSession>> {
        let row: Option<(String, String, String, Option<String>)> = self
            .db
            .query_row(
                "SELECT project,root,config,session_ref FROM sessions WHERE thread=?1",
                [thread.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        row.map(|(project, root, config, session_ref)| {
            Ok(SavedSession {
                project,
                root: PathBuf::from(root),
                config: serde_json::from_str(&config)?,
                session_ref,
            })
        })
        .transpose()
    }
    fn start(
        &mut self,
        thread: Uuid,
        generation: Uuid,
        project: &str,
        root: &Path,
        config: &RemoteConversationConfig,
    ) -> Result<()> {
        let transaction = self.db.transaction()?;
        transaction.execute("INSERT INTO sessions(thread,generation,project,root,config,session_ref) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(thread) DO UPDATE SET generation=excluded.generation,config=excluded.config,session_ref=excluded.session_ref,sequence=0,acknowledged=0", params![thread.to_string(), generation.to_string(), project, root.to_str().context("Project root is not valid UTF-8")?, serde_json::to_string(config)?, config.session_ref])?;
        transaction.execute("DELETE FROM events WHERE thread=?1", [thread.to_string()])?;
        transaction.commit()?;
        self.retained.insert(thread, RetainedEvents::default());
        Ok(())
    }
    fn bounds(&self, thread: Uuid) -> Result<(u64, u64)> {
        let first: i64 = self
            .db
            .prepare_cached("SELECT COALESCE(MIN(sequence),0) FROM events WHERE thread=?1")?
            .query_row([thread.to_string()], |row| row.get(0))?;
        Ok((wire_cursor(first)?, self.latest(thread)?))
    }
    fn latest(&self, thread: Uuid) -> Result<u64> {
        let latest: i64 = self
            .db
            .prepare_cached("SELECT sequence FROM sessions WHERE thread=?1")?
            .query_row([thread.to_string()], |row| row.get(0))
            .optional()?
            .unwrap_or(0);
        wire_cursor(latest)
    }
    fn append(
        &mut self,
        thread: Uuid,
        generation: Uuid,
        event: ConversationEvent,
        images: Vec<WireImage>,
    ) -> Result<Journaled> {
        let latest = self.latest(thread)?;
        let sequence = latest
            .checked_add(1)
            .context("Remote event sequence overflow")?;
        let stored_sequence = sql_cursor(sequence)?;
        let mut response = Response::Event {
            thread,
            generation,
            sequence,
            event,
            images,
        };
        let mut payload = serde_json::to_string(&response)?;
        if payload.len() > MAX_FRAME_BYTES - 4096 {
            // Native terminal/control frames are bounded by their adapters. Preserve their
            // identity and semantics even if a provider supplies an enormous reason string.
            if let Response::Event { event, images, .. } = &mut response {
                images.clear();
                match event {
                    ConversationEvent::PeerDelivery {
                        outcome:
                            harness::PeerDeliveryOutcome::Rejected(reason)
                            | harness::PeerDeliveryOutcome::Unknown(reason),
                        ..
                    }
                    | ConversationEvent::Failed {
                        message: reason, ..
                    } => {
                        let mut boundary = 4096.min(reason.len());
                        while !reason.is_char_boundary(boundary) {
                            boundary -= 1;
                        }
                        reason.truncate(boundary);
                        reason.push_str(" [remote reason truncated]");
                    }
                    ConversationEvent::TurnSettled { .. }
                    | ConversationEvent::SubmissionAccepted { .. }
                    | ConversationEvent::PeerTurnStarted { .. }
                    | ConversationEvent::StateChanged(_) => {}
                    _ => {
                        *event = ConversationEvent::Failed { request_id: None, message: "Host content event exceeded the remote frame budget and could not be transferred; request a smaller history page. No command was retried.".into() };
                    }
                }
            }
            payload = serde_json::to_string(&response)?;
        }
        let mut retained = self.retained.get(&thread).copied().unwrap_or_default();
        retained.count = retained
            .count
            .checked_add(1)
            .context("Retained event count overflow")?;
        retained.bytes = retained
            .bytes
            .checked_add(i64::try_from(payload.len())?)
            .context("Retained event byte count overflow")?;
        let transaction = self.db.transaction()?;
        transaction
            .prepare_cached("INSERT INTO events(thread,sequence,payload) VALUES(?1,?2,?3)")?
            .execute(params![thread.to_string(), stored_sequence, payload])?;
        let effect = match &response {
            Response::Event {
                event: ConversationEvent::TurnSettled { request_id, .. },
                ..
            } => Some(*request_id),
            Response::Event {
                event: ConversationEvent::PeerDelivery { attempt, .. },
                ..
            } => Some(*attempt),
            Response::Event {
                event:
                    ConversationEvent::Failed {
                        request_id: Some(id),
                        ..
                    },
                ..
            } => Some(*id),
            _ => None,
        };
        if let Some(effect) = effect {
            transaction
                .prepare_cached("UPDATE effects SET settled=1 WHERE id=?1")?
                .execute([effect.to_string()])?;
        }
        transaction
            .prepare_cached("UPDATE sessions SET sequence=?2 WHERE thread=?1")?
            .execute(params![thread.to_string(), stored_sequence])?;
        // Explicit replay-gap errors, never a silently truncated transcript.
        while retained.count > 2048 || retained.bytes > EVENT_BUDGET {
            let (oldest, bytes): (i64, i64) = transaction.prepare_cached(
                "SELECT sequence,LENGTH(CAST(payload AS BLOB)) FROM events WHERE thread=?1 ORDER BY sequence LIMIT 1",
            )?.query_row([thread.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))?;
            ensure!(
                oldest >= 0 && bytes >= 0 && retained.count > 0 && retained.bytes >= bytes,
                "Invalid durable event retention accounting"
            );
            transaction
                .prepare_cached("DELETE FROM events WHERE thread=?1 AND sequence=?2")?
                .execute(params![thread.to_string(), oldest])?;
            retained.count -= 1;
            retained.bytes -= bytes;
        }
        transaction.commit()?;
        self.retained.insert(thread, retained);
        Ok(Journaled { response, payload })
    }
    /// The journaled events after `after`, in sequence order, up to `REPLAY_PAGE_BYTES`
    /// of them; empty once none remain.
    fn replay_page(&self, thread: Uuid, after: u64) -> Result<Vec<(u64, Journaled)>> {
        let mut statement = self.db.prepare_cached(
            "SELECT sequence,payload FROM events WHERE thread=?1 AND sequence>?2 ORDER BY sequence LIMIT 256",
        )?;
        let mut rows = statement.query(params![thread.to_string(), sql_cursor(after)?])?;
        let (mut page, mut bytes) = (Vec::new(), 0usize);
        while bytes < REPLAY_PAGE_BYTES
            && let Some(row) = rows.next()?
        {
            let sequence = wire_cursor(row.get(0)?)?;
            let payload: String = row.get(1)?;
            bytes += payload.len();
            let response = serde_json::from_str(&payload)?;
            page.push((sequence, Journaled { response, payload }));
        }
        Ok(page)
    }
    #[cfg(test)]
    fn replay(&self, thread: Uuid, after: u64) -> Result<Vec<Response>> {
        let mut responses = Vec::new();
        let mut after = after;
        loop {
            let page = self.replay_page(thread, after)?;
            let Some((last, _)) = page.last() else {
                return Ok(responses);
            };
            after = *last;
            responses.extend(page.into_iter().map(|(_, journaled)| journaled.response));
        }
    }
    fn receipt(&self, id: Uuid, thread: Uuid) -> Result<Option<ReceiptStatus>> {
        self.lookup("receipts", id, thread)
    }
    fn effect(&self, id: Uuid, thread: Uuid) -> Result<Option<ReceiptStatus>> {
        self.lookup("effects", id, thread)
    }
    fn lookup(&self, table: &str, id: Uuid, thread: Uuid) -> Result<Option<ReceiptStatus>> {
        let row: Option<(String, String)> = self
            .db
            .query_row(
                &format!("SELECT thread,status FROM {table} WHERE id=?1"),
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(owner, status)| {
            ensure!(
                owner == thread.to_string(),
                "Request UUID belongs to another Conversation"
            );
            Ok(serde_json::from_str(&status)?)
        })
        .transpose()
    }
    fn receipts(&self, thread: Uuid) -> Result<Vec<(Uuid, ReceiptStatus)>> {
        let mut statement = self
            .db
            .prepare("SELECT id,status FROM receipts WHERE thread=?1")?;
        let rows = statement.query_map([thread.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (id, status) = row?;
            Ok((Uuid::parse_str(&id)?, serde_json::from_str(&status)?))
        })
        .collect()
    }
    fn record(
        &mut self,
        id: Uuid,
        thread: Uuid,
        effect: Option<Uuid>,
        status: &ReceiptStatus,
    ) -> Result<()> {
        let count: i64 = self
            .db
            .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))?;
        ensure!(
            count < MAX_RECEIPTS,
            "Host durable receipt limit reached; archive this host data directory before admitting more commands"
        );
        let status = serde_json::to_string(status)?;
        let transaction = self.db.transaction()?;
        transaction.execute(
            "INSERT INTO receipts(id,thread,effect,status) VALUES(?1,?2,?3,?4)",
            params![
                id.to_string(),
                thread.to_string(),
                effect.map(|id| id.to_string()),
                status
            ],
        )?;
        if let Some(effect) = effect {
            transaction.execute(
                "INSERT OR IGNORE INTO effects(id,thread,status) VALUES(?1,?2,?3)",
                params![effect.to_string(), thread.to_string(), status],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
    fn update_receipt(
        &mut self,
        id: Uuid,
        effect: Option<Uuid>,
        status: &ReceiptStatus,
    ) -> Result<()> {
        let rejected = matches!(status, ReceiptStatus::Rejected(_));
        let status = serde_json::to_string(status)?;
        let transaction = self.db.transaction()?;
        transaction.execute(
            "UPDATE receipts SET status=?2 WHERE id=?1",
            params![id.to_string(), status],
        )?;
        if let Some(effect) = effect {
            transaction.execute(
                "UPDATE effects SET status=?2,settled=MAX(settled,?3) WHERE id=?1",
                params![effect.to_string(), status, rejected],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
    #[cfg(test)]
    fn settled(&self, effect: Uuid) -> Result<()> {
        self.db.execute(
            "UPDATE effects SET settled=1 WHERE id=?1",
            [effect.to_string()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("drostje-host-test-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn journal_crash_never_replays_uncertain_admission() {
        let temp = Temp::new();
        let path = temp.0.join("journal.sqlite3");
        let (id, thread, effect) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        {
            let mut journal = Journal::open(&path).unwrap();
            journal
                .record(
                    id,
                    thread,
                    Some(effect),
                    &ReceiptStatus::Unknown("before queue".into()),
                )
                .unwrap();
            journal
                .update_receipt(id, Some(effect), &ReceiptStatus::Admitted)
                .unwrap();
        }
        let journal = Journal::open(&path).unwrap();
        assert!(matches!(
            journal.receipt(id, thread).unwrap(),
            Some(ReceiptStatus::Unknown(_))
        ));
        assert!(matches!(
            journal.effect(effect, thread).unwrap(),
            Some(ReceiptStatus::Unknown(_))
        ));
        assert!(journal.receipt(id, Uuid::new_v4()).is_err());
    }
    #[test]
    fn journal_preserves_settled_receipts_across_restart() {
        let temp = Temp::new();
        let path = temp.0.join("journal.sqlite3");
        let (id, thread, effect) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        {
            let mut journal = Journal::open(&path).unwrap();
            journal
                .record(id, thread, Some(effect), &ReceiptStatus::Admitted)
                .unwrap();
            journal.settled(effect).unwrap();
        }
        assert!(matches!(
            Journal::open(&path).unwrap().receipt(id, thread).unwrap(),
            Some(ReceiptStatus::Admitted)
        ));
    }
    #[test]
    fn project_paths_reject_traversal_and_external_symlinks() {
        let temp = Temp::new();
        let root = temp.0.join("project");
        fs::create_dir(&root).unwrap();
        fs::write(temp.0.join("secret"), "private").unwrap();
        fs::write(root.join("ok"), "visible").unwrap();
        std::os::unix::fs::symlink(temp.0.join("secret"), root.join("link")).unwrap();
        assert!(confined_path(&root, "../secret").is_err());
        assert!(confined_path(&root, "link").is_err());
        assert_eq!(confined_path(&root, "ok").unwrap(), root.join("ok"));
    }
    #[test]
    fn durable_events_reconcile_once_in_sequence() {
        let temp = Temp::new();
        let mut journal = Journal::open(&temp.0.join("journal.sqlite3")).unwrap();
        let (thread, generation) = (Uuid::new_v4(), Uuid::new_v4());
        let config = RemoteConversationConfig {
            harness: "pi".into(),
            session_ref: None,
            model: None,
            effort: None,
            permission_mode: None,
            allow_empty_session: false,
        };
        journal
            .start(thread, generation, "project", &temp.0, &config)
            .unwrap();
        journal
            .append(
                thread,
                generation,
                ConversationEvent::StateChanged(harness::ConversationState::Running),
                Vec::new(),
            )
            .unwrap();
        journal
            .append(
                thread,
                generation,
                ConversationEvent::StateChanged(harness::ConversationState::Ready),
                Vec::new(),
            )
            .unwrap();
        let replay = journal.replay(thread, 1).unwrap();
        assert_eq!(replay.len(), 1);
        assert!(matches!(replay[0], Response::Event { sequence: 2, .. }));
        assert!(journal.replay(thread, 2).unwrap().is_empty());
    }

    fn config() -> RemoteConversationConfig {
        RemoteConversationConfig {
            harness: "pi".into(),
            session_ref: None,
            model: None,
            effort: None,
            permission_mode: None,
            allow_empty_session: true,
        }
    }
    fn fixture(
        root: &Path,
    ) -> (
        Host,
        Uuid,
        async_channel::Receiver<ConversationCommand>,
        async_channel::Receiver<Outgoing>,
    ) {
        let (thread, generation, epoch) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut journal = Journal::open(&root.join("host.sqlite3")).unwrap();
        journal
            .start(thread, generation, "project", root, &config())
            .unwrap();
        let (commands, input) = async_channel::bounded(32);
        let (_, events) = async_channel::bounded(32);
        let (outgoing, responses) = async_channel::bounded(128);
        let (reconnect, _) = async_channel::bounded(1);
        let (actor_input, _) = mpsc::sync_channel(32);
        let host = Host {
            config: HostConfig {
                relay: RemoteHostConfig {
                    id: "test".into(),
                    name: "test".into(),
                    relay_url: "ws://127.0.0.1:1/remote".into(),
                    token_env: "UNUSED_TEST_TOKEN".into(),
                    projects: BTreeMap::new(),
                },
                data_dir: root.to_path_buf(),
                projects: BTreeMap::from([("project".into(), root.to_path_buf())]),
                executables: BTreeMap::new(),
            },
            journal,
            tools: BrowserToolServer::start_remote().unwrap(),
            outgoing,
            reconnect,
            input: actor_input,
            runtimes: BTreeMap::from([(
                thread,
                Runtime {
                    generation,
                    project: "project".into(),
                    handle: ConversationHandle { commands, events },
                    attached: true,
                    closing: false,
                    peer_cancellations: BTreeMap::new(),
                    state: harness::ConversationState::Ready,
                    ready: None,
                    peer_availability: None,
                    controls: BTreeMap::new(),
                    sent_images: HashSet::new(),
                },
            )]),
            network: BTreeMap::new(),
            epoch: Some(epoch),
            last_seen: Instant::now(),
            desktop: Capabilities::DESKTOP,
            pending_opens: BTreeMap::new(),
            attachment_bytes: 0,
        };
        (host, thread, input, responses)
    }
    fn decoded(body: &Outgoing) -> Response {
        serde_json::from_str(body.body()).unwrap()
    }
    fn request(host: &Host, thread: Uuid, operation: Operation) -> Request {
        Request {
            version: VERSION,
            capabilities: Capabilities::DESKTOP,
            id: Uuid::new_v4(),
            thread,
            epoch: host.epoch.unwrap(),
            operation,
        }
    }

    #[test]
    fn repeated_submission_receipts_do_not_repeat_native_commands() {
        let temp = Temp::new();
        let (mut host, thread, commands, _responses) = fixture(&temp.0);
        let effect = Uuid::new_v4();
        let command = WireCommand::Submit {
            request_id: effect,
            text: "no provider is attached".into(),
            images: Vec::new(),
        };
        let request = request(&host, thread, Operation::Command(command.clone()));
        host.request(request.clone()).unwrap();
        host.request(request).unwrap();
        host.command(Uuid::new_v4(), thread, command).unwrap();
        assert!(
            matches!(commands.try_recv().unwrap(), ConversationCommand::Submit { request_id, .. } if request_id == effect)
        );
        assert!(commands.try_recv().is_err());
        assert!(
            host.command(
                Uuid::new_v4(),
                thread,
                WireCommand::Submit {
                    request_id: Uuid::new_v4(),
                    text: "must not queue".into(),
                    images: Vec::new()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn desktop_loss_keeps_current_runtime_and_journals_its_settlement() {
        let temp = Temp::new();
        let (mut host, thread, commands, responses) = fixture(&temp.0);
        let generation = host.runtimes[&thread].generation;
        let stale = request(&host, thread, Operation::Command(WireCommand::Interrupt));
        host.runtimes.get_mut(&thread).unwrap().state = harness::ConversationState::Running;
        host.disconnect("test desktop loss");
        assert!(!commands.is_closed());
        assert!(
            commands.try_recv().is_err(),
            "disconnect must not send Interrupt or Close"
        );
        assert!(host.request(stale).is_err());
        host.event(
            thread,
            generation,
            ConversationEvent::TurnSettled {
                request_id: Uuid::new_v4(),
                outcome: harness::TurnOutcome::Completed,
            },
        )
        .unwrap();
        host.event(
            thread,
            generation,
            ConversationEvent::StateChanged(harness::ConversationState::Ready),
        )
        .unwrap();
        assert!(responses.try_recv().is_err());
        assert_eq!(host.journal.replay(thread, 0).unwrap().len(), 2);
        host.epoch = Some(Uuid::new_v4());
        host.last_seen = Instant::now();
        host.open(
            Uuid::new_v4(),
            thread,
            "project".into(),
            config(),
            0,
            Some(generation),
            false,
        )
        .unwrap();
        assert!(host.runtimes[&thread].attached);
        assert!(
            commands.try_recv().is_err(),
            "reattach must not replay native commands"
        );
        let recovered: Vec<Response> = std::iter::from_fn(|| responses.try_recv().ok())
            .map(|body| decoded(&body))
            .collect();
        assert!(
            recovered
                .iter()
                .any(|event| matches!(event, Response::Recovered { .. }))
        );
    }

    #[test]
    fn network_roundtrip_requires_current_runtime_and_live_desktop() {
        let temp = Temp::new();
        let (mut host, thread, _, responses) = fixture(&temp.0);
        let generation = host.runtimes[&thread].generation;
        let (reply, response) = mpsc::channel();
        host.tool(BrowserToolRequest {
            thread,
            operation: BrowserToolOperation::Network {
                token: generation,
                action: drostje_network::Action::Members,
            },
            reply,
        });
        let Response::NetworkRequest {
            call,
            generation: actual,
            ..
        } = decoded(&responses.try_recv().unwrap())
        else {
            panic!("expected forwarded network request")
        };
        assert_eq!(generation, actual);
        let message = request(
            &host,
            thread,
            Operation::NetworkResponse {
                call,
                result: Ok(json!({"members":[]})),
            },
        );
        host.request(message).unwrap();
        assert_eq!(response.recv().unwrap().unwrap(), json!({"members":[]}));
        let (reply, response) = mpsc::channel();
        host.tool(BrowserToolRequest {
            thread,
            operation: BrowserToolOperation::Network {
                token: Uuid::new_v4(),
                action: drostje_network::Action::Members,
            },
            reply,
        });
        assert!(response.recv().unwrap().is_err());
        let (reply, response) = mpsc::channel();
        host.tool(BrowserToolRequest {
            thread,
            operation: BrowserToolOperation::Network {
                token: generation,
                action: drostje_network::Action::Members,
            },
            reply,
        });
        host.disconnect("test lost desktop");
        assert!(response.recv().unwrap().is_err());
    }

    #[test]
    fn remote_artifacts_travel_as_html_to_the_attached_desktop_only() {
        let temp = Temp::new();
        let (mut host, thread, _, responses) = fixture(&temp.0);
        let generation = host.runtimes[&thread].generation;
        let (reply, response) = mpsc::channel();
        host.tool(BrowserToolRequest {
            thread,
            operation: BrowserToolOperation::ShowRemoteArtifact {
                name: "scope.html".into(),
                title: Some("Scope".into()),
                html: "<main data-drostje-artifact>Pick</main>".into(),
                placement: drostje_browser_tools::ArtifactPlacement::Composer,
            },
            reply,
        });
        let Response::ArtifactRequest {
            call,
            generation: actual,
            name,
            html,
            placement,
            ..
        } = decoded(&responses.try_recv().unwrap())
        else {
            panic!("expected a forwarded artifact")
        };
        assert_eq!((actual, name.as_str()), (generation, "scope.html"));
        assert_eq!(
            placement,
            drostje_browser_tools::ArtifactPlacement::Composer
        );
        assert!(html.contains("Pick"));
        let message = request(
            &host,
            thread,
            Operation::NetworkResponse {
                call,
                result: Ok(json!({"id":"shown"})),
            },
        );
        host.request(message).unwrap();
        assert_eq!(response.recv().unwrap().unwrap(), json!({"id":"shown"}));

        // Completion is the agent's call, forwarded to the desktop that owns the page.
        let browser = Uuid::new_v4();
        let (reply, response) = mpsc::channel();
        host.tool(BrowserToolRequest {
            thread,
            operation: BrowserToolOperation::CompleteArtifact { id: browser },
            reply,
        });
        let Response::ArtifactComplete {
            call,
            browser: forwarded,
            ..
        } = decoded(&responses.try_recv().unwrap())
        else {
            panic!("expected a forwarded completion")
        };
        assert_eq!(forwarded, browser);
        let message = request(
            &host,
            thread,
            Operation::NetworkResponse {
                call,
                result: Ok(json!({"completed":true})),
            },
        );
        host.request(message).unwrap();
        assert_eq!(response.recv().unwrap().unwrap(), json!({"completed":true}));

        // A desktop with artifacts but without completion is refused, never sent it.
        let mut partial =
            serde_json::to_value(request(&host, thread, Operation::Heartbeat)).unwrap();
        partial["capabilities"] = json!({"artifacts": true});
        host.request(serde_json::from_value(partial).unwrap())
            .unwrap();
        let (reply, response) = mpsc::channel();
        host.tool(BrowserToolRequest {
            thread,
            operation: BrowserToolOperation::CompleteArtifact { id: browser },
            reply,
        });
        assert!(
            response
                .recv()
                .unwrap()
                .unwrap_err()
                .contains("cannot complete artifacts")
        );
        assert!(responses.try_recv().is_err());

        // A desktop from before artifacts omits the capability: nothing is forwarded.
        let mut old = serde_json::to_value(request(&host, thread, Operation::Heartbeat)).unwrap();
        old.as_object_mut().unwrap().remove("capabilities");
        host.request(serde_json::from_value(old).unwrap()).unwrap();
        let (reply, response) = mpsc::channel();
        host.tool(BrowserToolRequest {
            thread,
            operation: BrowserToolOperation::ShowRemoteArtifact {
                name: "old.html".into(),
                title: None,
                html: String::new(),
                placement: drostje_browser_tools::ArtifactPlacement::Composer,
            },
            reply,
        });
        assert!(
            response
                .recv()
                .unwrap()
                .unwrap_err()
                .contains("update DROSTJE")
        );
        assert!(
            responses.try_recv().is_err(),
            "an old desktop never receives ArtifactRequest"
        );
        host.request(request(&host, thread, Operation::Heartbeat))
            .unwrap();

        host.disconnect("test lost desktop");
        let (reply, response) = mpsc::channel();
        host.tool(BrowserToolRequest {
            thread,
            operation: BrowserToolOperation::ShowRemoteArtifact {
                name: "late.html".into(),
                title: None,
                html: String::new(),
                placement: drostje_browser_tools::ArtifactPlacement::Composer,
            },
            reply,
        });
        assert!(
            response
                .recv()
                .unwrap()
                .unwrap_err()
                .contains("disconnected")
        );
    }

    #[test]
    fn disconnect_revokes_queued_peer_admission_without_closing_runtime() {
        let temp = Temp::new();
        let (mut host, thread, commands, _responses) = fixture(&temp.0);
        host.command(
            Uuid::new_v4(),
            thread,
            WireCommand::DeliverPeer {
                attempt: Uuid::new_v4(),
                messages: Vec::new(),
            },
        )
        .unwrap();
        let ConversationCommand::DeliverPeer(peer) = commands.try_recv().unwrap() else {
            panic!("expected peer context")
        };
        assert!(!peer.cancelled.is_closed());
        host.disconnect("test loss");
        assert!(peer.cancelled.is_closed());
        assert!(!commands.is_closed());
    }

    #[test]
    fn reconnect_uses_host_permission_mode_after_permission_auto() {
        let temp = Temp::new();
        let (mut host, thread, _commands, _responses) = fixture(&temp.0);
        let mut authoritative = config();
        authoritative.harness = "codex".into();
        authoritative.permission_mode = Some("auto".into());
        host.journal
            .db
            .execute(
                "UPDATE sessions SET config=?2 WHERE thread=?1",
                params![
                    thread.to_string(),
                    serde_json::to_string(&authoritative).unwrap()
                ],
            )
            .unwrap();
        host.journal
            .record(
                Uuid::new_v4(),
                thread,
                Some(Uuid::new_v4()),
                &ReceiptStatus::Admitted,
            )
            .unwrap();
        let generation = host.runtimes[&thread].generation;
        let mut stale_desktop = authoritative;
        stale_desktop.permission_mode = Some("ask".into());
        host.open(
            Uuid::new_v4(),
            thread,
            "project".into(),
            stale_desktop,
            0,
            Some(generation),
            false,
        )
        .unwrap();
        assert!(host.runtimes[&thread].attached);
        assert_eq!(
            host.journal
                .session(thread)
                .unwrap()
                .unwrap()
                .config
                .permission_mode
                .as_deref(),
            Some("auto")
        );
    }

    fn image() -> WireImage {
        use base64::Engine;
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        WireImage {
            id: Uuid::new_v4(),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            data: base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
        }
    }

    #[test]
    fn sqlite_cursors_reject_unsigned_overflow_and_negative_storage() {
        assert_eq!(sql_cursor(i64::MAX as u64).unwrap(), i64::MAX);
        assert!(sql_cursor(u64::MAX).is_err());
        assert!(wire_cursor(-1).is_err());
        let temp = Temp::new();
        let (host, thread, _, _) = fixture(&temp.0);
        assert!(host.journal.replay(thread, u64::MAX).is_err());
        host.journal
            .db
            .execute(
                "UPDATE sessions SET sequence=-1 WHERE thread=?1",
                [thread.to_string()],
            )
            .unwrap();
        assert!(host.journal.bounds(thread).is_err());
    }

    #[test]
    fn remote_peers_cannot_queue_next_turn_behind_active_turn() {
        let temp = Temp::new();
        let (mut host, thread, commands, _responses) = fixture(&temp.0);
        host.runtimes.get_mut(&thread).unwrap().state = harness::ConversationState::Running;
        assert!(
            host.command(
                Uuid::new_v4(),
                thread,
                WireCommand::DeliverPeer {
                    attempt: Uuid::new_v4(),
                    messages: Vec::new()
                }
            )
            .is_err()
        );
        assert!(
            commands.try_recv().is_err(),
            "No priority-next peer work may reach the native queue"
        );
        host.disconnect("desktop went offline");
        assert!(!commands.is_closed(), "The existing turn must remain alive");
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn native_size_rejection_never_creates_image_files() {
        use base64::Engine;
        let temp = Temp::new();
        let (mut host, thread, commands, _responses) = fixture(&temp.0);
        let mut image = image();
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .unwrap();
        // PNG decoders allow trailing bytes, so this remains a valid small-dimension
        // image while its encoded native request exceeds the 1 MiB boundary.
        bytes.resize(900 * 1024, 0);
        image.data = base64::engine::general_purpose::STANDARD.encode(bytes);
        assert!(
            host.command(
                Uuid::new_v4(),
                thread,
                WireCommand::Submit {
                    request_id: Uuid::new_v4(),
                    text: "too large".into(),
                    images: vec![image]
                }
            )
            .is_err()
        );
        assert_eq!(
            attachment_storage_bytes(&temp.0.join("attachments")).unwrap(),
            0
        );
        assert!(!temp.0.join("attachments").exists());
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn native_queue_rejection_rolls_back_new_images_and_preserves_budget() {
        let temp = Temp::new();
        let (mut host, thread, commands, _responses) = fixture(&temp.0);
        let image = image();
        let path = temp
            .0
            .join("attachments")
            .join(thread.to_string())
            .join(image.id.to_string());
        commands.close();
        host.command(
            Uuid::new_v4(),
            thread,
            WireCommand::Submit {
                request_id: Uuid::new_v4(),
                text: "queue is closed".into(),
                images: vec![image],
            },
        )
        .unwrap();
        assert!(!path.exists());
        assert_eq!(host.attachment_bytes, 0);
        assert_eq!(
            attachment_storage_bytes(&temp.0.join("attachments")).unwrap(),
            0
        );
    }

    #[test]
    fn partial_image_staging_failure_rolls_back_only_new_files() {
        use base64::Engine;
        let temp = Temp::new();
        let first = image();
        let second = image();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&first.data)
            .unwrap()
            .len() as u64;
        let directory = temp.0.join("attachments");
        assert!(
            stage_submission_images(
                Uuid::new_v4(),
                "within native limit",
                &[first.clone(), second],
                &directory,
                bytes
            )
            .is_err()
        );
        assert!(!directory.join(first.id.to_string()).exists());
        assert_eq!(attachment_storage_bytes(&directory).unwrap(), 0);
        materialize_images(
            std::slice::from_ref(&first),
            &directory,
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let preserved = directory.join(first.id.to_string());
        {
            let staged = stage_submission_images(
                Uuid::new_v4(),
                "reusing existing image",
                std::slice::from_ref(&first),
                &directory,
                0,
            )
            .unwrap();
            drop(staged);
        }
        assert!(
            preserved.exists(),
            "Rollback must never delete previously admitted image bytes"
        );
    }

    #[test]
    fn attachment_budget_survives_restart_and_blocks_new_writes() {
        let temp = Temp::new();
        let (mut host, thread, commands, _responses) = fixture(&temp.0);
        host.attachment_bytes = ATTACHMENT_BUDGET;
        assert!(
            host.command(
                Uuid::new_v4(),
                thread,
                WireCommand::Submit {
                    request_id: Uuid::new_v4(),
                    text: "budget is full".into(),
                    images: vec![image()]
                }
            )
            .is_err()
        );
        assert_eq!(
            attachment_storage_bytes(&temp.0.join("attachments")).unwrap(),
            0
        );
        assert!(commands.try_recv().is_err());
        let persisted = temp.0.join("attachments").join("persisted");
        File::create(&persisted)
            .unwrap()
            .set_len(ATTACHMENT_BUDGET)
            .unwrap();
        assert_eq!(
            attachment_storage_bytes(&temp.0.join("attachments")).unwrap(),
            ATTACHMENT_BUDGET
        );
        File::options()
            .write(true)
            .open(persisted)
            .unwrap()
            .set_len(ATTACHMENT_BUDGET + 1)
            .unwrap();
        assert!(attachment_storage_bytes(&temp.0.join("attachments")).is_err());
    }

    #[test]
    fn empty_reconfiguration_retains_owner_until_confirmed_actor_termination() {
        let temp = Temp::new();
        let (mut host, thread, commands, _responses) = fixture(&temp.0);
        let generation = host.runtimes[&thread].generation;
        let mut changed = config();
        changed.effort = Some("high".into());
        host.open(
            Uuid::new_v4(),
            thread,
            "project".into(),
            changed.clone(),
            0,
            Some(generation),
            false,
        )
        .unwrap();
        assert!(matches!(
            commands.try_recv().unwrap(),
            ConversationCommand::Close
        ));
        assert_eq!(host.runtimes.len(), 1);
        assert_eq!(host.runtimes[&thread].generation, generation);
        assert!(host.runtimes[&thread].closing);
        assert!(host.journal.session(thread).unwrap().is_some());
        assert_eq!(host.pending_opens.len(), 1);
        assert!(
            host.open(
                Uuid::new_v4(),
                thread,
                "project".into(),
                changed,
                0,
                Some(generation),
                false
            )
            .is_err()
        );
        assert!(commands.try_recv().is_err());
        host.closed(thread, Uuid::new_v4()).unwrap();
        assert_eq!(
            host.runtimes.len(),
            1,
            "An unrelated generation cannot release this owner"
        );
        host.closed(thread, generation).unwrap();
        // The fixture deliberately has no installed executable: replacement is attempted
        // only after closure, then rejects instead of launching any provider.
        assert!(host.runtimes.is_empty());
        assert!(host.pending_opens.is_empty());
    }

    #[test]
    fn retained_event_boundary_requires_explicit_gap_recovery() {
        let temp = Temp::new();
        let (mut host, thread, commands, responses) = fixture(&temp.0);
        let generation = host.runtimes[&thread].generation;
        // Seed the exact boundary in one commit, then exercise startup accounting and
        // the production append/recovery path without thousands of fsyncs in the test.
        {
            let transaction = host.journal.db.transaction().unwrap();
            for sequence in 1..=2048u64 {
                let response = Response::Event {
                    thread,
                    generation,
                    sequence,
                    event: ConversationEvent::TitleChanged("retained".into()),
                    images: Vec::new(),
                };
                transaction
                    .execute(
                        "INSERT INTO events(thread,sequence,payload) VALUES(?1,?2,?3)",
                        params![
                            thread.to_string(),
                            sql_cursor(sequence).unwrap(),
                            serde_json::to_string(&response).unwrap()
                        ],
                    )
                    .unwrap();
            }
            transaction
                .execute(
                    "UPDATE sessions SET sequence=2048 WHERE thread=?1",
                    [thread.to_string()],
                )
                .unwrap();
            transaction.commit().unwrap();
        }
        host.journal = Journal::open(&temp.0.join("host.sqlite3")).unwrap();
        assert_eq!(host.journal.retained[&thread].count, 2048);
        host.journal
            .append(
                thread,
                generation,
                ConversationEvent::TitleChanged("newest".into()),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(host.journal.bounds(thread).unwrap(), (2, 2049));
        assert_eq!(host.journal.retained[&thread].count, 2048);
        let error = host
            .open(
                Uuid::new_v4(),
                thread,
                "project".into(),
                config(),
                0,
                Some(generation),
                false,
            )
            .unwrap_err();
        assert!(error.to_string().contains("replay gap"), "{error:#}");
        assert!(
            responses.try_recv().is_err(),
            "A gap must not silently publish a partial transcript"
        );
        host.open(
            Uuid::new_v4(),
            thread,
            "project".into(),
            config(),
            0,
            Some(generation),
            true,
        )
        .unwrap();
        assert!(matches!(
            commands.try_recv().unwrap(),
            ConversationCommand::LoadHistory { cursor: None }
        ));
        let replay: Vec<Response> = std::iter::from_fn(|| responses.try_recv().ok())
            .map(|body| decoded(&body))
            .collect();
        assert!(replay.iter().any(|response| matches!(
            response,
            Response::Opened {
                replay_after: 2049,
                ..
            }
        )));
        assert!(replay.iter().any(|response| matches!(
            response,
            Response::Event {
                event: ConversationEvent::HistoryReset,
                ..
            }
        )));
        assert!(
            replay
                .iter()
                .any(|response| matches!(response, Response::Recovered { through: 2051, .. }))
        );
        let (count, bytes): (i64, i64) = host
            .journal
            .db
            .query_row(
                "SELECT COUNT(*),SUM(LENGTH(CAST(payload AS BLOB))) FROM events WHERE thread=?1",
                [thread.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(host.journal.retained[&thread].count, count);
        assert_eq!(host.journal.retained[&thread].bytes, bytes);
        assert!(count <= 2048 && bytes <= EVENT_BUDGET);
    }

    #[test]
    fn failed_event_transaction_does_not_advance_retention_totals() {
        let temp = Temp::new();
        let (mut host, thread, _commands, _responses) = fixture(&temp.0);
        let generation = host.runtimes[&thread].generation;
        host.journal.db.execute_batch("CREATE TRIGGER reject_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected persistence failure'); END;").unwrap();
        assert!(
            host.journal
                .append(
                    thread,
                    generation,
                    ConversationEvent::HistoryReset,
                    Vec::new()
                )
                .is_err()
        );
        assert_eq!(host.journal.retained[&thread].count, 0);
        assert_eq!(host.journal.retained[&thread].bytes, 0);
        assert_eq!(host.journal.bounds(thread).unwrap(), (0, 0));
        host.journal
            .db
            .execute_batch("DROP TRIGGER reject_event;")
            .unwrap();
        host.journal
            .append(
                thread,
                generation,
                ConversationEvent::HistoryReset,
                Vec::new(),
            )
            .unwrap();
        assert_eq!(host.journal.retained[&thread].count, 1);
        assert_eq!(host.journal.bounds(thread).unwrap(), (1, 1));
    }

    fn text(id: &str, text: &str) -> ConversationEvent {
        ConversationEvent::MessageUpdated(harness::Message {
            id: id.into(),
            role: "assistant".into(),
            content: vec![harness::ContentBlock::Text(text.into())],
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        })
    }
    fn tool(id: &str, result: Vec<harness::ContentBlock>) -> ConversationEvent {
        ConversationEvent::ToolUpdated(harness::ToolActivity {
            id: id.into(),
            name: "tool".into(),
            intent: None,
            input: Value::Null,
            result,
            error: false,
            exit_code: None,
            complete: false,
            raw: None,
        })
    }
    /// What a desktop keeps of a transcript: the latest message and tool per ID, and the
    /// order of everything else.
    fn transcript(events: &[ConversationEvent]) -> (BTreeMap<String, String>, Vec<String>) {
        let mut latest = BTreeMap::new();
        let mut others = Vec::new();
        for event in events {
            match event {
                ConversationEvent::MessageUpdated(message) => {
                    latest.insert(
                        format!("message:{}", message.id),
                        format!("{:?}", message.content),
                    );
                }
                ConversationEvent::ToolUpdated(tool) => {
                    latest.insert(format!("tool:{}", tool.id), format!("{:?}", tool.result));
                }
                other => others.push(format!("{other:?}")),
            }
        }
        (latest, others)
    }
    fn journaled(host: &Host, thread: Uuid) -> Vec<ConversationEvent> {
        host.journal
            .replay(thread, 0)
            .unwrap()
            .into_iter()
            .map(|response| match response {
                Response::Event { event, .. } => event,
                other => panic!("unexpected journal row {other:?}"),
            })
            .collect()
    }

    #[test]
    fn queued_updates_of_one_item_are_journaled_once_and_replay_to_the_same_transcript() {
        let temp = Temp::new();
        let (mut host, thread, _commands, responses) = fixture(&temp.0);
        let generation = host.runtimes[&thread].generation;
        let other = Uuid::new_v4();
        let produced = vec![
            text("a", "H"),
            text("a", "He"),
            text("a", "Hel"),
            tool("t", vec![harness::ContentBlock::Text("1".into())]),
            tool("t", vec![harness::ContentBlock::Text("12".into())]),
            text("b", "other message"),
            text("a", "Hello"),
            ConversationEvent::StateChanged(harness::ConversationState::Ready),
            text("a", "Hello!"),
        ];
        let (input, incoming) = mpsc::sync_channel(64);
        for event in produced.iter().cloned() {
            input
                .send(Input::Harness(thread, generation, event))
                .unwrap();
        }
        // Another runtime's update is never merged into this one.
        input
            .send(Input::Harness(other, generation, text("a", "foreign")))
            .unwrap();
        let mut deferred = None;
        while host.step(&incoming, &mut deferred, Duration::ZERO).unwrap() {}
        let stored = journaled(&host, thread);
        assert_eq!(
            stored
                .iter()
                .map(|event| format!("{event:?}"))
                .collect::<Vec<_>>(),
            [
                text("a", "Hel"),
                tool("t", vec![harness::ContentBlock::Text("12".into())]),
                text("b", "other message"),
                text("a", "Hello"),
                ConversationEvent::StateChanged(harness::ConversationState::Ready),
                text("a", "Hello!"),
            ]
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
        );
        assert_eq!(transcript(&stored), transcript(&produced));
        // The attached desktop received exactly the journaled rows, byte for byte.
        let sent: Vec<String> = std::iter::from_fn(|| responses.try_recv().ok())
            .map(|body| body.body().to_owned())
            .collect();
        let rows: Vec<String> = host
            .journal
            .replay_page(thread, 0)
            .unwrap()
            .into_iter()
            .map(|(_, journaled)| journaled.payload)
            .collect();
        assert_eq!(sent, rows);
    }

    #[test]
    fn replay_pages_are_bounded_and_resume_in_sequence() {
        let temp = Temp::new();
        let (mut host, thread, _commands, _responses) = fixture(&temp.0);
        let generation = host.runtimes[&thread].generation;
        let large = "x".repeat(REPLAY_PAGE_BYTES / 3);
        for _ in 0..7 {
            host.journal
                .append(
                    thread,
                    generation,
                    ConversationEvent::TitleChanged(large.clone()),
                    Vec::new(),
                )
                .unwrap();
        }
        let (mut after, mut sequences, mut pages) = (0, Vec::new(), 0);
        loop {
            let page = host.journal.replay_page(thread, after).unwrap();
            let Some((last, _)) = page.last() else { break };
            after = *last;
            pages += 1;
            let bytes: usize = page
                .iter()
                .map(|(_, journaled)| journaled.payload.len())
                .sum();
            assert!(bytes < REPLAY_PAGE_BYTES + large.len() + 1024, "{bytes}");
            sequences.extend(page.into_iter().map(|(sequence, _)| sequence));
        }
        assert_eq!(sequences, (1..=7).collect::<Vec<u64>>());
        assert_eq!(pages, 3);
    }

    #[test]
    fn image_bytes_cross_once_per_open_to_desktops_that_reuse_them() {
        use base64::Engine;
        let temp = Temp::new();
        let (mut host, thread, _commands, responses) = fixture(&temp.0);
        let generation = host.runtimes[&thread].generation;
        let wire = image();
        let path = temp.0.join("screenshot.png");
        fs::write(
            &path,
            base64::engine::general_purpose::STANDARD
                .decode(&wire.data)
                .unwrap(),
        )
        .unwrap();
        let block = harness::ContentBlock::Image(drostje_model::ImageAttachment {
            id: wire.id,
            path,
            mime: wire.mime.clone(),
            width: 1,
            height: 1,
            original: None,
        });
        let images = |responses: &async_channel::Receiver<Outgoing>| -> Vec<usize> {
            std::iter::from_fn(|| responses.try_recv().ok())
                .filter_map(|body| match decoded(&body) {
                    Response::Event { images, .. } => Some(images.len()),
                    _ => None,
                })
                .collect()
        };
        for _ in 0..2 {
            host.event(thread, generation, tool("shot", vec![block.clone()]))
                .unwrap();
        }
        assert_eq!(images(&responses), [1, 0]);
        // The journal stays self-contained: every row carries its image bytes.
        assert!(host.journal.replay(thread, 0).unwrap().iter().all(
            |response| matches!(response, Response::Event { images, .. } if images.len() == 1)
        ));
        // A reattaching desktop gets the bytes again, once: replayed rows, then the state
        // snapshot.
        host.open(
            Uuid::new_v4(),
            thread,
            "project".into(),
            config(),
            0,
            Some(generation),
            false,
        )
        .unwrap();
        assert_eq!(images(&responses), [1, 0, 0]);
        host.event(thread, generation, tool("shot", vec![block.clone()]))
            .unwrap();
        assert_eq!(images(&responses), [0]);
        // A desktop that predates reuse always receives the bytes.
        host.desktop = Capabilities {
            image_reuse: false,
            ..Capabilities::DESKTOP
        };
        host.event(thread, generation, tool("shot", vec![block]))
            .unwrap();
        assert_eq!(images(&responses), [1]);
    }
}
