use super::*;
use std::time::{Duration, Instant};

pub const MAX_WAIT_MS: u64 = 20_000;
const MAX_WAITERS: usize = 64;

pub fn default_timeout() -> u64 {
    MAX_WAIT_MS
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Urgent,
    #[default]
    Normal,
    Fyi,
}

impl Priority {
    pub fn legacy() -> Self {
        Self::Urgent
    }

    pub fn can_push(self, status: &str) -> bool {
        match self {
            Self::Urgent => true,
            Self::Normal => status.eq_ignore_ascii_case("idle"),
            Self::Fyi => false,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Urgent => "Urgent",
            Self::Normal => "Normal",
            Self::Fyi => "FYI",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboundPolicy {
    #[default]
    Accept,
    Hold,
    Refuse,
}

impl InboundPolicy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Accept => "Accept",
            Self::Hold => "Hold",
            Self::Refuse => "Refuse",
        }
    }
}

pub(super) struct MessageOptions {
    pub(super) priority: Priority,
    pub(super) reply_to: Option<Uuid>,
    pub(super) exchange: Option<Uuid>,
    pub(super) expect_reply: bool,
}

impl MessageOptions {
    pub(super) fn new(priority: Priority) -> Self {
        Self {
            priority,
            reply_to: None,
            exchange: None,
            expect_reply: false,
        }
    }
}

enum WaitEvent {
    Message,
    Reply(Uuid),
    Idle(Uuid),
}

pub(super) struct Waiter {
    token: Uuid,
    event: WaitEvent,
    pub(super) deadline: Instant,
    reply: Reply<Value>,
    question: Option<Value>,
}

pub(super) fn validate_identity(activity: &str, tags: &[String]) -> Result<()> {
    ensure!(activity.len() <= 512, "Activity exceeds 512 bytes");
    ensure!(tags.len() <= 16, "Use at most 16 identity tags");
    let mut unique = HashSet::new();
    for tag in tags {
        ensure!(
            !tag.is_empty() && tag.len() <= 128 && !tag.chars().any(char::is_whitespace),
            "Tags must be nonempty, at most 128 bytes, without whitespace"
        );
        let (key, _) = tag.split_once(':').unwrap_or((tag, ""));
        ensure!(
            ![
                "project", "worktree", "harness", "status", "id", "name", "inbound"
            ]
            .contains(&key.to_lowercase().as_str()),
            "Tag {key} is reserved for runtime context"
        );
        ensure!(unique.insert(tag.to_lowercase()), "Duplicate identity tag");
    }
    Ok(())
}

struct Selector(Vec<(String, String)>);

impl Selector {
    fn parse(value: &str) -> Result<Self> {
        ensure!(
            !value.trim().is_empty() && value.len() <= 512,
            "Selector must contain 1 to 512 bytes"
        );
        let mut terms = Vec::new();
        for term in value.split_whitespace() {
            if term == "*" {
                continue;
            }
            let (key, value) = if let Some(text) = term.strip_prefix('~') {
                ("text", text)
            } else {
                term.split_once('=')
                    .context("Use selector terms such as project=name role=reviewer or ~text")?
            };
            ensure!(
                !key.is_empty() && !value.is_empty(),
                "Selector terms need a key and value"
            );
            terms.push((key.to_lowercase(), value.to_lowercase()));
        }
        Ok(Self(terms))
    }

    fn matches(&self, member: &Member) -> bool {
        self.0.iter().all(|(key, value)| {
            let matches = |actual: &str| {
                let actual = actual.to_lowercase();
                value
                    .strip_suffix('*')
                    .map_or_else(|| actual == *value, |prefix| actual.starts_with(prefix))
            };
            match key.as_str() {
                "project" => matches(&member.project),
                "worktree" => matches(&member.worktree),
                "harness" => matches(&member.harness),
                "status" => matches(&member.status),
                "name" => matches(&member.title),
                "id" => matches(&member.thread.to_string()),
                "activity" => member.activity.to_lowercase().contains(value),
                "inbound" => matches(member.inbound.label()),
                "text" => [
                    &member.title,
                    &member.project,
                    &member.worktree,
                    &member.activity,
                ]
                .into_iter()
                .chain(member.tags.iter())
                .any(|field| field.to_lowercase().contains(value)),
                _ => member.tags.iter().any(|tag| {
                    tag.split_once(':').is_some_and(|(tag_key, tag_value)| {
                        tag_key.eq_ignore_ascii_case(key) && matches(tag_value)
                    })
                }),
            }
        })
    }
}

