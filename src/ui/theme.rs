use std::{borrow::Cow, collections::BTreeMap, path::Path, rc::Rc};

use anyhow::{Result, bail};
use gpui_kit::{
    component::{
        Theme, ThemeConfig, ThemeMode, ThemeRegistry, ThemeSet, highlighter::HighlightTheme,
    },
    *,
};
use serde_json::Value;

use crate::app::{AppState, NoticeLevel};

/// Themes shipped with DROSTJE.: Delft, the default, and Ink, a reading mode. The
/// default of each mode is the fallback, and bundled themes are listed before registry
/// themes. Kit's registry is not told about them: `ThemeRegistry::load_themes_from_str`
/// entries are dropped on every directory reload, so bundled themes live here instead.
const BUNDLED_THEMES: &str = include_str!("../../assets/themes/drostje.json");
/// Names the bundled defaults had before Delft; a saved choice of them means the default.
const RENAMED_THEMES: [(&str, &str); 2] = [
    ("DROSTJE. Light", "Delft Light"),
    ("DROSTJE. Dark", "Delft Dark"),
];
/// The faces bundled themes name, embedded so every platform renders the same type.
/// Each family's licence sits beside its files in `assets/fonts`.
const FONTS: [&[u8]; 22] = [
    include_bytes!("../../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf"),
    include_bytes!("../../assets/fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf"),
    include_bytes!("../../assets/fonts/ibm-plex-sans/IBMPlexSans-Medium.ttf"),
    include_bytes!("../../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/ibm-plex-sans/IBMPlexSans-Bold.ttf"),
    include_bytes!("../../assets/fonts/ibm-plex-sans/IBMPlexSans-BoldItalic.ttf"),
    include_bytes!("../../assets/fonts/ibm-plex-mono/IBMPlexMono-Regular.ttf"),
    include_bytes!("../../assets/fonts/ibm-plex-mono/IBMPlexMono-Italic.ttf"),
    include_bytes!("../../assets/fonts/ibm-plex-mono/IBMPlexMono-Bold.ttf"),
    include_bytes!("../../assets/fonts/libron/Libron-Regular.ttf"),
    include_bytes!("../../assets/fonts/libron/Libron-Italic.ttf"),
    include_bytes!("../../assets/fonts/libron/Libron-Bold.ttf"),
    include_bytes!("../../assets/fonts/libron/Libron-BoldItalic.ttf"),
    include_bytes!("../../assets/fonts/monaspace-xenon/MonaspaceXenon-Regular.otf"),
    include_bytes!("../../assets/fonts/monaspace-xenon/MonaspaceXenon-Italic.otf"),
    include_bytes!("../../assets/fonts/monaspace-xenon/MonaspaceXenon-Bold.otf"),
    // No Regular: normal text falls to Medium, which keeps the narrow Dosis from going thin.
    include_bytes!("../../assets/fonts/dosis/Dosis-Medium.ttf"),
    include_bytes!("../../assets/fonts/dosis/Dosis-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/dosis/Dosis-Bold.ttf"),
    include_bytes!("../../assets/fonts/maple-mono-nl/MapleMonoNL-Regular.ttf"),
    include_bytes!("../../assets/fonts/maple-mono-nl/MapleMonoNL-Italic.ttf"),
    include_bytes!("../../assets/fonts/maple-mono-nl/MapleMonoNL-Bold.ttf"),
];
/// The brand face: the wordmark and the Welcome screen, in every theme.
pub const DISPLAY_FONT: &str = "Dosis";
/// Base UI size when a theme sets no `font.size`. Kit uses it as the window's
/// rem, so it scales spacing and every `text_*` step along with the prose.
const UI_FONT_SIZE: Pixels = px(15.);

