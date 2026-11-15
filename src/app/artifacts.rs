use super::*;
use crate::browser::{ChannelEvent, ReceiptStatus};

const MAX_QUEUED: usize = 128;

#[derive(Clone, Debug)]
pub struct ArtifactEvent {
    pub browser: Uuid,
    pub title: String,
    pub event: ChannelEvent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactReceipt {
    pub browser: Uuid,
    pub event: String,
    pub status: ReceiptStatus,
    pub reason: Option<String>,
}

pub struct ArtifactChannels {
    outboxes: HashMap<ThreadId, Outbox>,
    receipts: (
        async_channel::Sender<Vec<ArtifactReceipt>>,
        async_channel::Receiver<Vec<ArtifactReceipt>>,
    ),
}

impl Default for ArtifactChannels {
    fn default() -> Self {
        Self {
            outboxes: HashMap::new(),
            receipts: async_channel::unbounded(),
        }
    }
}

struct Outbox {
    generation: Uuid,
    queued: VecDeque<ArtifactEvent>,
    attempt: Option<Attempt>,
    turn: Option<Uuid>,
}

struct Attempt {
    id: Uuid,
    events: Vec<(Uuid, String)>,
    _admission: async_channel::Sender<()>,
    idle: bool,
}

impl Outbox {
    fn new(generation: Uuid) -> Self {
        Self {
            generation,
            queued: VecDeque::new(),
            attempt: None,
            turn: None,
        }
    }
}

fn receipt(
    browser: Uuid,
    event: String,
    status: ReceiptStatus,
    reason: Option<String>,
) -> ArtifactReceipt {
    ArtifactReceipt {
        browser,
        event,
        status,
        reason,
    }
}

impl AppState {
    pub fn artifact_receipts(&self) -> async_channel::Receiver<Vec<ArtifactReceipt>> {
        self.artifacts.receipts.1.clone()
    }

    fn send_receipts(&self, receipts: Vec<ArtifactReceipt>) {
        if !receipts.is_empty() {
            let _ = self.artifacts.receipts.0.try_send(receipts);
        }
    }

    pub(super) fn close_artifacts(&mut self, thread: ThreadId) {
        let mut receipts = Vec::new();
        self.fail_outbox(
            thread,
            "The Conversation's agent closed before this event was delivered; reopen it, then send again",
            &mut receipts,
        );
        self.send_receipts(receipts);
    }

    pub fn artifact_events(
        &mut self,
        thread: ThreadId,
        browser: Uuid,
        title: String,
        events: Vec<ChannelEvent>,
    ) {
        let mut receipts = Vec::new();
        let generation = self
            .conversations
            .get(&thread)
            .filter(|session| {
                session.commands.is_some()
                    && !matches!(
                        session.state,
                        ConversationState::Disconnected | ConversationState::Failed
                    )
            })
            .map(|session| session.generation);
        let Some(generation) = generation.filter(|_| self.is_conversation(thread)) else {
            receipts.extend(events.into_iter().map(|event| {
                receipt(
                    browser,
                    event.id,
                    ReceiptStatus::Failed,
                    Some("This Conversation's agent is not running. Open the Conversation, start the agent, then send again".into()),
                )
            }));
            self.send_receipts(receipts);
            return;
        };
        self.reset_stale_outbox(thread, generation, &mut receipts);
        let outbox = self
            .artifacts
            .outboxes
            .entry(thread)
            .or_insert_with(|| Outbox::new(generation));
        for event in events {
            if outbox.queued.len() >= MAX_QUEUED {
                receipts.push(receipt(
                    browser,
                    event.id,
                    ReceiptStatus::Failed,
                    Some(
                        "Too many events are waiting for the agent; wait for it, then send again"
                            .into(),
                    ),
                ));
            } else {
                outbox.queued.push_back(ArtifactEvent {
                    browser,
                    title: title.clone(),
                    event,
                });
            }
        }
        self.pump_artifacts(thread, &mut receipts);
        if let Some(outbox) = self.artifacts.outboxes.get(&thread) {
            receipts.extend(
                outbox
                    .queued
                    .iter()
                    .filter(|queued| queued.browser == browser)
                    .map(|queued| {
                        receipt(
                            browser,
                            queued.event.id.clone(),
                            ReceiptStatus::Queued,
                            self.artifact_wait_reason(thread),
                        )
                    }),
            );
        }
        self.send_receipts(receipts);
    }

