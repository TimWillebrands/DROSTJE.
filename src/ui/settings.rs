use super::{theme, workspace::Workspace};
use crate::{app::AppState, model::ModelKey, platform};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, ThemeMode, ThemeRegistry, WindowExt,
        alert::Alert,
        button::{Button, ButtonVariants},
        form::{Field as FormField, Form},
        h_flex,
        input::{Input, InputState},
        searchable_list::SearchableListItem,
        select::{Select, SelectState},
        setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::json;
use std::{fmt, path::PathBuf};
use uuid::Uuid;

#[derive(Clone)]
pub(super) struct Choice {
    value: String,
    label: SharedString,
    unavailable: bool,
}

impl Choice {
    fn new(value: impl Into<String>, label: impl Into<SharedString>) -> Self {
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

struct SelectField {
    key: &'static str,
    label: &'static str,
    state: Entity<SelectState<Vec<Choice>>>,
}

#[derive(Debug)]
struct FieldError {
    key: &'static str,
    message: String,
}

impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for FieldError {}

fn field_error(key: &'static str, message: impl Into<String>) -> anyhow::Error {
    FieldError {
        key,
        message: message.into(),
    }
    .into()
}

#[derive(Clone)]
pub enum DialogKind {
    AddProject,
    RenameProject(Uuid),
    RenameThread(Uuid),
    ArchiveProject(Uuid),
    ArchiveThread(Uuid),
    Settings(Option<Uuid>),
}
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub input: Entity<InputState>,
}
pub struct Dialog {
    pub kind: DialogKind,
    pub fields: Vec<Field>,
    pub error: Option<String>,
    error_field: Option<&'static str>,
    selects: Vec<SelectField>,
    surface_id: Uuid,
    settings_id: SharedString,
    hosted: bool,
}
impl Dialog {
    pub fn new(kind: DialogKind, state: &AppState, window: &mut Window, cx: &mut App) -> Self {
        let mut selects = Vec::new();
        let mut fields = Vec::new();
        let mut field = |key, label, value: String| {
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_value(value, window, cx);
                input
            });
            fields.push(Field { key, label, input });
        };
        match &kind {
            DialogKind::AddProject => field("path", "Existing Git working tree", String::new()),
            DialogKind::RenameProject(id) => field(
                "name",
                "Project name",
                state
                    .snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == *id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default(),
            ),
            DialogKind::RenameThread(id) => field(
                "name",
                "Thread title",
                state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == *id)
                    .map(|t| t.title.clone())
                    .unwrap_or_default(),
            ),
            DialogKind::Settings(project) => {
                for (key, label) in [
                    ("nvim", "Neovim executable (blank: discover on PATH)"),
                    ("omp", "OMP executable (blank: discover on PATH)"),
                    ("factory", "Factory executable (blank: discover on PATH)"),
                ] {
                    field(
                        key,
                        label,
                        state
                            .snapshot
                            .settings
                            .get("executables")
                            .and_then(|v| v.get(key))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                    );
                }
                field(
                    "factory_data_root",
                    "Shared Factory data root (absolute path)",
                    state
                        .snapshot
                        .settings
                        .get("factory_data_root")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .into(),
                );
                if let Some(project) = project.and_then(|id| {
                    state
                        .snapshot
                        .projects
                        .iter()
                        .find(|p| p.id == id && !p.archived)
                }) {
                    field(
                        "provider",
                        "Interactive default provider (blank: harness default)",
                        project
                            .defaults
                            .interactive_model
                            .as_ref()
                            .map(|m| m.provider.clone())
                            .unwrap_or_default(),
                    );
                    field(
                        "model",
                        "Interactive default model ID",
                        project
                            .defaults
                            .interactive_model
                            .as_ref()
                            .map(|m| m.id.clone())
                            .unwrap_or_default(),
                    );
                    field(
                        "factory_model",
                        "Factory model (blank: harness default)",
                        project.defaults.factory_model.clone().unwrap_or_default(),
                    );
                    field(
                        "factory_thought_level",
                        "Factory thought level (optional)",
                        project
                            .defaults
                            .factory_thought_level
                            .clone()
                            .unwrap_or_default(),
                    );
                    field(
                        "factory_target_branch",
                        "Factory target branch (blank: Git branch)",
                        project
                            .defaults
                            .factory_target_branch
                            .clone()
                            .unwrap_or_default(),
                    );
                }
                let mode = state
                    .snapshot
                    .settings
                    .get("theme_mode")
                    .and_then(|value| value.as_str())
                    .filter(|mode| ["system", "light", "dark"].contains(mode))
                    .unwrap_or("system");
                selects.push(select_field(
                    "theme_mode",
                    "Appearance",
                    vec![
                        Choice::new("system", "System"),
                        Choice::new("light", "Light"),
                        Choice::new("dark", "Dark"),
                    ],
                    mode,
                    window,
                    cx,
                ));
                for (key, label, mode) in [
                    ("theme_light", "Light theme", ThemeMode::Light),
                    ("theme_dark", "Dark theme", ThemeMode::Dark),
                ] {
                    let selected = state
                        .snapshot
                        .settings
                        .get(key)
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    selects.push(select_field(
                        key,
                        label,
                        theme_choices(mode, selected, cx),
                        selected,
                        window,
                        cx,
                    ));
                }
                if let Some(project) = project.and_then(|id| {
                    state
                        .snapshot
                        .projects
                        .iter()
                        .find(|project| project.id == id && !project.archived)
                }) {
                    let selected = &project.defaults.factory_harness;
                    let mut choices = ["opencode", "codex", "claude-code"]
                        .into_iter()
                        .map(|name| Choice::new(name, name))
                        .collect::<Vec<_>>();
                    if !choices.iter().any(|choice| &choice.value == selected) {
                        choices.push(Choice {
                            value: selected.clone(),
                            label: format!("{selected} (unsupported)").into(),
                            unavailable: true,
                        });
                    }
                    selects.push(select_field(
                        "factory_harness",
                        "Harness",
                        choices,
                        selected,
                        window,
                        cx,
                    ));
                }
            }
            _ => {}
        }
        let surface_id = Uuid::new_v4();
        Self {
            kind,
            fields,
            error: None,
            error_field: None,
            selects,
            surface_id,
            settings_id: format!("drost-settings-{surface_id}").into(),
            hosted: false,
        }
    }
    pub(super) fn value(&self, key: &str, cx: &App) -> String {
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

    pub(super) fn select(&self, key: &str) -> Option<&Entity<SelectState<Vec<Choice>>>> {
        self.selects
            .iter()
            .find(|field| field.key == key)
            .map(|field| &field.state)
    }

    pub fn set_error(&mut self, error: &anyhow::Error) {
        self.error_field = error.downcast_ref::<FieldError>().map(|error| error.key);
        self.error = Some(format!("{error:#}"));
    }
    pub fn apply(&self, state: &mut AppState, cx: &App) -> anyhow::Result<bool> {
        if let Some(error) = &state.storage_error {
            anyhow::bail!("Storage is not writable: {error}");
        }
        match self.kind {
            DialogKind::AddProject => {
                let path = self.value("path", cx);
                if path.is_empty() {
                    return Err(field_error(
                        "path",
                        "Choose or enter an existing Git working tree.",
                    ));
                }
                let path = if let Some(suffix) = path.strip_prefix("~/") {
                    PathBuf::from(
                        std::env::var_os("HOME")
                            .ok_or_else(|| anyhow::anyhow!("HOME is unavailable"))?,
                    )
                    .join(suffix)
                } else {
                    PathBuf::from(path)
                };
                state.register_project(path);
            }
            DialogKind::RenameProject(id) => {
                let name = self.value("name", cx);
                if name.is_empty() {
                    return Err(field_error("name", "Project name cannot be blank."));
                }
                let mut project = state
                    .snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("Project is unavailable"))?;
                anyhow::ensure!(!project.archived, "Archived projects are read-only.");
                project.name = name;
                state.save_project(project);
            }
            DialogKind::RenameThread(id) => {
                let title = self.value("name", cx);
                if title.is_empty() {
                    return Err(field_error("name", "Thread title cannot be blank."));
                }
                let mut thread = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("Thread is unavailable"))?;
                anyhow::ensure!(!thread.archived, "Archived threads are read-only.");
                thread.title = title;
                thread.updated_at = platform::now();
                state.save_thread(thread);
            }
            DialogKind::ArchiveProject(id) => {
                anyhow::ensure!(
                    state.selected_project == Some(id),
                    "Project selection changed; reopen Archive Project."
                );
                if let Some(reason) = state.archive_project_reason() {
                    anyhow::bail!(reason);
                }
                let mut project = state
                    .project()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("Select a project"))?;
                project.archived = true;
                state.save_project(project);
                state.show_archived = true;
                state.set_setting("show_archived", json!(true));
            }
            DialogKind::ArchiveThread(id) => {
                anyhow::ensure!(
                    state.selected_thread == Some(id),
                    "Thread selection changed; reopen Archive thread."
                );
                if let Some(reason) = state.archive_thread_reason() {
                    anyhow::bail!(reason);
                }
                let mut thread = state
                    .thread()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("Select a thread"))?;
                thread.archived = true;
                thread.updated_at = platform::now();
                state.save_thread(thread);
                state.show_archived = true;
                state.set_setting("show_archived", json!(true));
            }
            DialogKind::Settings(project_id) => {
                let root = self.value("factory_data_root", cx);
                if !PathBuf::from(&root).is_absolute() {
                    return Err(field_error(
                        "factory_data_root",
                        "Factory data root must be an absolute path.",
                    ));
                }
                let mut project = project_id.and_then(|id| {
                    state
                        .snapshot
                        .projects
                        .iter()
                        .find(|p| p.id == id && !p.archived)
                        .cloned()
                });
                if let Some(project) = &mut project {
                    let provider = self.value("provider", cx);
                    let model = self.value("model", cx);
                    if provider.is_empty() != model.is_empty() {
                        return Err(field_error(
                            if provider.is_empty() {
                                "provider"
                            } else {
                                "model"
                            },
                            "Set both interactive provider and model ID, or leave both blank.",
                        ));
                    }
                    let harness = self.value("factory_harness", cx);
                    if !["opencode", "codex", "claude-code"].contains(&harness.as_str()) {
                        return Err(field_error(
                            "factory_harness",
                            "Choose opencode, codex, or claude-code.",
                        ));
                    }
                    project.defaults.interactive_model = if model.is_empty() {
                        None
                    } else {
                        Some(ModelKey {
                            provider,
                            id: model,
                        })
                    };
                    project.defaults.factory_harness = harness;
                    let optional = |key| {
                        let value = self.value(key, cx);
                        if value.is_empty() { None } else { Some(value) }
                    };
                    project.defaults.factory_model = optional("factory_model");
                    project.defaults.factory_thought_level = optional("factory_thought_level");
                    project.defaults.factory_target_branch = optional("factory_target_branch");
                }
                let mode = self.value("theme_mode", cx);
                let light = self.value("theme_light", cx);
                let dark = self.value("theme_dark", cx);
                let named = |value: String| (!value.is_empty()).then_some(value);
                let light = named(light);
                let dark = named(dark);
                theme::validate_preferences(&mode, None, None, cx)
                    .map_err(|error| field_error("theme_mode", format!("{error:#}")))?;
                theme::validate_preferences(&mode, light.as_deref(), None, cx)
                    .map_err(|error| field_error("theme_light", format!("{error:#}")))?;
                theme::validate_preferences(&mode, None, dark.as_deref(), cx)
                    .map_err(|error| field_error("theme_dark", format!("{error:#}")))?;
                state.set_setting(
                    "executables",
                    json!({ "nvim": self.value("nvim", cx), "omp": self.value("omp", cx), "factory": self.value("factory", cx) }),
                );
                state.set_setting("factory_data_root", json!(root));
                state.set_setting("theme_mode", json!(mode));
                state.set_setting("theme_light", json!(light));
                state.set_setting("theme_dark", json!(dark));
                if let Some(project) = project {
                    state.save_project(project);
                }
                state.refresh_diagnostics();
            }
        }
        Ok(true)
    }
    fn title(&self) -> &'static str {
        match self.kind {
            DialogKind::AddProject => "Add project",
            DialogKind::RenameProject(_) => "Rename project",
            DialogKind::RenameThread(_) => "Rename thread",
            DialogKind::ArchiveProject(_) => "Archive project",
            DialogKind::ArchiveThread(_) => "Archive thread",
            DialogKind::Settings(_) => "Settings",
        }
    }

    fn refresh_theme_choices(&self, window: &mut Window, cx: &mut App) {
        for (key, mode) in [
            ("theme_light", ThemeMode::Light),
            ("theme_dark", ThemeMode::Dark),
        ] {
            let Some(select) = self.select(key) else {
                continue;
            };
            let selected = self.value(key, cx);
            let choices = theme_choices(mode, &selected, cx);
            select.update(cx, |select, cx| {
                select.set_items(choices, window, cx);
                select.set_selected_value(&selected, window, cx);
                cx.notify();
            });
        }
    }

    fn field_content(&self, key: &str, readonly: bool, cx: &App) -> (&'static str, AnyElement) {
        let (label, control) =
            if let Some(field) = self.selects.iter().find(|field| field.key == key) {
                (
                    field.label,
                    Select::new(&field.state)
                        .disabled(readonly)
                        .into_any_element(),
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
                        .disabled(readonly)
                        .into_any_element(),
                )
            };
        let warning = match key {
            "factory_harness" => {
                let selected = self.value(key, cx);
                (!["opencode", "codex", "claude-code"].contains(&selected.as_str())).then(|| {
                    format!(
                        "Saved harness {selected:?} is unsupported. Choose a supported harness."
                    )
                })
            }
            "theme_light" | "theme_dark" => {
                let selected = self.value(key, cx);
                let mode = if key == "theme_light" {
                    ThemeMode::Light
                } else {
                    ThemeMode::Dark
                };
                (theme::initial_load_finished(cx) && !selected.is_empty() && !theme::theme_names(mode, cx).contains(&selected))
                    .then(|| format!("{selected} is unavailable for this mode. The registry default is used until its file is available."))
            }
            _ => None,
        };
        (
            label,
            v_flex()
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

    fn form_field(&self, key: &str, readonly: bool, cx: &App) -> FormField {
        let (label, control) = self.field_content(key, readonly, cx);
        FormField::new().label(label).child(control)
    }

    fn setting_item(
        &self,
        key: &'static str,
        group: &'static str,
        readonly: bool,
        owner: &WeakEntity<Workspace>,
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
        let id = self.surface_id;
        Some(
            SettingItem::new(
                label,
                SettingField::render(move |options, _, cx| {
                    let Some(owner) = owner.upgrade() else {
                        return div().into_any_element();
                    };
                    let workspace = owner.read(cx);
                    let Some(dialog) = workspace
                        .dialog
                        .as_ref()
                        .filter(|dialog| dialog.surface_id == id)
                    else {
                        return div().into_any_element();
                    };
                    dialog.field_content(key, options.is_disabled(), cx).1
                }),
            )
            .keywords([key, group])
            .layout(Axis::Vertical)
            .disabled(readonly),
        )
    }

    fn settings_body(
        &self,
        project_id: Option<Uuid>,
        state: &AppState,
        owner: &WeakEntity<Workspace>,
        _: &App,
    ) -> AnyElement {
        let readonly = state.storage_error.is_some();
        let fields = |title: &'static str, keys: &[&'static str]| {
            SettingGroup::new().title(title).items(
                keys.iter()
                    .filter_map(|key| self.setting_item(key, title, readonly, owner)),
            )
        };
        let theme_dir = state.data_dir.join("themes");
        let appearance = fields("Appearance", &["theme_mode", "theme_light", "theme_dark"])
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
            );
        let tools = fields("Tools", &["nvim", "omp", "factory"])
            .description("Drost does not install, authenticate, or upgrade tools.");
        let factory = fields(
            "Factory defaults",
            &[
                "factory_data_root",
                "factory_harness",
                "factory_model",
                "factory_thought_level",
                "factory_target_branch",
            ],
        );
        let mut project = fields("Project defaults", &["provider", "model"]);
        if let Some(selected) = project_id.and_then(|id| {
            state
                .snapshot
                .projects
                .iter()
                .find(|project| project.id == id)
        }) {
            project = project.description(format!(
                "{}{}",
                selected.name,
                if selected.archived {
                    " (archived, read-only)"
                } else {
                    ""
                }
            ));
        }
        if !self.fields.iter().any(|field| field.key == "provider") {
            project = project.item(
                SettingItem::render(|_, _, cx| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Select an active project to edit its conversation and Factory defaults.")
                })
                .keywords(["project", "defaults", "conversation", "factory", "archived"]),
            );
        }
        let mut diagnostics = SettingGroup::new()
            .title("Diagnostics")
            .description(format!(
                "Application data: {}. OMP runs with local user privileges. Factory uses its configured sandbox policy.",
                state.data_dir.display()
            ));
        for (name, title) in [("nvim", "Neovim"), ("omp", "OMP"), ("factory", "Factory")] {
            let status = state
                .diagnostics
                .get(name)
                .cloned()
                .unwrap_or_else(|| "Not inspected".into());
            let owner = owner.clone();
            diagnostics = diagnostics.item(
                SettingItem::new(
                    title,
                    SettingField::render(move |_, _, _| {
                        let owner = owner.clone();
                        Button::new(SharedString::from(format!("refresh-diagnostics-{name}")))
                            .small()
                            .label("Refresh diagnostics")
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |workspace, cx| {
                                    workspace.state.read(cx).refresh_diagnostics()
                                });
                            })
                    }),
                )
                .description(status)
                .keywords([name, "diagnostics", "setup", "retry"]),
            );
        }
        // Settings retains search/navigation internally; draft controls remain owned by Dialog.
        let settings = Settings::new(self.settings_id.clone()).page(
            SettingPage::new("Settings")
                .default_open(true)
                .resettable(false)
                .groups([appearance, tools, factory, project, diagnostics]),
        );
        v_flex()
            .id("dialog-body")
            .key_context("NativeInput")
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .child(div().flex_1().min_h_0().min_w_0().child(settings))
            // Search may hide the invalid field, so keep its validation result visible.
            .when_some(self.error.clone(), |view, error| {
                view.child(Alert::error("settings-validation", error).small())
            })
            .when(readonly, |view| {
                view.child(Alert::error("settings-storage-error",
                    "Storage is not writable. Changes are disabled; existing drafts remain in memory.").small())
            })
            .into_any_element()
    }

    fn body(&self, state: &AppState, owner: &WeakEntity<Workspace>, cx: &App) -> AnyElement {
        let readonly = state.storage_error.is_some();
        let mut body = v_flex().id("dialog-body").w_full().gap_5().pb_4();
        match self.kind {
            DialogKind::Settings(project_id) => {
                return self.settings_body(project_id, state, owner, cx);
            }
            DialogKind::AddProject => {
                let owner = owner.clone();
                body = body
                    .child(div().text_sm().text_color(cx.theme().muted_foreground)
                        .child("Register a working tree. Drost does not clone, modify, or delete its files."))
                    .child(Form::new().child(self.form_field("path", readonly, cx)))
                    .child(Button::new("choose-project-folder").small().disabled(readonly)
                        .label("Choose folder…").on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |workspace, cx| workspace.pick_project_folder(window, cx));
                        }));
            }
            DialogKind::ArchiveProject(_) | DialogKind::ArchiveThread(_) => {
                body = body.child("Archive retains threads, drafts, sessions, and Factory history. It never deletes checkout, session, or Factory files. Archived history remains accessible from Projects.");
            }
            DialogKind::RenameProject(_) | DialogKind::RenameThread(_) => {
                body = body.child(Form::new().child(self.form_field("name", readonly, cx)));
            }
        }
        body.when(self.error_field.is_none(), |view| {
            view.when_some(self.error.clone(), |view, error| {
                view.child(Alert::error("form-validation", error).small())
            })
        })
        .when(readonly, |view| {
            view.child(Alert::error("form-storage-error",
                "Storage is not writable. Changes are disabled; existing drafts remain in memory.").small())
        })
        .into_any_element()
    }
}

