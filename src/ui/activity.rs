//! How a thread shows its agent's activity: a dot that breathes while a turn runs and
//! stays lit after one ends out of sight, and a hairline sweeping along the running
//! thread's edges. Every copy shares one clock, so tab, row and composer move together;
//! reduced motion keeps them still. Both use the theme's signature color, the wordmark's
//! full stop; a failed turn uses danger.

use super::theme;
use crate::app::ThreadActivity;
use gpui_kit::{
    component::{ActiveTheme, tooltip::Tooltip},
    *,
};
use std::time::Duration;

/// Repaints per second for the ambient animations; smooth enough, and cheap.
const MAX_FPS: f32 = 30.;
/// The sweeping segment's share of the line.
const SEGMENT: f32 = 0.3;

/// The status dot for `activity`, or nothing while the thread is idle. `id` names the
/// element and its debug selector, so it must be unique in the window.
pub fn mark(id: SharedString, activity: ThreadActivity, cx: &App) -> Option<AnyElement> {
    let (color, tooltip) = match activity {
        ThreadActivity::Idle => return None,
        ThreadActivity::Running => (theme::signature(cx), "Turn running"),
        ThreadActivity::Unseen { failed: false } => (theme::signature(cx), "Turn finished"),
        ThreadActivity::Unseen { failed: true } => (cx.theme().danger, "Turn did not complete"),
    };
    let selector = id.clone();
    let dot = div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex_shrink_0()
        .size(rems(0.5))
        .rounded_full()
        .bg(color)
        .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx));
    if activity != ThreadActivity::Running || cx.reduce_motion() {
        return Some(dot.into_any_element());
    }
    Some(
        dot.with_animation(
            "activity-breath",
            Animation::new(Duration::from_millis(1600))
                .repeat_synced()
                .with_max_fps(MAX_FPS)
                .with_easing(pulsating_between(0.3, 1.)),
            |dot, opacity| dot.opacity(opacity),
        )
        .into_any_element(),
    )
}

/// A full-width hairline whose bright segment sweeps back and forth while a turn runs;
/// the caller places it on an edge. Reduced motion shows the whole line, still.
pub fn sweep(cx: &App) -> AnyElement {
    let color = theme::signature(cx);
    let track = div()
        .debug_selector(|| "activity-sweep".into())
        .w_full()
        .h(rems(0.125))
        .overflow_hidden();
    if cx.reduce_motion() {
        return track.bg(color.opacity(0.6)).into_any_element();
    }
    track
        .relative()
        .child(
            div()
                .debug_selector(|| "activity-sweep-segment".into())
                .absolute()
                .top_0()
                .h_full()
                .w(relative(SEGMENT))
                .rounded_full()
                .bg(color)
                .with_animation(
                    "activity-sweep",
                    Animation::new(Duration::from_millis(2400))
                        .repeat_synced()
                        .with_max_fps(MAX_FPS)
                        .with_easing(bounce(ease_in_out)),
                    |segment, travel| segment.left(relative(travel * (1. - SEGMENT))),
                ),
        )
        .into_any_element()
}

#[cfg(all(test, feature = "runtime-qa"))]
mod tests {
    use super::{mark, sweep};
    use crate::app::ThreadActivity;

    #[gpui_kit::test]
    fn idle_threads_carry_no_mark(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::init);
        for reduce_motion in [false, true] {
            cx.update(|cx| {
                cx.set_reduce_motion(reduce_motion);
                assert!(mark("idle".into(), ThreadActivity::Idle, cx).is_none());
                for activity in [
                    ThreadActivity::Running,
                    ThreadActivity::Unseen { failed: false },
                    ThreadActivity::Unseen { failed: true },
                ] {
                    assert!(mark("lit".into(), activity, cx).is_some(), "{activity:?}");
                }
            });
        }
    }

    #[gpui_kit::test]
    fn sweep_stays_inside_its_edge(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::{
            AppContext, Context, IntoElement, ParentElement, Render, Styled, VisualTestContext,
            Window, component::Root, div, px,
        };

        struct Edge;
        impl Render for Edge {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().w(px(400.)).child(sweep(cx))
            }
        }

        cx.update(gpui_kit::init);
        let window = cx.add_window(|window, cx| Root::new(cx.new(|_| Edge), window, cx));
        let bounds = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
            let mut visual = VisualTestContext::from_window(window.into(), cx);
            (
                visual.debug_bounds("activity-sweep").expect("line renders"),
                visual.debug_bounds("activity-sweep-segment"),
            )
        };

        let (line, segment) = bounds(cx);
        assert_eq!(line.size.width, px(400.));
        let segment = segment.expect("a moving segment");
        assert_eq!(segment.size.width, px(120.));
        assert!(segment.left() >= line.left() && segment.right() <= line.right());

        cx.update(|cx| cx.set_reduce_motion(true));
        let (line, segment) = bounds(cx);
        assert_eq!(line.size.width, px(400.));
        assert!(segment.is_none(), "reduced motion shows a still line");
    }
}
