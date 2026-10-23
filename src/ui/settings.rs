use super::{theme, workspace::Workspace};
use crate::{
    app::{AppState, CommitSubjectStatus, PresentationChange, WorktreeRequestStatus},
    harness,
    model::ModelKey,
    titles::{self, InstalledModels},
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, ThemeMode, ThemeRegistry, WindowExt,
        alert::Alert,
        button::{Button, ButtonVariants},
        form::{Field as FormField, Form},
        h_flex,
        input::{Input, InputEvent, InputState, Textarea, TextareaState},
        searchable_list::SearchableListItem,
        select::{Select, SelectEvent, SelectState},
        setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings},
        spinner::Spinner,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt, path::PathBuf};
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
fn retain_concurrent_value(initial: &str, draft: String, current: &str) -> String {
    if draft == initial && current != initial {
        current.to_owned()
    } else {
        draft
    }
}

/// Title model select value that reveals the Hugging Face reference field.
const OTHER_LLAMA_MODEL: &str = "<other>";
/// Settings field for a llama.cpp model that is not in the cache yet.
const LLAMA_MODEL_REFERENCE: &str = "llama_title_model_reference";

/// Conversation title settings the selected generator reads, in display order.
fn title_setting_keys(provider: &str, llama_model: &str) -> Vec<&'static str> {
    let llama = provider != "ollama";
    let ollama = provider != "llama_cpp";
    let mut keys = vec![titles::PROVIDER_SETTING];
    if llama {
        keys.push(titles::LLAMA_MODEL_SETTING);
        if llama_model == OTHER_LLAMA_MODEL {
            keys.push(LLAMA_MODEL_REFERENCE);
        }
    }
    if ollama {
        keys.push(titles::MODEL_SETTING);
    }
    keys
}

/// The default, every cached model, the saved model when the cache lacks it (llama.cpp
/// downloads it on first use), and an entry for naming another model.
fn llama_model_choices(
    cached: Option<&Result<Vec<String>, String>>,
    selected: &str,
) -> Vec<Choice> {
    let cached = cached.and_then(|cached| cached.as_ref().ok());
    let default = titles::DEFAULT_LLAMA_MODEL;
    let mut choices = vec![Choice::new(
        "",
        match cached {
            Some(cached) if !cached.iter().any(|model| model == default) => {
                format!("Default ({default}, not downloaded)")
            }
            _ => format!("Default ({default})"),
        },
    )];
    // The default is listed separately only when it was chosen explicitly.
    choices.extend(
        cached
            .into_iter()
            .flatten()
            .filter(|model| *model != default || selected == default)
            .map(|model| Choice::new(model.clone(), model.clone())),
    );
    if selected != OTHER_LLAMA_MODEL && !choices.iter().any(|choice| choice.value == selected) {
        choices.push(Choice::new(
            selected,
            if cached.is_some() {
                format!("{selected} (not downloaded)")
            } else {
                selected.to_owned()
            },
        ));
    }
    choices.push(Choice::new(OTHER_LLAMA_MODEL, "Other Hugging Face model…"));
    choices
}

/// The first-installed default, every installed model, and the saved model when Ollama is
/// known not to have it.
fn ollama_model_choices(
    installed: Option<&Result<Vec<String>, String>>,
    selected: &str,
) -> Vec<Choice> {
    let installed = installed.and_then(|installed| installed.as_ref().ok());
    let mut choices = vec![Choice::new(
        "",
        match installed.and_then(|models| models.first()) {
            Some(first) => format!("First installed model ({first})"),
            None => "First installed model".to_owned(),
        },
    )];
    choices.extend(
        installed
            .into_iter()
            .flatten()
            .map(|model| Choice::new(model.clone(), model.clone())),
    );
    if !choices.iter().any(|choice| choice.value == selected) {
        choices.push(Choice {
            value: selected.to_owned(),
            label: if installed.is_some() {
                format!("{selected} (not installed)").into()
            } else {
                selected.to_owned().into()
            },
            unavailable: installed.is_some(),
        });
    }
    choices
}

/// Factory launches require a local branch; blank takes the thread's branch when it starts.
fn target_branch_choices(branches: Option<&[String]>, selected: &str) -> Vec<Choice> {
    let mut choices = vec![Choice::new("", "Current branch when the thread starts")];
    choices.extend(
        branches
            .into_iter()
            .flatten()
            .map(|branch| Choice::new(branch.clone(), branch.clone())),
    );
    if !choices.iter().any(|choice| choice.value == selected) {
        choices.push(Choice {
            value: selected.to_owned(),
            label: if branches.is_some() {
                format!("{selected} (not a local branch)").into()
            } else {
                selected.to_owned().into()
            },
            unavailable: branches.is_some(),
        });
    }
    choices
}

/// The llama.cpp model to save: the selected entry, or the typed reference for another model.
fn chosen_llama_model(selected: String, reference: String) -> anyhow::Result<String> {
    if selected != OTHER_LLAMA_MODEL {
        return Ok(selected);
    }
    if reference.is_empty() {
        return Err(field_error(
            LLAMA_MODEL_REFERENCE,
            format!(
                "Enter a Hugging Face model reference, such as {}.",
                titles::DEFAULT_LLAMA_MODEL
            ),
        ));
    }
    Ok(reference)
}

#[derive(Clone, PartialEq, Eq)]
pub enum DialogKind {
    NewSpace,
    DeleteSpace(Uuid),
    /// A Conversation in a fresh worktree of this project, from a chosen base branch.
    NewWorktreeConversation(Uuid),
    /// Start another Conversation in the project checkout or an existing live worktree.
    NewWorkspaceConversation(Uuid),
    RemoveWorktree(Uuid),
    /// Check out another local branch, or a new one, in a Conversation's working tree.
    SwitchBranch(Uuid),
    /// Stage and commit everything in a Conversation's working tree.
    Commit(Uuid),
    Settings(Option<Uuid>),
}
pub enum SurfaceOutcome {
    Applied,
    SpaceCreated(Uuid),
    DeleteSpace(Uuid),
    /// Git is working in the background; the dialog stays open until the request settles.
    Pending(Uuid),
}
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub input: Entity<InputState>,
    pub(crate) initial: String,
}
/// The Commit dialog's message and the generated subject it asked for.
pub struct CommitDraft {
    message: Entity<TextareaState>,
    /// The subject request this dialog started; results of other requests are ignored.
    subject_request: Option<Uuid>,
    /// The message when the request started: a subject replaces it only if the user has not
    /// edited it since, so typed text is never overwritten.
    baseline: SharedString,
    /// Why the last request produced no subject, shown next to the control that retries.
    note: Option<String>,
}

