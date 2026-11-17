use super::AppState;
use crate::factory::{Liveness, ReportedState};
use crate::harness::{ConversationState, TurnOutcome};
use crate::model::{Thread, ThreadId, ThreadKind};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadCondition {
    Waiting,
    Starting,
    Running,
    Stopping,
    Paused,
    Failed,
    Disconnected,
    Unresolved,
}

impl ThreadCondition {
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Stopping => "Stopping",
            Self::Paused => "Paused",
            Self::Failed => "Failed",
            Self::Disconnected => "Disconnected",
            Self::Unresolved => "Unresolved",
        }
    }

    pub fn is_problem(self) -> bool {
        match self {
            Self::Failed | Self::Disconnected | Self::Unresolved => true,
            Self::Waiting | Self::Starting | Self::Running | Self::Stopping | Self::Paused => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadActivity {
    Idle,
    Running,
    Unseen { failed: bool },
}

#[derive(Default)]
pub struct UnseenTurns(HashMap<ThreadId, bool>);

impl UnseenTurns {
    pub fn turn_ended(&mut self, id: ThreadId, selected: Option<ThreadId>, failed: bool) {
        if selected == Some(id) {
            self.0.remove(&id);
        } else {
            self.0.insert(id, failed);
        }
    }

    pub fn seen(&mut self, id: ThreadId) {
        self.0.remove(&id);
    }

    pub fn activity(&self, id: ThreadId, running: bool) -> ThreadActivity {
        match (running, self.0.get(&id)) {
            (true, _) => ThreadActivity::Running,
            (false, Some(&failed)) => ThreadActivity::Unseen { failed },
            (false, None) => ThreadActivity::Idle,
        }
    }
}

impl AppState {
    pub fn thread_condition(&self, thread: &Thread) -> Option<ThreadCondition> {
        match thread.kind {
            ThreadKind::Conversation => {
                if let Some(session) = self.conversations.get(&thread.id) {
                    if !session.requests.is_empty()
                        || session.state == ConversationState::WaitingForInput
                    {
                        return Some(ThreadCondition::Waiting);
                    }
                    match session.state {
                        ConversationState::Starting => return Some(ThreadCondition::Starting),
                        ConversationState::Running => return Some(ThreadCondition::Running),
                        ConversationState::Interrupting => return Some(ThreadCondition::Stopping),
                        ConversationState::Failed => return Some(ThreadCondition::Failed),
                        ConversationState::Disconnected => {
                            return Some(ThreadCondition::Disconnected);
                        }
                        ConversationState::Ready | ConversationState::WaitingForInput => {}
                    }
                    if session.error.is_some() || session.outcome == Some(TurnOutcome::Failed) {
                        return Some(ThreadCondition::Failed);
                    }
                    if session.outcome == Some(TurnOutcome::Unknown) {
                        return Some(ThreadCondition::Unresolved);
                    }
                }
                thread
                    .submission
                    .as_ref()
                    .map(|_| ThreadCondition::Unresolved)
            }
            ThreadKind::Factory => {
                if self.pending_factory.contains(&thread.id) {
                    return Some(ThreadCondition::Starting);
                }
                let launch = self
                    .snapshot
                    .launches
                    .iter()
                    .find(|launch| launch.factory_thread_id == thread.id)?;
                if let Some(run) = self.factory_runs.get(&launch.id) {
                    match run.reported_state {
                        ReportedState::Completed => return None,
                        ReportedState::Failed => return Some(ThreadCondition::Failed),
                        ReportedState::Paused => return Some(ThreadCondition::Paused),
                        _ => {}
                    }
                    if run.launch.launch_error.is_some() {
                        return Some(ThreadCondition::Failed);
                    }
                    return Some(match run.liveness {
                        Liveness::OwnedRunning if run.reported_state == ReportedState::Pending => {
                            ThreadCondition::Waiting
                        }
                        Liveness::OwnedRunning => ThreadCondition::Running,
                        Liveness::Exited(_) | Liveness::Unverified => ThreadCondition::Unresolved,
                    });
                }
                Some(if launch.launch_error.is_some() {
                    ThreadCondition::Failed
                } else if self.owned_launches.contains(&launch.id) {
                    ThreadCondition::Running
                } else {
                    ThreadCondition::Unresolved
                })
            }
        }
    }

    pub fn thread_activity(&self, id: ThreadId) -> ThreadActivity {
        self.unseen_turns
            .activity(id, self.active_threads.contains(&id))
    }

    pub(super) fn turn_ended(&mut self, id: ThreadId) {
        let failed = self.conversations.get(&id).is_none_or(|session| {
            matches!(
                session.state,
                ConversationState::Failed | ConversationState::Disconnected
            ) || !matches!(
                session.outcome,
                Some(TurnOutcome::Completed | TurnOutcome::Interrupted)
            )
        });
        self.unseen_turns
            .turn_ended(id, self.selected_thread, failed);
    }
}

#[cfg(test)]
mod tests {
    use super::{ThreadActivity, UnseenTurns};
    use uuid::Uuid;

    #[test]
    fn turns_ending_out_of_sight_stay_unseen_until_selected() {
        let (shown, background) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut unseen = UnseenTurns::default();
        assert_eq!(unseen.activity(background, true), ThreadActivity::Running);

        unseen.turn_ended(shown, Some(shown), false);
        unseen.turn_ended(background, Some(shown), false);
        assert_eq!(unseen.activity(shown, false), ThreadActivity::Idle);
        assert_eq!(
            unseen.activity(background, false),
            ThreadActivity::Unseen { failed: false }
        );
        assert_eq!(unseen.activity(background, true), ThreadActivity::Running);
        unseen.turn_ended(background, None, true);
        assert_eq!(
            unseen.activity(background, false),
            ThreadActivity::Unseen { failed: true }
        );

        unseen.seen(background);
        assert_eq!(unseen.activity(background, false), ThreadActivity::Idle);
    }
}
