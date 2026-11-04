mod activity;
mod artifacts;
pub use artifacts::ArtifactReceipt;
mod commit;
mod conversation;
mod factory;
mod remote;
pub use remote::RemoteFileView;
mod network;
pub use network::NetworkState;
mod persistence;
mod project_diff;
mod shutdown;
mod spaces;
mod tracker;
mod worktrees;
use crate::harness::{
    self, CommandCatalog, ContentBlock, ContextUsage, ConversationCommand, ConversationEvent,
    ConversationHandle, ConversationState, Message, ModelChoice, ToolActivity, TurnOutcome,
    UsageLimit, UserAnswer, UserRequest, UserRequestKind,
};
use crate::{model::*, platform, store::Store};
pub use activity::ThreadActivity;
use anyhow::{Context as _, Result};
pub use commit::{CommitSubjectRequest, CommitSubjectStatus};
use gpui_kit::*;
pub use project_diff::{ProjectDiffState, ProjectDiffs};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    path::PathBuf,
};
pub use tracker::{LinkStatus, ReviewState, tracker_error_text};
use uuid::Uuid;
pub use worktrees::{
    RemovalFailure, WorkingTree, WorktreeRequest, WorktreeRequestKind, WorktreeRequestStatus,
};

/// The title of a Factory thread nobody named or configured yet.
pub const DEFAULT_FACTORY_TITLE: &str = "New Factory thread";

/// Content changes are separate from structural workspace invalidation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresentationChange {
    Draft(ThreadId),
    Thread(ThreadId),
    Transcript(ThreadId),
    Diff(WorkingTree),
    /// The review threads of a Conversation's pull request were (re)read.
    Review(ThreadId),
    /// A worktree request changed status; the dialog that opened it reads the new state.
    WorktreeRequest(Uuid),
    /// The commit subject request changed status; the Commit dialog reads the new state.
    CommitSubject(Uuid),
    Persistence,
    Selection,
    Projects,
}

impl EventEmitter<PresentationChange> for AppState {}

pub struct ConversationSession {
    pub generation: Uuid,
    pub state: ConversationState,
    pub commands: Option<async_channel::Sender<ConversationCommand>>,
    finished: Option<async_channel::Receiver<()>>,
    pub persisted: bool,
    pub peer_delivery: bool,
    pub peer_delivery_reason: Option<String>,
    pub models: Vec<ModelChoice>,
    pub model: Option<ModelKey>,
    /// Slash commands the harness offers for prompt text.
    pub slash_commands: CommandCatalog,
    pub messages: Vec<Arc<Message>>,
    /// The harness delivered the last page of provider history since the latest reset, so
    /// `messages` is the whole stored transcript.
    pub history_loaded: bool,
    pub tools: BTreeMap<String, Arc<ToolActivity>>,
    pub requests: BTreeMap<String, UserRequest>,
    pub statuses: BTreeMap<String, String>,
    pub widgets: BTreeMap<String, Vec<String>>,
    pub context_usage: Option<ContextUsage>,
    pub limits: Vec<UsageLimit>,
    pub links: Vec<(String, Option<String>)>,
    pub diagnostics: VecDeque<(String, Value)>,
    pub error: Option<String>,
    pub outcome: Option<TurnOutcome>,
    pub preparing: bool,
    pub transcript_revision: u64,
    transcript_changes: VecDeque<(u64, Option<usize>)>,
    indices: HashMap<String, usize>,
    /// IDs of every `ContentBlock::ToolCall` seen, so their result messages fold
    /// into the call's card instead of rendering as separate rows.
    tool_calls: HashMap<String, usize>,
    tool_results: HashMap<String, usize>,
    /// Every tool call in the order the assistant issued it, as `(tool name, call id)`,
    /// so presentation can find the latest call of one tool without scanning messages.
    tool_call_order: Vec<(String, String)>,
    /// Indices of harness-injected `custom` messages (job notices and the like).
    notice_indices: Vec<usize>,
    submitted: HashMap<Uuid, DraftSnapshot>,
    /// Whether the front of the thread's queue may start the next turn on its own. Set by
    /// an explicit queue or send-now action; cleared by a failure, an interrupt the user
    /// did not request through the queue, and a harness restart (a new session).
    pub auto_dispatch: bool,
    /// The user asked to send a queued message now, so the interrupt that settles the
    /// current turn must not stall the queue.
    interrupt_for_queue: bool,
}
impl ConversationSession {
    fn new(model: Option<ModelKey>) -> Self {
        Self {
            state: ConversationState::Starting,
            commands: None,
            persisted: false,
            peer_delivery: false,
            peer_delivery_reason: None,
            models: Vec::new(),
            model,
            slash_commands: CommandCatalog::Loading,
            generation: Uuid::new_v4(),
            finished: None,
            messages: Vec::new(),
            history_loaded: false,
            tools: BTreeMap::new(),
            requests: BTreeMap::new(),
            statuses: BTreeMap::new(),
            widgets: BTreeMap::new(),
            context_usage: None,
            limits: Vec::new(),
            links: Vec::new(),
            diagnostics: VecDeque::new(),
            error: None,
            outcome: None,
            preparing: false,
            transcript_revision: 0,
            transcript_changes: VecDeque::new(),
            indices: HashMap::new(),
            tool_calls: HashMap::new(),
            tool_results: HashMap::new(),
            tool_call_order: Vec::new(),
            notice_indices: Vec::new(),
            submitted: HashMap::new(),
            auto_dispatch: false,
            interrupt_for_queue: false,
        }
    }
    fn message(&mut self, message: Message) {
        let index = self
            .indices
            .get(&message.id)
            .copied()
            .unwrap_or(self.messages.len());
        for block in &message.content {
            if let ContentBlock::ToolCall { id, name, .. } = block {
                if !self.tool_calls.contains_key(id) {
                    self.tool_call_order.push((name.clone(), id.clone()));
                }
                self.tool_calls.insert(id.clone(), index);
                if let Some(result_index) = self.tool_results.get(id).copied() {
                    self.record_transcript_change(Some(result_index));
                }
            }
        }
        if let Some(call_id) = &message.tool_call_id {
            self.tool_results.insert(call_id.clone(), index);
            if let Some(call_index) = self.tool_calls.get(call_id).copied() {
                self.record_transcript_change(Some(call_index));
            }
        }
        if index == self.messages.len() {
            self.indices.insert(message.id.clone(), index);
            if message.role == "custom" {
                self.notice_indices.push(index);
            }
            self.messages.push(Arc::new(message));
        } else {
            self.messages[index] = Arc::new(message);
        }
        self.record_transcript_change(Some(index));
    }

    fn record_transcript_change(&mut self, index: Option<usize>) {
        self.transcript_revision += 1;
        if self.transcript_changes.len() == 512 {
            self.transcript_changes.pop_front();
        }
        self.transcript_changes
            .push_back((self.transcript_revision, index));
    }

    /// A lagging presentation resets once when its bounded change journal expires.
    pub fn transcript_changes_since(
        &self,
        revision: u64,
    ) -> Option<impl Iterator<Item = usize> + '_> {
        if self
            .transcript_changes
            .front()
            .is_some_and(|(first, _)| revision < first.saturating_sub(1))
            || self
                .transcript_changes
                .iter()
                .any(|(next, index)| *next > revision && index.is_none())
        {
            return None;
        }
        Some(
            self.transcript_changes
                .iter()
                .filter_map(move |(next, index)| (*next > revision).then_some(*index).flatten()),
        )
    }

    fn reset_history(&mut self) {
        self.history_loaded = false;
        self.messages.clear();
        self.indices.clear();
        self.tool_calls.clear();
        self.tool_results.clear();
        self.tool_call_order.clear();
        self.notice_indices.clear();
        self.record_transcript_change(None);
    }
    pub(super) fn mark_tool_changed(&mut self, call_id: &str) {
        self.record_transcript_change(self.tool_calls.get(call_id).copied());
    }

    /// True once the assistant message that issued this call has been seen.
    pub fn has_tool_call(&self, call_id: &str) -> bool {
        self.tool_calls.contains_key(call_id)
    }
    /// The historical result message answering a tool call, if any.
    pub fn tool_result(&self, call_id: &str) -> Option<&Arc<Message>> {
        self.tool_results
            .get(call_id)
            .and_then(|index| self.messages.get(*index))
    }
    /// Tool calls in issue order as `(tool name, call id)`.
    pub fn tool_call_order(&self) -> &[(String, String)] {
        &self.tool_call_order
    }
    /// The `ContentBlock::ToolCall` for a call id, once its message has been seen.
    pub fn tool_call_block(&self, call_id: &str) -> Option<&ContentBlock> {
        let message = self.messages.get(*self.tool_calls.get(call_id)?)?;
        message
            .content
            .iter()
            .find(|block| matches!(block, ContentBlock::ToolCall { id, .. } if id == call_id))
    }
    /// Harness-injected `custom` messages in transcript order.
    pub fn notices(&self) -> impl Iterator<Item = &Arc<Message>> {
        self.notice_indices
            .iter()
            .filter_map(|index| self.messages.get(*index))
    }
}

pub struct Bootstrap {
    pub data_dir: PathBuf,
    pub store: Store,
    pub snapshot: WorkspaceSnapshot,
    pub diagnostics: BTreeMap<String, String>,
    pub repositories: BTreeMap<ProjectId, Result<platform::RepositoryInfo, String>>,
    pub browser_tools: Option<crate::browser_tools::BrowserToolServer>,
}

impl Bootstrap {
    /// Runs before GPUI starts; subsequent filesystem, Git and database work is asynchronous.
    pub fn load() -> Result<Self> {
        let data_dir = platform::data_dir()?;
        let store = Store::open(data_dir.clone())?;
        let mut snapshot = smol::block_on(store.snapshot())?;
        if !snapshot.settings.contains_key("executables") {
            let value = json!({
                "nvim": "", "omp": "", "pi": "", "claude": "", "codex": "", "opencode": "",
                "llama": "", "factory": ""
            });
            smol::block_on(store.set_setting("executables".into(), value.clone()))?;
            snapshot.settings.insert("executables".into(), value);
        }
        if !snapshot.settings.contains_key("factory_data_root") {
            let root = platform::factory_data_root()?;
            let value = json!(root);
            smol::block_on(store.set_setting("factory_data_root".into(), value.clone()))?;
            snapshot.settings.insert("factory_data_root".into(), value);
        }
        let mut diagnostics = executable_diagnostics(&snapshot.settings);
        let browser_tools = match crate::browser_tools::BrowserToolServer::start() {
            Ok(server) => Some(server),
            Err(error) => {
                diagnostics.insert("browser tools".into(), format!("Unavailable: {error:#}"));
                None
            }
        };
        let repositories = snapshot
            .projects
            .iter()
            .map(|p| {
                (
                    p.id,
                    platform::inspect_repository(&p.root).map_err(|e| format!("{e:#}")),
                )
            })
            .collect();
        Ok(Self {
            data_dir,
            store,
            snapshot,
            diagnostics,
            repositories,
            browser_tools,
        })
    }
}