/// Terminal ANSI colors share the active theme's base palette; literal ANSI black/white
/// stay ordered by luminance even in light mode, independently of default text colors.
pub(super) fn terminal_theme(cx: &App) -> ghostty_terminal::TerminalTheme {
    use ghostty_terminal::{TerminalColor, TerminalTheme};
    let theme = Theme::global(cx);
    let color = |value: Hsla| {
        let rgba: Rgba = value.into();
        TerminalColor::new(
            (rgba.r * 255.).round() as u8,
            (rgba.g * 255.).round() as u8,
            (rgba.b * 255.).round() as u8,
        )
    };
    let (black, white) = if theme.mode.is_dark() {
        (theme.background, theme.foreground)
    } else {
        (theme.foreground, theme.background)
    };
    TerminalTheme::new(
        color(theme.background),
        color(theme.foreground),
        [
            black,
            theme.red,
            theme.green,
            theme.yellow,
            theme.blue,
            theme.magenta,
            theme.cyan,
            white,
            theme.muted_foreground,
            theme.red_light,
            theme.green_light,
            theme.yellow_light,
            theme.blue_light,
            theme.magenta_light,
            theme.cyan_light,
            white,
        ]
        .map(color),
    )
}

/// What bundled themes set beyond Kit's format, as `prose.*` and `signature` keys beside
/// Kit's own. Other themes get the defaults.
#[derive(Clone, Debug, Default, PartialEq)]
struct Extensions {
    prose: Prose,
    /// The wordmark's full stop and the running-turn mark; the accent when unset.
    signature: Option<Hsla>,
}

impl Extensions {
    fn from_theme(theme: &Value) -> Self {
        Self {
            prose: Prose::from_theme(theme),
            signature: theme.get("signature").map(|color| {
                color
                    .as_str()
                    .and_then(|color| Rgba::try_from(color).ok())
                    .expect("bundled signature is a hex color")
                    .into()
            }),
        }
    }
}

/// How a theme sets agent prose in the transcript; unset fields use the UI font.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prose {
    pub family: Option<SharedString>,
    pub size: Option<Pixels>,
    /// Line height as a multiple of the size.
    pub line_height: Option<f32>,
}

impl Prose {
    fn from_theme(theme: &Value) -> Self {
        let number = |key: &str| {
            theme.get(key).map(|value| {
                value
                    .as_f64()
                    .filter(|value| *value > 0.)
                    .unwrap_or_else(|| panic!("bundled {key} is a positive number"))
                    as f32
            })
        };
        Self {
            family: theme
                .get("prose.font.family")
                .and_then(Value::as_str)
                .map(|family| SharedString::from(family.to_owned())),
            size: number("prose.font.size").map(px),
            line_height: number("prose.line_height"),
        }
    }
}

struct Bundled {
    config: Rc<ThemeConfig>,
    extensions: Extensions,
}

/// Bundled themes in file order; each mode has exactly one default.
fn parse_bundled() -> Vec<Bundled> {
    let set: ThemeSet =
        serde_json::from_str(BUNDLED_THEMES).expect("assets/themes/drostje.json is valid Kit JSON");
    let raw: Value = serde_json::from_str(BUNDLED_THEMES).expect("bundled themes are JSON");
    let bundled: Vec<_> = set
        .themes
        .into_iter()
        .zip(raw["themes"].as_array().expect("bundled themes are a list"))
        .map(|(config, raw)| Bundled {
            config: Rc::new(config),
            extensions: Extensions::from_theme(raw),
        })
        .collect();
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        assert_eq!(
            bundled
                .iter()
                .filter(|theme| theme.config.mode == mode && theme.config.is_default)
                .count(),
            1,
            "assets/themes/drostje.json bundles one default {} theme",
            mode.name()
        );
    }
    bundled
}

/// A saved theme name under its current name.
pub fn saved_theme_name(name: &str) -> &str {
    RENAMED_THEMES
        .iter()
        .find(|(old, _)| *old == name)
        .map_or(name, |(_, new)| new)
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    fn from_setting(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_str) {
            Some("light") => Self::Light,
            Some("dark") => Self::Dark,
            _ => Self::System,
        }
    }

    fn native(self) -> Option<WindowAppearance> {
        match self {
            Self::System => None,
            Self::Light => Some(WindowAppearance::Light),
            Self::Dark => Some(WindowAppearance::Dark),
        }
    }
}

