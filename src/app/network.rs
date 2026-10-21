//! App-owned coordination between Space membership, harness lifetimes and the durable mailbox.
//! A single request queue preserves UI intent order across asynchronous persistence.
use super::*;
use crate::network::{Action, Network, NetworkSnapshot, Peer};

#[derive(Default)]
pub struct NetworkState {
    pub snapshot: NetworkSnapshot,
    pub error: Option<String>,
    pub pending: HashSet<ThreadId>,
    pub revision: u64,
    requests: Option<async_channel::Sender<Request>>,
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
    },
    Tool {
        peers: Vec<Peer>,
        token: Uuid,
        action: Action,
        cancelled: async_channel::Receiver<()>,
        reply: std::sync::mpsc::Sender<std::result::Result<Value, String>>,
    },
}

enum Update {
    Snapshot(NetworkSnapshot),
    Membership(ThreadId, std::result::Result<(), String>),
    Failed(String),
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
                        let result = network.flush().await.map_err(|error| format!("{error:#}"));
                        let _ = reply.send(result).await;
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
                    } => {
                        let result = async {
                            network.sync(peers).await?;
                            network.set_joined(thread, joined).await
                        }
                        .await
                        .map_err(|error| {
                            format!("Network membership could not be changed: {error:#}")
                        });
                        let _ = updates.send(Update::Membership(thread, result)).await;
                    }
                    Request::Tool {
                        peers,
                        token,
                        action,
                        cancelled,
                        reply,
                    } => {
                        let response = async {
                            network.sync(peers).await?;
                            network.begin_call(token, action, cancelled).await
                        }
                        .await;
                        match response {
                            Ok(response) => {
                                smol::spawn(async move {
                                    let result = response
                                        .recv()
                                        .await
                                        .context("Network stopped before replying")
                                        .and_then(|result| result)
                                        .map_err(|error| format!("{error:#}"));
                                    let _ = reply.send(result);
                                })
                                .detach();
                            }
                            Err(error) => {
                                let _ = reply.send(Err(format!("{error:#}")));
                            }
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
                                }
                                this.network.snapshot = snapshot;
                            }
                            Update::Membership(thread, result) => {
                                this.network.pending.remove(&thread);
                                this.network.error = result.err();
                            }
                            Update::Failed(error) => this.network.error = Some(error),
                        }
                        this.network.revision = this.network.revision.wrapping_add(1);
                        for thread in &this.snapshot.threads {
                            if thread.kind == ThreadKind::Conversation {
                                cx.emit(PresentationChange::Thread(thread.id));
                            }
                        }
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

    /// Builds the authoritative directory, never trusting a Space or sender supplied by a tool.
    fn network_peers(&self) -> Vec<Peer> {
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
                let worktree = self
                    .thread_worktree(thread)
                    .map(|worktree| worktree.path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| project.root.to_string_lossy().into_owned());
                Some(Peer {
                    thread: thread.id,
                    space,
                    title: thread.title.clone(),
                    project: project.name.clone(),
                    worktree,
                    harness: thread.harness.clone(),
                    status: status.into(),
                    token,
                    eligible,
                })
            })
            .collect()
    }

    pub(super) fn sync_network(&mut self) {
        let Some(requests) = &self.network.requests else {
            return;
        };
        if let Err(error) = requests.try_send(Request::Sync(self.network_peers())) {
            self.network.error = Some(format!(
                "Network update could not be queued: {error}. Reopen the application to reconnect."
            ));
        }
    }

    pub fn network_joined(&self, thread: ThreadId) -> bool {
        self.network
            .snapshot
            .members
            .iter()
            .any(|member| member.thread == thread && self.thread_in_space(thread, member.space))
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
        if self.browser_tools.is_none() && !self.network_joined(thread) {
            return Some(
                "Agent tool bridge is unavailable; reopen the application to reconnect".into(),
            );
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
        let unavailable = candidate.archived
            || self
                .snapshot
                .projects
                .iter()
                .find(|project| project.id == candidate.project_id)
                .is_none_or(|project| project.archived)
            || self
                .thread_worktree(candidate)
                .is_some_and(|worktree| !worktree.is_live());
        unavailable.then(|| "Archived Threads and removed worktrees cannot join the network".into())
    }

    pub fn set_network_joined(&mut self, thread: ThreadId, joined: bool, cx: &mut Context<Self>) {
        if let Some(reason) = self.network_membership_reason(thread) {
            self.network.error = Some(reason);
        } else if let Some(requests) = &self.network.requests {
            let request = Request::Membership {
                peers: self.network_peers(),
                thread,
                joined,
            };
            match requests.try_send(request) {
                Ok(()) => {
                    self.network.error = None;
                    self.network.pending.insert(thread);
                }
                Err(error) => {
                    self.network.error =
                        Some(format!("Network membership could not be queued: {error}"))
                }
            }
        }
        cx.emit(PresentationChange::Thread(thread));
    }

    pub(crate) fn network_tool(
        &mut self,
        thread: ThreadId,
        token: Uuid,
        action: Action,
        cancelled: async_channel::Receiver<()>,
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
        let request = Request::Tool {
            peers: self.network_peers(),
            token,
            action,
            cancelled,
            reply: reply.clone(),
        };
        if let Err(error) = requests.try_send(request) {
            let _ = reply.send(Err(format!(
                "Network request queue is unavailable: {error}"
            )));
        }
    }
}
