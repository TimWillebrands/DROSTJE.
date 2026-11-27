use super::{
    field::{COLUMNS, FULL, Field, ROWS},
    glyphs::{BLANK, Cell, Polarity, cell},
};
use image::ImageError;
use std::array;

pub(super) type Frame = [[Cell; COLUMNS]; ROWS];
pub(super) const BLANK_FRAME: Frame = [[BLANK; COLUMNS]; ROWS];
pub(super) const STEPS: usize = 18;
pub(super) const HOLD_TICKS: u64 = 240;
pub(super) const CYCLE_TICKS: u64 = Pose::ALL.len() as u64 * (HOLD_TICKS + STEPS as u64);
const SWEEP: u32 = FULL / 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum Pose {
    Wave,
    Scribe,
    Cheer,
    Hacker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Shot {
    Appear { pose: Pose, step: usize },
    Hold(Pose),
    Morph { from: Pose, step: usize },
}

pub(super) struct Reel {
    holds: Vec<Frame>,
    appears: Vec<Frame>,
    morphs: Vec<Frame>,
}

impl Pose {
    pub(super) const ALL: [Self; 4] = [Self::Wave, Self::Scribe, Self::Cheer, Self::Hacker];

    fn bundled(self) -> &'static [u8] {
        match self {
            Self::Wave => include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/cootje/wave.png"
            )),
            Self::Scribe => include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/cootje/scribe.png"
            )),
            Self::Cheer => include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/cootje/cheer.png"
            )),
            Self::Hacker => include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/cootje/hacker.png"
            )),
        }
    }

    pub(super) fn after(self, turns: u64) -> Self {
        Self::ALL[(self as usize + (turns % Self::ALL.len() as u64) as usize) % Self::ALL.len()]
    }
}

pub(super) fn appearing(pose: Pose, progress: f32) -> Shot {
    Shot::Appear {
        pose,
        step: ((progress.clamp(0., 1.) * STEPS as f32) as usize).min(STEPS - 1),
    }
}

pub(super) fn cycling(start: Pose, tick: u64) -> Shot {
    let scene = HOLD_TICKS + STEPS as u64;
    let pose = start.after(tick / scene);
    match (tick % scene).checked_sub(HOLD_TICKS) {
        None => Shot::Hold(pose),
        Some(step) => Shot::Morph {
            from: pose,
            step: step as usize,
        },
    }
}

pub(super) fn sweep(step: usize, row: usize) -> u32 {
    let progress = (step as u32 + 1) * FULL / (STEPS as u32 + 1);
    (progress * (FULL + SWEEP) / FULL)
        .saturating_sub(row as u32 * SWEEP / (ROWS as u32 - 1))
        .min(FULL)
}

fn draw(field: &Field, polarity: Polarity) -> Frame {
    array::from_fn(|row| array::from_fn(|column| cell(field.parts(column, row), polarity)))
}

fn passage(from: &Field, to: &Field, polarity: Polarity) -> impl Iterator<Item = Frame> {
    (0..STEPS).map(move |step| draw(&Field::blend(from, to, |row| sweep(step, row)), polarity))
}

