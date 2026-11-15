mod activity;
mod artifacts;
pub use artifacts::ArtifactReceipt;
mod bootstrap;
pub use bootstrap::Bootstrap;
mod commit;
mod conversation;
mod events;
pub use events::AppEvent;
mod factory;
mod layout;
pub use layout::{ChangesLayout, SidebarTab};
mod notices;
pub use notices::{NoticeLevel, WorkspaceNotice};
mod projects;
mod remote;
pub use remote::RemoteFileView;
mod network;
pub use network::NetworkState;
mod persistence;
use persistence::Write;
mod project_diff;
mod session;
pub use session::ConversationSession;
mod setup;
mod shutdown;
mod spaces;
mod threads;
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
    /// The Install Pi request whose look for an existing Pi is in flight.
    pi_check: Option<Uuid>,
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
    /// Shared so presentation can hold the page and detect a new one by pointer.
    pub factory_text: Option<Result<Arc<crate::factory::TextPage>, String>>,
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
                if let Some(shutdown) = shutdown
                    && let Err(error) = shutdown.run(true).await
                {
                    eprintln!("Forced platform shutdown: {error:#}");
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
            pi_check: None,
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
}