fn executable_diagnostics(settings: &BTreeMap<String, Value>) -> BTreeMap<String, String> {
    [
        "nvim", "omp", "pi", "claude", "codex", "opencode", "llama", "factory", "azdo",
    ]
    .into_iter()
    .map(|name| {
        let explicit = settings
            .get("executables")
            .and_then(|v| v.get(name))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let diagnostic = match platform::resolve_executable(name, explicit) {
            Ok(path) => format!("Resolved: {}", path.display()),
            Err(error) => format!("Unavailable: {error:#}"),
        };
        (name.into(), diagnostic)
    })
    .collect()
}

enum Write {
    Project(Project),
    Thread(
        Box<Thread>,
        Option<async_channel::Sender<Result<(), String>>>,
    ),
    Setting(String, Value),
    Worktree(Worktree),
    Flush(async_channel::Sender<Result<(), String>>),
}

/// A tab of a thread's sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTab {
    Changes,
    Output,
    Terminal,
    /// Space network membership and the Conversation's local agent activity.
    Agents,
    Browser,
    /// Review threads of the pull request pinned to a worktree Conversation.
    Review,
    /// Inspector for the selected thread or tool call.
    Details,
}
impl SidebarTab {
    fn from_saved(conversation: bool, saved: Option<&str>) -> Self {
        match saved {
            Some("terminal") => Self::Terminal,
            Some("output") => Self::Output,
            _ if !conversation => Self::Output,
            Some("agents") => Self::Agents,
            Some("browser") => Self::Browser,
            Some("review") => Self::Review,
            Some("details") => Self::Details,
            _ => Self::Changes,
        }
    }