#[derive(Clone, Default)]
struct Preferences {
    appearance: Appearance,
    light: Option<String>,
    dark: Option<String>,
}

fn saved_name(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(saved_theme_name)
}

impl Preferences {
    fn from_settings(settings: &BTreeMap<String, Value>) -> Self {
        Self {
            appearance: Appearance::from_setting(settings.get("theme_mode")),
            light: saved_name(settings.get("theme_light")).map(str::to_owned),
            dark: saved_name(settings.get("theme_dark")).map(str::to_owned),
        }
    }

    fn matches(&self, settings: &BTreeMap<String, Value>) -> bool {
        self.appearance == Appearance::from_setting(settings.get("theme_mode"))
            && self.light.as_deref() == saved_name(settings.get("theme_light"))
            && self.dark.as_deref() == saved_name(settings.get("theme_dark"))
    }
}

struct AppliedTheme {
    mode: ThemeMode,
    light_config: Rc<ThemeConfig>,
    dark_config: Rc<ThemeConfig>,
    // Kit's ThemeConfig has no PartialEq. Compare its JSON data only when
    // resolving a changed configuration, never on a render or routine state update.
    light: Value,
    dark: Value,
}

struct Coordinator {
    preferences: Preferences,
    bundled: Vec<Bundled>,
    native_appearance: Option<Appearance>,
    applied: Option<AppliedTheme>,
    /// Extensions of the theme in effect for the current mode.
    extensions: Extensions,
    fonts_error: Option<String>,
    warnings: Vec<String>,
}

impl Coordinator {
    fn new(preferences: Preferences, fonts_error: Option<String>) -> Self {
        Self {
            preferences,
            bundled: parse_bundled(),
            native_appearance: None,
            applied: None,
            extensions: Extensions::default(),
            fonts_error,
            warnings: Vec::new(),
        }
    }

    /// The default bundled theme of `mode`.
    fn bundled(&self, mode: ThemeMode) -> &Rc<ThemeConfig> {
        &self
            .bundled
            .iter()
            .find(|theme| theme.config.mode == mode && theme.config.is_default)
            .expect("parse_bundled checked one default per mode")
            .config
    }

    fn extensions(&self, theme: &Rc<ThemeConfig>) -> Extensions {
        self.bundled
            .iter()
            .find(|bundled| Rc::ptr_eq(&bundled.config, theme))
            .map(|bundled| bundled.extensions.clone())
            .unwrap_or_default()
    }
}

/// A bundled theme wins over a registry theme of the same name so a stray
/// file cannot replace the defaults; other registry themes are found by name.
fn find_theme<'a>(name: &str, cx: &'a App) -> Option<&'a Rc<ThemeConfig>> {
    let name = saved_theme_name(name);
    cx.global::<Coordinator>()
        .bundled
        .iter()
        .map(|theme| &theme.config)
        .find(|theme| theme.name.as_ref() == name)
        .or_else(|| ThemeRegistry::global(cx).themes().get(name))
}

impl Global for Coordinator {}

struct InitialLoad {
    finished: bool,
    error: Option<String>,
}

impl Global for InitialLoad {}

/// Initialize after Kit, before constructing a window or starting its editor.
/// Without a bootstrap (the recovery window), use System and registry defaults.
pub fn initialize(
    settings: Option<&BTreeMap<String, Value>>,
    data_dir: Option<&Path>,
    cx: &mut App,
) {
    // Register once per app; a second initialization reuses the faces.
    let fonts_error = match cx.try_global::<Coordinator>() {
        Some(coordinator) => coordinator.fonts_error.clone(),
        None => cx
            .text_system()
            .add_fonts(FONTS.iter().map(|font| Cow::Borrowed(*font)).collect())
            .err()
            .map(|error| {
                format!("Could not load the bundled fonts: {error:#}. Text uses system fonts.")
            }),
    };
    cx.set_global(Coordinator::new(
        settings.map(Preferences::from_settings).unwrap_or_default(),
        fonts_error,
    ));
    cx.set_global(InitialLoad {
        finished: data_dir.is_none(),
        error: None,
    });
    apply(None, cx);

    if let Some(data_dir) = data_dir {
        // Kit loads top-level ThemeSet JSON files and owns duplicate-name
        // precedence. Its callback marks the initial load attempt, NOT watcher
        // readiness: asynchronous watcher/parse errors are logged inside Kit.
        if let Err(error) = ThemeRegistry::watch_dir(data_dir.join("themes"), cx, |cx| {
            cx.global_mut::<InitialLoad>().finished = true;
        }) {
            let load = cx.global_mut::<InitialLoad>();
            load.finished = true;
            load.error = Some(format!("Could not load the themes directory: {error:#}"));
        }
    }
}

