//! Durable, opt-in messaging between Conversations in the same Space.
//!
//! Runtime capabilities never enter the persisted snapshot. The actor serializes
//! authorization and mutations; only a successful Store commit publishes state.
//! Waits are runtime-only notifications: the actor remains available for sends,
//! and callers must explicitly retrieve mail with inbox after being notified.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result, anyhow, bail, ensure};
use async_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::store::Store;

pub const SETTING: &str = "space_network";
pub const MAX_BODY_BYTES: usize = 8192;
pub const MAX_MEMBERS: usize = 256;
pub const MAX_MESSAGES: usize = 256;
pub const MAX_PENDING: usize = 1024;
pub const MAX_PENDING_PER_RECIPIENT: usize = 128;
pub const MAX_WAITERS: usize = 32;
const MAX_CONTEXT_BYTES: usize = 8192;
// Leave transport-envelope headroom under the tool bridge's 1 MiB limit.
const MAX_RESPONSE_BYTES: usize = 384 * 1024;
const MAX_INBOX_BYTES: usize = 256 * 1024;

type Reply<T> = Sender<Result<T>>;

#[derive(Clone)]
pub struct Network {
    commands: Sender<Command>,
}

#[derive(Clone, Debug)]
pub struct Peer {
    pub thread: Uuid,
    pub space: Uuid,
    pub title: String,
    pub project: String,
    pub worktree: String,
    pub harness: String,
    pub status: String,
    pub token: Option<Uuid>,
    pub eligible: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSnapshot {
    pub members: Vec<Member>,
    pub messages: Vec<NetworkMessage>,
    /// Runtime-only parked calls; waiting never acknowledges mailbox contents.
    #[serde(skip)]
    pub waiting: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub thread: Uuid,
    pub space: Uuid,
    pub title: String,
    pub project: String,
    pub worktree: String,
    pub harness: String,
    pub status: String,
    pub online: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkMessage {
    pub id: Uuid,
    pub space: Uuid,
    pub sender: Uuid,
    pub sender_name: String,
    pub body: String,
    pub broadcast: bool,
    pub created_at: String,
    pub deliveries: Vec<Delivery>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub recipient: Uuid,
    pub recipient_name: String,
    pub retrieved: bool,
    /// A terminal, non-retrieved delivery, e.g. "left", "moved", "unavailable".
    /// Closing is distinct from retrieval and prevents leave/rejoin redelivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Members,
    Send { recipient: Uuid, body: String },
    Broadcast { body: String },
    Inbox,
    Wait,
}

enum Command {
    Sync(Vec<Peer>, Reply<()>),
    SetJoined(Uuid, bool, Reply<()>),
    Barrier(Reply<()>),
    Call(Uuid, Action, Receiver<()>, Reply<Value>),
    CancelWait { thread: Uuid, id: Uuid },
}

impl Network {
    pub fn start(store: Store, saved: Option<&Value>) -> Result<(Self, Receiver<NetworkSnapshot>)> {
        let mut snapshot: NetworkSnapshot = match saved {
            Some(value) => serde_json::from_value(value.clone()).context("Read Space network")?,
            None => NetworkSnapshot::default(),
        };
        validate_snapshot(&snapshot).context("Invalid saved Space network")?;
        // A persisted online bit is not proof of a runtime in this process.
        for member in &mut snapshot.members {
            member.online = false;
            member.status = "Offline".into();
        }
        snapshot.error = None;
        let (commands, requests) = async_channel::bounded(64);
        // Coalesce complete snapshots instead of retaining an unbounded UI backlog.
        let (updates, receiver) = async_channel::bounded(1);
        let pending_updates = receiver.clone();
        updates
            .try_send(snapshot.clone())
            .map_err(|_| anyhow!("Initialize Space network snapshot channel"))?;
        let (cancellations, cancelled) = async_channel::unbounded();
        let actor = Actor {
            store,
            snapshot,
            peers: BTreeMap::new(),
            updates,
            pending_updates,
            waits: BTreeMap::new(),
            cancellations,
        };
        smol::spawn(actor.run(requests, cancelled)).detach();
        Ok((Self { commands }, receiver))
    }

    pub async fn sync(&self, peers: Vec<Peer>) -> Result<()> {
        self.request(|reply| Command::Sync(peers, reply)).await
    }

    pub async fn set_joined(&self, thread: Uuid, joined: bool) -> Result<()> {
        self.request(|reply| Command::SetJoined(thread, joined, reply))
            .await
    }

    /// Finish processing earlier commands without waiting for parked calls.
    pub async fn flush(&self) -> Result<()> {
        self.request(Command::Barrier).await
    }

    /// Enqueue in actor order without waiting for a parked call to complete.
    pub async fn begin_call(
        &self,
        token: Uuid,
        action: Action,
        cancelled: Receiver<()>,
    ) -> Result<Receiver<Result<Value>>> {
        let (reply, response) = async_channel::bounded(1);
        self.commands
            .send(Command::Call(token, action, cancelled, reply))
            .await
            .map_err(|_| anyhow!("Space network is unavailable"))?;
        Ok(response)
    }

    pub async fn call(&self, token: Uuid, action: Action) -> Result<Value> {
        // Keep the sender alive until completion. Dropping this future closes the
        // channel, including while begin_call is waiting for queue capacity.
        let (cancellation, cancelled) = async_channel::bounded(1);
        let response = self.begin_call(token, action, cancelled).await?;
        let result = response
            .recv()
            .await
            .map_err(|_| anyhow!("Space network stopped before replying"))?;
        drop(cancellation);
        result
    }

    async fn request<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T> {
        let (reply, response) = async_channel::bounded(1);
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| anyhow!("Space network is unavailable"))?;
        response
            .recv()
            .await
            .map_err(|_| anyhow!("Space network stopped before replying"))?
    }
}

struct Actor {
    store: Store,
    snapshot: NetworkSnapshot,
    peers: BTreeMap<Uuid, Peer>,
    updates: Sender<NetworkSnapshot>,
    pending_updates: Receiver<NetworkSnapshot>,
    waits: BTreeMap<Uuid, PendingWait>,
    cancellations: Sender<Command>,
}

struct PendingWait {
    id: Uuid,
    token: Uuid,
    cancelled: Receiver<()>,
    reply: Reply<Value>,
    // Dropping a completed/cancelled waiter also cancels its closure watcher.
    _watch: smol::Task<()>,
}

impl Actor {
    async fn run(mut self, requests: Receiver<Command>, cancelled: Receiver<Command>) {
        loop {
            // Cancellation has its own queue so watchers neither keep the public
            // command channel alive nor compete with sends for bounded capacity.
            let command = smol::future::or(cancelled.recv(), requests.recv()).await;
            let Ok(command) = command else {
                self.cancel_all("Space network shut down");
                break;
            };
            match command {
                Command::Sync(peers, reply) => {
                    let result = self.sync(peers).await;
                    if result.is_err() {
                        self.cancel_all("Space network context synchronization failed");
                    }
                    let _ = reply.try_send(result);
                }
                Command::SetJoined(thread, joined, reply) => {
                    let _ = reply.try_send(self.set_joined(thread, joined).await);
                }
                Command::Barrier(reply) => {
                    let _ = reply.try_send(self.healthy());
                }
                Command::Call(token, Action::Wait, cancelled, reply) => {
                    self.begin_wait(token, cancelled, reply);
                }
                Command::Call(token, action, cancelled, reply) => {
                    let result = if cancelled.is_closed() {
                        Err(anyhow!("Space network call cancelled"))
                    } else {
                        self.call(token, action).await
                    };
                    let _ = reply.try_send(result);
                }
                Command::CancelWait { thread, id } => {
                    if self.waits.get(&thread).is_some_and(|wait| wait.id == id) {
                        self.finish_wait(thread, Err(anyhow!("Space network wait cancelled")));
                        self.publish();
                    }
                }
            }
        }
    }

