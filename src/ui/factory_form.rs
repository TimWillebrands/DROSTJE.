use super::{
    settings::Field,
    workspace::{Region, Workspace},
};
use crate::{
    factory::{self, FactoryLaunchSpec, FactorySource, Liveness, WorkPacket},
    model::*,
    neovim::{DraftKey, DraftSnapshot},
    platform,
};
use anyhow::{Context as _, Result, ensure};
use gpui_kit::{
    component::{
        Disableable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        select::{Select, SelectEvent, SelectState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FactoryFormKind {
    Task,
    Folder,
    Azure,
}

pub struct FactoryReview {
    spec: FactoryLaunchSpec,
    draft: Option<DraftSnapshot>,
    dirty: bool,
    prior_unknown: Vec<LaunchId>,
}

pub struct FactoryForm {
    id: Uuid,
    project: Project,
    thread: Option<ThreadId>,
    message: Option<factory::OriginMessage>,
    kind: FactoryFormKind,
    fields: Vec<Field>,
    harness: Entity<SelectState<Vec<String>>>,
    pub(super) focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    review: Option<FactoryReview>,
    error: Option<String>,
    readiness: Option<String>,
    busy: bool,
    pending_handoff: Option<String>,
    auto_handoff: bool,
    _subscriptions: Vec<Subscription>,
}

impl FactoryForm {
    fn value(&self, key: &str, cx: &App) -> String {
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.input.read(cx).value().trim().to_string())
            .unwrap_or_default()
    }
    fn harness_value(&self, cx: &App) -> String {
        self.harness
            .read(cx)
            .selected_value()
            .cloned()
            .unwrap_or_default()
    }
}

impl Workspace {
    pub fn open_factory_form(
        &mut self,
        kind: FactoryFormKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.factory_form.is_some() {
            return;
        }
        if self.extension.is_some() {
            self.state.update(cx, |state, cx| {
                state.notice(
                    "Finish or cancel the extension response before editing a Factory task",
                );
                cx.notify();
            });
            return;
        }
        self.dismiss(window, cx);
        let state = self.state.read(cx);
        let Some(project) = state.project().cloned() else {
            return;
        };
        let saved = state
            .snapshot
            .settings
            .get(&format!("factory_draft:{}", project.id))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let handoff = self.handoff_message.take();
        let thread = handoff
            .as_ref()
            .map(|(id, _, _)| *id)
            .or_else(|| {
                saved
                    .get("origin_thread")
                    .and_then(Value::as_str)
                    .and_then(|id| Uuid::parse_str(id).ok())
            })
            .or(state.selected_thread);
        let message = if kind == FactoryFormKind::Task {
            handoff
                .as_ref()
                .map(|(_, index, text)| factory::OriginMessage {
                    index: *index,
                    displayed_text: text.clone(),
                })
                .or_else(|| {
                    saved
                        .get("origin_message")
                        .and_then(|value| serde_json::from_value(value.clone()).ok())
                })
        } else {
            None
        };
        let repository_branch = state
            .repositories
            .get(&project.id)
            .and_then(|info| info.as_ref().ok())
            .and_then(|info| info.branch.clone());
        let auto_handoff = handoff.is_some()
            && state
                .drafts
                .get(&DraftKey::Factory(project.id))
                .map(|draft| draft.text.is_empty())
                .unwrap_or_else(|| {
                    saved
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .is_empty()
                });
        let previous_focus = window.focused(cx);
        let focus = cx.focus_handle();
        let mut fields = Vec::new();
        let mut field = |key, label, value| {
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_value(value, window, cx);
                input
            });
            fields.push(Field { key, label, input });
        };
        if kind == FactoryFormKind::Task {
            field(
                "title",
                "Task title",
                saved
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            );
        }
        if kind == FactoryFormKind::Folder {
            field(
                "folder",
                "Existing Factory folder (must contain readable PRD.md)",
                String::new(),
            );
        }
        if kind == FactoryFormKind::Azure {
            field(
                "work_item",
                "Existing Azure work-item ID (positive integer)",
                String::new(),
            );
        }
        let branch = saved
            .get("target_branch")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(project.defaults.factory_target_branch.clone())
            .or(repository_branch)
            .unwrap_or_default();
        field("target_branch", "Local target branch", branch);
        field(
            "model",
            "Factory model (blank: harness default)",
            saved
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(project.defaults.factory_model.clone())
                .unwrap_or_default(),
        );
        field(
            "thought_level",
            "Thought level (optional)",
            saved
                .get("thought_level")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(project.defaults.factory_thought_level.clone())
                .unwrap_or_default(),
        );
        let harness_value = saved
            .get("harness")
            .and_then(Value::as_str)
            .unwrap_or(&project.defaults.factory_harness)
            .to_owned();
        let harness = cx.new(|cx| {
            let mut select = SelectState::new(
                ["omp", "opencode", "codex", "claude-code"]
                    .map(str::to_owned)
                    .to_vec(),
                None,
                window,
                cx,
            );
            select.set_selected_value(&harness_value, window, cx);
            select
        });
        let mut subscriptions = Vec::new();
        for field in &fields {
            subscriptions.push(cx.subscribe_in(
                &field.input,
                window,
                |workspace, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        workspace.factory_form_changed(cx);
                    }
                },
            ));
        }
        subscriptions.push(cx.subscribe_in(
            &harness,
            window,
            |workspace, _, _: &SelectEvent<Vec<String>>, _, cx| workspace.factory_form_changed(cx),
        ));
        let body = saved
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let pending_handoff = handoff.map(|(_, _, text)| text);
        let id = Uuid::new_v4();
        self.factory_form = Some(FactoryForm {
            id,
            project: project.clone(),
            thread,
            message,
            kind,
            fields,
            harness,
            focus: focus.clone(),
            previous_focus,
            review: None,
            error: None,
            readiness: None,
            busy: false,
            pending_handoff,
            auto_handoff,
            _subscriptions: subscriptions,
        });
        if kind == FactoryFormKind::Task {
            self.editor_override = Some((DraftKey::Factory(project.id), body));
            self.sync_session(window, cx);
        }
        focus.focus(window, cx);
        self.factory_form_changed(cx);
        cx.notify();
    }

    fn factory_form_changed(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.factory_form else {
            return;
        };
        form.review = None;
        form.readiness = None;
        let key = format!("factory_draft:{}", form.project.id);
        let mut value = self
            .state
            .read(cx)
            .snapshot
            .settings
            .get(&key)
            .cloned()
            .unwrap_or_else(|| json!({}));
        for field in &form.fields {
            if field.key != "folder" && field.key != "work_item" {
                value[field.key] = json!(field.input.read(cx).value().to_string());
            }
        }
        value["harness"] = json!(form.harness_value(cx));
        if form.kind == FactoryFormKind::Task {
            value["origin_message"] = json!(form.message);
            value["origin_thread"] = json!(form.thread);
        }
        self.state.update(cx, |state, cx| {
            state.set_setting(&key, value);
            cx.notify();
        });
        cx.notify();
    }

    pub fn close_factory_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.factory_form.as_ref().is_some_and(|form| form.busy) {
            return;
        }
        let Some(form) = self.factory_form.take() else {
            return;
        };
        self.editor_override = None;
        self.sync_session(window, cx);
        if let Some(focus) = form.previous_focus {
            focus.focus(window, cx);
        }
        cx.notify();
    }
    pub(super) fn apply_initial_factory_handoff(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.factory_form.as_ref().is_some_and(|form| {
            form.auto_handoff
                && !form.busy
                && self.editor_selection == Some(DraftKey::Factory(form.project.id))
        }) {
            self.use_factory_handoff(window, cx);
        }
    }

    pub fn use_factory_handoff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.factory_form else {
            return;
        };
        if form.busy || self.editor_switching {
            return;
        }
        let Some(text) = form.pending_handoff.clone() else {
            return;
        };
        let Some(editor) = self.state.read(cx).editor.clone() else {
            return;
        };
        let id = form.id;
        let key = DraftKey::Factory(form.project.id);
        form.busy = true;
        let automatic = form.auto_handoff;
        form.auto_handoff = false;
        cx.spawn_in(window, async move |workspace, window| {
            let result = async {
                let previous = editor.snapshot(key.clone()).await?;
                ensure!(!automatic || previous.text.is_empty(), "Factory draft is no longer empty; confirm replacement with the selected message");
                ensure!(editor.replace_if_revision(key, previous.revision(), text).await?, "Factory draft changed; selected message was not applied");
                Ok::<_, anyhow::Error>(())
            }.await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                if let Some(form) = workspace.factory_form.as_mut().filter(|form| form.id == id) {
                    form.busy = false;
                    match result { Ok(()) => { form.pending_handoff = None; form.review = None; }, Err(error) => form.error = Some(format!("{error:#}")) }
                    if let Some(editor) = &workspace.editor { editor.update(cx, |editor, cx| editor.focus(window, cx)); }
                    cx.notify();
                }
            });
        }).detach();
        cx.notify();
    }

    pub fn pick_factory_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.factory_form.as_ref().map(|form| form.id) else {
            return;
        };
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Factory Folder".into()),
        });
        cx.spawn_in(window, async move |workspace, window| {
            let selected = result.await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                if let Some(form) = workspace.factory_form.as_mut().filter(|form| form.id == id) {
                    match selected {
                        Ok(Ok(Some(paths))) => {
                            if let Some(path) = paths.first() {
                                if let Some(field) =
                                    form.fields.iter().find(|field| field.key == "folder")
                                {
                                    field.input.update(cx, |input, cx| {
                                        input.set_value(
                                            path.to_string_lossy().into_owned(),
                                            window,
                                            cx,
                                        )
                                    });
                                }
                            }
                        }
                        Ok(Ok(None)) => {}
                        error => form.error = Some(format!("Folder picker failed: {error:?}")),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn check_factory_readiness(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.factory_form else {
            return;
        };
        if form.busy {
            return;
        }
        let id = form.id;
        let cwd = form.project.root.clone();
        let harness = form.harness_value(cx);
        let executable = configured_executable(self.state.read(cx));
        form.busy = true;
        form.error = None;
        cx.spawn_in(window, async move |workspace, window| {
            let result = factory::check_readiness(executable, cwd, harness).await;
            let _ = workspace.update_in(window, |workspace, _, cx| {
                if let Some(form) = workspace.factory_form.as_mut().filter(|form| form.id == id) {
                    form.busy = false;
                    form.readiness = Some(match result {
                        Ok(output) => output,
                        Err(error) => format!("Readiness refused:\n{error:#}"),
                    });
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn review_factory_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.factory_form else {
            return;
        };
        if form.busy {
            return;
        }
        let result = self.factory_spec(cx);
        let (spec, editor) = match result {
            Ok(result) => result,
            Err(error) => {
                self.factory_form.as_mut().unwrap().error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let form = self.factory_form.as_mut().unwrap();
        let id = form.id;
        form.busy = true;
        form.error = None;
        cx.spawn_in(window, async move |workspace, window| {
            let result = async {
                let draft = if let Some(editor) = editor {
                    Some(editor.snapshot(DraftKey::Factory(spec.project_id)).await?)
                } else {
                    None
                };
                let mut spec = spec;
                if let Some(draft) = &draft {
                    ensure!(!draft.text.trim().is_empty(), "Enter a task before review");
                    spec.packet.as_mut().unwrap().task = draft.text.clone();
                }
                let (spec, dirty) = smol::unblock(move || validate_review(spec)).await?;
                Ok::<_, anyhow::Error>((spec, draft, dirty))
            }
            .await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                if workspace
                    .factory_form
                    .as_ref()
                    .is_none_or(|form| form.id != id)
                {
                    return;
                }
                let review = result.map(|(spec, draft, dirty)| {
                    let state = workspace.state.read(cx);
                    let prior_unknown = state
                        .snapshot
                        .launches
                        .iter()
                        .filter(|launch| {
                            if launch.launch_error.is_some() {
                                return false;
                            }
                            let Ok(old) = serde_json::from_value::<FactoryLaunchSpec>(
                                launch.descriptor.clone(),
                            ) else {
                                return false;
                            };
                            let same = match (&old.source, &spec.source) {
                                (FactorySource::LocalFolder(a), FactorySource::LocalFolder(b)) => {
                                    a == b
                                }
                                (
                                    FactorySource::AzureWorkItem(a),
                                    FactorySource::AzureWorkItem(b),
                                ) => a == b && old.data_root == spec.data_root,
                                _ => false,
                            };
                            same && state.factory_runs.get(&launch.id).is_none_or(|snapshot| {
                                !snapshot.reported_state.terminal()
                                    && !matches!(snapshot.liveness, Liveness::OwnedRunning)
                            })
                        })
                        .map(|launch| launch.id)
                        .collect();
                    if let Some(draft) = &draft {
                        workspace.state.update(cx, |state, cx| {
                            state.record_draft(draft.clone());
                            cx.notify();
                        });
                    }
                    FactoryReview {
                        spec,
                        draft,
                        dirty,
                        prior_unknown,
                    }
                });
                let form = workspace.factory_form.as_mut().unwrap();
                form.busy = false;
                match review {
                    Ok(review) => {
                        form.review = Some(review);
                        form.focus.focus(window, cx);
                    }
                    Err(error) => form.error = Some(format!("Nothing dispatched: {error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn factory_spec(
        &self,
        cx: &App,
    ) -> Result<(FactoryLaunchSpec, Option<crate::neovim::NvimEditor>)> {
        let state = self.state.read(cx);
        ensure!(
            !state.shutting_down && state.storage_error.is_none(),
            "Storage is unavailable or application is shutting down"
        );
        ensure!(
            state.factory.is_some(),
            "Factory tracking is unavailable; see diagnostics"
        );
        let form = self
            .factory_form
            .as_ref()
            .context("Open a Factory task form")?;
        ensure!(!form.project.archived, "Archived projects are read-only");
        let id = Uuid::new_v4();
        let source = match form.kind {
            FactoryFormKind::Task => {
                FactorySource::LocalFolder(state.data_dir.join("work-packets").join(id.to_string()))
            }
            FactoryFormKind::Folder => {
                let folder = form.value("folder", cx);
                ensure!(!folder.is_empty(), "Choose a Factory folder");
                FactorySource::LocalFolder(PathBuf::from(folder))
            }
            FactoryFormKind::Azure => {
                let work_item = form
                    .value("work_item", cx)
                    .parse::<i32>()
                    .context("Enter a positive Azure work-item ID")?;
                ensure!(work_item > 0, "Azure work-item ID must be positive");
                FactorySource::AzureWorkItem(work_item)
            }
        };
        let editor = if form.kind == FactoryFormKind::Task {
            ensure!(
                form.pending_handoff.is_none(),
                "Apply the selected message or explicitly keep the existing Factory draft"
            );
            ensure!(
                !self.editor_switching
                    && self.editor_selection == Some(DraftKey::Factory(form.project.id)),
                "Wait for the Factory draft to activate"
            );
            ensure!(
                !self
                    .editor
                    .as_ref()
                    .is_some_and(|view| view.read(cx).has_composition()),
                "Complete or cancel input composition before reviewing"
            );
            Some(state.editor.clone().context("Neovim is unavailable")?)
        } else {
            None
        };
        let optional = |key| {
            let value = form.value(key, cx);
            (!value.is_empty()).then_some(value)
        };
        let packet = if form.kind == FactoryFormKind::Task {
            let title = form.value("title", cx);
            ensure!(!title.is_empty(), "Enter a task title");
            Some(WorkPacket {
                title,
                task: String::new(),
            })
        } else {
            None
        };
        let data_root = state
            .snapshot
            .settings
            .get("factory_data_root")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .context("Configure the shared Factory data root in Settings")?;
        Ok((
            FactoryLaunchSpec {
                id,
                project_id: form.project.id,
                thread_id: form.thread,
                origin_message: form.message.clone(),
                source,
                target_repo: form.project.root.clone(),
                target_branch: form.value("target_branch", cx),
                data_root,
                log_dir: state
                    .data_dir
                    .join("factory-launches")
                    .join(id.to_string())
                    .join("logs"),
                harness: form.harness_value(cx),
                model: optional("model"),
                thought_level: optional("thought_level"),
                executable: configured_executable(state),
                packet,
                confirm_unknown: false,
            },
            editor,
        ))
    }

    pub fn dispatch_factory_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.factory_form else {
            return;
        };
        if form.busy {
            return;
        }
        let Some(review) = &form.review else {
            return;
        };
        if !review.prior_unknown.is_empty() && !review.spec.confirm_unknown {
            form.error =
                Some("Review the prior launch state and explicitly confirm re-dispatch".into());
            cx.notify();
            return;
        }
        let Some(manager) = self.state.read(cx).factory.clone() else {
            return;
        };
        let spec = review.spec.clone();
        let draft = review.draft.clone();
        let editor = self.state.read(cx).editor.clone();
        let id = form.id;
        let project_id = spec.project_id;
        form.busy = true;
        form.error = None;
        self.state.update(cx, |state, cx| {
            state.pending_factory.insert(project_id);
            cx.notify();
        });
        cx.spawn_in(window, async move |workspace, window| {
            let result = async {
                if let Some(draft) = &draft {
                    let editor = editor.as_ref().context("Editor disappeared; review again")?;
                    let current = editor.snapshot(draft.key.clone()).await?;
                    ensure!(current.revision() == draft.revision() && current.text == draft.text, "Task changed after review; nothing dispatched. Review again");
                }
                let launch_id = manager.dispatch(spec).await?;
                let clear = if let (Some(editor), Some(draft)) = (&editor, &draft) { Some(editor.replace_if_revision(draft.key.clone(), draft.revision(), String::new()).await) } else { None };
                Ok::<_, anyhow::Error>((launch_id, clear))
            }.await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.state.update(cx, |state, cx| { state.pending_factory.remove(&project_id); cx.notify(); });
                if let Some(form) = workspace.factory_form.as_mut().filter(|form| form.id == id) {
                    form.busy = false;
                    match result {
                        Ok((launch_id, clear)) => {
                            workspace.state.update(cx, |state, cx| {
                                if matches!(clear, Some(Ok(false)) | Some(Err(_))) { state.notice("Factory accepted the launch; the newer/editor-unavailable task draft was retained"); }
                                state.select_factory_launch(launch_id, cx);
                                state.notice(format!("Factory launch {launch_id} accepted. Closing Drost will not stop it."));
                                cx.notify();
                            });
                            workspace.close_factory_form(window, cx);
                            workspace.focus_region(Region::Factory, window, cx);
                        }
                        Err(error) => { form.error = Some(format!("Dispatch refused or failed: {error:#}")); form.review = None; }
                    }
                    cx.notify();
                }
            });
        }).detach();
        cx.notify();
    }
}

fn configured_executable(state: &crate::app::AppState) -> Option<PathBuf> {
    state
        .snapshot
        .settings
        .get("executables")
        .and_then(|value| value.get("factory"))
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
}

fn validate_review(mut spec: FactoryLaunchSpec) -> Result<(FactoryLaunchSpec, bool)> {
    let repository = platform::inspect_repository(&spec.target_repo)?;
    spec.target_repo = repository.root;
    ensure!(
        !spec.target_branch.is_empty(),
        "Detached HEAD or no branch selected; choose an existing local branch"
    );
    platform::validate_branch(&spec.target_repo, &spec.target_branch)?;
    ensure!(
        repository.branches.contains(&spec.target_branch),
        "Choose an existing local branch"
    );
    if spec.packet.is_none() {
        if let FactorySource::LocalFolder(folder) = &mut spec.source {
            *folder = std::fs::canonicalize(&*folder).context("Factory folder is unavailable")?;
            let prd = folder.join("PRD.md");
            let file = std::fs::File::open(&prd)
                .with_context(|| format!("Cannot read {}", prd.display()))?;
            ensure!(file.metadata()?.is_file(), "PRD.md is not a regular file");
        }
    }
    ensure!(
        spec.data_root.is_absolute(),
        "Shared Factory data root must be absolute"
    );
    Ok((spec, repository.dirty))
}

pub fn render(
    form: &FactoryForm,
    workspace: &Workspace,
    owner: &WeakEntity<Workspace>,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let state = workspace.state.read(cx);
    let close = owner.clone();
    let ready = owner.clone();
    let review = owner.clone();
    let dispatch = owner.clone();
    let mut body = v_flex()
        .id("factory-form-body")
        .overflow_y_scroll()
        .max_h((window.viewport_size().height - px(220.)).max(px(200.)))
        .gap_3();
    body = body.child(format!(
        "{} · {}",
        form.project.name,
        form.project.root.display()
    ));
    if let Some(reviewed) = &form.review {
        let spec = &reviewed.spec;
        body = body
            .child(div().text_lg().child("Review exact dispatch"))
            .child(format!(
                "Target: {} · branch {}",
                spec.target_repo.display(),
                spec.target_branch
            ))
            .child(format!(
                "Harness: {} · model: {} · thought level: {}",
                spec.harness,
                spec.model.as_deref().unwrap_or("harness default"),
                spec.thought_level.as_deref().unwrap_or("default")
            ))
            .child(format!("Shared Factory root: {}", spec.data_root.display()));
        body = body.child(match &spec.source {
            FactorySource::LocalFolder(path) => format!("Local packet: {}", path.display()),
            FactorySource::AzureWorkItem(id) => format!("Azure work item: {id}"),
        });
        if reviewed.dirty {
            body = body.child(div().text_color(rgb(0xE5C07B)).child("Worktree is dirty. Factory uses committed Git state; uncommitted changes are not included. Nothing is committed, stashed or cleaned by Drost."));
        }
        if let Some(packet) = &spec.packet {
            body = body
                .child(div().text_lg().child(packet.title.clone()))
                .child(
                    div()
                        .font_family("Menlo")
                        .text_sm()
                        .whitespace_normal()
                        .child(packet.task.clone()),
                );
        }
        for id in &reviewed.prior_unknown {
            let mut prior = v_flex()
                .p_2()
                .gap_1()
                .border_1()
                .border_color(rgb(0xE5C07B))
                .child(format!("Prior launch {id}"));
            if let Some(snapshot) = state.factory_runs.get(id) {
                prior = prior.child(snapshot.status_text()).child(format!(
                    "Work item {:?} · attempt {}",
                    snapshot.launch.work_item_id,
                    snapshot.launch.attempt_id.as_deref().unwrap_or("unknown")
                ));
                for diagnostic in &snapshot.diagnostics {
                    prior = prior.child(diagnostic.clone());
                }
                if let Some(path) = &snapshot.launch.event_path {
                    prior = prior.child(super::factory::path_actions("prior-events", path));
                }
                for note in snapshot.notes.iter().rev().take(3).rev() {
                    prior = prior.child(format!("{} · {}", note.event_type, note.payload));
                }
            } else {
                prior = prior.child("Launch outcome unknown; event replay has not established terminal state or process ownership.");
            }
            body = body.child(prior);
        }
        if !reviewed.prior_unknown.is_empty() {
            let confirm = owner.clone();
            body = body.child(
                Button::new("factory-confirm-unknown")
                    .small()
                    .label(if spec.confirm_unknown {
                        "Prior state reviewed; re-dispatch explicitly confirmed"
                    } else {
                        "I reviewed prior state; allow a new dispatch"
                    })
                    .disabled(form.busy || spec.confirm_unknown)
                    .on_click(move |_, _, cx| {
                        let _ = confirm.update(cx, |workspace, cx| {
                            if let Some(review) = workspace
                                .factory_form
                                .as_mut()
                                .and_then(|form| form.review.as_mut())
                            {
                                review.spec.confirm_unknown = true;
                            }
                            cx.notify();
                        });
                    }),
            );
        }
        let edit = owner.clone();
        body = body.child(
            Button::new("factory-edit-review")
                .small()
                .ghost()
                .label("Back to editing")
                .disabled(form.busy)
                .on_click(move |_, _, cx| {
                    let _ = edit.update(cx, |workspace, cx| {
                        if let Some(form) = &mut workspace.factory_form {
                            form.review = None;
                        }
                        cx.notify();
                    });
                }),
        );
    } else {
        if form.kind == FactoryFormKind::Folder {
            let pick = owner.clone();
            body = body.child("Factory may update this folder's frontmatter and ledger. Drost passes the existing packet unchanged.").child(Button::new("factory-pick-folder").small().label("Choose Folder…").disabled(form.busy).on_click(move |_, window, cx| { let _ = pick.update(cx, |workspace, cx| workspace.pick_factory_folder(window, cx)); }));
        }
        if form.kind == FactoryFormKind::Azure {
            body = body.child("Uses existing Factory/Azure authentication. No work item is created and no Azure configuration is changed.");
        }
        for field in &form.fields {
            body = body.child(
                v_flex()
                    .key_context("NativeInput")
                    .gap_1()
                    .child(field.label)
                    .child(Input::new(&field.input).disabled(form.busy)),
            );
        }
        body = body.child(
            v_flex()
                .key_context("NativeInput")
                .gap_1()
                .child("Factory harness (independent of interactive OMP)")
                .child(Select::new(&form.harness).disabled(form.busy)),
        );
        if let Some(Ok(repository)) = state.repositories.get(&form.project.id) {
            body = body.child(div().text_xs().child(format!(
                "Local branches: {}",
                repository.branches.join(", ")
            )));
        }
        if form.kind == FactoryFormKind::Task {
            if let Some(text) = &form.pending_handoff {
                let apply = owner.clone();
                let keep = owner.clone();
                body = body.child("Selected message (not the entire conversation). Apply it explicitly if replacing an existing Factory draft:")
                    .child(div().font_family("Menlo").text_xs().whitespace_normal().child(text.clone()))
                    .child(h_flex().gap_2().child(Button::new("factory-use-message").small().label("Use selected message").disabled(form.busy || workspace.editor_switching).on_click(move |_, window, cx| { let _ = apply.update(cx, |workspace, cx| workspace.use_factory_handoff(window, cx)); }))
                    .child(Button::new("factory-keep-draft").small().ghost().label("Keep existing draft instead").disabled(form.busy).on_click(move |_, _, cx| { let _ = keep.update(cx, |workspace, cx| { if let Some(form) = &mut workspace.factory_form { form.pending_handoff = None; form.message = None; } workspace.factory_form_changed(cx); }); })));
            }
            body = body.child("Markdown task · real Neovim");
            if let Some(editor) = &workspace.editor {
                body = body.child(
                    div()
                        .h(px(240.))
                        .border_1()
                        .border_color(rgb(0x343A46))
                        .child(editor.clone()),
                );
            }
            if let Some(error) = &state.editor_error {
                body = body.child(div().text_color(rgb(0xE6A0A0)).child(error.clone()));
            }
            let cancel_editor = owner.clone();
            let restart = owner.clone();
            body = body.child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("factory-cancel-editor-command")
                            .small()
                            .ghost()
                            .label("Cancel Editor Command")
                            .on_click(move |_, window, cx| {
                                let _ = cancel_editor.update(cx, |workspace, cx| {
                                    workspace.cancel_editor_command(window, cx)
                                });
                            }),
                    )
                    .child(
                        Button::new("factory-restart-editor")
                            .small()
                            .ghost()
                            .label("Restart Editor")
                            .on_click(move |_, window, cx| {
                                let _ = restart.update(cx, |workspace, cx| {
                                    workspace.restart_editor(window, cx)
                                });
                            }),
                    ),
            );
        }
    }
    if let Some(output) = &form.readiness {
        body = body.child(
            div()
                .font_family("Menlo")
                .text_xs()
                .whitespace_normal()
                .child(output.clone()),
        );
    }
    if let Some(error) = &form.error {
        body = body.child(div().text_color(rgb(0xE6A0A0)).child(error.clone()));
    }
    let disabled = form.busy || state.storage_error.is_some() || state.shutting_down;
    v_flex()
        .key_context("Dialog FactoryForm")
        .track_focus(&form.focus)
        .w(px(840.))
        .p_4()
        .gap_3()
        .bg(rgb(0x20242B))
        .border_1()
        .border_color(rgb(0x343A46))
        .rounded_lg()
        .child(div().text_lg().child(match form.kind {
            FactoryFormKind::Task => "Send to Factory",
            FactoryFormKind::Folder => "Open Factory Folder",
            FactoryFormKind::Azure => "Dispatch Azure Work Item",
        }))
        .child(body)
        .child("Only explicit Dispatch launches work. Factory jobs continue after Drost exits.")
        .child(
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("factory-close-form")
                        .small()
                        .ghost()
                        .label("Close")
                        .disabled(form.busy)
                        .on_click(move |_, window, cx| {
                            let _ = close.update(cx, |workspace, cx| {
                                workspace.close_factory_form(window, cx)
                            });
                        }),
                )
                .child(
                    Button::new("factory-readiness")
                        .small()
                        .label("Check Readiness")
                        .disabled(disabled)
                        .on_click(move |_, window, cx| {
                            let _ = ready.update(cx, |workspace, cx| {
                                workspace.check_factory_readiness(window, cx)
                            });
                        }),
                )
                .when(form.review.is_none(), |view| {
                    view.child(
                        Button::new("factory-review")
                            .small()
                            .primary()
                            .label(if form.busy {
                                "Working…"
                            } else {
                                "Review Task"
                            })
                            .disabled(disabled)
                            .on_click(move |_, window, cx| {
                                let _ = review.update(cx, |workspace, cx| {
                                    workspace.review_factory_task(window, cx)
                                });
                            }),
                    )
                })
                .when(form.review.is_some(), |view| {
                    view.child(
                        Button::new("factory-dispatch")
                            .small()
                            .primary()
                            .label(if form.busy {
                                "Checking and dispatching…"
                            } else {
                                "Dispatch"
                            })
                            .disabled(
                                disabled
                                    || form.review.as_ref().is_some_and(|review| {
                                        !review.prior_unknown.is_empty()
                                            && !review.spec.confirm_unknown
                                    }),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = dispatch.update(cx, |workspace, cx| {
                                    workspace.dispatch_factory_review(window, cx)
                                });
                            }),
                    )
                }),
        )
        .into_any_element()
}

#[cfg(all(test, feature = "runtime-qa"))]
#[path = "runtime_qa.rs"]
mod runtime_qa;
