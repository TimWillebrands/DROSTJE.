use super::*;

pub(super) const COMMAND: &str = "__drostje_peer";
const STATUS: &str = "drostje.peer.v1";

#[derive(Default)]
pub(super) struct Peers {
    pub ready: bool,
    pub handshake: bool,
    pub handshake_deadline: Option<Instant>,
    pub idle: Option<Uuid>,
    pub turns: Vec<Uuid>,
    pub failed: bool,
    pub interrupted: bool,
    pending: HashMap<Uuid, PeerAttempt>,
}
struct PeerAttempt {
    messages: Vec<PeerMessage>,
    rpc_id: String,
    cancelled: async_channel::Receiver<()>,
    terminal: bool,
    accepted: bool,
    started: bool,
    dispatched: bool,
    cancel_sent: bool,
    admission_deadline: Option<Instant>,
}

pub(super) fn messages(raw: &Value) -> Option<(Uuid, Vec<PeerMessage>)> {
    if raw["role"] != "custom"
        || raw["customType"] != "drostje-peer"
        || raw["details"]["origin"] != "peer"
        || raw["details"]["v"] != 1
    {
        return None;
    }
    let attempt = serde_json::from_value(raw["details"]["attempt"].clone()).ok()?;
    let messages: Vec<PeerMessage> =
        serde_json::from_value(raw["details"]["messages"].clone()).ok()?;
    if messages.is_empty() || messages.len() > 128 {
        return None;
    }
    Some((attempt, messages))
}

