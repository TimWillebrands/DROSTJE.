//! Which pages pay for frames. Reading pixels back and publishing them is the engine's
//! largest per-page cost, so only pages a pane shows, or a capture waits on, publish.
//! Hidden pages still composite at a low rate: Servo ticks animations and
//! `requestAnimationFrame` only when a WebView paints, so a hidden page keeps running.
use std::time::Duration;

/// The fastest a shown page publishes frames.
pub(super) const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// The fastest a hidden page composites, without reading back or publishing pixels.
pub(super) const HIDDEN_FRAME_INTERVAL: Duration = Duration::from_millis(250);

/// What one engine tick does with a page's pixels.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Paint {
    Skip,
    /// Paint and publish a frame of the current content.
    Publish,
    /// Paint only, so the page's animation clock advances; nothing is read back.
    Composite,
}

#[derive(Debug)]
pub(super) struct FrameDemand {
    /// A pane lays the page out; the UI reports it.
    visible: bool,
    /// Capture request IDs waiting for a published frame of the current content.
    captures: Vec<String>,
    /// Composited while hidden since the last publish, so the retained frame is older
    /// than the page even when nothing is dirty.
    unpublished: bool,
}

impl FrameDemand {
    pub(super) fn new(visible: bool) -> Self {
        Self {
            visible,
            captures: Vec::new(),
            unpublished: false,
        }
    }

    pub(super) fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Someone needs this page's pixels now: a pane shows it or a capture waits on it.
    pub(super) fn wanted(&self) -> bool {
        self.visible || !self.captures.is_empty()
    }

    pub(super) fn request_capture(&mut self, request_id: String) {
        self.captures.push(request_id);
    }

    /// Forget captures whose callers stopped waiting, so they stop forcing frames.
    pub(super) fn retain_captures(&mut self, mut waiting: impl FnMut(&str) -> bool) {
        self.captures.retain(|request_id| waiting(request_id));
    }

    pub(super) fn next(&self, dirty: bool, loading: bool, since_last: Duration) -> Paint {
        if loading {
            Paint::Skip
        } else if self.wanted() {
            if (dirty || self.unpublished) && since_last >= FRAME_INTERVAL {
                Paint::Publish
            } else {
                Paint::Skip
            }
        } else if dirty && since_last >= HIDDEN_FRAME_INTERVAL {
            Paint::Composite
        } else {
            Paint::Skip
        }
    }

    /// Record that `paint` ran.
    pub(super) fn painted(&mut self, paint: &Paint) {
        match paint {
            Paint::Publish => self.unpublished = false,
            Paint::Composite => self.unpublished = true,
            Paint::Skip => {}
        }
    }

    /// The captures a published frame now answers: the page has nothing unpainted.
    /// `current` is the engine's view: not dirty, a verified frame published and no
    /// screenshot barrier outstanding.
    pub(super) fn settle(&mut self, current: bool) -> Vec<String> {
        if current && !self.unpublished {
            std::mem::take(&mut self.captures)
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One page as the engine loop drives it: Servo marks it dirty, ticks paint it.
    struct Page {
        demand: FrameDemand,
        dirty: bool,
        published: u32,
        composited: u32,
    }

    impl Page {
        fn tick(&mut self) {
            let paint = self.demand.next(self.dirty, false, HIDDEN_FRAME_INTERVAL);
            match paint {
                Paint::Publish => self.published += 1,
                Paint::Composite => self.composited += 1,
                Paint::Skip => return,
            }
            self.dirty = false;
            self.demand.painted(&paint);
        }
    }

    #[test]
    fn hidden_dirty_page_publishes_nothing_and_becoming_visible_publishes_once() {
        let mut page = Page {
            demand: FrameDemand::new(false),
            dirty: true,
            published: 0,
            composited: 0,
        };
        page.tick();
        page.tick();
        assert_eq!(page.published, 0, "hidden pages read nothing back");
        assert_eq!(page.composited, 1, "but composite so animations advance");
        page.demand.set_visible(true);
        page.tick();
        assert_eq!(
            page.published, 1,
            "shown again: the composited content publishes"
        );
        page.tick();
        page.tick();
        assert_eq!(page.published, 1, "once, until the page changes again");
        page.dirty = true;
        page.tick();
        assert_eq!(page.published, 2);
    }

    #[test]
    fn rates_follow_visibility_and_loading_pages_wait() {
        let shown = FrameDemand::new(true);
        let hidden = FrameDemand::new(false);
        assert_eq!(shown.next(true, false, FRAME_INTERVAL), Paint::Publish);
        assert_eq!(shown.next(true, false, Duration::ZERO), Paint::Skip);
        assert_eq!(shown.next(false, false, FRAME_INTERVAL), Paint::Skip);
        assert_eq!(shown.next(true, true, FRAME_INTERVAL), Paint::Skip);
        assert_eq!(hidden.next(true, false, FRAME_INTERVAL), Paint::Skip);
        assert_eq!(
            hidden.next(true, false, HIDDEN_FRAME_INTERVAL),
            Paint::Composite
        );
        assert_eq!(
            hidden.next(false, false, HIDDEN_FRAME_INTERVAL),
            Paint::Skip
        );
    }

    #[test]
    fn captures_publish_hidden_pages_and_settle_only_on_current_frames() {
        let mut demand = FrameDemand::new(false);
        demand.painted(&Paint::Composite);
        demand.request_capture("a".into());
        assert!(demand.wanted());
        assert!(
            demand.settle(true).is_empty(),
            "a composite since the last publish is unpublished content"
        );
        assert_eq!(
            demand.next(false, false, FRAME_INTERVAL),
            Paint::Publish,
            "a capture publishes a hidden page's unpublished content"
        );
        demand.painted(&Paint::Publish);
        assert!(demand.settle(false).is_empty(), "not current yet");
        assert_eq!(demand.settle(true), ["a"]);
        assert!(!demand.wanted(), "a settled capture stops forcing frames");
        demand.request_capture("b".into());
        demand.request_capture("c".into());
        demand.retain_captures(|request_id| request_id == "c");
        assert_eq!(demand.settle(true), ["c"], "abandoned captures are dropped");
    }
}