    pub(super) fn artifact_runtime_changed(&mut self, thread: ThreadId) {
        if !self.artifacts.outboxes.contains_key(&thread) {
            return;
        }
        let mut receipts = Vec::new();
        let live = self
            .conversations
            .get(&thread)
            .filter(|session| {
                session.commands.is_some()
                    && !matches!(
                        session.state,
                        ConversationState::Disconnected | ConversationState::Failed
                    )
            })
            .map(|session| session.generation);
        match live {
            Some(generation) => {
                self.reset_stale_outbox(thread, generation, &mut receipts);
                self.pump_artifacts(thread, &mut receipts);
            }
            None => self.fail_outbox(
                thread,
                "The agent stopped before this event was delivered; start it, then send again",
                &mut receipts,
            ),
        }
        if self.artifacts.outboxes.get(&thread).is_some_and(|outbox| {
            outbox.queued.is_empty() && outbox.attempt.is_none() && outbox.turn.is_none()
        }) {
            self.artifacts.outboxes.remove(&thread);
        }
        self.send_receipts(receipts);
    }

    pub(super) fn artifact_delivery_event(
        &mut self,
        thread: ThreadId,
        attempt: Uuid,
        outcome: harness::PeerDeliveryOutcome,
    ) -> Option<harness::PeerDeliveryOutcome> {
        let Some(outbox) = self.artifacts.outboxes.get_mut(&thread) else {
            return Some(outcome);
        };
        if outbox
            .attempt
            .as_ref()
            .is_none_or(|current| current.id != attempt)
        {
            return Some(outcome);
        }
        let running = self
            .conversations
            .get(&thread)
            .is_some_and(|session| session.state == ConversationState::Running);
        let Some(current) = outbox.attempt.take() else {
            return Some(outcome);
        };
        let (status, reason, release) = match outcome {
            harness::PeerDeliveryOutcome::Accepted => {
                if current.idle && !running {
                    outbox.turn = Some(attempt);
                }
                (ReceiptStatus::Delivered, None, running)
            }
            harness::PeerDeliveryOutcome::Rejected(reason) => {
                (ReceiptStatus::Failed, Some(reason), true)
            }
            harness::PeerDeliveryOutcome::Unknown(reason) => {
                (ReceiptStatus::Unknown, Some(reason), true)
            }
        };
        if current.idle && release {
            self.release_artifact_reservation(thread);
        }
        let mut receipts: Vec<ArtifactReceipt> = current
            .events
            .into_iter()
            .map(|(browser, event)| receipt(browser, event, status, reason.clone()))
            .collect();
        self.pump_artifacts(thread, &mut receipts);
        self.send_receipts(receipts);
        None
    }

    pub(super) fn artifact_turn_started(&mut self, thread: ThreadId, request_id: Uuid) -> bool {
        let Some(outbox) = self.artifacts.outboxes.get_mut(&thread) else {
            return false;
        };
        let own = outbox
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.id == request_id)
            || outbox.turn == Some(request_id);
        if !own {
            return false;
        }
        outbox.turn = Some(request_id);
        if let Some(session) = self.conversations.get_mut(&thread) {
            session.preparing = false;
            session.outcome = None;
        }
        self.active_threads.insert(thread);
        true
    }

    pub(super) fn artifact_turn_settled(&mut self, thread: ThreadId, request_id: Uuid) {
        if let Some(outbox) = self.artifacts.outboxes.get_mut(&thread)
            && outbox.turn == Some(request_id)
        {
            outbox.turn = None;
        }
    }

