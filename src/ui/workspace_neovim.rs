//! Neovim tabs: the user's own Neovim in a terminal surface, opened empty or on a file
//! a Conversation displayed. A tab is a `Terminal` panel whose pane runs Neovim, so it
//! shares terminal persistence, Spaces, visibility, focus and close confirmation.
use super::super::terminal::{NeovimTarget, neovim_jump_expression, neovim_remote};
use super::*;
use crate::file_refs::{self, FileReference};

/// The `nvim` executable setting; blank discovers Neovim on PATH.
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

    /// The keyboard path to every file the selected Conversation displayed.
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

    /// Open `reference` in a Neovim tab, at its line. Relative paths resolve against the
    /// working tree of `thread` (or the selected Conversation). The file must exist and
    /// be text; that is checked off the UI thread, and a failure is a visible notice.
    /// A live Neovim tab already showing the file moves to the line instead.
    pub(in crate::ui) fn open_file_reference(
        &mut self,
        reference: FileReference,
        thread: Option<ThreadId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                // A removed worktree leaves no directory to start in; use the file's own.
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
        // The dock cannot change while these settle; the click is not silently lost.
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

    /// Neovim quit on its own (`:q`): nothing it owned is left, so neither is its tab.
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
    /// Each Neovim tab's title and what it opened, for runtime QA.
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
