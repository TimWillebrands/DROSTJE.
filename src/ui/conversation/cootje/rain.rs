use super::{
    field::{COLUMNS, ROWS},
    glyphs::{Cell, Tone},
    reel::Frame,
};

const GLYPHS: &[u8] = b"01DRSTJE<>[]{}=+*:;#$%&/\\|";
pub(super) const TRAIL: u64 = 5;
pub(super) const DROP_LANES: [u64; 2] = [41, 67];
const ROWS_PER_TICK: u64 = 2;

fn mix(value: u64) -> u64 {
    let value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn noise(a: u64, b: u64, c: u64) -> u64 {
    mix(mix(mix(a.wrapping_add(0x9e37_79b9_7f4a_7c15)) ^ b) ^ c)
}

fn glyph(column: usize, row: usize, tick: u64) -> u8 {
    GLYPHS[(noise(column as u64, row as u64, tick / 2) % GLYPHS.len() as u64) as usize]
}

pub(super) fn fall(frame: &Frame, tick: u64) -> Frame {
    let mut wet = *frame;
    for period in DROP_LANES {
        let column = (noise(tick / period, period, 0) % COLUMNS as u64) as usize;
        let head = (tick % period) * ROWS_PER_TICK;
        for depth in 0..=TRAIL {
            let Some(row) = head.checked_sub(depth).map(|row| row as usize) else {
                break;
            };
            if row >= ROWS {
                continue;
            }
            wet[row][column] = match (depth, frame[row][column]) {
                (0, _) => Cell {
                    glyph: glyph(column, row, tick),
                    tone: Tone::Head,
                },
                (
                    _,
                    Cell {
                        tone: Tone::Blank, ..
                    },
                ) => Cell {
                    glyph: glyph(column, row, tick),
                    tone: Tone::Trail(depth as u8),
                },
                (_, drawn) => Cell {
                    glyph: drawn.glyph,
                    tone: Tone::Trail(depth as u8),
                },
            };
        }
    }
    wet
}

#[cfg(test)]
mod tests {
    use super::{DROP_LANES, TRAIL, fall};
    use crate::ui::conversation::cootje::{
        glyphs::{Cell, Tone},
        reel::{BLANK_FRAME, Frame},
    };

    const INK: Cell = Cell {
        glyph: b'S',
        tone: Tone::Body(1),
    };

    fn wet(frame: &Frame) -> usize {
        frame
            .iter()
            .flatten()
            .filter(|cell| matches!(cell.tone, Tone::Head | Tone::Trail(_)))
            .count()
    }

    #[test]
    fn rain_stays_sparse_and_leaves_dry_spells() {
        let widest = DROP_LANES.len() * (1 + TRAIL as usize);
        let mut dry = 0;
        for tick in 0..2000 {
            let frame = fall(&BLANK_FRAME, tick);
            assert!(wet(&frame) <= widest, "tick {tick}");
            if wet(&frame) == 0 {
                assert_eq!(frame, BLANK_FRAME);
                dry += 1;
            }
            assert_eq!(frame, fall(&BLANK_FRAME, tick), "tick {tick}");
        }
        assert!(dry > 0 && dry < 2000);
    }

    #[test]
    fn a_passing_drop_lights_the_drawing_without_redrawing_it() {
        let drawing: Frame = [[INK; _]; _];
        let mut lit = 0;
        for tick in 0..2000 {
            for cell in fall(&drawing, tick).iter().flatten() {
                match cell.tone {
                    Tone::Trail(depth) => {
                        assert_eq!(cell.glyph, INK.glyph);
                        assert!((1..=TRAIL as u8).contains(&depth));
                        lit += 1;
                    }
                    Tone::Head => assert!(cell.glyph.is_ascii_graphic()),
                    _ => assert_eq!(*cell, INK),
                }
            }
        }
        assert!(lit > 0);
    }
}
