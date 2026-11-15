use super::*;
use crate::network::{
    Action, DeliveryBatch, DeliveryOutcome, DeliveryState, Network, NetworkSnapshot, Peer,
};

#[derive(Default)]
pub struct NetworkState {
    pub snapshot: Arc<NetworkSnapshot>,
    pub error: Option<String>,
    pub pending: HashSet<ThreadId>,
    pub revision: u64,
    requests: Option<async_channel::Sender<Request>>,
    pub(super) client: Option<Network>,
    runtimes: HashMap<ThreadId, RuntimeDelivery>,
}

struct RuntimeDelivery {
    generation: Uuid,
    enabled: bool,
    activation: Option<Uuid>,
    claiming: bool,
    lease: Option<DeliveryLease>,
    preparing: Option<Uuid>,
    peer_turn: Option<Uuid>,
}
struct DeliveryLease {
    attempt: Uuid,
    _admission: async_channel::Sender<()>,
}
impl RuntimeDelivery {
    fn new(generation: Uuid) -> Self {
        Self {
            generation,
            enabled: false,
            activation: None,
            claiming: false,
            lease: None,
            preparing: None,
            peer_turn: None,
        }
    }
    fn pause(&mut self) {
        self.enabled = false;
        self.activation = None;
        self.lease = None;
    }
    fn activate(&mut self, activation: Uuid) -> bool {
        if self.activation != Some(activation) {
            return false;
        }
        self.activation = None;
        self.enabled = true;
        true
    }
}

pub(super) struct NetworkDrain(async_channel::Sender<Request>);
impl NetworkDrain {
    pub(super) async fn wait(self) -> Result<()> {
        let (reply, response) = async_channel::bounded(1);
        self.0
            .send(Request::Barrier(reply))
            .await
            .context("Network stopped before quit")?;
        response
            .recv()
            .await
            .context("Network stopped during quit")?
            .map_err(anyhow::Error::msg)
    }
}
impl NetworkState {
    pub(super) fn drain(&self) -> Option<NetworkDrain> {
        self.requests.clone().map(NetworkDrain)
    }
}

enum Request {
    Sync(Vec<Peer>),
    Barrier(async_channel::Sender<std::result::Result<(), String>>),
    Membership {
        peers: Vec<Peer>,
        thread: ThreadId,
        joined: bool,
        activation: Option<(Uuid, Uuid)>,
    },
    Tool {
        peers: Vec<Peer>,
        token: Uuid,
        action: Action,
        reply: std::sync::mpsc::Sender<std::result::Result<Value, String>>,
    },
    Claim {
        peers: Vec<Peer>,
        thread: ThreadId,
        generation: Uuid,
    },
    Finish {
        attempt: Uuid,
        outcome: DeliveryOutcome,
    },
}
enum Update {
    Snapshot(Arc<NetworkSnapshot>),
    Membership {
        thread: ThreadId,
        joined: bool,
        activation: Option<(Uuid, Uuid)>,
        result: std::result::Result<(), String>,
    },
    Claim {
        thread: ThreadId,
        generation: Uuid,
        result: std::result::Result<Option<DeliveryBatch>, String>,
    },
    Failed(String),
}

pub(super) type PeerContext<'a> = (
    &'a ThreadKind,
    bool,
    ProjectId,
    Option<WorktreeId>,
    &'a str,
    &'a Option<RemoteLocation>,
    &'a str,
    &'a str,
    &'a Option<Value>,
);

pub(super) fn peer_context(thread: &Thread) -> PeerContext<'_> {
    (
        &thread.kind,
        thread.archived,
        thread.project_id,
        thread.worktree_id,
        &thread.title,
        &thread.remote,
        &thread.harness,
        &thread.updated_at,
        &thread.submission,
    )
}

