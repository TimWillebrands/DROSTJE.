use drostje_phone::view::{Appearance, ThemeView};
use drostje_themes::{Extensions, Mode, bundled_default};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub mode: Mode,
    pub light: Value,
    pub dark: Value,
}

impl Look {
    pub fn resolve(theme: Option<&ThemeView>, system_dark: bool) -> Self {
        let system = if system_dark { Mode::Dark } else { Mode::Light };
        match theme {
            None => Self {
                mode: system,
                light: bundled_default(Mode::Light),
                dark: bundled_default(Mode::Dark),
            },
            Some(theme) => Self {
                mode: match theme.appearance {
                    Appearance::System => system,
                    Appearance::Light => Mode::Light,
                    Appearance::Dark => Mode::Dark,
                },
                light: theme.light.clone(),
                dark: theme.dark.clone(),
            },
        }
    }

    pub fn face(&self, mode: Mode) -> &Value {
        match mode {
            Mode::Light => &self.light,
            Mode::Dark => &self.dark,
        }
    }

    pub fn active(&self) -> &Value {
        self.face(self.mode)
    }

    pub fn extensions(&self, mode: Mode) -> Extensions {
        Extensions::from_theme(self.face(mode)).unwrap_or_else(|error| {
            log::warn!("Ignoring the desktop theme's extensions: {error}");
            Extensions::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn desktop(appearance: Appearance) -> ThemeView {
        ThemeView {
            appearance,
            light: json!({"name": "Ink Light", "mode": "light", "grain": 0.15}),
            dark: json!({"name": "Cootje Dark", "mode": "dark", "signature": "#f2b33d"}),
        }
    }

    #[test]
    fn before_pairing_the_phone_wears_delft_in_its_own_appearance() {
        let day = Look::resolve(None, false);
        assert_eq!(day.mode, Mode::Light);
        assert_eq!(day.active()["name"], "Delft Light");
        let night = Look::resolve(None, true);
        assert_eq!(night.active()["name"], "Delft Dark");
        assert_eq!(night.extensions(night.mode), Extensions::default());
    }

    #[test]
    fn the_desktop_appearance_wins_unless_it_follows_the_system() {
        let system = desktop(Appearance::System);
        assert_eq!(
            Look::resolve(Some(&system), true).active()["name"],
            "Cootje Dark"
        );
        assert_eq!(
            Look::resolve(Some(&system), false).active()["name"],
            "Ink Light"
        );
        let dark = desktop(Appearance::Dark);
        assert_eq!(Look::resolve(Some(&dark), false).mode, Mode::Dark);
        let light = desktop(Appearance::Light);
        assert_eq!(Look::resolve(Some(&light), true).mode, Mode::Light);
    }

    #[test]
    fn extensions_come_from_the_active_theme_and_bad_ones_are_dropped() {
        let theme = desktop(Appearance::Dark);
        let dark = Look::resolve(Some(&theme), false);
        assert_eq!(
            dark.extensions(dark.mode).signature.as_deref(),
            Some("#f2b33d")
        );
        assert_eq!(dark.extensions(Mode::Light).grain, Some(0.15));
        let light = Look::resolve(Some(&desktop(Appearance::Light)), true);
        assert_eq!(light.extensions(light.mode).signature, None);
        assert_eq!(light.extensions(light.mode).grain, Some(0.15));
        let broken = ThemeView {
            dark: json!({"name": "Odd", "grain": "lots"}),
            ..theme
        };
        assert_eq!(
            Look::resolve(Some(&broken), true).extensions(Mode::Dark),
            Extensions::default()
        );
    }
}
