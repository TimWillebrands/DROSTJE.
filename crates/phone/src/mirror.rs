use uuid::Uuid;

use crate::{messages::Down, view::PhoneView};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Changed,
    Reset,
    Unchanged,
    NeedsSync,
    Revoked,
}

#[derive(Clone, Debug, Default)]
pub struct Mirror {
    pub view: PhoneView,
    position: Option<(Uuid, u64)>,
    awaiting_reset: bool,
    pub desktop_online: Option<bool>,
}

impl Mirror {
    pub fn from_cache(view: PhoneView) -> Self {
        Self {
            view,
            position: None,
            awaiting_reset: true,
            desktop_online: None,
        }
    }

    pub fn synced(&self) -> bool {
        self.position.is_some() && !self.awaiting_reset
    }

    pub fn awaiting_reset(&self) -> bool {
        self.awaiting_reset
    }

    pub fn request_reset(&mut self) {
        self.awaiting_reset = true;
    }

    fn follows(&self, epoch: Uuid, seq: u64) -> Result<bool, Applied> {
        match self.position {
            Some((current, last)) if current == epoch && seq <= last => Ok(false),
            Some((current, last))
                if current == epoch && seq == last + 1 && !self.awaiting_reset =>
            {
                Ok(true)
            }
            _ => Err(self.gap()),
        }
    }

    fn gap(&self) -> Applied {
        if self.awaiting_reset {
            Applied::Unchanged
        } else {
            Applied::NeedsSync
        }
    }

    pub fn receive(&mut self, down: Down) -> Applied {
        match down {
            Down::Revoked => Applied::Revoked,
            Down::Update {
                epoch,
                seq,
                reset: true,
                patch,
            } => {
                let mut view = PhoneView::default();
                if view.apply(patch).is_err() {
                    let applied = self.gap();
                    self.awaiting_reset = true;
                    return applied;
                }
                self.view = view;
                self.position = Some((epoch, seq));
                self.awaiting_reset = false;
                self.desktop_online = Some(true);
                Applied::Reset
            }
            Down::Update {
                epoch, seq, patch, ..
            } => match self.follows(epoch, seq) {
                Ok(false) => Applied::Unchanged,
                Ok(true) => {
                    let mut view = self.view.clone();
                    if view.apply(patch).is_err() {
                        self.awaiting_reset = true;
                        return Applied::NeedsSync;
                    }
                    self.view = view;
                    self.position = Some((epoch, seq));
                    self.desktop_online = Some(true);
                    Applied::Changed
                }
                Err(applied) => {
                    self.awaiting_reset = true;
                    applied
                }
            },
            Down::Presence { epoch, seq, online } => match self.follows(epoch, seq) {
                Ok(false) => Applied::Unchanged,
                Ok(true) => {
                    self.position = Some((epoch, seq));
                    self.desktop_online = Some(online);
                    Applied::Changed
                }
                Err(applied) => {
                    if !online {
                        self.desktop_online = Some(false);
                    }
                    self.awaiting_reset = true;
                    applied
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Patch;

    fn update(epoch: u128, seq: u64, reset: bool, desktop: &str) -> Down {
        Down::Update {
            epoch: Uuid::from_u128(epoch),
            seq,
            reset,
            patch: Patch {
                desktop: Some(desktop.into()),
                ..Patch::default()
            },
        }
    }

    #[test]
    fn a_reset_starts_the_mirror_and_updates_follow_in_order() {
        let mut mirror = Mirror::default();
        assert!(!mirror.synced());
        assert_eq!(mirror.receive(update(1, 4, true, "a")), Applied::Reset);
        assert!(mirror.synced());
        assert_eq!(mirror.receive(update(1, 5, false, "b")), Applied::Changed);
        assert_eq!(mirror.view.desktop, "b");
        assert_eq!(mirror.desktop_online, Some(true));
    }

    #[test]
    fn replayed_messages_are_ignored() {
        let mut mirror = Mirror::default();
        mirror.receive(update(1, 1, true, "a"));
        mirror.receive(update(1, 2, false, "b"));
        assert_eq!(mirror.receive(update(1, 2, false, "b")), Applied::Unchanged);
        assert_eq!(mirror.receive(update(1, 1, true, "a")), Applied::Reset);
        assert_eq!(mirror.view.desktop, "a");
    }

    #[test]
    fn a_gap_asks_for_one_sync_and_waits_for_the_reset() {
        let mut mirror = Mirror::default();
        mirror.receive(update(1, 1, true, "a"));
        assert_eq!(mirror.receive(update(1, 3, false, "c")), Applied::NeedsSync);
        assert!(mirror.awaiting_reset());
        assert_eq!(mirror.receive(update(1, 4, false, "d")), Applied::Unchanged);
        assert_eq!(mirror.view.desktop, "a");
        assert_eq!(mirror.receive(update(1, 9, true, "z")), Applied::Reset);
        assert_eq!(mirror.view.desktop, "z");
    }

    #[test]
    fn a_restarted_desktop_is_followed_from_its_reset() {
        let mut mirror = Mirror::default();
        mirror.receive(update(1, 7, true, "a"));
        assert_eq!(mirror.receive(update(2, 2, false, "x")), Applied::NeedsSync);
        assert_eq!(mirror.receive(update(2, 1, true, "new")), Applied::Reset);
    }

    #[test]
    fn presence_tracks_whether_the_desktop_runs() {
        let mut mirror = Mirror::default();
        mirror.receive(update(1, 1, true, "a"));
        let offline = Down::Presence {
            epoch: Uuid::from_u128(1),
            seq: 2,
            online: false,
        };
        assert_eq!(mirror.receive(offline), Applied::Changed);
        assert_eq!(mirror.desktop_online, Some(false));
        assert_eq!(mirror.view.desktop, "a");
    }

    #[test]
    fn a_cached_view_shows_until_the_first_reset() {
        let cached = PhoneView {
            desktop: "cached".into(),
            ..PhoneView::default()
        };
        let mut mirror = Mirror::from_cache(cached);
        assert_eq!(mirror.view.desktop, "cached");
        assert!(!mirror.synced());
        assert_eq!(mirror.receive(update(1, 3, false, "x")), Applied::Unchanged);
        assert_eq!(mirror.receive(update(1, 4, true, "fresh")), Applied::Reset);
        assert!(mirror.synced());
    }

    #[test]
    fn revocation_is_reported() {
        assert_eq!(Mirror::default().receive(Down::Revoked), Applied::Revoked);
    }
}
