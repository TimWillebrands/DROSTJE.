use super::field::{PARTS, Sample};

const GAIN: i32 = 215;
const BLANK_BELOW: i32 = 14;
pub(super) const LEVELS: u8 = 3;

const LILEX_SHAPES: [(u8, [u8; PARTS]); 95] = [
    (b' ', [0, 0, 0, 0, 0, 0]),
    (b'!', [39, 39, 50, 49, 45, 46]),
    (b'"', [84, 84, 27, 27, 0, 0]),
    (b'#', [64, 65, 236, 236, 64, 63]),
    (b'$', [127, 143, 151, 200, 126, 157]),
    (b'%', [133, 70, 186, 187, 69, 131]),
    (b'&', [99, 54, 217, 145, 126, 112]),
    (b'\'', [42, 42, 13, 13, 0, 0]),
    (b'(', [16, 74, 135, 2, 41, 76]),
    (b')', [75, 16, 2, 135, 77, 41]),
    (b'*', [4, 4, 175, 175, 42, 42]),
    (b'+', [4, 4, 145, 145, 21, 21]),
    (b',', [0, 0, 0, 0, 96, 47]),
    (b'-', [0, 0, 52, 52, 0, 0]),
    (b'.', [0, 0, 0, 0, 45, 46]),
    (b'/', [0, 87, 55, 83, 115, 0]),
    (b'0', [100, 101, 194, 194, 99, 99]),
    (b'1', [99, 61, 46, 133, 90, 131]),
    (b'2', [104, 102, 46, 143, 137, 91]),
    (b'3', [90, 103, 41, 186, 102, 100]),
    (b'4', [20, 115, 190, 193, 39, 96]),
    (b'5', [106, 77, 123, 153, 93, 103]),
    (b'6', [59, 31, 208, 151, 96, 102]),
    (b'7', [127, 131, 32, 150, 71, 7]),
    (b'8', [109, 109, 188, 188, 111, 111]),
    (b'9', [103, 98, 150, 208, 32, 57]),
    (b':', [0, 0, 45, 46, 45, 46]),
    (b';', [0, 0, 45, 46, 96, 47]),
    (b'<', [0, 19, 164, 89, 3, 79]),
    (b'=', [0, 0, 158, 158, 13, 13]),
    (b'>', [19, 0, 90, 164, 78, 3]),
    (b'?', [101, 103, 52, 119, 58, 33]),
    (b'@', [89, 92, 254, 186, 139, 125]),
    (b'A', [65, 66, 200, 204, 75, 77]),
    (b'B', [124, 103, 208, 194, 122, 108]),
    (b'C', [95, 104, 165, 1, 93, 104]),
    (b'D', [121, 97, 161, 166, 119, 96]),
    (b'E', [123, 85, 210, 80, 121, 85]),
    (b'F', [123, 88, 210, 77, 73, 0]),
    (b'G', [101, 99, 167, 125, 104, 118]),
    (b'H', [74, 74, 214, 214, 73, 73]),
    (b'I', [99, 99, 81, 81, 98, 98]),
    (b'J', [60, 111, 0, 161, 103, 89]),
    (b'K', [74, 89, 245, 150, 73, 85]),
    (b'L', [74, 0, 161, 0, 110, 90]),
    (b'M', [122, 118, 236, 233, 67, 67]),
    (b'N', [130, 67, 223, 224, 66, 127]),
    (b'O', [101, 101, 165, 165, 100, 100]),
    (b'P', [123, 111, 210, 142, 73, 0]),
    (b'Q', [101, 101, 165, 165, 121, 183]),
    (b'R', [123, 111, 210, 199, 73, 80]),
    (b'S', [109, 95, 126, 137, 106, 110]),
    (b'T', [127, 127, 81, 81, 37, 37]),
    (b'U', [74, 74, 161, 161, 103, 103]),
    (b'V', [78, 75, 162, 157, 66, 64]),
    (b'W', [66, 64, 255, 255, 118, 118]),
    (b'X', [82, 80, 157, 160, 79, 81]),
    (b'Y', [83, 82, 135, 134, 37, 37]),
    (b'Z', [90, 137, 96, 83, 142, 97]),
    (b'[', [86, 67, 133, 0, 112, 67]),
    (b'\\', [87, 0, 83, 55, 0, 115]),
    (b']', [67, 86, 0, 133, 67, 112]),
    (b'^', [65, 67, 95, 100, 0, 0]),
    (b'_', [0, 0, 0, 0, 93, 93]),
    (b'`', [56, 13, 0, 0, 0, 0]),
    (b'a', [0, 0, 165, 198, 112, 115]),
    (b'b', [88, 0, 191, 190, 107, 108]),
    (b'c', [0, 0, 171, 81, 90, 85]),
    (b'd', [0, 88, 190, 191, 108, 107]),
    (b'e', [0, 0, 219, 191, 94, 85]),
    (b'f', [37, 113, 149, 145, 100, 98]),
    (b'g', [0, 42, 191, 196, 216, 204]),
    (b'h', [88, 0, 191, 181, 70, 70]),
    (b'i', [14, 53, 81, 133, 73, 125]),
    (b'j', [0, 66, 74, 166, 71, 160]),
    (b'k', [88, 0, 209, 159, 69, 85]),
    (b'l', [109, 44, 77, 77, 99, 99]),
    (b'm', [0, 0, 252, 236, 96, 96]),
    (b'n', [0, 0, 185, 181, 70, 70]),
    (b'o', [0, 0, 175, 175, 94, 95]),
    (b'p', [0, 0, 185, 190, 191, 108]),
    (b'q', [0, 0, 190, 185, 108, 191]),
    (b'r', [0, 0, 187, 111, 117, 65]),
    (b's', [0, 0, 173, 162, 85, 102]),
    (b't', [69, 0, 220, 81, 60, 81]),
    (b'u', [0, 0, 148, 148, 103, 107]),
    (b'v', [0, 0, 145, 141, 61, 60]),
    (b'w', [0, 0, 227, 225, 103, 103]),
    (b'x', [0, 0, 156, 151, 77, 82]),
    (b'y', [0, 0, 148, 146, 163, 65]),
    (b'z', [0, 0, 122, 166, 125, 77]),
    (b'{', [69, 71, 79, 118, 77, 94]),
    (b'|', [39, 39, 67, 67, 50, 50]),
    (b'}', [71, 69, 118, 79, 94, 77]),
    (b'~', [0, 0, 109, 109, 0, 0]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Polarity {
    BrightOnDark,
    InkOnPaper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Blank,
    Body(u8),
    Pale(u8),
    Head,
    Trail(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Cell {
    pub(super) glyph: u8,
    pub(super) tone: Tone,
}

pub(super) const BLANK: Cell = Cell {
    glyph: b' ',
    tone: Tone::Blank,
};

impl Polarity {
    fn floor(self) -> i32 {
        match self {
            Self::BrightOnDark => 46,
            Self::InkOnPaper => 16,
        }
    }

    fn intensity(self, sample: Sample) -> i32 {
        let (covered, lit) = (i32::from(sample.covered), i32::from(sample.lit));
        let level = match self {
            Self::BrightOnDark => lit,
            Self::InkOnPaper => covered - lit,
        };
        (self.floor() * covered + (255 - self.floor()) * level.max(0)) / 255
    }
}

fn nearest_glyph(target: [i32; PARTS]) -> u8 {
    LILEX_SHAPES
        .iter()
        .min_by_key(|(_, shape)| {
            shape
                .iter()
                .zip(target)
                .map(|(&shape, target)| (i32::from(shape) - target).pow(2))
                .sum::<i32>()
        })
        .map_or(b' ', |&(glyph, _)| glyph)
}

pub(super) fn cell(parts: [Sample; PARTS], polarity: Polarity) -> Cell {
    let intensity = parts.map(|sample| polarity.intensity(sample));
    let target = intensity.map(|intensity| intensity * GAIN / 255);
    if target.iter().all(|&value| value < BLANK_BELOW) {
        return BLANK;
    }
    let glyph = nearest_glyph(target);
    if glyph == b' ' {
        return BLANK;
    }
    let level = (intensity.iter().sum::<i32>() / PARTS as i32 * i32::from(LEVELS) / 256) as u8;
    let (covered, pale) = parts.iter().fold((0, 0), |(covered, pale), sample| {
        (
            covered + u32::from(sample.covered),
            pale + u32::from(sample.pale),
        )
    });
    Cell {
        glyph,
        tone: if pale * 2 >= covered {
            Tone::Pale(level)
        } else {
            Tone::Body(level)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{BLANK, Cell, LILEX_SHAPES, Polarity, Tone, cell};
    use crate::ui::conversation::cootje::field::{ACROSS, PARTS, Sample};

    fn weight(glyph: u8) -> u32 {
        LILEX_SHAPES
            .iter()
            .find(|(candidate, _)| *candidate == glyph)
            .map(|(_, shape)| shape.iter().map(|&part| u32::from(part)).sum())
            .expect("a printable glyph")
    }

    fn flat(lit: u8) -> [Sample; PARTS] {
        [Sample {
            covered: 255,
            lit,
            pale: 0,
        }; PARTS]
    }

    #[test]
    fn every_printable_glyph_has_one_measured_shape() {
        let glyphs: Vec<u8> = LILEX_SHAPES.iter().map(|(glyph, _)| *glyph).collect();
        assert_eq!(glyphs, (b' '..=b'~').collect::<Vec<_>>());
        assert_eq!(weight(b' '), 0);
    }

    #[test]
    fn an_uncovered_cell_stays_blank() {
        for polarity in [Polarity::BrightOnDark, Polarity::InkOnPaper] {
            assert_eq!(cell([Sample::default(); PARTS], polarity), BLANK);
        }
    }

    #[test]
    fn more_light_draws_a_heavier_glyph_on_dark_and_a_lighter_one_on_paper() {
        let drawn = |lit, polarity| cell(flat(lit), polarity);
        let (dim, bright) = (
            drawn(40, Polarity::BrightOnDark),
            drawn(255, Polarity::BrightOnDark),
        );
        assert!(
            weight(dim.glyph) < weight(bright.glyph),
            "{dim:?} {bright:?}"
        );
        assert!(
            matches!((dim.tone, bright.tone), (Tone::Body(low), Tone::Body(high)) if low < high)
        );

        let (shadow, highlight) = (
            drawn(40, Polarity::InkOnPaper),
            drawn(255, Polarity::InkOnPaper),
        );
        assert!(weight(shadow.glyph) > weight(highlight.glyph));
        assert_ne!(
            drawn(0, Polarity::BrightOnDark),
            BLANK,
            "dark parts stay visible"
        );
    }

    #[test]
    fn a_glyph_follows_the_shape_inside_its_cell() {
        let half = |lit_part: fn(usize) -> bool| {
            let parts: [Sample; PARTS] = std::array::from_fn(|part| {
                if lit_part(part) {
                    flat(255)[0]
                } else {
                    Sample::default()
                }
            });
            cell(parts, Polarity::BrightOnDark).glyph
        };
        let shape = |glyph: u8| {
            LILEX_SHAPES
                .iter()
                .find(|(candidate, _)| *candidate == glyph)
                .map(|(_, shape)| shape.map(u32::from))
                .expect("a printable glyph")
        };
        let top = shape(half(|part| part < ACROSS));
        assert!(top[0] + top[1] > top[4] + top[5], "{top:?}");
        let bottom = shape(half(|part| part >= PARTS - ACROSS));
        assert!(bottom[4] + bottom[5] > bottom[0] + bottom[1], "{bottom:?}");
        let left = shape(half(|part| part % ACROSS == 0));
        assert!(
            left[0] + left[2] + left[4] > left[1] + left[3] + left[5],
            "{left:?}"
        );
    }

    #[test]
    fn mostly_white_cells_are_pale_and_the_rest_is_body() {
        let white = [Sample {
            covered: 255,
            lit: 255,
            pale: 255,
        }; PARTS];
        assert!(matches!(
            cell(white, Polarity::BrightOnDark),
            Cell {
                tone: Tone::Pale(_),
                ..
            }
        ));
        assert!(matches!(
            cell(flat(255), Polarity::BrightOnDark),
            Cell {
                tone: Tone::Body(_),
                ..
            }
        ));
    }
}