    fn begin_wait(&mut self, token: Uuid, cancelled: Receiver<()>, reply: Reply<Value>) {
        let prepare = || -> Result<(Uuid, usize)> {
            ensure!(!cancelled.is_closed(), "Space network wait cancelled");
            let caller = self.caller(token)?;
            ensure!(
                !self.waits.contains_key(&caller.thread),
                "Conversation already has a live Space network wait"
            );
            let pending = pending_count(&self.snapshot, caller);
            ensure!(
                pending > 0 || self.waits.len() < MAX_WAITERS,
                "Space network wait capacity reached ({MAX_WAITERS})"
            );
            Ok((caller.thread, pending))
        };
        let (thread, pending) = match prepare() {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = reply.try_send(Err(error));
                return;
            }
        };
        if pending > 0 {
            let result = if cancelled.is_closed() {
                Err(anyhow!("Space network wait cancelled"))
            } else {
                Ok(json!({"ready": true, "pending": pending}))
            };
            let _ = reply.try_send(result);
            return;
        }
        let id = Uuid::new_v4();
        let watch_cancelled = cancelled.clone();
        let cancellations = self.cancellations.clone();
        let watch = smol::spawn(async move {
            let _ = watch_cancelled.recv().await;
            let _ = cancellations.try_send(Command::CancelWait { thread, id });
        });
        self.waits.insert(
            thread,
            PendingWait {
                id,
                token,
                cancelled,
                reply,
                _watch: watch,
            },
        );
        self.snapshot.waiting = self.waits.keys().copied().collect();
        self.publish();
    }

    fn finish_wait(&mut self, thread: Uuid, result: Result<Value>) {
        if let Some(wait) = self.waits.remove(&thread) {
            let result = if result.is_ok() && wait.cancelled.is_closed() {
                Err(anyhow!("Space network wait cancelled"))
            } else {
                result
            };
            let _ = wait.reply.try_send(result);
            self.snapshot.waiting.retain(|waiting| *waiting != thread);
        }
    }

    fn cancel_all(&mut self, reason: &str) {
        if self.waits.is_empty() {
            return;
        }
        for (_, wait) in std::mem::take(&mut self.waits) {
            let _ = wait.reply.try_send(Err(anyhow!("{reason}")));
        }
        self.snapshot.waiting.clear();
        self.publish();
    }

    fn cancel_invalid_waits(&mut self) {
        let invalid = self
            .waits
            .iter()
            .filter_map(|(thread, wait)| {
                (wait.cancelled.is_closed() || self.caller(wait.token).is_err()).then_some(*thread)
            })
            .collect::<Vec<_>>();
        if invalid.is_empty() {
            return;
        }
        for thread in invalid {
            self.finish_wait(
                thread,
                Err(anyhow!("Space network wait cancelled: runtime unavailable")),
            );
        }
        self.publish();
    }

    fn wake_waits(&mut self) {
        // Cancellation is checked again after Store I/O, even if its watcher
        // command is still queued. Never wake an expired runtime capability.
        self.cancel_invalid_waits();
        let ready = self
            .waits
            .iter()
            .filter_map(|(thread, wait)| {
                let caller = self.caller(wait.token).ok()?;
                let pending = pending_count(&self.snapshot, caller);
                (pending > 0).then_some((*thread, pending))
            })
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return;
        }
        for (thread, pending) in ready {
            self.finish_wait(thread, Ok(json!({"ready": true, "pending": pending})));
        }
        self.publish();
    }

    fn publish(&self) {
        // There is only one producer. The consumer may race this drain, but cannot
        // fill the slot, so the following try_send always has room.
        let _ = self.pending_updates.try_recv();
        let _ = self.updates.try_send(self.snapshot.clone());
    }

    fn healthy(&self) -> Result<()> {
        if let Some(error) = &self.snapshot.error {
            bail!("Space network is disabled after a persistence failure: {error}");
        }
        Ok(())
    }

    async fn commit(&mut self, next: NetworkSnapshot) -> Result<()> {
        self.healthy()?;
        if next == self.snapshot {
            return Ok(());
        }
        let value = serde_json::to_value(&next).context("Encode Space network")?;
        if let Err(error) = self.store.set_setting(SETTING.into(), value).await {
            // Keep the last committed history, revoke every capability, and do not
            // pretend a failed invalidation or acknowledgement was durable.
            self.peers.clear();
            for member in &mut self.snapshot.members {
                member.online = false;
            }
            self.snapshot.error = Some(format!(
                "{error:#}. Restore writable storage, then reopen the application"
            ));
            self.cancel_all("Space network wait cancelled after persistence failure");
            self.publish();
            return Err(error.context("Persist Space network; network disabled"));
        }
        self.snapshot = next;
        self.publish();
        Ok(())
    }

    async fn sync(&mut self, peers: Vec<Peer>) -> Result<()> {
        // Revoke the old context before validation or awaiting any Store I/O.
        // In particular, a failed move/archive write cannot leave an old token live.
        self.peers.clear();
        self.healthy()?;
        let mut tokens = HashSet::new();
        let mut contexts = BTreeMap::new();
        for peer in peers {
            validate_context(
                &peer.title,
                &peer.project,
                &peer.worktree,
                &peer.harness,
                &peer.status,
            )?;
            if let Some(token) = peer.token {
                ensure!(
                    tokens.insert(token),
                    "Duplicate Space network runtime capability"
                );
            }
            let thread = peer.thread;
            ensure!(
                contexts.insert(thread, peer).is_none(),
                "Duplicate Space network thread context"
            );
        }
        self.peers = contexts;
        self.cancel_invalid_waits();
        let membership_changed = self.snapshot.members.iter().any(|member| {
            self.peers
                .get(&member.thread)
                .is_none_or(|peer| !peer.eligible || peer.space != member.space)
        });
        if !membership_changed {
            // Presence is runtime state. Do not copy or rewrite mailbox history on
            // every tool request or busy/idle transition.
            let mut changed = false;
            for member in &mut self.snapshot.members {
                if let Some(peer) = self.peers.get(&member.thread)
                    && (member.title != peer.title
                        || member.project != peer.project
                        || member.worktree != peer.worktree
                        || member.harness != peer.harness
                        || member.status != peer.status
                        || member.online != peer.token.is_some())
                {
                    *member = Member::from(peer);
                    changed = true;
                }
            }
            if changed {
                self.publish();
            }
            return Ok(());
        }
        let mut next = self.snapshot.clone();
        next.members.retain_mut(|member| {
            let reason = match self.peers.get(&member.thread) {
                Some(peer) if peer.eligible && peer.space == member.space => {
                    *member = Member::from(peer);
                    return true;
                }
                Some(peer) if peer.space != member.space => "moved",
                _ => "unavailable",
            };
            close_deliveries(&mut next.messages, member.thread, reason);
            false
        });
        self.commit(next).await
    }

    async fn set_joined(&mut self, thread: Uuid, joined: bool) -> Result<()> {
        self.healthy()?;
        if !joined {
            self.finish_wait(
                thread,
                Err(anyhow!("Space network wait cancelled: Conversation left")),
            );
        }
        let mut next = self.snapshot.clone();
        if joined {
            let peer = self
                .peers
                .get(&thread)
                .context("Conversation context is unavailable")?;
            ensure!(
                peer.eligible,
                "Only eligible Conversations may join the Space network"
            );
            if next.members.iter().any(|member| member.thread == thread) {
                return Ok(());
            }
            ensure!(
                next.members.len() < MAX_MEMBERS,
                "Space network member capacity reached ({MAX_MEMBERS})"
            );
            next.members.push(Member::from(peer));
            next.members.sort_by_key(|member| member.thread);
        } else {
            next.members.retain(|member| member.thread != thread);
            close_deliveries(&mut next.messages, thread, "left");
        }
        self.commit(next).await
    }

    fn caller(&self, token: Uuid) -> Result<&Member> {
        self.healthy()?;
        let peer = self
            .peers
            .values()
            .find(|peer| peer.token == Some(token) && peer.eligible)
            .context("Invalid or expired Space network capability")?;
        self.snapshot
            .members
            .iter()
            .find(|member| member.thread == peer.thread && member.space == peer.space)
            .context("Conversation has not joined the Space network; user opt-in is required")
    }

    async fn call(&mut self, token: Uuid, action: Action) -> Result<Value> {
        let caller = self.caller(token)?;
        match action {
            Action::Members => bounded_response(json!({"members": self.snapshot.members.iter()
                .filter(|member| member.space == caller.space).collect::<Vec<_>>()})),
            Action::Send { recipient, body } => {
                let sender = caller.clone();
                let recipient = self
                    .snapshot
                    .members
                    .iter()
                    .find(|member| member.thread == recipient && member.space == sender.space)
                    .context("Recipient is not a joined member of this Space")?
                    .clone();
                self.send(sender, vec![recipient], body, false).await
            }
            Action::Broadcast { body } => {
                let sender = caller.clone();
                let recipients = self
                    .snapshot
                    .members
                    .iter()
                    .filter(|member| member.space == sender.space && member.thread != sender.thread)
                    .cloned()
                    .collect::<Vec<_>>();
                ensure!(
                    !recipients.is_empty(),
                    "There are no other joined members in this Space"
                );
                self.send(sender, recipients, body, true).await
            }
            Action::Inbox => {
                let caller = caller.clone();
                let mut messages = pending_inbox(&self.snapshot, &caller)?;
                if messages.is_empty() {
                    return Ok(json!({"messages": [], "remaining": 0}));
                }
                let remaining = self
                    .snapshot
                    .messages
                    .iter()
                    .filter(|message| message.space == caller.space)
                    .filter(|message| {
                        message.deliveries.iter().any(|delivery| {
                            delivery.recipient == caller.thread && delivery.pending()
                        })
                    })
                    .count()
                    - messages.len();
                let ids = messages
                    .iter()
                    .map(|message| message.id)
                    .collect::<HashSet<_>>();
                let mut next = self.snapshot.clone();
                for message in &mut next.messages {
                    if message.space == caller.space && ids.contains(&message.id) {
                        for delivery in &mut message.deliveries {
                            if delivery.recipient == caller.thread && delivery.pending() {
                                delivery.retrieved = true;
                            }
                        }
                    }
                }
                self.commit(next).await?;
                for message in &mut messages {
                    message.deliveries[0].retrieved = true;
                }
                Ok(json!({"messages": messages, "remaining": remaining}))
            }
            Action::Wait => unreachable!("Wait is parked by the actor command dispatcher"),
        }
    }

    async fn send(
        &mut self,
        sender: Member,
        recipients: Vec<Member>,
        body: String,
        broadcast: bool,
    ) -> Result<Value> {
        let body = body.trim();
        validate_body(body)?;
        let mut pending = 0;
        let mut per_recipient = HashMap::<Uuid, usize>::new();
        for delivery in self
            .snapshot
            .messages
            .iter()
            .flat_map(|message| &message.deliveries)
        {
            if delivery.pending() {
                pending += 1;
                *per_recipient.entry(delivery.recipient).or_default() += 1;
            }
        }
        ensure!(
            pending + recipients.len() <= MAX_PENDING,
            "Space network pending delivery capacity reached ({MAX_PENDING})"
        );
        for recipient in &recipients {
            ensure!(
                per_recipient.get(&recipient.thread).copied().unwrap_or(0)
                    < MAX_PENDING_PER_RECIPIENT,
                "Pending inbox capacity reached for {} ({MAX_PENDING_PER_RECIPIENT})",
                recipient.title
            );
        }
        let mut next = self.snapshot.clone();
        if next.messages.len() >= MAX_MESSAGES {
            let index = next
                .messages
                .iter()
                .position(|message| !message.deliveries.iter().any(Delivery::pending))
                .context(
                    "Space network history capacity reached; retrieve pending messages first",
                )?;
            next.messages.remove(index);
        }
        let message = NetworkMessage {
            id: Uuid::new_v4(),
            space: sender.space,
            sender: sender.thread,
            sender_name: sender.title,
            body: body.into(),
            broadcast,
            created_at: crate::platform::now(),
            deliveries: recipients
                .into_iter()
                .map(|recipient| Delivery {
                    recipient: recipient.thread,
                    recipient_name: recipient.title,
                    retrieved: false,
                    closed: None,
                })
                .collect(),
        };
        let response = bounded_response(json!({"message": &message}))?;
        next.messages.push(message);
        self.commit(next).await?;
        self.wake_waits();
        Ok(response)
    }
}

