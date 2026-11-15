use super::AppState;
use crate::harness::{ConversationState, TurnOutcome};
use crate::model::ThreadId;
use std::collections::HashMap;

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
