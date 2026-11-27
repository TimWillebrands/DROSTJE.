mod appearance;
mod commit;
mod diagnostics;
mod fields;
mod hosts;
mod layer;
mod page;
mod phones;
mod presentation;
mod title_models;
mod worktrees;

use super::workspace::Workspace;
use crate::app::{AppState, PresentationChange};
use crate::titles::InstalledModels;
use appearance::ThemeNames;
use commit::CommitDraft;
use fields::{Controls, SelectField, field_error};
use gpui_kit::{
    component::{
        Sizable, ThemeRegistry,
        alert::Alert,
        form::Form,
        input::{InputEvent, InputState, TextareaState},
        select::SelectEvent,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use layer::{confirm_surface, footer};
use presentation::SurfacePresentation;
use uuid::Uuid;

pub(in crate::ui) use commit::render_commit_panel;
pub(in crate::ui) use fields::Choice;
pub(in crate::ui) use layer::{close_surface, open_surface};
pub(in crate::ui) use phones::selected_phone;

#[derive(Clone, PartialEq, Eq)]
pub enum DialogKind {
    NewSpace,
    DeleteSpace(Uuid),
    NewWorktreeConversation(Uuid),
    NewWorkspaceConversation(Uuid),
    RemoveWorktree(Uuid),
    Commit(Uuid),
    Settings(Option<Uuid>),
    PairPhone,
}
pub enum SurfaceOutcome {
    Applied,
    SpaceCreated(Uuid),
    DeleteSpace(Uuid),
    Pending(Uuid),
}
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub input: Entity<InputState>,
    pub(crate) initial: String,
}

pub struct SurfaceContent {
    pub kind: DialogKind,
    pub fields: Vec<Field>,
    pub error: Option<String>,
    error_field: Option<&'static str>,
    pub request: Option<Uuid>,
    last_request: Option<Uuid>,
    force: bool,
    selects: Vec<SelectField>,
    loaded_remote_host: String,
    title_models: Option<InstalledModels>,
    theme_names: ThemeNames,
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
        let theme_names = if matches!(kind, DialogKind::Settings(_)) {
            ThemeNames::read(cx)
        } else {
            ThemeNames::default()
        };
        let (fields, selects) = state.update(cx, |state, cx| {
            let mut controls = Controls::default();
            match &kind {
                DialogKind::NewSpace => {
                    controls.field("name", "Space name", String::new(), window, cx)
                }
                DialogKind::NewWorkspaceConversation(project) => {
                    worktrees::workspace_controls(project, state, &mut controls, window, cx)
                }
                DialogKind::NewWorktreeConversation(project) => {
                    worktrees::worktree_controls(project, state, &mut controls, window, cx)
                }
                DialogKind::RemoveWorktree(_) => {}
                DialogKind::Settings(project) => {
                    page::settings_controls(project, &theme_names, state, &mut controls, window, cx)
                }
                _ => {}
            }
            (controls.fields, controls.selects)
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
            subscriptions.push(cx.subscribe_in(
                &select.state,
                window,
                move |surface, _, _: &SelectEvent<Vec<Choice>>, window, cx| {
                    surface.clear_field_error(key, cx);
                    if key == "remote_host" {
                        surface.load_remote_host(window, cx);
                    }
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
            loaded_remote_host: String::new(),
            title_models,
            theme_names,
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

    pub(in crate::ui) fn surface_id(&self) -> Uuid {
        self.surface_id
    }
    pub(in crate::ui) fn hosted(&self) -> bool {
        self.hosted
    }
    pub(in crate::ui) fn unhost(&mut self, cx: &mut Context<Self>) {
        self.hosted = false;
        cx.notify();
    }

    pub(in crate::ui) fn last_request(&self) -> Option<Uuid> {
        self.last_request
    }

    pub fn begin_request(&mut self, request: Uuid, cx: &mut Context<Self>) {
        self.request = Some(request);
        self.last_request = Some(request);
        self.error = None;
        self.error_field = None;
        cx.notify();
    }

    fn refresh_presentation(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        if !self.presentation.matches(&self.kind, state) {
            self.presentation = SurfacePresentation::new(&self.kind, state);
            cx.notify();
        }
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

            DialogKind::Settings(project_id) => self.apply_settings(project_id, state, cx)?,
            DialogKind::PairPhone => {
                anyhow::bail!("Pair phone is confirmed from its own buttons")
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
            DialogKind::Commit(_) => "Commit changes",
            DialogKind::Settings(_) => "Settings",
            DialogKind::PairPhone => "Pair phone",
        }
    }

    fn body(&self, state: &AppState, owner: &WeakEntity<SurfaceContent>, cx: &App) -> AnyElement {
        let readonly = state.storage_error.is_some();
        let mut body = v_flex().id("dialog-body").w_full().gap_5().pb_4();
        match self.kind {
            DialogKind::Settings(project_id) => {
                return self.settings_body(project_id, state, owner, cx);
            }
            DialogKind::NewWorkspaceConversation(_) => {
                body = self.workspace_body(body, readonly, cx);
            }
            DialogKind::NewWorktreeConversation(project) => {
                body = self.worktree_body(body, project, state, readonly, cx);
            }
            DialogKind::Commit(thread) => {
                body = self.commit_body(body, thread, state, owner, readonly, cx);
            }
            DialogKind::RemoveWorktree(id) => {
                body = self.remove_worktree_body(body, id, state, cx);
            }
            DialogKind::NewSpace => {
                body = body.child(Form::new().child(self.form_field("name", readonly, cx)));
            }
            DialogKind::PairPhone => {
                body = self.pair_phone_body(body, cx);
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
