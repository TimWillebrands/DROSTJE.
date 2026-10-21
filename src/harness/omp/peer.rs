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
        if self.peers.handshake {
            return;
        }
        let found = raw.as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry["name"]
                    .as_str()
                    .is_some_and(|name| name.trim_start_matches('/') == COMMAND)
                    && entry["source"] == "extension"
            })
        });
        if found {
            self.peers.handshake = true;
            self.peers.handshake_deadline = Some(Instant::now() + CONTROL_TIMEOUT);
            if let Err(error) = self.peer_control(json!({"v":1,"op":"hello"}), None) {
                self.notice(
                    "warning",
                    format!("Native peer delivery unavailable: {error:#}"),
                );
            }
        } else {
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
            // Reserve idle admission before writing, without inventing a Submit.
            let idle = !self.active();
            let envelope = json!({"v":1,"op":"deliver","attempt":attempt,"messages":delivery.messages,"content":content,"trigger_turn":true});
            let rpc_id = self.peer_control(envelope, Some(attempt))?;
            if idle {
                self.peers.idle = Some(attempt);
            }
            self.peers.pending.insert(
                attempt,
                PeerAttempt {
                    messages: delivery.messages,
                    cancelled: delivery.cancelled,
                    terminal: false,
                    accepted: false,
                    started: false,
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
            pending.messages = Vec::new();
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
        if event["event"] == "ready" && self.peers.handshake {
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
                if event["idle"] == true {
                    self.peers.idle = Some(attempt);
                }
                if event["idle"] == false && self.peers.idle == Some(attempt) {
                    self.peers.idle = None;
                }
            }
            // Emitted by the registered message_start observer, not sendMessage's
            // void return or the prompt RPC acknowledgement.
            "accepted" => self.peer_outcome(attempt, PeerDeliveryOutcome::Accepted),
            "turn_started" if !self.peers.pending[&attempt].started => {
                self.peers.idle = None;
                self.peers.turns.push(attempt);
                self.peers.pending.get_mut(&attempt).unwrap().started = true;
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
                !pending.terminal
                    && (all || pending.cancelled.is_closed() || !pending.cancelled.is_empty())
            })
            .map(|(attempt, _)| *attempt)
            .collect();
        for attempt in attempts {
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
        self.peers.failed = false;
        self.peers.interrupted = false;
        had_turn
    }
    pub(super) fn peer_settled(&mut self) -> bool {
        self.peers.idle = None;
        let attempts: Vec<_> = self
            .peers
            .pending
            .iter()
            .filter(|(_, pending)| !pending.terminal)
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
