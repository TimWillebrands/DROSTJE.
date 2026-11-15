//! Thread-owned shells outlive sidebar/tab visibility, but never their owning open Thread.
use super::*;

pub(in crate::ui) struct ThreadTerminal {
    pub pane: Option<Entity<TerminalPane>>,
    pub error: Option<String>,
    request: uuid::Uuid,
}

pub(in crate::ui) fn thread_terminal_location(
    state: &AppState,
    id: ThreadId,
) -> Result<(ProjectId, Option<WorktreeId>, PathBuf), String> {
    let thread = state
        .snapshot
        .threads
        .iter()
        .find(|thread| thread.id == id)
        .ok_or_else(|| "Select a thread first".to_owned())?;
    if thread.remote.is_some() {
        return Err(
            "Terminal sidebar is unavailable for remote Conversations; open a local Thread".into(),
        );
    }
    if let Some(worktree) = thread.worktree_id {
        let worktree = state
            .worktree(worktree)
            .filter(|worktree| worktree.is_live())
            .ok_or_else(|| "This Thread's local worktree is unavailable".to_owned())?;
        return Ok((thread.project_id, Some(worktree.id), worktree.path.clone()));
    }
    let project = state
        .snapshot
        .projects
        .iter()
        .find(|project| project.id == thread.project_id)
        .ok_or_else(|| "This Thread's local project is unavailable".to_owned())?;
    Ok((project.id, None, project.root.clone()))
}

fn require_directory(root: &std::path::Path) -> Result<(), String> {
    match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(format!(
            "Terminal directory is not a directory: {}",
            root.display()
        )),
        Err(error) => Err(format!(
            "Terminal directory is unavailable: {}: {error}",
            root.display()
        )),
    }
}

impl Workspace {
    pub(in crate::ui) fn sidebar_terminal(&self, id: ThreadId) -> Option<Entity<TerminalPane>> {
        self.thread_terminals.get(&id)?.pane.clone()
    }
    /// Native runtime QA inspects the retained shell without starting a provider.
    #[cfg(feature = "runtime-qa")]
    pub fn terminal_sidebar_pane(&self, id: ThreadId) -> Option<Entity<TerminalPane>> {
        self.sidebar_terminal(id)
    }

    pub(in crate::ui) fn selected_sidebar_terminal(
        &self,
        cx: &App,
    ) -> Option<(ThreadId, Entity<TerminalPane>)> {
        let state = self.state.read(cx);
        let id = state.thread()?.id;
        (state.thread_sidebar_open(id) && state.thread_sidebar_tab(id) == SidebarTab::Terminal)
            .then(|| self.sidebar_terminal(id).map(|pane| (id, pane)))?
    }