impl From<&Peer> for Member {
    fn from(peer: &Peer) -> Self {
        Self {
            thread: peer.thread,
            space: peer.space,
            title: peer.title.clone(),
            project: peer.project.clone(),
            worktree: peer.worktree.clone(),
            harness: peer.harness.clone(),
            status: peer.status.clone(),
            online: peer.token.is_some(),
        }
    }
}

impl Delivery {
    fn pending(&self) -> bool {
        !self.retrieved && self.closed.is_none()
    }
}

fn close_deliveries(messages: &mut [NetworkMessage], thread: Uuid, reason: &str) {
    for delivery in messages
        .iter_mut()
        .flat_map(|message| &mut message.deliveries)
    {
        if delivery.recipient == thread && delivery.pending() {
            delivery.closed = Some(reason.into());
        }
    }
}

fn pending_count(snapshot: &NetworkSnapshot, caller: &Member) -> usize {
    snapshot
        .messages
        .iter()
        .filter(|message| message.space == caller.space)
        .filter(|message| {
            message
                .deliveries
                .iter()
                .any(|delivery| delivery.recipient == caller.thread && delivery.pending())
        })
        .count()
}

fn pending_inbox(snapshot: &NetworkSnapshot, caller: &Member) -> Result<Vec<NetworkMessage>> {
    let mut messages = Vec::new();
    let mut bytes = b"{\"messages\":[],\"remaining\":256}".len();
    for message in snapshot
        .messages
        .iter()
        .filter(|message| message.space == caller.space)
    {
        let Some(delivery) = message
            .deliveries
            .iter()
            .find(|delivery| delivery.recipient == caller.thread && delivery.pending())
        else {
            continue;
        };
        let message = NetworkMessage {
            id: message.id,
            space: message.space,
            sender: message.sender,
            sender_name: message.sender_name.clone(),
            body: message.body.clone(),
            broadcast: message.broadcast,
            created_at: message.created_at.clone(),
            deliveries: vec![delivery.clone()],
        };
        let size = serde_json::to_vec(&message)?.len() + usize::from(!messages.is_empty());
        if bytes + size > MAX_INBOX_BYTES {
            ensure!(
                !messages.is_empty(),
                "A network message exceeds the inbox response limit"
            );
            break;
        }
        bytes += size;
        messages.push(message);
    }
    Ok(messages)
}

fn bounded_response(response: Value) -> Result<Value> {
    ensure!(
        serde_json::to_vec(&response)?.len() <= MAX_RESPONSE_BYTES,
        "Space network response exceeds {MAX_RESPONSE_BYTES} bytes; inspect activity in the sidebar"
    );
    Ok(response)
}

fn validate_body(body: &str) -> Result<()> {
    ensure!(!body.trim().is_empty(), "Message body must not be empty");
    ensure!(
        body.len() <= MAX_BODY_BYTES,
        "Message body exceeds {MAX_BODY_BYTES} bytes"
    );
    Ok(())
}

fn validate_context(
    title: &str,
    project: &str,
    worktree: &str,
    harness: &str,
    status: &str,
) -> Result<()> {
    ensure!(
        [title, project, worktree, harness, status]
            .iter()
            .all(|value| value.len() <= MAX_CONTEXT_BYTES),
        "Space network member metadata exceeds {MAX_CONTEXT_BYTES} bytes per field"
    );
    Ok(())
}