impl Actor {
    fn member_in_space(&self, thread: Uuid, space: Uuid) -> Result<&Member> {
        self.snapshot
            .members
            .iter()
            .find(|member| member.thread == thread && member.space == space)
            .context("Recipient is not a joined member of this Space")
    }

    fn visible_message(&self, caller: &Member, id: Uuid) -> Result<&NetworkMessage> {
        self.snapshot
            .messages
            .iter()
            .find(|message| {
                message.id == id
                    && message.space == caller.space
                    && (message.sender == caller.thread
                        || message
                            .deliveries
                            .iter()
                            .any(|delivery| delivery.recipient == caller.thread))
            })
            .context("Message is not in this member's retained history")
    }

    pub(super) async fn coordinate(&mut self, token: Uuid, action: Action) -> Result<Value> {
        let caller = self.caller(token)?.clone();
        match action {
            Action::Discover { selector } => {
                let selector = Selector::parse(&selector)?;
                bounded_response(json!({"members": self.snapshot.members.iter()
                    .filter(|member| member.space == caller.space && selector.matches(member)).collect::<Vec<_>>()}))
            }
            Action::Multicast {
                selector,
                body,
                priority,
            } => {
                let selector = Selector::parse(&selector)?;
                let recipients = self
                    .snapshot
                    .members
                    .iter()
                    .filter(|member| {
                        member.space == caller.space
                            && member.thread != caller.thread
                            && selector.matches(member)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                ensure!(
                    !recipients.is_empty(),
                    "No other joined member matches this selector"
                );
                self.send(
                    caller,
                    recipients,
                    body,
                    false,
                    MessageOptions::new(priority),
                )
                .await
            }
            Action::Reply {
                message,
                body,
                priority,
            } => {
                let original = self.visible_message(&caller, message)?;
                ensure!(
                    original
                        .deliveries
                        .iter()
                        .any(|delivery| delivery.recipient == caller.thread),
                    "Only a recipient may reply to a message"
                );
                let recipient = self.member_in_space(original.sender, caller.space)?.clone();
                let options = MessageOptions {
                    priority,
                    reply_to: Some(original.id),
                    exchange: Some(original.exchange.unwrap_or(original.id)),
                    expect_reply: false,
                };
                self.send(caller, vec![recipient], body, false, options)
                    .await
            }
            Action::Inbox => {
                let messages = self
                    .snapshot
                    .messages
                    .iter()
                    .filter(|message| message.space == caller.space)
                    .filter_map(|message| {
                        message
                            .deliveries
                            .iter()
                            .find(|delivery| {
                                delivery.recipient == caller.thread
                                    && delivery.state == DeliveryState::Queued
                            })
                            .map(|delivery| recipient_message(message, delivery))
                    })
                    .take(16)
                    .collect::<Vec<_>>();
                self.read_messages(caller.thread, messages, "inbox").await
            }
            Action::History { message, query } => {
                ensure!(
                    query.as_ref().is_none_or(|value| value.len() <= 512),
                    "History query exceeds 512 bytes"
                );
                let exchange = message
                    .map(|id| {
                        self.visible_message(&caller, id)
                            .map(|message| message.exchange.unwrap_or(message.id))
                    })
                    .transpose()?;
                let messages = self
                    .snapshot
                    .messages
                    .iter()
                    .rev()
                    .filter(|message| {
                        message.space == caller.space
                            && (message.sender == caller.thread
                                || message
                                    .deliveries
                                    .iter()
                                    .any(|delivery| delivery.recipient == caller.thread))
                            && exchange.is_none_or(|exchange| {
                                message.exchange.unwrap_or(message.id) == exchange
                            })
                            && query.as_ref().is_none_or(|query| {
                                message.body.to_lowercase().contains(&query.to_lowercase())
                            })
                    })
                    .take(16)
                    .map(|message| self.message_for(&caller, message))
                    .collect::<Vec<_>>();
                bounded_response(json!({"messages": messages}))
            }
            Action::SetIdentity { activity, tags } => {
                let mut next = NetworkSnapshot::clone(&self.snapshot);
                let member = next
                    .members
                    .iter_mut()
                    .find(|member| member.thread == caller.thread)
                    .context("Member left the network")?;
                if let Some(activity) = activity {
                    member.activity = activity.trim().into();
                }
                if let Some(tags) = tags {
                    member.tags = tags;
                }
                validate_identity(&member.activity, &member.tags)?;
                let response = bounded_response(json!({"member": member}))?;
                self.commit(next).await?;
                Ok(response)
            }
            Action::SetInbound { policy } => {
                let mut next = NetworkSnapshot::clone(&self.snapshot);
                let member = next
                    .members
                    .iter_mut()
                    .find(|member| member.thread == caller.thread)
                    .context("Member left the network")?;
                member.inbound = policy;
                let response = bounded_response(json!({"member": member}))?;
                if policy == InboundPolicy::Refuse {
                    for delivery in next
                        .messages
                        .iter_mut()
                        .flat_map(|message| &mut message.deliveries)
                    {
                        if delivery.recipient == caller.thread
                            && delivery.state == DeliveryState::Queued
                        {
                            delivery.state = DeliveryState::Refused {
                                reason: "Recipient refuses incoming messages".into(),
                            };
                        }
                    }
                }
                self.commit(next).await?;
                Ok(response)
            }
            Action::Members
            | Action::Send { .. }
            | Action::Broadcast { .. }
            | Action::Ask { .. }
            | Action::Wait { .. } => bail!("Action must use its network coordinator entry point"),
        }
    }

    fn message_for(&self, caller: &Member, message: &NetworkMessage) -> NetworkMessage {
        let mut result = message.clone();
        if caller.thread != message.sender {
            result
                .deliveries
                .retain(|delivery| delivery.recipient == caller.thread);
        }
        result
    }

    async fn read_messages(
        &mut self,
        recipient: Uuid,
        mut messages: Vec<NetworkMessage>,
        event: &str,
    ) -> Result<Value> {
        let ids: HashSet<_> = messages.iter().map(|message| message.id).collect();
        for delivery in messages
            .iter_mut()
            .flat_map(|message| &mut message.deliveries)
        {
            if delivery.recipient == recipient && delivery.state == DeliveryState::Queued {
                delivery.state = DeliveryState::Read;
            }
        }
        let response = bounded_response(json!({"event": event, "messages": messages}))?;
        if !ids.is_empty() {
            let mut next = NetworkSnapshot::clone(&self.snapshot);
            for delivery in next
                .messages
                .iter_mut()
                .filter(|message| ids.contains(&message.id))
                .flat_map(|message| &mut message.deliveries)
            {
                if delivery.recipient == recipient && delivery.state == DeliveryState::Queued {
                    delivery.state = DeliveryState::Read;
                }
            }
            self.commit(next).await?;
        }
        Ok(response)
    }

    fn validate_wait(&self, token: Uuid, timeout_ms: u64) -> Result<()> {
        let caller = self.caller(token)?;
        ensure!(
            caller.delivery_enabled,
            "Waiting requires an authorized live runtime"
        );
        ensure!(
            (1..=MAX_WAIT_MS).contains(&timeout_ms),
            "Wait timeout must be 1 to 20000 milliseconds"
        );
        ensure!(
            self.waiters.len() < MAX_WAITERS,
            "Network wait capacity reached"
        );
        ensure!(
            !self.waiters.iter().any(|waiter| waiter.token == token),
            "This runtime already has a pending network wait"
        );
        Ok(())
    }

    async fn prepare_wait(
        &mut self,
        token: Uuid,
        action: Action,
    ) -> Result<(WaitEvent, u64, Option<Value>)> {
        match action {
            Action::Ask {
                recipient,
                body,
                timeout_ms,
            } => {
                self.validate_wait(token, timeout_ms)?;
                let caller = self.caller(token)?.clone();
                ensure!(recipient != caller.thread, "An agent cannot ask itself");
                let recipient = self.member_in_space(recipient, caller.space)?.clone();
                let mut options = MessageOptions::new(Priority::Urgent);
                options.expect_reply = true;
                let question = self
                    .send(caller, vec![recipient], body, false, options)
                    .await?;
                let id = serde_json::from_value(question["message"]["id"].clone())?;
                Ok((WaitEvent::Reply(id), timeout_ms, Some(question)))
            }
            Action::Wait { event, timeout_ms } => {
                self.validate_wait(token, timeout_ms)?;
                let caller = self.caller(token)?;
                let event = if event == "message" {
                    WaitEvent::Message
                } else if let Some(id) = event.strip_prefix("reply:") {
                    let id = Uuid::parse_str(id).context("Invalid reply message UUID")?;
                    let message = self.visible_message(caller, id)?;
                    ensure!(
                        message.sender == caller.thread,
                        "Wait for replies only to your own messages"
                    );
                    WaitEvent::Reply(id)
                } else if let Some(id) = event.strip_prefix("idle:") {
                    let id = Uuid::parse_str(id).context("Invalid idle Thread UUID")?;
                    ensure!(
                        id != caller.thread,
                        "An agent cannot wait for itself to become idle"
                    );
                    self.member_in_space(id, caller.space)?;
                    WaitEvent::Idle(id)
                } else {
                    bail!(
                        "Wait event must be message, reply:<message UUID>, or idle:<Thread UUID>"
                    );
                };
                Ok((event, timeout_ms, None))
            }
            _ => bail!("Expected ask or wait"),
        }
    }

    pub(super) async fn handle_call(&mut self, token: Uuid, action: Action, reply: Reply<Value>) {
        if reply.is_closed() {
            return;
        }
        if matches!(action, Action::Ask { .. } | Action::Wait { .. }) {
            match self.prepare_wait(token, action).await {
                Ok((event, timeout_ms, question)) => self.waiters.push(Waiter {
                    token,
                    event,
                    deadline: Instant::now() + Duration::from_millis(timeout_ms),
                    reply,
                    question,
                }),
                Err(error) => {
                    let _ = reply.try_send(Err(error));
                }
            }
        } else {
            let _ = reply.try_send(self.call(token, action).await);
        }
    }

    async fn wait_result(&mut self, waiter: &Waiter) -> Result<Option<Value>> {
        let caller = self.caller(waiter.token)?.clone();
        ensure!(
            caller.delivery_enabled,
            "Runtime authorization was revoked while waiting"
        );
        match waiter.event {
            WaitEvent::Idle(id) => {
                let target = self.member_in_space(id, caller.space)?;
                if target.online && target.status.eq_ignore_ascii_case("idle") {
                    return bounded_response(json!({"event":"idle", "member":target})).map(Some);
                }
            }
            WaitEvent::Message | WaitEvent::Reply(_) => {
                if let WaitEvent::Reply(id) = waiter.event {
                    self.visible_message(&caller, id)?;
                }
                let message = self.snapshot.messages.iter().find_map(|message| {
                    if message.space != caller.space {
                        return None;
                    }
                    let matches = match waiter.event {
                        WaitEvent::Message => true,
                        WaitEvent::Reply(id) => message.reply_to == Some(id),
                        WaitEvent::Idle(_) => false,
                    };
                    if !matches {
                        return None;
                    }
                    message
                        .deliveries
                        .iter()
                        .find(|delivery| {
                            delivery.recipient == caller.thread
                                && (delivery.state == DeliveryState::Queued
                                    || (matches!(waiter.event, WaitEvent::Reply(_))
                                        && matches!(
                                            delivery.state,
                                            DeliveryState::Delivered | DeliveryState::Read
                                        )))
                        })
                        .map(|delivery| recipient_message(message, delivery))
                });
                if let Some(message) = message {
                    let event = if matches!(waiter.event, WaitEvent::Reply(_)) {
                        "reply"
                    } else {
                        "message"
                    };
                    return self
                        .read_messages(caller.thread, vec![message], event)
                        .await
                        .map(Some);
                }
            }
        }
        if Instant::now() >= waiter.deadline {
            return Ok(Some(json!({"event":"timeout"})));
        }
        Ok(None)
    }

    pub(super) async fn resolve_waiters(&mut self) {
        for waiter in std::mem::take(&mut self.waiters) {
            if waiter.reply.is_closed() {
                continue;
            }
            match self.wait_result(&waiter).await {
                Ok(None) => self.waiters.push(waiter),
                result => {
                    let result = result
                        .and_then(|value| value.context("Wait resolved without an event"))
                        .and_then(|mut value| {
                            if let Some(question) = waiter.question {
                                value["question"] = question["message"].clone();
                            }
                            bounded_response(value)
                        });
                    let _ = waiter.reply.try_send(result);
                }
            }
        }
    }
}
