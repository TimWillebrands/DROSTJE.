use super::{theme, workspace::Workspace};
use crate::{app::AppState, model::ModelKey, platform};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, StyledExt, ThemeMode, ThemeRegistry, WindowExt,
        button::{Button, ButtonVariants},
        form::{Field as FormField, Form},
        h_flex,
        input::{Input, InputState},
        searchable_list::SearchableListItem,
        select::{Select, SelectState},
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
                    ("omp", "OMP executable (blank: discover on PATH)"),
                    ("nvim", "Neovim executable (blank: discover on PATH)"),
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
        Self {
            kind,
            fields,
            error: None,
            error_field: None,
            selects,
            surface_id: Uuid::new_v4(),
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
                state.set_setting("executables", json!({ "omp": self.value("omp", cx), "nvim": self.value("nvim", cx), "factory": self.value("factory", cx) }));
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

    fn form_field(&self, key: &str, readonly: bool, cx: &App) -> FormField {
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
        FormField::new().label(label).child(
            v_flex()
                .key_context("NativeInput")
                .w_full()
                .gap_1()
                .child(control)
                .when(self.error_field == Some(key), |view| {
                    view.when_some(self.error.clone(), |view, error| {
                        view.child(div().text_sm().text_color(cx.theme().danger).child(error))
                    })
                })
                .when_some(warning, |view, warning| {
                    view.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().warning)
                            .child(warning),
                    )
                }),
        )
    }

    fn body(&self, state: &AppState, owner: &WeakEntity<Workspace>, cx: &App) -> AnyElement {
        let readonly = state.storage_error.is_some();
        let mut body = v_flex().id("dialog-body").w_full().gap_5().pb_4();
        match self.kind {
            DialogKind::Settings(project_id) => {
                let theme_dir = state.data_dir.join("themes");
                let open_dir = theme_dir.clone();
                body = body
                    .child(section("Appearance")
                        .child(Form::new()
                            .child(self.form_field("theme_mode", readonly, cx))
                            .child(self.form_field("theme_light", readonly, cx))
                            .child(self.form_field("theme_dark", readonly, cx)))
                        .child(div().text_sm().text_color(cx.theme().muted_foreground)
                            .child(format!("Themes: {}", theme_dir.display())))
                        .child(div().text_sm().text_color(cx.theme().muted_foreground)
                            .child("Add GPUI Kit ThemeSet JSON files with a themes array. Use unique theme names; duplicate names use the registry’s first entry."))
                        .child(Button::new("open-theme-folder").small().label("Open folder")
                            .on_click(move |_, _, cx| { cx.reveal_path(&open_dir); })))
                    .child(section("Tools")
                        .child(Form::new()
                            .child(self.form_field("omp", readonly, cx))
                            .child(self.form_field("nvim", readonly, cx))
                            .child(self.form_field("factory", readonly, cx)))
                        .child(div().text_sm().text_color(cx.theme().muted_foreground)
                            .child("Drost does not install, authenticate, or upgrade tools.")));
                let mut factory = section("Factory defaults")
                    .child(Form::new().child(self.form_field("factory_data_root", readonly, cx)));
                if self.select("factory_harness").is_some() {
                    factory = factory.child(
                        Form::new()
                            .child(self.form_field("factory_harness", readonly, cx))
                            .child(self.form_field("factory_model", readonly, cx))
                            .child(self.form_field("factory_thought_level", readonly, cx))
                            .child(self.form_field("factory_target_branch", readonly, cx)),
                    );
                }
                body = body.child(factory);
                let project = project_id.and_then(|id| {
                    state
                        .snapshot
                        .projects
                        .iter()
                        .find(|project| project.id == id)
                });
                let mut defaults = section("Project defaults");
                if let Some(project) = project {
                    defaults = defaults.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{}{}",
                                project.name,
                                if project.archived {
                                    " (archived, read-only)"
                                } else {
                                    ""
                                }
                            )),
                    );
                }
                defaults = if self.fields.iter().any(|field| field.key == "provider") {
                    defaults.child(
                        Form::new()
                            .child(self.form_field("provider", readonly, cx))
                            .child(self.form_field("model", readonly, cx)),
                    )
                } else {
                    defaults.child(div().text_sm().text_color(cx.theme().muted_foreground)
                        .child("Select an active project to edit its conversation and Factory defaults."))
                };
                body = body.child(defaults);
                let mut diagnostics = section("Diagnostics")
                    .child(div().text_sm().text_color(cx.theme().muted_foreground)
                        .child(format!("Application data: {}", state.data_dir.display())))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground)
                        .child("OMP and Neovim run with local user privileges. Factory uses its configured sandbox policy."));
                for name in ["omp", "nvim", "factory"] {
                    diagnostics = diagnostics.child(
                        v_flex()
                            .gap_1()
                            .child(div().text_sm().font_medium().child(name.to_uppercase()))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        state
                                            .diagnostics
                                            .get(name)
                                            .cloned()
                                            .unwrap_or_else(|| "Not inspected".into()),
                                    ),
                            ),
                    );
                }
                let owner = owner.clone();
                body = body.child(
                    diagnostics.child(
                        Button::new("refresh-diagnostics")
                            .small()
                            .label("Refresh diagnostics")
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |workspace, cx| {
                                    workspace.state.read(cx).refresh_diagnostics()
                                });
                            }),
                    ),
                );
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
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
        })
        .when(readonly, |view| {
            view.child(div().text_sm().text_color(cx.theme().danger).child(
                "Storage is not writable. Changes are disabled; existing drafts remain in memory.",
            ))
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

fn section(title: &'static str) -> Div {
    v_flex()
        .w_full()
        .gap_3()
        .child(div().font_semibold().child(title))
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
                        .size((window.rem_size() * 42.).min(window.viewport_size().width))
                        .resizable(false)
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