    /// The sidebar `(open, tab)` after asking for `requested`: a hidden sidebar or
    /// another tab switches to it; the tab already on screen hides the sidebar.
    pub fn toggle(open: bool, current: Self, requested: Self) -> (bool, Self) {
        if open && current == requested {
            (false, current)
        } else {
            (true, requested)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Diagnostic,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct WorkspaceNotice {
    pub level: NoticeLevel,
    pub text: String,
}

/// Events arrive on one bounded channel; backend integrations can extend this owner
/// without putting process work or provider state into panel render functions.
pub enum AppEvent {
    WriteFinished(Result<(), String>),
    RepositoryChecked(ProjectId, Result<platform::RepositoryInfo, String>),
    ProjectResolved(SpaceId, Result<platform::RepositoryInfo, String>),
    Diagnostics(BTreeMap<String, String>),
    TitleModels(Uuid, crate::titles::InstalledModels),
    PiInstalled(Result<PathBuf, String>),
    StorageFailed(String),
    InteractiveActivity {
        thread_id: ThreadId,
        active: bool,
    },
    FactorySnapshot(Box<crate::factory::FactorySnapshot>),
    HarnessOpened(ThreadId, Uuid, Result<ConversationHandle, String>),
    Harness(ThreadId, Uuid, ConversationEvent),
    Notice(WorkspaceNotice),
    HarnessDrained(async_channel::Sender<()>),
    WorktreeCreated(Uuid, Result<Box<Worktree>, String>),
    WorktreeRenamed(WorktreeId, Result<String, String>),
    WorktreeInspected(WorktreeId, Result<platform::WorktreeStatus, String>),
    /// `Ok(true)` when the branch was deleted along with the directory.
    WorktreeRemoved(Option<Uuid>, WorktreeId, Result<bool, RemovalFailure>),
    /// The live HEAD of a worktree; `Ok(None)` is detached.
    WorktreeHeadRead(WorktreeId, Result<Option<String>, String>),
    BranchSwitched(Uuid, WorkingTree, Result<String, String>),
    /// A commit finished; `Ok` carries the new commit's short hash and subject.
    Committed(Uuid, WorkingTree, Result<(String, String), String>),
}

pub struct AppState {
    pub data_dir: PathBuf,
    pub store: Store,
    pub snapshot: WorkspaceSnapshot,
    pub spaces: Vec<Space>,
    pub active_space: SpaceId,
    space_state: spaces::Spaces,
    pub selected_project: Option<ProjectId>,
    pub selected_thread: Option<ThreadId>,
    pub open_threads: Vec<ThreadId>,
    pub selected_launch: Option<LaunchId>,
    pub show_archived: bool,
    pub collapsed: HashSet<ProjectId>,
    pub repositories: BTreeMap<ProjectId, Result<platform::RepositoryInfo, String>>,
    pub diagnostics: BTreeMap<String, String>,
    pub browser_tools: Option<crate::browser_tools::BrowserToolServer>,
    pub network: NetworkState,
    artifacts: artifacts::ArtifactChannels,
    pub remote_client: crate::remote::Client,
    pub remote_file: Option<RemoteFileView>,
    /// Models the title providers have locally; `None` until first inspected.
    pub title_models: Option<crate::titles::InstalledModels>,
    /// The inspection in flight; results of earlier ones are dropped.
    title_models_request: Option<Uuid>,
    pub notices: VecDeque<WorkspaceNotice>,
    pub notice_sequence: u64,
    pub storage_error: Option<String>,
    pub pending_writes: usize,
    /// Changed in memory; no persistence work is queued until a commit boundary.
    edited_threads: HashSet<ThreadId>,
    pub installing_pi: bool,
    pub registering: bool,
    pub active_threads: HashSet<ThreadId>,
    unseen_turns: activity::UnseenTurns,
    pub owned_launches: HashSet<LaunchId>,
    pub events: async_channel::Sender<AppEvent>,
    pub shutting_down: bool,
    pub shutdown_complete: bool,
    pub shutdown_error: Option<String>,
    pub conversations: BTreeMap<ThreadId, ConversationSession>,
    pub project_diffs: ProjectDiffs,
    pub pull_requests: tracker::PullRequests,
    pub drafts: HashMap<DraftKey, DraftSnapshot>,
    pub editor: Option<crate::neovim::NvimEditor>,
    pub editor_error: Option<String>,
    pub factory: Option<crate::factory::FactoryManager>,
    pub factory_runs: BTreeMap<LaunchId, crate::factory::FactorySnapshot>,
    pub factory_text: Option<Result<crate::factory::TextPage, String>>,
    pub factory_text_path: Option<PathBuf>,
    pub factory_text_generation: Uuid,
    pub pending_factory: HashSet<ThreadId>,
    /// Conversations with a title request in flight; at most one each.
    titling: HashSet<ThreadId>,
    /// The worktree creation or removal in flight, kept until the next one starts so the
    /// dialog can show how it ended.
    pub worktree_request: Option<WorktreeRequest>,
    /// The commit subject the local model is writing, or how that ended; one at a time.
    pub commit_subject: Option<commit::CommitSubjectRequest>,
    /// Live HEAD per worktree as last read from Git; absent means not read yet.
    pub worktree_heads: HashMap<WorktreeId, Option<String>>,
    renaming_worktrees: HashSet<WorktreeId>,
    inspecting_worktrees: HashSet<WorktreeId>,
    writes: async_channel::Sender<Write>,
}

impl AppState {
    pub fn new(bootstrap: Bootstrap, cx: &mut Context<Self>) -> Self {
        let Bootstrap {
            data_dir,
            store,
            snapshot,
            diagnostics,
            repositories,
            browser_tools,
        } = bootstrap;
        let (space_state, storage_error) = match spaces::Spaces::load(&snapshot) {
            Ok(spaces) => (spaces, None),
            Err(error) => (
                spaces::Spaces::recovery(&snapshot),
                Some(format!(
                    "Spaces are read-only: {error:#}. Restore the saved spaces metadata before making changes."
                )),
            ),
        };
        let session = space_state.active_session();
        let selected_project = session.selected_project;
        let selected_thread = session.selected_thread;
        let open_threads = session.open_threads;
        let show_archived = session.show_archived;
        let selected_launch = selected_thread.and_then(|id| {
            snapshot
                .launches
                .iter()
                .find(|launch| launch.factory_thread_id == id)
                .map(|launch| launch.id)
        });
        let (events, receiver) = async_channel::bounded(256);
        let (writes, requests) = async_channel::unbounded();
        let completed = events.clone();
        let failures = store.failures.clone();
        let durable_store = store.clone();
        let mut notices = VecDeque::new();
        if let Some(error) = &storage_error {
            notices.push_back(WorkspaceNotice {
                level: NoticeLevel::Error,
                text: error.clone(),
            });
        }
        let factory = match crate::factory::FactoryManager::open(
            durable_store.clone(),
            data_dir.clone(),
            snapshot.launches.clone(),
        ) {
            Ok(manager) => {
                let receiver = manager.events.clone();
                let sender = events.clone();
                smol::spawn(async move {
                    while let Ok(snapshot) = receiver.recv().await {
                        if sender
                            .send(AppEvent::FactorySnapshot(Box::new(snapshot)))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
                Some(manager)
            }
            Err(error) => {
                notices.push_back(WorkspaceNotice {
                    level: NoticeLevel::Error,
                    text: format!("Factory tracking is unavailable: {error:#}"),
                });
                None
            }
        };
        smol::spawn(persistence::run(store, requests, completed)).detach();
        let failed = events.clone();
        smol::spawn(async move {
            while let Ok(message) = failures.recv().await {
                if failed.send(AppEvent::StorageFailed(message)).await.is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let mut history: Option<(ThreadId, Uuid, std::vec::IntoIter<Message>, Option<String>)> =
                None;
            loop {
                let first = if history.is_none() {
                    match receiver.recv().await {
                        Ok(event) => Some(event),
                        Err(_) => break,
                    }
                } else {
                    None
                };
                if this
                    .update(cx, |this, cx| {
                        let started = std::time::Instant::now();
                        let mut first = first;
                        for _ in 0..64 {
                            let event = if let Some((id, generation, messages, cursor)) =
                                history.as_mut()
                            {
                                let chunk = messages.by_ref().take(8).collect();
                                let finished = messages.len() == 0;
                                let event = AppEvent::Harness(
                                    *id,
                                    *generation,
                                    ConversationEvent::History {
                                        messages: chunk,
                                        next_cursor: if finished { cursor.take() } else { None },
                                    },
                                );
                                if finished {
                                    history = None;
                                }
                                event
                            } else {
                                let Some(event) = first.take().or_else(|| receiver.try_recv().ok())
                                else {
                                    break;
                                };
                                if let AppEvent::Harness(
                                    id,
                                    generation,
                                    ConversationEvent::History {
                                        messages,
                                        next_cursor,
                                    },
                                ) = event
                                {
                                    history =
                                        Some((id, generation, messages.into_iter(), next_cursor));
                                    continue;
                                }
                                event
                            };
                            this.receive(event, cx);
                            if started.elapsed() >= std::time::Duration::from_millis(2) {
                                break;
                            }
                        }
                    })
                    .is_err()
                {
                    break;
                }
                // A timer crosses the executor turn; yield_now can immediately
                // repoll this task while native input is still waiting.
                smol::Timer::after(std::time::Duration::from_millis(1)).await;
            }
        })
        .detach();
        let quit_state = cx.entity();
        App::on_app_quit(cx, move |cx| {
            let state = quit_state.clone();
            let shutdown = state.update(cx, |state, _| {
                if state.shutdown_complete {
                    return None;
                }
                state.shutting_down = true;
                Some(shutdown::Shutdown::new(state))
            });
            async move {
                // Last-resort best effort for platform termination that bypasses
                // the DROSTJE. quit action. Normal quit has already completed cleanup.
                if let Some(shutdown) = shutdown {
                    if let Err(error) = shutdown.run(true).await {
                        eprintln!("Forced platform shutdown: {error:#}");
                    }
                }
            }
        })
        .detach();
        let mut state = Self {
            data_dir,
            store: durable_store,
            snapshot,
            spaces: space_state.spaces.clone(),
            active_space: space_state.active,
            space_state,
            selected_project,
            selected_thread,
            open_threads,
            selected_launch,
            show_archived,
            collapsed: session.collapsed,
            repositories,
            diagnostics,
            browser_tools,
            network: NetworkState::default(),
            artifacts: Default::default(),
            remote_client: crate::remote::Client::new(),
            remote_file: None,
            title_models: None,
            title_models_request: None,
            notice_sequence: notices.len() as u64,
            notices,
            installing_pi: false,
            storage_error,
            pending_writes: 0,
            edited_threads: HashSet::new(),
            registering: false,
            conversations: BTreeMap::new(),
            project_diffs: ProjectDiffs::default(),
            pull_requests: tracker::PullRequests::default(),
            drafts: HashMap::new(),
            editor: None,
            editor_error: None,
            shutting_down: false,
            shutdown_complete: false,
            shutdown_error: None,
            factory,
            factory_runs: BTreeMap::new(),
            factory_text: None,
            factory_text_path: None,
            factory_text_generation: Uuid::new_v4(),
            pending_factory: HashSet::new(),
            titling: HashSet::new(),
            worktree_request: None,
            commit_subject: None,
            worktree_heads: HashMap::new(),
            renaming_worktrees: HashSet::new(),
            inspecting_worktrees: HashSet::new(),
            active_threads: HashSet::new(),
            unseen_turns: activity::UnseenTurns::default(),
            owned_launches: HashSet::new(),
            events,
            writes,
        };
        state.persist_space_session();
        state.reconcile_worktrees();
        state.start_pull_request_polling(cx);
        state.start_network(cx);
        state
    }

    fn receive(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        match event {
            AppEvent::WriteFinished(result) => {
                self.pending_writes = self.pending_writes.saturating_sub(1);
                if let Err(error) = result {
                    self.storage_error = Some(error.clone());
                    self.pause_all_network_delivery();
                    self.sync_network();
                    self.notice(NoticeLevel::Error, error);
                    cx.notify();
                } else {
                    cx.emit(PresentationChange::Persistence);
                }
                return;
            }
            AppEvent::StorageFailed(error) => {
                self.storage_error = Some(error.clone());
                self.pause_all_network_delivery();
                self.sync_network();
                self.notice(NoticeLevel::Error, error);
            }
            AppEvent::RepositoryChecked(id, info) => {
                self.repositories.insert(id, info);
            }
            AppEvent::ProjectResolved(space, result) => {
                self.registering = false;
                match result {
                    Ok(info) => {
                        if let Some((id, archived)) = self
                            .snapshot
                            .projects
                            .iter()
                            .find(|p| p.root == info.root)
                            .map(|p| (p.id, p.archived))
                        {
                            self.repositories.insert(id, Ok(info));
                            if self.space_mutation_reason().is_none() {
                                let (destination, added) =
                                    self.register_project_membership(id, space);
                                let name =
                                    self.space_name(destination).unwrap_or("Default").to_owned();
                                if destination == self.active_space {
                                    self.select_project(id);
                                    if archived {
                                        self.set_show_archived(true);
                                    }
                                    self.notice(
                                        NoticeLevel::Info,
                                        if added {
                                            format!("Project added to space {name} and selected.")
                                        } else {
                                            "This working tree is already in this space; selected the existing project.".into()
                                        },
                                    );
                                } else {
                                    self.notice(
                                        NoticeLevel::Info,
                                        if added {
                                            format!("Project added to space {name}. Switch to that space to open it.")
                                        } else {
                                            format!("This working tree is already in space {name}. Switch to that space to open it.")
                                        },
                                    );
                                }
                            }
                        } else if self.space_mutation_reason().is_none() {
                            let project = Project {
                                id: Uuid::new_v4(),
                                name: info
                                    .root
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "Project".into()),
                                root: info.root.clone(),
                                archived: false,
                                defaults: ProjectDefaults::default(),
                            };
                            let id = project.id;
                            self.repositories.insert(project.id, Ok(info));
                            let destination = self.save_project_in_space(project, space);
                            if destination == self.active_space {
                                self.select_project(id);
                            } else {
                                let name =
                                    self.space_name(destination).unwrap_or("Default").to_owned();
                                self.notice(NoticeLevel::Info, format!("Project registered in space {name}. Switch to that space to open it."));
                            }
                        }
                    }
                    Err(error) => self.notice(NoticeLevel::Error, error),
                }
            }
            AppEvent::Diagnostics(diagnostics) => self.diagnostics = diagnostics,
            AppEvent::TitleModels(request, models) => {
                if self.title_models_request == Some(request) {
                    self.title_models_request = None;
                    self.title_models = Some(models);
                }
            }
            AppEvent::PiInstalled(result) => {
                self.installing_pi = false;
                match result {
                    Ok(path) => {
                        let mut executables = self
                            .snapshot
                            .settings
                            .get("executables")
                            .and_then(Value::as_object)
                            .cloned()
                            .unwrap_or_default();
                        executables.insert(
                            "pi".into(),
                            Value::String(path.to_string_lossy().into_owned()),
                        );
                        self.set_setting("executables", Value::Object(executables));
                        self.notice(
                            NoticeLevel::Info,
                            format!("Pi installed at {}", path.display()),
                        );
                    }
                    Err(error) => self.notice(
                        NoticeLevel::Error,
                        format!("Pi installation failed: {error}"),
                    ),
                }
                self.refresh_diagnostics();
            }
            AppEvent::InteractiveActivity { thread_id, active } => {
                if active && self.is_conversation(thread_id) {
                    self.active_threads.insert(thread_id);
                } else {
                    self.active_threads.remove(&thread_id);
                    // A pull request that closed during the turn archives now.
                    if self.settle_closed_pull_requests() {
                        cx.emit(PresentationChange::Projects);
                        cx.notify();
                    }
                }
                cx.emit(PresentationChange::Thread(thread_id));
                return;
            }
            AppEvent::FactorySnapshot(snapshot) => self.receive_factory(*snapshot),
            AppEvent::Notice(notice) => self.notice(notice.level, notice.text),
            AppEvent::HarnessOpened(id, generation, result) => {
                if self.shutting_down || !self.is_conversation(id) {
                    if let Ok(handle) = &result {
                        let _ = handle.commands.try_send(ConversationCommand::Close);
                    }
                    return;
                }
                match (
                    self.conversations
                        .get_mut(&id)
                        .filter(|s| s.generation == generation),
                    result,
                ) {
                    (Some(session), Ok(handle)) => {
                        session.commands = Some(handle.commands);
                    }
                    (Some(session), Err(error)) => {
                        session.state = ConversationState::Failed;
                        session.error = Some(error);
                    }
                    (None, Ok(handle)) => {
                        let _ = handle.commands.try_send(ConversationCommand::Close);
                    }
                    (None, Err(_)) => {}
                }
                self.sync_network();
                cx.emit(PresentationChange::Projects);
                cx.emit(PresentationChange::Thread(id));
                return;
            }
            AppEvent::Harness(id, generation, event) => {
                if self
                    .conversations
                    .get(&id)
                    .is_some_and(|s| s.generation == generation)
                {
                    let project_rows_changed = matches!(
                        &event,
                        ConversationEvent::Ready { .. }
                            | ConversationEvent::StateChanged(_)
                            | ConversationEvent::TitleChanged(_)
                            | ConversationEvent::UserRequest(_)
                            | ConversationEvent::UserRequestDismissed { .. }
                            | ConversationEvent::SubmissionAccepted { .. }
                            | ConversationEvent::TurnSettled { .. }
                            | ConversationEvent::Failed { .. }
                            | ConversationEvent::PeerDeliveryAvailable { .. }
                            | ConversationEvent::PeerDelivery { .. }
                            | ConversationEvent::PeerTurnStarted { .. }
                    );
                    let was_running = self.active_threads.contains(&id);
                    self.receive_conversation(id, event, cx);
                    if was_running && !self.active_threads.contains(&id) {
                        self.turn_ended(id);
                    }
                    if project_rows_changed {
                        cx.emit(PresentationChange::Projects);
                    }
                }
                return;
            }
            AppEvent::HarnessDrained(reply) => {
                let _ = reply.try_send(());
                return;
            }
            AppEvent::WorktreeCreated(request, result) => {
                self.worktree_created(request, result, cx)
            }
            AppEvent::WorktreeRenamed(id, result) => self.worktree_renamed(id, result, cx),
            AppEvent::WorktreeInspected(id, result) => self.worktree_inspected(id, result, cx),
            AppEvent::WorktreeRemoved(request, id, result) => {
                self.worktree_removed(request, id, result, cx)
            }
            AppEvent::WorktreeHeadRead(id, result) => {
                if let Ok(head) = result {
                    self.record_head(WorkingTree::Worktree(id), head);
                }
                cx.emit(PresentationChange::Projects);
                for thread in self.worktree_threads(id) {
                    cx.emit(PresentationChange::Thread(thread));
                }
            }
            AppEvent::BranchSwitched(request, tree, result) => {
                self.branch_switched(request, tree, result, cx)
            }
            AppEvent::Committed(request, tree, result) => self.committed(request, tree, result, cx),
        }
        cx.notify();
    }

    pub fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notice_sequence = self.notice_sequence.wrapping_add(1);
        if self.notices.len() == 200 {
            self.notices.pop_front();
        }
        self.notices.push_back(WorkspaceNotice {
            level,
            text: text.into(),
        });
    }

    fn enqueue(&mut self, request: Write) -> bool {
        if self.storage_error.is_some() {
            return false;
        }
        match self.writes.try_send(request) {
            Ok(()) => {
                self.pending_writes += 1;
                true
            }
            Err(error) => {
                self.storage_error = Some(error.to_string());
                self.pause_all_network_delivery();
                self.sync_network();
                false
            }
        }
    }

    pub fn set_setting(&mut self, key: &str, value: Value) {
        self.snapshot.settings.insert(key.into(), value.clone());
        self.enqueue(Write::Setting(key.into(), value));
    }

    pub fn save_project(&mut self, project: Project) {
        self.save_project_in_space(project, self.active_space);
    }

    fn save_project_in_space(&mut self, project: Project, space: SpaceId) -> SpaceId {
        let id = project.id;
        let is_new = !self.snapshot.projects.iter().any(|old| old.id == id);
        if is_new && self.space_mutation_reason().is_some() {
            return space;
        }
        if let Some(old) = self
            .snapshot
            .projects
            .iter_mut()
            .find(|p| p.id == project.id)
        {
            *old = project.clone();
        } else {
            self.snapshot.projects.push(project.clone());
        }
        self.enqueue(Write::Project(project));
        self.sync_network();
        if is_new {
            self.register_project_membership(id, space).0
        } else {
            space
        }
    }

    pub fn save_thread(&mut self, thread: Thread) {
        if self.storage_error.is_none() {
            self.edited_threads.remove(&thread.id);
        }
        if let Some(old) = self.snapshot.threads.iter_mut().find(|t| t.id == thread.id) {
            *old = thread.clone();
        } else {
            self.snapshot.threads.push(thread.clone());
        }
        self.enqueue(Write::Thread(Box::new(thread), None));
        self.sync_network();
    }

    /// Typing changes memory only: no timer, write request, or save notification.
    pub fn edit_thread(&mut self, thread: Thread) {
        let id = thread.id;
        if let Some(old) = self.snapshot.threads.iter_mut().find(|t| t.id == id) {
            *old = thread;
        } else {
            self.snapshot.threads.push(thread);
        }
        self.edited_threads.insert(id);
    }

    pub fn commit_thread_edits(&mut self, id: ThreadId) -> bool {
        if self.storage_error.is_some() || !self.edited_threads.remove(&id) {
            return false;
        }
        let Some(thread) = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .cloned()
        else {
            return false;
        };
        let queued = self.enqueue(Write::Thread(Box::new(thread), None));
        if !queued {
            self.edited_threads.insert(id);
        }
        queued
    }

    fn commit_all_thread_edits(&mut self) {
        let ids: Vec<_> = self.edited_threads.iter().copied().collect();
        for id in ids {
            self.commit_thread_edits(id);
        }
    }

    /// A persistence acknowledgement in the same queue as draft edits and layout changes.
    /// Callers must await success before sending any corresponding external side effect.
    pub fn persist_thread(
        &mut self,
        thread: Thread,
    ) -> async_channel::Receiver<Result<(), String>> {
        let (reply, result) = async_channel::bounded(1);
        if let Some(old) = self.snapshot.threads.iter_mut().find(|t| t.id == thread.id) {
            *old = thread.clone();
        } else {
            self.snapshot.threads.push(thread.clone());
        }
        if let Some(error) = &self.storage_error {
            let _ = reply.try_send(Err(error.clone()));
        } else {
            self.edited_threads.remove(&thread.id);
            self.commit_all_thread_edits();
            self.enqueue(Write::Thread(Box::new(thread), Some(reply)));
        }
        result
    }

    pub fn project(&self) -> Option<&Project> {
        self.selected_project
            .filter(|id| self.project_in_active_space(*id))
            .and_then(|id| self.snapshot.projects.iter().find(|p| p.id == id))
    }
    pub fn thread(&self) -> Option<&Thread> {
        self.selected_thread
            .filter(|id| self.thread_in_active_space(*id))
            .and_then(|id| self.snapshot.threads.iter().find(|t| t.id == id))
    }
    pub fn conversation_thread(&self) -> Option<&Thread> {
        self.thread()
            .filter(|thread| thread.kind == ThreadKind::Conversation)
    }
    pub fn factory_thread(&self) -> Option<&Thread> {
        self.thread()
            .filter(|thread| thread.kind == ThreadKind::Factory)
    }
    /// Whether a thread's sidebar (Changes, Output) is shown. The settings key predates
    /// the Output tab and is kept so existing choices survive.
    pub fn thread_sidebar_open(&self, id: ThreadId) -> bool {
        let id = id.to_string();
        self.snapshot
            .settings
            .get("conversation_diff_open")
            .and_then(Value::as_object)
            .and_then(|threads| threads.get(&id))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// The sidebar tab a thread shows. Factory threads have no Changes tab.
    pub fn thread_sidebar_tab(&self, id: ThreadId) -> SidebarTab {
        SidebarTab::from_saved(
            self.is_conversation(id),
            self.snapshot
                .settings
                .get("thread_sidebar_tab")
                .and_then(Value::as_object)
                .and_then(|threads| threads.get(&id.to_string()))
                .and_then(Value::as_str),
        )
    }

    /// Show `tab` in the thread's sidebar, or hide the sidebar when `tab` is already shown.
    pub fn toggle_thread_sidebar(&mut self, id: ThreadId, tab: SidebarTab) {
        let (open, tab) = SidebarTab::toggle(
            self.thread_sidebar_open(id),
            self.thread_sidebar_tab(id),
            tab,
        );
        self.set_thread_sidebar(id, open, tab);
    }

    /// Open the thread's sidebar on `tab`.
    pub fn show_thread_sidebar(&mut self, id: ThreadId, tab: SidebarTab) {
        self.set_thread_sidebar(id, true, tab);
    }

    fn set_thread_sidebar(&mut self, id: ThreadId, open: bool, tab: SidebarTab) {
        let key = id.to_string();
        let mut threads = self
            .snapshot
            .settings
            .get("conversation_diff_open")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if open {
            threads.remove(&key);
        } else {
            threads.insert(key.clone(), Value::Bool(false));
        }
        if self.snapshot.settings.get("conversation_diff_open")
            != Some(&Value::Object(threads.clone()))
        {
            self.set_setting("conversation_diff_open", Value::Object(threads));
        }
        if self.thread_sidebar_tab(id) != tab {
            let mut tabs = self
                .snapshot
                .settings
                .get("thread_sidebar_tab")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            match tab {
                SidebarTab::Changes => tabs.remove(&key),
                SidebarTab::Output => tabs.insert(key, Value::String("output".into())),
                SidebarTab::Terminal => tabs.insert(key, Value::String("terminal".into())),
                SidebarTab::Agents => tabs.insert(key, Value::String("agents".into())),
                SidebarTab::Browser => tabs.insert(key, Value::String("browser".into())),
                SidebarTab::Review => tabs.insert(key, Value::String("review".into())),
                SidebarTab::Details => tabs.insert(key, Value::String("details".into())),
            };
            self.set_setting("thread_sidebar_tab", Value::Object(tabs));
        }
    }
    pub fn thread_sidebar_width(&self, id: ThreadId) -> f32 {
        self.snapshot
            .settings
            .get("thread_sidebar_width")
            .and_then(|widths| widths.get(id.to_string()))
            .and_then(Value::as_f64)
            .filter(|width| width.is_finite())
            .unwrap_or(352.) as f32
    }

    pub fn set_thread_sidebar_width(&mut self, id: ThreadId, width: f32) {
        if !width.is_finite() {
            return;
        }
        let mut widths = self
            .snapshot
            .settings
            .get("thread_sidebar_width")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        widths.insert(id.to_string(), json!(width.clamp(180., 1600.)));
        self.set_setting("thread_sidebar_width", Value::Object(widths));
    }
    /// User-resized composer height in logical pixels; absent keeps content-aware sizing.
    pub fn thread_composer_height(&self, id: ThreadId) -> Option<f32> {
        self.snapshot
            .settings
            .get("thread_composer_height")
            .and_then(|heights| heights.get(id.to_string()))
            .and_then(Value::as_f64)
            .filter(|height| height.is_finite())
            .map(|height| height.clamp(128., 1600.) as f32)
    }

    pub fn set_thread_composer_height(&mut self, id: ThreadId, height: f32) {
        if !height.is_finite() {
            return;
        }
        let height = height.clamp(128., 1600.);
        if self.thread_composer_height(id) == Some(height) {
            return;
        }
        let mut heights = self
            .snapshot
            .settings
            .get("thread_composer_height")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        heights.insert(id.to_string(), json!(height));
        self.set_setting("thread_composer_height", Value::Object(heights));
    }

    pub fn thread_activity(&self, id: ThreadId) -> ThreadActivity {
        self.unseen_turns
            .activity(id, self.active_threads.contains(&id))
    }

    /// A turn that did not complete or stop on request ended badly: shown as failed.
    fn turn_ended(&mut self, id: ThreadId) {
        let failed = self.conversations.get(&id).is_none_or(|session| {
            matches!(
                session.state,
                ConversationState::Failed | ConversationState::Disconnected
            ) || !matches!(
                session.outcome,
                Some(TurnOutcome::Completed | TurnOutcome::Interrupted)
            )
        });
        self.unseen_turns
            .turn_ended(id, self.selected_thread, failed);
    }

    /// Whether `thread` was never used: a placeholder made by New conversation or New
    /// Factory thread that Projects lists only while its tab is open, so an abandoned one
    /// disappears without being archived or deleted. Every user activity (drafting, sending,
    /// queueing, renaming, editing a Factory configuration, archiving) bumps `updated_at`,
    /// so an unused row still carries its creation timestamp and default title. Choosing a
    /// harness, model or effort is configuration, not activity. A Host-bound draft remains
    /// discoverable so its Host can be changed or released. Live state also counts as activity.
    pub fn is_untouched(&self, thread: &Thread) -> bool {
        let id = thread.id;
        let unused = !thread.archived
            && thread.remote.is_none()
            && thread.created_at == thread.updated_at
            && thread.draft.is_empty()
            && thread.attachments.is_empty()
            && thread.submission.is_none()
            && thread.queued.is_empty()
            && !self.active_threads.contains(&id);
        unused
            && match thread.kind {
                ThreadKind::Conversation => {
                    thread.title == crate::titles::DEFAULT_CONVERSATION_TITLE
                        && self.conversations.get(&id).is_none_or(|session| {
                            !session.preparing
                                && !session.messages.iter().any(|message| {
                                    matches!(message.role.as_str(), "user" | "assistant")
                                })
                        })
                }
                ThreadKind::Factory => {
                    thread.title == DEFAULT_FACTORY_TITLE
                        && !self.pending_factory.contains(&id)
                        && !self
                            .snapshot
                            .launches
                            .iter()
                            .any(|launch| launch.factory_thread_id == id)
                }
            }
    }

    /// Whether Projects and the palette list `thread` in `space`: an untouched thread only
    /// while its tab is open in that space.
    pub fn thread_listed_in(&self, thread: &Thread, space: SpaceId) -> bool {
        if !self.thread_in_space(thread.id, space) {
            return false;
        }
        let open = if space == self.active_space {
            self.open_threads.contains(&thread.id)
        } else {
            self.space_session(space)
                .is_some_and(|session| session.open_threads.contains(&thread.id))
        };
        open || !self.is_untouched(thread)
    }

    fn is_conversation(&self, id: ThreadId) -> bool {
        self.snapshot
            .threads
            .iter()
            .any(|thread| thread.id == id && thread.kind == ThreadKind::Conversation)
    }
    pub fn mutable_project_reason(&self) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is read-only: {error}"));
        }
        match self.project() {
            None => Some("Select a project first.".into()),
            Some(p) if p.archived => Some("This project is archived. History is read-only.".into()),
            _ => None,
        }
    }
    pub fn archive_project_reason(&self) -> Option<String> {
        if let Some(reason) = self.mutable_project_reason() {
            return Some(reason);
        }
        let id = self.selected_project?;
        if self.snapshot.threads.iter().any(|t| {
            t.project_id == id
                && (self.active_threads.contains(&t.id) || self.pending_factory.contains(&t.id))
        }) || self
            .snapshot
            .launches
            .iter()
            .any(|l| l.project_id == id && self.owned_launches.contains(&l.id))
        {
            Some("Stop active interactive turns and wait for owned Factory processes before archiving.".into())
        } else {
            None
        }
    }
    pub fn unarchive_project(&mut self, id: ProjectId) -> anyhow::Result<()> {
        anyhow::ensure!(!self.shutting_down, "Application is shutting down");
        if let Some(error) = &self.storage_error {
            anyhow::bail!("Storage is read-only: {error}");
        }
        let mut project = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Project is unavailable"))?;
        anyhow::ensure!(project.archived, "Project is not archived.");
        project.archived = false;
        self.save_project(project);
        Ok(())
    }
    pub fn rename_thread_reason(&self) -> Option<String> {
        let Some(thread) = self.thread() else {
            return Some("Select a thread first.".into());
        };
        self.rename_thread_reason_for(thread.id)
    }
    /// Why `id` cannot be renamed right now, independent of the current selection.
    pub fn rename_thread_reason_for(&self, id: ThreadId) -> Option<String> {
        if !self.thread_in_active_space(id) {
            return Some("This thread belongs to another space.".into());
        }
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is read-only: {error}"));
        }
        let Some(thread) = self.snapshot.threads.iter().find(|thread| thread.id == id) else {
            return Some("This thread is no longer available.".into());
        };
        let Some(project) = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == thread.project_id)
        else {
            return Some("This thread's project is no longer available.".into());
        };
        if project.archived {
            return Some("This project is archived. History is read-only.".into());
        }
        thread
            .archived
            .then(|| "Archived threads are read-only.".into())
    }
    pub fn archive_thread_reason(&self) -> Option<String> {
        if let Some(reason) = self.mutable_project_reason() {
            return Some(reason);
        }
        let Some(thread) = self.thread() else {
            return Some("Select a thread first.".into());
        };
        self.archive_thread_reason_for(thread.id)
    }
    /// Why `id` cannot be archived right now, independent of the current selection.
    pub fn archive_thread_reason_for(&self, id: ThreadId) -> Option<String> {
        if !self.thread_in_active_space(id) {
            return Some("This thread belongs to another space.".into());
        }
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is read-only: {error}"));
        }
        let Some(thread) = self.snapshot.threads.iter().find(|t| t.id == id) else {
            return Some("This thread is no longer available.".into());
        };
        if self
            .snapshot
            .projects
            .iter()
            .any(|p| p.id == thread.project_id && p.archived)
        {
            return Some("This project is archived. History is read-only.".into());
        }
        if thread.archived {
            return Some("This thread is already archived.".into());
        }
        if self.active_threads.contains(&thread.id)
            || self.pending_factory.contains(&thread.id)
            || self
                .snapshot
                .launches
                .iter()
                .any(|l| l.factory_thread_id == thread.id && self.owned_launches.contains(&l.id))
        {
            Some(
                "This thread has an active turn, pending Factory start, or owned Factory process."
                    .into(),
            )
        } else {
            None
        }
    }

    /// Why `id` cannot return from the archive right now.
    pub fn unarchive_thread_reason_for(&self, id: ThreadId) -> Option<String> {
        if !self.thread_in_active_space(id) {
            return Some("This thread belongs to another space.".into());
        }
        if let Some(reason) = self.space_mutation_reason() {
            return Some(reason);
        }
        let Some(thread) = self.snapshot.threads.iter().find(|t| t.id == id) else {
            return Some("This thread is no longer available.".into());
        };
        if !thread.archived {
            return Some("This thread is not archived.".into());
        }
        if self
            .snapshot
            .projects
            .iter()
            .any(|p| p.id == thread.project_id && p.archived)
        {
            return Some("This project is archived. History is read-only.".into());
        }
        if self.thread_worktree(thread).is_some_and(|w| !w.is_live()) {
            return Some("This thread's worktree was removed.".into());
        }
        None
    }
    pub fn unarchive_thread_reason(&self) -> Option<String> {
        match self.thread() {
            Some(thread) => self.unarchive_thread_reason_for(thread.id),
            None => Some("Select a thread first.".into()),
        }
    }
    /// Returns an archived thread to Projects with its history and draft.
    pub fn unarchive_thread(&mut self, id: ThreadId) -> Result<()> {
        if let Some(reason) = self.unarchive_thread_reason_for(id) {
            anyhow::bail!(reason);
        }
        let mut thread = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .cloned()
            .context("This thread is no longer available.")?;
        thread.archived = false;
        thread.updated_at = platform::now();
        self.save_thread(thread);
        Ok(())
    }

    pub fn select_project(&mut self, id: ProjectId) {
        if !self.project_in_active_space(id) {
            return;
        }
        self.selected_project = Some(id);
        if self.thread().is_some_and(|t| t.project_id != id) {
            if let Some(previous) = self.selected_thread {
                self.commit_thread_edits(previous);
            }
            self.selected_thread = None;
            self.select_launch_output(None);
        }
        self.persist_space_session();
    }
    /// A terminal has a project but no thread; keep open tabs and commit the previous draft.
    pub fn select_terminal_project(&mut self, id: ProjectId) {
        if !self.project_in_active_space(id) {
            return;
        }
        if let Some(previous) = self.selected_thread {
            self.commit_thread_edits(previous);
        }
        self.selected_project = Some(id);
        self.selected_thread = None;
        self.select_launch_output(None);
        self.persist_space_session();
    }
    pub fn select_thread(&mut self, id: ThreadId) {
        if !self.thread_in_active_space(id) {
            return;
        }
        let Some(thread) = self.snapshot.threads.iter().find(|thread| thread.id == id) else {
            return;
        };
        let launch_id = (thread.kind == ThreadKind::Factory)
            .then(|| {
                self.snapshot
                    .launches
                    .iter()
                    .find(|launch| launch.factory_thread_id == id)
                    .map(|launch| launch.id)
            })
            .flatten();
        self.selected_project = Some(thread.project_id);
        self.unseen_turns.seen(id);
        if self.selected_thread != Some(id)
            && let Some(previous) = self.selected_thread
        {
            self.commit_thread_edits(previous);
        }
        self.selected_thread = Some(id);
        if !self.open_threads.contains(&id) {
            self.open_threads.push(id);
        }
        self.select_launch_output(launch_id);
        self.persist_space_session();
    }
    pub fn close_thread(&mut self, id: ThreadId) {
        if !self.thread_in_active_space(id) {
            return;
        }
        self.pause_network_delivery(id);
        self.commit_thread_edits(id);
        let closed_ix = self.open_threads.iter().position(|open| *open == id);
        if let Some(ix) = closed_ix {
            self.open_threads.remove(ix);
        }
        if self.is_conversation(id) && !self.active_threads.contains(&id) {
            if let Some(session) = self.conversations.remove(&id) {
                self.close_artifacts(id);
                if let Some(commands) = session.commands {
                    let _ = commands.try_send(ConversationCommand::Close);
                }
            }
        }
        if self.selected_thread == Some(id) {
            if let Some(next) = closed_ix
                .and_then(|ix| {
                    self.open_threads
                        .get(ix.min(self.open_threads.len().saturating_sub(1)))
                })
                .copied()
            {
                self.select_thread(next);
            } else {
                self.selected_thread = None;
                self.select_launch_output(None);
            }
        }
        self.persist_space_session();
        self.sync_network();
    }
    /// Closes every open tab of `project`, committing their drafts.
    pub fn close_project_threads(&mut self, project: ProjectId) {
        let open: Vec<_> = self
            .open_threads
            .iter()
            .copied()
            .filter(|id| {
                self.snapshot
                    .threads
                    .iter()
                    .any(|thread| thread.id == *id && thread.project_id == project)
            })
            .collect();
        for id in open {
            self.close_thread(id);
        }
    }
    pub fn new_thread(&mut self, kind: ThreadKind) -> Result<()> {
        self.new_thread_with_harness(kind, None)
    }
    pub fn new_conversation(&mut self, harness: &str) -> Result<()> {
        anyhow::ensure!(
            harness::CONVERSATION_HARNESSES.contains(&harness),
            "Unsupported conversation harness: {harness}"
        );
        self.new_thread_with_harness(ThreadKind::Conversation, Some(harness))
    }
    /// Starts a fresh Conversation in an explicit checkout; `None` means the project root,
    /// regardless of the selected Conversation's worktree.
    pub fn new_conversation_in_workspace(&mut self, worktree_id: Option<WorktreeId>) -> Result<()> {
        if let Some(reason) = self.mutable_project_reason() {
            anyhow::bail!(reason);
        }
        anyhow::ensure!(
            !self.worktree_request_pending(),
            "A worktree operation is in progress."
        );
        let project = self.project().context("Select a project")?.clone();
        if let Some(id) = worktree_id {
            let worktree = self.worktree(id).context("Worktree not found")?;
            anyhow::ensure!(
                worktree.project_id == project.id,
                "This worktree belongs to another project."
            );
            anyhow::ensure!(worktree.is_live(), "This worktree was removed.");
        }
        let id = Uuid::new_v4();
        let thread = self.build_conversation(&project, None, id, worktree_id);
        self.save_thread(thread);
        self.register_thread_membership(id, project.id, self.active_space)?;
        self.select_thread(id);
        Ok(())
    }
    fn new_thread_with_harness(
        &mut self,
        kind: ThreadKind,
        conversation_harness: Option<&str>,
    ) -> Result<()> {
        if let Some(reason) = self.mutable_project_reason() {
            anyhow::bail!(reason);
        }
        let project = self.project().context("Select a project")?.clone();
        // A new thread follows the selected Conversation's checkout: its live worktree or the
        // project root. Factory threads only borrow the branch as their default target.
        let selected_worktree = self
            .thread()
            .filter(|thread| {
                thread.kind == ThreadKind::Conversation && thread.project_id == project.id
            })
            .and_then(|thread| self.thread_worktree(thread))
            .filter(|worktree| worktree.is_live())
            .cloned();
        let id = Uuid::new_v4();
        let thread = match kind {
            ThreadKind::Conversation => self.build_conversation(
                &project,
                conversation_harness,
                id,
                selected_worktree.map(|worktree| worktree.id),
            ),
            ThreadKind::Factory => {
                let timestamp = platform::now();
                Thread {
                    id,
                    project_id: project.id,
                    kind,
                    remote: None,
                    execution_started: false,
                    factory: Some(FactoryThreadConfig {
                        source_kind: FactorySourceKind::WorkItem,
                        source: String::new(),
                        harness: if project.defaults.factory_harness.trim().is_empty() {
                            "opencode".into()
                        } else {
                            project.defaults.factory_harness.clone()
                        },
                        model: String::new(),
                        thought_level: String::new(),
                        target_branch: project
                            .defaults
                            .factory_target_branch
                            .clone()
                            .or_else(|| {
                                selected_worktree
                                    .as_ref()
                                    .map(|worktree| worktree.branch.clone())
                            })
                            .unwrap_or_else(|| {
                                self.repositories
                                    .get(&project.id)
                                    .and_then(|repository| repository.as_ref().ok())
                                    .and_then(|repository| repository.branch.clone())
                                    .unwrap_or_default()
                            }),
                    }),
                    title: DEFAULT_FACTORY_TITLE.into(),
                    harness: String::new(),
                    session_ref: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    worktree_id: None,
                    pull_request: None,
                    draft: String::new(),
                    attachments: Vec::new(),
                    submission: None,
                    queued: Vec::new(),
                    archived: false,
                    created_at: timestamp.clone(),
                    updated_at: timestamp,
                }
            }
        };
        let project_id = thread.project_id;
        self.save_thread(thread);
        self.register_thread_membership(id, project_id, self.active_space)?;
        self.select_thread(id);
        Ok(())
    }
    /// A fresh Conversation row in `project`, on `worktree_id` or the project root. It copies
    /// the selected Conversation's harness settings (see `conversation_defaults`).
    pub(crate) fn build_conversation(
        &self,
        project: &Project,
        harness: Option<&str>,
        id: ThreadId,
        worktree_id: Option<WorktreeId>,
    ) -> Thread {
        let timestamp = platform::now();
        let defaults = conversation::conversation_defaults(
            &self.snapshot.threads,
            project,
            self.conversation_thread(),
            harness,
        );
        Thread {
            id,
            project_id: project.id,
            kind: ThreadKind::Conversation,
            factory: None,
            title: crate::titles::DEFAULT_CONVERSATION_TITLE.into(),
            harness: defaults.harness,
            session_ref: None,
            model: defaults.model,
            effort: defaults.effort,
            permission_mode: defaults.permission_mode,
            worktree_id,
            remote: None,
            execution_started: false,
            pull_request: None,
            draft: String::new(),
            attachments: Vec::new(),
            submission: None,
            queued: Vec::new(),
            archived: false,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        }
    }
    pub fn register_project(&mut self, path: PathBuf) {
        if self.space_mutation_reason().is_some() || self.registering {
            return;
        }
        self.registering = true;
        let sender = self.events.clone();
        let space = self.active_space;
        smol::spawn(async move {
            let result = smol::unblock(move || {
                platform::inspect_repository(&path).map_err(|e| format!("{e:#}"))
            })
            .await;
            let _ = sender.send(AppEvent::ProjectResolved(space, result)).await;
        })
        .detach();
    }
    pub fn install_pi(&mut self) {
        if self.installing_pi {
            return;
        }
        if self.storage_error.is_some() {
            self.notice(
                NoticeLevel::Warning,
                "Storage is not writable; Pi's managed path cannot be saved.",
            );
            return;
        }
        let explicit = self
            .snapshot
            .settings
            .get("executables")
            .and_then(|value| value.get("pi"))
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty());
        if let Ok(path) = platform::resolve_executable("pi", explicit) {
            self.notice(
                NoticeLevel::Info,
                format!("Pi is already available at {}", path.display()),
            );
            return;
        }
        self.installing_pi = true;
        self.diagnostics.insert("pi".into(), "Installing…".into());
        let sender = self.events.clone();
        let data_dir = self.data_dir.clone();
        smol::spawn(async move {
            let result = harness::install_pi(data_dir)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(AppEvent::PiInstalled(result)).await;
        })
        .detach();
    }

