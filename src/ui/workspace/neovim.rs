use super::super::terminal::{NeovimTarget, neovim_jump_expression, neovim_remote};
use super::*;
use crate::file_refs::{self, FileReference};
use gpui_kit::component::{
    Disableable,
    alert::Alert,
    input::{Textarea, TextareaState},
    label::Label,
    spinner::Spinner,
};

struct RemoteFileDialog {
    state: Entity<AppState>,
    thread: ThreadId,
    path: String,
    text: Entity<TextareaState>,
    _subscription: Subscription,
}

impl Render for RemoteFileDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let file = state
            .remote_file
            .as_ref()
            .filter(|file| file.thread == self.thread && file.path == self.path);
        let mut body = v_flex()
            .id("remote-file-view")
            .test_support()
            .gap_3()
            .min_w_0()
            .child(
                Label::new(state.thread_host_label(self.thread))
                    .secondary("Read-only Host file · never opened from the local Project"),
            );
        if let Some(file) = file {
            if file.loading {
                body = body
                    .child(Spinner::new())
                    .child(Label::new("Reading from Host…"));
            } else if let Some(error) = &file.error {
                body = body.child(Alert::error("remote-file-error", error.clone()));
            } else {
                body = body.child(Textarea::new(&self.text).readonly(true).h(rems(24.)));
                if file.truncated {
                    body = body.child(Alert::warning(
                        "remote-file-truncated",
                        "This Host file exceeds the preview limit; only its beginning is shown.",
                    ));
                }
            }
        } else {
            body = body.child(Alert::info(
                "remote-file-unavailable",
                "This file view is no longer active. Close it and open the file again.",
            ));
        }
        let loading = file.is_some_and(|file| file.loading);
        body = body.child(
            Button::new("retry-remote-file")
                .label(Command::RetryRemoteFile.label())
                .disabled(loading)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(RetryRemoteFile), cx)),
        );
        body.on_action(cx.listener(|view, _: &RetryRemoteFile, _, cx| {
            cx.stop_propagation();
            view.state.update(cx, |state, cx| {
                if Command::RetryRemoteFile.disabled_reason(state).is_none() {
                    state.request_remote_file(view.thread, view.path.clone(), cx);
                    cx.notify();
                }
            });
        }))
    }
}

pub(super) fn neovim_setting(state: &AppState) -> Option<String> {
    state
        .snapshot
        .settings
        .get("executables")
        .and_then(|value| value.get("nvim"))
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(str::to_owned)
}

