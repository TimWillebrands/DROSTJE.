//! Film grain over window chrome. One static noise tile is painted at device
//! resolution, one texel per screen pixel, on a grid anchored to the window origin so
//! neighbouring surfaces continue the same pattern. Reading surfaces get none.

use std::sync::{Arc, OnceLock};

use gpui_kit::*;

/// Device pixels per tile edge.
const TILE: u32 = 256;
/// Opacity of the layer at full strength; the noise itself averages half coverage.
const MAX_OPACITY: f32 = 0.2;

/// Grey noise: each texel is white or black with a random alpha, so the grain neither
/// lightens nor darkens a surface on average and reads the same in both modes.
fn noise(seed: u32) -> Vec<u8> {
    let mut state = seed.max(1);
    let mut pixels = Vec::with_capacity((TILE * TILE * 4) as usize);
    for _ in 0..TILE * TILE {
        // xorshift32: deterministic, so the grain does not change between launches.
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

fn tile() -> Arc<RenderImage> {
    static TILE_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
    TILE_IMAGE
        .get_or_init(|| {
            let buffer = image::RgbaImage::from_raw(TILE, TILE, noise(0x9e37_79b9))
                .expect("noise fills the tile");
            Arc::new(RenderImage::new([image::Frame::new(buffer)]))
        })
        .clone()
}

fn paint(bounds: Bounds<Pixels>, window: &mut Window) {
    let tile = tile();
    let edge = px(TILE as f32 / window.scale_factor());
    let mut y = edge * (bounds.top() / edge).floor();
    while y < bounds.bottom() {
        let mut x = edge * (bounds.left() / edge).floor();
        while x < bounds.right() {
            // Decorative: a tile the atlas cannot take is left out, not reported.
            let _ = window.paint_image(
                bounds,
                Bounds::new(point(x, y), size(edge, edge)),
                Corners::default(),
                tile.clone(),
                0,
                false,
            );
            x += edge;
        }
        y += edge;
    }
}

/// A layer over its `relative()` parent at `strength` (0 to 1), or nothing at 0. It has
/// no hitbox, so the chrome beneath keeps every pointer event.
pub fn layer(strength: f32) -> Option<AnyElement> {
    (strength > 0.).then(|| {
        div()
            .absolute()
            .inset_0()
            .opacity(strength.min(1.) * MAX_OPACITY)
            .child(canvas(|_, _, _| {}, |bounds, _, window, _| paint(bounds, window)).size_full())
            .into_any_element()
    })
}

#[cfg(test)]
mod tests {
    use super::{TILE, noise};

    #[test]
    fn noise_is_grey_balanced_and_stable() {
        let pixels = noise(0x9e37_79b9);
        assert_eq!(pixels.len(), (TILE * TILE * 4) as usize);
        assert_eq!(pixels, noise(0x9e37_79b9), "the same grain every launch");
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
}