    pub(super) fn artifact_has_other_turn(&self, thread: ThreadId, settled: Uuid) -> bool {
        self.artifacts.outboxes.get(&thread).is_some_and(|outbox| {
            outbox.turn.is_some_and(|turn| turn != settled)
                || outbox
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.idle && attempt.id != settled)
        })
    }

    pub(super) fn artifact_delivery_in_flight(&self, thread: ThreadId) -> bool {
        self.artifacts
            .outboxes
            .get(&thread)
            .is_some_and(|outbox| outbox.attempt.is_some())
    }

    fn artifact_can_receive(&self, thread: ThreadId) -> bool {
        let remote = self
            .snapshot
            .threads
            .iter()
            .any(|candidate| candidate.id == thread && candidate.remote.is_some());
        let submission = self
            .snapshot
            .threads
            .iter()
            .any(|candidate| candidate.id == thread && candidate.submission.is_some());
        !self.network_delivery_in_flight(thread)
            && self.conversations.get(&thread).is_some_and(|session| {
                session.commands.is_some()
                    && session.persisted
                    && session.peer_delivery
                    && !session.preparing
                    && session.requests.is_empty()
                    && (session.state == ConversationState::Ready
                        || (session.state == ConversationState::Running && !remote))
                    && (!submission || self.active_threads.contains(&thread))
            })
    }

    fn artifact_wait_reason(&self, thread: ThreadId) -> Option<String> {
        let session = self.conversations.get(&thread)?;
        Some(if !session.peer_delivery {
            session.peer_delivery_reason.clone().map_or_else(
                || "This agent cannot receive artifact events yet".to_owned(),
                |reason| format!("This agent cannot receive artifact events yet: {reason}"),
            )
        } else if !session.requests.is_empty() {
            "Answer the agent's pending request first".to_owned()
        } else {
            "The agent receives it at its next safe moment".to_owned()
        })
    }

    fn pump_artifacts(&mut self, thread: ThreadId, receipts: &mut Vec<ArtifactReceipt>) {
        let ready = self.artifacts.outboxes.get(&thread).is_some_and(|outbox| {
            outbox.attempt.is_none() && outbox.turn.is_none() && !outbox.queued.is_empty()
        });
        if !ready || !self.artifact_can_receive(thread) {
            return;
        }
        let Some(commands) = self
            .conversations
            .get(&thread)
            .and_then(|session| session.commands.clone())
        else {
            return;
        };
        let space = self.thread_space(thread).unwrap_or_default();
        let idle = !self.active_threads.contains(&thread);
        let Some(outbox) = self.artifacts.outboxes.get_mut(&thread) else {
            return;
        };
        let events: Vec<ArtifactEvent> = outbox.queued.drain(..).collect();
        let attempt = Uuid::new_v4();
        let messages = events
            .iter()
            .map(|queued| harness::PeerMessage {
                id: Uuid::new_v4(),
                sender: queued.browser,
                sender_name: queued.title.clone(),
                space,
                body: queued.event.body.clone(),
                origin: harness::ContextOrigin::Artifact,
            })
            .collect();
        let (admission, cancelled) = async_channel::bounded(1);
        if let Err(error) =
            commands.try_send(ConversationCommand::DeliverPeer(harness::PeerDelivery {
                attempt,
                messages,
                cancelled,
            }))
        {
            receipts.extend(events.into_iter().map(|queued| {
                receipt(
                    queued.browser,
                    queued.event.id,
                    ReceiptStatus::Failed,
                    Some(format!(
                        "The agent's command queue refused the event: {error}"
                    )),
                )
            }));
            return;
        }
        outbox.attempt = Some(Attempt {
            id: attempt,
            events: events
                .into_iter()
                .map(|queued| (queued.browser, queued.event.id))
                .collect(),
            _admission: admission,
            idle,
        });
        if idle {
            if let Some(session) = self.conversations.get_mut(&thread) {
                session.preparing = true;
            }
            self.active_threads.insert(thread);
        }
    }

    fn release_artifact_reservation(&mut self, thread: ThreadId) {
        if let Some(session) = self.conversations.get_mut(&thread) {
            session.preparing = false;
            if session.state == ConversationState::Ready {
                self.active_threads.remove(&thread);
            }
        }
    }

    fn reset_stale_outbox(
        &mut self,
        thread: ThreadId,
        generation: Uuid,
        receipts: &mut Vec<ArtifactReceipt>,
    ) {
        if self
            .artifacts
            .outboxes
            .get(&thread)
            .is_some_and(|outbox| outbox.generation != generation)
        {
            self.fail_outbox(
                thread,
                "The agent restarted before this event was delivered; send it again",
                receipts,
            );
        }
    }

    fn fail_outbox(&mut self, thread: ThreadId, reason: &str, receipts: &mut Vec<ArtifactReceipt>) {
        let Some(outbox) = self.artifacts.outboxes.remove(&thread) else {
            return;
        };
        if let Some(attempt) = outbox.attempt {
            receipts.extend(attempt.events.into_iter().map(|(browser, event)| {
                receipt(
                    browser,
                    event,
                    ReceiptStatus::Unknown,
                    Some("The agent stopped before confirming this event".into()),
                )
            }));
        }
        receipts.extend(outbox.queued.into_iter().map(|queued| {
            receipt(
                queued.browser,
                queued.event.id,
                ReceiptStatus::Failed,
                Some(reason.to_owned()),
            )
        }));
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod tests {
    use super::{ArtifactReceipt, ChannelEvent, ReceiptStatus};
    use crate::app::{AppState, Bootstrap, ConversationSession};
    use crate::harness::{
        self, ConversationCommand, ConversationEvent, ConversationState, TurnOutcome, UserRequest,
    };
    use crate::model::{Project, ProjectDefaults, ThreadId, ThreadKind};
    use crate::store::Store;
    use gpui_kit::{AppContext as _, Entity, TestAppContext};
    use std::{cell::RefCell, collections::BTreeMap, path::PathBuf};
    use uuid::Uuid;

    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        _directory: Directory,
        state: Entity<AppState>,
        thread: ThreadId,
        native: async_channel::Receiver<ConversationCommand>,
        receipts: Receipts,
    }

    struct Receipts(
        async_channel::Receiver<Vec<ArtifactReceipt>>,
        RefCell<Vec<ArtifactReceipt>>,
    );
    impl Receipts {
        fn borrow(&self) -> std::cell::Ref<'_, Vec<ArtifactReceipt>> {
            while let Ok(batch) = self.0.try_recv() {
                self.1.borrow_mut().extend(batch);
            }
            self.1.borrow()
        }
    }

    fn open(cx: &mut TestAppContext) -> Fixture {
        cx.dispatcher.allow_parking();
        let directory =
            Directory(std::env::temp_dir().join(format!("drostje-artifacts-{}", Uuid::new_v4())));
        let store = Store::open(directory.0.join("data")).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Artifacts".into(),
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
                    data_dir: directory.0.join("data"),
                    store,
                    snapshot,
                    diagnostics: BTreeMap::new(),
                    repositories: BTreeMap::new(),
                    browser_tools: None,
                },
                cx,
            )
        });
        let (commands, native) = async_channel::bounded(8);
        let thread = state.update(cx, |state, _| {
            state.select_project(project_id);
            state.new_thread(ThreadKind::Conversation).unwrap();
            let thread = state.selected_thread.unwrap();
            let mut session = ConversationSession::new(None);
            session.commands = Some(commands);
            session.state = ConversationState::Ready;
            session.persisted = true;
            session.peer_delivery = true;
            state.conversations.insert(thread, session);
            thread
        });
        let receipts = Receipts(
            state.read_with(cx, |state, _| state.artifact_receipts()),
            RefCell::default(),
        );
        Fixture {
            _directory: directory,
            state,
            thread,
            native,
            receipts,
        }
    }

    fn event(id: &str) -> ChannelEvent {
        ChannelEvent::parse(&format!(
            r#"__DROSTJE_CHANNEL__{{"v":1,"id":"{id}","type":"answer","data":{{"scope":"crate"}}}}"#
        ))
        .unwrap()
        .unwrap()
    }

    fn delivered(native: &async_channel::Receiver<ConversationCommand>) -> harness::PeerDelivery {
        match native.try_recv() {
            Ok(ConversationCommand::DeliverPeer(delivery)) => delivery,
            other => panic!("expected an artifact delivery, got {other:?}"),
        }
    }

    #[gpui_kit::test]
    fn an_idle_agent_wakes_for_artifact_input_and_every_page_learns_the_outcome(
        cx: &mut TestAppContext,
    ) {
        let f = open(cx);
        let (thread, browser) = (f.thread, Uuid::new_v4());
        f.state.update(cx, |state, _| {
            state.artifact_events(thread, browser, "Scope".into(), vec![event("e1")]);
        });
        let first = delivered(&f.native);
        assert_eq!(first.messages.len(), 1);
        assert_eq!(first.messages[0].origin, harness::ContextOrigin::Artifact);
        assert_eq!(first.messages[0].sender, browser);
        assert_eq!(first.messages[0].sender_name, "Scope");
        assert!(first.messages[0].body.contains(r#""scope":"crate""#));
        f.state.update(cx, |state, _| {
            assert!(
                state.conversations[&thread].preparing,
                "idle runtime is reserved"
            );
            assert!(state.active_threads.contains(&thread));
        });
        assert!(f.receipts.borrow().is_empty(), "dispatch is not delivery");

        f.state.update(cx, |state, _| {
            state.artifact_events(thread, browser, "Scope".into(), vec![event("e2")]);
        });
        assert!(f.native.try_recv().is_err());
        assert_eq!(f.receipts.borrow()[0].event, "e2");
        assert_eq!(f.receipts.borrow()[0].status, ReceiptStatus::Queued);

        f.state.update(cx, |state, cx| {
            state.receive_conversation(
                thread,
                ConversationEvent::PeerDelivery {
                    attempt: first.attempt,
                    outcome: harness::PeerDeliveryOutcome::Accepted,
                },
                cx,
            );
            state.receive_conversation(
                thread,
                ConversationEvent::PeerTurnStarted {
                    request_id: first.attempt,
                },
                cx,
            );
            assert!(!state.conversations[&thread].preparing);
        });
        assert!(
            f.native.try_recv().is_err(),
            "an artifact turn is never interrupted as an unauthorized peer turn"
        );
        let delivered_receipt = f.receipts.borrow()[1].clone();
        assert_eq!(
            (delivered_receipt.event.as_str(), delivered_receipt.status),
            ("e1", ReceiptStatus::Delivered)
        );

        f.state.update(cx, |state, cx| {
            state.receive_conversation(
                thread,
                ConversationEvent::TurnSettled {
                    request_id: first.attempt,
                    outcome: TurnOutcome::Completed,
                },
                cx,
            );
            state.receive_conversation(
                thread,
                ConversationEvent::StateChanged(ConversationState::Ready),
                cx,
            );
        });
        let second = delivered(&f.native);
        assert!(second.messages[0].body.contains(r#""id":"e2""#));
        f.state.update(cx, |state, cx| {
            state.receive_conversation(
                thread,
                ConversationEvent::PeerDelivery {
                    attempt: second.attempt,
                    outcome: harness::PeerDeliveryOutcome::Unknown("socket closed".into()),
                },
                cx,
            );
            assert!(
                !state.conversations[&thread].preparing,
                "unknown releases the reservation"
            );
        });
        let last = f.receipts.borrow().last().cloned().unwrap();
        assert_eq!(last.status, ReceiptStatus::Unknown);
        assert_eq!(last.reason.as_deref(), Some("socket closed"));
        assert!(
            f.native.try_recv().is_err(),
            "an unknown outcome is never resent"
        );
    }

    #[gpui_kit::test]
    fn events_without_a_live_agent_fail_and_a_new_runtime_never_inherits_them(
        cx: &mut TestAppContext,
    ) {
        let f = open(cx);
        let (thread, browser) = (f.thread, Uuid::new_v4());
        f.state.update(cx, |state, _| {
            state
                .conversations
                .get_mut(&thread)
                .unwrap()
                .requests
                .insert(
                    "permission".into(),
                    UserRequest {
                        id: "permission".into(),
                        kind: harness::UserRequestKind::Permission,
                        title: "Allow Bash?".into(),
                        options: Vec::new(),
                        text: "cargo test".into(),
                        placeholder: None,
                        deadline: None,
                    },
                );
            state.artifact_events(thread, browser, "Scope".into(), vec![event("held")]);
        });
        assert!(f.native.try_recv().is_err());
        let held = f.receipts.borrow()[0].clone();
        assert_eq!(held.status, ReceiptStatus::Queued);
        assert_eq!(
            held.reason.as_deref(),
            Some("Answer the agent's pending request first")
        );

        f.state.update(cx, |state, _| {
            let mut restarted = ConversationSession::new(None);
            restarted.commands = state.conversations[&thread].commands.clone();
            restarted.state = ConversationState::Ready;
            restarted.persisted = true;
            restarted.peer_delivery = true;
            state.conversations.insert(thread, restarted);
            state.artifact_runtime_changed(thread);
        });
        assert!(
            f.native.try_recv().is_err(),
            "a new generation never inherits events"
        );
        assert_eq!(f.receipts.borrow()[1].status, ReceiptStatus::Failed);

        f.state.update(cx, |state, _| {
            state.conversations.remove(&thread);
            state.artifact_events(thread, browser, "Scope".into(), vec![event("late")]);
        });
        let late = f.receipts.borrow()[2].clone();
        assert_eq!(late.status, ReceiptStatus::Failed);
        assert!(late.reason.unwrap().contains("not running"));
    }

    #[gpui_kit::test]
    fn closing_a_conversation_settles_its_waiting_pages_at_once(cx: &mut TestAppContext) {
        let f = open(cx);
        let (thread, browser) = (f.thread, Uuid::new_v4());
        f.state.update(cx, |state, _| {
            state.conversations.get_mut(&thread).unwrap().peer_delivery = false;
            state.artifact_events(thread, browser, "Scope".into(), vec![event("waiting")]);
        });
        assert_eq!(f.receipts.borrow()[0].status, ReceiptStatus::Queued);
        f.state.update(cx, |state, _| state.close_thread(thread));
        let closed = f.receipts.borrow()[1].clone();
        assert_eq!(
            (closed.event.as_str(), closed.status),
            ("waiting", ReceiptStatus::Failed)
        );
        assert!(closed.reason.unwrap().contains("closed"));
        assert!(
            matches!(f.native.try_recv(), Ok(ConversationCommand::Close)),
            "the runtime closes; the event was never sent to it"
        );
    }
}