impl Workspace {
    pub(super) fn new_neovim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        if let Some((project, worktree, root)) = self.terminal_location(cx) {
            self.add_terminal(
                project,
                worktree,
                root,
                Some(NeovimTarget::default()),
                window,
                cx,
            );
        }
    }

    pub(super) fn open_conversation_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| thread.id)
        else {
            return;
        };
        self.dismiss(window, cx);
        if self.navigation_blocked(window, cx) {
            return;
        }
        let references = self
            .state
            .read(cx)
            .conversations
            .get(&thread)
            .map(conversation::file_references)
            .unwrap_or_default();
        self.palette = Some(Palette::open_files(
            cx.entity().downgrade(),
            self.state.clone(),
            thread,
            references,
            window,
            cx,
        ));
        cx.notify();
    }

    pub(in crate::ui) fn open_file_reference(
        &mut self,
        reference: FileReference,
        thread: Option<ThreadId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote_thread = {
            let state = self.state.read(cx);
            thread
                .or_else(|| state.conversation_thread().map(|thread| thread.id))
                .and_then(|id| state.snapshot.threads.iter().find(|thread| thread.id == id))
                .filter(|thread| thread.remote.is_some())
                .map(|thread| thread.id)
        };
        if let Some(thread) = remote_thread {
            self.open_remote_file(thread, reference.path, window, cx);
            return;
        }
        let label = reference.label();
        let location = {
            let state = self.state.read(cx);
            thread
                .or_else(|| state.conversation_thread().map(|thread| thread.id))
                .and_then(|id| state.snapshot.threads.iter().find(|thread| thread.id == id))
                .and_then(|thread| {
                    let (_, base) = state.working_tree_of(thread)?;
                    let worktree = state
                        .thread_worktree(thread)
                        .filter(|worktree| worktree.is_live())
                        .map(|worktree| worktree.id);
                    Some((thread.project_id, worktree, base))
                })
        };
        let Some((project, worktree, base)) = location else {
            self.file_notice(
                format!("Could not open {label}: select its conversation first."),
                cx,
            );
            return;
        };
        let home = crate::platform::home().ok();
        let path = reference.resolve(&base, home.as_deref());
        cx.spawn_in(window, async move |workspace, cx| {
            let checked = smol::unblock(move || {
                file_refs::inspect(&path).map_err(|error| error.to_string())?;
                let file = std::fs::canonicalize(&path).map_err(|error| {
                    format!("{} could not be resolved: {error}", path.display())
                })?;
                let root = if base.is_dir() {
                    base
                } else {
                    file.parent().map_or(base, Path::to_path_buf)
                };
                Ok::<_, String>((file, root))
            })
            .await;
            let _ = workspace.update_in(cx, |workspace, window, cx| match checked {
                Ok((file, root)) => {
                    let target = NeovimTarget {
                        file: Some(file),
                        line: reference.line,
                        column: reference.column,
                    };
                    workspace.show_in_neovim(project, worktree, root, target, &label, window, cx);
                }
                Err(error) => workspace.file_notice(format!("Could not open {label}: {error}"), cx),
            });
        })
        .detach();
    }

    fn open_remote_file(
        &mut self,
        thread: ThreadId,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss(window, cx);
        if self.navigation_blocked(window, cx) {
            return;
        }
        self.state.update(cx, |state, cx| {
            state.request_remote_file(thread, path.clone(), cx);
            cx.notify();
        });
        let state = self.state.clone();
        let content = cx.new(|cx| {
            let text = cx.new(|cx| TextareaState::new(window, cx));
            let subscription = cx.observe_in(
                &state,
                window,
                |view: &mut RemoteFileDialog, _, window, cx| {
                    let content = view
                        .state
                        .read(cx)
                        .remote_file
                        .as_ref()
                        .filter(|file| file.thread == view.thread && file.path == view.path)
                        .and_then(|file| file.content.clone());
                    if let Some(content) = content
                        && view.text.read(cx).value().as_ref() != content.as_str()
                    {
                        view.text
                            .update(cx, |text, cx| text.set_value(content, window, cx));
                    }
                    cx.notify();
                },
            );
            RemoteFileDialog {
                state: state.clone(),
                thread,
                path: path.clone(),
                text,
                _subscription: subscription,
            }
        });
        window.open_dialog(cx, move |dialog, window, _| {
            let close = state.clone();
            dialog
                .title(format!("Host file · {path}"))
                .width(window.rem_size() * 48.)
                .child(content.clone())
                .on_close(move |_, _, cx| {
                    close.update(cx, |state, cx| {
                        state.close_remote_file();
                        cx.notify();
                    })
                })
        });
    }

    #[expect(clippy::too_many_arguments, reason = "a tab's full launch context")]
    fn show_in_neovim(
        &mut self,
        project: ProjectId,
        worktree: Option<WorktreeId>,
        root: PathBuf,
        target: NeovimTarget,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preparing_space_change
            || self.terminal_confirmation
            || self.rebuilding
            || self.state.read(cx).shutting_down
        {
            self.file_notice(format!("Could not open {label} right now; try again."), cx);
            return;
        }
        let Some(file) = target.file.clone() else {
            return;
        };
        let existing = self.terminals.borrow().values().find_map(|panel| {
            let pane = panel.read(cx).terminal.clone()?;
            let shown = pane.read(cx);
            (shown.space_id == self.presented_space
                && shown.is_alive(cx)
                && shown.neovim_target().and_then(|shown| shown.file.as_ref()) == Some(&file))
            .then(|| (panel.clone(), pane.clone()))
        });
        let Some((panel, pane)) = existing else {
            self.add_terminal(project, worktree, root, Some(target), window, cx);
            return;
        };
        self.focus_panel(panel, window, cx);
        let Some((executable, socket)) = pane.read(cx).neovim_server(cx) else {
            return;
        };
        let expression = neovim_jump_expression(&file, target.line, target.column);
        cx.spawn(async move |workspace, cx| {
            let jumped =
                smol::unblock(move || neovim_remote(&executable, &socket, &expression)).await;
            if let Err(error) = jumped {
                let _ = workspace.update(cx, |workspace, cx| workspace.file_notice(error, cx));
            }
        })
        .detach();
    }

    fn file_notice(&self, message: String, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.notice(NoticeLevel::Warning, message);
            cx.notify();
        });
    }

    pub(super) fn neovim_exited(
        &mut self,
        id: TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rebuilding || self.state.read(cx).shutting_down {
            return;
        }
        self.finish_terminal_stop(id, false, window, cx);
    }
}

#[cfg(feature = "runtime-qa")]
impl Workspace {
    pub fn neovim_tabs(&self, cx: &App) -> Vec<(String, NeovimTarget)> {
        self.terminals
            .borrow()
            .values()
            .filter_map(|panel| {
                let panel = panel.read(cx);
                let target = panel.terminal.as_ref()?.read(cx).neovim_target()?.clone();
                Some((panel.panel_title(cx), target))
            })
            .collect()
    }
}
