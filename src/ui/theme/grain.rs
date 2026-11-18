use std::sync::{Arc, OnceLock};

use gpui_kit::*;

use crate::themes::grain::{TILE, opacity, tile_rgba};

fn tile() -> Arc<RenderImage> {
    static TILE_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
    TILE_IMAGE
        .get_or_init(|| {
            let buffer = image::RgbaImage::from_raw(TILE, TILE, tile_rgba().to_vec())
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

pub fn layer(strength: f32) -> Option<AnyElement> {
    opacity(strength).map(|opacity| {
        div()
            .absolute()
            .inset_0()
            .opacity(opacity)
            .child(canvas(|_, _, _| {}, |bounds, _, window, _| paint(bounds, window)).size_full())
            .into_any_element()
    })
}