fn select_field(
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

fn theme_choices(mode: ThemeMode, selected: &str, cx: &App) -> Vec<Choice> {
    let mut choices = vec![Choice::new("", "Registry default")];
    choices.extend(
        theme::theme_names(mode, cx)
            .into_iter()
            .map(|name| Choice::new(name.clone(), name)),
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

struct SurfaceContent {
    owner: WeakEntity<Workspace>,
    id: Uuid,
    _subscriptions: Vec<Subscription>,
}

impl Render for SurfaceContent {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div().into_any_element();
        };
        let workspace = owner.read(cx);
        let Some(dialog) = workspace
            .dialog
            .as_ref()
            .filter(|dialog| dialog.surface_id == self.id)
        else {
            return div().into_any_element();
        };
        let owner = self.owner.clone();
        let id = self.id;
        div()
            .w_full()
            .when(matches!(dialog.kind, DialogKind::Settings(_)), |view| {
                view.size_full().min_h_0().min_w_0().overflow_hidden()
            })
            .key_context("DrostForm")
            .on_action(move |_: &super::commands::Confirm, window, cx| {
                cx.stop_propagation();
                confirm_surface(&owner, id, window, cx);
            })
            .child(dialog.body(workspace.state.read(cx), &self.owner, cx))
            .into_any_element()
    }
}

fn footer(owner: &WeakEntity<Workspace>, id: Uuid, cx: &App) -> AnyElement {
    let Some(workspace) = owner.upgrade() else {
        return div().into_any_element();
    };
    let workspace = workspace.read(cx);
    let Some(dialog) = workspace
        .dialog
        .as_ref()
        .filter(|dialog| dialog.surface_id == id)
    else {
        return div().into_any_element();
    };
    let readonly = workspace.state.read(cx).storage_error.is_some();
    let archive = matches!(
        dialog.kind,
        DialogKind::ArchiveProject(_) | DialogKind::ArchiveThread(_)
    );
    let cancel = owner.clone();
    let confirm = owner.clone();
    h_flex()
        .w_full()
        .justify_end()
        .gap_2()
        .child(
            Button::new("dialog-cancel")
                .ghost()
                .label("Cancel")
                .on_click(move |_, window, cx| {
                    let _ = cancel.update(cx, |workspace, cx| {
                        if workspace
                            .dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.surface_id == id)
                        {
                            workspace.dismiss(window, cx);
                        }
                    });
                }),
        )
        .child(
            Button::new("dialog-confirm")
                .primary()
                .disabled(readonly)
                .label(if archive {
                    "Archive"
                } else if matches!(dialog.kind, DialogKind::AddProject) {
                    "Add project"
                } else {
                    "Save"
                })
                .on_click(move |_, window, cx| {
                    confirm_surface(&confirm, id, window, cx);
                }),
        )
        .into_any_element()
}

