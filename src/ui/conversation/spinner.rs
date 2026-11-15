//! The busy mark of running tool calls and background jobs: the kit's loader icon turning,
//! repainted at most `MAX_FPS` times a second instead of on every vsync, and phase-locked
//! across copies so every spinner in a view repaints in the same frame. Reduced motion
//! keeps it still.

use gpui_kit::{
    component::{Icon, IconName, Sizable, Size},
    *,
};
use std::time::Duration;

/// Repaints per second; a turning loader reads as smooth well below the display rate.
pub(in crate::ui) const MAX_FPS: f32 = 30.;
/// One full turn, as the kit's spinner takes.
const TURN: Duration = Duration::from_millis(800);

#[derive(IntoElement)]
pub(in crate::ui) struct Spinner {
    size: Size,
}

impl Spinner {
    pub(in crate::ui) fn new() -> Self {
        Self { size: Size::Medium }
    }
}

impl Sizable for Spinner {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for Spinner {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().child(
            Icon::new(IconName::Loader)
                .with_size(self.size)
                .with_animation(
                    "drostje-spinner",
                    Animation::new(TURN)
                        .repeat_synced()
                        .with_max_fps(MAX_FPS)
                        .with_easing(ease_in_out),
                    |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
                ),
        )
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod tests {
    use super::{MAX_FPS, Spinner};
    use gpui_kit::{
        AppContext, Context, IntoElement, ParentElement, Render, Window, component::Root,
        component::Sizable, div,
    };
    use std::{cell::Cell, rc::Rc, time::Duration};

    struct Host(Rc<Cell<usize>>);
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.0.set(self.0.get() + 1);
            div()
                .child(Spinner::new().xsmall())
                .child(Spinner::new().xsmall())
        }
    }

    #[gpui_kit::test]
    fn spinners_repaint_on_a_capped_timer_instead_of_every_frame(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let renders = Rc::new(Cell::new(0));
        let window = cx.add_window({
            let renders = renders.clone();
            |window, cx| Root::new(cx.new(|_| Host(renders)), window, cx)
        });
        cx.run_until_parked();
        let first = renders.get();
        assert!(first > 0);

        // No per-frame callback: the next vsync does not render the view again.
        let callbacks = cx
            .update_window(window.into(), |_, window, cx| {
                window.simulate_next_frame(cx)
            })
            .unwrap();
        assert_eq!(callbacks, 0, "a spinner must not request every frame");
        cx.run_until_parked();
        assert_eq!(renders.get(), first);

        // A tick later the view repaints.
        cx.executor()
            .advance_clock(Duration::from_secs_f32(1. / MAX_FPS) + Duration::from_millis(1));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
        })
        .unwrap();
        assert!(renders.get() > first, "the spinner keeps turning");
    }

    #[gpui_kit::test]
    fn reduced_motion_keeps_the_spinner_still(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let renders = Rc::new(Cell::new(0));
        let _window = cx.add_window({
            let renders = renders.clone();
            |window, cx| Root::new(cx.new(|_| Host(renders)), window, cx)
        });
        cx.run_until_parked();
        let first = renders.get();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(renders.get(), first, "a still spinner schedules no repaint");
    }
}
