//! Lifetime of the composed terminal readback in GPUI's sprite atlas.

use gpui_kit::{Bounds, Pixels, RenderImage, Window};
use std::{cell::Cell, rc::Rc, sync::Arc};

/// The frame GPUI composes for a terminal, plus the frames it replaced.
///
/// `Window::paint_image` uploads every new image into the window's sprite atlas,
/// and only `Window::drop_image` frees that texture. A replaced frame can still be
/// referenced by the scene being built (when it was painted earlier in the same
/// frame) and by the presented scene, which GPUI re-presents without drawing. So
/// a replaced frame leaves the atlas only during a paint in a later frame, when
/// neither scene can contain it, or through [`FrameSlot::release`], which also
/// forces the next frame to be redrawn instead of re-presented.
pub(crate) struct FrameSlot<I = RenderImage> {
    current: Option<Arc<I>>,
    /// Replaced frames, each tagged with the epoch it was replaced in.
    retired: Vec<(u64, Arc<I>)>,
    /// Advanced by a next-frame callback, so a paint that observes a newer value
    /// than a frame's tag runs in a later frame than its replacement.
    epoch: Rc<Cell<u64>>,
    /// The epoch for which an advance is already scheduled.
    advance_scheduled: Option<u64>,
}

impl<I> Default for FrameSlot<I> {
    fn default() -> Self {
        Self {
            current: None,
            retired: Vec::new(),
            epoch: Rc::default(),
            advance_scheduled: None,
        }
    }
}

impl<I> FrameSlot<I> {
    #[cfg(test)]
    fn current(&self) -> Option<&Arc<I>> {
        self.current.as_ref()
    }

    pub(crate) fn replace(&mut self, frame: Arc<I>) {
        if self
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &frame))
        {
            return;
        }
        if let Some(previous) = self.current.replace(frame) {
            self.retire(previous);
        }
    }

    /// Stops composing a frame. Needs no window: release happens on a later paint
    /// or through [`FrameSlot::release`].
    pub(crate) fn clear(&mut self) {
        if let Some(previous) = self.current.take() {
            self.retire(previous);
        }
    }

    fn retire(&mut self, frame: Arc<I>) {
        self.retired.push((self.epoch.get(), frame));
    }

    /// Hands every frame replaced in an earlier epoch to `release`.
    fn release_stale(&mut self, mut release: impl FnMut(Arc<I>)) {
        let epoch = self.epoch.get();
        for (_, frame) in self.retired.extract_if(.., |(retired, _)| *retired < epoch) {
            release(frame);
        }
    }

    /// Returns the callback that ends the current epoch, once per epoch, while
    /// replaced frames wait for it. Run it when the next frame starts.
    fn schedule_advance(&mut self) -> Option<impl FnOnce() + 'static> {
        let epoch = self.epoch.get();
        if self.retired.is_empty() || self.advance_scheduled == Some(epoch) {
            return None;
        }
        self.advance_scheduled = Some(epoch);
        let shared = Rc::clone(&self.epoch);
        Some(move || shared.set(epoch + 1))
    }

    /// Empties the slot, yielding the current frame and every replaced frame.
    fn take_all(&mut self) -> impl Iterator<Item = Arc<I>> {
        self.current
            .take()
            .into_iter()
            .chain(self.retired.drain(..).map(|(_, frame)| frame))
    }
}