pub struct SurfaceContent {
    pub kind: DialogKind,
    pub fields: Vec<Field>,
    pub error: Option<String>,
    error_field: Option<&'static str>,
    /// The worktree request this dialog started and is waiting for.
    pub request: Option<Uuid>,
    /// The latest request this form owns, including a settled request whose error is shown.
    last_request: Option<Uuid>,
    /// Removal was refused for uncommitted changes; the next confirm discards them.
    force: bool,
    selects: Vec<SelectField>,
    /// The installed models the title model choices were built from.
    title_models: Option<InstalledModels>,
    commit: Option<CommitDraft>,
    surface_id: Uuid,
    settings_id: SharedString,
    hosted: bool,
    owner: WeakEntity<Workspace>,
    state: Entity<AppState>,
    presentation: SurfacePresentation,
    _subscriptions: Vec<Subscription>,
}
impl SurfaceContent {
    pub fn new(
        kind: DialogKind,
        state: Entity<AppState>,
        owner: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (fields, selects) = state.update(cx, |state, cx| {
            let mut selects = Vec::new();
            let mut fields = Vec::new();
            let mut field = |key, label, value: String| {
                let initial = value.clone();
                let input = cx.new(|cx| {
                    let mut input = InputState::new(window, cx);
                    input.set_value(value, window, cx);
                    input
                });
                fields.push(Field {
                    key,
                    label,
                    input,
                    initial,
                });
            };
            match &kind {
                DialogKind::NewSpace => field("name", "Space name", String::new()),
                DialogKind::NewWorkspaceConversation(project) => {
                    let root = state
                        .snapshot
                        .projects
                        .iter()
                        .find(|candidate| candidate.id == *project)
                        .map(|project| project.root.display().to_string())
                        .unwrap_or_default();
                    let mut choices =
                        vec![Choice::new("project", format!("Current checkout — {root}"))];
                    choices.extend(
                        state
                            .snapshot
                            .worktrees
                            .iter()
                            .filter(|worktree| {
                                worktree.project_id == *project && worktree.is_live()
                            })
                            .map(|worktree| {
                                Choice::new(
                                    worktree.id.to_string(),
                                    format!(
                                        "{} · {} — {}",
                                        worktree.codename,
                                        state.worktree_branch_label(worktree),
                                        worktree.path.display()
                                    ),
                                )
                            }),
                    );
                    let selected = state
                        .conversation_thread()
                        .filter(|thread| thread.project_id == *project)
                        .and_then(|thread| state.thread_worktree(thread))
                        .filter(|worktree| worktree.is_live())
                        .map(|worktree| worktree.id.to_string())
                        .unwrap_or_else(|| "project".into());
                    selects.push(select_field(
                        "workspace",
                        "Workspace",
                        choices,
                        &selected,
                        window,
                        cx,
                    ));
                }
                DialogKind::NewWorktreeConversation(project) => {
                    let repository = state
                        .repositories
                        .get(project)
                        .and_then(|repository| repository.as_ref().ok());
                    let branches: Vec<String> = repository
                        .map(|repository| repository.branches.clone())
                        .unwrap_or_default();
                    // Default to where the selected Conversation already works.
                    let selected = state
                        .conversation_thread()
                        .filter(|thread| thread.project_id == *project)
                        .and_then(|thread| state.thread_worktree(thread))
                        .filter(|worktree| worktree.is_live())
                        .map(|worktree| worktree.branch.clone())
                        .or_else(|| repository.and_then(|repository| repository.branch.clone()))
                        .filter(|branch| branches.contains(branch))
                        .or_else(|| branches.first().cloned())
                        .unwrap_or_default();
                    if !branches.is_empty() {
                        selects.push(select_field(
                            "base",
                            "Base branch",
                            branches
                                .iter()
                                .map(|branch| Choice::new(branch.clone(), branch.clone()))
                                .collect(),
                            &selected,
                            window,
                            cx,
                        ));
                    }
                }
                DialogKind::RemoveWorktree(_) => {}
                DialogKind::SwitchBranch(thread) => {
                    field(
                        "new_branch",
                        "New branch from HEAD (optional)",
                        String::new(),
                    );
                    let thread = state.snapshot.threads.iter().find(|t| t.id == *thread);
                    let repository = thread.and_then(|thread| {
                        state
                            .repositories
                            .get(&thread.project_id)
                            .and_then(|repository| repository.as_ref().ok())
                    });
                    let own = thread.and_then(|thread| thread.worktree_id);
                    // Git allows a branch in one worktree only: hide the ones checked out
                    // elsewhere, as far as the app has seen them.
                    let elsewhere: Vec<String> = thread
                        .map(|thread| {
                            let mut taken: Vec<String> = state
                                .snapshot
                                .worktrees
                                .iter()
                                .filter(|w| {
                                    w.is_live()
                                        && w.project_id == thread.project_id
                                        && Some(w.id) != own
                                })
                                .map(|w| state.worktree_branch_label(w))
                                .collect();
                            if own.is_some()
                                && let Some(branch) = repository.and_then(|r| r.branch.clone())
                            {
                                taken.push(branch);
                            }
                            taken
                        })
                        .unwrap_or_default();
                    let current = thread.and_then(|thread| match state.thread_worktree(thread) {
                        Some(worktree) => Some(state.worktree_branch_label(worktree)),
                        None => repository.and_then(|r| r.branch.clone()),
                    });
                    let branches: Vec<String> = repository
                        .map(|repository| repository.branches.clone())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|branch| !elsewhere.contains(branch))
                        .collect();
                    if !branches.is_empty() {
                        selects.push(select_field(
                            "branch",
                            "Branch",
                            branches
                                .iter()
                                .map(|branch| Choice::new(branch.clone(), branch.clone()))
                                .collect(),
                            current
                                .as_deref()
                                .filter(|current| branches.iter().any(|b| b == current))
                                .unwrap_or(&branches[0]),
                            window,
                            cx,
                        ));
                    }
                }
                DialogKind::Settings(project) => {
                    for (key, label) in [
                        ("nvim", "Neovim executable (blank: discover on PATH)"),
                        ("omp", "OMP executable (blank: discover on PATH)"),
                        ("pi", "Pi executable (blank: discover on PATH)"),
                        ("claude", "Claude Code executable (blank: discover on PATH)"),
                        ("codex", "Codex executable (blank: discover on PATH)"),
                        ("opencode", "OpenCode executable (blank: discover on PATH)"),
                        (
                            "llama",
                            "llama.cpp executable (blank: discover automatically)",
                        ),
                        ("factory", "Factory executable (blank: discover on PATH)"),
                        (
                            "azdo",
                            "azdo executable for Azure DevOps (blank: discover on PATH)",
                        ),
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
                    field(
                        LLAMA_MODEL_REFERENCE,
                        "Hugging Face model (owner/repository:quantisation)",
                        String::new(),
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
                        let target_branch = project
                            .defaults
                            .factory_target_branch
                            .clone()
                            .unwrap_or_default();
                        let branches = state
                            .repositories
                            .get(&project.id)
                            .and_then(|repository| repository.as_ref().ok())
                            .map(|repository| repository.branches.as_slice());
                        selects.push(select_field(
                            "factory_target_branch",
                            "Factory target branch",
                            target_branch_choices(branches, &target_branch),
                            &target_branch,
                            window,
                            cx,
                        ));
                    }

                    let title_provider = state
                        .snapshot
                        .settings
                        .get(titles::PROVIDER_SETTING)
                        .and_then(|value| value.as_str())
                        .filter(|value| ["automatic", "llama_cpp", "ollama"].contains(value))
                        .unwrap_or("automatic");
                    selects.push(select_field(
                        titles::PROVIDER_SETTING,
                        "Title generator",
                        vec![
                            Choice::new("automatic", "Automatic (llama.cpp, then Ollama)"),
                            Choice::new("llama_cpp", "Llama.cpp"),
                            Choice::new("ollama", "Ollama"),
                        ],
                        title_provider,
                        window,
                        cx,
                    ));
                    // Models change outside the app; earlier results show until these land.
                    if !state.title_models_loading() {
                        state.refresh_title_models();
                    }
                    let installed = state.title_models.as_ref();
                    let saved = |key| {
                        state
                            .snapshot
                            .settings
                            .get(key)
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim()
                            .to_owned()
                    };
                    let llama_model = saved(titles::LLAMA_MODEL_SETTING);
                    selects.push(select_field(
                        titles::LLAMA_MODEL_SETTING,
                        "llama.cpp model",
                        llama_model_choices(installed.map(|models| &models.llama), &llama_model),
                        &llama_model,
                        window,
                        cx,
                    ));
                    let ollama_model = saved(titles::MODEL_SETTING);
                    selects.push(select_field(
                        titles::MODEL_SETTING,
                        "Ollama model",
                        ollama_model_choices(installed.map(|models| &models.ollama), &ollama_model),
                        &ollama_model,
                        window,
                        cx,
                    ));
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
                            .map_or("", theme::saved_theme_name);
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
                        let selected = &project.defaults.interactive_harness;
                        let mut choices = harness::CONVERSATION_HARNESSES
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
                            "interactive_harness",
                            "Conversation harness",
                            choices,
                            selected,
                            window,
                            cx,
                        ));
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
            (fields, selects)
        });
        let presentation = SurfacePresentation::new(&kind, state.read(cx));
        let error = match &kind {
            DialogKind::NewWorktreeConversation(_) if selects.is_empty() => {
                Some("This repository has no local branches yet. Commit first.".to_owned())
            }
            _ => None,
        };
        let mut subscriptions = vec![
            cx.observe_in(&state, window, |surface, _, window, cx| {
                surface.refresh_presentation(cx);
                surface.refresh_title_model_choices(window, cx);
            }),
            cx.subscribe(
                &state,
                |surface, _, change: &PresentationChange, cx| match change {
                    PresentationChange::Persistence => surface.refresh_presentation(cx),
                    PresentationChange::WorktreeRequest(request) => {
                        surface.worktree_request_changed(*request, cx)
                    }
                    _ => {}
                },
            ),
            cx.observe_global::<gpui_kit::component::Theme>(|_, cx| cx.notify()),
            cx.observe_global_in::<ThemeRegistry>(window, |surface, window, cx| {
                surface.refresh_theme_choices(window, cx);
                cx.notify();
            }),
        ];
        for field in &fields {
            let key = field.key;
            subscriptions.push(cx.subscribe(
                &field.input,
                move |surface, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        surface.clear_field_error(key, cx);
                    }
                },
            ));
        }
        for select in &selects {
            let key = select.key;
            subscriptions.push(cx.subscribe(
                &select.state,
                move |surface, _, _: &SelectEvent<Vec<Choice>>, cx| {
                    surface.clear_field_error(key, cx);
                },
            ));
            subscriptions.push(cx.observe(&select.state, |_, _, cx| cx.notify()));
        }
        let commit = match &kind {
            DialogKind::Commit(_) => {
                let message = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(3, 10)
                        .placeholder("Summarize the change in one line")
                });
                subscriptions.push(
                    cx.subscribe(&message, |surface, _, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            surface.clear_field_error("message", cx);
                        }
                    }),
                );
                subscriptions.push(cx.subscribe_in(
                    &state,
                    window,
                    |surface, _, change: &PresentationChange, window, cx| {
                        if let PresentationChange::CommitSubject(request) = change {
                            surface.commit_subject_changed(*request, window, cx);
                        }
                    },
                ));
                Some(CommitDraft {
                    message,
                    subject_request: None,
                    baseline: SharedString::default(),
                    note: None,
                })
            }
            _ => None,
        };
        let surface_id = Uuid::new_v4();
        let title_models = state.read(cx).title_models.clone();
        Self {
            kind,
            fields,
            error,
            error_field: None,
            request: None,
            last_request: None,
            force: false,
            selects,
            title_models,
            commit,
            surface_id,
            settings_id: format!("drostje-settings-{surface_id}").into(),
            hosted: false,
            owner,
            state,
            presentation,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn surface_id(&self) -> Uuid {
        self.surface_id
    }
    pub(super) fn hosted(&self) -> bool {
        self.hosted
    }
    pub(super) fn unhost(&mut self, cx: &mut Context<Self>) {
        self.hosted = false;
        cx.notify();
    }

    pub(super) fn last_request(&self) -> Option<Uuid> {
        self.last_request
    }

    /// Marks the request this dialog is waiting for; the confirm control is disabled until
    /// the App layer reports how it ended.
    pub fn begin_request(&mut self, request: Uuid, cx: &mut Context<Self>) {
        self.request = Some(request);
        self.last_request = Some(request);
        self.error = None;
        self.error_field = None;
        cx.notify();
    }

    /// The Commit dialog's message, for native tests that type into it.
    #[cfg(all(test, feature = "runtime-qa"))]
    pub(crate) fn commit_message(&self) -> Option<Entity<TextareaState>> {
        self.commit.as_ref().map(|commit| commit.message.clone())
    }
    pub fn focus_commit_message(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(commit) = &self.commit {
            commit.message.focus_handle(cx).focus(window, cx);
        }
    }

    /// Asks the local model for a subject for the Commit dialog's changes.
    pub fn write_commit_subject(&mut self, cx: &mut Context<Self>) {
        let DialogKind::Commit(thread) = self.kind else {
            return;
        };
        let Some(commit) = self.commit.as_mut() else {
            return;
        };
        commit.baseline = commit.message.read(cx).value();
        commit.note = None;
        match self
            .state
            .update(cx, |state, cx| state.write_commit_subject(thread, cx))
        {
            Ok(request) => commit.subject_request = Some(request),
            Err(error) => {
                commit.subject_request = None;
                commit.note = Some(format!("{error:#}"));
            }
        }
        cx.notify();
    }

    fn commit_subject_changed(
        &mut self,
        request: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(commit) = self
            .commit
            .as_mut()
            .filter(|commit| commit.subject_request == Some(request))
        else {
            return;
        };
        let status = self
            .state
            .read(cx)
            .commit_subject
            .as_ref()
            .filter(|pending| pending.id == request)
            .map(|pending| pending.status.clone());
        match status {
            Some(CommitSubjectStatus::Pending) => return,
            Some(CommitSubjectStatus::Written(subject)) => {
                if commit.message.read(cx).value() == commit.baseline {
                    commit
                        .message
                        .update(cx, |message, cx| message.set_value(subject, window, cx));
                } else {
                    commit.note =
                        Some("Kept your edits; generate again to replace them.".to_owned());
                }
            }
            Some(CommitSubjectStatus::Unavailable) => {
                commit.note = Some(
                    "No message generated: configure the llama.cpp executable or start Ollama, then try again."
                        .to_owned(),
                );
            }
            Some(CommitSubjectStatus::Failed(error)) => {
                commit.note = Some(format!(
                    "Could not generate a message: {error}. Check title generation in Settings."
                ));
            }
            None => {}
        }
        commit.subject_request = None;
        cx.notify();
    }

    fn worktree_request_changed(&mut self, request: Uuid, cx: &mut Context<Self>) {
        if self.request != Some(request) {
            return;
        }
        let status = self
            .state
            .read(cx)
            .worktree_request
            .as_ref()
            .filter(|pending| pending.id == request)
            .map(|pending| pending.status.clone());
        match status {
            Some(WorktreeRequestStatus::Failed(error)) => {
                self.request = None;
                self.error = Some(error);
                self.error_field = None;
            }
            Some(WorktreeRequestStatus::Refused(error)) => {
                self.request = None;
                self.force = true;
                self.error = Some(error);
                self.error_field = None;
            }
            // Done: the Workspace closes the dialog. Pending or gone: nothing to show yet.
            _ => {}
        }
        cx.notify();
    }

    fn refresh_presentation(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        if !self.presentation.matches(&self.kind, state) {
            self.presentation = SurfacePresentation::new(&self.kind, state);
            cx.notify();
        }
    }

    fn clear_field_error(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.error.is_some() && (self.error_field.is_none() || self.error_field == Some(key)) {
            self.error = None;
            self.error_field = None;
            cx.notify();
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

    pub fn set_error(&mut self, error: &anyhow::Error, cx: &mut Context<Self>) {
        self.error_field = error.downcast_ref::<FieldError>().map(|error| error.key);
        self.error = Some(format!("{error:#}"));
        cx.notify();
    }
    pub fn apply(&self, state: &mut AppState, cx: &App) -> anyhow::Result<SurfaceOutcome> {
        if let Some(error) = &state.storage_error {
            anyhow::bail!("Storage is not writable: {error}");
        }
        match self.kind {
            DialogKind::NewSpace => {
                return state
                    .create_space(&self.value("name", cx))
                    .map(SurfaceOutcome::SpaceCreated);
            }
            DialogKind::DeleteSpace(id) => return Ok(SurfaceOutcome::DeleteSpace(id)),
            DialogKind::NewWorkspaceConversation(project) => {
                anyhow::ensure!(
                    state.selected_project == Some(project),
                    "Project selection changed; reopen New conversation in workspace."
                );
                let selected = self.value("workspace", cx);
                let worktree = if selected == "project" {
                    None
                } else {
                    Some(
                        Uuid::parse_str(&selected)
                            .map_err(|_| field_error("workspace", "Choose a workspace."))?,
                    )
                };
                state.new_conversation_in_workspace(worktree)?;
            }
            DialogKind::NewWorktreeConversation(project) => {
                let base = self.value("base", cx);
                if base.is_empty() {
                    return Err(field_error("base", "Choose a base branch."));
                }
                return state
                    .create_worktree_conversation(project, &base)
                    .map(SurfaceOutcome::Pending);
            }
            DialogKind::RemoveWorktree(id) => {
                return state
                    .remove_worktree(id, self.force)
                    .map(SurfaceOutcome::Pending);
            }
            DialogKind::SwitchBranch(thread) => {
                anyhow::ensure!(
                    state.conversation_thread().is_some_and(|t| t.id == thread),
                    "Thread selection changed; reopen Switch branch."
                );
                let new = self.value("new_branch", cx);
                let (branch, create) = if new.is_empty() {
                    (self.value("branch", cx), false)
                } else {
                    (new, true)
                };
                if branch.is_empty() {
                    return Err(field_error(
                        "new_branch",
                        "Choose a branch or name a new one.",
                    ));
                }
                return state
                    .switch_branch(&branch, create)
                    .map(SurfaceOutcome::Pending);
            }
            DialogKind::Commit(thread) => {
                let message = self
                    .commit
                    .as_ref()
                    .map(|commit| commit.message.read(cx).value().trim().to_owned())
                    .unwrap_or_default();
                if message.is_empty() {
                    return Err(field_error("message", "Write a commit message."));
                }
                return state
                    .commit_changes(thread, &message)
                    .map(SurfaceOutcome::Pending);
            }

            DialogKind::Settings(project_id) => {
                let llama_model = chosen_llama_model(
                    self.value(titles::LLAMA_MODEL_SETTING, cx),
                    self.value(LLAMA_MODEL_REFERENCE, cx),
                )?;
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
                    let interactive_harness = self.value("interactive_harness", cx);
                    if !harness::CONVERSATION_HARNESSES.contains(&interactive_harness.as_str()) {
                        return Err(field_error(
                            "interactive_harness",
                            "Choose omp, pi, or claude-code.",
                        ));
                    }
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
                    project.defaults.interactive_harness = interactive_harness;
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
                let pi = {
                    let value = self.value("pi", cx);
                    let initial = &self
                        .fields
                        .iter()
                        .find(|field| field.key == "pi")
                        .expect("Pi settings field")
                        .initial;
                    let current = state
                        .snapshot
                        .settings
                        .get("executables")
                        .and_then(|value| value.get("pi"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    retain_concurrent_value(initial, value, current)
                };
                state.set_setting(
                    "executables",
                    json!({
                        "nvim": self.value("nvim", cx),
                        "omp": self.value("omp", cx),
                        "pi": pi,
                        "claude": self.value("claude", cx),
                        "codex": self.value("codex", cx),
                        "opencode": self.value("opencode", cx),
                        "llama": self.value("llama", cx),
                        "factory": self.value("factory", cx),
                        "azdo": self.value("azdo", cx),
                    }),
                );
                state.set_setting("factory_data_root", json!(root));
                state.set_setting(titles::LLAMA_MODEL_SETTING, json!(llama_model));
                state.set_setting(
                    titles::MODEL_SETTING,
                    json!(self.value(titles::MODEL_SETTING, cx)),
                );
                state.set_setting(
                    titles::PROVIDER_SETTING,
                    json!(self.value(titles::PROVIDER_SETTING, cx)),
                );
                state.set_setting("theme_mode", json!(mode));
                state.set_setting("theme_light", json!(light));
                state.set_setting("theme_dark", json!(dark));
                if let Some(project) = project {
                    state.save_project(project);
                }
                state.refresh_diagnostics();
                state.refresh_title_models();
            }
        }
        Ok(SurfaceOutcome::Applied)
    }
    fn title(&self) -> &'static str {
        match self.kind {
            DialogKind::NewSpace => "New space",
            DialogKind::DeleteSpace(_) => "Delete space",
            DialogKind::NewWorktreeConversation(_) => "New conversation in new worktree",
            DialogKind::NewWorkspaceConversation(_) => "New conversation in workspace",
            DialogKind::RemoveWorktree(_) => "Remove worktree",
            DialogKind::SwitchBranch(_) => "Switch branch",
            DialogKind::Commit(_) => "Commit changes",
            DialogKind::Settings(_) => "Settings",
        }
    }

    /// Rebuilds the title model choices when an inspection lands, keeping the draft selection.
    fn refresh_title_model_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let installed = &self.state.read(cx).title_models;
        if *installed == self.title_models {
            return;
        }
        self.title_models = installed.clone();
        for key in [titles::LLAMA_MODEL_SETTING, titles::MODEL_SETTING] {
            let Some(select) = self.select(key).cloned() else {
                continue;
            };
            let selected = self.value(key, cx);
            let models = self.title_models.as_ref();
            let choices = if key == titles::LLAMA_MODEL_SETTING {
                llama_model_choices(models.map(|models| &models.llama), &selected)
            } else {
                ollama_model_choices(models.map(|models| &models.ollama), &selected)
            };
            select.update(cx, |select, cx| {
                select.set_items(choices, window, cx);
                select.set_selected_value(&selected, window, cx);
                cx.notify();
            });
        }
        cx.notify();
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
                        .disabled(readonly)
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
                (theme::initial_load_finished(cx) && !selected.is_empty() && !theme::theme_names(mode, cx).contains(&selected))
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

    /// What the app knows about a field's options, shown under its label.
    fn field_description(&self, key: &str, state: &AppState) -> Option<String> {
        let installed = state.title_models.as_ref();
        let count = |models: &[String], noun: &str| match models.len() {
            1 => format!("1 {noun}"),
            n => format!("{n} {noun}s"),
        };
        match key {
            titles::LLAMA_MODEL_SETTING => Some(match installed.map(|models| &models.llama) {
                None => "Looking for downloaded models…".into(),
                Some(Ok(models)) if models.is_empty() => {
                    "The llama.cpp cache is empty. The chosen model downloads on first use.".into()
                }
                Some(Ok(models)) => format!(
                    "{} in the llama.cpp cache. Other models download on first use.",
                    count(models, "model")
                ),
                Some(Err(error)) => error.clone(),
            }),
            titles::MODEL_SETTING => {
                let host = titles::ollama_host();
                Some(match installed.map(|models| &models.ollama) {
                    None => "Looking for installed models…".into(),
                    Some(Ok(models)) if models.is_empty() => {
                        format!("Ollama at {host} has no models. Pull one, then refresh models.")
                    }
                    Some(Ok(models)) => {
                        format!("{} installed at {host}", count(models, "model"))
                    }
                    Some(Err(error)) => format!("{error}. Start Ollama, then refresh models."),
                })
            }
            LLAMA_MODEL_REFERENCE => Some("Downloads from Hugging Face on first use.".into()),
            "factory_target_branch" => {
                let DialogKind::Settings(Some(project)) = self.kind else {
                    return None;
                };
                match state.repositories.get(&project) {
                    Some(Err(error)) => Some(format!("Local branches are unavailable: {error}")),
                    None => Some("Reading local branches…".into()),
                    Some(Ok(_)) => None,
                }
            }
            _ => None,
        }
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

    fn settings_body(
        &self,
        project_id: Option<Uuid>,
        state: &AppState,
        owner: &WeakEntity<SurfaceContent>,
        cx: &App,
    ) -> AnyElement {
        let readonly = state.storage_error.is_some();
        let fields = |title: &'static str, keys: &[&'static str]| {
            SettingGroup::new()
                .title(title)
                .items(keys.iter().filter_map(|key| {
                    let item = self.setting_item(key, title, readonly, owner)?;
                    Some(match self.field_description(key, state) {
                        Some(description) => item.description(description),
                        None => item,
                    })
                }))
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
        let tools = fields(
            "Tools",
            &[
                "nvim", "omp", "pi", "claude", "codex", "opencode", "llama", "factory", "azdo",
            ],
        )
        .description(
            "Tool installation and upgrades run only at your explicit request. DROSTJE. does not authenticate for you.",
        );
        let provider = self.value(titles::PROVIDER_SETTING, cx);
        let loading = state.title_models_loading();
        let refresh = owner.clone();
        let titles = fields(
            "Conversation titles",
            &title_setting_keys(&provider, &self.value(titles::LLAMA_MODEL_SETTING, cx)),
        )
        .item(
            SettingItem::new(
                "Installed models",
                SettingField::render(move |options, _, _| {
                    let refresh = refresh.clone();
                    Button::new("refresh-title-models")
                        .small()
                        .label("Refresh models")
                        .loading(loading)
                        .disabled(loading || options.is_disabled())
                        .on_click(move |_, _, cx| {
                            let _ = refresh.update(cx, |surface, cx| {
                                surface.state.update(cx, |state, cx| {
                                    state.refresh_title_models();
                                    cx.notify();
                                })
                            });
                        })
                }),
            )
            .description(if loading {
                "Looking for downloaded and installed models…"
            } else {
                "Lists the llama.cpp cache and the models Ollama has installed."
            })
            .keywords(["titles", "models", "refresh", "llama", "ollama"]),
        )
        .description(match provider.as_str() {
            "llama_cpp" => "Runs the local llama.cpp CLI.".to_owned(),
            "ollama" => format!("Asks the Ollama server at {}.", titles::ollama_host()),
            _ => format!(
                "Runs the local llama.cpp CLI first and asks Ollama at {} when llama.cpp is unavailable.",
                titles::ollama_host()
            ),
        });
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
        let mut project = fields(
            "Project defaults",
            &["interactive_harness", "provider", "model"],
        );
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
                "Application data: {}. Conversation harnesses run with local user privileges. Factory uses its configured sandbox policy.",
                state.data_dir.display()
            ));
        for (name, title) in [
            ("nvim", "Neovim"),
            ("omp", "OMP"),
            ("pi", "Pi"),
            ("claude", "Claude Code"),
            ("codex", "Codex"),
            ("opencode", "OpenCode"),
            ("llama", "llama.cpp"),
            ("factory", "Factory"),
            ("azdo", "Azure DevOps (azdo)"),
        ] {
            let status = state
                .diagnostics
                .get(name)
                .cloned()
                .unwrap_or_else(|| "Not inspected".into());
            // A resolved azdo can still be signed out; that is a setup issue too.
            let tracker_issue = (name == "azdo")
                .then(|| state.tracker_setup_issue())
                .flatten();
            let status = match &tracker_issue {
                Some(issue) => format!("{status}. {issue}"),
                None => status,
            };
            let install_pi =
                name == "pi" && (state.installing_pi || status.starts_with("Unavailable:"));
            let install_reason = install_pi
                .then(|| super::commands::Command::InstallPi.disabled_reason(state))
                .flatten();
            let owner = owner.clone();
            diagnostics = diagnostics.item(
                SettingItem::new(
                    title,
                    SettingField::render(move |_, _, _| {
                        if install_pi {
                            let tooltip = install_reason.clone().unwrap_or_else(|| {
                                "Install Pi in DROSTJE.'s application data".into()
                            });
                            Button::new("install-pi")
                                .small()
                                .label(
                                    if install_reason.as_deref()
                                        == Some("Pi installation is already running.")
                                    {
                                        "Installing…"
                                    } else {
                                        "Install Pi"
                                    },
                                )
                                .disabled(install_reason.is_some())
                                .tooltip(tooltip)
                                .on_click(move |_, window, cx| {
                                    window.dispatch_action(Box::new(super::commands::InstallPi), cx)
                                })
                        } else if tracker_issue.is_some() {
                            Button::new("refresh-pull-requests")
                                .small()
                                .label("Check again")
                                .on_click(move |_, window, cx| {
                                    window.dispatch_action(
                                        Box::new(super::commands::RefreshPullRequests),
                                        cx,
                                    )
                                })
                        } else {
                            let owner = owner.clone();
                            Button::new(SharedString::from(format!("refresh-diagnostics-{name}")))
                                .small()
                                .label("Refresh diagnostics")
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |surface, cx| {
                                        surface.state.read(cx).refresh_diagnostics()
                                    });
                                })
                        }
                    }),
                )
                .description(status)
                .keywords([name, "diagnostics", "setup", "retry"]),
            );
        }
        // Kit Settings retains search/navigation; this entity owns the draft controls.
        let settings = Settings::new(self.settings_id.clone()).page(
            SettingPage::new("Settings")
                .default_open(true)
                .resettable(false)
                .groups([appearance, tools, titles, factory, project, diagnostics]),
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

    fn body(&self, state: &AppState, owner: &WeakEntity<SurfaceContent>, cx: &App) -> AnyElement {
        let readonly = state.storage_error.is_some();
        let mut body = v_flex().id("dialog-body").w_full().gap_5().pb_4();
        match self.kind {
            DialogKind::Settings(project_id) => {
                return self.settings_body(project_id, state, owner, cx);
            }
            DialogKind::NewWorkspaceConversation(_) => {
                body = body
                    .child(div().text_sm().text_color(cx.theme().muted_foreground)
                        .child("Starts a new conversation in the selected directory. The current conversation, its draft, and its session stay where they are. Worktrees can be shared by several conversations."))
                    .child(div().id("workspace-choice").test_support()
                        .child(Form::new().child(self.form_field("workspace", readonly, cx))));
            }
            DialogKind::NewWorktreeConversation(project) => {
                let pending = self.request.is_some();
                let location = state
                    .snapshot
                    .projects
                    .iter()
                    .find(|candidate| candidate.id == project)
                    .map(|project| {
                        crate::worktrees::worktree_dir(&state.data_dir, &project.root, "…")
                    });
                body = body
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(match location {
                                Some(location) => format!(
                                    "Creates a linked worktree at {} on a new branch under {}; the branch is renamed after the first generated title. The conversation runs there instead of the project root.",
                                    location.display(),
                                    crate::worktrees::BRANCH_PREFIX
                                ),
                                None => "Creates a linked worktree on a new branch; the conversation runs there instead of the project root.".into(),
                            }),
                    )
                    .when(self.select("base").is_some(), |view| {
                        view.child(Form::new().child(self.form_field("base", readonly || pending, cx)))
                    })
                    .when(pending, |view| {
                        view.child(
                            h_flex()
                                .id("worktree-pending")
                                .gap_2()
                                .items_center()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(Spinner::new().xsmall())
                                .child("Creating worktree…"),
                        )
                    });
            }
            DialogKind::SwitchBranch(thread) => {
                let pending = self.request.is_some();
                let tree = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == thread)
                    .map(|thread| match state.thread_worktree(thread) {
                        Some(worktree) => format!("worktree {}", worktree.codename),
                        None => "the project root".to_owned(),
                    })
                    .unwrap_or_else(|| "this working tree".to_owned());
                body = body
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Checks out a branch in {tree}. Uncommitted changes block the switch; commit or stash them first. Branches checked out in another worktree are not listed."
                            )),
                    )
                    .child(
                        Form::new()
                            .when(self.select("branch").is_some(), |form| {
                                form.child(self.form_field("branch", readonly || pending, cx))
                            })
                            .child(self.form_field("new_branch", readonly || pending, cx)),
                    )
                    .when(pending, |view| {
                        view.child(
                            h_flex()
                                .id("worktree-pending")
                                .gap_2()
                                .items_center()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(Spinner::new().xsmall())
                                .child("Switching branch…"),
                        )
                    });
            }
            DialogKind::Commit(thread) => {
                let pending = self.request.is_some();
                let thread = state.snapshot.threads.iter().find(|t| t.id == thread);
                let tree = thread
                    .map(|thread| match state.thread_worktree(thread) {
                        Some(worktree) => format!("worktree {}", worktree.codename),
                        None => "the project root".to_owned(),
                    })
                    .unwrap_or_else(|| "this working tree".to_owned());
                let summary = thread
                    .and_then(|thread| state.conversation_diff(thread.id))
                    .and_then(|diff| diff.result.as_ref())
                    .and_then(|result| result.as_ref().ok())
                    .filter(|diff| diff.changed_files > 0)
                    .map(|diff| {
                        format!(
                            " ({} {}, +{} −{})",
                            diff.changed_files,
                            if diff.changed_files == 1 {
                                "file"
                            } else {
                                "files"
                            },
                            diff.additions,
                            diff.deletions
                        )
                    })
                    .unwrap_or_default();
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Stages and commits every change in {tree}{summary}, untracked files included. Hooks run as the repository configures them."
                        )),
                );
                if let Some(commit) = &self.commit {
                    let writing = commit.subject_request.is_some();
                    let surface = owner.clone();
                    body = body
                        .child(
                            Form::new().child(
                                FormField::new().label("Message").child(
                                    v_flex()
                                        .key_context("NativeInput CommitMessage")
                                        .w_full()
                                        .gap_1()
                                        .child(
                                            Textarea::new(&commit.message)
                                                .disabled(readonly || pending),
                                        )
                                        .when(self.error_field == Some("message"), |view| {
                                            view.when_some(self.error.clone(), |view, error| {
                                                view.child(
                                                    Alert::error("field-error-message", error)
                                                        .small(),
                                                )
                                            })
                                        }),
                                ),
                            ),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Button::new("generate-commit-message")
                                        .small()
                                        .label("Generate message")
                                        .loading(writing)
                                        .disabled(readonly || pending || writing)
                                        .on_click(move |_, _, cx| {
                                            let _ = surface.update(cx, |surface, cx| {
                                                surface.write_commit_subject(cx)
                                            });
                                        }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .when(writing, |note| {
                                            note.child("Writing a message with the local model…")
                                        })
                                        .when(!writing, |note| note.children(commit.note.clone())),
                                ),
                        );
                }
                body = body.when(pending, |view| {
                    view.child(
                        h_flex()
                            .id("worktree-pending")
                            .gap_2()
                            .items_center()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(Spinner::new().xsmall())
                            .child("Committing…"),
                    )
                });
            }
            DialogKind::RemoveWorktree(id) => {
                let pending = self.request.is_some();
                match state.worktree(id) {
                    Some(worktree) => {
                        body = body
                            .child(format!(
                                "Remove worktree {} at {}? Its branch {} is deleted only when it is merged into {}. Conversations in it stay in history but cannot continue; terminals opened in it keep running.",
                                worktree.codename,
                                worktree.path.display(),
                                worktree.branch,
                                worktree.base
                            ))
                            .when(self.force, |view| {
                                view.child(
                                    Alert::warning(
                                        "worktree-force",
                                        "Uncommitted changes in this worktree will be discarded.",
                                    )
                                    .small(),
                                )
                            });
                    }
                    None => {
                        body = body.child("This worktree is no longer available.");
                    }
                }
                body = body.when(pending, |view| {
                    view.child(
                        h_flex()
                            .id("worktree-pending")
                            .gap_2()
                            .items_center()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(Spinner::new().xsmall())
                            .child("Removing worktree…"),
                    )
                });
            }
            DialogKind::NewSpace => {
                body = body.child(Form::new().child(self.form_field("name", readonly, cx)));
            }
            DialogKind::DeleteSpace(id) => {
                let name = state
                    .spaces
                    .iter()
                    .find(|space| space.id == id)
                    .map(|space| space.name.as_str())
                    .unwrap_or("this space");
                body = body.child(format!("Delete “{name}”? All its projects and threads move to Default. No projects, threads, drafts, or running agents are deleted."));
            }
        }
        body.when(self.error_field.is_none() && !matches!(self.kind, DialogKind::Commit(_)), |view| {
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

struct SurfacePresentation {
    readonly: bool,
    project: Option<(String, bool)>,
    diagnostics: BTreeMap<String, String>,
}

impl SurfacePresentation {
    fn project<'a>(kind: &DialogKind, state: &'a AppState) -> Option<(&'a str, bool)> {
        let DialogKind::Settings(Some(id)) = kind else {
            return None;
        };
        state
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == *id)
            .map(|project| (project.name.as_str(), project.archived))
    }

    fn new(kind: &DialogKind, state: &AppState) -> Self {
        Self {
            readonly: state.storage_error.is_some(),
            project: Self::project(kind, state).map(|(name, archived)| (name.to_owned(), archived)),
            diagnostics: if matches!(kind, DialogKind::Settings(_)) {
                state.diagnostics.clone()
            } else {
                BTreeMap::new()
            },
        }
    }

    fn matches(&self, kind: &DialogKind, state: &AppState) -> bool {
        self.readonly == state.storage_error.is_some()
            && self
                .project
                .as_ref()
                .map(|(name, archived)| (name.as_str(), *archived))
                == Self::project(kind, state)
            && (!matches!(kind, DialogKind::Settings(_)) || self.diagnostics == state.diagnostics)
    }
}

