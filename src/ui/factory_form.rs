use super::{settings::Field, workspace::Workspace};
use crate::{
    app::{AppState, NoticeLevel, PresentationChange},
    factory::{self, FactoryLaunchSpec, FactorySource, Liveness},
    model::*,
    platform,
};
use anyhow::{Context as _, Result, ensure};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        collapsible::Collapsible,
        dialog::DialogButtonProps,
        form::{Field as FormField, Form},
        h_flex,
        input::{Input, InputEvent, InputState},
        select::{Select, SelectEvent, SelectState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::Value;
use std::{cell::Cell, path::PathBuf, rc::Rc};
use uuid::Uuid;

const HARNESSES: [&str; 3] = ["opencode", "codex", "claude-code"];
const SOURCES: [&str; 2] = ["Work item", "Existing folder"];

struct FormError {
    field: Option<&'static str>,
    text: String,
}

impl FormError {
    fn field(field: &'static str, text: impl Into<String>) -> Self {
        Self {
            field: Some(field),
            text: text.into(),
        }
    }

    fn general(error: impl std::fmt::Display) -> Self {
        Self {
            field: None,
            text: error.to_string(),
        }
    }
}

pub struct FactoryThreadForm {
    pub thread_id: ThreadId,
    state: Entity<AppState>,
    owner: WeakEntity<Workspace>,
    project_id: ProjectId,
    config: FactoryThreadConfig,
    fields: Vec<Field>,
    source: Entity<SelectState<Vec<String>>>,
    harness: Entity<SelectState<Vec<String>>>,
    pub(super) focus: FocusHandle,
    advanced: bool,
    error: Option<FormError>,
    readiness: Option<Result<String, String>>,
    readiness_request: Option<Uuid>,
    _subscriptions: Vec<Subscription>,
}

impl FactoryThreadForm {
    fn config(&self, cx: &App) -> FactoryThreadConfig {
        let mut config = self.config.clone();
        for field in &self.fields {
            let value = field.input.read(cx).value().to_string();
            match field.key {
                "source" => config.source = value,
                "model" => config.model = value,
                "target_branch" => config.target_branch = value,
                "thought_level" => config.thought_level = value,
                _ => unreachable!("Factory form field"),
            }
        }
        if let Some(value) = self.harness.read(cx).selected_value() {
            config.harness = value.clone();
        }
        config.source_kind = match self.source.read(cx).selected_value().map(String::as_str) {
            Some("Existing folder") => FactorySourceKind::Folder,
            _ => FactorySourceKind::WorkItem,
        };
        config
    }

    fn input(&self, key: &str) -> &Entity<InputState> {
        &self
            .fields
            .iter()
            .find(|field| field.key == key)
            .expect("Factory form field")
            .input
    }

    fn field_error(&self, key: &str) -> Option<&str> {
        self.error
            .as_ref()
            .filter(|error| error.field == Some(key))
            .map(|error| error.text.as_str())
    }
}

impl FactoryThreadForm {
    fn new(
        thread: &Thread,
        state: Entity<AppState>,
        owner: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let thread_id = thread.id;
        let project_id = thread.project_id;
        let config = thread
            .factory
            .clone()
            .expect("Factory threads require configuration");
        let mut fields = Vec::new();
        for (key, label, value) in [
            ("source", "Source", &config.source),
            ("model", "Model", &config.model),
            ("target_branch", "Target branch", &config.target_branch),
            ("thought_level", "Thought level", &config.thought_level),
        ] {
            let input = cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_value(value.clone(), window, cx);
                input
            });
            fields.push(Field {
                key,
                label,
                input,
                initial: value.clone(),
            });
        }
        let source = cx.new(|cx| {
            let mut select =
                SelectState::new(SOURCES.map(str::to_owned).to_vec(), None, window, cx);
            let selected = match config.source_kind {
                FactorySourceKind::WorkItem => SOURCES[0],
                FactorySourceKind::Folder => SOURCES[1],
            };
            select.set_selected_value(&selected.to_owned(), window, cx);
            select
        });
        let harness = cx.new(|cx| {
            let mut select =
                SelectState::new(HARNESSES.map(str::to_owned).to_vec(), None, window, cx);
            select.set_selected_value(&config.harness, window, cx);
            select
        });
        let mut subscriptions = Vec::new();
        for field in &fields {
            let key = field.key;
            subscriptions.push(cx.subscribe_in(
                &field.input,
                window,
                move |form, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        let value = input.read(cx).value().to_string();
                        form.config_changed(
                            move |config| match key {
                                "source" => config.source = value,
                                "model" => config.model = value,
                                "target_branch" => config.target_branch = value,
                                "thought_level" => config.thought_level = value,
                                _ => unreachable!("Factory form field"),
                            },
                            cx,
                        );
                        if !input.focus_handle(cx).is_focused(window) {
                            form.commit_edits(cx);
                        }
                    } else if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        form.commit_edits(cx);
                    }
                },
            ));
        }
        subscriptions.push(cx.subscribe_in(
            &source,
            window,
            move |form, _, event: &SelectEvent<Vec<String>>, _, cx| {
                let SelectEvent::Confirm(Some(value)) = event else {
                    return;
                };
                let kind = if value == SOURCES[1] {
                    FactorySourceKind::Folder
                } else {
                    FactorySourceKind::WorkItem
                };
                form.config_changed(move |config| config.source_kind = kind, cx);
                form.commit_edits(cx);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &harness,
            window,
            move |form, _, event: &SelectEvent<Vec<String>>, _, cx| {
                let SelectEvent::Confirm(Some(value)) = event else {
                    return;
                };
                let value = value.clone();
                form.config_changed(move |config| config.harness = value, cx);
                form.commit_edits(cx);
            },
        ));
        subscriptions.push(cx.observe_window_activation(window, |form, window, cx| {
            if !window.is_window_active() {
                form.commit_edits(cx);
            }
        }));
        subscriptions.push(cx.observe(&state, |_, _, cx| cx.notify()));
        subscriptions.push(cx.observe_global::<gpui_kit::component::Theme>(|_, cx| cx.notify()));
        subscriptions.push(
            cx.subscribe(&state, |form, _, change: &PresentationChange, cx| {
                if matches!(change, PresentationChange::Thread(id) if *id == form.thread_id)
                    || (matches!(change, PresentationChange::Persistence)
                        && form.state.read(cx).storage_error.is_some())
                {
                    cx.notify();
                }
            }),
        );
        Self {
            thread_id,
            state,
            owner,
            project_id,
            config,
            fields,
            source,
            harness,
            focus: cx.focus_handle(),
            advanced: false,
            error: None,
            readiness: None,
            readiness_request: None,
            _subscriptions: subscriptions,
        }
    }

    fn config_changed(
        &mut self,
        change: impl FnOnce(&mut FactoryThreadConfig),
        cx: &mut Context<Self>,
    ) {
        let thread_id = self.thread_id;
        let config = self.state.update(cx, |state, _| {
            if state.pending_factory.contains(&thread_id) {
                return None;
            }
            let mut thread = editable_thread(state, thread_id).ok()?.clone();
            let config = thread.factory.as_mut()?;
            change(config);
            let config = config.clone();
            if state
                .snapshot
                .threads
                .iter()
                .any(|old| old.id == thread_id && old.factory.as_ref() == Some(&config))
            {
                return Some(config);
            }
            thread.updated_at = platform::now();
            state.edit_thread(thread);
            Some(config)
        });
        if let Some(config) = config {
            let branch_warning = |config: &FactoryThreadConfig| {
                self.state
                    .read(cx)
                    .repositories
                    .get(&self.project_id)
                    .is_some_and(|repository| {
                        repository.as_ref().is_ok_and(|repository| {
                            repository.dirty
                                && repository.branch.as_deref() == Some(config.target_branch.trim())
                        })
                    })
            };
            let presentation_changed = self.config.source_kind != config.source_kind
                || self.config.harness != config.harness
                || branch_warning(&self.config) != branch_warning(&config)
                || self.error.is_some()
                || self.readiness.is_some()
                || self.readiness_request.is_some();
            self.config = config;
            self.error = None;
            self.readiness = None;
            self.readiness_request = None;
            if presentation_changed {
                cx.notify();
            }
        }
    }

    fn commit_edits(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if state.commit_thread_edits(self.thread_id) {
                cx.emit(PresentationChange::Persistence);
            }
        });
    }
}

