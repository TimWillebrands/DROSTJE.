use image::RgbaImage;

pub(super) const COLUMNS: usize = 56;
pub(super) const ROWS: usize = 30;
pub(super) const ACROSS: usize = 2;
pub(super) const DOWN: usize = 3;
pub(super) const PARTS: usize = ACROSS * DOWN;
pub(super) const FULL: u32 = 256;
const WIDTH: usize = COLUMNS * ACROSS;
const HEIGHT: usize = ROWS * DOWN;
const KEY_MARGIN: i32 = 60;
const PALE_SPREAD: i32 = 48;
const PALE_ABOVE: i32 = 176;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Sample {
    pub(super) covered: u8,
    pub(super) lit: u8,
    pub(super) pale: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Field {
    samples: Box<[Sample]>,
}

fn keyed_out(pixel: [u8; 4]) -> bool {
    let [red, green, blue, alpha] = pixel.map(i32::from);
    alpha < 128 || (green > red + KEY_MARGIN && green > blue + KEY_MARGIN)
}

fn luma(pixel: [u8; 4]) -> i32 {
    let [red, green, blue, _] = pixel.map(i32::from);
    (54 * red + 183 * green + 19 * blue) >> 8
}

fn pale(pixel: [u8; 4]) -> bool {
    let [red, green, blue, _] = pixel.map(i32::from);
    let spread = red.max(green).max(blue) - red.min(green).min(blue);
    spread < PALE_SPREAD && luma(pixel) > PALE_ABOVE
}

fn brightness_range(image: &RgbaImage) -> (i32, i32) {
    let mut histogram = [0usize; 256];
    for pixel in image.pixels().filter(|pixel| !keyed_out(pixel.0)) {
        histogram[luma(pixel.0) as usize] += 1;
    }
    let total: usize = histogram.iter().sum();
    let percentile = |share: usize| {
        let mut seen = 0;
        histogram
            .iter()
            .position(|count| {
                seen += count;
                seen * 100 >= total * share
            })
            .unwrap_or(255) as i32
    };
    let low = percentile(2);
    (low, percentile(98).max(low + 1))
}

fn mix(from: u8, to: u8, weight: u32) -> u8 {
    ((u32::from(from) * (FULL - weight) + u32::from(to) * weight) / FULL) as u8
}

impl Field {
    pub(super) fn empty() -> Self {
        Self {
            samples: vec![Sample::default(); WIDTH * HEIGHT].into(),
        }
    }

    pub(super) fn sample(image: &RgbaImage) -> Self {
        let (low, high) = brightness_range(image);
        let (width, height) = (image.width() as usize, image.height() as usize);
        let mut sums = vec![[0u64; 4]; WIDTH * HEIGHT];
        for (x, y, pixel) in image.enumerate_pixels() {
            let sum =
                &mut sums[(y as usize * HEIGHT / height) * WIDTH + x as usize * WIDTH / width];
            sum[0] += 1;
            if keyed_out(pixel.0) {
                continue;
            }
            sum[1] += 1;
            sum[2] += ((luma(pixel.0) - low) * 255 / (high - low)).clamp(0, 255) as u64;
            sum[3] += u64::from(pale(pixel.0));
        }
        Self {
            samples: sums
                .into_iter()
                .map(|[pixels, covered, lit, pale]| {
                    let pixels = pixels.max(1);
                    Sample {
                        covered: (covered * 255 / pixels) as u8,
                        lit: (lit / pixels) as u8,
                        pale: (pale * 255 / pixels) as u8,
                    }
                })
                .collect(),
        }
    }

    pub(super) fn blend(from: &Self, to: &Self, weight: impl Fn(usize) -> u32) -> Self {
        Self {
            samples: from
                .samples
                .iter()
                .zip(&to.samples)
                .enumerate()
                .map(|(ix, (from, to))| {
                    let weight = weight(ix / WIDTH / DOWN).min(FULL);
                    Sample {
                        covered: mix(from.covered, to.covered, weight),
                        lit: mix(from.lit, to.lit, weight),
                        pale: mix(from.pale, to.pale, weight),
                    }
                })
                .collect(),
        }
    }

    pub(super) fn parts(&self, column: usize, row: usize) -> [Sample; PARTS] {
        std::array::from_fn(|part| {
            self.samples[(row * DOWN + part / ACROSS) * WIDTH + column * ACROSS + part % ACROSS]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ACROSS, COLUMNS, DOWN, FULL, Field, ROWS, Sample};
    use image::{Rgba, RgbaImage};

    const KEY: Rgba<u8> = Rgba([0, 255, 0, 255]);
    const PAPER: Rgba<u8> = Rgba([250, 250, 245, 255]);
    const BLUE: Rgba<u8> = Rgba([30, 140, 220, 255]);
    const NAVY: Rgba<u8> = Rgba([30, 30, 70, 255]);

    fn picture(paint: impl Fn(u32, u32) -> Rgba<u8>) -> RgbaImage {
        RgbaImage::from_fn(
            (COLUMNS * ACROSS * 2) as u32,
            (ROWS * DOWN * 2) as u32,
            paint,
        )
    }

    fn halves() -> RgbaImage {
        picture(|x, _| {
            if x < (COLUMNS * ACROSS) as u32 {
                KEY
            } else {
                PAPER
            }
        })
    }

    #[test]
    fn the_key_colour_is_left_out_and_the_subject_is_sampled() {
        let field = Field::sample(&halves());
        for row in 0..ROWS {
            for sample in field.parts(0, row) {
                assert_eq!(sample, Sample::default());
            }
            for sample in field.parts(COLUMNS - 1, row) {
                assert_eq!((sample.covered, sample.pale), (255, 255));
            }
        }
    }

    #[test]
    fn brightness_is_stretched_over_the_subject_and_only_whites_are_pale() {
        let field = Field::sample(&picture(|x, _| {
            match x * 3 / (COLUMNS * ACROSS * 2) as u32 {
                0 => NAVY,
                1 => BLUE,
                _ => PAPER,
            }
        }));
        let [navy, blue, paper] =
            [0, COLUMNS / 2, COLUMNS - 1].map(|column| field.parts(column, 3)[0]);
        assert_eq!((navy.lit, paper.lit), (0, 255));
        assert!(navy.lit < blue.lit && blue.lit < paper.lit, "{blue:?}");
        assert_eq!((navy.pale, blue.pale, paper.pale), (0, 0, 255));
        assert_eq!((navy.covered, blue.covered), (255, 255));
    }

    #[test]
    fn a_blend_runs_from_one_field_to_the_other_row_by_row() {
        let (nothing, drawn) = (Field::empty(), Field::sample(&halves()));
        assert_eq!(Field::blend(&nothing, &drawn, |_| 0), nothing);
        assert_eq!(Field::blend(&nothing, &drawn, |_| FULL), drawn);
        assert_eq!(Field::blend(&nothing, &drawn, |_| FULL * 4), drawn);

        let swept = Field::blend(
            &nothing,
            &drawn,
            |row| if row == 0 { FULL } else { FULL / 2 },
        );
        assert_eq!(swept.parts(COLUMNS - 1, 0), drawn.parts(COLUMNS - 1, 0));
        for sample in swept.parts(COLUMNS - 1, 1) {
            assert!((126..=128).contains(&sample.covered), "{sample:?}");
        }
    }
}