impl Render for SurfaceContent {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let surface = cx.entity().downgrade();
        let owner = self.owner.clone();
        let id = self.surface_id;
        let body = self.body(self.state.read(cx), &surface, cx);
        let body = if matches!(self.kind, DialogKind::Commit(_)) {
            div()
                .id("commit-fields")
                .max_h(rems(17.))
                .overflow_y_scroll()
                .child(body)
                .into_any_element()
        } else {
            body
        };
        v_flex()
            .w_full()
            .when(matches!(self.kind, DialogKind::Settings(_)), |view| {
                view.size_full().min_h_0().min_w_0().overflow_hidden()
            })
            .key_context("DrostjeForm")
            .on_action(move |_: &super::commands::Confirm, window, cx| {
                cx.stop_propagation();
                confirm_surface(&owner, id, window, cx);
            })
            .when(matches!(self.kind, DialogKind::Commit(_)), |view| {
                view.when_some(self.error.clone(), |view, error| {
                    view.child(
                        div()
                            .id("commit-validation")
                            .test_support()
                            .child(Alert::error("commit-validation-alert", error).small()),
                    )
                })
            })
            .child(body)
            .when(matches!(self.kind, DialogKind::Commit(_)), |view| {
                view.child(footer(self))
            })
            .into_any_element()
    }
}

