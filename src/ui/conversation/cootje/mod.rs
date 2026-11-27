mod field;
mod glyphs;
mod rain;
mod reel;

pub(in crate::ui) use reel::Pose;

use crate::ui::theme;
use field::COLUMNS;
use glyphs::{Cell, LEVELS, Polarity, Tone};
use gpui_kit::{component::ActiveTheme, *};
use reel::{BLANK_FRAME, CYCLE_TICKS, Frame, Reel, STEPS, Shot};
use std::{ops::Range, sync::Arc, time::Duration};

const FPS: f32 = 12.;
const SHADES: [f32; LEVELS as usize] = [0.45, 0.7, 1.];

#[derive(Default)]
enum Stage {
    #[default]
    Unbuilt,
    Building,
    Ready(Arc<Reel>),
    Unavailable,
}

#[derive(Default)]
struct Reels([Stage; 2]);

impl Global for Reels {}

fn reel(polarity: Polarity, cx: &mut App) -> Option<Arc<Reel>> {
    let slot = polarity as usize;
    match &cx.default_global::<Reels>().0[slot] {
        Stage::Ready(reel) => return Some(reel.clone()),
        Stage::Building | Stage::Unavailable => return None,
        Stage::Unbuilt => {}
    }
    cx.global_mut::<Reels>().0[slot] = Stage::Building;
    cx.spawn(async move |cx| {
        let built = cx
            .background_spawn(async move { Reel::bundled(polarity) })
            .await;
        cx.update(|cx| {
            cx.global_mut::<Reels>().0[slot] = match built {
                Ok(reel) => Stage::Ready(Arc::new(reel)),
                Err(error) => {
                    eprintln!("Could not draw the COOTJE mascot: {error:#}");
                    Stage::Unavailable
                }
            };
            cx.refresh_windows();
        });
    })
    .detach();
    None
}

fn ticks(count: u64) -> Duration {
    Duration::from_secs_f32(count as f32 / FPS)
}

fn tick_at(progress: f32) -> u64 {
    ((progress.clamp(0., 1.) * CYCLE_TICKS as f32) as u64).min(CYCLE_TICKS - 1)
}