impl Actor {
    pub(super) fn peer_control(
        &mut self,
        envelope: Value,
        attempt: Option<Uuid>,
    ) -> Result<String> {
        let encoded = STANDARD.encode(serde_json::to_vec(&envelope)?);
        let id = Uuid::new_v4().to_string();
        self.request(
            json!({"id":id,"type":"prompt","message":format!("/{COMMAND} {encoded}")}),
            RequestKind::Peer(attempt),
        )?;
        Ok(id)
    }
    pub(super) fn peer_commands(&mut self, raw: &Value) {
        let found = raw.as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry["name"]
                    .as_str()
                    .is_some_and(|name| name.trim_start_matches('/') == COMMAND)
                    && entry["source"] == "extension"
            })
        });
        if found && !self.peers.handshake {
            self.peers.handshake = true;
            self.peers.handshake_deadline = Some(Instant::now() + CONTROL_TIMEOUT);
            if let Err(error) = self.peer_control(json!({"v":1,"op":"hello"}), None) {
                self.notice(
                    "warning",
                    format!("Native peer delivery unavailable: {error:#}"),
                );
            }
        } else if !found {
            self.peers.ready = false;
            self.peers.handshake_deadline = None;
            // A removed private command must never fall through as an ordinary
            // user prompt. Withdraw every still-unwritten private RPC frame.
            let mut withdrawn = std::collections::HashSet::new();
            let queued: Vec<_> = self
                .writes
                .iter()
                .enumerate()
                .filter_map(|(index, bytes)| {
                    if index == 0 && self.write_offset != 0 {
                        return None;
                    }
                    let frame: Value = serde_json::from_slice(bytes).ok()?;
                    let id = frame["id"].as_str()?;
                    self.requests
                        .pending
                        .get(id)
                        .filter(|request| matches!(request.kind, RequestKind::Peer(_)))
                        .map(|_| (index, id.to_owned()))
                })
                .collect();
            for (index, id) in queued.into_iter().rev() {
                self.writes.remove(index);
                self.requests.pending.remove(&id);
                withdrawn.insert(id);
            }
            self.peers.idle = None;
            let pending: Vec<_> = self
                .peers
                .pending
                .iter()
                .filter(|(_, pending)| !pending.terminal)
                .map(|(attempt, pending)| (*attempt, withdrawn.contains(&pending.rpc_id)))
                .collect();
            for (attempt, unsent) in pending {
                let pending = self.peers.pending.get_mut(&attempt).unwrap();
                pending.admission_deadline = None;
                pending.cancel_sent = true;
                let reason = "Native peer extension disappeared; reopen explicitly".into();
                self.peer_outcome(
                    attempt,
                    if unsent {
                        PeerDeliveryOutcome::Rejected(reason)
                    } else {
                        PeerDeliveryOutcome::Unknown(reason)
                    },
                );
            }
            self.emit(ConversationEvent::PeerDeliveryAvailable {
                available: false,
                reason: Some("Native runtime did not register the peer delivery extension".into()),
            });
        }
    }
    pub(super) fn peer_tick(&mut self) -> Result<()> {
        self.cancel_peers(
            false,
            "Peer delivery cancelled after possible dispatch; never replay",
        )?;
        if self
            .peers
            .handshake_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.peers.handshake_deadline = None;
            self.emit(ConversationEvent::PeerDeliveryAvailable {
                available: false,
                reason: Some("Native peer delivery readiness was not confirmed".into()),
            });
        }
        let expired: Vec<_> = self
            .peers
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending
                    .admission_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
            })
            .map(|(attempt, _)| *attempt)
            .collect();
        for attempt in expired {
            self.peers
                .pending
                .get_mut(&attempt)
                .unwrap()
                .admission_deadline = None;
            self.peer_control(
                json!({"v":1,"op":"cancel","attempt":attempt}),
                Some(attempt),
            )?;
            self.peers.pending.get_mut(&attempt).unwrap().cancel_sent = true;
            if self.peers.idle == Some(attempt) {
                self.peers.idle = None;
            }
            self.peer_outcome(attempt, PeerDeliveryOutcome::Unknown(
                "Native peer admission was not confirmed; cancellation tombstone sent, never replay".into()));
        }
        Ok(())
    }
    pub(super) fn deliver_peer(&mut self, delivery: PeerDelivery) {
        let attempt = delivery.attempt;
        let result = (|| -> Result<()> {
            ensure!(
                !self.closing && self.initialized && self.peers.ready,
                "Native peer delivery unavailable"
            );
            ensure!(
                self.dialogs.is_empty() && !self.turn.interrupting,
                "Native peer delivery blocked by user interaction or interrupt"
            );
            ensure!(
                self.peers.idle.is_none(),
                "Native peer turn admission is pending"
            );
            ensure!(
                !self.peers.pending.contains_key(&attempt),
                "Peer delivery attempt cannot be replayed"
            );
            ensure!(
                self.peers.pending.len() < 4096,
                "Peer delivery lifetime limit reached; reopen explicitly"
            );
            ensure!(
                !delivery.cancelled.is_closed() && delivery.cancelled.is_empty(),
                "Peer delivery cancelled before dispatch"
            );
            ensure!(
                !delivery.messages.is_empty() && delivery.messages.len() <= 128,
                "Invalid peer delivery message count"
            );
            let content = super::super::peer_context(&delivery.messages)?;
            // Every dispatch reserves admission until native isIdle resolves the
            // busy-host/native-idle race. Host activity is not native readiness.
            let envelope = json!({"v":1,"op":"deliver","attempt":attempt,"messages":delivery.messages,"content":content,"trigger_turn":true});
            let rpc_id = self.peer_control(envelope, Some(attempt))?;
            self.peers.idle = Some(attempt);
            self.peers.pending.insert(
                attempt,
                PeerAttempt {
                    messages: delivery.messages,
                    cancelled: delivery.cancelled,
                    terminal: false,
                    accepted: false,
                    started: false,
                    dispatched: false,
                    cancel_sent: false,
                    admission_deadline: Some(Instant::now() + CONTROL_TIMEOUT),
                    rpc_id,
                },
            );
            Ok(())
        })();
        if let Err(error) = result {
            self.emit(ConversationEvent::PeerDelivery {
                attempt,
                outcome: PeerDeliveryOutcome::Rejected(format!("{error:#}")),
            });
        }
    }
    pub(super) fn peer_outcome(&mut self, attempt: Uuid, outcome: PeerDeliveryOutcome) {
        let Some(pending) = self.peers.pending.get_mut(&attempt) else {
            return;
        };
        let accepted = matches!(outcome, PeerDeliveryOutcome::Accepted);
        // A real late native echo may resolve an earlier uncertain outcome.
        if pending.accepted || (pending.terminal && !accepted) {
            return;
        }
        pending.terminal = true;
        pending.accepted = accepted;
        if accepted {
            pending.dispatched = true;
            pending.messages.clear();
            pending.admission_deadline = None;
        }
        self.emit(ConversationEvent::PeerDelivery { attempt, outcome });
    }
    pub(super) fn peer_message(&mut self, raw: &Value) {
        let Some((attempt, echoed)) = messages(raw) else {
            return;
        };
        let Some(pending) = self.peers.pending.get(&attempt) else {
            return;
        };
        // Compare the native details to the durable host snapshot, not free text.
        if pending.messages.len() != echoed.len()
            || !pending
                .messages
                .iter()
                .zip(&echoed)
                .all(|(expected, actual)| {
                    expected.id == actual.id
                        && expected.sender == actual.sender
                        && expected.sender_name == actual.sender_name
                        && expected.space == actual.space
                        && expected.body == actual.body
                })
        {
            return;
        }
        self.peer_outcome(attempt, PeerDeliveryOutcome::Accepted);
    }
    pub(super) fn peer_status(&mut self, value: &Value) -> Result<bool> {
        if value["method"] != "setStatus" || value["statusKey"] != STATUS {
            return Ok(false);
        }
        self.emit(ConversationEvent::Diagnostic {
            label: "Native peer lifecycle".into(),
            raw: value.clone(),
        });
        let Some(text) = value["statusText"].as_str() else {
            return Ok(true);
        };
        let Ok(event) = serde_json::from_str::<Value>(text) else {
            return Ok(true);
        };
        let dialect = if self.dialect == Dialect::Omp {
            "omp"
        } else {
            "pi"
        };
        if event["v"] != 1 || event["dialect"] != dialect {
            return Ok(true);
        }
        if event["event"] == "ready"
            && self.peers.handshake
            && self
                .peers
                .handshake_deadline
                .is_some_and(|deadline| Instant::now() < deadline)
        {
            self.peers.ready = true;
            self.peers.handshake_deadline = None;
            self.emit(ConversationEvent::PeerDeliveryAvailable {
                available: true,
                reason: None,
            });
            return Ok(true);
        }
        let Ok(attempt) = serde_json::from_value::<Uuid>(event["attempt"].clone()) else {
            return Ok(true);
        };
        if !self.peers.pending.contains_key(&attempt) {
            return Ok(true);
        }
        match event["event"].as_str().unwrap_or("") {
            "dispatching" => {
                self.peers.pending.get_mut(&attempt).unwrap().dispatched = true;
                if event["idle"] == true {
                    self.peers.idle = Some(attempt);
                }
                if event["idle"] == false && self.peers.idle == Some(attempt) {
                    self.peers.idle = None;
                }
                if event["idle"] == false {
                    self.peers
                        .pending
                        .get_mut(&attempt)
                        .unwrap()
                        .admission_deadline = None;
                }
            }
            // Emitted by the registered message_start observer, not sendMessage's
            // void return or the prompt RPC acknowledgement.
            "accepted" => self.peer_outcome(attempt, PeerDeliveryOutcome::Accepted),
            "turn_started" if !self.peers.pending[&attempt].started => {
                self.peers.idle = None;
                self.peers.turns.push(attempt);
                self.peers.pending.get_mut(&attempt).unwrap().started = true;
                self.peers.pending.get_mut(&attempt).unwrap().dispatched = true;
                self.peers
                    .pending
                    .get_mut(&attempt)
                    .unwrap()
                    .admission_deadline = None;
                match IdleSleepAssertion::acquire("DROSTJE. peer-origin conversation turn") {
                    Ok(assertion) => self.idle_sleep_assertion = Some(assertion),
                    Err(error) => self.notice(
                        "warning",
                        format!("Could not prevent idle sleep during peer turn: {error:#}"),
                    ),
                }
                self.emit(ConversationEvent::PeerTurnStarted {
                    request_id: attempt,
                });
                self.state(ConversationState::Running);
            }
            "rejected" | "unknown" => {
                self.peers
                    .pending
                    .get_mut(&attempt)
                    .unwrap()
                    .admission_deadline = None;
                if self.peers.idle == Some(attempt) {
                    self.peers.idle = None;
                }
                let reason = string(&event, "reason");
                let outcome = if event["event"] == "rejected" {
                    PeerDeliveryOutcome::Rejected(reason)
                } else {
                    PeerDeliveryOutcome::Unknown(reason)
                };
                self.peer_outcome(attempt, outcome);
            }
            _ => {}
        }
        Ok(true)
    }
    pub(super) fn cancel_peers(&mut self, all: bool, reason: &str) -> Result<()> {
        let attempts: Vec<_> = self
            .peers
            .pending
            .iter()
            .filter(|(_, pending)| {
                !pending.accepted
                    && !pending.cancel_sent
                    && (all || pending.cancelled.is_closed() || !pending.cancelled.is_empty())
            })
            .map(|(attempt, _)| *attempt)
            .collect();
        for attempt in attempts {
            let pending = self.peers.pending.get_mut(&attempt).unwrap();
            pending.cancel_sent = true;
            pending.admission_deadline = None;
            let rpc_id = &self.peers.pending[&attempt].rpc_id;
            let queued = self.writes.iter().position(|bytes| {
                serde_json::from_slice::<Value>(bytes)
                    .ok()
                    .is_some_and(|frame| frame["id"].as_str() == Some(rpc_id.as_str()))
            });
            let unsent = queued.is_some_and(|index| index != 0 || self.write_offset == 0);
            if unsent {
                self.writes.remove(queued.unwrap());
                self.requests.pending.remove(rpc_id);
                if self.peers.idle == Some(attempt) {
                    self.peers.idle = None;
                }
                self.peer_outcome(
                    attempt,
                    PeerDeliveryOutcome::Rejected("Peer delivery cancelled before dispatch".into()),
                );
            } else {
                self.peer_control(
                    json!({"v":1,"op":"cancel","attempt":attempt}),
                    Some(attempt),
                )?;
                self.peer_outcome(attempt, PeerDeliveryOutcome::Unknown(reason.into()));
            }
        }
        Ok(())
    }
    pub(super) fn peer_disconnected(&mut self, reason: &str) {
        self.peers.ready = false;
        self.peers.idle = None;
        self.finish_peer_turns(TurnOutcome::Unknown);
        let attempts: Vec<_> = self
            .peers
            .pending
            .iter()
            .filter(|(_, pending)| !pending.terminal)
            .map(|(attempt, _)| *attempt)
            .collect();
        for attempt in attempts {
            self.peer_outcome(attempt, PeerDeliveryOutcome::Unknown(reason.into()));
        }
    }
    pub(super) fn peer_dispatch_error(&mut self, reason: &str) {
        self.peers.idle = None;
        let attempts: Vec<_> = self
            .peers
            .pending
            .iter()
            .filter(|(_, pending)| !pending.terminal)
            .map(|(attempt, _)| *attempt)
            .collect();
        for attempt in attempts {
            self.peer_outcome(attempt, PeerDeliveryOutcome::Unknown(reason.into()));
        }
    }
    pub(super) fn finish_peer_turns(&mut self, outcome: TurnOutcome) -> bool {
        let turns = std::mem::take(&mut self.peers.turns);
        let had_turn = !turns.is_empty();
        for request_id in turns {
            self.emit(ConversationEvent::TurnSettled {
                request_id,
                outcome: outcome.clone(),
            });
        }
        if had_turn && self.turn.id.is_none() {
            self.turn.interrupting = false;
            self.idle_sleep_assertion = None;
        }
        self.peers.failed = false;
        self.peers.interrupted = false;
        had_turn
    }
    pub(super) fn peer_settled(&mut self) -> bool {
        let attempts: Vec<_> = self
            .peers
            .pending
            .iter()
            // An old run can settle while a new idle custom-message admission
            // is in native asynchronous preprocessing. Only its own lifecycle,
            // an explicit failure, or the admission deadline can settle it.
            .filter(|(attempt, pending)| {
                !pending.terminal && pending.dispatched && self.peers.idle != Some(**attempt)
            })
            .map(|(attempt, _)| *attempt)
            .collect();
        for attempt in attempts {
            self.peer_outcome(
                attempt,
                PeerDeliveryOutcome::Unknown(
                    "Native session settled without a peer context receipt; never replay".into(),
                ),
            );
        }
        let outcome = if self.peers.interrupted {
            TurnOutcome::Interrupted
        } else if self.peers.failed {
            TurnOutcome::Failed
        } else {
            TurnOutcome::Completed
        };
        self.finish_peer_turns(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{peer_fixture, peer_status, test_actor, written};
    use super::*;

    fn dispatch(actor: &mut Actor, attempt: Uuid, idle: bool) -> Result<()> {
        let mut frame = peer_status(actor.dialect, "dispatching", Some(attempt));
        let mut status: Value = serde_json::from_str(frame["statusText"].as_str().unwrap())?;
        status["idle"] = json!(idle);
        frame["statusText"] = json!(status.to_string());
        actor.frame(frame)
    }

    #[test]
    fn readiness_timeout_rejects_late_native_ready() -> Result<()> {
        for dialect in [Dialect::Omp, Dialect::Pi] {
            let (mut actor, root) = test_actor(dialect)?;
            actor.peers.handshake = true;
            actor.peers.handshake_deadline = Some(Instant::now() - CONTROL_TIMEOUT);
            actor.peer_tick()?;
            actor.frame(peer_status(dialect, "ready", None))?;
            assert!(!actor.peers.ready);
            assert!(actor.events.iter().any(|event| matches!(
                event,
                ConversationEvent::PeerDeliveryAvailable {
                    available: false,
                    ..
                }
            )));
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn native_idle_decides_admission_in_both_host_activity_races() -> Result<()> {
        for dialect in [Dialect::Omp, Dialect::Pi] {
            for host_busy in [false, true] {
                let (mut actor, root) = test_actor(dialect)?;
                actor.peers.ready = true;
                actor.state = if host_busy {
                    ConversationState::Running
                } else {
                    ConversationState::Ready
                };
                let (delivery, _lease) = peer_fixture();
                let attempt = delivery.attempt;
                actor.deliver_peer(delivery);
                assert_eq!(actor.peers.idle, Some(attempt));
                dispatch(&mut actor, attempt, host_busy)?;
                if host_busy {
                    actor.frame(peer_status(dialect, "turn_started", Some(attempt)))?;
                    assert_eq!(actor.peers.turns, vec![attempt]);
                } else {
                    assert!(actor.peers.idle.is_none());
                    actor.frame(peer_status(dialect, "accepted", Some(attempt)))?;
                    assert!(actor.peers.turns.is_empty());
                }
                fs::remove_dir_all(root)?;
            }
        }
        Ok(())
    }

    #[test]
    fn admission_timeout_tombstones_once_and_correlates_late_lifecycle() -> Result<()> {
        for dialect in [Dialect::Omp, Dialect::Pi] {
            let (mut actor, root) = test_actor(dialect)?;
            actor.peers.ready = true;
            let (delivery, _lease) = peer_fixture();
            let attempt = delivery.attempt;
            actor.deliver_peer(delivery);
            actor.writes.clear();
            dispatch(&mut actor, attempt, true)?;
            actor
                .peers
                .pending
                .get_mut(&attempt)
                .unwrap()
                .admission_deadline = Some(Instant::now() - CONTROL_TIMEOUT);
            actor.peer_tick()?;
            assert!(actor.peers.idle.is_none());
            assert!(actor.peers.pending[&attempt].cancel_sent);
            assert!(actor.events.iter().any(|event| matches!(event,
                ConversationEvent::PeerDelivery { attempt: id, outcome: PeerDeliveryOutcome::Unknown(_) } if *id == attempt)));
            let count = actor.writes.len();
            actor.peer_tick()?;
            assert_eq!(
                actor.writes.len(),
                count,
                "uncertain dispatch is never replayed"
            );
            actor.frame(peer_status(dialect, "turn_started", Some(attempt)))?;
            actor.frame(peer_status(dialect, "accepted", Some(attempt)))?;
            actor.peers.interrupted = true;
            actor.frame(json!({"type":if dialect == Dialect::Omp {"session_settled"} else {"agent_settled"}}))?;
            assert!(actor.events.iter().any(|event| matches!(event,
                ConversationEvent::TurnSettled { request_id, outcome: TurnOutcome::Interrupted } if *request_id == attempt)));
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn dispatch_exception_cancellation_and_replay_never_become_user_prompts() -> Result<()> {
        for dialect in [Dialect::Omp, Dialect::Pi] {
            let (mut actor, root) = test_actor(dialect)?;
            actor.peers.ready = true;
            let (delivery, lease) = peer_fixture();
            let attempt = delivery.attempt;
            let replay = PeerDelivery {
                attempt,
                messages: delivery.messages.clone(),
                cancelled: delivery.cancelled.clone(),
            };
            actor.deliver_peer(delivery);
            actor.writes.clear();
            actor.frame(json!({"type":"extension_error","event":"send_message","error":"native preflight failed"}))?;
            assert!(actor.peers.idle.is_none());
            drop(lease);
            actor.peer_tick()?;
            let frames = written(&actor);
            assert_eq!(
                frames.len(),
                1,
                "uncertain failures still require cancellation tombstones"
            );
            actor.deliver_peer(replay);
            assert_eq!(
                written(&actor).len(),
                1,
                "an attempt is never dispatched twice"
            );
            assert!(
                !actor
                    .events
                    .iter()
                    .any(|event| matches!(event, ConversationEvent::SubmissionAccepted { .. }))
            );
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn old_native_settlement_does_not_expire_an_unobserved_dispatch() -> Result<()> {
        let (mut actor, root) = test_actor(Dialect::Pi)?;
        actor.peers.ready = true;
        let (delivery, _lease) = peer_fixture();
        let attempt = delivery.attempt;
        actor.deliver_peer(delivery);
        actor.frame(json!({"type":"agent_settled"}))?;
        assert_eq!(actor.peers.idle, Some(attempt));
        assert!(!actor.peers.pending[&attempt].terminal);
        dispatch(&mut actor, attempt, true)?;
        actor.frame(json!({"type":"agent_settled"}))?;
        assert_eq!(actor.peers.idle, Some(attempt));
        assert!(!actor.peers.pending[&attempt].terminal);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn native_pi_abort_flag_settles_peer_without_an_assistant_message() -> Result<()> {
        let (mut actor, root) = test_actor(Dialect::Pi)?;
        actor.peers.ready = true;
        let (delivery, _lease) = peer_fixture();
        let attempt = delivery.attempt;
        actor.deliver_peer(delivery);
        actor.frame(peer_status(Dialect::Pi, "turn_started", Some(attempt)))?;
        actor.frame(json!({"type":"agent_settled","aborted":true}))?;
        assert!(actor.events.iter().any(|event| matches!(event,
            ConversationEvent::TurnSettled { request_id, outcome: TurnOutcome::Interrupted } if *request_id == attempt)));
        assert!(!actor.turn.interrupting);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn removed_native_command_withdraws_unwritten_private_prompts() -> Result<()> {
        for dialect in [Dialect::Omp, Dialect::Pi] {
            let (mut actor, root) = test_actor(dialect)?;
            actor.peers.ready = true;
            actor.peers.handshake = true;
            let (delivery, _lease) = peer_fixture();
            let attempt = delivery.attempt;
            actor.deliver_peer(delivery);
            actor.peer_commands(&json!([]));
            assert!(!actor.peers.ready);
            assert!(
                actor.writes.is_empty(),
                "removed private command must not become user input"
            );
            assert!(actor.events.iter().any(|event| matches!(event,
                ConversationEvent::PeerDelivery { attempt: id, outcome: PeerDeliveryOutcome::Rejected(_) } if *id == attempt)));
            actor.peer_commands(&json!([{"name":COMMAND,"source":"extension"}]));
            assert!(
                actor.writes.is_empty(),
                "runtime removal needs explicit reopen, not uncertain retry"
            );
            fs::remove_dir_all(root)?;
        }
        Ok(())
    }
}
