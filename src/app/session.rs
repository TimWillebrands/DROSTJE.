use super::*;

pub struct ConversationSession {
    pub generation: Uuid,
    pub state: ConversationState,
    pub commands: Option<async_channel::Sender<ConversationCommand>>,
    pub(super) finished: Option<async_channel::Receiver<()>>,
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
    /// Bumped whenever `tools` may have changed, so presentation can cache what it derives.
    pub(super) tools_revision: u64,
    pub(super) transcript_changes: VecDeque<(u64, Option<usize>)>,
    pub(super) indices: HashMap<String, usize>,
    /// IDs of every `ContentBlock::ToolCall` seen, so their result messages fold
    /// into the call's card instead of rendering as separate rows.
    pub(super) tool_calls: HashMap<String, usize>,
    pub(super) tool_results: HashMap<String, usize>,
    /// Every tool call in the order the assistant issued it, as `(tool name, call id)`,
    /// so presentation can find the latest call of one tool without scanning messages.
    pub(super) tool_call_order: Vec<(String, String)>,
    /// Indices of harness-injected `custom` messages (job notices and the like).
    pub(super) notice_indices: Vec<usize>,
    pub(super) submitted: HashMap<Uuid, DraftSnapshot>,
    /// Whether the front of the thread's queue may start the next turn on its own. Set by
    /// an explicit queue or send-now action; cleared by a failure, an interrupt the user
    /// did not request through the queue, and a harness restart (a new session).
    pub auto_dispatch: bool,
    /// The user asked to send a queued message now, so the interrupt that settles the
    /// current turn must not stall the queue.
    pub(super) interrupt_for_queue: bool,
}
impl ConversationSession {
    pub(super) fn new(model: Option<ModelKey>) -> Self {
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
            tools_revision: 0,
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
    pub(super) fn message(&mut self, message: Message) {
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

    pub(super) fn record_transcript_change(&mut self, index: Option<usize>) {
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

    pub(super) fn reset_history(&mut self) {
        self.history_loaded = false;
        self.messages.clear();
        self.indices.clear();
        self.tool_calls.clear();
        self.tool_results.clear();
        self.tool_call_order.clear();
        self.notice_indices.clear();
        self.record_transcript_change(None);
    }
    /// Only the call's row shows its activity. Activity can arrive before the message that
    /// carries the call; that message adds the row later, so nothing rendered changes yet.
    pub(super) fn mark_tool_changed(&mut self, call_id: &str) {
        self.tools_revision += 1;
        if let Some(index) = self.tool_calls.get(call_id).copied() {
            self.record_transcript_change(Some(index));
        }
    }

    /// Changes whenever a tool's live activity may have changed.
    pub fn tools_revision(&self) -> u64 {
        self.tools_revision
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

#[cfg(test)]
mod tests {
    use super::{ContentBlock, ConversationSession, Message, Value};

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

    /// Harnesses report a call's activity before the message that carries it. A full
    /// reset there would snap a reader who scrolled back to the latest output.
    #[test]
    fn tool_activity_before_its_call_keeps_the_rendered_rows() {
        let mut session = ConversationSession::new(None);
        session.message(message(
            "assistant:1",
            "assistant",
            vec![ContentBlock::Text("Looking.".into())],
        ));
        let rendered = session.transcript_revision;
        let tools = session.tools_revision();
        session.mark_tool_changed("call_1");
        assert_eq!(
            session
                .transcript_changes_since(rendered)
                .map(Iterator::collect::<Vec<_>>),
            Some(Vec::new()),
            "no row shows the call yet"
        );
        assert_ne!(session.tools_revision(), tools, "the tools still changed");
        session.message(message(
            "assistant:2",
            "assistant",
            vec![ContentBlock::ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                intent: None,
                input: Value::Null,
            }],
        ));
        session.mark_tool_changed("call_1");
        assert_eq!(
            session
                .transcript_changes_since(rendered)
                .map(Iterator::collect::<Vec<_>>),
            Some(vec![1, 1])
        );
    }
}
