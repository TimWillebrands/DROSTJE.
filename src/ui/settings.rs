use super::workspace::Workspace;
use crate::{app::AppState, model::ModelKey, platform};
use gpui_kit::component::Disableable;
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::json;
use std::path::PathBuf;
use uuid::Uuid;

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
    pub focus: FocusHandle,
    pub previous_focus: Option<FocusHandle>,
}
impl Dialog {
    pub fn new(kind: DialogKind, state: &AppState, window: &mut Window, cx: &mut App) -> Self {
        let previous_focus = window.focused(cx);
        let focus = cx.focus_handle();
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
                "Conversation title",
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
                        "factory_harness",
                        "Factory harness: omp / opencode / codex / claude-code",
                        project.defaults.factory_harness.clone(),
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
            }
            _ => {}
        }
        if let Some(field) = fields.first() {
            field.input.focus_handle(cx).focus(window, cx);
        } else {
            focus.focus(window, cx);
        }
        Self {
            kind,
            fields,
            error: None,
            focus,
            previous_focus,
        }
    }
    fn value(&self, key: &str, cx: &App) -> String {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .map(|f| f.input.read(cx).value().trim().to_string())
            .unwrap_or_default()
    }
    pub fn apply(&self, state: &mut AppState, cx: &App) -> anyhow::Result<bool> {
        if let Some(error) = &state.storage_error {
            anyhow::bail!("Storage is not writable: {error}");
        }
        match self.kind {
            DialogKind::AddProject => {
                let path = self.value("path", cx);
                anyhow::ensure!(
                    !path.is_empty(),
                    "Choose or enter an existing Git working tree."
                );
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
                anyhow::ensure!(!name.is_empty(), "Project name cannot be blank.");
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
                anyhow::ensure!(!title.is_empty(), "Conversation title cannot be blank.");
                let mut thread = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("Conversation is unavailable"))?;
                anyhow::ensure!(!thread.archived, "Archived conversations are read-only.");
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
                    "Conversation selection changed; reopen Archive Conversation."
                );
                if let Some(reason) = state.archive_thread_reason() {
                    anyhow::bail!(reason);
                }
                let mut thread = state
                    .thread()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("Select a conversation"))?;
                thread.archived = true;
                thread.updated_at = platform::now();
                state.save_thread(thread);
                state.show_archived = true;
                state.set_setting("show_archived", json!(true));
            }
            DialogKind::Settings(project_id) => {
                let root = self.value("factory_data_root", cx);
                anyhow::ensure!(
                    PathBuf::from(&root).is_absolute(),
                    "Factory data root must be an absolute path."
                );
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
                    anyhow::ensure!(
                        provider.is_empty() == model.is_empty(),
                        "Set both interactive provider and model ID, or leave both blank."
                    );
                    let harness = self.value("factory_harness", cx);
                    anyhow::ensure!(
                        ["omp", "opencode", "codex", "claude-code"].contains(&harness.as_str()),
                        "Choose one of the four supported Factory harness IDs."
                    );
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
                state.set_setting("executables", json!({ "omp": self.value("omp", cx), "nvim": self.value("nvim", cx), "factory": self.value("factory", cx) }));
                state.set_setting("factory_data_root", json!(root));
                if let Some(project) = project {
                    state.save_project(project);
                }
                state.refresh_diagnostics();
            }
        }
        Ok(true)
    }
    pub fn render(
        &self,
        state: &AppState,
        owner: &WeakEntity<Workspace>,
        window: &Window,
    ) -> AnyElement {
        let title = match &self.kind {
            DialogKind::AddProject => "Add Project",
            DialogKind::RenameProject(_) => "Rename Project",
            DialogKind::RenameThread(_) => "Rename Conversation",
            DialogKind::ArchiveProject(_) => "Archive Project",
            DialogKind::ArchiveThread(_) => "Archive Conversation",
            DialogKind::Settings(_) => "Settings",
        };
        let mut body = v_flex()
            .id("dialog-body")
            .overflow_y_scroll()
            .max_h((window.viewport_size().height - px(240.)).max(px(180.)))
            .gap_3();
        match self.kind {
            DialogKind::AddProject => { let owner = owner.clone(); body = body.child("Register a working tree; Drost does not clone, modify, or delete its files.").child(Button::new("choose-project-folder").small().label("Choose Folder…").on_click(move |_, window, cx| { let _ = owner.update(cx, |workspace, cx| workspace.pick_project_folder(window, cx)); })); }
            DialogKind::ArchiveProject(_) | DialogKind::ArchiveThread(_) => body = body.child("Archive retains all conversations, drafts, sessions and Factory history. It never deletes checkout, session or Factory files. Archived history remains accessible with Show Archived."),
            DialogKind::Settings(_) => {
                body = body.child(format!("Application data: {}", state.data_dir.display())).child("Drost never installs, authenticates or upgrades tools. OMP and Neovim run with local user privileges; Factory uses its configured sandbox policy.");
                for name in ["omp", "nvim", "factory"] {
                    body = body.child(v_flex().text_xs().gap_1().child(name.to_uppercase()).child(state.diagnostics.get(name).cloned().unwrap_or_else(|| "Not inspected".into())));
                }
                let owner = owner.clone();
                body = body.child(Button::new("refresh-diagnostics").small().label("Refresh Paths and Git Diagnostics").on_click(move |_, _, cx| { let _ = owner.update(cx, |workspace, cx| { workspace.state.read(cx).refresh_diagnostics(); }); }));
                if let Some(project) = state.project() { body = body.child(format!("Project defaults · {}{}", project.name, if project.archived { " (archived, read-only)" } else { "" })); }
            }
            _ => {}
        }
        for field in &self.fields {
            body = body.child(
                v_flex()
                    .key_context("NativeInput")
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(0x8B95A5)).child(field.label))
                    .child(Input::new(&field.input)),
            );
        }
        let cancel = owner.clone();
        let confirm = owner.clone();
        let readonly = state.storage_error.is_some();
        v_flex().key_context("Dialog").track_focus(&self.focus).w(px(680.)).p_4().gap_3().bg(rgb(0x20242B)).border_1().border_color(rgb(0x343A46)).rounded_lg()
            .child(div().text_lg().child(title)).child(body)
            .when_some(self.error.clone(), |view, error| view.child(div().text_color(rgb(0xE6A0A0)).child(error)))
            .when(readonly, |view| view.child("Storage is not writable. Changes are disabled; existing drafts remain in memory."))
            .child(h_flex().justify_end().gap_2()
                .child(Button::new("dialog-cancel").small().ghost().label("Cancel").on_click(move |_, window, cx| { let _ = cancel.update(cx, |workspace, cx| workspace.dismiss(window, cx)); }))
                .child(Button::new("dialog-confirm").small().primary().disabled(readonly).label(if matches!(self.kind, DialogKind::ArchiveProject(_) | DialogKind::ArchiveThread(_)) { "Archive" } else { "Save" }).on_click(move |_, window, cx| { let _ = confirm.update(cx, |workspace, cx| workspace.confirm_dialog(window, cx)); })))
            .into_any_element()
    }
}
