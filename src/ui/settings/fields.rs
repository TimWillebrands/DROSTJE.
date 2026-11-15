//! Shared form controls: choices, select and text fields, field errors, and the field
//! content every dialog and Settings group renders.
use super::{DialogKind, Field, SurfaceContent, appearance::missing_theme};
use crate::ui::theme;
use crate::{harness, titles};
use gpui_kit::{
    component::{
        Sizable, ThemeMode,
        alert::Alert,
        form::Field as FormField,
        input::{Input, InputState},
        searchable_list::SearchableListItem,
        select::{Select, SelectState},
        setting::{SettingField, SettingItem},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::fmt;

#[derive(Clone)]
pub(in crate::ui) struct Choice {
    pub(super) value: String,
    pub(super) label: SharedString,
    pub(super) unavailable: bool,
}

impl Choice {
    pub(super) fn new(value: impl Into<String>, label: impl Into<SharedString>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            unavailable: false,
        }
    }
}

impl SearchableListItem for Choice {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &String {
        &self.value
    }

    fn disabled(&self) -> bool {
        self.unavailable
    }
}

pub(super) struct SelectField {
    pub(super) key: &'static str,
    pub(super) label: &'static str,
    pub(super) state: Entity<SelectState<Vec<Choice>>>,
}

#[derive(Debug)]
pub(super) struct FieldError {
    pub(super) key: &'static str,
    message: String,
}

impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for FieldError {}

pub(super) fn field_error(key: &'static str, message: impl Into<String>) -> anyhow::Error {
    FieldError {
        key,
        message: message.into(),
    }
    .into()
}

/// The controls a form is built from, in display order.
#[derive(Default)]
pub(super) struct Controls {
    pub(super) fields: Vec<Field>,
    pub(super) selects: Vec<SelectField>,
}

impl Controls {
    pub(super) fn field(
        &mut self,
        key: &'static str,
        label: &'static str,
        value: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        let initial = value.clone();
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(value, window, cx);
            input
        });
        self.fields.push(Field {
            key,
            label,
            input,
            initial,
        });
    }

    pub(super) fn select(
        &mut self,
        key: &'static str,
        label: &'static str,
        choices: Vec<Choice>,
        selected: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.selects
            .push(select_field(key, label, choices, selected, window, cx));
    }
}

pub(super) fn select_field(
    key: &'static str,
    label: &'static str,
    choices: Vec<Choice>,
    selected: &str,
    window: &mut Window,
    cx: &mut App,
) -> SelectField {
    let state = cx.new(|cx| {
        let mut state = SelectState::new(choices, None, window, cx);
        state.set_selected_value(&selected.to_owned(), window, cx);
        state
    });
    SelectField { key, label, state }
}

impl SurfaceContent {
    pub(super) fn clear_field_error(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.error.is_some() && (self.error_field.is_none() || self.error_field == Some(key)) {
            self.error = None;
            self.error_field = None;
            cx.notify();
        }
    }

