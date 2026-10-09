use crate::model::ThreadId;
use std::collections::HashMap;

/// What a thread's agent is doing, as its tab, Projects row and title bar show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadActivity {
    Idle,
    Running,
    /// A turn ended while the thread was not selected; `failed` unless it completed.
    Unseen {
        failed: bool,
    },
}

/// Turns that ended out of sight, kept until their thread is next selected. Transient:
/// a restart forgets them rather than guessing what the user already read.
#[derive(Default)]
pub struct UnseenTurns(HashMap<ThreadId, bool>);

impl UnseenTurns {
    /// Records the end of a turn on `id`; one ending on the selected thread was seen.
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
        // A new turn shows as running; its own end replaces the earlier outcome.
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