impl AppState {
    pub(super) fn start_network(&mut self, cx: &mut Context<Self>) {
        let (network, snapshots) = match Network::start(
            self.store.clone(),
            self.snapshot.settings.get(crate::network::SETTING),
        ) {
            Ok(network) => network,
            Err(error) => {
                self.network.error = Some(format!("Network unavailable: {error:#}"));
                return;
            }
        };
        self.network.client = Some(network.clone());
        let (requests, receiver) = async_channel::bounded(64);
        let (updates, changes) = async_channel::bounded(64);
        self.network.requests = Some(requests);
        let snapshots_changed = updates.clone();
        smol::spawn(async move {
            while let Ok(snapshot) = snapshots.recv().await {
                if snapshots_changed
                    .send(Update::Snapshot(snapshot))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        smol::spawn(async move {
            while let Ok(request) = receiver.recv().await {
                match request {
                    Request::Barrier(reply) => {
                        let _ = reply
                            .send(network.flush().await.map_err(|error| format!("{error:#}")))
                            .await;
                    }
                    Request::Sync(peers) => {
                        if let Err(error) = network.sync(peers).await {
                            let _ = updates
                                .send(Update::Failed(format!("Network unavailable: {error:#}")))
                                .await;
                        }
                    }
                    Request::Membership {
                        peers,
                        thread,
                        joined,
                        activation,
                    } => {
                        let result = async {
                            network.sync(peers).await?;
                            network.set_joined(thread, joined).await
                        }
                        .await
                        .map_err(|error| {
                            format!("Network membership could not be changed: {error:#}")
                        });
                        let _ = updates
                            .send(Update::Membership {
                                thread,
                                joined,
                                activation,
                                result,
                            })
                            .await;
                    }
                    Request::Tool {
                        peers,
                        token,
                        action,
                        reply,
                    } => {
                        let result = async {
                            network.sync(peers).await?;
                            network.call(token, action).await
                        }
                        .await
                        .map_err(|error| format!("{error:#}"));
                        let _ = reply.send(result);
                    }
                    Request::Claim {
                        peers,
                        thread,
                        generation,
                    } => {
                        let result = async {
                            network.sync(peers).await?;
                            network.claim(generation).await
                        }
                        .await
                        .map_err(|error| format!("{error:#}"));
                        let _ = updates
                            .send(Update::Claim {
                                thread,
                                generation,
                                result,
                            })
                            .await;
                    }
                    Request::Finish { attempt, outcome } => {
                        if let Err(error) = network.finish(attempt, outcome).await {
                            let _ = updates
                                .send(Update::Failed(format!(
                                    "Network receipt could not be saved: {error:#}"
                                )))
                                .await;
                        }
                    }
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            while let Ok(update) = changes.recv().await {
                if this
                    .update(cx, |this, cx| {
                        match update {
                            Update::Snapshot(snapshot) => {
                                if let Some(error) = &snapshot.error {
                                    this.network.error = Some(error.clone());
                                    this.pause_all_network_delivery();
                                }
                                this.network.snapshot = snapshot;
                                this.schedule_network_delivery();
                            }
                            Update::Membership {
                                thread,
                                joined,
                                activation,
                                result,
                            } => {
                                this.network.pending.remove(&thread);
                                this.network.error = result.err();
                                if this.network.error.is_none()
                                    && joined
                                    && let Some((generation, activation)) = activation
                                    && this
                                        .conversations
                                        .get(&thread)
                                        .is_some_and(|session| session.generation == generation)
                                    && let Some(runtime) = this
                                        .network
                                        .runtimes
                                        .get_mut(&thread)
                                        .filter(|runtime| runtime.generation == generation)
                                {
                                    runtime.activate(activation);
                                }
                                this.sync_network();
                                this.schedule_network_delivery();
                            }
                            Update::Claim {
                                thread,
                                generation,
                                result,
                            } => this.dispatch_network_claim(thread, generation, result),
                            Update::Failed(error) => {
                                this.network.error = Some(error);
                                this.pause_all_network_delivery();
                            }
                        }
                        this.network.revision = this.network.revision.wrapping_add(1);
                        for thread in &this.snapshot.threads {
                            if thread.kind == ThreadKind::Conversation {
                                cx.emit(PresentationChange::Thread(thread.id));
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        self.sync_network();
    }

    fn runtime_network(&mut self, thread: ThreadId, generation: Uuid) -> &mut RuntimeDelivery {
        let runtime = self
            .network
            .runtimes
            .entry(thread)
            .or_insert_with(|| RuntimeDelivery::new(generation));
        if runtime.generation != generation {
            *runtime = RuntimeDelivery::new(generation);
        }
        runtime
    }

    pub(super) fn pause_network_delivery(&mut self, thread: ThreadId) {
        if let Some(runtime) = self.network.runtimes.get_mut(&thread) {
            runtime.pause();
            if runtime.preparing.take().is_some()
                && runtime.peer_turn.is_none()
                && let Some(session) = self.conversations.get_mut(&thread)
            {
                session.preparing = false;
                if session.state == ConversationState::Ready {
                    self.active_threads.remove(&thread);
                }
            }
        }
    }
    pub(super) fn pause_all_network_delivery(&mut self) {
        let threads: Vec<_> = self.network.runtimes.keys().copied().collect();
        for thread in threads {
            self.pause_network_delivery(thread);
        }
    }

    pub(super) fn authorize_network_user_turn(&mut self, thread: ThreadId) {
        if !self
            .network
            .snapshot
            .members
            .iter()
            .any(|member| member.thread == thread && member.automatic)
        {
            return;
        }
        let Some(generation) = self
            .conversations
            .get(&thread)
            .map(|session| session.generation)
        else {
            return;
        };
        self.runtime_network(thread, generation).enabled = true;
        self.sync_network();
    }

    fn network_peers(&self) -> Vec<Peer> {
        let cutoff = platform::timestamp_before(crate::network::DORMANT_AFTER);
        self.snapshot
            .threads
            .iter()
            .filter_map(|thread| {
                if thread.kind != ThreadKind::Conversation {
                    return None;
                }
                let space = self.thread_space(thread.id)?;
                let project = self
                    .snapshot
                    .projects
                    .iter()
                    .find(|project| project.id == thread.project_id)?;
                let session = self.conversations.get(&thread.id);
                let eligible = !thread.archived
                    && !project.archived
                    && self
                        .thread_worktree(thread)
                        .is_none_or(|worktree| worktree.is_live());
                let (status, token) = match session {
                    Some(session) => {
                        let status = match session.state {
                            ConversationState::Starting => "Starting",
                            ConversationState::Ready => "Idle",
                            ConversationState::Running => "Working",
                            ConversationState::WaitingForInput => "Waiting for input",
                            ConversationState::Interrupting => "Stopping",
                            ConversationState::Disconnected => "Offline",
                            ConversationState::Failed => "Failed",
                        };
                        let token = (!self.shutting_down
                            && self.storage_error.is_none()
                            && !matches!(
                                session.state,
                                ConversationState::Disconnected | ConversationState::Failed
                            ))
                        .then_some(session.generation);
                        (status, token)
                    }
                    None => ("Offline", None),
                };
                Some(Peer {
                    thread: thread.id,
                    space,
                    title: thread.title.clone(),
                    project: project.name.clone(),
                    worktree: match &thread.remote {
                        Some(location) => {
                            format!("remote://{}/{}", location.host, location.project)
                        }
                        None => self
                            .thread_worktree(thread)
                            .map(|worktree| worktree.path.to_string_lossy().into_owned())
                            .unwrap_or_else(|| project.root.to_string_lossy().into_owned()),
                    },
                    harness: thread.harness.clone(),
                    status: status.into(),
                    token,
                    eligible,
                    delivery_enabled: self.network_delivery_enabled(thread.id),
                    can_receive: self.network_can_receive(thread.id),
                    dormant: session.is_none()
                        && !self.active_threads.contains(&thread.id)
                        && !self.thread_open_in(thread.id, space)
                        && thread.updated_at < cutoff,
                })
            })
            .collect()
    }

    pub(super) fn sync_network(&mut self) {
        if self.shutting_down || self.storage_error.is_some() {
            self.pause_all_network_delivery();
        }
        let revoked: Vec<_> = self
            .network
            .runtimes
            .iter()
            .filter_map(|(thread, runtime)| {
                let live = self.conversations.get(thread).is_some_and(|session| {
                    session.generation == runtime.generation
                        && !matches!(
                            session.state,
                            ConversationState::Disconnected
                                | ConversationState::Failed
                                | ConversationState::Interrupting
                        )
                });
                let same_space = self
                    .network
                    .snapshot
                    .members
                    .iter()
                    .find(|member| member.thread == *thread)
                    .is_none_or(|member| self.thread_in_space(*thread, member.space));
                (!live
                    || !self.open_threads.contains(thread)
                    || !self.network_context_eligible(*thread)
                    || !same_space)
                    .then_some(*thread)
            })
            .collect();
        for thread in revoked {
            self.pause_network_delivery(thread);
        }
        let Some(requests) = &self.network.requests else {
            return;
        };
        if let Err(error) = requests.try_send(Request::Sync(self.network_peers())) {
            self.network.error = Some(format!(
                "Network update could not be queued: {error}. Reopen the application to reconnect."
            ));
            self.pause_all_network_delivery();
        }
    }

    pub fn network_joined(&self, thread: ThreadId) -> bool {
        self.network
            .snapshot
            .members
            .iter()
            .any(|member| member.thread == thread && self.thread_in_space(thread, member.space))
    }
    fn network_context_eligible(&self, thread: ThreadId) -> bool {
        self.snapshot
            .threads
            .iter()
            .find(|candidate| candidate.id == thread)
            .is_some_and(|candidate| {
                candidate.kind == ThreadKind::Conversation
                    && !candidate.archived
                    && self
                        .snapshot
                        .projects
                        .iter()
                        .any(|project| project.id == candidate.project_id && !project.archived)
                    && self
                        .thread_worktree(candidate)
                        .is_none_or(|worktree| worktree.is_live())
            })
    }
    pub fn network_delivery_enabled(&self, thread: ThreadId) -> bool {
        if self.shutting_down
            || self.storage_error.is_some()
            || self.network.snapshot.error.is_some()
            || !self.open_threads.contains(&thread)
            || !self.network_context_eligible(thread)
        {
            return false;
        }
        let Some(session) = self.conversations.get(&thread) else {
            return false;
        };
        session.peer_delivery
            && !matches!(
                session.state,
                ConversationState::Disconnected
                    | ConversationState::Failed
                    | ConversationState::Interrupting
            )
            && self
                .network
                .runtimes
                .get(&thread)
                .is_some_and(|runtime| runtime.generation == session.generation && runtime.enabled)
            && self.network.snapshot.members.iter().any(|member| {
                member.thread == thread
                    && member.automatic
                    && self.thread_in_space(thread, member.space)
            })
    }
    pub(super) fn network_delivery_in_flight(&self, thread: ThreadId) -> bool {
        self.network.runtimes.get(&thread).is_some_and(|runtime| {
            runtime.claiming || runtime.lease.is_some() || runtime.preparing.is_some()
        })
    }
    fn network_can_receive(&self, thread: ThreadId) -> bool {
        self.network_delivery_enabled(thread)
            && !self.artifact_delivery_in_flight(thread)
            && self.conversations.get(&thread).is_some_and(|session| {
                session.persisted
                    && !session.preparing
                    && session.requests.is_empty()
                    && (session.state == ConversationState::Ready
                        || (session.state == ConversationState::Running
                            && !self.snapshot.threads.iter().any(|candidate| {
                                candidate.id == thread && candidate.remote.is_some()
                            })))
                    && (!self
                        .snapshot
                        .threads
                        .iter()
                        .any(|candidate| candidate.id == thread && candidate.submission.is_some())
                        || self.active_threads.contains(&thread))
            })
    }

    pub fn network_delivery_reason(&self, thread: ThreadId) -> Option<String> {
        if let Some(reason) = self.network_membership_reason(thread) {
            return Some(reason);
        }
        if !self.network_joined(thread) {
            return Some("Join the network to receive peer messages".into());
        }
        if let Some(reason) = self.network_runtime_reason(thread) {
            return Some(reason);
        }
        if !self
            .network
            .snapshot
            .members
            .iter()
            .any(|member| member.thread == thread && member.automatic)
        {
            return Some("Automatic delivery needs your approval; Resume lets peer messages start turns and use tokens".into());
        }
        if !self.network_delivery_enabled(thread) {
            return Some(
                "Automatic delivery is paused; Resume or send a prompt to enable this runtime"
                    .into(),
            );
        }
        let session = &self.conversations[&thread];
        if !session.requests.is_empty() || session.state == ConversationState::WaitingForInput {
            return Some("Queued until the agent's request for your input is resolved".into());
        }
        if session.preparing {
            return Some("Waiting for the current turn to be admitted".into());
        }
        None
    }

    fn network_runtime_reason(&self, thread: ThreadId) -> Option<String> {
        if !self.open_threads.contains(&thread) {
            return Some("Open this Thread before enabling automatic delivery".into());
        }
        let Some(session) = self.conversations.get(&thread) else {
            return Some("Open the agent before enabling automatic delivery".into());
        };
        if !session.peer_delivery {
            return Some(session.peer_delivery_reason.clone().unwrap_or_else(|| {
                "The native peer bridge is unavailable; reconnect the agent to retry".into()
            }));
        }
        if session.commands.is_none()
            || matches!(
                session.state,
                ConversationState::Starting
                    | ConversationState::Disconnected
                    | ConversationState::Failed
                    | ConversationState::Interrupting
            )
        {
            return Some(
                "The agent is not ready to receive messages; open or reconnect it first".into(),
            );
        }
        if !session.persisted {
            return Some("Saving the provider session reference".into());
        }
        if self
            .snapshot
            .threads
            .iter()
            .any(|candidate| candidate.id == thread && candidate.submission.is_some())
            && !self.active_threads.contains(&thread)
            && !session.preparing
        {
            return Some(
                "Review the previous submitted Draft before enabling automatic delivery".into(),
            );
        }
        None
    }

    pub fn network_membership_reason(&self, thread: ThreadId) -> Option<String> {
        if self.network.requests.is_none() {
            return Some(
                self.network
                    .error
                    .clone()
                    .unwrap_or_else(|| "Network is unavailable".into()),
            );
        }
        if let Some(error) = &self.network.snapshot.error {
            return Some(format!(
                "Network is disabled after a persistence failure: {error}"
            ));
        }
        if let Some(reason) = self.space_mutation_reason() {
            return Some(reason);
        }
        if self.network.pending.contains(&thread) {
            return Some("Network membership is being saved".into());
        }
        let Some(candidate) = self
            .snapshot
            .threads
            .iter()
            .find(|candidate| candidate.id == thread)
        else {
            return Some("Select a Conversation first".into());
        };
        if candidate.kind != ThreadKind::Conversation {
            return Some("Select a Conversation first".into());
        }
        (candidate.archived
            || self
                .snapshot
                .projects
                .iter()
                .find(|project| project.id == candidate.project_id)
                .is_none_or(|project| project.archived)
            || self
                .thread_worktree(candidate)
                .is_some_and(|worktree| !worktree.is_live()))
        .then(|| "Archived Threads and removed worktrees cannot join the network".into())
    }
    pub fn network_join_reason(&self, thread: ThreadId) -> Option<String> {
        self.network_membership_reason(thread).or_else(|| {
            if self.network_joined(thread) {
                Some("This agent has already joined the Space network".into())
            } else if self.browser_tools.is_none() {
                Some("Agent tool bridge is unavailable; reopen the application to reconnect".into())
            } else {
                self.network_runtime_reason(thread)
            }
        })
    }
    pub fn network_resume_reason(&self, thread: ThreadId) -> Option<String> {
        self.network_membership_reason(thread).or_else(|| {
            if !self.network_joined(thread) {
                Some("Join the Space network first".into())
            } else if self.network_delivery_enabled(thread) {
                Some("Automatic network delivery is already enabled".into())
            } else {
                self.network_runtime_reason(thread)
            }
        })
    }
    pub fn resume_network_delivery(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        if let Some(reason) = self.network_resume_reason(thread) {
            self.network.error = Some(reason);
            cx.emit(PresentationChange::Thread(thread));
        } else {
            self.set_network_joined(thread, true, cx);
        }
    }

    pub fn set_network_joined(&mut self, thread: ThreadId, joined: bool, cx: &mut Context<Self>) {
        let reason = if joined && self.network_joined(thread) {
            self.network_resume_reason(thread)
        } else if joined {
            self.network_join_reason(thread)
        } else {
            self.network_membership_reason(thread)
        };
        if let Some(reason) = reason {
            self.network.error = Some(reason);
            cx.emit(PresentationChange::Thread(thread));
            return;
        }
        let activation = if joined {
            self.conversations
                .get(&thread)
                .map(|session| session.generation)
                .map(|generation| {
                    let activation = Uuid::new_v4();
                    self.runtime_network(thread, generation).activation = Some(activation);
                    (generation, activation)
                })
        } else {
            self.pause_network_delivery(thread);
            None
        };
        let request = Request::Membership {
            peers: self.network_peers(),
            thread,
            joined,
            activation,
        };
        if let Some(requests) = &self.network.requests {
            match requests.try_send(request) {
                Ok(()) => {
                    self.network.error = None;
                    self.network.pending.insert(thread);
                }
                Err(error) => {
                    self.pause_network_delivery(thread);
                    self.network.error =
                        Some(format!("Network membership could not be queued: {error}"));
                }
            }
        }
        cx.emit(PresentationChange::Thread(thread));
    }

    pub(super) fn schedule_network_delivery(&mut self) {
        let recipients: HashSet<_> = self
            .network
            .snapshot
            .messages
            .iter()
            .flat_map(|message| &message.deliveries)
            .filter(|delivery| matches!(delivery.state, DeliveryState::Queued))
            .map(|delivery| delivery.recipient)
            .collect();
        for thread in recipients {
            if !self.network_can_receive(thread) {
                continue;
            }
            let Some(generation) = self
                .conversations
                .get(&thread)
                .map(|session| session.generation)
            else {
                continue;
            };
            let runtime = self.runtime_network(thread, generation);
            if runtime.claiming || runtime.lease.is_some() {
                continue;
            }
            runtime.claiming = true;
            let request = Request::Claim {
                peers: self.network_peers(),
                thread,
                generation,
            };
            if self
                .network
                .requests
                .as_ref()
                .is_none_or(|requests| requests.try_send(request).is_err())
            {
                if let Some(runtime) = self.network.runtimes.get_mut(&thread) {
                    runtime.claiming = false;
                }
                self.network.error =
                    Some("Network delivery queue is unavailable; Resume after reconnecting".into());
                self.pause_network_delivery(thread);
            }
        }
    }

    fn dispatch_network_claim(
        &mut self,
        thread: ThreadId,
        generation: Uuid,
        result: std::result::Result<Option<DeliveryBatch>, String>,
    ) {
        if let Some(runtime) = self
            .network
            .runtimes
            .get_mut(&thread)
            .filter(|runtime| runtime.generation == generation)
        {
            runtime.claiming = false;
        }
        let batch = match result {
            Ok(Some(batch)) => batch,
            Ok(None) => return,
            Err(error) => {
                self.network.error = Some(error);
                self.pause_network_delivery(thread);
                return;
            }
        };
        if !self.network_delivery_enabled(thread)
            || !self
                .conversations
                .get(&thread)
                .is_some_and(|session| session.generation == generation)
        {
            self.finish_network_delivery(
                batch.attempt,
                DeliveryOutcome::Failed("Delivery revoked before dispatch to the runtime".into()),
            );
            return;
        }
        if !self.network_can_receive(thread) {
            self.finish_network_delivery(batch.attempt, DeliveryOutcome::Deferred);
            return;
        }
        let Some(commands) = self
            .conversations
            .get(&thread)
            .and_then(|session| session.commands.clone())
        else {
            self.finish_network_delivery(
                batch.attempt,
                DeliveryOutcome::Failed("Agent disconnected before delivery".into()),
            );
            return;
        };
        let (admission, cancelled) = async_channel::bounded(1);
        let attempt = batch.attempt;
        let messages = batch
            .messages
            .into_iter()
            .map(|message| harness::PeerMessage {
                id: message.id,
                sender: message.sender,
                sender_name: message.sender_name,
                space: message.space,
                body: message.body,
                origin: harness::ContextOrigin::Peer,
            })
            .collect();
        let idle = !self.active_threads.contains(&thread);
        let runtime = self.runtime_network(thread, generation);
        runtime.lease = Some(DeliveryLease {
            attempt,
            _admission: admission,
        });
        if idle {
            runtime.preparing = Some(attempt);
        }
        if idle {
            if let Some(session) = self.conversations.get_mut(&thread) {
                session.preparing = true;
            }
            self.active_threads.insert(thread);
        }
        let Some(mut owner) = self
            .snapshot
            .threads
            .iter()
            .find(|candidate| candidate.id == thread)
            .cloned()
        else {
            return;
        };
        owner.execution_started = true;
        let saved = self.persist_thread(owner);
        let events = self.events.clone();
        smol::spawn(async move {
            let result = async {
                saved
                    .recv()
                    .await
                    .context("Persistence owner closed")?
                    .map_err(anyhow::Error::msg)?;
                anyhow::ensure!(
                    !cancelled.is_closed(),
                    "Peer delivery admission was revoked"
                );
                commands
                    .try_send(ConversationCommand::DeliverPeer(harness::PeerDelivery {
                        attempt,
                        messages,
                        cancelled,
                    }))
                    .context("Agent command queue refused delivery")?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                let _ = events
                    .send(AppEvent::Harness(
                        thread,
                        generation,
                        ConversationEvent::PeerDelivery {
                            attempt,
                            outcome: harness::PeerDeliveryOutcome::Rejected(format!("{error:#}")),
                        },
                    ))
                    .await;
            }
        })
        .detach();
    }

    fn clear_network_preparing(&mut self, thread: ThreadId, attempt: Uuid) {
        if let Some(runtime) = self.network.runtimes.get_mut(&thread)
            && runtime.preparing == Some(attempt)
        {
            runtime.preparing = None;
            if runtime.peer_turn.is_none()
                && let Some(session) = self.conversations.get_mut(&thread)
            {
                session.preparing = false;
                if session.state == ConversationState::Ready {
                    self.active_threads.remove(&thread);
                }
            }
        }
    }
    fn finish_network_delivery(&mut self, attempt: Uuid, outcome: DeliveryOutcome) {
        if let Some(requests) = &self.network.requests
            && let Err(error) = requests.try_send(Request::Finish { attempt, outcome })
        {
            self.network.error = Some(format!("Network receipt could not be queued: {error}"));
            self.pause_all_network_delivery();
        }
    }

    pub(super) fn network_delivery_event(
        &mut self,
        thread: ThreadId,
        attempt: Uuid,
        outcome: harness::PeerDeliveryOutcome,
    ) {
        let current = self.network.runtimes.get(&thread).is_some_and(|runtime| {
            runtime
                .lease
                .as_ref()
                .is_some_and(|lease| lease.attempt == attempt)
                || runtime.preparing == Some(attempt)
                || runtime.peer_turn == Some(attempt)
        });
        if let Some(runtime) = self.network.runtimes.get_mut(&thread)
            && runtime
                .lease
                .as_ref()
                .is_some_and(|lease| lease.attempt == attempt)
        {
            runtime.lease = None;
        }
        let outcome = match outcome {
            harness::PeerDeliveryOutcome::Accepted => {
                if self
                    .conversations
                    .get(&thread)
                    .is_some_and(|session| session.state == ConversationState::Running)
                {
                    self.clear_network_preparing(thread, attempt);
                }
                DeliveryOutcome::Delivered
            }
            harness::PeerDeliveryOutcome::Rejected(reason) => {
                self.clear_network_preparing(thread, attempt);
                if current {
                    self.pause_network_delivery(thread);
                }
                DeliveryOutcome::Failed(reason)
            }
            harness::PeerDeliveryOutcome::Unknown(reason) => {
                self.clear_network_preparing(thread, attempt);
                if current {
                    self.pause_network_delivery(thread);
                }
                DeliveryOutcome::Unknown(reason)
            }
        };
        self.finish_network_delivery(attempt, outcome);
    }
    pub(super) fn network_peer_turn_started(&mut self, thread: ThreadId, request_id: Uuid) {
        let Some(generation) = self
            .conversations
            .get(&thread)
            .map(|session| session.generation)
        else {
            return;
        };
        if !self.network_delivery_enabled(thread) {
            self.send_command(thread, ConversationCommand::Interrupt);
            self.clear_network_preparing(thread, request_id);
            return;
        }
        let runtime = self.runtime_network(thread, generation);
        runtime.preparing = None;
        runtime.peer_turn = Some(request_id);
        if let Some(session) = self.conversations.get_mut(&thread) {
            session.preparing = false;
            session.outcome = None;
        }
        self.active_threads.insert(thread);
    }
    pub(super) fn network_has_other_turn(&self, thread: ThreadId, settled: Uuid) -> bool {
        self.network.runtimes.get(&thread).is_some_and(|runtime| {
            runtime.peer_turn.is_some_and(|turn| turn != settled)
                || runtime.preparing.is_some_and(|turn| turn != settled)
        })
    }
    pub(super) fn network_peer_turn_settled(
        &mut self,
        thread: ThreadId,
        request_id: Uuid,
        outcome: &TurnOutcome,
    ) {
        if let Some(runtime) = self.network.runtimes.get_mut(&thread) {
            if runtime.peer_turn == Some(request_id) {
                runtime.peer_turn = None;
            }
            if runtime.preparing == Some(request_id) {
                runtime.preparing = None;
            }
        }
        if *outcome != TurnOutcome::Completed {
            self.pause_network_delivery(thread);
        }
    }

    pub(crate) fn network_tool(
        &mut self,
        thread: ThreadId,
        token: Uuid,
        action: Action,
        reply: std::sync::mpsc::Sender<std::result::Result<Value, String>>,
    ) {
        if self.shutting_down || self.storage_error.is_some() {
            let _ = reply.send(Err(
                "Network is unavailable while shutting down or storage is read-only".into(),
            ));
            return;
        }
        if !self
            .conversations
            .get(&thread)
            .is_some_and(|session| session.generation == token)
        {
            let _ = reply.send(Err(
                "Network capability no longer belongs to this Conversation runtime".into(),
            ));
            return;
        }
        let Some(requests) = &self.network.requests else {
            let _ = reply.send(Err(self
                .network
                .error
                .clone()
                .unwrap_or_else(|| "Network is unavailable".into())));
            return;
        };
        if let Err(error) = requests.try_send(Request::Tool {
            peers: self.network_peers(),
            token,
            action,
            reply: reply.clone(),
        }) {
            let _ = reply.send(Err(format!(
                "Network request queue is unavailable: {error}"
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeDelivery;
    use uuid::Uuid;

    #[test]
    fn stopping_before_opt_in_commit_cannot_reenable_automatic_turns() {
        let mut runtime = RuntimeDelivery::new(Uuid::new_v4());
        let activation = Uuid::new_v4();
        runtime.activation = Some(activation);
        runtime.pause();
        assert!(!runtime.activate(activation));
        assert!(!runtime.enabled);
        let renewed = Uuid::new_v4();
        runtime.activation = Some(renewed);
        assert!(!runtime.activate(activation));
        assert!(runtime.activate(renewed));
        assert!(runtime.enabled);
    }

    #[test]
    fn stopping_revokes_the_pending_native_admission_lease() {
        let mut runtime = RuntimeDelivery::new(Uuid::new_v4());
        let (admission, cancelled) = async_channel::bounded(1);
        runtime.enabled = true;
        runtime.lease = Some(super::DeliveryLease {
            attempt: Uuid::new_v4(),
            _admission: admission,
        });
        runtime.pause();
        assert!(cancelled.is_closed());
        assert!(!runtime.enabled);
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn only_a_closed_runtimeless_inactive_conversation_is_dormant(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.dispatcher.allow_parking();
        use super::*;

        let directory = std::env::temp_dir().join(format!("drost-dormant-{}", Uuid::new_v4()));
        let store = Store::open(directory.join("data")).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Dormant".into(),
            root: directory.join("repo"),
            archived: false,
            defaults: ProjectDefaults::default(),
        };
        let project_id = project.id;
        smol::block_on(store.save_project(project)).unwrap();
        let snapshot = smol::block_on(store.snapshot()).unwrap();
        let state = cx.new(|cx| {
            AppState::new(
                Bootstrap {
                    data_dir: directory.clone(),
                    store,
                    snapshot,
                    diagnostics: BTreeMap::new(),
                    repositories: BTreeMap::new(),
                    browser_tools: None,
                },
                cx,
            )
        });
        state.update(cx, |state, _| {
            let dormant = |state: &AppState, id| {
                state
                    .network_peers()
                    .into_iter()
                    .find(|peer| peer.thread == id)
                    .unwrap()
                    .dormant
            };
            let touched = |state: &mut AppState, id, at: &str| {
                let thread = state.snapshot.threads.iter_mut();
                thread.filter(|t| t.id == id).for_each(|t| {
                    t.updated_at = at.into();
                });
            };
            state.select_project(project_id);
            state.new_thread(ThreadKind::Conversation).unwrap();
            let id = state.selected_thread.unwrap();
            let old = "2000-01-01T00:00:00Z";
            touched(state, id, old);
            assert!(!dormant(state, id), "An open tab keeps its member");

            state.close_thread(id);
            assert!(dormant(state, id));

            touched(state, id, &platform::now());
            assert!(!dormant(state, id), "Recent activity keeps its member");
            touched(state, id, old);

            state
                .conversations
                .insert(id, ConversationSession::new(None));
            assert!(!dormant(state, id), "A live runtime keeps its member");
            state.conversations.remove(&id);

            state.active_threads.insert(id);
            assert!(!dormant(state, id), "An unresolved turn keeps its member");
            state.active_threads.remove(&id);
            assert!(dormant(state, id));

            state.select_thread(id);
            let other = state.create_space("Elsewhere").unwrap();
            state.switch_space(other).unwrap();
            assert!(!state.open_threads.contains(&id));
            assert!(
                !dormant(state, id),
                "A tab in an inactive Space keeps its member"
            );
        });
        let _ = std::fs::remove_dir_all(directory);
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn harness_updates_that_save_the_thread_keep_automatic_delivery(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.dispatcher.allow_parking();
        use super::*;

        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory =
            Directory(std::env::temp_dir().join(format!("drost-saves-{}", Uuid::new_v4())));
        let store = Store::open(directory.0.join("data")).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Saves".into(),
            root: directory.0.join("repo"),
            archived: false,
            defaults: ProjectDefaults::default(),
        };
        let project_id = project.id;
        smol::block_on(store.save_project(project)).unwrap();
        let snapshot = smol::block_on(store.snapshot()).unwrap();
        let state = cx.new(|cx| {
            AppState::new(
                Bootstrap {
                    data_dir: directory.0.clone(),
                    store,
                    snapshot,
                    diagnostics: BTreeMap::new(),
                    repositories: BTreeMap::new(),
                    browser_tools: None,
                },
                cx,
            )
        });
        state.update(cx, |state, cx| {
            state.select_project(project_id);
            state.new_thread(ThreadKind::Conversation).unwrap();
            let thread = state.selected_thread.unwrap();
            let mut session = ConversationSession::new(None);
            let generation = session.generation;
            let (commands, _native) = async_channel::bounded(8);
            session.commands = Some(commands);
            session.state = ConversationState::Ready;
            session.persisted = true;
            session.peer_delivery = true;
            state.conversations.insert(thread, session);
            state.runtime_network(thread, generation).enabled = true;
            let peer = state
                .network_peers()
                .into_iter()
                .find(|peer| peer.thread == thread)
                .unwrap();
            let mut member = crate::network::Member::from(&peer);
            member.automatic = true;
            Arc::make_mut(&mut state.network.snapshot)
                .members
                .push(member);
            assert!(state.network_delivery_enabled(thread));

            let request_id = Uuid::new_v4();
            state
                .snapshot
                .threads
                .iter_mut()
                .find(|candidate| candidate.id == thread)
                .unwrap()
                .submission = Some(serde_json::json!({
                "request_id": request_id.to_string(),
                "state": "accepted",
            }));
            for (name, event) in [
                (
                    "title",
                    ConversationEvent::TitleChanged("Named by the harness".into()),
                ),
                (
                    "model",
                    ConversationEvent::ModelSelected(ModelKey {
                        provider: "provider".into(),
                        id: "model".into(),
                    }),
                ),
                (
                    "settled",
                    ConversationEvent::TurnSettled {
                        request_id,
                        outcome: TurnOutcome::Completed,
                    },
                ),
            ] {
                state.receive_conversation(thread, event, cx);
                assert!(
                    state.network_delivery_enabled(thread),
                    "{name}: saving the thread paused automatic delivery"
                );
            }
            let saved = state
                .snapshot
                .threads
                .iter()
                .find(|candidate| candidate.id == thread)
                .unwrap();
            assert_eq!(saved.title, "Named by the harness");
            assert_eq!(saved.model.as_ref().unwrap().id, "model");
            assert!(saved.submission.is_none(), "the settled turn was saved");
        });
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn saving_a_thread_syncs_the_network_only_when_its_peer_context_changed(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.dispatcher.allow_parking();
        use super::*;

        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory =
            Directory(std::env::temp_dir().join(format!("drost-context-{}", Uuid::new_v4())));
        let store = Store::open(directory.0.join("data")).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Context".into(),
            root: directory.0.join("repo"),
            archived: false,
            defaults: ProjectDefaults::default(),
        };
        let project_id = project.id;
        smol::block_on(store.save_project(project)).unwrap();
        let snapshot = smol::block_on(store.snapshot()).unwrap();
        let state = cx.new(|cx| {
            AppState::new(
                Bootstrap {
                    data_dir: directory.0.clone(),
                    store,
                    snapshot,
                    diagnostics: BTreeMap::new(),
                    repositories: BTreeMap::new(),
                    browser_tools: None,
                },
                cx,
            )
        });
        state.update(cx, |state, _| {
            state.select_project(project_id);
            let (requests, work) = async_channel::bounded(64);
            state.network.requests = Some(requests);
            let syncs = |work: &async_channel::Receiver<Request>| {
                std::iter::from_fn(|| work.try_recv().ok())
                    .filter(|request| matches!(request, Request::Sync(_)))
                    .count()
            };

            state.new_thread(ThreadKind::Conversation).unwrap();
            assert!(syncs(&work) > 0, "a new thread is a new peer");
            let thread = state.thread().unwrap().clone();

            let mut quiet = thread.clone();
            quiet.model = Some(ModelKey {
                provider: "provider".into(),
                id: "model".into(),
            });
            quiet.effort = Some("high".into());
            quiet.permission_mode = Some("plan".into());
            quiet.session_ref = Some("session".into());
            quiet.draft = "draft".into();
            quiet.execution_started = true;
            assert_eq!(peer_context(&quiet), peer_context(&thread));
            state.save_thread(quiet.clone());
            assert_eq!(syncs(&work), 0, "an unchanged peer context needs no sync");
            assert_eq!(
                state.thread().unwrap().model,
                quiet.model,
                "the save still applied"
            );

            type Change = (&'static str, fn(&mut Thread));
            let changes: [Change; 7] = [
                ("title", |t| t.title = "Renamed".into()),
                ("harness", |t| t.harness = "codex".into()),
                ("updated_at", |t| {
                    t.updated_at = "2999-01-01T00:00:00Z".into()
                }),
                ("submission", |t| {
                    t.submission = Some(serde_json::json!({"state": "pending"}))
                }),
                ("worktree", |t| t.worktree_id = Some(Uuid::new_v4())),
                ("remote", |t| {
                    t.remote = Some(RemoteLocation {
                        host: "host".into(),
                        project: "mounted".into(),
                    })
                }),
                ("archived", |t| t.archived = true),
            ];
            for (field, change) in changes {
                let mut changed = state.thread().unwrap().clone();
                change(&mut changed);
                state.save_thread(changed);
                assert!(syncs(&work) > 0, "{field} reaches the network");
            }
        });
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn claimed_messages_wait_when_native_readiness_changes(cx: &mut gpui_kit::TestAppContext) {
        cx.dispatcher.allow_parking();
        use super::*;

        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory =
            Directory(std::env::temp_dir().join(format!("drost-network-{}", Uuid::new_v4())));
        let store = Store::open(directory.0.join("data")).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Readiness".into(),
            root: directory.0.join("repo"),
            archived: false,
            defaults: ProjectDefaults::default(),
        };
        let project_id = project.id;
        smol::block_on(store.save_project(project)).unwrap();
        let snapshot = smol::block_on(store.snapshot()).unwrap();
        let state = cx.new(|cx| {
            AppState::new(
                Bootstrap {
                    data_dir: directory.0.clone(),
                    store,
                    snapshot,
                    diagnostics: BTreeMap::new(),
                    repositories: BTreeMap::new(),
                    browser_tools: None,
                },
                cx,
            )
        });
        state.update(cx, |state, cx| {
            state.select_project(project_id);
            state.new_thread(ThreadKind::Conversation).unwrap();
            let thread = state.selected_thread.unwrap();
            let mut session = ConversationSession::new(None);
            let generation = session.generation;
            let (commands, native) = async_channel::bounded(8);
            session.commands = Some(commands);
            session.state = ConversationState::Ready;
            session.persisted = true;
            session.peer_delivery = true;
            state.conversations.insert(thread, session);
            state.runtime_network(thread, generation).enabled = true;
            let peer = state
                .network_peers()
                .into_iter()
                .find(|peer| peer.thread == thread)
                .unwrap();
            let mut member = crate::network::Member::from(&peer);
            member.automatic = true;
            Arc::make_mut(&mut state.network.snapshot)
                .members
                .push(member);
            let (requests, work) = async_channel::bounded(8);
            state.network.requests = Some(requests);
            let (mailbox, snapshots) = Network::start(state.store.clone(), None).unwrap();
            smol::block_on(async {
                mailbox.sync(state.network_peers()).await.unwrap();
                mailbox.set_joined(thread, true).await.unwrap();
                mailbox
                    .call(
                        generation,
                        Action::Send {
                            recipient: thread,
                            body: "Keep this peer message queued while the user decides".into(),
                        },
                    )
                    .await
                    .unwrap();
            });

            for blocker in [
                "permission",
                "input",
                "preparing",
                "unsaved",
                "submission",
                "remote-running",
            ] {
                assert!(state.network_can_receive(thread));
                state.runtime_network(thread, generation).claiming = true;
                let batch = smol::block_on(mailbox.claim(generation)).unwrap().unwrap();
                let attempt = batch.attempt;
                let session = state.conversations.get_mut(&thread).unwrap();
                match blocker {
                    "permission" => {
                        session.requests.insert(
                            "permission".into(),
                            UserRequest {
                                id: "permission".into(),
                                kind: harness::UserRequestKind::Permission,
                                title: "Approve tool".into(),
                                options: Vec::new(),
                                text: "The user must decide".into(),
                                placeholder: None,
                                deadline: None,
                            },
                        );
                    }
                    "input" => session.state = ConversationState::WaitingForInput,
                    "preparing" => session.preparing = true,
                    "remote-running" => {
                        session.state = ConversationState::Running;
                        state
                            .snapshot
                            .threads
                            .iter_mut()
                            .find(|candidate| candidate.id == thread)
                            .unwrap()
                            .remote = Some(RemoteLocation {
                            host: "host".into(),
                            project: "mounted".into(),
                        });
                    }
                    "unsaved" => session.persisted = false,
                    "submission" => {
                        state
                            .snapshot
                            .threads
                            .iter_mut()
                            .find(|candidate| candidate.id == thread)
                            .unwrap()
                            .submission = Some(serde_json::json!({"state":"pending"}));
                    }
                    _ => unreachable!(),
                }
                state.dispatch_network_claim(thread, generation, Ok(Some(batch)));
                assert!(
                    native.is_empty(),
                    "{blocker}: no native write before readiness"
                );
                let Request::Finish {
                    attempt: actual,
                    outcome,
                } = work.try_recv().unwrap()
                else {
                    panic!("{blocker}: the unwritten claim must return to the mailbox");
                };
                assert_eq!(actual, attempt);
                assert_eq!(outcome, DeliveryOutcome::Deferred, "{blocker}");
                let runtime = &state.network.runtimes[&thread];
                assert!(!runtime.claiming);
                assert!(runtime.lease.is_none());
                assert!(runtime.preparing.is_none());
                assert!(!state.active_threads.contains(&thread));
                smol::block_on(mailbox.finish(actual, outcome)).unwrap();
                let queued = snapshots.try_recv().unwrap();
                assert_eq!(
                    queued.messages[0].deliveries[0].state,
                    DeliveryState::Queued,
                    "{blocker}"
                );
                assert_eq!(queued.messages[0].deliveries[0].attempt, None, "{blocker}");
                let session = state.conversations.get_mut(&thread).unwrap();
                session.requests.clear();
                session.state = ConversationState::Ready;
                session.preparing = false;
                session.persisted = true;
                state
                    .snapshot
                    .threads
                    .iter_mut()
                    .find(|candidate| candidate.id == thread)
                    .unwrap()
                    .submission = None;
                if blocker == "remote-running" {
                    assert!(
                        state.network_can_receive(thread),
                        "Remote messages become eligible only after returning to Ready"
                    );
                    state
                        .snapshot
                        .threads
                        .iter_mut()
                        .find(|candidate| candidate.id == thread)
                        .unwrap()
                        .remote = None;
                    state.conversations.get_mut(&thread).unwrap().state =
                        ConversationState::Running;
                    assert!(
                        state.network_can_receive(thread),
                        "Local runtimes retain busy-turn peer delivery"
                    );
                    state.conversations.get_mut(&thread).unwrap().state = ConversationState::Ready;
                }
            }
            state.conversations.get_mut(&thread).unwrap().state = ConversationState::Running;
            assert!(
                state.network_can_receive(thread),
                "Busy-turn injection needs no terminal event"
            );
            state.network.runtimes.get_mut(&thread).unwrap().enabled = false;
            assert!(
                !state.network_can_receive(thread),
                "A stopped runtime cannot be woken"
            );
            state.network.runtimes.get_mut(&thread).unwrap().enabled = true;
            Arc::make_mut(&mut state.network.snapshot).members[0].automatic = false;
            assert!(
                !state.network_can_receive(thread),
                "Membership without wake consent is insufficient"
            );
            Arc::make_mut(&mut state.network.snapshot).members[0].automatic = true;
            state.network.runtimes.get_mut(&thread).unwrap().generation = Uuid::new_v4();
            assert!(
                !state.network_can_receive(thread),
                "Old runtime authorization cannot cross a generation"
            );
            state.network.runtimes.get_mut(&thread).unwrap().generation = generation;
            let membership = Arc::make_mut(&mut state.network.snapshot)
                .members
                .pop()
                .unwrap();
            assert!(
                !state.network_can_receive(thread),
                "Leaving revokes busy-turn injection"
            );
            Arc::make_mut(&mut state.network.snapshot)
                .members
                .push(membership);
            state.network.runtimes.get_mut(&thread).unwrap().pause();
            assert!(
                !state.network_can_receive(thread),
                "A new incoming message cannot reactivate Stop"
            );
            state.authorize_network_user_turn(thread);
            assert!(
                state.network_can_receive(thread),
                "Explicit user work can reactivate prior consent"
            );
            while work.try_recv().is_ok() {}
            state.conversations.get_mut(&thread).unwrap().state = ConversationState::Ready;
            let batch = smol::block_on(mailbox.claim(generation)).unwrap().unwrap();
            let attempt = batch.attempt;
            state.dispatch_network_claim(thread, generation, Ok(Some(batch)));
            assert!(state.conversations[&thread].preparing);
            assert!(state.active_threads.contains(&thread));
            state.conversations.get_mut(&thread).unwrap().state = ConversationState::Running;
            state.network_delivery_event(thread, attempt, harness::PeerDeliveryOutcome::Accepted);
            assert!(
                !state.conversations[&thread].preparing,
                "Busy native delivery needs no new turn admission"
            );
            assert!(state.network.runtimes[&thread].preparing.is_none());
            assert!(
                state.active_threads.contains(&thread),
                "The existing native turn stays active"
            );
            state.network_peer_turn_started(thread, attempt);
            state.receive_conversation(
                thread,
                ConversationEvent::UserRequest(UserRequest {
                    id: "peer-permission".into(),
                    kind: harness::UserRequestKind::Permission,
                    title: "Peer turn permission".into(),
                    options: Vec::new(),
                    text: "Only the user may decide".into(),
                    placeholder: None,
                    deadline: None,
                }),
                cx,
            );
            state.receive_conversation(
                thread,
                ConversationEvent::TurnSettled {
                    request_id: Uuid::new_v4(),
                    outcome: TurnOutcome::Completed,
                },
                cx,
            );
            assert!(
                state.active_threads.contains(&thread),
                "An earlier turn cannot settle the native peer turn"
            );
            assert!(
                state.conversations[&thread]
                    .requests
                    .contains_key("peer-permission")
            );
            assert!(state.conversations[&thread].outcome.is_none());
            state.receive_conversation(
                thread,
                ConversationEvent::TurnSettled {
                    request_id: attempt,
                    outcome: TurnOutcome::Completed,
                },
                cx,
            );
            assert!(!state.active_threads.contains(&thread));
            assert!(state.conversations[&thread].requests.is_empty());
            assert_eq!(
                state.conversations[&thread].outcome,
                Some(TurnOutcome::Completed)
            );
        });
    }
}