fn runs(cells: &[Cell; COLUMNS]) -> Vec<(Range<usize>, Tone)> {
    let mut start = 0;
    cells
        .chunk_by(|left, right| left.tone == right.tone)
        .map(|run| {
            let range = start..start + run.len();
            start = range.end;
            (range, run[0].tone)
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Palette {
    body: Hsla,
    pale: Hsla,
}

impl Palette {
    fn tint(self, tone: Tone) -> Option<Hsla> {
        let shade = |level: u8| SHADES[usize::from(level).min(SHADES.len() - 1)];
        match tone {
            Tone::Blank => None,
            Tone::Body(level) => Some(self.body.opacity(shade(level))),
            Tone::Pale(level) => Some(self.pale.opacity(shade(level))),
            Tone::Head => Some(self.pale),
            Tone::Trail(depth) => Some(self.body.opacity(1. - 0.1 * f32::from(depth))),
        }
    }
}

fn lines(frame: &Frame, palette: Palette) -> Vec<StyledText> {
    frame
        .iter()
        .map(|cells| {
            let text: String = cells.iter().map(|cell| char::from(cell.glyph)).collect();
            StyledText::new(text).with_highlights(runs(cells).into_iter().filter_map(
                |(range, tone)| {
                    palette.tint(tone).map(|color| {
                        (
                            range,
                            HighlightStyle {
                                color: Some(color),
                                ..Default::default()
                            },
                        )
                    })
                },
            ))
        })
        .collect()
}

#[derive(IntoElement)]
pub(in crate::ui) struct Cootje {
    entrance: Pose,
}

impl Cootje {
    pub(in crate::ui) fn new(entrance: Pose) -> Self {
        Self { entrance }
    }
}

impl RenderOnce for Cootje {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let polarity = if cx.theme().is_dark() {
            Polarity::BrightOnDark
        } else {
            Polarity::InkOnPaper
        };
        let palette = Palette {
            body: theme::signature(cx),
            pale: cx.theme().foreground,
        };
        let entrance = self.entrance;
        let canvas = div()
            .debug_selector(|| "cootje".into())
            .flex_shrink_0()
            .font_family(cx.theme().mono_font_family.clone())
            .font_features(FontFeatures::disable_ligatures())
            .text_size(rems(0.4375))
            .line_height(relative(1.1))
            .text_left()
            .whitespace_nowrap();
        let Some(reel) = reel(polarity, cx) else {
            return canvas
                .children(lines(&BLANK_FRAME, palette))
                .into_any_element();
        };
        if cx.reduce_motion() || !window.is_window_active() {
            return canvas
                .children(lines(reel.frame(Shot::Hold(entrance)), palette))
                .into_any_element();
        }
        canvas
            .with_animations(
                "cootje-reel",
                vec![
                    Animation::new(ticks(STEPS as u64)).with_max_fps(FPS),
                    Animation::new(ticks(CYCLE_TICKS))
                        .repeat()
                        .with_max_fps(FPS),
                ],
                move |canvas, act, progress| {
                    if act == 0 {
                        let shot = reel::appearing(entrance, progress);
                        return canvas.children(lines(reel.frame(shot), palette));
                    }
                    let tick = tick_at(progress);
                    let frame = rain::fall(reel.frame(reel::cycling(entrance, tick)), tick);
                    canvas.children(lines(&frame, palette))
                },
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CYCLE_TICKS,
        field::COLUMNS,
        glyphs::{BLANK, Cell, Tone},
        runs, tick_at,
    };

    #[test]
    fn highlight_runs_tile_a_line_in_order() {
        let mut cells = [BLANK; COLUMNS];
        for (column, cell) in cells.iter_mut().enumerate().skip(3).take(9) {
            *cell = Cell {
                glyph: b'$',
                tone: if column < 7 {
                    Tone::Body(2)
                } else {
                    Tone::Pale(1)
                },
            };
        }
        assert_eq!(
            runs(&cells),
            [
                (0..3, Tone::Blank),
                (3..7, Tone::Body(2)),
                (7..12, Tone::Pale(1)),
                (12..COLUMNS, Tone::Blank),
            ]
        );
    }

    #[test]
    fn progress_maps_onto_the_whole_cycle() {
        assert_eq!(tick_at(0.), 0);
        assert_eq!(tick_at(0.5), CYCLE_TICKS / 2);
        assert_eq!(tick_at(1.), CYCLE_TICKS - 1);
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod window_tests {
    use super::{Cootje, FPS, Pose, Reels, Stage};
    use gpui_kit::{
        AppContext, Context, IntoElement, ParentElement, Render, TestAppContext, VisualTestContext,
        Window, WindowHandle, component::Root, div, px,
    };
    use std::{cell::Cell, rc::Rc, time::Duration};

    struct Host(Rc<Cell<usize>>);
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.0.set(self.0.get() + 1);
            div().child(Cootje::new(Pose::Wave))
        }
    }

    fn open(cx: &mut TestAppContext, reduce_motion: bool) -> (WindowHandle<Root>, Rc<Cell<usize>>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(reduce_motion);
        });
        let renders = Rc::new(Cell::new(0));
        let window = cx.add_window({
            let renders = renders.clone();
            |window, cx| Root::new(cx.new(|_| Host(renders)), window, cx)
        });
        cx.update_window(window.into(), |_, window, _| window.activate_window())
            .unwrap();
        (window, renders)
    }

    fn draw(cx: &mut TestAppContext, window: WindowHandle<Root>) {
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
    }

    fn built(cx: &mut TestAppContext) -> usize {
        cx.update(|cx| {
            cx.default_global::<Reels>()
                .0
                .iter()
                .filter(|stage| matches!(stage, Stage::Ready(_)))
                .count()
        })
    }

    fn a_frame_later(cx: &mut TestAppContext, window: WindowHandle<Root>) {
        cx.executor()
            .advance_clock(Duration::from_secs_f32(1. / FPS) + Duration::from_millis(1));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn the_mascot_keeps_its_place_while_the_poses_are_drawn_in_the_background(
        cx: &mut TestAppContext,
    ) {
        let (window, _) = open(cx, true);
        draw(cx, window);
        assert_eq!(built(cx), 0, "nothing is decoded on the GPUI thread");
        let waiting = VisualTestContext::from_window(window.into(), cx)
            .debug_bounds("cootje")
            .expect("the mascot reserves its place");
        assert!(waiting.size.width > px(0.) && waiting.size.height > px(0.));

        cx.run_until_parked();
        draw(cx, window);
        assert_eq!(built(cx), 1, "only the polarity in use is built");
        let drawn = VisualTestContext::from_window(window.into(), cx)
            .debug_bounds("cootje")
            .expect("the mascot renders");
        assert_eq!(drawn.size, waiting.size);
    }

    #[gpui_kit::test]
    fn the_reel_repaints_on_a_capped_timer_instead_of_every_frame(cx: &mut TestAppContext) {
        let (window, renders) = open(cx, false);
        cx.run_until_parked();
        draw(cx, window);
        cx.run_until_parked();
        let first = renders.get();

        let callbacks = cx
            .update_window(window.into(), |_, window, cx| {
                window.simulate_next_frame(cx)
            })
            .unwrap();
        assert_eq!(callbacks, 0, "the reel must not request every frame");
        cx.run_until_parked();
        assert_eq!(renders.get(), first);

        a_frame_later(cx, window);
        assert!(renders.get() > first, "the reel keeps playing");
    }

    #[gpui_kit::test]
    fn reduced_motion_holds_the_entrance_pose_still(cx: &mut TestAppContext) {
        let (window, renders) = open(cx, true);
        cx.run_until_parked();
        draw(cx, window);
        cx.run_until_parked();
        let first = renders.get();
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        assert_eq!(renders.get(), first, "a held pose schedules no repaint");
    }

    #[gpui_kit::test]
    fn a_window_in_the_background_stops_the_reel(cx: &mut TestAppContext) {
        let (window, renders) = open(cx, false);
        cx.run_until_parked();
        draw(cx, window);
        VisualTestContext::from_window(window.into(), cx).deactivate_window();
        cx.run_until_parked();
        a_frame_later(cx, window);
        a_frame_later(cx, window);
        let settled = renders.get();
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        assert_eq!(
            renders.get(),
            settled,
            "no repaint is scheduled while inactive"
        );
    }
}