/// Renders a retained Commit surface inline, including its existing message body and actions.
pub(super) fn render_commit_panel(content: Entity<SurfaceContent>, cx: &App) -> AnyElement {
    v_flex()
        .id("commit-changes-panel")
        .debug_selector(|| "commit-changes-panel".into())
        .test_support()
        .w_full()
        .flex_shrink_0()
        .gap_3()
        .p_3()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius)
        .child(content)
        .into_any_element()
}

struct SurfaceFooter {
    content: WeakEntity<SurfaceContent>,
    _subscription: Subscription,
}

impl Render for SurfaceFooter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(content) = self.content.upgrade() else {
            return div().into_any_element();
        };
        footer(content.read(cx))
    }
}

fn footer(dialog: &SurfaceContent) -> AnyElement {
    let readonly = dialog.presentation.readonly;
    let pending = dialog.request.is_some();
    let id = dialog.surface_id;
    let cancel = dialog.owner.clone();
    let confirm = dialog.owner.clone();
    h_flex()
        .w_full()
        .justify_end()
        .gap_2()
        .child(
            Button::new("dialog-cancel")
                .ghost()
                .label(
                    if matches!(dialog.kind, DialogKind::Settings(_) | DialogKind::Commit(_)) {
                        "Close"
                    } else {
                        "Cancel"
                    },
                )
                .on_click(move |_, window, cx| {
                    let _ = cancel.update(cx, |workspace, cx| {
                        if workspace
                            .dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.read(cx).surface_id == id)
                        {
                            workspace.dismiss(window, cx);
                        }
                    });
                }),
        )
        .child(
            Button::new("dialog-confirm")
                .primary()
                .disabled(readonly || pending)
                .loading(pending)
                .label(if matches!(dialog.kind, DialogKind::DeleteSpace(_)) {
                    "Delete space"
                } else if matches!(dialog.kind, DialogKind::NewSpace) {
                    "Create space"
                } else if matches!(
                    dialog.kind,
                    DialogKind::NewWorktreeConversation(_)
                        | DialogKind::NewWorkspaceConversation(_)
                ) {
                    "Create conversation"
                } else if matches!(dialog.kind, DialogKind::SwitchBranch(_)) {
                    "Switch"
                } else if matches!(dialog.kind, DialogKind::Commit(_)) {
                    "Commit"
                } else if matches!(dialog.kind, DialogKind::RemoveWorktree(_)) {
                    if dialog.force {
                        "Discard changes and remove"
                    } else {
                        "Remove worktree"
                    }
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
            .is_some_and(|dialog| dialog.read(cx).surface_id == id)
        {
            workspace.confirm_dialog(window, cx);
        }
    });
}