impl Workspace {
    pub fn sync_factory_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let thread = state.factory_thread().filter(|thread| {
            !state
                .snapshot
                .launches
                .iter()
                .any(|launch| launch.factory_thread_id == thread.id)
        });
        let Some(thread) = thread else {
            if self.factory_form.take().is_some() {
                cx.notify();
            }
            return;
        };
        if self
            .factory_form
            .as_ref()
            .is_some_and(|form| form.read(cx).thread_id == thread.id)
        {
            return;
        }
        let thread = thread.clone();
        let state = self.state.clone();
        let owner = cx.entity().downgrade();
        self.factory_form =
            Some(cx.new(|cx| FactoryThreadForm::new(&thread, state, owner, window, cx)));
        cx.notify();
    }

    fn factory_form_error(
        &mut self,
        thread_id: ThreadId,
        error: FormError,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = self
            .factory_form
            .as_ref()
            .filter(|form| form.read(cx).thread_id == thread_id)
        {
            form.update(cx, |form, cx| form.set_error(error, cx));
        } else if !self
            .state
            .read(cx)
            .snapshot
            .launches
            .iter()
            .any(|launch| launch.factory_thread_id == thread_id)
        {
            self.state.update(cx, |state, cx| {
                if state.storage_error.is_none() {
                    state.notice(
                        NoticeLevel::Error,
                        format!("Factory thread {thread_id}: {}", error.text),
                    );
                }
                cx.notify();
            });
        }
    }

    fn finish_factory_start(
        &mut self,
        thread_id: ThreadId,
        error: Option<FormError>,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            state.pending_factory.remove(&thread_id);
            cx.emit(PresentationChange::Thread(thread_id));
        });
        if let Some(error) = error {
            self.factory_form_error(thread_id, error, cx);
        }
    }
}