/// Whether the initial registry load attempt finished; not watcher readiness.
pub fn initial_load_finished(cx: &App) -> bool {
    cx.try_global::<InitialLoad>()
        .is_none_or(|load| load.finished)
}

/// Bundled themes first, then registry themes in Kit's order, without repeats.
pub fn theme_names(mode: ThemeMode, cx: &App) -> Vec<String> {
    let bundled: Vec<_> = cx
        .global::<Coordinator>()
        .bundled
        .iter()
        .filter(|theme| theme.config.mode == mode)
        .map(|theme| theme.config.name.to_string())
        .collect();
    let registry = ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .filter(|theme| theme.mode == mode && !bundled.iter().any(|name| *name == theme.name))
        .map(|theme| theme.name.to_string())
        .collect::<Vec<_>>();
    bundled.into_iter().chain(registry).collect()
}

/// Agent prose in the theme in effect; empty fields fall back to the UI font.
pub fn prose(cx: &App) -> Prose {
    cx.try_global::<Coordinator>()
        .map(|coordinator| coordinator.extensions.prose.clone())
        .unwrap_or_default()
}

/// The full stop's color in the theme in effect.
pub fn signature(cx: &App) -> Hsla {
    cx.try_global::<Coordinator>()
        .and_then(|coordinator| coordinator.extensions.signature)
        .unwrap_or_else(|| Theme::global(cx).primary)
}

/// Validate new choices while allowing an unavailable saved choice to survive
/// an unrelated Settings save. Resolution keeps that name and uses a fallback.
pub fn validate_preferences(
    mode: &str,
    light: Option<&str>,
    dark: Option<&str>,
    cx: &App,
) -> Result<()> {
    if !matches!(mode, "system" | "light" | "dark") {
        bail!("Choose System, Light, or Dark appearance.");
    }
    let preferences = cx
        .try_global::<Coordinator>()
        .map(|theme| &theme.preferences);
    for (mode, name, saved) in [
        (
            ThemeMode::Light,
            light,
            preferences.and_then(|p| p.light.as_deref()),
        ),
        (
            ThemeMode::Dark,
            dark,
            preferences.and_then(|p| p.dark.as_deref()),
        ),
    ] {
        let Some(name) = name else { continue };
        if name.trim().is_empty() {
            bail!("Choose a {} theme or the registry default.", mode.name());
        }
        if Some(name) == saved {
            continue;
        }
        let Some(theme) = find_theme(name, cx) else {
            bail!(
                "The theme “{name}” is unavailable. Choose an installed {} theme.",
                mode.name()
            );
        };
        if theme.mode != mode {
            bail!("The theme “{name}” is not a {} theme.", mode.name());
        }
    }
    Ok(())
}

fn resolve(
    name: Option<&str>,
    mode: ThemeMode,
    cx: &App,
    warnings: &mut Vec<String>,
) -> Rc<ThemeConfig> {
    let fallback = cx.global::<Coordinator>().bundled(mode);
    if let Some(name) = name {
        let selected = find_theme(name, cx);
        if let Some(theme) = selected.filter(|theme| theme.mode == mode) {
            return theme.clone();
        }
        if cx.global::<InitialLoad>().finished {
            let reason = if selected.is_some() {
                format!("is not a {} theme", mode.name())
            } else {
                "is unavailable".to_owned()
            };
            warnings.push(format!(
                "The saved {} theme “{name}” {reason}. Using “{}”; the saved choice is retained and will recover when its theme file is fixed.",
                mode.name(), fallback.name,
            ));
        }
    }
    fallback.clone()
}

