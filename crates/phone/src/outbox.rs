use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::{messages::Down, view::PhoneView};

pub const INTERVAL: Duration = Duration::from_secs(1);
pub const SLOW_INTERVAL: Duration = Duration::from_secs(5);
pub const SLOW_FOR: Duration = Duration::from_secs(120);
pub const HEARTBEAT: Duration = Duration::from_secs(120);
pub const FAILURE_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct Outbox {
    epoch: Uuid,
    seq: u64,
    sent: PhoneView,
    reset_needed: bool,
    next_allowed: Instant,
    slow_until: Option<Instant>,
    last_sent: Option<Instant>,
}

#[derive(Debug)]
pub struct Prepared {
    pub down: Down,
    seq: u64,
    view: PhoneView,
}

impl Outbox {
    pub fn new(now: Instant) -> Self {
        Self {
            epoch: Uuid::new_v4(),
            seq: 0,
            sent: PhoneView::default(),
            reset_needed: true,
            next_allowed: now,
            slow_until: None,
            last_sent: None,
        }
    }

    pub fn request_reset(&mut self) {
        self.reset_needed = true;
    }

    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.next_allowed.checked_duration_since(now)
    }

    pub fn prepare(&self, current: &PhoneView, now: Instant) -> Option<Prepared> {
        if now < self.next_allowed {
            return None;
        }
        let seq = self.seq + 1;
        let down = if self.reset_needed {
            Down::Update {
                epoch: self.epoch,
                seq,
                reset: true,
                patch: PhoneView::default().diff(current),
            }
        } else {
            let patch = self.sent.diff(current);
            if patch.is_empty() {
                let idle = self
                    .last_sent
                    .is_none_or(|last| now.duration_since(last) >= HEARTBEAT);
                if !idle {
                    return None;
                }
                Down::Presence {
                    epoch: self.epoch,
                    seq,
                    online: true,
                }
            } else {
                Down::Update {
                    epoch: self.epoch,
                    seq,
                    reset: false,
                    patch,
                }
            }
        };
        Some(Prepared {
            down,
            seq,
            view: current.clone(),
        })
    }

    pub fn farewell(&mut self) -> Down {
        self.seq += 1;
        Down::Presence {
            epoch: self.epoch,
            seq: self.seq,
            online: false,
        }
    }

    fn interval(&self, now: Instant) -> Duration {
        if self.slow_until.is_some_and(|until| now < until) {
            SLOW_INTERVAL
        } else {
            INTERVAL
        }
    }

    pub fn delivered(&mut self, prepared: Prepared, now: Instant) {
        self.seq = prepared.seq;
        self.sent = prepared.view;
        self.reset_needed = false;
        self.last_sent = Some(now);
        self.next_allowed = now + self.interval(now);
    }

    pub fn rate_limited(
        &mut self,
        prepared: Prepared,
        retry_after: Option<Duration>,
        now: Instant,
    ) {
        self.seq = prepared.seq;
        self.reset_needed = true;
        self.slow_until = Some(now + SLOW_FOR);
        self.next_allowed = now + retry_after.unwrap_or(SLOW_INTERVAL).max(SLOW_INTERVAL);
    }

    pub fn failed(&mut self, prepared: Prepared, now: Instant) {
        self.seq = prepared.seq;
        self.reset_needed = true;
        self.next_allowed = now + FAILURE_BACKOFF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::{Applied, Mirror};

    fn view(desktop: &str) -> PhoneView {
        PhoneView {
            desktop: desktop.into(),
            ..PhoneView::default()
        }
    }

    #[test]
    fn the_first_message_is_a_reset_and_later_ones_are_patches() {
        let start = Instant::now();
        let mut outbox = Outbox::new(start);
        let mut mirror = Mirror::default();
        let first = outbox.prepare(&view("a"), start).unwrap();
        assert!(matches!(first.down, Down::Update { reset: true, .. }));
        assert_eq!(mirror.receive(first.down.clone()), Applied::Reset);
        outbox.delivered(first, start);
        assert!(outbox.prepare(&view("b"), start).is_none());
        let later = start + INTERVAL;
        let second = outbox.prepare(&view("b"), later).unwrap();
        assert!(matches!(second.down, Down::Update { reset: false, .. }));
        assert_eq!(mirror.receive(second.down.clone()), Applied::Changed);
        assert_eq!(mirror.view, view("b"));
    }

    #[test]
    fn nothing_is_sent_for_an_unchanged_view_until_the_heartbeat() {
        let start = Instant::now();
        let mut outbox = Outbox::new(start);
        let first = outbox.prepare(&view("a"), start).unwrap();
        outbox.delivered(first, start);
        assert!(outbox.prepare(&view("a"), start + INTERVAL).is_none());
        let beat = outbox.prepare(&view("a"), start + HEARTBEAT).unwrap();
        assert!(matches!(beat.down, Down::Presence { online: true, .. }));
    }

    #[test]
    fn a_failed_or_uncertain_publish_is_followed_by_a_reset_with_a_new_sequence() {
        let start = Instant::now();
        let mut outbox = Outbox::new(start);
        let mut mirror = Mirror::default();
        let first = outbox.prepare(&view("a"), start).unwrap();
        mirror.receive(first.down.clone());
        outbox.delivered(first, start);
        let lost = outbox.prepare(&view("b"), start + INTERVAL).unwrap();
        outbox.failed(lost, start + INTERVAL);
        assert!(outbox.prepare(&view("c"), start + INTERVAL).is_none());
        let retry = outbox
            .prepare(&view("c"), start + INTERVAL + FAILURE_BACKOFF)
            .unwrap();
        let Down::Update { seq, reset, .. } = &retry.down else {
            panic!("expected an update")
        };
        assert_eq!((*seq, *reset), (3, true));
        assert_eq!(mirror.receive(retry.down.clone()), Applied::Reset);
        assert_eq!(mirror.view, view("c"));
    }

    #[test]
    fn rate_limits_wait_for_the_server_and_slow_down_for_a_while() {
        let start = Instant::now();
        let mut outbox = Outbox::new(start);
        let first = outbox.prepare(&view("a"), start).unwrap();
        outbox.rate_limited(first, Some(Duration::from_secs(30)), start);
        assert_eq!(outbox.wait(start), Some(Duration::from_secs(30)));
        let resumed = start + Duration::from_secs(30);
        let next = outbox.prepare(&view("a"), resumed).unwrap();
        outbox.delivered(next, resumed);
        assert_eq!(outbox.wait(resumed), Some(SLOW_INTERVAL));
        let calm = resumed + SLOW_FOR;
        let later = outbox.prepare(&view("b"), calm).unwrap();
        outbox.delivered(later, calm);
        assert_eq!(outbox.wait(calm), Some(INTERVAL));
    }

    #[test]
    fn a_sync_request_resends_everything() {
        let start = Instant::now();
        let mut outbox = Outbox::new(start);
        let first = outbox.prepare(&view("a"), start).unwrap();
        outbox.delivered(first, start);
        outbox.request_reset();
        let again = outbox.prepare(&view("a"), start + INTERVAL).unwrap();
        assert!(matches!(again.down, Down::Update { reset: true, .. }));
    }

    #[test]
    fn the_farewell_takes_the_next_sequence() {
        let start = Instant::now();
        let mut outbox = Outbox::new(start);
        let mut mirror = Mirror::default();
        let first = outbox.prepare(&view("a"), start).unwrap();
        mirror.receive(first.down.clone());
        outbox.delivered(first, start);
        assert_eq!(mirror.receive(outbox.farewell()), Applied::Changed);
        assert_eq!(mirror.desktop_online, Some(false));
    }
}
