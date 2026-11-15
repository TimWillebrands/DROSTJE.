use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use async_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use drostje_store::Store;

pub const SETTING: &str = "space_network";
pub const MAX_BODY_BYTES: usize = 8192;
pub const MAX_MEMBERS: usize = 256;
pub const MAX_MESSAGES: usize = 256;
pub const MAX_PENDING: usize = 1024;
pub const MAX_PENDING_PER_RECIPIENT: usize = 128;
pub const MAX_BATCH_BYTES: usize = 64 * 1024;
pub const DORMANT_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
const MAX_CONTEXT_BYTES: usize = 8192;
const MAX_RESPONSE_BYTES: usize = 384 * 1024;

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
    pub delivery_enabled: bool,
    pub can_receive: bool,
    pub dormant: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSnapshot {
    pub members: Vec<Member>,
    pub messages: Vec<NetworkMessage>,
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
    #[serde(default)]
    pub automatic: bool,
    #[serde(default)]
    pub delivery_enabled: bool,
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
    pub state: DeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeliveryState {
    Queued,
    Delivering,
    Delivered,
    Failed { reason: String },
    Unknown { reason: String },
    Closed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryBatch {
    pub attempt: Uuid,
    pub recipient: Uuid,
    pub messages: Vec<NetworkMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Delivered,
    Failed(String),
    Unknown(String),
    Deferred,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Members,
    Send { recipient: Uuid, body: String },
    Broadcast { body: String },
}

enum Command {
    Sync(Vec<Peer>, Reply<()>),
    SetJoined(Uuid, bool, Reply<()>),
    Barrier(Reply<()>),
    Call(Uuid, Action, Reply<Value>),
    Claim(Uuid, Reply<Option<DeliveryBatch>>),
    Finish(Uuid, DeliveryOutcome, Reply<()>),
}

impl Network {
    pub fn start(
        store: Store,
        saved: Option<&Value>,
    ) -> Result<(Self, Receiver<Arc<NetworkSnapshot>>)> {
        let mut snapshot = match saved {
            Some(value) => read_saved(value)?,
            None => NetworkSnapshot::default(),
        };
        validate_snapshot(&snapshot).context("Invalid saved Space network")?;
        for member in &mut snapshot.members {
            member.online = false;
            member.delivery_enabled = false;
            member.status = "Offline".into();
        }
        for delivery in snapshot
            .messages
            .iter_mut()
            .flat_map(|message| &mut message.deliveries)
        {
            if delivery.state == DeliveryState::Delivering {
                delivery.state = DeliveryState::Unknown {
                    reason: "Application restarted before delivery receipt".into(),
                };
            }
        }
        snapshot.error = None;
        let startup_write = saved
            .map(|value| serde_json::to_value(&snapshot).map(|normalized| normalized != *value))
            .transpose()?
            .unwrap_or(false);
        let snapshot = Arc::new(snapshot);
        let (commands, requests) = async_channel::bounded(64);
        let (updates, receiver) = async_channel::bounded(1);
        let pending_updates = receiver.clone();
        if !startup_write {
            updates
                .try_send(Arc::clone(&snapshot))
                .map_err(|_| anyhow!("Initialize Space network snapshot channel"))?;
        }
        let actor = Actor {
            store,
            snapshot,
            peers: BTreeMap::new(),
            updates,
            pending_updates,
        };
        smol::spawn(actor.run(requests, startup_write)).detach();
        Ok((Self { commands }, receiver))
    }

    pub async fn sync(&self, peers: Vec<Peer>) -> Result<()> {
        self.request(|reply| Command::Sync(peers, reply)).await
    }

    pub async fn set_joined(&self, thread: Uuid, joined: bool) -> Result<()> {
        self.request(|reply| Command::SetJoined(thread, joined, reply))
            .await
    }

    pub async fn flush(&self) -> Result<()> {
        self.request(Command::Barrier).await
    }

    pub async fn call(&self, token: Uuid, action: Action) -> Result<Value> {
        self.request(|reply| Command::Call(token, action, reply))
            .await
    }

    pub async fn claim(&self, token: Uuid) -> Result<Option<DeliveryBatch>> {
        self.request(|reply| Command::Claim(token, reply)).await
    }

    pub async fn finish(&self, attempt: Uuid, outcome: DeliveryOutcome) -> Result<()> {
        self.request(|reply| Command::Finish(attempt, outcome, reply))
            .await
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
    snapshot: Arc<NetworkSnapshot>,
    peers: BTreeMap<Uuid, Peer>,
    updates: Sender<Arc<NetworkSnapshot>>,
    pending_updates: Receiver<Arc<NetworkSnapshot>>,
}

impl Actor {
    async fn run(mut self, requests: Receiver<Command>, startup_write: bool) {
        if startup_write {
            let result = self
                .store
                .set_setting_encoded(SETTING.into(), &*self.snapshot)
                .await;
            if let Err(error) = result {
                self.disable(&error);
            } else {
                self.publish();
            }
        }
        while let Ok(command) = requests.recv().await {
            match command {
                Command::Sync(peers, reply) => {
                    let _ = reply.try_send(self.sync(peers).await);
                }
                Command::SetJoined(thread, joined, reply) => {
                    let _ = reply.try_send(self.set_joined(thread, joined).await);
                }
                Command::Barrier(reply) => {
                    let _ = reply.try_send(self.healthy());
                }
                Command::Call(token, action, reply) => {
                    let _ = reply.try_send(self.call(token, action).await);
                }
                Command::Claim(token, reply) => {
                    let _ = reply.try_send(self.claim(token).await);
                }
                Command::Finish(attempt, outcome, reply) => {
                    let _ = reply.try_send(self.finish(attempt, outcome).await);
                }
            }
        }
    }

    fn publish(&self) {
        let _ = self.pending_updates.try_recv();
        let _ = self.updates.try_send(Arc::clone(&self.snapshot));
    }

    fn healthy(&self) -> Result<()> {
        if let Some(error) = &self.snapshot.error {
            bail!("Space network is disabled after a persistence failure: {error}");
        }
        Ok(())
    }

    fn disable(&mut self, error: &anyhow::Error) {
        self.peers.clear();
        let snapshot = Arc::make_mut(&mut self.snapshot);
        for member in &mut snapshot.members {
            member.online = false;
            member.delivery_enabled = false;
        }
        snapshot.error = Some(format!(
            "{error:#}. Restore writable storage, then reopen the application"
        ));
        self.publish();
    }

    async fn commit(&mut self, next: NetworkSnapshot) -> Result<()> {
        self.healthy()?;
        if let Err(error) = self.store.set_setting_encoded(SETTING.into(), &next).await {
            self.disable(&error);
            return Err(error.context("Persist Space network; network disabled"));
        }
        self.snapshot = Arc::new(next);
        self.publish();
        Ok(())
    }

    async fn sync(&mut self, peers: Vec<Peer>) -> Result<()> {
        let previous = std::mem::take(&mut self.peers);
        self.healthy()?;
        let contexts = (|| -> Result<BTreeMap<Uuid, Peer>> {
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
                ensure!(
                    contexts.insert(peer.thread, peer).is_none(),
                    "Duplicate Space network thread context"
                );
            }
            Ok(contexts)
        })();
        let contexts = match contexts {
            Ok(contexts) => contexts,
            Err(error) => {
                let mut next = NetworkSnapshot::clone(&self.snapshot);
                let mut changed = false;
                for member in &mut next.members {
                    changed |= member.online || member.delivery_enabled;
                    member.online = false;
                    member.delivery_enabled = false;
                    changed |= revoke_attempts(
                        &mut next.messages,
                        member.thread,
                        "Runtime context invalidated",
                    );
                }
                if changed {
                    self.commit(next).await?;
                }
                return Err(error);
            }
        };
        let membership_changed = self.snapshot.members.iter().any(|member| {
            contexts
                .get(&member.thread)
                .is_none_or(|peer| !peer.eligible || peer.dormant || peer.space != member.space)
        });
        let revoked_attempt = self
            .snapshot
            .messages
            .iter()
            .flat_map(|message| &message.deliveries)
            .any(|delivery| {
                delivery.state == DeliveryState::Delivering
                    && contexts.get(&delivery.recipient).is_none_or(|peer| {
                        peer.token.is_none()
                            || !peer.delivery_enabled
                            || previous
                                .get(&delivery.recipient)
                                .is_none_or(|old| old.token != peer.token)
                    })
            });
        if !membership_changed && !revoked_attempt {
            let changed = self
                .snapshot
                .members
                .iter()
                .any(|member| member.stale(&contexts[&member.thread]));
            if changed {
                for member in &mut Arc::make_mut(&mut self.snapshot).members {
                    member.refresh(&contexts[&member.thread]);
                }
            }
            self.peers = contexts;
            if changed {
                self.publish();
            }
            return Ok(());
        }
        let mut next = NetworkSnapshot::clone(&self.snapshot);
        let mut changed = false;
        next.members.retain_mut(|member| {
            let peer = match contexts.get(&member.thread) {
                Some(peer) if peer.eligible && !peer.dormant && peer.space == member.space => peer,
                Some(peer) if peer.space != member.space => {
                    close_deliveries(&mut next.messages, member.thread, "moved");
                    changed = true;
                    return false;
                }
                Some(peer) if peer.dormant => {
                    close_deliveries(&mut next.messages, member.thread, "inactive");
                    changed = true;
                    return false;
                }
                _ => {
                    close_deliveries(&mut next.messages, member.thread, "unavailable");
                    changed = true;
                    return false;
                }
            };
            if peer.token.is_none()
                || !peer.delivery_enabled
                || previous
                    .get(&member.thread)
                    .is_none_or(|old| old.token != peer.token)
            {
                changed |= revoke_attempts(
                    &mut next.messages,
                    member.thread,
                    "Runtime authorization revoked",
                );
            }
            changed |= member.refresh(peer);
            true
        });
        if changed {
            self.commit(next).await?;
        }
        self.peers = contexts;
        Ok(())
    }

    async fn set_joined(&mut self, thread: Uuid, joined: bool) -> Result<()> {
        self.healthy()?;
        let mut next = NetworkSnapshot::clone(&self.snapshot);
        let changed;
        if joined {
            let peer = self
                .peers
                .get(&thread)
                .context("Conversation context is unavailable")?;
            ensure!(
                peer.eligible,
                "Only eligible Conversations may join the Space network"
            );
            if let Some(member) = next
                .members
                .iter_mut()
                .find(|member| member.thread == thread)
            {
                let consented = std::mem::replace(&mut member.automatic, true);
                changed = member.refresh(peer) || !consented;
            } else {
                ensure!(
                    next.members.len() < MAX_MEMBERS,
                    "Space network member capacity reached ({MAX_MEMBERS})"
                );
                let mut member = Member::from(peer);
                member.automatic = true;
                member.refresh(peer);
                next.members.push(member);
                next.members.sort_by_key(|member| member.thread);
                changed = true;
            }
        } else {
            let members = next.members.len();
            next.members.retain(|member| member.thread != thread);
            let closed = close_deliveries(&mut next.messages, thread, "left");
            changed = closed || next.members.len() != members;
        }
        if !changed {
            return Ok(());
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
        }
    }

    async fn claim(&mut self, token: Uuid) -> Result<Option<DeliveryBatch>> {
        let caller = self.caller(token)?;
        let peer = &self.peers[&caller.thread];
        if !caller.automatic || !peer.delivery_enabled || !peer.can_receive {
            return Ok(None);
        }
        if self
            .snapshot
            .messages
            .iter()
            .flat_map(|message| &message.deliveries)
            .any(|delivery| {
                delivery.recipient == caller.thread && delivery.state == DeliveryState::Delivering
            })
        {
            return Ok(None);
        }
        let mut batch = DeliveryBatch {
            attempt: Uuid::new_v4(),
            recipient: caller.thread,
            messages: Vec::new(),
        };
        let mut bytes = encoded_len(&batch)?;
        for message in self
            .snapshot
            .messages
            .iter()
            .filter(|message| message.space == caller.space)
        {
            let Some(delivery) = message.deliveries.iter().find(|delivery| {
                delivery.recipient == caller.thread && delivery.state == DeliveryState::Queued
            }) else {
                continue;
            };
            let mut message = recipient_message(message, delivery);
            message.deliveries[0].state = DeliveryState::Delivering;
            message.deliveries[0].attempt = Some(batch.attempt);
            let size = encoded_len(&message)? + usize::from(!batch.messages.is_empty());
            if bytes + size > MAX_BATCH_BYTES && !batch.messages.is_empty() {
                break;
            }
            bytes += size;
            batch.messages.push(message);
        }
        if batch.messages.is_empty() {
            return Ok(None);
        }
        let ids = batch
            .messages
            .iter()
            .map(|message| message.id)
            .collect::<HashSet<_>>();
        let mut next = NetworkSnapshot::clone(&self.snapshot);
        for message in &mut next.messages {
            if ids.contains(&message.id) {
                for delivery in &mut message.deliveries {
                    if delivery.recipient == batch.recipient {
                        delivery.state = DeliveryState::Delivering;
                        delivery.attempt = Some(batch.attempt);
                    }
                }
            }
        }
        self.commit(next).await?;
        Ok(Some(batch))
    }

    async fn finish(&mut self, attempt: Uuid, outcome: DeliveryOutcome) -> Result<()> {
        self.healthy()?;
        let state = match outcome {
            DeliveryOutcome::Delivered => DeliveryState::Delivered,
            DeliveryOutcome::Deferred => DeliveryState::Queued,
            DeliveryOutcome::Failed(reason) => {
                ensure!(
                    reason.len() <= MAX_CONTEXT_BYTES,
                    "Delivery reason exceeds {MAX_CONTEXT_BYTES} bytes"
                );
                DeliveryState::Failed { reason }
            }
            DeliveryOutcome::Unknown(reason) => {
                ensure!(
                    reason.len() <= MAX_CONTEXT_BYTES,
                    "Delivery reason exceeds {MAX_CONTEXT_BYTES} bytes"
                );
                DeliveryState::Unknown { reason }
            }
        };
        let resolves = |delivery: &Delivery| {
            delivery.attempt == Some(attempt)
                && (delivery.state == DeliveryState::Delivering
                    || (matches!(delivery.state, DeliveryState::Unknown { .. })
                        && matches!(
                            state,
                            DeliveryState::Delivered | DeliveryState::Failed { .. }
                        )))
        };
        if !self
            .snapshot
            .messages
            .iter()
            .flat_map(|message| &message.deliveries)
            .any(resolves)
        {
            return Ok(());
        }
        let mut next = NetworkSnapshot::clone(&self.snapshot);
        for delivery in next
            .messages
            .iter_mut()
            .flat_map(|message| &mut message.deliveries)
        {
            if resolves(delivery) {
                delivery.state = state.clone();
                if state == DeliveryState::Queued {
                    delivery.attempt = None;
                }
            }
        }
        self.commit(next).await
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
                "Pending delivery capacity reached for {} ({MAX_PENDING_PER_RECIPIENT})",
                recipient.title
            );
        }
        let mut next = NetworkSnapshot::clone(&self.snapshot);
        if next.messages.len() >= MAX_MESSAGES {
            let index = next
                .messages
                .iter()
                .position(|message| !message.deliveries.iter().any(Delivery::pending))
                .context(
                    "Space network history capacity reached; pending deliveries must finish first",
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
            created_at: drostje_platform::now(),
            deliveries: recipients
                .into_iter()
                .map(|recipient| Delivery {
                    recipient: recipient.thread,
                    recipient_name: recipient.title,
                    state: DeliveryState::Queued,
                    attempt: None,
                })
                .collect(),
        };
        let response = bounded_response(json!({"message": &message}))?;
        next.messages.push(message);
        self.commit(next).await?;
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
            automatic: false,
            delivery_enabled: false,
        }
    }
}