impl FactoryThreadForm {
    fn set_error(&mut self, error: FormError, cx: &mut Context<Self>) {
        if error.field == Some("target_branch") || error.field == Some("thought_level") {
            self.advanced = true;
        }
        self.error = Some(error);
        cx.notify();
    }

    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread_id = self.thread_id;
        let state = self.state.read(cx);
        if state.pending_factory.contains(&thread_id) {
            return;
        }
        let Ok(thread) = editable_thread(state, thread_id) else {
            return;
        };
        let config = thread.factory.as_ref().expect("Factory configuration");
        if config.source_kind != FactorySourceKind::Folder {
            return;
        }
        let previous = config.source.clone();
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose Factory folder".into()),
        });
        cx.spawn_in(window, async move |form, window| {
            let selected = result.await;
            let _ = form.update_in(window, |form, window, cx| {
                let state = form.state.read(cx);
                if state.pending_factory.contains(&thread_id) {
                    return;
                }
                let Ok(thread) = editable_thread(state, thread_id) else {
                    return;
                };
                let config = thread.factory.as_ref().expect("Factory configuration");
                if config.source_kind != FactorySourceKind::Folder || config.source != previous {
                    return;
                }
                match selected {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.first() {
                            let value = path.to_string_lossy().into_owned();
                            form.config_changed(|config| config.source = value.clone(), cx);
                            form.input("source")
                                .update(cx, |input, cx| input.set_value(value, window, cx));
                        }
                    }
                    Ok(Ok(None)) => {}
                    error => form.set_error(
                        FormError::field("source", format!("Folder picker failed: {error:?}")),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    fn check_readiness(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread_id = self.thread_id;
        let state = self.state.read(cx);
        if self.readiness_request.is_some() || state.pending_factory.contains(&thread_id) {
            return;
        }
        let Ok(thread) = editable_thread(state, thread_id) else {
            return;
        };
        let harness = self.config(cx).harness;
        if !HARNESSES.contains(&harness.as_str()) {
            self.set_error(unsupported_harness(&harness), cx);
            return;
        }
        let cwd = state
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == thread.project_id)
            .expect("Factory project")
            .root
            .clone();
        let executable = configured_executable(state);
        let request = Uuid::new_v4();
        self.readiness_request = Some(request);
        self.readiness = None;
        self.error = None;
        cx.spawn_in(window, async move |form, window| {
            let result = factory::check_readiness(executable, cwd, harness)
                .await
                .map_err(|error| format!("Readiness refused:\n{error:#}"));
            let _ = form.update_in(window, |form, _, cx| {
                if form.readiness_request == Some(request) {
                    form.readiness_request = None;
                    form.readiness = Some(result);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}

impl Workspace {
    pub fn check_factory_readiness(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.state.read(cx).factory_thread().map(|thread| thread.id) else {
            return;
        };
        if let Some(form) = self
            .factory_form
            .as_ref()
            .filter(|form| form.read(cx).thread_id == thread_id)
        {
            form.update(cx, |form, cx| form.check_readiness(window, cx));
        }
    }

    pub fn start_factory_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.state.read(cx).factory_thread().map(|thread| thread.id) else {
            return;
        };
        self.start_factory_run_for(thread_id, window, cx);
    }

    fn start_factory_run_for(
        &mut self,
        thread_id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self
            .factory_form
            .as_ref()
            .filter(|form| form.read(cx).thread_id == thread_id)
        else {
            return;
        };
        if form.read(cx).readiness_request.is_some()
            || self.state.read(cx).pending_factory.contains(&thread_id)
        {
            return;
        }
        let config = form.read(cx).config(cx);
        // Capture raw inputs, including incomplete values, before validating or starting work.
        form.update(cx, |form, cx| {
            form.config_changed(|saved| *saved = config.clone(), cx)
        });
        let spec = match factory_spec(self.state.read(cx), thread_id, &config) {
            Ok(spec) => spec,
            Err(error) => {
                self.factory_form_error(thread_id, error, cx);
                return;
            }
        };
        let state = self.state.read(cx);
        let unresolved: Vec<_> = state
            .snapshot
            .launches
            .iter()
            .filter(|launch| {
                !state.pending_factory.contains(&launch.factory_thread_id)
                    && launch.launch_error.is_none()
                    && state.factory_runs.get(&launch.id).is_none_or(|snapshot| {
                        !snapshot.reported_state.terminal()
                            && !matches!(snapshot.liveness, Liveness::OwnedRunning)
                    })
            })
            .filter_map(|launch| {
                serde_json::from_value::<FactoryLaunchSpec>(launch.descriptor.clone())
                    .ok()
                    .map(|spec| (launch.id, spec))
            })
            .collect();
        self.state.update(cx, |state, cx| {
            state.pending_factory.insert(thread_id);
            cx.emit(PresentationChange::Thread(thread_id));
        });
        cx.spawn_in(window, async move |workspace, window| {
            let result = smol::unblock(move || {
                let spec = validate_source(spec)?;
                let prior: Vec<_> = unresolved
                    .into_iter()
                    .filter(|(_, old)| factory::same_source(old, &spec))
                    .map(|(id, _)| id)
                    .collect();
                Ok::<_, FormError>((spec, prior))
            })
            .await;
            let _ = workspace.update_in(window, |workspace, window, cx| match result {
                Err(error) => workspace.finish_factory_start(thread_id, Some(error), cx),
                Ok((spec, prior)) if prior.is_empty() => {
                    workspace.dispatch_factory_run(thread_id, config, spec, window, cx)
                }
                Ok((spec, prior)) => {
                    workspace.confirm_factory_start(thread_id, config, spec, prior, window, cx)
                }
            });
        })
        .detach();
    }

    fn confirm_factory_start(
        &mut self,
        thread_id: ThreadId,
        config: FactoryThreadConfig,
        spec: FactoryLaunchSpec,
        prior: Vec<LaunchId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A delayed validation result must not interrupt another thread or surface.
        if self.state.read(cx).selected_thread != Some(thread_id)
            || self.dialog.is_some()
            || self.extension.is_some()
            || self.palette.is_some()
            || window.has_active_dialog(cx)
        {
            self.finish_factory_start(thread_id, Some(FormError::general("A prior run has unresolved state. Return to this Factory thread and Start run to review it.")), cx);
            return;
        }
        let source = match &spec.source {
            FactorySource::AzureWorkItem(id) => format!("Work item #{id}"),
            FactorySource::LocalFolder(path) => format!("Work item folder {}", path.display()),
        };
        let description = format!(
            "{source} has unresolved prior launch state and may still be running. Starting another run can duplicate work.\n\nPrior launch{}:\n{}",
            if prior.len() == 1 { "" } else { "es" },
            prior
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
        let owner = cx.entity().downgrade();
        let accepted = Rc::new(Cell::new(false));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let confirm = owner.clone();
            let close = owner.clone();
            let config = config.clone();
            let spec = spec.clone();
            let did_accept = accepted.clone();
            let closed_after_accept = accepted.clone();
            alert
                .title("Start another Factory run?")
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Start another run")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    did_accept.set(true);
                    let mut spec = spec.clone();
                    spec.confirm_unknown = true;
                    let _ = confirm.update(cx, |workspace, cx| {
                        workspace.dispatch_factory_run(thread_id, config.clone(), spec, window, cx)
                    });
                    true
                })
                .on_close(move |_, _, cx| {
                    if !closed_after_accept.get() {
                        let _ = close.update(cx, |workspace, cx| {
                            workspace.finish_factory_start(thread_id, None, cx)
                        });
                    }
                })
        });
    }

    fn dispatch_factory_run(
        &mut self,
        thread_id: ThreadId,
        config: FactoryThreadConfig,
        spec: FactoryLaunchSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = self.state.update(cx, |state, cx| {
            let thread = editable_thread(state, thread_id)?.clone();
            ensure!(
                state.pending_factory.contains(&thread_id),
                "Factory start was canceled"
            );
            ensure!(
                thread.factory.as_ref() == Some(&config),
                "Factory inputs changed; start again with the current configuration"
            );
            let manager = state
                .factory
                .clone()
                .context("Factory tracking is unavailable; see Output")?;
            let persisted = state.persist_thread(thread);
            cx.emit(PresentationChange::Persistence);
            Ok::<_, anyhow::Error>((manager, persisted))
        });
        let (manager, persisted) = match result {
            Ok(result) => result,
            Err(error) => {
                self.finish_factory_start(
                    thread_id,
                    Some(FormError::general(format!("Nothing dispatched: {error:#}"))),
                    cx,
                );
                return;
            }
        };
        cx.spawn_in(window, async move |workspace, window| {
            let result = async {
                persisted
                    .recv()
                    .await
                    .context("Factory thread persistence stopped")?
                    .map_err(anyhow::Error::msg)?;
                // The ACK is a barrier over all queued edits; shutdown or a changed owner still refuses dispatch.
                workspace.update_in(window, |workspace, _, cx| {
                    let state = workspace.state.read(cx);
                    let thread = editable_thread(state, thread_id)?;
                    ensure!(
                        state.pending_factory.contains(&thread_id)
                            && thread.factory.as_ref() == Some(&config),
                        "Factory start was canceled or inputs changed"
                    );
                    Ok::<_, anyhow::Error>(())
                })??;
                manager.dispatch(thread_id, spec).await
            }
            .await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                let error = result
                    .err()
                    .map(|error| FormError::general(format!("Start refused or failed: {error:#}")));
                workspace.finish_factory_start(thread_id, error, cx);
                // FactorySnapshot owns the transition, including durable prerequisite failures.
                workspace.sync_factory_form(window, cx);
            });
        })
        .detach();
    }
}

fn editable_thread(state: &AppState, thread_id: ThreadId) -> Result<&Thread> {
    ensure!(!state.shutting_down, "Application is shutting down");
    ensure!(
        state.storage_error.is_none(),
        "Storage is unavailable; resolve the storage error before starting"
    );
    let thread = state
        .snapshot
        .threads
        .iter()
        .find(|thread| thread.id == thread_id && thread.kind == ThreadKind::Factory)
        .context("Factory thread is unavailable")?;
    ensure!(!thread.archived, "Archived threads are read-only");
    let project = state
        .snapshot
        .projects
        .iter()
        .find(|project| project.id == thread.project_id)
        .context("Factory project is unavailable")?;
    ensure!(!project.archived, "Archived projects are read-only");
    ensure!(
        !state
            .snapshot
            .launches
            .iter()
            .any(|launch| launch.factory_thread_id == thread_id),
        "This thread already owns a run; create a New Factory thread"
    );
    Ok(thread)
}

fn unsupported_harness(harness: &str) -> FormError {
    FormError::field(
        "harness",
        format!("Unsupported harness {harness:?}. Choose opencode, codex, or claude-code."),
    )
}

fn configured_executable(state: &AppState) -> Option<PathBuf> {
    state
        .snapshot
        .settings
        .get("executables")
        .and_then(|value| value.get("factory"))
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
}

fn factory_spec(
    state: &AppState,
    thread_id: ThreadId,
    config: &FactoryThreadConfig,
) -> Result<FactoryLaunchSpec, FormError> {
    let thread = editable_thread(state, thread_id).map_err(FormError::general)?;
    if state.factory.is_none() {
        return Err(FormError::general(
            "Factory tracking is unavailable; see Output",
        ));
    }
    if !HARNESSES.contains(&config.harness.as_str()) {
        return Err(unsupported_harness(&config.harness));
    }
    let source = match config.source_kind {
        FactorySourceKind::WorkItem => {
            let id = config
                .source
                .trim()
                .parse::<i32>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or_else(|| {
                    FormError::field("source", "Enter a positive Azure DevOps User Story ID")
                })?;
            FactorySource::AzureWorkItem(id)
        }
        FactorySourceKind::Folder => {
            if config.source.trim().is_empty() {
                return Err(FormError::field(
                    "source",
                    "Choose an existing folder containing a readable PRD.md",
                ));
            }
            FactorySource::LocalFolder(PathBuf::from(config.source.trim()))
        }
    };
    if config.target_branch.trim().is_empty() {
        return Err(FormError::field(
            "target_branch",
            "Choose an existing local target branch",
        ));
    }
    let data_root = state
        .snapshot
        .settings
        .get("factory_data_root")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(|path| PathBuf::from(path.trim()))
        .ok_or_else(|| FormError::general("Configure the shared Factory data root in Settings"))?;
    if !data_root.is_absolute() {
        return Err(FormError::general(
            "The shared Factory data root in Settings must be absolute",
        ));
    }
    let project = state
        .snapshot
        .projects
        .iter()
        .find(|project| project.id == thread.project_id)
        .expect("Factory project");
    let id = Uuid::new_v4();
    let optional = |value: &str| (!value.trim().is_empty()).then(|| value.trim().to_owned());
    Ok(FactoryLaunchSpec {
        id,
        project_id: project.id,
        source,
        target_repo: project.root.clone(),
        target_branch: config.target_branch.trim().to_owned(),
        data_root,
        log_dir: state
            .data_dir
            .join("factory-launches")
            .join(id.to_string())
            .join("logs"),
        harness: config.harness.clone(),
        model: optional(&config.model),
        thought_level: optional(&config.thought_level),
        executable: configured_executable(state),
        confirm_unknown: false,
    })
}

fn validate_source(mut spec: FactoryLaunchSpec) -> Result<FactoryLaunchSpec, FormError> {
    let repository = platform::inspect_repository(&spec.target_repo)
        .map_err(|error| FormError::general(format!("Repository is unavailable: {error:#}")))?;
    spec.target_repo = repository.root;
    platform::validate_branch(&spec.target_repo, &spec.target_branch)
        .map_err(|error| FormError::field("target_branch", format!("{error:#}")))?;
    if !repository.branches.contains(&spec.target_branch) {
        return Err(FormError::field(
            "target_branch",
            "Choose an existing local branch",
        ));
    }
    if let FactorySource::LocalFolder(folder) = &mut spec.source {
        *folder = std::fs::canonicalize(&*folder).map_err(|error| {
            FormError::field("source", format!("Factory folder is unavailable: {error}"))
        })?;
        let prd = folder.join("PRD.md");
        let file = std::fs::File::open(&prd).map_err(|error| {
            FormError::field("source", format!("Cannot read {}: {error}", prd.display()))
        })?;
        if !file
            .metadata()
            .map_err(|error| FormError::field("source", error.to_string()))?
            .is_file()
        {
            return Err(FormError::field("source", "PRD.md must be a regular file"));
        }
    }
    if spec.data_root.exists() && !spec.data_root.is_dir() {
        return Err(FormError::general(
            "The shared Factory data root in Settings is not a directory",
        ));
    }
    Ok(spec)
}

fn input_field(
    form: &FactoryThreadForm,
    key: &str,
    label: &'static str,
    disabled: bool,
    cx: &App,
) -> FormField {
    FormField::new().label(label).child(
        v_flex()
            .key_context("NativeInput")
            .gap_1()
            .child(Input::new(form.input(key)).disabled(disabled))
            .when_some(form.field_error(key), |view, error| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error.to_owned()),
                )
            }),
    )
}