fn validate_snapshot(snapshot: &NetworkSnapshot) -> Result<()> {
    ensure!(
        snapshot.members.len() <= MAX_MEMBERS,
        "Too many saved network members"
    );
    ensure!(
        snapshot.messages.len() <= MAX_MESSAGES,
        "Too many saved network messages"
    );
    let mut members = HashMap::new();
    for member in &snapshot.members {
        validate_context(
            &member.title,
            &member.project,
            &member.worktree,
            &member.harness,
            &member.status,
        )?;
        ensure!(
            members.insert(member.thread, member.space).is_none(),
            "Duplicate saved member"
        );
    }
    let mut ids = HashSet::new();
    let mut pending = 0;
    let mut per_recipient = HashMap::<Uuid, usize>::new();
    for message in &snapshot.messages {
        ensure!(ids.insert(message.id), "Duplicate saved message");
        validate_body(&message.body)?;
        ensure!(
            message.sender_name.len() <= MAX_CONTEXT_BYTES && message.created_at.len() <= 128,
            "Oversized saved message metadata"
        );
        ensure!(
            !message.deliveries.is_empty() && message.deliveries.len() <= MAX_MEMBERS,
            "Invalid saved delivery count"
        );
        ensure!(
            message.broadcast || message.deliveries.len() == 1,
            "Invalid direct message delivery count"
        );
        let mut recipients = HashSet::new();
        for delivery in &message.deliveries {
            ensure!(
                recipients.insert(delivery.recipient),
                "Duplicate saved delivery"
            );
            ensure!(
                delivery.recipient_name.len() <= MAX_CONTEXT_BYTES,
                "Oversized saved recipient name"
            );
            if let Some(reason) = &delivery.closed {
                ensure!(
                    !delivery.retrieved
                        && matches!(reason.as_str(), "left" | "moved" | "unavailable"),
                    "Invalid saved delivery disposition"
                );
            }
            if delivery.pending() {
                ensure!(
                    members.get(&delivery.recipient) == Some(&message.space),
                    "Saved pending delivery has no joined recipient in its Space"
                );
                pending += 1;
                let count = per_recipient.entry(delivery.recipient).or_default();
                *count += 1;
                ensure!(
                    *count <= MAX_PENDING_PER_RECIPIENT,
                    "Saved inbox exceeds capacity"
                );
            }
        }
    }
    ensure!(
        pending <= MAX_PENDING,
        "Saved pending queue exceeds capacity"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoreError;
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("drostje-network-{}", Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            // Store drops nonblocking; allow its actor to drain before removing
            // the real SQLite/WAL files, matching the Store integration fixtures.
            if let Ok(lock) = fs::File::options()
                .read(true)
                .write(true)
                .open(self.0.join("instance.lock"))
            {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match lock.try_lock() {
                        Ok(()) => break,
                        Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        _ => return,
                    }
                }
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        network: Network,
        updates: Receiver<NetworkSnapshot>,
        store: Store,
        directory: Directory,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = Directory::new();
            let store = Store::open(directory.0.clone()).unwrap();
            let (network, updates) = Network::start(store.clone(), None).unwrap();
            assert_eq!(updates.try_recv().unwrap(), NetworkSnapshot::default());
            Self {
                network,
                updates,
                store,
                directory,
            }
        }

        async fn join(&self, peers: &[Peer]) {
            self.network.sync(peers.to_vec()).await.unwrap();
            for peer in peers {
                self.network.set_joined(peer.thread, true).await.unwrap();
            }
        }

        async fn saved(&self) -> NetworkSnapshot {
            let workspace = self.store.snapshot().await.unwrap();
            serde_json::from_value(workspace.settings[SETTING].clone()).unwrap()
        }

        async fn send(&self, sender: &Peer, recipient: &Peer, body: &str) -> Uuid {
            let response = self
                .network
                .call(
                    token(sender),
                    Action::Send {
                        recipient: recipient.thread,
                        body: body.into(),
                    },
                )
                .await
                .unwrap();
            serde_json::from_value(response["message"]["id"].clone()).unwrap()
        }

        async fn park(&self, caller: &Peer) -> (Sender<()>, Receiver<Result<Value>>) {
            let (cancellation, cancelled) = async_channel::bounded(1);
            let response = self
                .network
                .begin_call(token(caller), Action::Wait, cancelled)
                .await
                .unwrap();
            within(self.network.flush()).await.unwrap();
            assert!(response.is_empty(), "empty mailboxes must park");
            (cancellation, response)
        }
    }

    fn peer(space: Uuid, name: &str) -> Peer {
        Peer {
            thread: Uuid::new_v4(),
            space,
            title: name.into(),
            project: "project".into(),
            worktree: "worktree".into(),
            harness: "omp".into(),
            status: "idle".into(),
            token: Some(Uuid::new_v4()),
            eligible: true,
        }
    }

    fn token(peer: &Peer) -> Uuid {
        peer.token.unwrap()
    }

    fn messages(response: &Value) -> Vec<NetworkMessage> {
        serde_json::from_value(response["messages"].clone()).unwrap()
    }

    async fn within<T>(future: impl Future<Output = T>) -> T {
        smol::future::or(future, async {
            smol::Timer::after(Duration::from_secs(10)).await;
            panic!("Space network operation did not complete");
        })
        .await
    }

    async fn wait_result(response: &Receiver<Result<Value>>) -> Result<Value> {
        within(response.recv())
            .await
            .expect("actor must reply, not drop the wait")
    }

    fn reopen(path: &Path) -> Store {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match Store::open(path.to_path_buf()) {
                Ok(store) => return store,
                Err(error)
                    if matches!(
                        error.downcast_ref::<StoreError>(),
                        Some(StoreError::InstanceInUse(_))
                    ) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("Cannot reopen network Store: {error:#}"),
            }
        }
    }

    #[test]
    fn wait_reports_queued_mail_repeatedly_without_retrieval() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            assert!(
                fixture
                    .network
                    .call(Uuid::new_v4(), Action::Wait)
                    .await
                    .is_err()
            );
            fixture
                .network
                .sync(vec![a.clone(), b.clone()])
                .await
                .unwrap();
            assert!(
                fixture.network.call(token(&b), Action::Wait).await.is_err(),
                "a runtime capability alone must not bypass user opt-in"
            );
            fixture.join(&[a.clone(), b.clone()]).await;
            let first = fixture.send(&a, &b, "first").await;
            let second = fixture.send(&a, &b, "second").await;
            let before = fixture.saved().await;
            for _ in 0..2 {
                assert_eq!(
                    within(fixture.network.call(token(&b), Action::Wait))
                        .await
                        .unwrap(),
                    json!({"ready": true, "pending": 2})
                );
                assert_eq!(fixture.saved().await, before);
            }
            let inbox = messages(
                &fixture
                    .network
                    .call(token(&b), Action::Inbox)
                    .await
                    .unwrap(),
            );
            assert_eq!(
                inbox.iter().map(|message| message.id).collect::<Vec<_>>(),
                vec![first, second]
            );
            assert!(
                fixture
                    .saved()
                    .await
                    .messages
                    .iter()
                    .all(|message| message.deliveries[0].retrieved)
            );
        });
    }

    #[test]
    fn parked_direct_and_broadcast_waits_do_not_block_sends_or_acknowledge() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            let c = peer(a.space, "Carol");
            fixture.join(&[a.clone(), b.clone(), c.clone()]).await;
            let (_cancel_b, response_b) = fixture.park(&b).await;
            fixture
                .network
                .sync(vec![a.clone(), b.clone(), c.clone()])
                .await
                .unwrap();
            assert!(
                response_b.is_empty(),
                "equal sync must not cancel a live wait"
            );
            assert_eq!(fixture.updates.try_recv().unwrap().waiting, vec![b.thread]);
            assert!(fixture.saved().await.waiting.is_empty());
            let direct = within(fixture.send(&a, &b, "direct")).await;
            assert_eq!(
                wait_result(&response_b).await.unwrap(),
                json!({"ready": true, "pending": 1})
            );
            assert!(fixture.updates.try_recv().unwrap().waiting.is_empty());
            assert!(!fixture.saved().await.messages[0].deliveries[0].retrieved);
            assert_eq!(
                messages(
                    &fixture
                        .network
                        .call(token(&b), Action::Inbox)
                        .await
                        .unwrap()
                )[0]
                .id,
                direct
            );

            let (_cancel_b, response_b) = fixture.park(&b).await;
            let (_cancel_c, response_c) = fixture.park(&c).await;
            within(fixture.network.call(
                token(&a),
                Action::Broadcast {
                    body: "everyone".into(),
                },
            ))
            .await
            .unwrap();
            for response in [&response_b, &response_c] {
                assert_eq!(
                    wait_result(response).await.unwrap(),
                    json!({"ready": true, "pending": 1})
                );
            }
            let saved = fixture.saved().await;
            assert!(saved.messages[1].broadcast);
            assert!(saved.messages[1].deliveries.iter().all(Delivery::pending));
            assert!(fixture.updates.try_recv().unwrap().waiting.is_empty());
        });
    }

    #[test]
    fn transport_cancellation_cannot_consume_mail_or_cancel_a_replacement_wait() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            let (cancel, response) = fixture.park(&b).await;
            drop(cancel);
            // Cancellation can race the send or its Store commit; either way it
            // wins over notification and never touches the queued delivery.
            let sent = within(fixture.send(&a, &b, "survives cancellation")).await;
            assert!(wait_result(&response).await.is_err());
            within(fixture.network.flush()).await.unwrap();
            assert!(fixture.updates.try_recv().unwrap().waiting.is_empty());
            let saved = fixture.saved().await;
            assert_eq!(saved.messages[0].id, sent);
            assert!(saved.messages[0].deliveries[0].pending());
            assert_eq!(
                fixture.network.call(token(&b), Action::Wait).await.unwrap(),
                json!({"ready": true, "pending": 1})
            );
            assert_eq!(
                messages(
                    &fixture
                        .network
                        .call(token(&b), Action::Inbox)
                        .await
                        .unwrap()
                )[0]
                .id,
                sent
            );

            let (cancel, response) = fixture.park(&b).await;
            drop(cancel);
            assert!(wait_result(&response).await.is_err());
            let (_cancel, replacement) = fixture.park(&b).await;
            within(fixture.send(&a, &b, "replacement runtime wait")).await;
            assert_eq!(
                wait_result(&replacement).await.unwrap(),
                json!({"ready": true, "pending": 1})
            );
        });
    }

    #[test]
    fn dropping_call_future_closes_cancellation_and_removes_waiting_presentation() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            let _ = fixture.updates.try_recv();
            let network = fixture.network.clone();
            let capability = token(&b);
            let call = smol::spawn(async move { network.call(capability, Action::Wait).await });
            let parked = within(fixture.updates.recv()).await.unwrap();
            assert_eq!(parked.waiting, vec![b.thread]);
            assert!(call.cancel().await.is_none(), "wait must still be parked");
            let cancelled = within(fixture.updates.recv()).await.unwrap();
            assert!(cancelled.waiting.is_empty());
            let sent = fixture.send(&a, &b, "not consumed by dropped caller").await;
            assert!(fixture.saved().await.messages[0].deliveries[0].pending());
            assert_eq!(
                messages(
                    &fixture
                        .network
                        .call(token(&b), Action::Inbox)
                        .await
                        .unwrap()
                )[0]
                .id,
                sent
            );
        });
    }

    #[test]
    fn waits_are_cancelled_on_leave_rotation_offline_move_archive_and_invalid_sync() {
        smol::block_on(async {
            for operation in [
                "leave", "rotate", "offline", "move", "archive", "missing", "invalid",
            ] {
                let fixture = Fixture::new();
                let a = peer(Uuid::new_v4(), "Alice");
                let mut b = peer(a.space, "Bob");
                fixture.join(&[a.clone(), b.clone()]).await;
                let (_cancel, response) = fixture.park(&b).await;
                match operation {
                    "leave" => fixture.network.set_joined(b.thread, false).await.unwrap(),
                    "missing" => fixture.network.sync(vec![a.clone()]).await.unwrap(),
                    "invalid" => {
                        assert!(
                            fixture
                                .network
                                .sync(vec![a.clone(), b.clone(), b.clone()])
                                .await
                                .is_err()
                        );
                    }
                    _ => {
                        match operation {
                            "rotate" => b.token = Some(Uuid::new_v4()),
                            "offline" => b.token = None,
                            "move" => b.space = Uuid::new_v4(),
                            "archive" => b.eligible = false,
                            _ => unreachable!(),
                        }
                        fixture
                            .network
                            .sync(vec![a.clone(), b.clone()])
                            .await
                            .unwrap();
                    }
                }
                assert!(wait_result(&response).await.is_err(), "{operation}");
                assert!(
                    fixture.updates.try_recv().unwrap().waiting.is_empty(),
                    "{operation}"
                );
                if operation == "rotate" {
                    fixture.send(&a, &b, "new token owns queued mail").await;
                    assert_eq!(
                        fixture.network.call(token(&b), Action::Wait).await.unwrap(),
                        json!({"ready": true, "pending": 1})
                    );
                }
            }
        });
    }

    #[test]
    fn failed_store_commit_cancels_all_waits_without_notifying_unaccepted_mail() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            let (_cancel_a, response_a) = fixture.park(&a).await;
            let (_cancel_b, response_b) = fixture.park(&b).await;
            let before = fixture.saved().await;
            let blocker =
                rusqlite::Connection::open(fixture.directory.0.join("drostje.sqlite3")).unwrap();
            blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
            assert!(
                within(fixture.network.call(
                    token(&a),
                    Action::Send {
                        recipient: b.thread,
                        body: "must not notify".into(),
                    }
                ))
                .await
                .is_err()
            );
            assert!(wait_result(&response_a).await.is_err());
            assert!(wait_result(&response_b).await.is_err());
            let failed = fixture.updates.try_recv().unwrap();
            assert!(failed.waiting.is_empty());
            assert!(failed.error.is_some());
            assert_eq!(fixture.saved().await, before);
            assert!(fixture.network.flush().await.is_err());
            blocker.execute_batch("ROLLBACK").unwrap();
        });
    }

    #[test]
    fn waits_reject_duplicates_and_bound_parked_capacity_without_blocking_sends() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let space = Uuid::new_v4();
            let peers = (0..MAX_WAITERS + 2)
                .map(|index| peer(space, &format!("peer {index}")))
                .collect::<Vec<_>>();
            fixture.join(&peers).await;
            let mut waits = Vec::new();
            for peer in peers.iter().take(MAX_WAITERS) {
                waits.push(fixture.park(peer).await);
            }
            assert!(
                within(fixture.network.call(token(&peers[0]), Action::Wait))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("already")
            );
            assert!(
                within(
                    fixture
                        .network
                        .call(token(&peers[MAX_WAITERS]), Action::Wait)
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("capacity")
            );
            within(fixture.send(&peers[MAX_WAITERS + 1], &peers[0], "capacity release")).await;
            assert!(wait_result(&waits[0].1).await.is_ok());
            let (_cancel, response) = fixture.park(&peers[MAX_WAITERS]).await;
            within(fixture.send(&peers[MAX_WAITERS + 1], &peers[MAX_WAITERS], "new slot")).await;
            assert!(wait_result(&response).await.is_ok());
        });
    }

    #[test]
    fn shutdown_cancels_parked_waits_and_real_store_restart_never_restores_them() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            fixture
                .send(&b, &a, "durable but unrelated to Bob's wait")
                .await;
            let (_cancel, response) = fixture.park(&b).await;
            assert_eq!(fixture.updates.try_recv().unwrap().waiting, vec![b.thread]);
            fixture.store.flush().await.unwrap();
            let Fixture {
                network,
                updates,
                store,
                directory,
            } = fixture;
            drop(network);
            assert!(wait_result(&response).await.is_err());
            drop(updates);
            drop(store);

            let store = reopen(&directory.0);
            let workspace = store.snapshot().await.unwrap();
            let (network, updates) =
                Network::start(store.clone(), workspace.settings.get(SETTING)).unwrap();
            let restarted = updates.try_recv().unwrap();
            assert!(restarted.waiting.is_empty());
            assert_eq!(restarted.messages.len(), 1);
            assert!(restarted.messages[0].deliveries[0].pending());
            assert!(network.call(token(&b), Action::Wait).await.is_err());
            network.sync(vec![a.clone(), b.clone()]).await.unwrap();
            assert_eq!(
                network.call(token(&a), Action::Wait).await.unwrap(),
                json!({"ready": true, "pending": 1})
            );
            drop(network);
            drop(updates);
            drop(store);
        });
    }

    #[test]
    fn requires_user_opt_in_and_runtime_capability_and_isolates_spaces() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            let c = peer(Uuid::new_v4(), "Carol");
            let mut factory = peer(a.space, "Factory attempt");
            factory.eligible = false;
            fixture
                .network
                .sync(vec![a.clone(), b.clone(), c.clone(), factory.clone()])
                .await
                .unwrap();
            assert!(
                fixture
                    .network
                    .call(token(&a), Action::Members)
                    .await
                    .is_err()
            );
            assert!(
                fixture
                    .network
                    .set_joined(factory.thread, true)
                    .await
                    .is_err()
            );
            for member in [&a, &b, &c] {
                fixture
                    .network
                    .set_joined(member.thread, true)
                    .await
                    .unwrap();
            }
            assert!(
                fixture
                    .network
                    .call(Uuid::new_v4(), Action::Members)
                    .await
                    .is_err()
            );
            let visible = fixture
                .network
                .call(token(&a), Action::Members)
                .await
                .unwrap();
            let visible: Vec<Member> = serde_json::from_value(visible["members"].clone()).unwrap();
            assert_eq!(visible.len(), 2);
            assert!(visible.iter().all(|member| member.space == a.space));
            assert!(
                fixture
                    .network
                    .call(
                        token(&a),
                        Action::Send {
                            recipient: c.thread,
                            body: "cross-Space".into(),
                        }
                    )
                    .await
                    .is_err()
            );
            fixture.send(&a, &b, "  untrusted peer content  ").await;
            assert!(
                messages(
                    &fixture
                        .network
                        .call(token(&a), Action::Inbox)
                        .await
                        .unwrap()
                )
                .is_empty()
            );
            assert!(
                messages(
                    &fixture
                        .network
                        .call(token(&c), Action::Inbox)
                        .await
                        .unwrap()
                )
                .is_empty()
            );
            let inbox = messages(
                &fixture
                    .network
                    .call(token(&b), Action::Inbox)
                    .await
                    .unwrap(),
            );
            assert_eq!(inbox.len(), 1);
            assert_eq!(inbox[0].sender, a.thread);
            assert_eq!(inbox[0].body, "untrusted peer content");
            assert!(inbox[0].deliveries[0].retrieved);
            assert!(
                messages(
                    &fixture
                        .network
                        .call(token(&b), Action::Inbox)
                        .await
                        .unwrap()
                )
                .is_empty()
            );
            let saved = fixture.saved().await;
            assert!(saved.messages[0].deliveries[0].retrieved);
            let encoded = serde_json::to_string(&saved).unwrap();
            for member in [&a, &b, &c] {
                assert!(!encoded.contains(&token(member).to_string()));
            }
            assert!(
                serde_json::from_value::<Action>(json!({
                    "action": "send", "recipient": b.thread, "body": "spoof", "sender": c.thread,
                }))
                .is_err()
            );
        });
    }

    #[test]
    fn offline_queue_and_rotated_tokens_preserve_names_and_equal_sync_is_quiet() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let mut a = peer(Uuid::new_v4(), "Original sender");
            let mut b = peer(a.space, "Original recipient");
            fixture.join(&[a.clone(), b.clone()]).await;
            let old_token = token(&b);
            b.token = None;
            b.status = "offline".into();
            fixture
                .network
                .sync(vec![a.clone(), b.clone()])
                .await
                .unwrap();
            let id = fixture.send(&a, &b, "queued offline").await;
            assert!(
                !fixture
                    .saved()
                    .await
                    .members
                    .iter()
                    .find(|member| member.thread == b.thread)
                    .unwrap()
                    .online
            );
            assert!(
                fixture
                    .network
                    .call(old_token, Action::Inbox)
                    .await
                    .is_err()
            );
            a.title = "Renamed sender".into();
            b.title = "Renamed recipient".into();
            b.token = Some(Uuid::new_v4());
            b.status = "busy".into();
            fixture
                .network
                .sync(vec![b.clone(), a.clone()])
                .await
                .unwrap();
            fixture.updates.try_recv().unwrap();
            fixture
                .network
                .sync(vec![a.clone(), b.clone()])
                .await
                .unwrap();
            assert!(
                fixture.updates.try_recv().is_err(),
                "Equal contexts must not publish another snapshot"
            );
            assert!(
                fixture
                    .network
                    .call(old_token, Action::Inbox)
                    .await
                    .is_err()
            );
            let inbox = messages(
                &fixture
                    .network
                    .call(token(&b), Action::Inbox)
                    .await
                    .unwrap(),
            );
            assert_eq!(inbox[0].id, id);
            assert_eq!(inbox[0].sender_name, "Original sender");
            assert_eq!(inbox[0].deliveries[0].recipient_name, "Original recipient");
            let saved = fixture.saved().await;
            assert_eq!(
                saved
                    .members
                    .iter()
                    .find(|member| member.thread == a.thread)
                    .unwrap()
                    .title,
                a.title
            );
            assert_eq!(
                saved
                    .members
                    .iter()
                    .find(|member| member.thread == b.thread)
                    .unwrap()
                    .status,
                "busy"
            );
        });
    }

    #[test]
    fn broadcasts_freeze_recipients_and_preserve_partial_dispositions_across_leave_rejoin() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            let c = peer(a.space, "Carol");
            let d = peer(a.space, "Dave");
            fixture.join(std::slice::from_ref(&a)).await;
            assert!(
                fixture
                    .network
                    .call(
                        token(&a),
                        Action::Broadcast {
                            body: "nobody".into()
                        }
                    )
                    .await
                    .is_err()
            );
            fixture.join(&[a.clone(), b.clone(), c.clone()]).await;
            fixture
                .network
                .call(
                    token(&a),
                    Action::Broadcast {
                        body: "frozen".into(),
                    },
                )
                .await
                .unwrap();
            fixture
                .network
                .sync(vec![a.clone(), b.clone(), c.clone(), d.clone()])
                .await
                .unwrap();
            fixture.network.set_joined(d.thread, true).await.unwrap();
            let inbox = messages(
                &fixture
                    .network
                    .call(token(&b), Action::Inbox)
                    .await
                    .unwrap(),
            );
            assert_eq!(inbox.len(), 1);
            assert_eq!(inbox[0].deliveries.len(), 1);
            assert_eq!(inbox[0].deliveries[0].recipient, b.thread);
            fixture.network.set_joined(c.thread, false).await.unwrap();
            assert!(
                fixture
                    .network
                    .call(token(&c), Action::Inbox)
                    .await
                    .is_err()
            );
            fixture.network.set_joined(c.thread, true).await.unwrap();
            assert!(
                messages(
                    &fixture
                        .network
                        .call(token(&c), Action::Inbox)
                        .await
                        .unwrap()
                )
                .is_empty()
            );
            assert!(
                messages(
                    &fixture
                        .network
                        .call(token(&d), Action::Inbox)
                        .await
                        .unwrap()
                )
                .is_empty()
            );
            let saved = fixture.saved().await;
            assert_eq!(saved.messages.len(), 1);
            assert_eq!(saved.messages[0].deliveries.len(), 2);
            let delivered = saved.messages[0]
                .deliveries
                .iter()
                .find(|delivery| delivery.recipient == b.thread)
                .unwrap();
            assert!(delivered.retrieved);
            assert!(delivered.closed.is_none());
            let closed = saved.messages[0]
                .deliveries
                .iter()
                .find(|delivery| delivery.recipient == c.thread)
                .unwrap();
            assert!(!closed.retrieved);
            assert_eq!(closed.closed.as_deref(), Some("left"));
            fixture.send(&a, &c, "new membership").await;
            assert_eq!(
                messages(
                    &fixture
                        .network
                        .call(token(&c), Action::Inbox)
                        .await
                        .unwrap()
                )
                .len(),
                1
            );
        });
    }

    #[test]
    fn moves_archives_and_missing_threads_close_stale_deliveries_without_cross_space_reads() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Old Space");
            let mut b = peer(a.space, "Moving");
            fixture.join(&[a.clone(), b.clone()]).await;
            fixture.send(&a, &b, "old Space secret").await;
            b.space = Uuid::new_v4();
            let c = peer(b.space, "New Space");
            fixture
                .network
                .sync(vec![a.clone(), b.clone(), c.clone()])
                .await
                .unwrap();
            assert!(
                fixture
                    .network
                    .call(token(&b), Action::Inbox)
                    .await
                    .is_err()
            );
            fixture.network.set_joined(b.thread, true).await.unwrap();
            fixture.network.set_joined(c.thread, true).await.unwrap();
            fixture.send(&c, &b, "new Space message").await;
            let inbox = messages(
                &fixture
                    .network
                    .call(token(&b), Action::Inbox)
                    .await
                    .unwrap(),
            );
            assert_eq!(inbox.len(), 1);
            assert_eq!(inbox[0].body, "new Space message");
            fixture.send(&c, &b, "before archive").await;
            b.eligible = false;
            fixture
                .network
                .sync(vec![a.clone(), b.clone(), c.clone()])
                .await
                .unwrap();
            b.eligible = true;
            fixture
                .network
                .sync(vec![a.clone(), b.clone(), c.clone()])
                .await
                .unwrap();
            fixture.network.set_joined(b.thread, true).await.unwrap();
            assert!(
                messages(
                    &fixture
                        .network
                        .call(token(&b), Action::Inbox)
                        .await
                        .unwrap()
                )
                .is_empty()
            );
            fixture.send(&c, &b, "before deletion").await;
            fixture
                .network
                .sync(vec![a.clone(), c.clone()])
                .await
                .unwrap();
            fixture.network.sync(vec![a, b.clone(), c]).await.unwrap();
            fixture.network.set_joined(b.thread, true).await.unwrap();
            assert!(
                messages(
                    &fixture
                        .network
                        .call(token(&b), Action::Inbox)
                        .await
                        .unwrap()
                )
                .is_empty()
            );
            let saved = fixture.saved().await;
            assert_eq!(
                saved.messages[0].deliveries[0].closed.as_deref(),
                Some("moved")
            );
            assert_eq!(
                saved.messages[2].deliveries[0].closed.as_deref(),
                Some("unavailable")
            );
            assert_eq!(
                saved.messages[3].deliveries[0].closed.as_deref(),
                Some("unavailable")
            );
        });
    }

    #[test]
    fn real_store_reopen_restores_members_and_pending_but_never_runtime_authority() {
        smol::block_on(async {
            let Fixture {
                network,
                updates,
                store,
                directory,
            } = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let mut b = peer(a.space, "Bob");
            network.sync(vec![a.clone(), b.clone()]).await.unwrap();
            network.set_joined(a.thread, true).await.unwrap();
            network.set_joined(b.thread, true).await.unwrap();
            network
                .call(
                    token(&a),
                    Action::Send {
                        recipient: b.thread,
                        body: "survives restart".into(),
                    },
                )
                .await
                .unwrap();
            store.flush().await.unwrap();
            drop(network);
            drop(updates);
            drop(store);

            let store = reopen(&directory.0);
            let workspace = store.snapshot().await.unwrap();
            let (network, updates) =
                Network::start(store.clone(), workspace.settings.get(SETTING)).unwrap();
            let initial = updates.try_recv().unwrap();
            assert_eq!(initial.members.len(), 2);
            assert!(initial.members.iter().all(|member| !member.online));
            assert_eq!(initial.messages.len(), 1);
            let old_token = token(&b);
            assert!(network.call(old_token, Action::Inbox).await.is_err());
            b.token = Some(Uuid::new_v4());
            network.sync(vec![a.clone(), b.clone()]).await.unwrap();
            assert!(network.call(old_token, Action::Inbox).await.is_err());
            let inbox = messages(&network.call(token(&b), Action::Inbox).await.unwrap());
            assert_eq!(inbox.len(), 1);
            assert_eq!(inbox[0].body, "survives restart");
            store.flush().await.unwrap();
            drop(network);
            drop(updates);
            drop(store);

            let store = reopen(&directory.0);
            let workspace = store.snapshot().await.unwrap();
            let (network, updates) =
                Network::start(store.clone(), workspace.settings.get(SETTING)).unwrap();
            network.sync(vec![a, b.clone()]).await.unwrap();
            assert!(messages(&network.call(token(&b), Action::Inbox).await.unwrap()).is_empty());
            drop(network);
            drop(updates);
            drop(store);
        });
    }

    #[test]
    fn concurrent_sends_and_inbox_reads_are_serial_and_never_double_retrieve() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            let mut sends = Vec::new();
            for index in 0..64 {
                let network = fixture.network.clone();
                let token = token(&a);
                let recipient = b.thread;
                sends.push(smol::spawn(async move {
                    network
                        .call(
                            token,
                            Action::Send {
                                recipient,
                                body: format!("message {index}"),
                            },
                        )
                        .await
                        .unwrap()
                }));
            }
            let mut ids = HashSet::new();
            for send in sends {
                let response = send.await;
                assert!(ids.insert(response["message"]["id"].as_str().unwrap().to_owned()));
            }
            let left = fixture.network.clone();
            let right = fixture.network.clone();
            let token = token(&b);
            let left = smol::spawn(async move { left.call(token, Action::Inbox).await.unwrap() });
            let right = smol::spawn(async move { right.call(token, Action::Inbox).await.unwrap() });
            let first = messages(&left.await);
            let second = messages(&right.await);
            assert_eq!(first.len() + second.len(), 64);
            assert!(first.is_empty() || second.is_empty());
            assert!(
                fixture
                    .saved()
                    .await
                    .messages
                    .iter()
                    .all(|message| message.deliveries[0].retrieved)
            );
        });
    }

    #[test]
    fn pending_inbox_capacity_rejects_broadcast_atomically_and_does_not_evict_pending() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Full inbox");
            let c = peer(a.space, "Empty inbox");
            fixture.join(&[a.clone(), b.clone(), c.clone()]).await;
            for _ in 0..MAX_PENDING_PER_RECIPIENT {
                fixture.send(&a, &b, "pending").await;
            }
            let before = fixture.saved().await;
            assert!(
                fixture
                    .network
                    .call(
                        token(&a),
                        Action::Send {
                            recipient: b.thread,
                            body: "overflow".into()
                        }
                    )
                    .await
                    .is_err()
            );
            assert!(
                fixture
                    .network
                    .call(
                        token(&a),
                        Action::Broadcast {
                            body: "atomic overflow".into()
                        }
                    )
                    .await
                    .is_err()
            );
            assert_eq!(fixture.saved().await, before);
            assert!(
                messages(
                    &fixture
                        .network
                        .call(token(&c), Action::Inbox)
                        .await
                        .unwrap()
                )
                .is_empty()
            );
            assert_eq!(
                messages(
                    &fixture
                        .network
                        .call(token(&b), Action::Inbox)
                        .await
                        .unwrap()
                )
                .len(),
                MAX_PENDING_PER_RECIPIENT
            );
            fixture
                .network
                .call(
                    token(&a),
                    Action::Broadcast {
                        body: "room now".into(),
                    },
                )
                .await
                .unwrap();
            let saved = fixture.saved().await;
            assert_eq!(saved.messages.last().unwrap().deliveries.len(), 2);
        });
    }

    #[test]
    fn global_pending_capacity_is_shared_across_recipients_and_frees_on_retrieval() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let recipients = (0..8)
                .map(|index| peer(a.space, &format!("recipient {index}")))
                .collect::<Vec<_>>();
            let mut peers = vec![a.clone()];
            peers.extend(recipients.clone());
            fixture.join(&peers).await;
            assert_eq!(8 * MAX_PENDING_PER_RECIPIENT, MAX_PENDING);
            for _ in 0..MAX_PENDING_PER_RECIPIENT {
                fixture
                    .network
                    .call(
                        token(&a),
                        Action::Broadcast {
                            body: "eight deliveries".into(),
                        },
                    )
                    .await
                    .unwrap();
            }
            let extra = peer(a.space, "Additional empty inbox");
            peers.push(extra.clone());
            fixture.network.sync(peers).await.unwrap();
            fixture
                .network
                .set_joined(extra.thread, true)
                .await
                .unwrap();
            let before = fixture.saved().await;
            assert!(
                fixture
                    .network
                    .call(
                        token(&a),
                        Action::Send {
                            recipient: extra.thread,
                            body: "global overflow".into()
                        }
                    )
                    .await
                    .is_err()
            );
            assert_eq!(fixture.saved().await, before);
            fixture
                .network
                .call(token(&recipients[0]), Action::Inbox)
                .await
                .unwrap();
            fixture.send(&a, &extra, "freed capacity").await;
        });
    }

    #[test]
    fn history_capacity_prunes_only_terminal_messages() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let recipients = [
                peer(a.space, "Bob"),
                peer(a.space, "Carol"),
                peer(a.space, "Dave"),
            ];
            let mut peers = vec![a.clone()];
            peers.extend(recipients.iter().cloned());
            fixture.join(&peers).await;
            let mut ids = Vec::new();
            for index in 0..MAX_MESSAGES {
                ids.push(
                    fixture
                        .send(&a, &recipients[index % recipients.len()], "pending history")
                        .await,
                );
            }
            assert!(
                fixture
                    .network
                    .call(
                        token(&a),
                        Action::Send {
                            recipient: recipients[1].thread,
                            body: "full history".into()
                        }
                    )
                    .await
                    .is_err()
            );
            assert_eq!(fixture.saved().await.messages.len(), MAX_MESSAGES);
            fixture
                .network
                .call(token(&recipients[0]), Action::Inbox)
                .await
                .unwrap();
            fixture
                .send(&a, &recipients[1], "evicts oldest retrieved only")
                .await;
            let saved = fixture.saved().await;
            assert_eq!(saved.messages.len(), MAX_MESSAGES);
            assert!(!saved.messages.iter().any(|message| message.id == ids[0]));
            for (index, id) in ids.iter().enumerate().skip(1) {
                if index % recipients.len() != 0 {
                    assert!(saved.messages.iter().any(|message| message.id == *id));
                }
            }
        });
    }

    #[test]
    fn bodies_are_byte_bounded_and_large_escaped_inboxes_are_retrieved_in_transport_safe_batches() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            for body in [" \n\t ".to_owned(), "é".repeat(MAX_BODY_BYTES / 2 + 1)] {
                assert!(
                    fixture
                        .network
                        .call(
                            token(&a),
                            Action::Send {
                                recipient: b.thread,
                                body
                            }
                        )
                        .await
                        .is_err()
                );
            }
            assert!(fixture.saved().await.messages.is_empty());
            let body = "\0".repeat(MAX_BODY_BYTES);
            for _ in 0..24 {
                fixture.send(&a, &b, &body).await;
            }
            assert!(
                fixture
                    .saved()
                    .await
                    .messages
                    .iter()
                    .all(|message| !message.deliveries[0].retrieved)
            );
            let mut retrieved = HashSet::new();
            let mut batches = 0;
            loop {
                let inbox = fixture
                    .network
                    .call(token(&b), Action::Inbox)
                    .await
                    .unwrap();
                assert!(serde_json::to_vec(&inbox).unwrap().len() <= MAX_INBOX_BYTES);
                let batch = messages(&inbox);
                assert_eq!(inbox["remaining"], 24 - retrieved.len() - batch.len());
                if batch.is_empty() {
                    break;
                }
                batches += 1;
                for message in batch {
                    assert!(retrieved.insert(message.id));
                    assert_eq!(message.body, body);
                }
            }
            assert_eq!(retrieved.len(), 24);
            assert!(batches > 1);
            assert!(
                fixture
                    .saved()
                    .await
                    .messages
                    .iter()
                    .all(|message| message.deliveries[0].retrieved)
            );
        });
    }

    #[test]
    fn failed_writes_never_accept_send_retrieval_or_stale_membership_invalidation() {
        smol::block_on(async {
            for operation in ["send", "inbox", "move", "leave"] {
                let fixture = Fixture::new();
                let a = peer(Uuid::new_v4(), "Alice");
                let mut b = peer(a.space, "Bob");
                fixture.join(&[a.clone(), b.clone()]).await;
                fixture.send(&a, &b, "last committed pending message").await;
                let before = fixture.saved().await;
                let blocker =
                    rusqlite::Connection::open(fixture.directory.0.join("drostje.sqlite3"))
                        .unwrap();
                blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
                let result = match operation {
                    "send" => fixture
                        .network
                        .call(
                            token(&a),
                            Action::Send {
                                recipient: b.thread,
                                body: "must not appear".into(),
                            },
                        )
                        .await
                        .map(|_| ()),
                    "inbox" => fixture
                        .network
                        .call(token(&b), Action::Inbox)
                        .await
                        .map(|_| ()),
                    "move" => {
                        b.space = Uuid::new_v4();
                        fixture.network.sync(vec![a.clone(), b.clone()]).await
                    }
                    "leave" => fixture.network.set_joined(b.thread, false).await,
                    _ => unreachable!(),
                };
                assert!(
                    result.is_err(),
                    "{operation} must report persistence failure"
                );
                let failed = fixture.updates.try_recv().unwrap();
                assert!(failed.error.is_some());
                assert!(failed.members.iter().all(|member| !member.online));
                assert_eq!(failed.messages, before.messages);
                assert_eq!(fixture.saved().await, before);
                assert!(
                    fixture
                        .network
                        .call(token(&a), Action::Members)
                        .await
                        .is_err()
                );
                assert!(
                    fixture
                        .network
                        .call(token(&b), Action::Inbox)
                        .await
                        .is_err()
                );
                blocker.execute_batch("ROLLBACK").unwrap();
                assert!(
                    fixture.network.sync(vec![a, b]).await.is_err(),
                    "A failed actor cannot silently re-enable itself"
                );
            }
        });
    }

    #[test]
    fn member_capacity_and_saved_state_validation_fail_honestly_without_losing_state() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let space = Uuid::new_v4();
            let peers = (0..=MAX_MEMBERS)
                .map(|index| peer(space, &format!("member {index}")))
                .collect::<Vec<_>>();
            fixture.network.sync(peers.clone()).await.unwrap();
            for peer in peers.iter().take(MAX_MEMBERS) {
                fixture.network.set_joined(peer.thread, true).await.unwrap();
            }
            let before = fixture.saved().await;
            assert!(
                fixture
                    .network
                    .set_joined(peers[MAX_MEMBERS].thread, true)
                    .await
                    .is_err()
            );
            assert_eq!(fixture.saved().await, before);
            let mut corrupt = before.clone();
            corrupt.members.push(corrupt.members[0].clone());
            assert!(
                Network::start(
                    fixture.store.clone(),
                    Some(&serde_json::to_value(corrupt).unwrap())
                )
                .is_err()
            );
            let mut corrupt = before;
            corrupt.members[1].thread = corrupt.members[0].thread;
            assert!(
                Network::start(
                    fixture.store.clone(),
                    Some(&serde_json::to_value(corrupt).unwrap())
                )
                .is_err()
            );
            assert!(
                Network::start(
                    fixture.store.clone(),
                    Some(&json!({"members": [], "messages": [], "token": Uuid::new_v4()}))
                )
                .is_err()
            );
        });
    }
}
