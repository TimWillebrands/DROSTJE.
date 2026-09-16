use std::{collections::BTreeMap, path::Path, rc::Rc};

use anyhow::{Result, bail};
use gpui_kit::{
    component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry, highlighter::HighlightTheme},
    *,
};
use serde_json::Value;

use crate::app::{AppState, NoticeLevel};

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

#[derive(Default)]
struct Coordinator {
    preferences: Preferences,
    native_appearance: Option<Appearance>,
    applied: Option<AppliedTheme>,
    warnings: Vec<String>,
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
    cx.set_global(Coordinator {
        preferences: settings.map(Preferences::from_settings).unwrap_or_default(),
        ..Default::default()
    });
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

pub fn theme_names(mode: ThemeMode, cx: &App) -> Vec<String> {
    ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .filter(|theme| theme.mode == mode)
        .map(|theme| theme.name.to_string())
        .collect()
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
        let Some(theme) = ThemeRegistry::global(cx).themes().get(name) else {
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
    let registry = ThemeRegistry::global(cx);
    let fallback = match mode {
        ThemeMode::Light => registry.default_light_theme(),
        ThemeMode::Dark => registry.default_dark_theme(),
    };
    if let Some(name) = name {
        let selected = registry.themes().get(name);
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
    cx.global_mut::<Coordinator>().applied = Some(applied);
    if changed {
        // Kit only replaces optional typography/radius/highlight fields when
        // present. Reset those fields to Kit defaults so switching away from a
        // custom theme does not leak its styling into another theme.
        let defaults = Theme::default();
        let theme = Theme::global_mut(cx);
        theme.font_family = defaults.font_family;
        theme.font_size = defaults.font_size;
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