    /// Whether an inspection of installed title models is in flight.
    pub fn title_models_loading(&self) -> bool {
        self.title_models_request.is_some()
    }

    /// Lists installed title models in the background; earlier results stay visible meanwhile.
    pub fn refresh_title_models(&mut self) {
        let request = Uuid::new_v4();
        self.title_models_request = Some(request);
        let sender = self.events.clone();
        let llama = self
            .snapshot
            .settings
            .get("executables")
            .and_then(|value| value.get("llama"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        smol::spawn(async move {
            let models =
                smol::unblock(move || crate::titles::installed_models(llama.as_deref())).await;
            let _ = sender.send(AppEvent::TitleModels(request, models)).await;
        })
        .detach();
    }

    pub fn refresh_diagnostics(&self) {
        let sender = self.events.clone();
        let settings = self.snapshot.settings.clone();
        let projects: Vec<_> = self
            .snapshot
            .projects
            .iter()
            .map(|p| (p.id, p.root.clone()))
            .collect();
        smol::spawn(async move {
            let diagnostics = smol::unblock(move || executable_diagnostics(&settings)).await;
            if sender
                .send(AppEvent::Diagnostics(diagnostics))
                .await
                .is_err()
            {
                return;
            }
            for (id, root) in projects {
                let result = smol::unblock(move || {
                    platform::inspect_repository(&root).map_err(|e| format!("{e:#}"))
                })
                .await;
                if sender
                    .send(AppEvent::RepositoryChecked(id, result))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentBlock, ConversationSession, Message, SidebarTab, Value};

    #[cfg(feature = "runtime-qa")]
    mod threads {
        use crate::app::{
            AppState, Bootstrap, ConversationSession, WorktreeRequest, WorktreeRequestKind,
            WorktreeRequestStatus,
        };
        use crate::model::{
            ImageAttachment, ModelKey, Project, ProjectDefaults, ThreadKind, Worktree,
        };
        use crate::store::Store;
        use gpui_kit::{AppContext as _, Entity, TestAppContext};
        use std::{collections::BTreeMap, fs, path::PathBuf};
        use uuid::Uuid;

        struct TempDirectory(PathBuf);
        impl Drop for TempDirectory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        fn open(cx: &mut TestAppContext) -> (TempDirectory, Entity<AppState>, Uuid) {
            let dir = std::env::temp_dir().join(format!("drostje-app-{}", Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            let dir = TempDirectory(dir);
            let store = Store::open(dir.0.join("data")).unwrap();
            let project = Project {
                id: Uuid::new_v4(),
                name: "Project".into(),
                root: dir.0.join("root"),
                archived: false,
                defaults: ProjectDefaults::default(),
            };
            let id = project.id;
            smol::block_on(store.save_project(project)).unwrap();
            let snapshot = smol::block_on(store.snapshot()).unwrap();
            let bootstrap = Bootstrap {
                data_dir: dir.0.join("data"),
                store,
                snapshot,
                diagnostics: BTreeMap::new(),
                repositories: BTreeMap::new(),
                browser_tools: None,
            };
            let state = cx.new(|cx| AppState::new(bootstrap, cx));
            state.update(cx, |state, _| state.select_project(id));
            (dir, state, id)
        }

        #[gpui_kit::test]
        fn terminal_sidebar_persists_for_conversation_and_factory(cx: &mut TestAppContext) {
            let (_dir, state, _) = open(cx);
            state.update(cx, |state, _| {
                for kind in [ThreadKind::Conversation, ThreadKind::Factory] {
                    state.new_thread(kind).unwrap();
                    let id = state.selected_thread.unwrap();
                    state.show_thread_sidebar(id, crate::app::SidebarTab::Terminal);
                    assert_eq!(
                        state.thread_sidebar_tab(id),
                        crate::app::SidebarTab::Terminal
                    );
                    state.toggle_thread_sidebar(id, crate::app::SidebarTab::Terminal);
                    assert!(!state.thread_sidebar_open(id));
                    assert_eq!(
                        state.thread_sidebar_tab(id),
                        crate::app::SidebarTab::Terminal
                    );
                    assert_eq!(
                        state.snapshot.settings["thread_sidebar_tab"][id.to_string()],
                        "terminal"
                    );
                    state.show_thread_sidebar(id, crate::app::SidebarTab::Output);
                    state.show_thread_sidebar(id, crate::app::SidebarTab::Terminal);
                    assert!(state.thread_sidebar_open(id));
                    assert_eq!(
                        state.thread_sidebar_tab(id),
                        crate::app::SidebarTab::Terminal
                    );
                }
            });
        }

        fn new_conversation(state: &Entity<AppState>, cx: &mut TestAppContext) -> Uuid {
            state.update(cx, |state, _| {
                state.new_thread(ThreadKind::Conversation).unwrap();
                state.selected_thread.unwrap()
            })
        }

        fn worktree(project_id: Uuid) -> Worktree {
            let id = Uuid::new_v4();
            Worktree {
                id,
                project_id,
                path: std::env::temp_dir().join(id.to_string()),
                codename: "quiet-forest".into(),
                branch: "worktree-branch".into(),
                base: "main".into(),
                named: true,
                created_by: Uuid::new_v4(),
                created_at: "2026-01-01T00:00:00Z".into(),
                removed_at: None,
            }
        }

        /// Gives `id` the activity a sent prompt leaves behind.
        fn use_thread(state: &Entity<AppState>, id: Uuid, cx: &mut TestAppContext) {
            state.update(cx, |state, _| {
                let mut thread = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == id)
                    .cloned()
                    .unwrap();
                thread.title = "Used".into();
                thread.updated_at = "2999-01-01T00:00:00Z".into();
                state.save_thread(thread);
            });
        }

        #[gpui_kit::test]
        fn untouched_threads_leave_projects_with_their_tab(cx: &mut TestAppContext) {
            let (_dir, state, _) = open(cx);
            let draft = new_conversation(&state, cx);
            let used = new_conversation(&state, cx);
            use_thread(&state, used, cx);
            state.update(cx, |state, _| {
                let listed = |state: &AppState, id| {
                    let thread = state.snapshot.threads.iter().find(|t| t.id == id).unwrap();
                    state.thread_listed_in(thread, state.active_space)
                };
                let thread = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == draft)
                    .unwrap();
                assert!(state.is_untouched(thread));
                assert!(listed(state, draft), "An open placeholder is listed");
                state.close_thread(draft);
                state.close_thread(used);
                assert!(
                    !listed(state, draft),
                    "A closed placeholder leaves Projects"
                );
                assert!(
                    listed(state, used),
                    "A used thread stays after its tab closes"
                );
                assert!(
                    state.snapshot.threads.iter().any(|t| t.id == draft),
                    "Nothing is deleted"
                );
            });
        }

        #[gpui_kit::test]
        fn host_bound_empty_conversation_stays_discoverable_after_close(cx: &mut TestAppContext) {
            cx.dispatcher.allow_parking();
            let (_dir, state, project) = open(cx);
            let id = new_conversation(&state, cx);
            state.update(cx, |state, _| {
                state
                    .save_remote_host(crate::remote::RemoteHostConfig {
                        id: "empty-host".into(),
                        name: "Empty Host".into(),
                        relay_url: "wss://127.0.0.1:1/remote".into(),
                        token_env: format!("DROSTJE_TEST_UNUSED_{}", Uuid::new_v4().simple()),
                        projects: BTreeMap::from([(project.to_string(), "mounted".into())]),
                    })
                    .unwrap();
                state.set_thread_host(id, Some("empty-host")).unwrap();
                state.close_thread(id);
                let thread = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|thread| thread.id == id)
                    .unwrap();
                assert!(!state.is_untouched(thread));
                assert!(state.thread_listed_in(thread, state.active_space));
                assert!(!state.open_threads.contains(&id));
                assert!(state.remove_remote_host_reason("empty-host").is_some());
                state.select_thread(id);
                assert_eq!(state.selected_thread, Some(id));
                assert!(state.open_threads.contains(&id));
                state.set_thread_host(id, None).unwrap();
                assert!(state.remove_remote_host_reason("empty-host").is_none());
                state.remove_remote_host("empty-host").unwrap();
            });
        }

        #[gpui_kit::test]
        fn archiving_closes_the_tab_without_selecting_the_thread(cx: &mut TestAppContext) {
            let (_dir, state, _) = open(cx);
            let archived = new_conversation(&state, cx);
            use_thread(&state, archived, cx);
            let current = new_conversation(&state, cx);
            use_thread(&state, current, cx);
            state.update(cx, |state, _| {
                state.archive_thread(archived).unwrap();
                assert_eq!(state.selected_thread, Some(current), "Selection stays put");
                assert_eq!(state.open_threads, vec![current], "The archived tab closed");
                assert!(!state.show_archived, "Archived threads stay hidden");
                assert!(state.unarchive_thread_reason_for(archived).is_none());
                state.unarchive_thread(archived).unwrap();
                assert!(
                    !state
                        .snapshot
                        .threads
                        .iter()
                        .find(|t| t.id == archived)
                        .unwrap()
                        .archived
                );
                assert_eq!(
                    state.open_threads,
                    vec![current],
                    "Unarchiving opens nothing"
                );

                state.archive_thread(current).unwrap();
                assert_eq!(state.selected_thread, None);
                assert!(state.open_threads.is_empty());
            });
        }
        #[gpui_kit::test]
        fn undoing_project_archive_restores_its_read_only_state(cx: &mut TestAppContext) {
            cx.dispatcher.allow_parking();
            let (_dir, state, project) = open(cx);
            state.update(cx, |state, _| {
                let mut archived = state.project().unwrap().clone();
                archived.archived = true;
                state.save_project(archived);
                assert!(state.project().unwrap().archived);
                state.unarchive_project(project).unwrap();
                assert!(!state.project().unwrap().archived);
                assert!(state.unarchive_project(project).is_err());
            });
        }

        #[gpui_kit::test]
        fn a_new_conversation_copies_the_selected_one(cx: &mut TestAppContext) {
            let (_dir, state, _) = open(cx);
            let source = new_conversation(&state, cx);
            state.update(cx, |state, _| {
                let mut thread = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == source)
                    .cloned()
                    .unwrap();
                thread.harness = "claude-code".into();
                thread.effort = Some("high".into());
                thread.permission_mode = Some("plan".into());
                state.save_thread(thread);
            });
            let copy = new_conversation(&state, cx);
            state.update(cx, |state, _| {
                let thread = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == copy)
                    .unwrap();
                assert_eq!(thread.harness, "claude-code");
                assert_eq!(thread.effort.as_deref(), Some("high"));
                assert_eq!(thread.permission_mode.as_deref(), Some("plan"));
            });
        }

        #[gpui_kit::test]
        fn switching_to_auto_persists_only_a_harness_auto_mode(cx: &mut TestAppContext) {
            use crate::harness::{UserAnswer, UserRequest, UserRequestKind};
            cx.dispatcher.allow_parking();
            let (_dir, state, _) = open(cx);
            for (harness, expected) in [
                ("codex", Some("auto")),
                ("claude-code", Some("auto")),
                ("opencode", Some("plan")),
            ] {
                let id = new_conversation(&state, cx);
                state.update(cx, |state, _| {
                    let mut thread = state.thread().unwrap().clone();
                    thread.harness = harness.into();
                    thread.permission_mode = Some("plan".into());
                    state.save_thread(thread);
                    let mut session = ConversationSession::new(None);
                    session.requests.insert(
                        "ask".into(),
                        UserRequest {
                            id: "ask".into(),
                            kind: UserRequestKind::Permission,
                            title: "Allow?".into(),
                            options: Vec::new(),
                            text: String::new(),
                            placeholder: None,
                            deadline: None,
                        },
                    );
                    state.conversations.insert(id, session);
                    state.respond(id, "ask".into(), UserAnswer::PermissionAuto);
                    let thread = state.snapshot.threads.iter().find(|t| t.id == id).unwrap();
                    assert_eq!(thread.permission_mode.as_deref(), expected, "{harness}");
                });
            }
        }

        #[gpui_kit::test]
        fn explicit_project_root_does_not_inherit_the_selected_worktree(cx: &mut TestAppContext) {
            let (_dir, state, project) = open(cx);
            let source = new_conversation(&state, cx);
            state.update(cx, |state, _| {
                let workspace = worktree(project);
                let workspace_id = workspace.id;
                state.save_worktree(workspace);
                let mut thread = state.thread().unwrap().clone();
                thread.worktree_id = Some(workspace_id);
                state.save_thread(thread);

                state.new_thread(ThreadKind::Conversation).unwrap();
                assert_eq!(state.thread().unwrap().worktree_id, Some(workspace_id));
                state.select_thread(source);
                state.new_conversation_in_workspace(None).unwrap();

                let thread = state.thread().unwrap();
                assert_ne!(thread.id, source);
                assert_eq!(thread.project_id, project);
                assert_eq!(thread.worktree_id, None);
                assert!(state.thread_in_space(thread.id, state.active_space));
                assert!(state.open_threads.contains(&thread.id));
                assert_eq!(
                    state
                        .snapshot
                        .threads
                        .iter()
                        .find(|t| t.id == source)
                        .unwrap()
                        .worktree_id,
                    Some(workspace_id)
                );
            });
        }

        #[gpui_kit::test]
        fn choosing_a_workspace_preserves_source_draft_session_and_defaults(
            cx: &mut TestAppContext,
        ) {
            let (_dir, state, project) = open(cx);
            let source = new_conversation(&state, cx);
            state.update(cx, |state, _| {
                let source_workspace = worktree(project);
                let source_workspace_id = source_workspace.id;
                state.save_worktree(source_workspace);
                let target = worktree(project);
                let target_id = target.id;
                state.save_worktree(target);
                let mut thread = state.thread().unwrap().clone();
                thread.worktree_id = Some(source_workspace_id);
                thread.harness = "claude-code".into();
                thread.model = Some(ModelKey {
                    provider: "anthropic".into(),
                    id: "claude-opus".into(),
                });
                thread.effort = Some("high".into());
                thread.permission_mode = Some("plan".into());
                thread.session_ref = Some("existing-provider-session".into());
                thread.draft = "Keep this unsent prompt".into();
                thread.attachments.push(ImageAttachment {
                    id: Uuid::new_v4(),
                    path: PathBuf::from("/tmp/draft.png"),
                    mime: "image/png".into(),
                    width: 10,
                    height: 20,
                    original: None,
                });
                let original = serde_json::to_value(&thread).unwrap();
                let model = thread.model.clone();
                state.edit_thread(thread);
                let session = ConversationSession::new(model.clone());
                let generation = session.generation;
                state.conversations.insert(source, session);

                state
                    .new_conversation_in_workspace(Some(target_id))
                    .unwrap();

                let thread = state.thread().unwrap();
                assert_ne!(thread.id, source);
                assert_eq!(thread.project_id, project);
                assert_eq!(thread.worktree_id, Some(target_id));
                assert_eq!(thread.kind, ThreadKind::Conversation);
                assert_eq!(thread.harness, "claude-code");
                assert_eq!(thread.model, model);
                assert_eq!(thread.effort.as_deref(), Some("high"));
                assert_eq!(thread.permission_mode.as_deref(), Some("plan"));
                assert_eq!(thread.title, crate::titles::DEFAULT_CONVERSATION_TITLE);
                assert!(thread.session_ref.is_none());
                assert!(thread.draft.is_empty());
                assert!(thread.attachments.is_empty());
                assert!(thread.submission.is_none());
                assert!(thread.queued.is_empty());
                assert!(!thread.archived);
                assert!(thread.factory.is_none());
                assert_eq!(thread.created_at, thread.updated_at);
                assert!(!state.conversations.contains_key(&thread.id));
                assert_eq!(
                    state.conversations.get(&source).unwrap().generation,
                    generation
                );
                assert_eq!(
                    serde_json::to_value(
                        state
                            .snapshot
                            .threads
                            .iter()
                            .find(|t| t.id == source)
                            .unwrap()
                    )
                    .unwrap(),
                    original
                );
            });
        }

        #[gpui_kit::test]
        fn workspace_creation_without_a_source_uses_project_defaults(cx: &mut TestAppContext) {
            let (_dir, state, project) = open(cx);
            state.update(cx, |state, _| {
                let defaults = &mut state
                    .snapshot
                    .projects
                    .iter_mut()
                    .find(|p| p.id == project)
                    .unwrap()
                    .defaults;
                defaults.interactive_harness = "claude-code".into();
                defaults.interactive_model = Some(ModelKey {
                    provider: "anthropic".into(),
                    id: "claude-opus".into(),
                });
                let model = defaults.interactive_model.clone();
                state.new_conversation_in_workspace(None).unwrap();
                let thread = state.thread().unwrap();
                assert_eq!(thread.project_id, project);
                assert_eq!(thread.worktree_id, None);
                assert_eq!(thread.harness, "claude-code");
                assert_eq!(thread.model, model);
                assert_eq!(thread.effort, None);
                assert_eq!(thread.permission_mode, None);
            });
        }

        #[gpui_kit::test]
        fn invalid_workspaces_do_not_create_or_select_a_thread(cx: &mut TestAppContext) {
            let (_dir, state, project) = open(cx);
            let source = new_conversation(&state, cx);
            state.update(cx, |state, _| {
                let mut removed = worktree(project);
                removed.removed_at = Some("2026-02-01T00:00:00Z".into());
                let removed_id = removed.id;
                state.save_worktree(removed);
                let foreign = worktree(Uuid::new_v4());
                let foreign_id = foreign.id;
                state.save_worktree(foreign);
                let threads = serde_json::to_value(&state.snapshot.threads).unwrap();
                let open = state.open_threads.clone();

                for id in [Uuid::new_v4(), removed_id, foreign_id] {
                    assert!(state.new_conversation_in_workspace(Some(id)).is_err());
                    assert_eq!(state.selected_thread, Some(source));
                    assert_eq!(state.open_threads, open);
                    assert_eq!(
                        serde_json::to_value(&state.snapshot.threads).unwrap(),
                        threads
                    );
                }
                state.worktree_request = Some(WorktreeRequest {
                    id: Uuid::new_v4(),
                    kind: WorktreeRequestKind::Remove {
                        worktree: removed_id,
                        force: false,
                    },
                    status: WorktreeRequestStatus::Pending,
                });
                assert!(state.new_conversation_in_workspace(None).is_err());
                assert_eq!(state.selected_thread, Some(source));
                assert_eq!(state.open_threads, open);
                assert_eq!(
                    serde_json::to_value(&state.snapshot.threads).unwrap(),
                    threads
                );
            });
        }

        #[gpui_kit::test]
        fn workspace_creation_rejects_immutable_or_missing_projects(cx: &mut TestAppContext) {
            let (_dir, state, project) = open(cx);
            state.update(cx, |state, _| {
                state
                    .snapshot
                    .projects
                    .iter_mut()
                    .find(|p| p.id == project)
                    .unwrap()
                    .archived = true;
                assert!(state.new_conversation_in_workspace(None).is_err());
                state
                    .snapshot
                    .projects
                    .iter_mut()
                    .find(|p| p.id == project)
                    .unwrap()
                    .archived = false;
                state.storage_error = Some("read-only test store".into());
                assert!(state.new_conversation_in_workspace(None).is_err());
                state.storage_error = None;
                state.shutting_down = true;
                assert!(state.new_conversation_in_workspace(None).is_err());
                state.shutting_down = false;
                state.selected_project = None;
                assert!(state.new_conversation_in_workspace(None).is_err());
                assert!(state.snapshot.threads.is_empty());
                assert!(state.open_threads.is_empty());
                assert_eq!(state.selected_thread, None);
            });
        }

        #[gpui_kit::test]
        fn closed_pull_requests_archive_their_conversation_once_it_is_idle(
            cx: &mut TestAppContext,
        ) {
            use crate::app::{AppEvent, LinkStatus};
            use crate::model::{PullRequestLink, PullRequestState};
            let (_dir, state, project) = open(cx);
            let id = new_conversation(&state, cx);
            use_thread(&state, id, cx);
            state.update(cx, |state, cx| {
                let tree = worktree(project);
                let tree_id = tree.id;
                state.save_worktree(tree);
                let mut thread = state.thread().unwrap().clone();
                thread.worktree_id = Some(tree_id);
                thread.pull_request = Some(PullRequestLink {
                    id: 3170,
                    url: "https://dev.azure.com/o/p/_git/r/pullrequest/3170".into(),
                    title: "Fix upload".into(),
                    state: PullRequestState::Completed,
                    work_items: vec![2596],
                    archived_on_close: false,
                });
                state.save_thread(thread);
                let find = |state: &AppState| {
                    state
                        .snapshot
                        .threads
                        .iter()
                        .find(|t| t.id == id)
                        .unwrap()
                        .clone()
                };

                // A running turn holds the archive back and says why.
                state.active_threads.insert(id);
                state.settle_closed_pull_requests();
                assert!(!find(state).archived);
                assert!(matches!(
                    state.pull_request_status(id),
                    Some(LinkStatus::ArchivePending(_))
                ));

                // The turn ends: the Conversation archives, naming the pull request.
                state.receive(
                    AppEvent::InteractiveActivity {
                        thread_id: id,
                        active: false,
                    },
                    cx,
                );
                let archived = find(state);
                assert!(archived.archived);
                assert!(archived.pull_request.unwrap().archived_on_close);
                assert!(state.pull_request_status(id).is_none());
                assert!(
                    state
                        .notices
                        .iter()
                        .any(|notice| notice.text.contains("pull request 3170 was completed"))
                );

                // Unarchived by the user, it stays.
                state.unarchive_thread(id).unwrap();
                state.settle_closed_pull_requests();
                assert!(!find(state).archived);
            });
        }
    }

    #[test]
    fn sidebar_toggle_switches_tabs_before_hiding() {
        use SidebarTab::{Changes, Output};
        assert_eq!(SidebarTab::toggle(true, Changes, Output), (true, Output));
        assert_eq!(SidebarTab::toggle(true, Output, Output), (false, Output));
        assert_eq!(SidebarTab::toggle(false, Output, Output), (true, Output));
        assert_eq!(SidebarTab::toggle(false, Output, Changes), (true, Changes));
    }

    fn message(id: &str, role: &str, content: Vec<ContentBlock>) -> Message {
        Message {
            id: id.into(),
            role: role.into(),
            content,
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        }
    }

    #[test]
    fn session_folds_tool_results_under_their_call() {
        let mut session = ConversationSession::new(None);
        assert!(!session.has_tool_call("call_1"));
        session.message(message(
            "assistant:1",
            "assistant",
            vec![ContentBlock::ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                intent: None,
                input: Value::Null,
            }],
        ));
        assert!(session.has_tool_call("call_1"));
        assert!(session.tool_result("call_1").is_none());
        assert_eq!(
            session.tool_call_order(),
            &[("bash".to_owned(), "call_1".to_owned())]
        );
        assert!(matches!(
            session.tool_call_block("call_1"),
            Some(ContentBlock::ToolCall { name, .. }) if name == "bash"
        ));
        // A streamed update of the same assistant message does not repeat the call.
        session.message(message(
            "assistant:1",
            "assistant",
            vec![ContentBlock::ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                intent: None,
                input: Value::Null,
            }],
        ));
        assert_eq!(session.tool_call_order().len(), 1);
        let mut result = message(
            "toolResult:2",
            "toolResult",
            vec![ContentBlock::Text("partial".into())],
        );
        result.tool_call_id = Some("call_1".into());
        session.message(result.clone());
        result.content = vec![ContentBlock::Text("final".into())];
        result.exit_code = Some(1);
        let rendered_revision = session.transcript_revision;
        session.message(result);
        assert_eq!(session.messages.len(), 2);
        let found = session
            .tool_result("call_1")
            .expect("result indexed by call id");
        assert_eq!(found.exit_code, Some(1));
        assert!(matches!(&found.content[..], [ContentBlock::Text(text)] if text == "final"));
        let changed: std::collections::BTreeSet<_> = session
            .transcript_changes_since(rendered_revision)
            .unwrap()
            .collect();
        assert_eq!(changed, std::collections::BTreeSet::from([0, 1]));
        session.message(message(
            "custom:9",
            "custom",
            vec![ContentBlock::Text(
                "<system-notice>x</system-notice>".into(),
            )],
        ));
        assert_eq!(session.notices().count(), 1);
        session.reset_history();
        assert!(!session.has_tool_call("call_1"));
        assert!(session.tool_result("call_1").is_none());
        assert!(session.messages.is_empty());
        assert!(session.tool_call_order().is_empty());
        assert_eq!(session.notices().count(), 0);
    }

    #[test]
    fn lagging_transcript_resets_instead_of_reusing_stale_rows() {
        let mut session = ConversationSession::new(None);
        session.message(message(
            "one",
            "assistant",
            vec![ContentBlock::Text("first".into())],
        ));
        let old_revision = session.transcript_revision;
        for index in 0..513 {
            session.message(message(
                "one",
                "assistant",
                vec![ContentBlock::Text(index.to_string())],
            ));
        }
        assert!(session.transcript_changes_since(old_revision).is_none());
        let current = session.transcript_revision;
        session.message(message(
            "one",
            "assistant",
            vec![ContentBlock::Text("latest".into())],
        ));
        assert_eq!(
            session
                .transcript_changes_since(current)
                .unwrap()
                .collect::<Vec<_>>(),
            [0]
        );
        let current = session.transcript_revision;
        session.reset_history();
        assert!(session.transcript_changes_since(current).is_none());
    }
}
