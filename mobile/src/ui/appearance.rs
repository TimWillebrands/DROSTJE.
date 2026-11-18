use std::{
    borrow::Cow,
    rc::Rc,
    sync::{Arc, OnceLock},
};

use drostje_themes::{Mode, bundled_default, grain};
use gpui_kit::{
    component::{ActiveTheme, Theme, ThemeConfig, ThemeMode, highlighter::HighlightTheme},
    *,
};
use serde_json::Value;

use crate::look::Look;

struct FontsLoaded;

impl Global for FontsLoaded {}

pub fn load_fonts(cx: &mut App) {
    if cx.has_global::<FontsLoaded>() {
        return;
    }
    cx.set_global(FontsLoaded);
    let faces = drostje_fonts::FACES
        .iter()
        .map(|face| Cow::Borrowed(face.bytes))
        .collect();
    if let Err(error) = cx.text_system().add_fonts(faces) {
        log::error!("Could not load the bundled fonts: {error:#}. Text uses system fonts.");
    }
}

pub fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

fn kit_mode(mode: Mode) -> ThemeMode {
    match mode {
        Mode::Light => ThemeMode::Light,
        Mode::Dark => ThemeMode::Dark,
    }
}

pub fn current_mode(cx: &App) -> Mode {
    if cx.theme().mode.is_dark() {
        Mode::Dark
    } else {
        Mode::Light
    }
}

fn config(face: &Value, mode: Mode) -> Rc<ThemeConfig> {
    let bundled = || {
        serde_json::from_value::<ThemeConfig>(bundled_default(mode))
            .expect("assets/themes/drostje.json is valid Kit JSON")
    };
    let config = match serde_json::from_value::<ThemeConfig>(face.clone()) {
        Ok(config) if config.mode == kit_mode(mode) => config,
        Ok(config) => {
            log::warn!("The desktop sent “{}” as a {mode:?} theme", config.name);
            bundled()
        }
        Err(error) => {
            log::warn!("The desktop theme is not Kit theme JSON: {error}");
            bundled()
        }
    };
    Rc::new(config)
}

pub fn apply(look: &Look, cx: &mut App) {
    let defaults = Theme::default();
    let theme = Theme::global_mut(cx);
    theme.font_family = defaults.font_family;
    theme.font_size = defaults.font_size;
    theme.mono_font_family = defaults.mono_font_family;
    theme.mono_font_size = defaults.mono_font_size;
    theme.radius = defaults.radius;
    theme.radius_lg = defaults.radius_lg;
    theme.shadow = defaults.shadow;
    theme.highlight_theme = match look.mode {
        Mode::Light => HighlightTheme::default_light(),
        Mode::Dark => HighlightTheme::default_dark(),
    };
    theme.light_theme = config(&look.light, Mode::Light);
    theme.dark_theme = config(&look.dark, Mode::Dark);
    Theme::change(kit_mode(look.mode), None, cx);
    cx.refresh_windows();
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extras {
    pub signature: Option<Hsla>,
    pub prose_family: Option<SharedString>,
    pub prose_size: Option<Pixels>,
    pub prose_line_height: Option<f32>,
    pub grain: f32,
}

impl Extras {
    pub fn of(look: Option<&Look>, cx: &App) -> Self {
        let Some(look) = look else {
            return Self::default();
        };
        let extensions = look.extensions(current_mode(cx));
        Self {
            signature: extensions
                .signature
                .as_deref()
                .and_then(|color| Rgba::try_from(color).ok())
                .map(Hsla::from),
            prose_family: extensions.prose.family.map(SharedString::from),
            prose_size: extensions.prose.size.map(px),
            prose_line_height: extensions.prose.line_height,
            grain: extensions.grain.unwrap_or(0.),
        }
    }

    pub fn signature(&self, cx: &App) -> Hsla {
        self.signature.unwrap_or(cx.theme().primary)
    }
}

fn tile() -> Arc<RenderImage> {
    static TILE_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
    TILE_IMAGE
        .get_or_init(|| {
            let buffer =
                image::RgbaImage::from_raw(grain::TILE, grain::TILE, grain::tile_rgba().to_vec())
                    .expect("noise fills the tile");
            Arc::new(RenderImage::new([image::Frame::new(buffer)]))
        })
        .clone()
}

fn paint_grain(bounds: Bounds<Pixels>, window: &mut Window) {
    let tile = tile();
    let edge = px(grain::TILE as f32 / window.scale_factor());
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

pub fn grain_layer(strength: f32) -> Option<AnyElement> {
    grain::opacity(strength).map(|opacity| {
        div()
            .id("grain")
            .absolute()
            .inset_0()
            .opacity(opacity)
            .child(
                canvas(
                    |_, _, _| {},
                    |bounds, _, window, _| paint_grain(bounds, window),
                )
                .size_full(),
            )
            .into_any_element()
    })
}
