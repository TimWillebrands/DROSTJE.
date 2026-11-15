//! The Commit form: its message draft, the generated subject, and the inline commit panel.
use super::{DialogKind, SurfaceContent};
use crate::app::{AppState, CommitSubjectStatus};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable,
        alert::Alert,
        button::Button,
        form::{Field as FormField, Form},
        h_flex,
        input::{Textarea, TextareaState},
        spinner::Spinner,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use uuid::Uuid;

/// The Commit dialog's message and the generated subject it asked for.
pub(super) struct CommitDraft {
    pub(super) message: Entity<TextareaState>,
    /// The subject request this dialog started; results of other requests are ignored.
    pub(super) subject_request: Option<Uuid>,
    /// The message when the request started: a subject replaces it only if the user has not
    /// edited it since, so typed text is never overwritten.
    pub(super) baseline: SharedString,
    /// Why the last request produced no subject, shown next to the control that retries.
    pub(super) note: Option<String>,
}

impl SurfaceContent {
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

    pub(super) fn commit_subject_changed(
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

    pub(super) fn commit_body(
        &self,
        mut body: Stateful<Div>,
        thread: Uuid,
        state: &AppState,
        owner: &WeakEntity<SurfaceContent>,
        readonly: bool,
        cx: &App,
    ) -> Stateful<Div> {
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
                                .child(Textarea::new(&commit.message).disabled(readonly || pending))
                                .when(self.error_field == Some("message"), |view| {
                                    view.when_some(self.error.clone(), |view, error| {
                                        view.child(
                                            Alert::error("field-error-message", error).small(),
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
                                    let _ = surface
                                        .update(cx, |surface, cx| surface.write_commit_subject(cx));
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
        body
    }
}

/// Renders a retained Commit surface inline, including its existing message body and actions.
pub(in crate::ui) fn render_commit_panel(content: Entity<SurfaceContent>, cx: &App) -> AnyElement {
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
