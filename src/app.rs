mod conversation;
mod factory;
mod persistence;
mod project_diff;
mod shutdown;
mod spaces;
use crate::harness::{
    self, ContentBlock, ContextUsage, ConversationCommand, ConversationEvent, ConversationHandle,
    ConversationState, Message, ModelChoice, ToolActivity, TurnOutcome, UsageLimit, UserAnswer,
    UserRequest,
};
use crate::{model::*, platform, store::Store};
use anyhow::{Context as _, Result};
use gpui_kit::*;
pub use project_diff::{ProjectDiffState, ProjectDiffs};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    path::PathBuf,
};
use uuid::Uuid;

/// Content changes are separate from structural workspace invalidation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresentationChange {
    Draft(ThreadId),
    Thread(ThreadId),
    Transcript(ThreadId),
    Diff(ProjectId),
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
    pub models: Vec<ModelChoice>,
    pub model: Option<ModelKey>,
    pub messages: Vec<Arc<Message>>,
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
            models: Vec::new(),
            model,
            generation: Uuid::new_v4(),
            finished: None,
            messages: Vec::new(),
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
}

impl Bootstrap {
    /// Runs before GPUI starts; subsequent filesystem, Git and database work is asynchronous.
    pub fn load() -> Result<Self> {
        let data_dir = platform::data_dir()?;
        let store = Store::open(data_dir.clone())?;
        let mut snapshot = smol::block_on(store.snapshot())?;
        if !snapshot.settings.contains_key("executables") {
            let value = json!({
                "nvim": "", "omp": "", "pi": "", "claude": "", "llama": "", "factory": ""
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
        let diagnostics = executable_diagnostics(&snapshot.settings);
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
        })
    }
}

fn executable_diagnostics(settings: &BTreeMap<String, Value>) -> BTreeMap<String, String> {
    ["nvim", "omp", "pi", "claude", "llama", "factory"]
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
    Flush(async_channel::Sender<Result<(), String>>),
}

/// A tab of a thread's sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTab {
    Changes,
    Output,
}
impl SidebarTab {
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
    PiInstalled(Result<PathBuf, String>),
    StorageFailed(String),
    InteractiveActivity { thread_id: ThreadId, active: bool },
    FactorySnapshot(Box<crate::factory::FactorySnapshot>),
    HarnessOpened(ThreadId, Uuid, Result<ConversationHandle, String>),
    Harness(ThreadId, Uuid, ConversationEvent),
    Notice(WorkspaceNotice),
    HarnessDrained(async_channel::Sender<()>),
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
    pub notices: VecDeque<WorkspaceNotice>,
    pub notice_sequence: u64,
    pub storage_error: Option<String>,
    pub pending_writes: usize,
    /// Changed in memory; no persistence work is queued until a commit boundary.
    edited_threads: HashSet<ThreadId>,
    pub installing_pi: bool,
    pub registering: bool,
    pub active_threads: HashSet<ThreadId>,
    pub owned_launches: HashSet<LaunchId>,
    pub events: async_channel::Sender<AppEvent>,
    pub shutting_down: bool,
    pub shutdown_complete: bool,
    pub shutdown_error: Option<String>,
    pub conversations: BTreeMap<ThreadId, ConversationSession>,
    pub project_diffs: ProjectDiffs,
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
            notice_sequence: notices.len() as u64,
            notices,
            installing_pi: false,
            storage_error,
            pending_writes: 0,
            edited_threads: HashSet::new(),
            registering: false,
            conversations: BTreeMap::new(),
            project_diffs: ProjectDiffs::default(),
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
            active_threads: HashSet::new(),
            owned_launches: HashSet::new(),
            events,
            writes,
        };
        state.persist_space_session();
        state
    }

    fn receive(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        match event {
            AppEvent::WriteFinished(result) => {
                self.pending_writes = self.pending_writes.saturating_sub(1);
                if let Err(error) = result {
                    self.storage_error = Some(error.clone());
                    self.notice(NoticeLevel::Error, error);
                    cx.notify();
                } else {
                    cx.emit(PresentationChange::Persistence);
                }
                return;
            }
            AppEvent::StorageFailed(error) => {
                self.storage_error = Some(error.clone());
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
                    );
                    self.receive_conversation(id, event, cx);
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
        if !self.is_conversation(id) {
            return SidebarTab::Output;
        }
        match self
            .snapshot
            .settings
            .get("thread_sidebar_tab")
            .and_then(Value::as_object)
            .and_then(|threads| threads.get(&id.to_string()))
            .and_then(Value::as_str)
        {
            Some("output") => SidebarTab::Output,
            _ => SidebarTab::Changes,
        }
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
        if self.is_conversation(id) && self.thread_sidebar_tab(id) != tab {
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
            };
            self.set_setting("thread_sidebar_tab", Value::Object(tabs));
        }
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
        self.commit_thread_edits(id);
        let closed_ix = self.open_threads.iter().position(|open| *open == id);
        if let Some(ix) = closed_ix {
            self.open_threads.remove(ix);
        }
        if self.is_conversation(id) && !self.active_threads.contains(&id) {
            if let Some(session) = self.conversations.remove(&id) {
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
    fn new_thread_with_harness(
        &mut self,
        kind: ThreadKind,
        conversation_harness: Option<&str>,
    ) -> Result<()> {
        if let Some(reason) = self.mutable_project_reason() {
            anyhow::bail!(reason);
        }
        let project = self.project().context("Select a project")?;
        let timestamp = platform::now();
        let (title, harness, model, factory) = match kind {
            ThreadKind::Conversation => (
                crate::titles::DEFAULT_CONVERSATION_TITLE,
                conversation_harness.map(str::to_owned).unwrap_or_else(|| {
                    if project.defaults.interactive_harness.trim().is_empty() {
                        "omp".into()
                    } else {
                        project.defaults.interactive_harness.clone()
                    }
                }),
                project.defaults.interactive_model.clone(),
                None,
            ),
            ThreadKind::Factory => (
                "New Factory thread",
                String::new(),
                None,
                Some(FactoryThreadConfig {
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
                        .unwrap_or_else(|| {
                            self.repositories
                                .get(&project.id)
                                .and_then(|repository| repository.as_ref().ok())
                                .and_then(|repository| repository.branch.clone())
                                .unwrap_or_default()
                        }),
                }),
            ),
        };
        let thread = Thread {
            id: Uuid::new_v4(),
            project_id: project.id,
            kind,
            factory,
            title: title.into(),
            harness,
            session_ref: None,
            model,
            effort: None,
            permission_mode: None,
            draft: String::new(),
            attachments: Vec::new(),
            submission: None,
            queued: Vec::new(),
            archived: false,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        let id = thread.id;
        let project = thread.project_id;
        self.save_thread(thread);
        self.register_thread_membership(id, project, self.active_space)?;
        self.select_thread(id);
        Ok(())
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
