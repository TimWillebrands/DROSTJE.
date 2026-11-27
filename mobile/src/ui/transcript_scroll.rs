use std::{cell::Cell, rc::Rc};

use gpui_kit::*;

#[derive(Clone, Copy, Default)]
enum TouchScroll {
    #[default]
    Idle,
    Dragging(Pixels),
    Coasting(Pixels),
}

#[derive(Default)]
pub(super) struct TranscriptScroll {
    pub(super) handle: ScrollHandle,
    touch: Rc<Cell<TouchScroll>>,
}

impl TranscriptScroll {
    pub(super) fn reset_gesture(&self) {
        self.touch.set(TouchScroll::Idle);
    }

    pub(super) fn limit_coasting(&self) -> impl IntoElement + use<> {
        let handle = self.handle.clone();
        let touch = self.touch.clone();
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture
                        || event.source != ScrollSource::Touch
                        || !hitbox.should_handle_scroll(window)
                    {
                        return;
                    }
                    let ScrollDelta::Pixels(delta) = event.delta else {
                        return;
                    };
                    if let Some(dy) = limit_delta(
                        &touch,
                        event.touch_phase,
                        delta.y,
                        handle.bounds().size.height,
                    ) {
                        cx.stop_propagation();
                        let mut offset = handle.offset();
                        let y = (offset.y + dy).clamp(-handle.max_offset().y, Pixels::ZERO);
                        if y != offset.y {
                            offset.y = y;
                            handle.set_offset(offset);
                            window.refresh();
                        }
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }
}

fn limit_delta(
    touch: &Cell<TouchScroll>,
    phase: TouchPhase,
    dy: Pixels,
    height: Pixels,
) -> Option<Pixels> {
    if phase == TouchPhase::Started {
        touch.set(TouchScroll::Dragging(dy));
    } else if phase == TouchPhase::Cancelled {
        touch.set(TouchScroll::Idle);
    } else {
        match touch.get() {
            TouchScroll::Idle => {}
            TouchScroll::Dragging(drag) if phase == TouchPhase::Ended => {
                touch.set(TouchScroll::Coasting((drag + dy).abs().min(height / 2.)));
            }
            TouchScroll::Dragging(drag) => touch.set(TouchScroll::Dragging(drag + dy)),
            TouchScroll::Coasting(remaining) if !dy.is_zero() => {
                let limited = dy.clamp(-remaining, remaining);
                touch.set(TouchScroll::Coasting(remaining - limited.abs()));
                if limited != dy {
                    return Some(limited);
                }
            }
            TouchScroll::Coasting(_) => {}
        }
    }
    None
}