impl FrameSlot {
    /// Call from paint only. Releases frames no scene can reference any more,
    /// then composes the current frame.
    pub(crate) fn paint(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> Result<(), String> {
        self.release_unreferenced(window);
        let Some(frame) = self.current.clone() else {
            return Ok(());
        };
        window
            .paint_image(bounds, bounds, Default::default(), frame, 0, false)
            .map_err(|error| error.to_string())
    }

    /// Call from paint only, including paints that compose nothing, so a hidden
    /// terminal does not keep replaced frames in the atlas.
    pub(crate) fn release_unreferenced(&mut self, window: &mut Window) {
        self.release_stale(|frame| {
            let _ = window.drop_image(frame);
        });
        if let Some(advance) = self.schedule_advance() {
            window.on_next_frame(move |_, _| advance());
        }
    }

    /// Final teardown outside paint: frees every frame from `window`'s atlas and
    /// redraws it, so GPUI never re-presents a scene that refers to them.
    pub(crate) fn release(&mut self, window: &mut Window) {
        for frame in self.take_all() {
            let _ = window.drop_image(frame);
        }
        window.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::FrameSlot;
    use std::sync::Arc;

    fn stale(slot: &mut FrameSlot<u8>) -> Vec<u8> {
        let mut frames = Vec::new();
        slot.release_stale(|frame| frames.push(*frame));
        frames
    }

    #[test]
    fn replaced_frames_wait_for_a_later_frame_before_release() {
        let mut slot = FrameSlot::default();
        slot.replace(Arc::new(1));
        assert!(slot.schedule_advance().is_none(), "nothing waits yet");
        slot.replace(Arc::new(2));
        slot.replace(Arc::new(3));
        assert!(
            stale(&mut slot).is_empty(),
            "frame 1 and 2 may be in this scene"
        );
        let advance = slot.schedule_advance().expect("replaced frames wait");
        assert!(slot.schedule_advance().is_none(), "one advance per epoch");

        advance();
        slot.replace(Arc::new(4));
        assert_eq!(stale(&mut slot), [1, 2]);
        assert!(stale(&mut slot).is_empty(), "released frames are gone");
        assert_eq!(slot.current().map(|frame| **frame), Some(4));

        slot.schedule_advance().expect("frame 3 waits")();
        assert_eq!(stale(&mut slot), [3]);
        assert!(slot.schedule_advance().is_none());
    }

    #[test]
    fn replacing_with_the_composed_frame_retires_nothing() {
        let mut slot = FrameSlot::default();
        let frame = Arc::new(1);
        slot.replace(Arc::clone(&frame));
        slot.replace(Arc::clone(&frame));
        assert!(slot.schedule_advance().is_none());
        assert!(Arc::ptr_eq(slot.current().unwrap(), &frame));
    }

    #[test]
    fn clearing_retires_the_frame_and_teardown_takes_everything() {
        let mut slot = FrameSlot::default();
        slot.replace(Arc::new(1));
        slot.clear();
        assert!(slot.current().is_none());
        assert!(stale(&mut slot).is_empty(), "frame 1 may be presented");
        slot.schedule_advance().unwrap()();
        assert_eq!(stale(&mut slot), [1]);

        slot.replace(Arc::new(2));
        slot.replace(Arc::new(3));
        let mut all = slot.take_all().map(|frame| *frame).collect::<Vec<_>>();
        all.sort();
        assert_eq!(all, [2, 3]);
        assert!(slot.current().is_none());
        assert!(slot.schedule_advance().is_none());
    }
}

/// Proves atlas residency against GPUI's real sprite atlas in a headless window.
#[cfg(all(test, feature = "runtime-qa"))]
mod atlas_tests {
    use super::FrameSlot;
    use gpui_kit::{
        Context, IntoElement, ParentElement as _, Render, RenderImage, Styled as _, TestAppContext,
        VisualTestContext, Window, canvas, div,
    };
    use std::sync::Arc;

    /// Paints its slot like `Terminal::paint`; `repaint_with` replaces the frame
    /// after the first paint and paints again within the same frame.
    #[derive(Default)]
    struct FrameView {
        slot: FrameSlot,
        repaint_with: Option<Arc<RenderImage>>,
    }

    impl Render for FrameView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = cx.entity().downgrade();
            div().size_full().child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        view.update(cx, |view, _| {
                            view.slot.paint(bounds, window).unwrap();
                            if let Some(frame) = view.repaint_with.take() {
                                view.slot.replace(frame);
                                view.slot.paint(bounds, window).unwrap();
                            }
                        })
                        .unwrap();
                    },
                )
                .size_full(),
            )
        }
    }

    fn frame(value: u8) -> Arc<RenderImage> {
        let pixels = image::RgbaImage::from_pixel(4, 4, image::Rgba([value; 4]));
        Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
            pixels
        )]))
    }

    fn next_frame(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    fn in_atlas(cx: &mut VisualTestContext, frame: &RenderImage) -> bool {
        cx.update(|window, _| window.has_image_atlas_entry(frame))
    }

    #[gpui_kit::test]
    fn replaced_frames_leave_the_atlas_once_no_scene_can_present_them(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| FrameView::default());
        let (first, second, third) = (frame(1), frame(2), frame(3));

        view.update(cx, |view, _| view.slot.replace(Arc::clone(&first)));
        next_frame(cx);
        assert!(in_atlas(cx, &first));

        // One frame paints `first`, then replaces it and paints `second`.
        view.update(cx, |view, _| view.repaint_with = Some(Arc::clone(&second)));
        next_frame(cx);
        assert!(in_atlas(cx, &first), "this frame's scene still paints it");
        assert!(in_atlas(cx, &second));

        next_frame(cx);
        assert!(
            !in_atlas(cx, &first),
            "a later paint frees the replaced frame"
        );
        assert!(in_atlas(cx, &second));

        // Replacing between frames frees the old frame one frame later.
        view.update(cx, |view, _| view.slot.replace(Arc::clone(&third)));
        next_frame(cx);
        next_frame(cx);
        assert!(!in_atlas(cx, &second));
        assert!(in_atlas(cx, &third));

        cx.update(|window, cx| view.update(cx, |view, _| view.slot.release(window)));
        assert!(!in_atlas(cx, &third), "teardown frees the composed frame");
        next_frame(cx);
        assert!(!in_atlas(cx, &third), "nothing paints it again");
    }
}