impl Render for FactoryThreadForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render(self, &cx.entity().downgrade(), cx)
    }
}

fn render(form: &FactoryThreadForm, owner: &WeakEntity<FactoryThreadForm>, cx: &App) -> AnyElement {
    let state = form.state.read(cx);
    let thread_id = form.thread_id;
    let project = state
        .snapshot
        .projects
        .iter()
        .find(|project| project.id == form.project_id);
    let pending = state.pending_factory.contains(&thread_id);
    let disabled =
        pending || form.readiness_request.is_some() || editable_thread(state, thread_id).is_err();
    let folder = form.config.source_kind == FactorySourceKind::Folder;
    let pick = owner.clone();
    let ready = owner.clone();
    let start = form.owner.clone();
    let advanced = owner.clone();
    let mut source = input_field(
        form,
        "source",
        if folder {
            "Existing folder"
        } else {
            "Azure DevOps User Story ID"
        },
        disabled,
        cx,
    );
    if folder {
        source = source
            .description(
                "Existing PRD/issues folder. DROSTJE. does not generate or edit its contents.",
            )
            .child(
                Button::new("factory-pick-folder")
                    .small()
                    .label("Choose folder…")
                    .disabled(disabled)
                    .on_click(move |_, window, cx| {
                        let _ = pick.update(cx, |form, cx| form.pick_folder(window, cx));
                    }),
            );
    }
    let harness_error = form.field_error("harness").map(str::to_owned).or_else(|| {
        (!HARNESSES.contains(&form.config.harness.as_str()))
            .then(|| unsupported_harness(&form.config.harness).text)
    });
    let fields = Form::new()
        .child(
            FormField::new().label("Source").child(
                div()
                    .key_context("NativeInput")
                    .child(Select::new(&form.source).disabled(disabled)),
            ),
        )
        .child(source)
        .child(
            FormField::new().label("Harness").child(
                v_flex()
                    .key_context("NativeInput")
                    .gap_1()
                    .child(
                        Select::new(&form.harness)
                            .placeholder("Choose a supported harness")
                            .disabled(disabled),
                    )
                    .when_some(harness_error, |view, error| {
                        view.child(div().text_sm().text_color(cx.theme().danger).child(error))
                    }),
            ),
        )
        .child(
            input_field(form, "model", "Model", disabled, cx)
                .description("provider/model; leave blank for the harness default"),
        );
    let mut branch = input_field(form, "target_branch", "Target branch", disabled, cx);
    if let Some(Ok(repository)) = state.repositories.get(&form.project_id)
        && repository.dirty
        && repository.branch.as_deref() == Some(form.config.target_branch.trim())
    {
        branch = branch.child(div().text_sm().text_color(cx.theme().warning).child("Factory uses this branch’s committed state; uncommitted working-tree changes are not included."));
    }
    let context = v_flex()
        .gap_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(format!(
            "Repository: {}",
            project
                .map(|project| project.root.display().to_string())
                .unwrap_or_else(|| "unavailable".into())
        ))
        .child(format!(
            "Factory data root: {}",
            state
                .snapshot
                .settings
                .get("factory_data_root")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .unwrap_or("configure in Settings")
        ));
    let advanced_fields = v_flex()
        .gap_3()
        .child(
            Form::new().child(branch).child(
                input_field(form, "thought_level", "Thought level", disabled, cx)
                    .description("Optional harness setting"),
            ),
        )
        .child(context);
    v_flex()
        .id("factory-thread-form")
        .key_context("FactoryThreadForm")
        .track_focus(&form.focus)
        .size_full()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .p_4()
        .text_color(cx.theme().foreground)
        .child(
            v_flex()
                .w_full()
                .max_w(rems(42.))
                .mx_auto()
                .gap_3()
                .child(fields)
                .child(
                    Collapsible::new()
                        .open(form.advanced)
                        .child(
                            Button::new("factory-advanced")
                                .small()
                                .ghost()
                                .label(if form.advanced {
                                    "Advanced −"
                                } else {
                                    "Advanced +"
                                })
                                .on_click(move |_, _, cx| {
                                    let _ = advanced.update(cx, |form, cx| {
                                        form.advanced = !form.advanced;
                                        cx.notify();
                                    });
                                }),
                        )
                        .content(advanced_fields),
                )
                .when_some(form.readiness.as_ref(), |view, result| {
                    view.child(
                        div()
                            .id("factory-readiness-result")
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_sm()
                            .whitespace_normal()
                            .text_color(if result.is_err() {
                                cx.theme().danger
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(match result {
                                Ok(output) | Err(output) => output.clone(),
                            }),
                    )
                })
                .when_some(
                    form.error.as_ref().filter(|error| error.field.is_none()),
                    |view, error| {
                        view.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(error.text.clone()),
                        )
                    },
                )
                .when(state.factory.is_none(), |view| {
                    view.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child("Factory tracking is unavailable; see Output."),
                    )
                })
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .justify_end()
                        .child(
                            Button::new("factory-readiness")
                                .label(if form.readiness_request.is_some() {
                                    "Checking readiness…"
                                } else {
                                    "Check readiness"
                                })
                                .disabled(disabled)
                                .on_click(move |_, window, cx| {
                                    let _ = ready
                                        .update(cx, |form, cx| form.check_readiness(window, cx));
                                }),
                        )
                        .child(
                            Button::new("factory-start-run")
                                .primary()
                                .label(if pending {
                                    "Starting run…"
                                } else {
                                    "Start run"
                                })
                                .disabled(disabled || state.factory.is_none())
                                .on_click(move |_, window, cx| {
                                    let _ = start.update(cx, |workspace, cx| {
                                        workspace.start_factory_run_for(thread_id, window, cx)
                                    });
                                }),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Factory runs independently and continues after DROSTJE. closes."),
                ),
        )
        .into_any_element()
}

#[cfg(all(test, feature = "runtime-qa"))]
#[path = "runtime_qa.rs"]
mod runtime_qa;