impl Member {
    fn stale(&self, peer: &Peer) -> bool {
        let online = peer.token.is_some();
        let enabled = self.automatic && peer.delivery_enabled && online;
        self.online != online
            || self.delivery_enabled != enabled
            || self.title != peer.title
            || self.project != peer.project
            || self.worktree != peer.worktree
            || self.harness != peer.harness
            || self.status != peer.status
    }

    fn refresh(&mut self, peer: &Peer) -> bool {
        let online = peer.token.is_some();
        let enabled = self.automatic && peer.delivery_enabled && online;
        let mut changed = self.online != online || self.delivery_enabled != enabled;
        for (value, current) in [
            (&mut self.title, &peer.title),
            (&mut self.project, &peer.project),
            (&mut self.worktree, &peer.worktree),
            (&mut self.harness, &peer.harness),
            (&mut self.status, &peer.status),
        ] {
            if *value != *current {
                value.clone_from(current);
                changed = true;
            }
        }
        self.online = online;
        self.delivery_enabled = enabled;
        changed
    }
}

impl Delivery {
    pub fn pending(&self) -> bool {
        matches!(
            self.state,
            DeliveryState::Queued | DeliveryState::Delivering
        )
    }
}

fn revoke_attempts(messages: &mut [NetworkMessage], thread: Uuid, reason: &str) -> bool {
    let mut revoked = false;
    for delivery in messages
        .iter_mut()
        .flat_map(|message| &mut message.deliveries)
    {
        if delivery.recipient == thread && delivery.state == DeliveryState::Delivering {
            delivery.state = DeliveryState::Unknown {
                reason: reason.into(),
            };
            revoked = true;
        }
    }
    revoked
}