    pub(super) fn ensure_sidebar_terminal(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.thread_terminals.contains_key(&id) || self.state.read(cx).shutting_down {
            return;
        }
        let location = thread_terminal_location(self.state.read(cx), id);
        let request = uuid::Uuid::new_v4();
        self.thread_terminals.insert(
            id,
            ThreadTerminal {
                pane: None,
                error: location.as_ref().err().cloned(),
                request,
            },
        );
        let Ok((project, worktree, root)) = location else {
            return;
        };
        cx.spawn_in(window, async move |owner, cx| {
            let checked = root.clone();
            let result = smol::unblock(move || require_directory(&checked)).await;
            let _ = owner.update_in(cx, |workspace, window, cx| {
                if workspace.state.read(cx).shutting_down
                    || !workspace.thread_terminals.get(&id).is_some_and(|entry| entry.request == request)
                { return; }
                let current = thread_terminal_location(workspace.state.read(cx), id);
                let result = result.and_then(|()| {
                    if matches!(&current, Ok((p, w, r)) if *p == project && *w == worktree && *r == root) { Ok(()) }
                    else { Err("Thread working directory changed; choose Restart terminal to open the new directory".into()) }
                });
                match result {
                    Ok(()) => {
                        let Some(space) = workspace.state.read(cx).thread_space(id) else { return; };
                        let pane = cx.new(|cx| TerminalPane::new(project, worktree, space, root, window, cx));
                        if let Some(entry) = workspace.thread_terminals.get_mut(&id) { entry.pane = Some(pane); }
                    }
                    Err(error) => {
                        if let Some(entry) = workspace.thread_terminals.get_mut(&id) { entry.error = Some(error); }
                    }
                }
                workspace.sync_terminal_visibility(window, cx);
                if workspace.state.read(cx).selected_thread == Some(id)
                    && workspace.state.read(cx).thread_sidebar_tab(id) == SidebarTab::Terminal
                    && workspace.state.read(cx).thread_sidebar_open(id)
                    && workspace.threads.borrow().get(&id).is_some_and(|panel| panel.read(cx).focus.is_focused(window))
                    && let Some(pane) = workspace.sidebar_terminal(id)
                { pane.update(cx, |pane, cx| pane.focus(window, cx)); }
                workspace.state.update(cx, |_, cx| cx.emit(PresentationChange::Thread(id)));
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn sync_sidebar_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stale: Vec<_> = self
            .thread_terminals
            .keys()
            .copied()
            .filter(|id| {
                let state = self.state.read(cx);
                !state
                    .snapshot
                    .threads
                    .iter()
                    .any(|thread| thread.id == *id && !thread.archived)
                    || (state.thread_in_active_space(*id) && !state.open_threads.contains(id))
            })
            .collect();
        for id in stale {
            self.drop_sidebar_terminal(id, cx);
        }
        for (&id, entry) in &self.thread_terminals {
            if let Some(pane) = &entry.pane
                && let Some(space) = self.state.read(cx).thread_space(id)
            {
                pane.update(cx, |pane, _| pane.space_id = space);
            }
        }
        let dock = self.dock.read(cx);
        let visible: Vec<_> = self
            .threads
            .borrow()
            .iter()
            .filter_map(|(&id, panel)| {
                let state = self.state.read(cx);
                (state.thread_sidebar_open(id)
                    && state.thread_sidebar_tab(id) == SidebarTab::Terminal
                    && panel_visible(panel, dock, cx))
                .then_some(id)
            })
            .collect();
        for id in visible {
            self.ensure_sidebar_terminal(id, window, cx);
        }
    }

    pub(super) fn drop_sidebar_terminal(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        if let Some(entry) = self.thread_terminals.remove(&id)
            && let Some(pane) = entry.pane
        {
            pane.update(cx, |pane, cx| pane.close(cx));
        }
    }

    pub(in crate::ui) fn dispatch_sidebar_terminal(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.threads.borrow().get(&id).cloned();
        if let Some(panel) = panel {
            self.focus_panel(panel, window, cx);
            window.dispatch_action(Command::ToggleTerminal.action(), cx);
        }
    }

    pub(super) fn toggle_terminal_sidebar(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).thread_sidebar_open(id)
            && self.state.read(cx).thread_sidebar_tab(id) == SidebarTab::Terminal
        {
            self.toggle_sidebar_tab(id, SidebarTab::Terminal, window, cx);
            let panel = self.threads.borrow().get(&id).cloned();
            if let Some(panel) = panel {
                self.focus_panel(panel, window, cx);
            }
        } else {
            self.show_sidebar_tab(id, SidebarTab::Terminal, window, cx);
        }
        self.sync_terminal_visibility(window, cx);
    }

    /// A Thread close owns its shell too. A running shell always gets an explicit decision.
    pub(super) fn confirm_sidebar_terminal_stop(
        &mut self,
        id: ThreadId,
        restart: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pane) = self.sidebar_terminal(id) else {
            return false;
        };
        if !pane.read(cx).is_alive(cx) {
            if restart {
                pane.update(cx, |pane, cx| {
                    pane.restart(window, cx);
                    pane.focus(window, cx);
                });
                return true;
            }
            self.drop_sidebar_terminal(id, cx);
            return false;
        }
        if self.terminal_confirmation {
            return true;
        }
        self.terminal_confirmation = true;
        let title = self
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .map(|thread| thread.title.clone())
            .unwrap_or_else(|| "Thread".into());
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let confirm = owner.clone();
            let cancel = owner.clone();
            let pane = pane.clone();
            alert.title(format!("{} terminal for {title}?", if restart { "Restart" } else { "Close" }))
                .description("This stops this Thread's shell and running jobs. Hiding the Terminal tab keeps them running.")
                .button_props(DialogButtonProps::default().ok_text(if restart { "Restart terminal" } else { "Close thread" }).cancel_text("Cancel").show_cancel(true))
                .on_ok(move |_, window, cx| {
                    let confirm = confirm.clone();
                    let pane = pane.clone();
                    window.defer(cx, move |window, cx| {
                        window.close_dialog(cx);
                        let _ = confirm.update(cx, |workspace, cx| {
                            workspace.terminal_confirmation = false;
                            if workspace.sidebar_terminal(id).as_ref() != Some(&pane) { return; }
                            if restart {
                                pane.update(cx, |pane, cx| { pane.restart(window, cx); pane.focus(window, cx); });
                            } else {
                                workspace.drop_sidebar_terminal(id, cx);
                                workspace.close_thread_tab(id, window, cx);
                            }
                            cx.notify();
                        });
                    });
                    false
                })
                .on_cancel(move |_, _, cx| {
                    let _ = cancel.update(cx, |workspace, cx| { workspace.terminal_confirmation = false; cx.notify(); });
                    true
                })
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::require_directory;
    #[test]
    fn terminal_directory_must_exist_and_be_a_directory() {
        let temp =
            std::env::temp_dir().join(format!("drostje-terminal-dir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&temp).unwrap();
        assert!(require_directory(&temp).is_ok());
        assert!(
            require_directory(&temp.join("missing"))
                .unwrap_err()
                .contains("unavailable")
        );
        let file = temp.join("file");
        std::fs::write(&file, "not a directory").unwrap();
        assert!(
            require_directory(&file)
                .unwrap_err()
                .contains("not a directory")
        );
        std::fs::remove_dir_all(temp).unwrap();
    }
}