fn confirm_surface(owner: &WeakEntity<Workspace>, id: Uuid, window: &mut Window, cx: &mut App) {
    let _ = owner.update(cx, |workspace, cx| {
        if workspace
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.surface_id == id)
        {
            workspace.confirm_dialog(window, cx);
        }
    });
}

fn clear_surface(owner: &WeakEntity<Workspace>, id: Uuid, cx: &mut App) {
    let _ = owner.update(cx, |workspace, cx| {
        if workspace
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.surface_id == id)
        {
            workspace.dialog = None;
            cx.notify();
        }
    });
}

/// The Root captures trigger focus and owns trapping, dismissal, and one focus restoration.
pub(super) fn open_surface(owner: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| {
        let Ok(Some((id, kind, title, state, selects))) = owner.update(cx, |workspace, _| {
            let dialog = workspace.dialog.as_mut().filter(|dialog| !dialog.hosted)?;
            dialog.hosted = true;
            Some((
                dialog.surface_id,
                dialog.kind.clone(),
                dialog.title(),
                workspace.state.clone(),
                dialog
                    .selects
                    .iter()
                    .map(|field| field.state.clone())
                    .collect::<Vec<_>>(),
            ))
        }) else {
            return;
        };
        let Some(workspace) = owner.upgrade() else {
            return;
        };
        let content = cx.new(|cx| {
            let mut subscriptions = vec![
                cx.observe(&workspace, |_, _, cx| cx.notify()),
                cx.observe(&state, |_, _, cx| cx.notify()),
                cx.observe_global_in::<ThemeRegistry>(
                    window,
                    |this: &mut SurfaceContent, window, cx| {
                        let _ = this.owner.update(cx, |workspace, cx| {
                            if let Some(dialog) = workspace
                                .dialog
                                .as_ref()
                                .filter(|dialog| dialog.surface_id == this.id)
                            {
                                dialog.refresh_theme_choices(window, cx);
                            }
                        });
                        cx.notify();
                    },
                ),
            ];
            for select in &selects {
                subscriptions.push(cx.observe(select, |_, _, cx| cx.notify()));
            }
            SurfaceContent {
                owner: owner.clone(),
                id,
                _subscriptions: subscriptions,
            }
        });
        let host_owner = owner.clone();
        match kind {
            DialogKind::Settings(_) => {
                window.open_sheet(cx, move |sheet, window, cx| {
                    let close = host_owner.clone();
                    sheet
                        .title(title)
                        .size((window.rem_size() * 56.).min(window.viewport_size().width))
                        .resizable(false)
                        .px_0()
                        .child(content.clone())
                        .footer(footer(&host_owner, id, cx))
                        .on_close(move |_, _, cx| clear_surface(&close, id, cx))
                });
            }
            DialogKind::ArchiveProject(_) | DialogKind::ArchiveThread(_) => {
                window.open_alert_dialog(cx, move |alert, window, cx| {
                    let close = host_owner.clone();
                    let confirm = host_owner.clone();
                    alert
                        .title(title)
                        .width((window.rem_size() * 30.).min(window.viewport_size().width))
                        .child(content.clone())
                        .footer(footer(&host_owner, id, cx))
                        .on_ok(move |_, window, cx| {
                            confirm_surface(&confirm, id, window, cx);
                            false
                        })
                        .on_close(move |_, _, cx| clear_surface(&close, id, cx))
                });
            }
            _ => {
                window.open_dialog(cx, move |dialog, window, cx| {
                    let close = host_owner.clone();
                    let confirm = host_owner.clone();
                    dialog
                        .title(title)
                        .width((window.rem_size() * 30.).min(window.viewport_size().width))
                        .child(content.clone())
                        .footer(footer(&host_owner, id, cx))
                        .on_ok(move |_, window, cx| {
                            confirm_surface(&confirm, id, window, cx);
                            false
                        })
                        .on_close(move |_, _, cx| clear_surface(&close, id, cx))
                });
            }
        }
        window.on_next_frame(move |window, cx| {
            let Some(owner) = owner.upgrade() else { return };
            let workspace = owner.read(cx);
            let Some(dialog) = workspace
                .dialog
                .as_ref()
                .filter(|dialog| dialog.surface_id == id)
            else {
                return;
            };
            let focus = if let Some(select) = dialog.select("theme_mode") {
                Some(select.focus_handle(cx))
            } else {
                dialog
                    .fields
                    .first()
                    .map(|field| field.input.focus_handle(cx))
            };
            if let Some(focus) = focus {
                focus.focus(window, cx);
            }
        });
    });
}

pub(super) fn close_surface(kind: &DialogKind, window: &mut Window, cx: &mut App) {
    if matches!(kind, DialogKind::Settings(_)) {
        window.close_sheet(cx);
    } else {
        window.close_dialog(cx);
    }
}