/// Closing a layer hides it; the Workspace retains this entity as the draft owner.
fn unhost_surface(owner: &WeakEntity<Workspace>, id: Uuid, cx: &mut App) {
    let _ = owner.update(cx, |workspace, cx| {
        if let Some(dialog) = workspace
            .dialog
            .as_ref()
            .filter(|dialog| dialog.read(cx).surface_id == id)
        {
            dialog.update(cx, |dialog, _| dialog.hosted = false);
            cx.notify();
        }
    });
}

/// The Root captures trigger focus and owns trapping, dismissal, and one focus restoration.
pub(super) fn open_surface(owner: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| {
        let Ok(Some(content)) = owner.update(cx, |workspace, cx| {
            if workspace.quit_requested {
                return None;
            }
            workspace
                .dialog
                .as_ref()
                .filter(|dialog| !dialog.read(cx).hosted)
                .cloned()
        }) else {
            return;
        };
        let (id, kind, title) = content.update(cx, |dialog, _| {
            dialog.hosted = true;
            (dialog.surface_id, dialog.kind.clone(), dialog.title())
        });
        let footer = cx.new(|cx| SurfaceFooter {
            content: content.downgrade(),
            _subscription: cx.observe(&content, |_, _, cx| cx.notify()),
        });
        let host_owner = owner.clone();
        match kind {
            DialogKind::Settings(_) => {
                window.open_sheet(cx, move |sheet, window, _| {
                    let close = host_owner.clone();
                    sheet
                        .title(title)
                        .size((window.rem_size() * 56.).min(window.viewport_size().width))
                        .resizable(false)
                        .px_0()
                        .child(
                            content
                                .clone()
                                .cached(StyleRefinement::default().size_full()),
                        )
                        .footer(footer.clone())
                        .on_close(move |_, _, cx| unhost_surface(&close, id, cx))
                });
            }
            DialogKind::DeleteSpace(_) | DialogKind::RemoveWorktree(_) => {
                window.open_alert_dialog(cx, move |alert, window, _| {
                    let close = host_owner.clone();
                    let confirm = host_owner.clone();
                    alert
                        .title(title)
                        .width((window.rem_size() * 30.).min(window.viewport_size().width))
                        .child(content.clone())
                        .footer(footer.clone())
                        .on_ok(move |_, window, cx| {
                            confirm_surface(&confirm, id, window, cx);
                            false
                        })
                        .on_close(move |_, _, cx| unhost_surface(&close, id, cx))
                });
            }
            _ => {
                window.open_dialog(cx, move |dialog, window, _| {
                    let close = host_owner.clone();
                    let confirm = host_owner.clone();
                    dialog
                        .title(title)
                        .width((window.rem_size() * 30.).min(window.viewport_size().width))
                        .child(content.clone())
                        .footer(footer.clone())
                        .on_ok(move |_, window, cx| {
                            confirm_surface(&confirm, id, window, cx);
                            false
                        })
                        .on_close(move |_, _, cx| unhost_surface(&close, id, cx))
                });
            }
        }
        window.on_next_frame(move |window, cx| {
            let Some(owner) = owner.upgrade() else { return };
            let workspace = owner.read(cx);
            if workspace.quit_requested {
                return;
            }
            let Some(dialog) = workspace.dialog.as_ref().filter(|dialog| {
                let dialog = dialog.read(cx);
                dialog.surface_id == id && dialog.hosted
            }) else {
                return;
            };
            let dialog = dialog.read(cx);
            if matches!(dialog.kind, DialogKind::Settings(_)) {
                // Settings retains its search and selected group. A saved field can
                // be virtualized away; focus the first mounted control instead.
                window.focus_next(cx);
                return;
            }
            let focus = if let Some(select) = dialog
                .select("theme_mode")
                .or_else(|| dialog.select("branch"))
            {
                Some(select.focus_handle(cx))
            } else if let Some(commit) = &dialog.commit {
                Some(commit.message.focus_handle(cx))
            } else {
                dialog
                    .fields
                    .first()
                    .map(|field| field.input.focus_handle(cx))
                    .or_else(|| {
                        dialog
                            .selects
                            .first()
                            .map(|select| select.state.focus_handle(cx))
                    })
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

#[cfg(test)]
mod tests {
    use super::{
        Choice, FieldError, LLAMA_MODEL_REFERENCE, OTHER_LLAMA_MODEL, chosen_llama_model,
        llama_model_choices, ollama_model_choices, retain_concurrent_value, target_branch_choices,
        title_setting_keys,
    };
    use crate::titles;

    fn shown(choices: &[Choice]) -> Vec<(&str, &str, bool)> {
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

    #[test]
    fn title_settings_follow_the_selected_generator() {
        use titles::{LLAMA_MODEL_SETTING as LLAMA, MODEL_SETTING as OLLAMA, PROVIDER_SETTING};
        assert_eq!(
            title_setting_keys("automatic", ""),
            [PROVIDER_SETTING, LLAMA, OLLAMA]
        );
        assert_eq!(
            title_setting_keys("llama_cpp", ""),
            [PROVIDER_SETTING, LLAMA]
        );
        assert_eq!(
            title_setting_keys("ollama", OTHER_LLAMA_MODEL),
            [PROVIDER_SETTING, OLLAMA]
        );
        assert_eq!(
            title_setting_keys("llama_cpp", OTHER_LLAMA_MODEL),
            [PROVIDER_SETTING, LLAMA, LLAMA_MODEL_REFERENCE]
        );
    }

    #[test]
    fn llama_choices_list_the_cache_and_keep_a_saved_download() {
        let default = titles::DEFAULT_LLAMA_MODEL;
        let missing_default = format!("Default ({default}, not downloaded)");
        let cached = Ok(vec!["org/a-GGUF:Q8_0".to_owned()]);
        assert_eq!(
            shown(&llama_model_choices(Some(&cached), "org/b-GGUF:Q4_0")),
            [
                ("", missing_default.as_str(), false),
                ("org/a-GGUF:Q8_0", "org/a-GGUF:Q8_0", false),
                ("org/b-GGUF:Q4_0", "org/b-GGUF:Q4_0 (not downloaded)", false),
                (OTHER_LLAMA_MODEL, "Other Hugging Face model…", false),
            ]
        );
        // Unknown cache: the saved model is listed without a claim about its download.
        assert_eq!(
            shown(&llama_model_choices(None, "org/b-GGUF:Q4_0"))[1],
            ("org/b-GGUF:Q4_0", "org/b-GGUF:Q4_0", false)
        );
        // A cached default appears once, unless it was chosen explicitly.
        let cached = Ok(vec![default.to_owned()]);
        assert_eq!(
            shown(&llama_model_choices(Some(&cached), "")),
            [
                ("", format!("Default ({default})").as_str(), false),
                (OTHER_LLAMA_MODEL, "Other Hugging Face model…", false),
            ]
        );
        assert_eq!(llama_model_choices(Some(&cached), default).len(), 3);
        let failed = Err("llama was not found".to_owned());
        assert_eq!(llama_model_choices(Some(&failed), "").len(), 2);
    }

    #[test]
    fn ollama_choices_mark_a_saved_model_ollama_lacks() {
        let installed = Ok(vec!["lfm2:24b".to_owned()]);
        assert_eq!(
            shown(&ollama_model_choices(Some(&installed), "gone:1b")),
            [
                ("", "First installed model (lfm2:24b)", false),
                ("lfm2:24b", "lfm2:24b", false),
                ("gone:1b", "gone:1b (not installed)", true),
            ]
        );
        let unreachable = Err("Ollama is not running".to_owned());
        assert_eq!(
            shown(&ollama_model_choices(Some(&unreachable), "gone:1b"))[1],
            ("gone:1b", "gone:1b", false)
        );
        assert_eq!(ollama_model_choices(None, "").len(), 1);
    }

    #[test]
    fn target_branch_choices_offer_local_branches() {
        let branches = ["main".to_owned(), "release".to_owned()];
        assert_eq!(
            shown(&target_branch_choices(Some(&branches), "gone")),
            [
                ("", "Current branch when the thread starts", false),
                ("main", "main", false),
                ("release", "release", false),
                ("gone", "gone (not a local branch)", true),
            ]
        );
        assert_eq!(target_branch_choices(Some(&branches), "main").len(), 3);
        assert_eq!(
            shown(&target_branch_choices(None, "main"))[1],
            ("main", "main", false)
        );
    }

    #[test]
    fn other_llama_model_requires_a_reference() {
        assert_eq!(
            chosen_llama_model("org/a:Q8_0".into(), "ignored".into()).unwrap(),
            "org/a:Q8_0"
        );
        assert_eq!(
            chosen_llama_model(OTHER_LLAMA_MODEL.into(), "org/new:Q4_0".into()).unwrap(),
            "org/new:Q4_0"
        );
        let error = chosen_llama_model(OTHER_LLAMA_MODEL.into(), String::new()).unwrap_err();
        assert_eq!(
            error.downcast_ref::<FieldError>().map(|error| error.key),
            Some(LLAMA_MODEL_REFERENCE)
        );
    }

    #[test]
    fn unchanged_settings_draft_preserves_completed_install_path() {
        assert_eq!(
            retain_concurrent_value(
                "",
                String::new(),
                "/managed/pi/lib/node_modules/pi/dist/cli.js"
            ),
            "/managed/pi/lib/node_modules/pi/dist/cli.js"
        );
        assert_eq!(
            retain_concurrent_value(
                "/old/pi",
                "/chosen/pi".into(),
                "/managed/pi/lib/node_modules/pi/dist/cli.js"
            ),
            "/chosen/pi"
        );
    }
}