fn close_deliveries(messages: &mut [NetworkMessage], thread: Uuid, reason: &str) -> bool {
    let mut closed = false;
    for delivery in messages
        .iter_mut()
        .flat_map(|message| &mut message.deliveries)
    {
        if delivery.recipient == thread {
            delivery.state = match delivery.state {
                DeliveryState::Queued => DeliveryState::Closed {
                    reason: reason.into(),
                },
                DeliveryState::Delivering => DeliveryState::Unknown {
                    reason: reason.into(),
                },
                _ => continue,
            };
            closed = true;
        }
    }
    closed
}

fn recipient_message(message: &NetworkMessage, delivery: &Delivery) -> NetworkMessage {
    NetworkMessage {
        id: message.id,
        space: message.space,
        sender: message.sender,
        sender_name: message.sender_name.clone(),
        body: message.body.clone(),
        broadcast: message.broadcast,
        created_at: message.created_at.clone(),
        deliveries: vec![delivery.clone()],
    }
}

fn read_saved(saved: &Value) -> Result<NetworkSnapshot> {
    const LEGACY_RECEIPT: &str =
        "Retrieved through the legacy inbox without a native delivery receipt";
    let mut value = saved.clone();
    if let Some(messages) = value.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if let Some(deliveries) = message.get_mut("deliveries").and_then(Value::as_array_mut) {
                for delivery in deliveries {
                    let fields = delivery.as_object_mut().context("Invalid saved delivery")?;
                    if fields.contains_key("state") {
                        ensure!(
                            !fields.contains_key("retrieved") && !fields.contains_key("closed"),
                            "Mixed saved delivery formats"
                        );
                        if fields["state"]["status"] == "delivered"
                            && fields.get("attempt").is_none_or(Value::is_null)
                        {
                            fields.insert(
                                "state".into(),
                                serde_json::to_value(DeliveryState::Unknown {
                                    reason: LEGACY_RECEIPT.into(),
                                })?,
                            );
                        }
                        continue;
                    }
                    ensure!(
                        !fields.contains_key("attempt"),
                        "Legacy delivery has an attempt"
                    );
                    let retrieved = fields
                        .remove("retrieved")
                        .context("Saved delivery has no state")?
                        .as_bool()
                        .context("Invalid legacy retrieval flag")?;
                    let closed = fields.remove("closed");
                    let state = match closed {
                        Some(Value::String(reason)) => {
                            ensure!(!retrieved, "Conflicting legacy delivery dispositions");
                            DeliveryState::Closed { reason }
                        }
                        Some(Value::Null) | None if retrieved => DeliveryState::Unknown {
                            reason: LEGACY_RECEIPT.into(),
                        },
                        Some(Value::Null) | None => DeliveryState::Queued,
                        _ => bail!("Invalid legacy delivery disposition"),
                    };
                    fields.insert("state".into(), serde_json::to_value(state)?);
                }
            }
        }
    }
    serde_json::from_value(value).context("Read Space network")
}

fn encoded_len(value: &impl Serialize) -> Result<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}