fn apply(window: Option<&mut Window>, cx: &mut App) -> Vec<String> {
    let preferences = cx.global::<Coordinator>().preferences.clone();
    if cx.global::<Coordinator>().native_appearance != Some(preferences.appearance) {
        // Record before calling the platform: resulting appearance notifications
        // must not set the native override a second time.
        cx.global_mut::<Coordinator>().native_appearance = Some(preferences.appearance);
        cx.set_window_appearance(preferences.appearance.native());
    }
    let mode = match preferences.appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
        Appearance::System => ThemeMode::from(
            window
                .as_ref()
                .map(|window| window.appearance())
                .unwrap_or_else(|| cx.window_appearance()),
        ),
    };
    let mut warnings = Vec::new();
    if let Some(error) = &cx.global::<InitialLoad>().error {
        warnings.push(error.clone());
    }
    if let Some(error) = &cx.global::<Coordinator>().fonts_error {
        warnings.push(error.clone());
    }
    let light = resolve(
        preferences.light.as_deref(),
        ThemeMode::Light,
        cx,
        &mut warnings,
    );
    let dark = resolve(
        preferences.dark.as_deref(),
        ThemeMode::Dark,
        cx,
        &mut warnings,
    );
    if cx
        .global::<Coordinator>()
        .applied
        .as_ref()
        .is_some_and(|applied| {
            applied.mode == mode
                && Rc::ptr_eq(&applied.light_config, &light)
                && Rc::ptr_eq(&applied.dark_config, &dark)
        })
    {
        return warnings;
    }
    let applied = AppliedTheme {
        mode,
        light_config: light.clone(),
        dark_config: dark.clone(),
        light: serde_json::to_value(light.as_ref()).expect("Kit theme fields are JSON-compatible"),
        dark: serde_json::to_value(dark.as_ref()).expect("Kit theme fields are JSON-compatible"),
    };
    let changed = cx
        .global::<Coordinator>()
        .applied
        .as_ref()
        .is_none_or(|previous| {
            previous.mode != applied.mode
                || previous.light != applied.light
                || previous.dark != applied.dark
        });
    let extensions =
        cx.global::<Coordinator>()
            .extensions(if mode.is_dark() { &dark } else { &light });
    let coordinator = cx.global_mut::<Coordinator>();
    coordinator.applied = Some(applied);
    coordinator.extensions = extensions;
    if changed {
        // Kit only replaces optional typography/radius/highlight fields when
        // present. Reset those fields to Kit defaults so switching away from a
        // custom theme does not leak its styling into another theme.
        let defaults = Theme::default();
        let theme = Theme::global_mut(cx);
        theme.font_family = defaults.font_family;
        theme.font_size = UI_FONT_SIZE;
        theme.mono_font_family = defaults.mono_font_family;
        theme.mono_font_size = defaults.mono_font_size;
        theme.radius = defaults.radius;
        theme.radius_lg = defaults.radius_lg;
        theme.shadow = defaults.shadow;
        theme.highlight_theme = if mode.is_dark() {
            HighlightTheme::default_dark()
        } else {
            HighlightTheme::default_light()
        };
        theme.light_theme = light;
        theme.dark_theme = dark;
        // change applies the effective config and syncs all direct mutations to
        // Base; a separate sync_base would duplicate that projection.
        Theme::change(mode, window, cx);
        cx.refresh_windows();
    }
    warnings
}

fn update(state: &Entity<AppState>, window: &mut Window, cx: &mut App) {
    let warnings = apply(Some(window), cx);
    let coordinator = cx.global_mut::<Coordinator>();
    let new_warnings: Vec<_> = warnings
        .iter()
        .filter(|warning| !coordinator.warnings.contains(warning))
        .cloned()
        .collect();
    coordinator.warnings = warnings;
    if !new_warnings.is_empty() {
        state.update(cx, |state, cx| {
            for warning in new_warnings {
                state.notice(NoticeLevel::Warning, warning);
            }
            cx.notify();
        });
    }
}

