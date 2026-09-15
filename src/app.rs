mod conversation;
mod factory;
mod shutdown;
use crate::{
    harness::{
        self, ConversationCommand, ConversationEvent, ConversationHandle, ConversationState,
        Message, ModelChoice, ToolActivity, TurnOutcome, UserAnswer, UserRequest,
    },
    neovim::{DraftKey, DraftSnapshot, NvimEditor},
};
use crate::{model::*, platform, store::Store};
use anyhow::{Context as _, Result};
use gpui_kit::*;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    path::PathBuf,
};
use uuid::Uuid;

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
    pub links: Vec<(String, Option<String>)>,
    pub diagnostics: VecDeque<(String, Value)>,
    pub error: Option<String>,
    pub outcome: Option<TurnOutcome>,
    pub preparing: bool,
    pub list: ListState,
    indices: HashMap<String, usize>,
    submitted: HashMap<Uuid, DraftSnapshot>,
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
            links: Vec::new(),
            diagnostics: VecDeque::new(),
            error: None,
            outcome: None,
            preparing: false,
            list: ListState::new(0, ListAlignment::Bottom, px(500.)),
            indices: HashMap::new(),
            submitted: HashMap::new(),
        }
    }
    fn message(&mut self, message: Message) {
        if let Some(index) = self.indices.get(&message.id).copied() {
            self.messages[index] = Arc::new(message);
            self.list.splice(index..index + 1, 1);
        } else {
            let index = self.messages.len();
            self.indices.insert(message.id.clone(), index);
            self.messages.push(Arc::new(message));
            self.list.splice(index..index, 1);
        }
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
            let value = json!({ "omp": "", "nvim": "", "factory": "" });
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
    ["omp", "nvim", "factory"]
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
    Thread(Thread, Option<async_channel::Sender<Result<(), String>>>),
    Setting(String, Value),
    Flush(async_channel::Sender<Result<(), String>>),
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
    ProjectResolved(Result<platform::RepositoryInfo, String>),
    Diagnostics(BTreeMap<String, String>),
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
    pub selected_project: Option<ProjectId>,
    pub selected_thread: Option<ThreadId>,
    pub selected_launch: Option<LaunchId>,
    pub show_archived: bool,
    pub collapsed: HashSet<ProjectId>,
    pub repositories: BTreeMap<ProjectId, Result<platform::RepositoryInfo, String>>,
    pub diagnostics: BTreeMap<String, String>,
    pub notices: VecDeque<WorkspaceNotice>,
    pub notice_sequence: u64,
    pub storage_error: Option<String>,
    pub pending_writes: usize,
    pub registering: bool,
    pub active_threads: HashSet<ThreadId>,
    pub owned_launches: HashSet<LaunchId>,
    pub events: async_channel::Sender<AppEvent>,
    pub shutting_down: bool,
    pub shutdown_complete: bool,
    pub shutdown_error: Option<String>,
    pub conversations: BTreeMap<ThreadId, ConversationSession>,
    pub editor: Option<NvimEditor>,
    pub drafts: HashMap<DraftKey, DraftSnapshot>,
    pub editor_error: Option<String>,
    pub factory: Option<crate::factory::FactoryManager>,
    pub factory_runs: BTreeMap<LaunchId, crate::factory::FactorySnapshot>,
    pub factory_text: Option<Result<crate::factory::TextPage, String>>,
    pub factory_text_path: Option<PathBuf>,
    pub factory_text_generation: Uuid,
    pub pending_factory: HashSet<ThreadId>,
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
        let selected_thread = snapshot
            .settings
            .get("selected_thread")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .filter(|id| snapshot.threads.iter().any(|t| t.id == *id))
            .or_else(|| {
                let launch_id = snapshot
                    .settings
                    .get("selected_launch")
                    .and_then(Value::as_str)
                    .and_then(|id| Uuid::parse_str(id).ok())?;
                let launch = snapshot
                    .launches
                    .iter()
                    .find(|launch| launch.id == launch_id)?;
                snapshot
                    .threads
                    .iter()
                    .find(|thread| {
                        thread.id == launch.factory_thread_id
                            && thread.kind == ThreadKind::Factory
                            && thread.project_id == launch.project_id
                    })
                    .map(|thread| thread.id)
            });
        let selected_project = selected_thread
            .and_then(|id| {
                snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| t.project_id)
            })
            .or_else(|| {
                snapshot
                    .settings
                    .get("selected_project")
                    .and_then(Value::as_str)
                    .and_then(|id| Uuid::parse_str(id).ok())
                    .filter(|id| snapshot.projects.iter().any(|p| p.id == *id))
            })
            .or_else(|| snapshot.projects.iter().find(|p| !p.archived).map(|p| p.id));
        let selected_launch = selected_thread.and_then(|id| {
            snapshot
                .threads
                .iter()
                .find(|thread| thread.id == id && thread.kind == ThreadKind::Factory)
                .and_then(|thread| {
                    snapshot
                        .launches
                        .iter()
                        .find(|launch| launch.factory_thread_id == thread.id)
                        .map(|launch| launch.id)
                })
        });
        let show_archived = snapshot
            .settings
            .get("show_archived")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (events, receiver) = async_channel::bounded(256);
        let (writes, requests) = async_channel::unbounded();
        let completed = events.clone();
        let failures = store.failures.clone();
        let durable_store = store.clone();
        let mut notices = VecDeque::new();
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
        smol::spawn(async move {
            while let Ok(request) = requests.recv().await {
                let result = match request {
                    Write::Project(project) => store.save_project(project).await,
                    Write::Thread(thread, reply) => {
                        let result = store
                            .save_thread(thread)
                            .await
                            .map_err(|error| format!("{error:#}"));
                        if let Some(reply) = reply {
                            let _ = reply.send(result.clone()).await;
                        }
                        result.map_err(anyhow::Error::msg)
                    }
                    Write::Setting(key, value) => store.set_setting(key, value).await,
                    Write::Flush(reply) => {
                        let result = store.flush().await.map_err(|e| format!("{e:#}"));
                        let _ = reply.send(result).await;
                        continue;
                    }
                }
                .map_err(|e| format!("{e:#}"));
                let _ = completed.send(AppEvent::WriteFinished(result)).await;
            }
        })
        .detach();
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
            while let Ok(first) = receiver.recv().await {
                let mut batch = Vec::with_capacity(64);
                batch.push(first);
                while batch.len() < 64 {
                    let Ok(event) = receiver.try_recv() else {
                        break;
                    };
                    batch.push(event);
                }
                let _ = this.update(cx, |this, cx| {
                    for event in batch {
                        this.receive(event, cx);
                    }
                });
                smol::future::yield_now().await;
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
            let mut async_cx = cx.to_async();
            async move {
                // Last-resort best effort for platform termination that bypasses
                // Drost's quit action. Normal quit has already completed cleanup.
                if let Some(shutdown) = shutdown {
                    if let Err(error) = shutdown.run(state, &mut async_cx, true).await {
                        eprintln!("Forced platform shutdown: {error:#}");
                    }
                }
            }
        })
        .detach();
        Self {
            data_dir,
            store: durable_store,
            snapshot,
            selected_project,
            selected_thread,
            selected_launch,
            show_archived,
            collapsed: HashSet::new(),
            repositories,
            diagnostics,
            notice_sequence: notices.len() as u64,
            notices,
            storage_error: None,
            pending_writes: 0,
            registering: false,
            conversations: BTreeMap::new(),
            editor: None,
            drafts: HashMap::new(),
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
            active_threads: HashSet::new(),
            owned_launches: HashSet::new(),
            events,
            writes,
        }
    }

    fn receive(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        match event {
            AppEvent::WriteFinished(result) => {
                self.pending_writes = self.pending_writes.saturating_sub(1);
                if let Err(error) = result {
                    self.storage_error = Some(error.clone());
                    self.notice(NoticeLevel::Error, error);
                }
            }
            AppEvent::StorageFailed(error) => {
                self.storage_error = Some(error.clone());
                self.notice(NoticeLevel::Error, error);
            }
            AppEvent::RepositoryChecked(id, info) => {
                self.repositories.insert(id, info);
            }
            AppEvent::ProjectResolved(result) => {
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
                            self.select_project(id);
                            self.repositories.insert(id, Ok(info));
                            if archived {
                                self.show_archived = true;
                                self.set_setting("show_archived", json!(true));
                            }
                            self.notice(NoticeLevel::Info, "This working tree is already registered; selected the existing project.");
                        } else if self.storage_error.is_none() {
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
                            self.select_project(project.id);
                            self.repositories.insert(project.id, Ok(info));
                            self.save_project(project);
                        }
                    }
                    Err(error) => self.notice(NoticeLevel::Error, error),
                }
            }
            AppEvent::Diagnostics(diagnostics) => self.diagnostics = diagnostics,
            AppEvent::InteractiveActivity { thread_id, active } => {
                if active && self.is_conversation(thread_id) {
                    self.active_threads.insert(thread_id);
                } else {
                    self.active_threads.remove(&thread_id);
                }
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
            }
            AppEvent::Harness(id, generation, event) => {
                if self
                    .conversations
                    .get(&id)
                    .is_some_and(|s| s.generation == generation)
                {
                    self.receive_conversation(id, event, cx);
                }
            }
            AppEvent::HarnessDrained(reply) => {
                let _ = reply.try_send(());
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

    fn enqueue(&mut self, request: Write) {
        if self.storage_error.is_some() {
            return;
        }
        match self.writes.try_send(request) {
            Ok(()) => self.pending_writes += 1,
            Err(error) => {
                self.storage_error = Some(error.to_string());
            }
        }
    }

    pub fn set_setting(&mut self, key: &str, value: Value) {
        self.snapshot.settings.insert(key.into(), value.clone());
        self.enqueue(Write::Setting(key.into(), value));
    }

    pub fn save_project(&mut self, project: Project) {
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
    }

    pub fn save_thread(&mut self, thread: Thread) {
        if let Some(old) = self.snapshot.threads.iter_mut().find(|t| t.id == thread.id) {
            *old = thread.clone();
        } else {
            self.snapshot.threads.push(thread.clone());
        }
        self.enqueue(Write::Thread(thread, None));
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
            self.enqueue(Write::Thread(thread, Some(reply)));
        }
        result
    }

    pub fn project(&self) -> Option<&Project> {
        self.selected_project
            .and_then(|id| self.snapshot.projects.iter().find(|p| p.id == id))
    }
    pub fn thread(&self) -> Option<&Thread> {
        self.selected_thread
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
    pub fn archive_thread_reason(&self) -> Option<String> {
        if let Some(reason) = self.mutable_project_reason() {
            return Some(reason);
        }
        let Some(thread) = self.thread() else {
            return Some("Select a thread first.".into());
        };
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
        self.selected_project = Some(id);
        self.set_setting("selected_project", json!(id));
        if self.thread().is_some_and(|t| t.project_id != id) {
            self.selected_thread = None;
            self.set_setting("selected_thread", Value::Null);
            self.select_launch_output(None);
        }
    }
    pub fn select_thread(&mut self, id: ThreadId) {
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
        self.selected_thread = Some(id);
        self.set_setting("selected_thread", json!(id));
        self.set_setting("selected_project", json!(self.selected_project));
        self.select_launch_output(launch_id);
    }
    pub fn close_thread(&mut self, id: ThreadId) {
        if self.is_conversation(id) && !self.active_threads.contains(&id) {
            if let Some(session) = self.conversations.remove(&id) {
                if let Some(commands) = session.commands {
                    let _ = commands.try_send(ConversationCommand::Close);
                }
            }
        }
        if self.selected_thread == Some(id) {
            self.selected_thread = None;
            self.set_setting("selected_thread", Value::Null);
            self.select_launch_output(None);
        }
    }
    pub fn new_thread(&mut self, kind: ThreadKind) -> Result<()> {
        if let Some(reason) = self.mutable_project_reason() {
            anyhow::bail!(reason);
        }
        let project = self.project().context("Select a project")?;
        let timestamp = platform::now();
        let (title, harness, model, factory) = match kind {
            ThreadKind::Conversation => (
                "New conversation",
                "omp".into(),
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
            draft: String::new(),
            attachments: Vec::new(),
            submission: None,
            archived: false,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        let id = thread.id;
        self.save_thread(thread);
        self.select_thread(id);
        Ok(())
    }
    pub fn register_project(&mut self, path: PathBuf) {
        if self.storage_error.is_some() || self.registering {
            return;
        }
        self.registering = true;
        let sender = self.events.clone();
        smol::spawn(async move {
            let result = smol::unblock(move || {
                platform::inspect_repository(&path).map_err(|e| format!("{e:#}"))
            })
            .await;
            let _ = sender.send(AppEvent::ProjectResolved(result)).await;
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