fn bounded_response(response: Value) -> Result<Value> {
    ensure!(
        encoded_len(&response)? <= MAX_RESPONSE_BYTES,
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
    let mut attempts = HashMap::<Uuid, Uuid>::new();
    let mut in_flight = HashMap::<Uuid, Uuid>::new();
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
            match &delivery.state {
                DeliveryState::Queued | DeliveryState::Closed { .. } => {
                    ensure!(
                        delivery.attempt.is_none(),
                        "Unclaimed saved delivery has an attempt"
                    );
                }
                DeliveryState::Delivering
                | DeliveryState::Delivered
                | DeliveryState::Failed { .. } => {
                    ensure!(
                        delivery.attempt.is_some(),
                        "Saved delivery is missing its attempt"
                    );
                }
                DeliveryState::Unknown { .. } => {}
            }
            match &delivery.state {
                DeliveryState::Failed { reason }
                | DeliveryState::Unknown { reason }
                | DeliveryState::Closed { reason } => {
                    ensure!(
                        reason.len() <= MAX_CONTEXT_BYTES,
                        "Oversized saved delivery reason"
                    );
                }
                _ => {}
            }
            if let Some(attempt) = delivery.attempt {
                ensure!(
                    attempts
                        .insert(attempt, delivery.recipient)
                        .is_none_or(|recipient| recipient == delivery.recipient),
                    "Saved attempt belongs to multiple recipients"
                );
                if delivery.state == DeliveryState::Delivering {
                    ensure!(
                        in_flight
                            .insert(delivery.recipient, attempt)
                            .is_none_or(|old| old == attempt),
                        "Recipient has multiple saved in-flight batches"
                    );
                }
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
                    "Saved pending deliveries exceed capacity"
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
    use drostje_store::StoreError;
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
        updates: Receiver<Arc<NetworkSnapshot>>,
        store: Store,
        directory: Directory,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = Directory::new();
            let store = Store::open(directory.0.clone()).unwrap();
            let (network, updates) = Network::start(store.clone(), None).unwrap();
            assert_eq!(*updates.try_recv().unwrap(), NetworkSnapshot::default());
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
            delivery_enabled: true,
            can_receive: true,
            dormant: false,
        }
    }

    fn token(peer: &Peer) -> Uuid {
        peer.token.unwrap()
    }

    async fn within<T>(future: impl Future<Output = T>) -> T {
        smol::future::or(future, async {
            smol::Timer::after(Duration::from_secs(10)).await;
            panic!("Space network operation did not complete");
        })
        .await
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
    fn requires_explicit_membership_current_capability_and_same_space() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            let c = peer(Uuid::new_v4(), "Carol");
            let mut factory = peer(a.space, "Factory");
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
            assert!(fixture.network.claim(token(&a)).await.is_err());
            assert!(
                fixture
                    .network
                    .set_joined(factory.thread, true)
                    .await
                    .is_err()
            );
            for joined in [&a, &b, &c] {
                fixture
                    .network
                    .set_joined(joined.thread, true)
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
            assert!(fixture.network.claim(Uuid::new_v4()).await.is_err());
            let response = fixture
                .network
                .call(token(&a), Action::Members)
                .await
                .unwrap();
            let members: Vec<Member> = serde_json::from_value(response["members"].clone()).unwrap();
            assert_eq!(members.len(), 2);
            assert!(
                members
                    .iter()
                    .all(|member| member.space == a.space && member.automatic)
            );
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
            fixture.send(&a, &b, "  untrusted peer context  ").await;
            assert!(fixture.network.claim(token(&a)).await.unwrap().is_none());
            assert!(fixture.network.claim(token(&c)).await.unwrap().is_none());
            let batch = fixture.network.claim(token(&b)).await.unwrap().unwrap();
            assert_eq!(batch.messages[0].body, "untrusted peer context");
            assert_eq!(batch.messages[0].sender, a.thread);
            let encoded = serde_json::to_string(&fixture.saved().await).unwrap();
            for peer in [&a, &b, &c] {
                assert!(!encoded.contains(&token(peer).to_string()));
            }
            assert!(
                !serde_json::to_string(&batch)
                    .unwrap()
                    .contains(&token(&b).to_string())
            );
            for action in [
                json!({"action": "inbox"}),
                json!({"action": "wait"}),
                json!({"action": "send", "recipient": b.thread, "body": "spoof", "sender": c.thread}),
                json!({"action": "send", "recipient": b.thread}),
            ] {
                assert!(serde_json::from_value::<Action>(action).is_err());
            }
        });
    }

    #[test]
    fn legacy_opt_in_migrates_durably_without_authorizing_automatic_delivery() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            let mut ids = Vec::new();
            for body in ["already retrieved", "closed", "queued"] {
                ids.push(fixture.send(&a, &b, body).await);
            }
            let mut legacy = serde_json::to_value(fixture.saved().await).unwrap();
            for member in legacy["members"].as_array_mut().unwrap() {
                member.as_object_mut().unwrap().remove("automatic");
                member.as_object_mut().unwrap().remove("delivery_enabled");
            }
            for (index, message) in legacy["messages"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .enumerate()
            {
                let delivery = message["deliveries"][0].as_object_mut().unwrap();
                delivery.remove("state");
                delivery.insert("retrieved".into(), json!(index == 0));
                delivery.insert(
                    "closed".into(),
                    if index == 1 {
                        json!("left")
                    } else {
                        Value::Null
                    },
                );
            }
            let Fixture {
                network,
                updates,
                store,
                directory: _directory,
            } = fixture;
            drop(network);
            drop(updates);
            store
                .set_setting(SETTING.into(), legacy.clone())
                .await
                .unwrap();
            let (network, updates) = Network::start(store.clone(), Some(&legacy)).unwrap();
            network.flush().await.unwrap();
            let migrated = updates.recv().await.unwrap();
            assert!(
                migrated
                    .members
                    .iter()
                    .all(|member| !member.automatic && !member.delivery_enabled && !member.online)
            );
            assert_eq!(
                migrated
                    .messages
                    .iter()
                    .map(|message| message.id)
                    .collect::<Vec<_>>(),
                ids
            );
            assert!(matches!(
                migrated.messages[0].deliveries[0].state,
                DeliveryState::Unknown { .. }
            ));
            assert!(migrated.messages[0].deliveries[0].attempt.is_none());
            let mut prior = serde_json::to_value(&*migrated).unwrap();
            prior["messages"][0]["deliveries"][0]["state"] = json!({"status":"delivered"});
            assert!(matches!(
                read_saved(&prior).unwrap().messages[0].deliveries[0].state,
                DeliveryState::Unknown { .. }
            ));
            assert_eq!(
                migrated.messages[1].deliveries[0].state,
                DeliveryState::Closed {
                    reason: "left".into()
                }
            );
            assert_eq!(
                migrated.messages[2].deliveries[0].state,
                DeliveryState::Queued
            );
            let saved = store.snapshot().await.unwrap().settings[SETTING].clone();
            assert_eq!(saved, serde_json::to_value(&*migrated).unwrap());
            for message in saved["messages"].as_array().unwrap() {
                assert!(message["deliveries"][0].get("retrieved").is_none());
                assert!(message["deliveries"][0].get("closed").is_none());
            }
            assert!(network.claim(token(&b)).await.is_err());
            network.sync(vec![a, b.clone()]).await.unwrap();
            assert!(network.claim(token(&b)).await.unwrap().is_none());
            network.set_joined(b.thread, true).await.unwrap();
            let batch = network.claim(token(&b)).await.unwrap().unwrap();
            assert_eq!(batch.messages.len(), 1);
            assert_eq!(batch.messages[0].id, ids[2]);
            let saved: NetworkSnapshot =
                serde_json::from_value(store.snapshot().await.unwrap().settings[SETTING].clone())
                    .unwrap();
            assert!(
                saved
                    .members
                    .iter()
                    .find(|member| member.thread == b.thread)
                    .unwrap()
                    .automatic
            );
        });
    }

    #[test]
    fn claim_is_committed_before_return_and_only_one_batch_can_be_in_flight() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            let first = fixture.send(&a, &b, "first").await;
            let second = fixture.send(&a, &b, "second").await;
            let blocker =
                rusqlite::Connection::open(fixture.directory.0.join("drostje.sqlite3")).unwrap();
            blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
            let (reply, response) = async_channel::bounded(1);
            let network = fixture.network.clone();
            let capability = token(&b);
            smol::spawn(async move {
                let _ = reply.send(network.claim(capability).await).await;
            })
            .detach();
            smol::Timer::after(Duration::from_millis(20)).await;
            assert!(
                response.is_empty(),
                "host must not receive an uncommitted claim"
            );
            blocker.execute_batch("ROLLBACK").unwrap();
            let batch = within(response.recv()).await.unwrap().unwrap().unwrap();
            assert_eq!(batch.recipient, b.thread);
            assert_eq!(
                batch
                    .messages
                    .iter()
                    .map(|message| message.id)
                    .collect::<Vec<_>>(),
                vec![first, second]
            );
            let saved = fixture.saved().await;
            for message in &saved.messages {
                assert_eq!(message.deliveries[0].state, DeliveryState::Delivering);
                assert_eq!(message.deliveries[0].attempt, Some(batch.attempt));
            }
            fixture.send(&a, &b, "next batch").await;
            assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
            fixture
                .network
                .finish(batch.attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
            let next = fixture.network.claim(token(&b)).await.unwrap().unwrap();
            assert_ne!(batch.attempt, next.attempt);
            assert_eq!(next.messages.len(), 1);
            assert_eq!(next.messages[0].body, "next batch");
        });
    }

    #[test]
    fn busy_and_idle_delivery_require_readiness_without_revoking_in_flight_attempts() {
        smol::block_on(async {
            for status in ["busy", "idle"] {
                let fixture = Fixture::new();
                let a = peer(Uuid::new_v4(), "Alice");
                let mut b = peer(a.space, "Bob");
                b.status = status.into();
                b.can_receive = false;
                fixture.join(&[a.clone(), b.clone()]).await;
                let id = fixture.send(&a, &b, status).await;
                assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
                assert_eq!(
                    fixture.saved().await.messages[0].deliveries[0].state,
                    DeliveryState::Queued
                );
                b.can_receive = true;
                fixture
                    .network
                    .sync(vec![a.clone(), b.clone()])
                    .await
                    .unwrap();
                let batch = fixture.network.claim(token(&b)).await.unwrap().unwrap();
                assert_eq!(batch.messages[0].id, id);
                b.can_receive = false;
                fixture
                    .network
                    .sync(vec![a.clone(), b.clone()])
                    .await
                    .unwrap();
                assert_eq!(
                    fixture.saved().await.messages[0].deliveries[0].state,
                    DeliveryState::Delivering
                );
                fixture
                    .network
                    .finish(batch.attempt, DeliveryOutcome::Delivered)
                    .await
                    .unwrap();
                assert_eq!(
                    fixture.saved().await.messages[0].deliveries[0].state,
                    DeliveryState::Delivered
                );
                assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
            }
        });
    }

    #[test]
    fn offline_and_disabled_runtime_queue_mail_until_current_authorization_returns() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let mut a = peer(Uuid::new_v4(), "Original sender");
            let mut b = peer(a.space, "Original recipient");
            fixture.join(&[a.clone(), b.clone()]).await;
            let old_token = token(&b);
            b.token = None;
            b.delivery_enabled = false;
            b.status = "Offline".into();
            fixture
                .network
                .sync(vec![a.clone(), b.clone()])
                .await
                .unwrap();
            let id = fixture.send(&a, &b, "offline queue").await;
            assert!(fixture.network.claim(old_token).await.is_err());
            a.title = "Renamed sender".into();
            b.title = "Renamed recipient".into();
            b.token = Some(Uuid::new_v4());
            fixture
                .network
                .sync(vec![a.clone(), b.clone()])
                .await
                .unwrap();
            assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
            assert!(fixture.network.claim(old_token).await.is_err());
            b.delivery_enabled = true;
            fixture
                .network
                .sync(vec![a.clone(), b.clone()])
                .await
                .unwrap();
            let _ = fixture.updates.try_recv();
            fixture.network.sync(vec![b.clone(), a]).await.unwrap();
            assert!(
                fixture.updates.try_recv().is_err(),
                "equal contexts should be quiet"
            );
            let batch = fixture.network.claim(token(&b)).await.unwrap().unwrap();
            assert_eq!(batch.messages[0].id, id);
            assert_eq!(batch.messages[0].sender_name, "Original sender");
            assert_eq!(
                batch.messages[0].deliveries[0].recipient_name,
                "Original recipient"
            );
        });
    }

    #[test]
    fn revocation_marks_attempt_unknown_and_context_changes_close_only_queued_mail() {
        smol::block_on(async {
            for operation in [
                "leave", "move", "archive", "missing", "rotate", "offline", "disable", "invalid",
            ] {
                let fixture = Fixture::new();
                let a = peer(Uuid::new_v4(), "Alice");
                let mut b = peer(a.space, "Bob");
                fixture.join(&[a.clone(), b.clone()]).await;
                fixture.send(&a, &b, "possibly accepted").await;
                let batch = fixture.network.claim(token(&b)).await.unwrap().unwrap();
                fixture.send(&a, &b, "still queued").await;
                match operation {
                    "leave" => fixture.network.set_joined(b.thread, false).await.unwrap(),
                    "missing" => fixture.network.sync(vec![a.clone()]).await.unwrap(),
                    "invalid" => assert!(
                        fixture
                            .network
                            .sync(vec![a.clone(), b.clone(), b.clone()])
                            .await
                            .is_err()
                    ),
                    _ => {
                        match operation {
                            "move" => b.space = Uuid::new_v4(),
                            "archive" => b.eligible = false,
                            "rotate" => b.token = Some(Uuid::new_v4()),
                            "offline" => b.token = None,
                            "disable" => b.delivery_enabled = false,
                            _ => unreachable!(),
                        }
                        fixture
                            .network
                            .sync(vec![a.clone(), b.clone()])
                            .await
                            .unwrap();
                    }
                }
                let saved = fixture.saved().await;
                assert!(
                    matches!(
                        saved.messages[0].deliveries[0].state,
                        DeliveryState::Unknown { .. }
                    ),
                    "{operation}"
                );
                assert_eq!(saved.messages[0].deliveries[0].attempt, Some(batch.attempt));
                let expected = match operation {
                    "leave" => DeliveryState::Closed {
                        reason: "left".into(),
                    },
                    "move" => DeliveryState::Closed {
                        reason: "moved".into(),
                    },
                    "archive" | "missing" => DeliveryState::Closed {
                        reason: "unavailable".into(),
                    },
                    _ => DeliveryState::Queued,
                };
                assert_eq!(
                    saved.messages[1].deliveries[0].state, expected,
                    "{operation}"
                );
                fixture
                    .network
                    .finish(batch.attempt, DeliveryOutcome::Delivered)
                    .await
                    .unwrap();
                assert_eq!(
                    fixture.saved().await.messages[0].deliveries[0].state,
                    DeliveryState::Delivered
                );
                b.space = a.space;
                b.eligible = true;
                b.token = Some(Uuid::new_v4());
                b.delivery_enabled = true;
                fixture.network.sync(vec![a, b.clone()]).await.unwrap();
                fixture.network.set_joined(b.thread, true).await.unwrap();
                let next = fixture.network.claim(token(&b)).await.unwrap();
                if matches!(expected, DeliveryState::Closed { .. }) {
                    assert!(next.is_none(), "rejoin cannot resurrect closed mail");
                } else {
                    assert_eq!(next.unwrap().messages[0].body, "still queued");
                }
            }
        });
    }

    #[test]
    fn stale_receipts_cannot_overwrite_other_attempts_or_final_outcomes() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            fixture.send(&a, &b, "unknown").await;
            let first = fixture.network.claim(token(&b)).await.unwrap().unwrap();
            fixture
                .network
                .finish(
                    first.attempt,
                    DeliveryOutcome::Unknown("connection lost".into()),
                )
                .await
                .unwrap();
            assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
            fixture.send(&a, &b, "new attempt").await;
            let second = fixture.network.claim(token(&b)).await.unwrap().unwrap();
            let before = fixture.saved().await;
            fixture
                .network
                .finish(Uuid::new_v4(), DeliveryOutcome::Delivered)
                .await
                .unwrap();
            assert_eq!(fixture.saved().await, before);
            fixture
                .network
                .finish(first.attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
            let saved = fixture.saved().await;
            assert_eq!(
                saved.messages[0].deliveries[0].state,
                DeliveryState::Delivered
            );
            assert_eq!(
                saved.messages[1].deliveries[0].state,
                DeliveryState::Delivering
            );
            assert_eq!(
                saved.messages[1].deliveries[0].attempt,
                Some(second.attempt)
            );
            fixture
                .network
                .finish(
                    first.attempt,
                    DeliveryOutcome::Unknown("stale failure".into()),
                )
                .await
                .unwrap();
            fixture
                .network
                .finish(
                    second.attempt,
                    DeliveryOutcome::Failed("native rejected".into()),
                )
                .await
                .unwrap();
            let before = fixture.saved().await;
            fixture
                .network
                .finish(second.attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
            fixture
                .network
                .finish(first.attempt, DeliveryOutcome::Failed("late".into()))
                .await
                .unwrap();
            assert_eq!(fixture.saved().await, before);
            assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
            assert!(
                !before
                    .messages
                    .iter()
                    .any(|message| message.deliveries[0].pending())
            );
        });
    }

    #[test]
    fn broadcasts_freeze_members_and_claims_expose_only_the_recipient_disposition() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            let c = peer(a.space, "Carol");
            let d = peer(a.space, "Late joiner");
            let elsewhere = peer(Uuid::new_v4(), "Other Space");
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
            fixture
                .join(&[a.clone(), b.clone(), c.clone(), elsewhere.clone()])
                .await;
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
                .sync(vec![
                    a.clone(),
                    b.clone(),
                    c.clone(),
                    d.clone(),
                    elsewhere.clone(),
                ])
                .await
                .unwrap();
            fixture.network.set_joined(d.thread, true).await.unwrap();
            let batch = fixture.network.claim(token(&b)).await.unwrap().unwrap();
            assert_eq!(batch.messages.len(), 1);
            assert!(batch.messages[0].broadcast);
            assert_eq!(batch.messages[0].deliveries.len(), 1);
            assert_eq!(batch.messages[0].deliveries[0].recipient, b.thread);
            fixture
                .network
                .finish(batch.attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
            fixture.network.set_joined(c.thread, false).await.unwrap();
            fixture.network.set_joined(c.thread, true).await.unwrap();
            assert!(fixture.network.claim(token(&c)).await.unwrap().is_none());
            assert!(fixture.network.claim(token(&d)).await.unwrap().is_none());
            assert!(
                fixture
                    .network
                    .claim(token(&elsewhere))
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(fixture.network.claim(token(&a)).await.unwrap().is_none());
            let saved = fixture.saved().await;
            assert_eq!(saved.messages[0].deliveries.len(), 2);
            assert_eq!(
                saved.messages[0]
                    .deliveries
                    .iter()
                    .find(|delivery| delivery.recipient == b.thread)
                    .unwrap()
                    .state,
                DeliveryState::Delivered
            );
            assert_eq!(
                saved.messages[0]
                    .deliveries
                    .iter()
                    .find(|delivery| delivery.recipient == c.thread)
                    .unwrap()
                    .state,
                DeliveryState::Closed {
                    reason: "left".into()
                }
            );
            fixture.send(&a, &c, "new membership").await;
            assert_eq!(
                fixture
                    .network
                    .claim(token(&c))
                    .await
                    .unwrap()
                    .unwrap()
                    .messages[0]
                    .body,
                "new membership"
            );
        });
    }

    #[test]
    fn a_dormant_member_leaves_durably_and_its_unread_mail_closes_without_touching_history() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let mut b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            let delivered = fixture.send(&a, &b, "read").await;
            deliver_all(&fixture.network, &b).await;
            let queued = fixture.send(&a, &b, "unread").await;
            b.token = None;
            b.delivery_enabled = false;
            b.can_receive = false;
            b.status = "Offline".into();
            fixture
                .network
                .sync(vec![a.clone(), b.clone()])
                .await
                .unwrap();
            assert_eq!(
                fixture.saved().await.members.len(),
                2,
                "Being offline alone keeps consent and the mailbox"
            );

            b.dormant = true;
            fixture
                .network
                .sync(vec![a.clone(), b.clone()])
                .await
                .unwrap();
            let saved = fixture.saved().await;
            assert_eq!(
                saved
                    .members
                    .iter()
                    .map(|member| member.thread)
                    .collect::<Vec<_>>(),
                vec![a.thread]
            );
            let state = |id| {
                saved
                    .messages
                    .iter()
                    .find(|message| message.id == id)
                    .unwrap()
                    .deliveries[0]
                    .state
                    .clone()
            };
            assert_eq!(state(delivered), DeliveryState::Delivered);
            assert_eq!(
                state(queued),
                DeliveryState::Closed {
                    reason: "inactive".into()
                }
            );

            b.dormant = false;
            fixture.network.sync(vec![a, b]).await.unwrap();
            assert_eq!(
                fixture.saved().await.members.len(),
                1,
                "Only an explicit Join restores consent"
            );
        });
    }

    fn saved_text(directory: &Path) -> String {
        rusqlite::Connection::open_with_flags(
            directory.join("drostje.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT value_json FROM settings WHERE key = ?1",
            [SETTING],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn migration_and_commits_persist_the_same_bytes_for_a_fixed_network() {
        smol::block_on(async {
            let directory = Directory::new();
            let store = Store::open(directory.0.clone()).unwrap();
            let id = |n: u128| Uuid::from_u128(n);
            let member = |thread: Uuid, title: &str, automatic: bool| {
                json!({"thread": thread, "space": id(10), "title": title, "project": "project",
                    "worktree": "worktree", "harness": "omp", "status": "busy", "online": true,
                    "automatic": automatic, "delivery_enabled": automatic})
            };
            let saved = json!({
                "members": [member(id(1), "Alice", true), member(id(2), "Bob", false)],
                "messages": [{"id": id(20), "space": id(10), "sender": id(1), "sender_name": "Alice",
                    "body": "hello \"Bob\" 日本語", "broadcast": false, "created_at": "2026-09-15T12:00:00Z",
                    "deliveries": [{"recipient": id(2), "recipient_name": "Bob",
                        "state": {"status": "delivering"}, "attempt": id(30)}]}],
            });
            store
                .set_setting(SETTING.into(), saved.clone())
                .await
                .unwrap();
            let (network, _updates) = Network::start(store.clone(), Some(&saved)).unwrap();
            network.flush().await.unwrap();
            store.flush().await.unwrap();
            let members = |alice: &str, bob: &str| {
                format!(
                    r#"{{"members":[{{"thread":"{}","space":"{}","title":"Alice","project":"project","worktree":"worktree","harness":"omp",{alice}}},{{"thread":"{}","space":"{}","title":"Bob","project":"project","worktree":"worktree","harness":"omp",{bob}}}],"#,
                    id(1),
                    id(10),
                    id(2),
                    id(10)
                )
            };
            let messages = format!(
                r#""messages":[{{"id":"{}","space":"{}","sender":"{}","sender_name":"Alice","body":"hello \"Bob\" 日本語","broadcast":false,"created_at":"2026-09-15T12:00:00Z","deliveries":[{{"recipient":"{}","recipient_name":"Bob","state":{{"status":"unknown","reason":"Application restarted before delivery receipt"}},"attempt":"{}"}}]}}]}}"#,
                id(20),
                id(10),
                id(1),
                id(2),
                id(30)
            );
            assert_eq!(
                saved_text(&directory.0),
                members(
                    r#""status":"Offline","online":false,"automatic":true,"delivery_enabled":false"#,
                    r#""status":"Offline","online":false,"automatic":false,"delivery_enabled":false"#,
                ) + &messages
            );

            let peer = |thread: Uuid, title: &str| Peer {
                thread,
                space: id(10),
                title: title.into(),
                status: "idle".into(),
                token: Some(Uuid::new_v4()),
                ..peer(id(10), title)
            };
            network
                .sync(vec![peer(id(1), "Alice"), peer(id(2), "Bob")])
                .await
                .unwrap();
            network.set_joined(id(2), true).await.unwrap();
            store.flush().await.unwrap();
            assert_eq!(
                saved_text(&directory.0),
                members(
                    r#""status":"idle","online":true,"automatic":true,"delivery_enabled":true"#,
                    r#""status":"idle","online":true,"automatic":true,"delivery_enabled":true"#,
                ) + &messages
            );
        });
    }

    #[test]
    fn real_store_restart_resets_presence_and_never_retries_uncertain_delivery() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let mut b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            let uncertain = fixture.send(&a, &b, "in flight at crash").await;
            let attempt = fixture
                .network
                .claim(token(&b))
                .await
                .unwrap()
                .unwrap()
                .attempt;
            let queued = fixture.send(&a, &b, "still queued").await;
            let Fixture {
                network,
                updates,
                store,
                directory,
            } = fixture;
            store.flush().await.unwrap();
            drop(network);
            drop(updates);
            drop(store);
            let store = reopen(&directory.0);
            let workspace = store.snapshot().await.unwrap();
            let (network, updates) =
                Network::start(store.clone(), workspace.settings.get(SETTING)).unwrap();
            network.flush().await.unwrap();
            let restarted = updates.recv().await.unwrap();
            assert!(
                restarted
                    .members
                    .iter()
                    .all(|member| member.automatic && !member.online && !member.delivery_enabled)
            );
            assert_eq!(restarted.messages[0].id, uncertain);
            assert!(matches!(
                restarted.messages[0].deliveries[0].state,
                DeliveryState::Unknown { .. }
            ));
            assert_eq!(restarted.messages[0].deliveries[0].attempt, Some(attempt));
            assert_eq!(restarted.messages[1].id, queued);
            assert_eq!(
                restarted.messages[1].deliveries[0].state,
                DeliveryState::Queued
            );
            assert_eq!(
                store.snapshot().await.unwrap().settings[SETTING],
                serde_json::to_value(&*restarted).unwrap()
            );
            let old_token = token(&b);
            assert!(network.claim(old_token).await.is_err());
            b.token = Some(Uuid::new_v4());
            b.delivery_enabled = false;
            network.sync(vec![a.clone(), b.clone()]).await.unwrap();
            assert!(network.claim(old_token).await.is_err());
            assert!(network.claim(token(&b)).await.unwrap().is_none());
            b.delivery_enabled = true;
            network.sync(vec![a, b.clone()]).await.unwrap();
            let batch = network.claim(token(&b)).await.unwrap().unwrap();
            assert_eq!(batch.messages.len(), 1);
            assert_eq!(batch.messages[0].id, queued);
            assert_ne!(batch.attempt, attempt);
            network
                .finish(attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
            network
                .finish(batch.attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
            drop(network);
            drop(updates);
            drop(store);
            let store = reopen(&directory.0);
            let workspace = store.snapshot().await.unwrap();
            let (network, updates) =
                Network::start(store.clone(), workspace.settings.get(SETTING)).unwrap();
            network.flush().await.unwrap();
            assert!(
                updates
                    .recv()
                    .await
                    .unwrap()
                    .messages
                    .iter()
                    .all(|message| message.deliveries[0].state == DeliveryState::Delivered)
            );
        });
    }

    #[test]
    fn concurrent_sends_and_claims_preserve_order_and_never_double_lease() {
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
                assert!(ids.insert(send.await["message"]["id"].as_str().unwrap().to_owned()));
            }
            let order = fixture
                .saved()
                .await
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>();
            let left = fixture.network.clone();
            let right = fixture.network.clone();
            let token = token(&b);
            let left = smol::spawn(async move { left.claim(token).await.unwrap() });
            let right = smol::spawn(async move { right.claim(token).await.unwrap() });
            let (left, right) = (left.await, right.await);
            assert!(left.is_none() || right.is_none());
            let batch = left.or(right).unwrap();
            assert_eq!(
                batch
                    .messages
                    .iter()
                    .map(|message| message.id)
                    .collect::<Vec<_>>(),
                order
            );
            fixture
                .network
                .finish(batch.attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
            assert!(
                fixture
                    .saved()
                    .await
                    .messages
                    .iter()
                    .all(|message| message.deliveries[0].state == DeliveryState::Delivered)
            );
        });
    }

    async fn deliver_all(network: &Network, peer: &Peer) {
        while let Some(batch) = network.claim(token(peer)).await.unwrap() {
            network
                .finish(batch.attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
        }
    }

    #[test]
    fn recipient_capacity_counts_in_flight_and_broadcast_overflow_is_atomic() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Full");
            let c = peer(a.space, "Empty");
            fixture.join(&[a.clone(), b.clone(), c.clone()]).await;
            for _ in 0..MAX_PENDING_PER_RECIPIENT {
                fixture.send(&a, &b, "pending").await;
            }
            let batch = fixture.network.claim(token(&b)).await.unwrap().unwrap();
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
            assert!(fixture.network.claim(token(&c)).await.unwrap().is_none());
            fixture
                .network
                .finish(batch.attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
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
            assert_eq!(
                fixture
                    .saved()
                    .await
                    .messages
                    .last()
                    .unwrap()
                    .deliveries
                    .len(),
                2
            );
        });
    }

    #[test]
    fn global_capacity_is_shared_and_only_terminal_dispositions_free_it() {
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
            let extra = peer(a.space, "Empty");
            peers.push(extra.clone());
            fixture.network.sync(peers).await.unwrap();
            fixture
                .network
                .set_joined(extra.thread, true)
                .await
                .unwrap();
            let batch = fixture
                .network
                .claim(token(&recipients[0]))
                .await
                .unwrap()
                .unwrap();
            let before = fixture.saved().await;
            assert!(
                fixture
                    .network
                    .call(
                        token(&a),
                        Action::Send {
                            recipient: extra.thread,
                            body: "overflow".into()
                        }
                    )
                    .await
                    .is_err()
            );
            assert_eq!(fixture.saved().await, before);
            fixture
                .network
                .finish(
                    batch.attempt,
                    DeliveryOutcome::Failed("native rejection".into()),
                )
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
                            body: "full history".into(),
                        }
                    )
                    .await
                    .is_err()
            );
            assert_eq!(fixture.saved().await.messages.len(), MAX_MESSAGES);
            deliver_all(&fixture.network, &recipients[0]).await;
            fixture
                .send(&a, &recipients[1], "evicts oldest terminal only")
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
    fn bodies_are_byte_bounded_and_escaped_batches_stay_within_64_kib_in_order() {
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
            let mut ids = Vec::new();
            for _ in 0..24 {
                ids.push(fixture.send(&a, &b, &body).await);
            }
            let mut delivered = Vec::new();
            let mut batches = 0;
            while let Some(batch) = fixture.network.claim(token(&b)).await.unwrap() {
                assert!(serde_json::to_vec(&batch).unwrap().len() <= MAX_BATCH_BYTES);
                batches += 1;
                for message in &batch.messages {
                    assert_eq!(message.body, body);
                    delivered.push(message.id);
                }
                fixture
                    .network
                    .finish(batch.attempt, DeliveryOutcome::Delivered)
                    .await
                    .unwrap();
            }
            assert_eq!(delivered, ids);
            assert!(batches > 1);
            assert!(
                fixture
                    .saved()
                    .await
                    .messages
                    .iter()
                    .all(|message| message.deliveries[0].state == DeliveryState::Delivered)
            );
        });
    }

    #[test]
    fn escaped_metadata_remains_deliverable_as_a_bounded_singleton() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), &"\0".repeat(MAX_CONTEXT_BYTES));
            let b = peer(a.space, &"\0".repeat(MAX_CONTEXT_BYTES));
            fixture.join(&[a.clone(), b.clone()]).await;
            let id = fixture.send(&a, &b, "small body").await;
            let batch = fixture.network.claim(token(&b)).await.unwrap().unwrap();
            assert!(encoded_len(&batch).unwrap() > MAX_BATCH_BYTES);
            assert_eq!(batch.messages[0].id, id);
            assert_eq!(batch.messages[0].sender_name, a.title);
            fixture
                .network
                .finish(batch.attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
            assert_eq!(
                fixture.saved().await.messages[0].deliveries[0].state,
                DeliveryState::Delivered
            );
        });
    }

    #[test]
    fn a_lease_not_sent_to_native_can_be_deferred_but_uncertain_mail_cannot() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Sender");
            let b = peer(a.space, "Recipient");
            fixture.join(&[a.clone(), b.clone()]).await;
            let id = fixture
                .send(&a, &b, "wait for the open permission dialog")
                .await;
            let first = fixture.network.claim(token(&b)).await.unwrap().unwrap();
            fixture
                .network
                .finish(first.attempt, DeliveryOutcome::Deferred)
                .await
                .unwrap();
            let second = fixture.network.claim(token(&b)).await.unwrap().unwrap();
            assert_ne!(first.attempt, second.attempt);
            assert_eq!(second.messages[0].id, id);
            fixture
                .network
                .finish(
                    second.attempt,
                    DeliveryOutcome::Unknown("native write was not confirmed".into()),
                )
                .await
                .unwrap();
            let before = fixture.saved().await;
            fixture
                .network
                .finish(second.attempt, DeliveryOutcome::Deferred)
                .await
                .unwrap();
            assert_eq!(fixture.saved().await, before);
            assert!(fixture.network.claim(token(&b)).await.unwrap().is_none());
        });
    }

    #[test]
    fn native_refusal_resolves_a_held_attempt_without_retry() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let sender = peer(Uuid::new_v4(), "Sender");
            let recipient = peer(sender.space, "Recipient");
            fixture.join(&[sender.clone(), recipient.clone()]).await;
            let id = fixture
                .send(&sender, &recipient, "Await the native policy decision")
                .await;
            let batch = fixture
                .network
                .claim(token(&recipient))
                .await
                .unwrap()
                .unwrap();
            fixture
                .network
                .finish(
                    batch.attempt,
                    DeliveryOutcome::Unknown("Native policy held this message".into()),
                )
                .await
                .unwrap();
            assert!(
                fixture
                    .network
                    .claim(token(&recipient))
                    .await
                    .unwrap()
                    .is_none()
            );
            fixture
                .network
                .finish(
                    batch.attempt,
                    DeliveryOutcome::Failed("Native policy denied this message".into()),
                )
                .await
                .unwrap();
            let saved = fixture.saved().await;
            assert_eq!(saved.messages[0].id, id);
            assert_eq!(
                saved.messages[0].deliveries[0].state,
                DeliveryState::Failed {
                    reason: "Native policy denied this message".into(),
                }
            );
            assert_eq!(saved.messages[0].deliveries[0].attempt, Some(batch.attempt));
            assert!(
                fixture
                    .network
                    .claim(token(&recipient))
                    .await
                    .unwrap()
                    .is_none()
            );
            fixture
                .network
                .finish(batch.attempt, DeliveryOutcome::Delivered)
                .await
                .unwrap();
            assert_eq!(
                fixture.saved().await,
                saved,
                "Refused messages cannot later claim acceptance"
            );
        });
    }

    #[test]
    fn store_failures_never_publish_uncommitted_send_claim_receipt_or_revocation() {
        smol::block_on(async {
            for operation in ["send", "claim", "finish", "move", "leave", "disable"] {
                let fixture = Fixture::new();
                let a = peer(Uuid::new_v4(), "Alice");
                let mut b = peer(a.space, "Bob");
                fixture.join(&[a.clone(), b.clone()]).await;
                fixture.send(&a, &b, "last committed").await;
                let attempt = if operation == "finish" || operation == "disable" {
                    Some(
                        fixture
                            .network
                            .claim(token(&b))
                            .await
                            .unwrap()
                            .unwrap()
                            .attempt,
                    )
                } else {
                    None
                };
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
                    "claim" => fixture.network.claim(token(&b)).await.map(|_| ()),
                    "finish" => {
                        fixture
                            .network
                            .finish(attempt.unwrap(), DeliveryOutcome::Delivered)
                            .await
                    }
                    "move" => {
                        b.space = Uuid::new_v4();
                        fixture.network.sync(vec![a.clone(), b.clone()]).await
                    }
                    "leave" => fixture.network.set_joined(b.thread, false).await,
                    "disable" => {
                        b.delivery_enabled = false;
                        fixture.network.sync(vec![a.clone(), b.clone()]).await
                    }
                    _ => unreachable!(),
                };
                assert!(result.is_err(), "{operation}");
                let failed = fixture.updates.try_recv().unwrap();
                assert!(failed.error.is_some(), "{operation}");
                assert!(
                    failed
                        .members
                        .iter()
                        .all(|member| !member.online && !member.delivery_enabled)
                );
                assert_eq!(failed.messages, before.messages);
                assert_eq!(fixture.saved().await, before);
                assert!(
                    fixture
                        .network
                        .call(token(&a), Action::Members)
                        .await
                        .is_err()
                );
                assert!(fixture.network.claim(token(&b)).await.is_err());
                blocker.execute_batch("ROLLBACK").unwrap();
                assert!(
                    fixture.network.sync(vec![a, b]).await.is_err(),
                    "reopen is required after failure"
                );
            }
        });
    }

    #[test]
    fn startup_migration_failure_disables_mutations_without_publishing_authority() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            fixture.join(std::slice::from_ref(&a)).await;
            let mut legacy = serde_json::to_value(fixture.saved().await).unwrap();
            legacy["members"][0]
                .as_object_mut()
                .unwrap()
                .remove("automatic");
            legacy["members"][0]
                .as_object_mut()
                .unwrap()
                .remove("delivery_enabled");
            let Fixture {
                network,
                updates,
                store,
                directory,
            } = fixture;
            drop(network);
            drop(updates);
            store
                .set_setting(SETTING.into(), legacy.clone())
                .await
                .unwrap();
            let blocker = rusqlite::Connection::open(directory.0.join("drostje.sqlite3")).unwrap();
            blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
            let (network, updates) = Network::start(store.clone(), Some(&legacy)).unwrap();
            assert!(network.flush().await.is_err());
            let failed = updates.recv().await.unwrap();
            assert!(failed.error.is_some());
            assert!(
                failed
                    .members
                    .iter()
                    .all(|member| !member.online && !member.delivery_enabled && !member.automatic)
            );
            assert_eq!(store.snapshot().await.unwrap().settings[SETTING], legacy);
            blocker.execute_batch("ROLLBACK").unwrap();
            assert!(network.sync(vec![a]).await.is_err());
        });
    }

    #[test]
    fn member_capacity_and_saved_validation_preserve_the_last_committed_state() {
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
            let mut corrupt = before.clone();
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
                    Some(&json!({
                        "members": [], "messages": [], "token": Uuid::new_v4(),
                    }))
                )
                .is_err()
            );
            let mut invalid = peers[0].clone();
            invalid.title = "x".repeat(MAX_CONTEXT_BYTES + 1);
            assert!(fixture.network.sync(vec![invalid]).await.is_err());
            assert!(
                fixture
                    .network
                    .call(token(&peers[0]), Action::Members)
                    .await
                    .is_err()
            );
        });
    }

    #[test]
    fn saved_attempts_and_dispositions_are_validated_without_compatibility_aliases() {
        smol::block_on(async {
            let fixture = Fixture::new();
            let a = peer(Uuid::new_v4(), "Alice");
            let b = peer(a.space, "Bob");
            fixture.join(&[a.clone(), b.clone()]).await;
            fixture.send(&a, &b, "first").await;
            fixture.send(&a, &b, "second").await;
            let before = fixture.saved().await;
            for invalid in [
                "missing_attempt",
                "queued_attempt",
                "duplicate_id",
                "mixed",
                "two_attempts",
                "cross_recipient_attempt",
            ] {
                let mut saved = serde_json::to_value(&before).unwrap();
                let attempt = Uuid::new_v4();
                match invalid {
                    "missing_attempt" => {
                        saved["messages"][0]["deliveries"][0]["state"] =
                            json!({"status": "delivering"})
                    }
                    "queued_attempt" => {
                        saved["messages"][0]["deliveries"][0]["attempt"] = json!(attempt)
                    }
                    "duplicate_id" => {
                        saved["messages"][1]["id"] = saved["messages"][0]["id"].clone()
                    }
                    "mixed" => saved["messages"][0]["deliveries"][0]["retrieved"] = json!(false),
                    "two_attempts" | "cross_recipient_attempt" => {
                        for index in 0..2 {
                            saved["messages"][index]["deliveries"][0]["state"] =
                                json!({"status": "delivering"});
                            saved["messages"][index]["deliveries"][0]["attempt"] =
                                json!(if index == 0 || invalid == "cross_recipient_attempt" {
                                    attempt
                                } else {
                                    Uuid::new_v4()
                                });
                        }
                        if invalid == "cross_recipient_attempt" {
                            saved["messages"][1]["deliveries"][0]["recipient"] = json!(a.thread);
                        }
                    }
                    _ => unreachable!(),
                }
                assert!(
                    Network::start(fixture.store.clone(), Some(&saved)).is_err(),
                    "{invalid}"
                );
            }
            assert!(
                serde_json::from_value::<Delivery>(json!({
                    "recipient": b.thread, "recipient_name": "Bob", "retrieved": false,
                }))
                .is_err()
            );
            assert_eq!(fixture.saved().await, before);
        });
    }
}