    pub(in crate::ui) fn value(&self, key: &str, cx: &App) -> String {
        if let Some(select) = self.select(key) {
            return select
                .read(cx)
                .selected_value()
                .cloned()
                .unwrap_or_default();
        }
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.input.read(cx).value().trim().to_string())
            .unwrap_or_default()
    }

    pub(in crate::ui) fn select(&self, key: &str) -> Option<&Entity<SelectState<Vec<Choice>>>> {
        self.selects
            .iter()
            .find(|field| field.key == key)
            .map(|field| &field.state)
    }

    pub fn set_error(&mut self, error: &anyhow::Error, cx: &mut Context<Self>) {
        self.error_field = error.downcast_ref::<FieldError>().map(|error| error.key);
        self.error = Some(format!("{error:#}"));
        cx.notify();
    }

    pub(super) fn field_content(
        &self,
        key: &str,
        readonly: bool,
        cx: &App,
    ) -> (&'static str, AnyElement) {
        let (label, control, field_key) =
            if let Some(field) = self.selects.iter().find(|field| field.key == key) {
                (
                    field.label,
                    Select::new(&field.state)
                        .disabled(readonly)
                        .into_any_element(),
                    field.key,
                )
            } else {
                let field = self
                    .fields
                    .iter()
                    .find(|field| field.key == key)
                    .expect("Settings field");
                (
                    field.label,
                    Input::new(&field.input)
                        .disabled(
                            readonly
                                || (key == "remote_host_id"
                                    && !self.value("remote_host", cx).is_empty())
                                || (key == "remote_project_key"
                                    && matches!(self.kind, DialogKind::Settings(None))),
                        )
                        .into_any_element(),
                    field.key,
                )
            };
        let warning = match key {
            "interactive_harness" => {
                let selected = self.value(key, cx);
                (!harness::CONVERSATION_HARNESSES.contains(&selected.as_str())).then(|| {
                    format!(
                        "Saved conversation harness {selected:?} is unsupported. Choose a supported harness."
                    )
                })
            }
            "factory_harness" => {
                let selected = self.value(key, cx);
                (!["opencode", "codex", "claude-code"].contains(&selected.as_str())).then(|| {
                    format!(
                        "Saved harness {selected:?} is unsupported. Choose a supported harness."
                    )
                })
            }
            titles::MODEL_SETTING => {
                let selected = self.value(key, cx);
                let installed = self
                    .state
                    .read(cx)
                    .title_models
                    .as_ref()
                    .and_then(|models| models.ollama.as_ref().ok());
                (!selected.is_empty() && installed.is_some_and(|models| !models.contains(&selected)))
                    .then(|| format!("Ollama has no model {selected}. Choose an installed model or pull it, then refresh models."))
            }
            "factory_target_branch" => {
                let selected = self.value(key, cx);
                let branches = match self.kind {
                    DialogKind::Settings(Some(project)) => self
                        .state
                        .read(cx)
                        .repositories
                        .get(&project)
                        .and_then(|repository| repository.as_ref().ok())
                        .map(|repository| &repository.branches),
                    _ => None,
                };
                (!selected.is_empty() && branches.is_some_and(|branches| !branches.contains(&selected)))
                    .then(|| format!("{selected} is not a local branch. Factory launches need one; choose a branch."))
            }
            "theme_light" | "theme_dark" => {
                let selected = self.value(key, cx);
                let mode = if key == "theme_light" {
                    ThemeMode::Light
                } else {
                    ThemeMode::Dark
                };
                (theme::initial_load_finished(cx) && missing_theme(self.theme_names.of(mode), &selected))
                    .then(|| format!("{selected} is unavailable for this mode. The registry default is used until its file is available."))
            }
            _ => None,
        };
        (
            label,
            v_flex()
                .id(field_key)
                .test_support()
                .key_context("NativeInput")
                .w_full()
                .gap_1()
                .child(control)
                .when(self.error_field == Some(key), |view| {
                    view.when_some(self.error.clone(), |view, error| {
                        view.child(
                            Alert::error(SharedString::from(format!("field-error-{key}")), error)
                                .small(),
                        )
                    })
                })
                .when_some(warning, |view, warning| {
                    view.child(
                        Alert::warning(SharedString::from(format!("field-warning-{key}")), warning)
                            .small(),
                    )
                })
                .into_any_element(),
        )
    }

    pub(super) fn form_field(&self, key: &str, readonly: bool, cx: &App) -> FormField {
        let (label, control) = self.field_content(key, readonly, cx);
        FormField::new().label(label).child(control)
    }

    pub(super) fn setting_item(
        &self,
        key: &'static str,
        group: &'static str,
        readonly: bool,
        owner: &WeakEntity<SurfaceContent>,
    ) -> Option<SettingItem> {
        let label = self
            .selects
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.label)
            .or_else(|| {
                self.fields
                    .iter()
                    .find(|field| field.key == key)
                    .map(|field| field.label)
            })?;
        let owner = owner.clone();
        Some(
            SettingItem::new(
                label,
                SettingField::render(move |options, _, cx| {
                    let Some(owner) = owner.upgrade() else {
                        return div().into_any_element();
                    };
                    owner
                        .read(cx)
                        .field_content(key, options.is_disabled(), cx)
                        .1
                }),
            )
            .keywords([key, group])
            .layout(Axis::Vertical)
            .disabled(readonly),
        )
    }
}

#[cfg(test)]
pub(super) fn shown(choices: &[Choice]) -> Vec<(&str, &str, bool)> {
    choices
        .iter()
        .map(|choice| {
            (
                choice.value.as_str(),
                choice.label.as_ref(),
                choice.unavailable,
            )
        })
        .collect()
}
