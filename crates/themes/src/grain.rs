use std::sync::OnceLock;

pub const TILE: u32 = 256;
pub const MAX_OPACITY: f32 = 0.2;
const SEED: u32 = 0x9e37_79b9;

fn noise(seed: u32) -> Vec<u8> {
    let mut state = seed.max(1);
    let mut pixels = Vec::with_capacity((TILE * TILE * 4) as usize);
    for _ in 0..TILE * TILE {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let value = (state >> 24) as u8;
        let shade = if value >= 128 { 255 } else { 0 };
        let alpha = value.abs_diff(128).saturating_mul(2);
        pixels.extend_from_slice(&[shade, shade, shade, alpha]);
    }
    pixels
}

pub fn tile_rgba() -> &'static [u8] {
    static PIXELS: OnceLock<Vec<u8>> = OnceLock::new();
    PIXELS.get_or_init(|| noise(SEED))
}

pub fn opacity(strength: f32) -> Option<f32> {
    (strength > 0.).then(|| strength.min(1.) * MAX_OPACITY)
}

#[cfg(test)]
mod tests {
    use super::{SEED, TILE, noise, opacity, tile_rgba};

    #[test]
    fn noise_is_grey_balanced_and_stable() {
        let pixels = tile_rgba();
        assert_eq!(pixels.len(), (TILE * TILE * 4) as usize);
        assert_eq!(pixels, noise(SEED), "the same grain every launch");
        let (mut white, mut black) = (0usize, 0usize);
        for texel in pixels.as_chunks::<4>().0 {
            assert!(texel[0] == texel[1] && texel[1] == texel[2], "grey only");
            if texel[0] == 255 {
                white += texel[3] as usize;
            } else {
                black += texel[3] as usize;
            }
        }
        let ratio = white as f32 / black as f32;
        assert!(
            (0.95..1.05).contains(&ratio),
            "light and dark grain balance: {ratio}"
        );
    }

    #[test]
    fn no_strength_means_no_grain_and_full_strength_is_capped() {
        assert_eq!(opacity(0.), None);
        assert_eq!(opacity(0.5), Some(0.1));
        assert_eq!(opacity(3.), Some(0.2));
    }
}