impl Reel {
    pub(super) fn bundled(polarity: Polarity) -> Result<Self, ImageError> {
        let fields = Pose::ALL
            .iter()
            .map(|pose| {
                image::load_from_memory(pose.bundled())
                    .map(|image| Field::sample(&image.to_rgba8()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::from_fields(&fields, polarity))
    }

    pub(super) fn from_fields(fields: &[Field], polarity: Polarity) -> Self {
        let nothing = Field::empty();
        Self {
            holds: fields.iter().map(|field| draw(field, polarity)).collect(),
            appears: fields
                .iter()
                .flat_map(|field| passage(&nothing, field, polarity))
                .collect(),
            morphs: fields
                .iter()
                .zip(fields.iter().cycle().skip(1))
                .flat_map(|(from, to)| passage(from, to, polarity))
                .collect(),
        }
    }

    pub(super) fn frame(&self, shot: Shot) -> &Frame {
        match shot {
            Shot::Hold(pose) => &self.holds[pose as usize],
            Shot::Appear { pose, step } => {
                &self.appears[pose as usize * STEPS + step.min(STEPS - 1)]
            }
            Shot::Morph { from, step } => &self.morphs[from as usize * STEPS + step.min(STEPS - 1)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CYCLE_TICKS, Frame, HOLD_TICKS, Pose, Reel, STEPS, Shot, appearing, cycling, sweep,
    };
    use crate::ui::conversation::cootje::{
        field::{FULL, ROWS},
        glyphs::{Polarity, Tone},
    };
    use std::sync::LazyLock;

    static REELS: LazyLock<[Reel; 2]> = LazyLock::new(|| {
        [Polarity::BrightOnDark, Polarity::InkOnPaper]
            .map(|polarity| Reel::bundled(polarity).expect("the bundled poses decode"))
    });

    fn drawn(frame: &Frame) -> usize {
        frame
            .iter()
            .flatten()
            .filter(|cell| cell.tone != Tone::Blank)
            .count()
    }

    #[test]
    fn the_bundled_poses_become_four_distinct_two_tone_drawings() {
        for reel in REELS.iter() {
            let holds = Pose::ALL.map(|pose| reel.frame(Shot::Hold(pose)));
            for (ix, hold) in holds.iter().enumerate() {
                let cells = hold.iter().flatten();
                assert!(drawn(hold) > hold.len() * hold[0].len() / 4, "{ix}");
                assert!(drawn(hold) < hold.len() * hold[0].len(), "{ix}");
                assert!(cells.clone().any(|cell| matches!(cell.tone, Tone::Pale(_))));
                assert!(cells.clone().any(|cell| matches!(cell.tone, Tone::Body(_))));
                assert!(
                    cells
                        .clone()
                        .all(|cell| cell.glyph == b' ' || cell.glyph.is_ascii_graphic())
                );
                assert!(holds[..ix].iter().all(|other| other != hold), "{ix}");
            }
        }
    }

    #[test]
    fn the_sweep_runs_down_the_rows_and_never_backwards() {
        for row in 0..ROWS {
            for step in 1..STEPS {
                assert!(sweep(step, row) >= sweep(step - 1, row));
                assert!(sweep(step, row) <= FULL);
            }
            if row > 0 {
                assert!((0..STEPS).all(|step| sweep(step, row) <= sweep(step, row - 1)));
            }
        }
        assert!(sweep(0, 0) > 0 && sweep(0, ROWS - 1) == 0);
        assert_eq!(sweep(STEPS - 1, 0), FULL);
        assert!(sweep(STEPS - 1, ROWS - 1) > FULL * 4 / 5);
    }

    #[test]
    fn a_morph_leaves_one_pose_row_by_row_and_arrives_at_the_next() {
        for reel in REELS.iter() {
            for from in Pose::ALL {
                let (before, after) = (
                    reel.frame(Shot::Hold(from)),
                    reel.frame(Shot::Hold(from.after(1))),
                );
                for step in 0..STEPS {
                    let frame = reel.frame(Shot::Morph { from, step });
                    for row in 0..ROWS {
                        match sweep(step, row) {
                            0 => assert_eq!(frame[row], before[row], "{from:?} {step} {row}"),
                            FULL => assert_eq!(frame[row], after[row], "{from:?} {step} {row}"),
                            _ => {}
                        }
                    }
                }
                let last = reel.frame(Shot::Morph {
                    from,
                    step: STEPS - 1,
                });
                assert!(last != before);
            }
        }
    }

    #[test]
    fn a_pose_appears_out_of_nothing() {
        for reel in REELS.iter() {
            for pose in Pose::ALL {
                let step = |step| drawn(reel.frame(Shot::Appear { pose, step }));
                assert!(step(0) < step(STEPS / 2) && step(STEPS / 2) <= step(STEPS - 1));
                assert!(step(0) < drawn(reel.frame(Shot::Hold(pose))) / 4);
            }
        }
        assert_eq!(
            appearing(Pose::Cheer, 0.),
            Shot::Appear {
                pose: Pose::Cheer,
                step: 0
            }
        );
        assert_eq!(
            appearing(Pose::Cheer, 1.),
            Shot::Appear {
                pose: Pose::Cheer,
                step: STEPS - 1
            }
        );
    }

    #[test]
    fn the_cycle_holds_each_pose_morphs_on_and_returns_to_the_entrance() {
        let scene = HOLD_TICKS + STEPS as u64;
        for entrance in Pose::ALL {
            assert_eq!(cycling(entrance, 0), Shot::Hold(entrance));
            assert_eq!(cycling(entrance, HOLD_TICKS - 1), Shot::Hold(entrance));
            assert_eq!(
                cycling(entrance, HOLD_TICKS),
                Shot::Morph {
                    from: entrance,
                    step: 0
                }
            );
            assert_eq!(
                cycling(entrance, scene - 1),
                Shot::Morph {
                    from: entrance,
                    step: STEPS - 1
                }
            );
            let visited: Vec<Shot> = (0..Pose::ALL.len() as u64)
                .map(|turn| cycling(entrance, turn * scene))
                .collect();
            for pose in Pose::ALL {
                assert!(visited.contains(&Shot::Hold(pose)), "{entrance:?} {pose:?}");
            }
            assert_eq!(
                cycling(entrance, CYCLE_TICKS - 1),
                Shot::Morph {
                    from: entrance.after(Pose::ALL.len() as u64 - 1),
                    step: STEPS - 1
                }
            );
            assert_eq!(entrance.after(Pose::ALL.len() as u64), entrance);
        }
    }
}