/// Keep these subscriptions with the workspace, not in rendering. In
/// particular, routine AppState notifications must not reapply the palette.
pub fn attach(state: Entity<AppState>, window: &mut Window, cx: &mut App) -> Vec<Subscription> {
    if !cx.has_global::<Coordinator>() {
        let state = state.read(cx);
        let settings = state.snapshot.settings.clone();
        let data_dir = state.data_dir.clone();
        initialize(Some(&settings), Some(&data_dir), cx);
    }
    if !cx
        .global::<Coordinator>()
        .preferences
        .matches(&state.read(cx).snapshot.settings)
    {
        let preferences = Preferences::from_settings(&state.read(cx).snapshot.settings);
        cx.global_mut::<Coordinator>().preferences = preferences;
    }
    update(&state, window, cx);

    let settings = window.observe(&state, cx, |state, window, cx| {
        if cx
            .global::<Coordinator>()
            .preferences
            .matches(&state.read(cx).snapshot.settings)
        {
            return;
        }
        let preferences = Preferences::from_settings(&state.read(cx).snapshot.settings);
        cx.global_mut::<Coordinator>().preferences = preferences;
        update(&state, window, cx);
    });
    let weak_state = state.downgrade();
    let registry = window.observe_global::<ThemeRegistry>(cx, move |window, cx| {
        if let Some(state) = weak_state.upgrade() {
            update(&state, window, cx);
        }
    });
    let weak_state = state.downgrade();
    let initial_load = window.observe_global::<InitialLoad>(cx, move |window, cx| {
        if let Some(state) = weak_state.upgrade() {
            update(&state, window, cx);
        }
    });
    let appearance = window.observe_window_appearance(move |window, cx| {
        if cx.global::<Coordinator>().preferences.appearance == Appearance::System {
            update(&state, window, cx);
        }
    });
    vec![settings, registry, initial_load, appearance]
}

