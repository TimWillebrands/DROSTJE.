use super::{
    SurfaceContent,
    fields::{Choice, Controls},
};
use crate::app::{AppState, WorktreeRequestStatus};
use gpui_kit::{
    component::{ActiveTheme, Sizable, alert::Alert, form::Form, h_flex, spinner::Spinner},
    prelude::FluentBuilder,
    *,
};
use uuid::Uuid;

pub(super) fn workspace_controls(
    project: &Uuid,
    state: &mut AppState,
    controls: &mut Controls,
    window: &mut Window,
    cx: &mut App,
) {
    let root = state
        .snapshot
        .projects
        .iter()
        .find(|candidate| candidate.id == *project)
        .map(|project| project.root.display().to_string())
        .unwrap_or_default();
    let mut choices = vec![Choice::new("project", format!("Current checkout — {root}"))];
    choices.extend(
        state
            .snapshot
            .worktrees
            .iter()
            .filter(|worktree| worktree.project_id == *project && worktree.is_live())
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
    controls.select("workspace", "Workspace", choices, &selected, window, cx);
}

pub(super) fn worktree_controls(
    project: &Uuid,
    state: &mut AppState,
    controls: &mut Controls,
    window: &mut Window,
    cx: &mut App,
) {
    let repository = state
        .repositories
        .get(project)
        .and_then(|repository| repository.as_ref().ok());
    let branches: Vec<String> = repository
        .map(|repository| repository.branches.clone())
        .unwrap_or_default();
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
        controls.select(
            "base",
            "Base branch",
            branches
                .iter()
                .map(|branch| Choice::new(branch.clone(), branch.clone()))
                .collect(),
            &selected,
            window,
            cx,
        );
    }
}

impl SurfaceContent {
    pub(super) fn worktree_request_changed(&mut self, request: Uuid, cx: &mut Context<Self>) {
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
            _ => {}
        }
        cx.notify();
    }

    pub(super) fn workspace_body(
        &self,
        body: Stateful<Div>,
        readonly: bool,
        cx: &App,
    ) -> Stateful<Div> {
        body
                    .child(div().text_sm().text_color(cx.theme().muted_foreground)
                        .child("Starts a new conversation in the selected directory. The current conversation, its draft, and its session stay where they are. Worktrees can be shared by several conversations."))
                    .child(div().id("workspace-choice").test_support()
                        .child(Form::new().child(self.form_field("workspace", readonly, cx))))
    }

    pub(super) fn worktree_body(
        &self,
        mut body: Stateful<Div>,
        project: Uuid,
        state: &AppState,
        readonly: bool,
        cx: &App,
    ) -> Stateful<Div> {
        let pending = self.request.is_some();
        let location = state
            .snapshot
            .projects
            .iter()
            .find(|candidate| candidate.id == project)
            .map(|project| crate::worktrees::worktree_dir(&state.data_dir, &project.root, "…"));
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
        body
    }

    pub(super) fn remove_worktree_body(
        &self,
        mut body: Stateful<Div>,
        id: Uuid,
        state: &AppState,
        cx: &App,
    ) -> Stateful<Div> {
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
        body
    }
}
