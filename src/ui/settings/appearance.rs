use super::{DialogKind, SurfaceContent, fields::Choice};
use crate::app::AppState;
use crate::ui::theme;
use gpui_kit::{
    component::{
        Sizable, ThemeMode,
        button::Button,
        setting::{SettingField, SettingGroup, SettingItem},
    },
    *,
};

impl SurfaceContent {
    pub(super) fn refresh_theme_choices(&mut self, window: &mut Window, cx: &mut App) {
        if !matches!(self.kind, DialogKind::Settings(_)) {
            return;
        }
        self.theme_names = ThemeNames::read(cx);
        for (key, mode) in [
            ("theme_light", ThemeMode::Light),
            ("theme_dark", ThemeMode::Dark),
        ] {
            let Some(select) = self.select(key) else {
                continue;
            };
            let selected = self.value(key, cx);
            let choices = theme_choices(self.theme_names.of(mode), &selected);
            select.update(cx, |select, cx| {
                select.set_items(choices, window, cx);
                select.set_selected_value(&selected, window, cx);
                cx.notify();
            });
        }
    }

    pub(super) fn appearance_group(
        &self,
        state: &AppState,
        readonly: bool,
        owner: &WeakEntity<SurfaceContent>,
    ) -> SettingGroup {
        let fields = |title: &'static str, keys: &[&'static str]| {
            self.group(title, keys, readonly, state, owner)
        };
        let theme_dir = state.data_dir.join("themes");
        fields("Appearance", &["theme_mode", "theme_light", "theme_dark"])
            .item(
                SettingItem::new(
                    "Theme files",
                    SettingField::render(move |_, _, _| {
                        let theme_dir = theme_dir.clone();
                        Button::new("open-theme-folder")
                            .small()
                            .label("Open folder")
                            .on_click(move |_, _, cx| cx.reveal_path(&theme_dir))
                    }),
                )
                .description(format!(
                    "Themes: {}. Add GPUI Kit ThemeSet JSON files with a themes array. Use unique theme names; duplicate names use the registry’s first entry.",
                    state.data_dir.join("themes").display()
                ))
                .keywords(["appearance", "theme", "folder"]),
            )
    }
}

#[derive(Default)]
pub(super) struct ThemeNames {
    light: Vec<String>,
    dark: Vec<String>,
}

impl ThemeNames {
    pub(super) fn read(cx: &App) -> Self {
        Self {
            light: theme::theme_names(ThemeMode::Light, cx),
            dark: theme::theme_names(ThemeMode::Dark, cx),
        }
    }

    pub(super) fn of(&self, mode: ThemeMode) -> &[String] {
        match mode {
            ThemeMode::Light => &self.light,
            ThemeMode::Dark => &self.dark,
        }
    }
}

pub(super) fn missing_theme(names: &[String], selected: &str) -> bool {
    !selected.is_empty() && !names.iter().any(|name| name == selected)
}

pub(super) fn theme_choices(names: &[String], selected: &str) -> Vec<Choice> {
    let mut choices = vec![Choice::new("", "Registry default")];
    choices.extend(
        names
            .iter()
            .map(|name| Choice::new(name.clone(), name.clone())),
    );
    if !choices.iter().any(|choice| choice.value == selected) {
        choices.push(Choice {
            value: selected.to_owned(),
            label: selected.to_owned().into(),
            unavailable: true,
        });
    }
    choices
}

#[cfg(test)]
mod tests {
    use super::{missing_theme, theme_choices};
    use crate::ui::settings::fields::shown;

    #[test]
    fn theme_choices_and_warning_agree_on_a_missing_saved_theme() {
        let names = ["Delft".to_owned(), "Dune".to_owned()];
        assert_eq!(
            shown(&theme_choices(&names, "Gone")),
            [
                ("", "Registry default", false),
                ("Delft", "Delft", false),
                ("Dune", "Dune", false),
                ("Gone", "Gone", true),
            ]
        );
        assert!(missing_theme(&names, "Gone"));
        assert_eq!(theme_choices(&names, "Dune").len(), 3);
        assert!(!missing_theme(&names, "Dune"));
        assert_eq!(theme_choices(&names, "").len(), 3);
        assert!(!missing_theme(&names, ""));
        assert!(missing_theme(&[], "Dune"));
    }
}
