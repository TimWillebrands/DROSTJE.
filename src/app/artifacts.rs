//! The artifact channel: the user's interactions with an HTML artifact reach the
//! Conversation that showed it as artifact-origin context, through the same native
//! injection path as peer messages. Events live in memory only. Nothing is persisted,
//! a dispatched event is never resent, and no Draft or prompt is touched.
//!
//! Lifecycle per Conversation runtime generation: events queue until the runtime can
//! receive context (idle, or running locally), then one attempt carries every queued
//! event. An idle runtime starts an artifact-origin turn; a running one is steered at
//! its next safe boundary. Every event's page learns its outcome through a receipt:
//! `queued` is intermediate, and each queued event later gets a final one: delivery, a
//! failure, or `unknown`. Closing or replacing a runtime settles its events at once
//! (`close_artifacts`), and a new generation never inherits an older one's events.
use super::*;
use crate::browser::{ChannelEvent, ReceiptStatus};

/// Events held per Conversation before the page is asked to wait.
const MAX_QUEUED: usize = 128;

/// One event from an artifact page, attributed to the browser session that sent it.
#[derive(Clone, Debug)]
pub struct ArtifactEvent {
    pub browser: Uuid,
    pub title: String,
    pub event: ChannelEvent,
}

/// What an artifact page should be told about one of its events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactReceipt {
    pub browser: Uuid,
    pub event: String,
    pub status: ReceiptStatus,
    pub reason: Option<String>,
    /// The page's final answer reached the agent: its purpose is fulfilled.
    pub completes: bool,
}

/// Owns every Conversation's outbox, and the receipt queue the presentation owner drains
/// into the pages. Receipts travel on a channel, not as entity events, so even paths that
/// hold no GPUI context (closing a Conversation) settle their pages immediately.
pub struct ArtifactChannels {
    outboxes: HashMap<ThreadId, Outbox>,
    /// Unbounded on purpose: a dropped final receipt would leave a page waiting. Volume
    /// is bounded by the per-Conversation queue and each page's backlog.
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
    /// The runtime these events were sent to; a new generation never inherits them.
    generation: Uuid,
    queued: VecDeque<ArtifactEvent>,
    attempt: Option<Attempt>,
    /// The artifact-origin turn the runtime started for an idle attempt.
    turn: Option<Uuid>,
}

struct Attempt {
    id: Uuid,
    /// `(browser, event, final)` for every event the attempt carries.
    events: Vec<(Uuid, String, bool)>,
    /// Dropping this closes admission; already admitted context cannot be recalled.
    _admission: async_channel::Sender<()>,
    /// Dispatched to an idle runtime: the session is reserved until its turn starts.
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
        completes: false,
    }
}

impl AppState {
    /// Receipts for artifact pages, in order; the presentation owner forwards them.
    pub fn artifact_receipts(&self) -> async_channel::Receiver<Vec<ArtifactReceipt>> {
        self.artifacts.receipts.1.clone()
    }

    fn send_receipts(&self, receipts: Vec<ArtifactReceipt>) {
        if !receipts.is_empty() {
            // Closed only when AppState itself is gone, with every page.
            let _ = self.artifacts.receipts.0.try_send(receipts);
        }
    }

    /// The Conversation's runtime is being closed or replaced: settle its pages now.
    pub(super) fn close_artifacts(&mut self, thread: ThreadId) {
        let mut receipts = Vec::new();
        self.fail_outbox(
            thread,
            "The Conversation's agent closed before this event was delivered; reopen it, then send again",
            &mut receipts,
        );
        self.send_receipts(receipts);
    }

    /// Accept events from an artifact this Conversation showed. Each event's page gets
    /// a receipt: queued, delivered, failed with a reason, or unknown.
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

    /// Re-check a Conversation after any runtime event; dispatches what can go now.
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

    /// The runtime reported an attempt's outcome. Returns the outcome unhandled when the
    /// attempt is not an artifact delivery, so peer delivery can settle it.
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
            // An idle runtime keeps its reservation until the turn it admits starts.
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
            .map(|(browser, event, last)| ArtifactReceipt {
                completes: last && status == ReceiptStatus::Delivered,
                ..receipt(browser, event, status, reason.clone())
            })
            .collect();
        self.pump_artifacts(thread, &mut receipts);
        self.send_receipts(receipts);
        None
    }

    /// `true` when the started turn is the artifact attempt's own.
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

    /// Another artifact turn, or an idle reservation, still keeps the Conversation busy.
    pub(super) fn artifact_has_other_turn(&self, thread: ThreadId, settled: Uuid) -> bool {
        self.artifacts.outboxes.get(&thread).is_some_and(|outbox| {
            outbox.turn.is_some_and(|turn| turn != settled)
                || outbox
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.idle && attempt.id != settled)
        })
    }

    /// An artifact attempt awaits admission; peer delivery must not race it.
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
                    // A remote native queue cannot revoke accepted next-turn context on
                    // desktop detach, so remote runtimes receive only while idle.
                    && (session.state == ConversationState::Ready
                        || (session.state == ConversationState::Running && !remote))
                    && (!submission || self.active_threads.contains(&thread))
            })
    }

    /// Why queued events wait, shown in the page next to its controls.
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
            // Nothing reached the runtime: the queue refused it, so the page can resend.
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
                .map(|queued| (queued.browser, queued.event.id, queued.event.completes))
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

    /// A new runtime generation never inherits an older one's events.
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

    /// Settle everything an ended runtime held. Dispatched events become unknown, never
    /// failed: the runtime may have admitted them before it stopped.
    fn fail_outbox(&mut self, thread: ThreadId, reason: &str, receipts: &mut Vec<ArtifactReceipt>) {
        let Some(outbox) = self.artifacts.outboxes.remove(&thread) else {
            return;
        };
        if let Some(attempt) = outbox.attempt {
            receipts.extend(attempt.events.into_iter().map(|(browser, event, _)| {
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

    /// Every receipt sent so far, in order.
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
        // Persistence runs on real background executors, as in production.
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

    /// A form answer, final like the kit's default form route.
    fn event(id: &str) -> ChannelEvent {
        ChannelEvent::parse(&format!(
            r#"__DROSTJE_CHANNEL__{{"v":1,"id":"{id}","type":"answer","data":{{"scope":"crate"}},"final":true}}"#
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
        f.state.update(cx, |state, cx| {
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

        // A second event waits for the first attempt; it is never merged into it.
        f.state.update(cx, |state, cx| {
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
        assert!(
            delivered_receipt.completes,
            "a delivered final answer fulfils its page"
        );
        assert!(
            !f.receipts.borrow()[0].completes,
            "a queued answer has not reached the agent yet"
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
        assert!(
            !last.completes,
            "an unconfirmed answer leaves its page open"
        );
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
        f.state.update(cx, |state, cx| {
            // A pending permission request holds artifact input back.
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

        f.state.update(cx, |state, cx| {
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

        f.state.update(cx, |state, cx| {
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