pub fn attach_recovery(window: &mut Window, cx: &mut App) -> Subscription {
    apply(Some(window), cx);
    window.observe_window_appearance(|window, cx| {
        apply(Some(window), cx);
    })
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the Kit facade exports a `test` macro that would
    // shadow `#[test]`.
    use super::{DISPLAY_FONT, FONTS, Prose, parse_bundled, saved_theme_name};
    use gpui_kit::{FontStyle, FontWeight, Hsla, Rgba, component::ThemeMode, font, px};
    use std::borrow::Cow;

    #[test]
    fn bundled_themes_are_delft_by_default_with_ink_and_cootje_beside_it() {
        let bundled = parse_bundled();
        let names: Vec<_> = bundled
            .iter()
            .map(|theme| {
                (
                    theme.config.name.as_ref(),
                    theme.config.mode,
                    theme.config.is_default,
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("Delft Light", ThemeMode::Light, true),
                ("Delft Dark", ThemeMode::Dark, true),
                ("Ink Light", ThemeMode::Light, false),
                ("Ink Dark", ThemeMode::Dark, false),
                ("Cootje Light", ThemeMode::Light, false),
                ("Cootje Dark", ThemeMode::Dark, false),
            ]
        );
        let theme = |name: &str| {
            bundled
                .iter()
                .find(|theme| theme.config.name.as_ref() == name)
                .unwrap()
        };
        let delft = theme("Delft Light");
        assert_eq!(delft.config.colors.background.as_deref(), Some("#f7f9fc"));
        assert_eq!(delft.config.colors.foreground.as_deref(), Some("#1b2740"));
        assert_eq!(delft.config.colors.primary.as_deref(), Some("#2453a6"));
        assert_eq!(delft.config.font_family.as_deref(), Some("IBM Plex Sans"));
        assert_eq!(
            delft.config.mono_font_family.as_deref(),
            Some("IBM Plex Mono")
        );
        assert_eq!(
            delft.extensions.prose,
            Prose::default(),
            "Delft prose is its UI font"
        );
        for name in ["Ink Light", "Ink Dark"] {
            let ink = theme(name);
            assert_eq!(
                ink.config.mono_font_family.as_deref(),
                Some("Monaspace Xenon")
            );
            assert_eq!(
                ink.extensions.prose,
                Prose {
                    family: Some("Libron".into()),
                    size: Some(px(17.)),
                    line_height: Some(1.55),
                }
            );
        }
        let hex = |hex: &str| -> Hsla { Rgba::try_from(hex).unwrap().into() };
        for (name, signature) in [("Cootje Light", "#c07f00"), ("Cootje Dark", "#f2b33d")] {
            let cootje = theme(name);
            assert_eq!(cootje.config.font_family.as_deref(), Some("Dosis"));
            assert_eq!(cootje.config.font_size, Some(16.));
            assert_eq!(
                cootje.config.mono_font_family.as_deref(),
                Some("Maple Mono NL")
            );
            assert_eq!(cootje.extensions.prose, Prose::default(), "{name}");
            assert_eq!(cootje.extensions.signature, Some(hex(signature)), "{name}");
        }
        for name in ["Delft Light", "Delft Dark", "Ink Light", "Ink Dark"] {
            assert_eq!(
                theme(name).extensions.signature,
                None,
                "{name} uses its accent"
            );
        }
        for theme in &bundled {
            // Kit 0.6.1 drops the flattened `editor.*` / status colors of a
            // highlight block while parsing (its own default does too), so
            // only the syntax table is asserted here.
            let highlight = theme
                .config
                .highlight
                .as_ref()
                .expect("bundled syntax colors");
            assert!(highlight.syntax.keyword.is_some(), "{}", theme.config.name);
            assert!(highlight.syntax.string.is_some(), "{}", theme.config.name);
        }
    }

    #[test]
    fn saved_defaults_from_before_delft_mean_delft() {
        assert_eq!(saved_theme_name("DROSTJE. Light"), "Delft Light");
        assert_eq!(saved_theme_name("DROSTJE. Dark"), "Delft Dark");
        assert_eq!(saved_theme_name("Ink Dark"), "Ink Dark");
        assert_eq!(saved_theme_name("Runtime dark"), "Runtime dark");
    }

    /// The test platform's text system is a stub, so this asks the real one.
    #[test]
    fn every_family_a_bundled_theme_names_is_embedded() {
        let text_system = gpui_kit::platform::current_platform(true).text_system();
        text_system
            .add_fonts(FONTS.iter().map(|font| Cow::Borrowed(*font)).collect())
            .expect("bundled fonts load");
        let installed = text_system.all_font_names();
        let mut families = vec![DISPLAY_FONT.to_owned()];
        for theme in parse_bundled() {
            families.extend(
                [
                    theme.config.font_family.clone(),
                    theme.config.mono_font_family.clone(),
                    theme.extensions.prose.family.clone(),
                ]
                .into_iter()
                .flatten()
                .map(|family| family.to_string()),
            );
        }
        for family in families {
            assert!(installed.contains(&family), "{family} is embedded");
        }
        let face = |family, weight, style| {
            let mut face = font(family);
            face.weight = weight;
            face.style = style;
            text_system.font_id(&face).unwrap()
        };
        // Bold and italic text uses real faces, not the regular one.
        for family in ["IBM Plex Sans", "Libron"] {
            let face = |weight, style| face(family, weight, style);
            let regular = face(FontWeight::NORMAL, FontStyle::Normal);
            assert_ne!(
                regular,
                face(FontWeight::BOLD, FontStyle::Normal),
                "{family} bold"
            );
            assert_ne!(
                regular,
                face(FontWeight::NORMAL, FontStyle::Italic),
                "{family} italic"
            );
        }
        // Dosis has no Regular: normal text is set in Medium, and bold stays bold.
        let normal = face("Dosis", FontWeight::NORMAL, FontStyle::Normal);
        assert_eq!(normal, face("Dosis", FontWeight::MEDIUM, FontStyle::Normal));
        assert_ne!(normal, face("Dosis", FontWeight::BOLD, FontStyle::Normal));
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod runtime_tests {
    use super::{initialize, prose, resolve, signature, theme_names, validate_preferences};
    use gpui_kit::{
        Hsla, TestAppContext,
        component::{Theme, ThemeMode},
        rgb,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    #[gpui_kit::test]
    fn bundled_themes_are_the_fallback_and_listed_first(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            initialize(None, None, cx);

            let light = theme_names(ThemeMode::Light, cx);
            assert_eq!(light[..3], ["Delft Light", "Ink Light", "Cootje Light"]);
            assert_eq!(
                theme_names(ThemeMode::Dark, cx)[..3],
                ["Delft Dark", "Ink Dark", "Cootje Dark"]
            );
            assert!(light.contains(&"Default Light".to_owned()));

            let theme = Theme::global(cx);
            assert_eq!(theme.light_theme.name.as_ref(), "Delft Light");
            assert_eq!(theme.dark_theme.name.as_ref(), "Delft Dark");
            let expected: Hsla = if theme.mode.is_dark() {
                rgb(0xdce3f0).into()
            } else {
                rgb(0x1b2740).into()
            };
            assert_eq!(theme.foreground, expected);
            assert_eq!(theme.font_family.as_ref(), "IBM Plex Sans");
            assert_eq!(prose(cx).family, None, "Delft prose is its UI font");
            assert_eq!(
                signature(cx),
                theme.primary,
                "Delft's full stop is its accent"
            );

            validate_preferences("light", Some("Delft Light"), Some("Ink Dark"), cx)
                .expect("bundled names validate");
            validate_preferences("light", Some("DROSTJE. Light"), None, cx)
                .expect("names from before Delft still validate");
            validate_preferences("light", Some("Default Light"), None, cx)
                .expect("Kit defaults stay selectable");
            assert!(validate_preferences("light", Some("Missing"), None, cx).is_err());
            assert!(validate_preferences("light", Some("Ink Dark"), None, cx).is_err());

            let mut warnings = Vec::new();
            let resolved = resolve(Some("Missing"), ThemeMode::Light, cx, &mut warnings);
            assert_eq!(resolved.name.as_ref(), "Delft Light");
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            let mut warnings = Vec::new();
            let resolved = resolve(Some("DROSTJE. Dark"), ThemeMode::Dark, cx, &mut warnings);
            assert_eq!(resolved.name.as_ref(), "Delft Dark");
            assert!(warnings.is_empty(), "{warnings:?}");
        });
    }

    #[gpui_kit::test]
    fn ink_sets_agent_prose_in_libron_and_switching_away_restores_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            let settings = |light: &str| {
                BTreeMap::from([
                    ("theme_mode".to_owned(), json!("light")),
                    ("theme_light".to_owned(), json!(light)),
                ])
            };
            initialize(Some(&settings("Ink Light")), None, cx);
            let theme = Theme::global(cx);
            assert_eq!(theme.mono_font_family.as_ref(), "Monaspace Xenon");
            assert_eq!(prose(cx).family.as_deref(), Some("Libron"));

            initialize(Some(&settings("Delft Light")), None, cx);
            assert_eq!(Theme::global(cx).mono_font_family.as_ref(), "IBM Plex Mono");
            assert_eq!(prose(cx).family, None);
        });
    }

    #[gpui_kit::test]
    fn cootje_sets_the_ui_in_dosis_with_a_saffron_full_stop(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            let settings = BTreeMap::from([
                ("theme_mode".to_owned(), json!("dark")),
                ("theme_dark".to_owned(), json!("Cootje Dark")),
            ]);
            initialize(Some(&settings), None, cx);
            let theme = Theme::global(cx);
            assert_eq!(theme.font_family.as_ref(), "Dosis");
            assert_eq!(theme.mono_font_family.as_ref(), "Maple Mono NL");
            assert_eq!(theme.font_size, gpui_kit::px(16.));
            let saffron: Hsla = rgb(0xf2b33d).into();
            assert_eq!(signature(cx), saffron);
            assert_ne!(signature(cx), theme.primary);
        });
    }
}
