use super::*;

pub(super) fn reopen_terminal(
    saved: &PanelState,
    terminals: &TerminalPanels,
    state: &Entity<AppState>,
    owner: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<WorkspacePanel>> {
    let tab = terminal_tab(saved)?;
    let (space, root, worktree) = {
        let state = state.read(cx);
        let space = state.active_space;
        if !state.project_in_space(tab.project, space) {
            return None;
        }
        let project = state
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == tab.project)?;
        tab.worktree
            .and_then(|id| state.worktree(id))
            .filter(|worktree| worktree.is_live() && worktree.project_id == tab.project)
            .map(|worktree| (space, worktree.path.clone(), Some(worktree.id)))
            .unwrap_or((space, project.root.clone(), None))
    };
    let setting = neovim::neovim_setting(state.read(cx));
    let pane = cx.new(|cx| match tab.neovim {
        Some(target) => TerminalPane::neovim(
            tab.project,
            worktree,
            space,
            root,
            target,
            setting,
            window,
            cx,
        ),
        None => TerminalPane::new(tab.project, worktree, space, root, window, cx),
    });
    let panel = cx.new(|cx| {
        let mut panel = WorkspacePanel::new(
            PanelKind::Terminal(tab.terminal),
            state.clone(),
            owner.clone(),
            window,
            cx,
        );
        panel.attach_terminal(tab.terminal, pane, window, cx);
        panel
    });
    terminals.borrow_mut().insert(tab.terminal, panel.clone());
    Some(panel)
}

impl Workspace {
    pub(in crate::ui) fn has_active_terminal(&self, cx: &App) -> bool {
        self.active_terminal(cx).is_some()
    }
    pub(in crate::ui) fn active_terminal(&self, cx: &App) -> Option<Entity<TerminalPane>> {
        self.active_terminal
            .and_then(|id| self.terminals.borrow().get(&id).cloned())
            .and_then(|panel| panel.read(cx).terminal.clone())
            .filter(|pane| pane.read(cx).space_id == self.presented_space)
    }
    #[cfg(all(test, feature = "runtime-qa"))]
    pub(in crate::ui) fn terminal_count(&self, _: &App) -> usize {
        self.terminals.borrow().len()
    }
    pub(in crate::ui) fn has_running_terminals(&self, cx: &App) -> bool {
        self.terminals.borrow().values().any(|panel| {
            panel
                .read(cx)
                .terminal
                .as_ref()
                .is_some_and(|pane| pane.read(cx).is_alive(cx))
        }) || self
            .thread_terminals
            .values()
            .filter_map(|entry| entry.pane.as_ref())
            .any(|pane| pane.read(cx).is_alive(cx))
    }
    pub(in crate::ui) fn close_all_terminals(&mut self, cx: &mut Context<Self>) {
        for (_, entry) in std::mem::take(&mut self.thread_terminals) {
            if let Some(pane) = entry.pane {
                pane.update(cx, |pane, cx| pane.close(cx));
            }
        }
        for (_, panel) in std::mem::take(&mut *self.terminals.borrow_mut()) {
            if let Some(pane) = panel.read(cx).terminal.clone() {
                pane.update(cx, |pane, cx| pane.close(cx));
            }
        }
        self.active_terminal = None;
        self.space_terminals.clear();
        self.live_layouts.clear();
    }
    pub(super) fn active_space_terminals(&self, cx: &App) -> Vec<Entity<WorkspacePanel>> {
        self.terminals
            .borrow()
            .values()
            .filter(|panel| {
                panel
                    .read(cx)
                    .terminal
                    .as_ref()
                    .is_some_and(|pane| pane.read(cx).space_id == self.presented_space)
            })
            .cloned()
            .collect()
    }
    pub(super) fn hide_terminals(&self, cx: &mut App) {
        for pane in self
            .thread_terminals
            .values()
            .filter_map(|entry| entry.pane.as_ref())
        {
            pane.update(cx, |pane, cx| pane.set_visible(false, cx));
        }
        for panel in self.terminals.borrow().values() {
            if let Some(pane) = panel.read(cx).terminal.clone() {
                pane.update(cx, |pane, cx| pane.set_visible(false, cx));
            }
        }
    }
    pub(super) fn sync_terminal_visibility(&self, _: &mut Window, cx: &mut App) {
        let dock = self.dock.read(cx);
        let mut panes: Vec<_> = self
            .terminals
            .borrow()
            .values()
            .filter_map(|panel| {
                let pane = panel.read(cx).terminal.clone()?;
                let visible = pane.read(cx).space_id == self.presented_space
                    && panel_visible(panel, dock, cx);
                Some((pane, visible))
            })
            .collect();
        let state = self.state.read(cx);
        for (&id, entry) in &self.thread_terminals {
            let Some(pane) = &entry.pane else {
                continue;
            };
            let visible = pane.read(cx).space_id == self.presented_space
                && state.thread_sidebar_open(id)
                && state.thread_sidebar_tab(id) == SidebarTab::Terminal
                && self
                    .threads
                    .borrow()
                    .get(&id)
                    .is_some_and(|panel| panel_visible(panel, dock, cx));
            panes.push((pane.clone(), visible));
        }
        for (pane, visible) in panes {
            pane.update(cx, |pane, cx| pane.set_visible(visible, cx));
        }
    }
    pub(super) fn terminal_focused(
        &mut self,
        id: TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rebuilding || self.preparing_space_change {
            return;
        }
        let Some(panel) = self.terminals.borrow().get(&id).cloned() else {
            return;
        };
        let Some(pane) = panel.read(cx).terminal.clone() else {
            return;
        };
        if pane.read(cx).space_id != self.presented_space {
            return;
        }
        let project = pane.read(cx).project_id;
        self.browsers.active = None;
        self.browsers.focused = None;
        self.active_terminal = Some(id);
        self.space_terminals.insert(self.presented_space, id);
        self.state.update(cx, |state, cx| {
            state.select_terminal_project(project);
            cx.emit(PresentationChange::Selection);
        });
        self.sync_session(window, cx);
        self.sync_terminal_visibility(window, cx);
        cx.notify();
    }
    pub(in crate::ui) fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        if let Some((project, worktree, root)) = self.terminal_location(cx) {
            self.add_terminal(project, worktree, root, None, window, cx);
        }
    }
    pub(super) fn terminal_location(
        &self,
        cx: &App,
    ) -> Option<(ProjectId, Option<WorktreeId>, PathBuf)> {
        let state = self.state.read(cx);
        let project = state.selected_project.and_then(|id| {
            state
                .snapshot
                .projects
                .iter()
                .find(|project| project.id == id)
        })?;
        let (root, worktree) = state
            .conversation_thread()
            .filter(|thread| thread.project_id == project.id)
            .and_then(|thread| state.thread_worktree(thread))
            .filter(|worktree| worktree.is_live())
            .map(|worktree| (worktree.path.clone(), Some(worktree.id)))
            .unwrap_or((project.root.clone(), None));
        Some((project.id, worktree, root))
    }
    pub(super) fn add_terminal(
        &mut self,
        project: ProjectId,
        worktree: Option<WorktreeId>,
        root: PathBuf,
        neovim: Option<NeovimTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = TerminalId::new();
        let owner = cx.entity().downgrade();
        let state = self.state.clone();
        let space = self.presented_space;
        let anchor = self
            .thread_target(cx)
            .and_then(|panel| panel.read(cx).group.clone())
            .and_then(|group| group.upgrade())
            .map(|group| group.read(cx).node());
        let setting = neovim::neovim_setting(self.state.read(cx));
        let pane = cx.new(|cx| match neovim {
            Some(target) => {
                TerminalPane::neovim(project, worktree, space, root, target, setting, window, cx)
            }
            None => TerminalPane::new(project, worktree, space, root, window, cx),
        });
        let panel = cx.new(|cx| {
            let mut panel = WorkspacePanel::new(PanelKind::Terminal(id), state, owner, window, cx);
            panel.attach_terminal(id, pane, window, cx);
            panel
        });
        self.terminals.borrow_mut().insert(id, panel.clone());
        let panel_id = PanelId::from(panel.entity_id());
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                panel_handle(panel.clone()),
                DockPlacement::Center,
                None,
                window,
                cx,
            );
            if let Some(node) = anchor {
                dock.move_panel(
                    panel_id,
                    InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: true,
                    },
                    window,
                    cx,
                );
            }
        });
        self.terminal_focused(id, window, cx);
        self.sync_thread_panels(window, cx);
        self.focus_panel(panel, window, cx);
        self.flush_layout(cx);
    }
    pub(in crate::ui) fn restart_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active_terminal {
            self.confirm_terminal_stop(id, true, window, cx);
        } else if let Some((id, _)) = self.selected_sidebar_terminal(cx) {
            self.confirm_sidebar_terminal_stop(id, true, window, cx);
        } else if let Some(id) = self
            .state
            .read(cx)
            .thread()
            .filter(|thread| {
                self.state.read(cx).thread_sidebar_tab(thread.id) == SidebarTab::Terminal
            })
            .map(|thread| thread.id)
        {
            self.drop_sidebar_terminal(id, cx);
            self.show_sidebar_tab(id, SidebarTab::Terminal, window, cx);
        }
    }
    pub(super) fn close_terminal_tab(
        &mut self,
        id: TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_terminal_stop(id, false, window, cx);
    }
    pub(super) fn confirm_terminal_stop(
        &mut self,
        id: TerminalId,
        restart: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminal_confirmation
            || self.preparing_space_change
            || self.state.read(cx).shutting_down
        {
            return;
        }
        let Some(panel) = self.terminals.borrow().get(&id).cloned() else {
            return;
        };
        let Some(pane) = panel.read(cx).terminal.clone() else {
            return;
        };
        if !pane.read(cx).is_alive(cx) {
            self.finish_terminal_stop(id, restart, window, cx);
            return;
        }
        if pane.read(cx).neovim_target().is_some() {
            self.show_terminal_stop_confirmation(id, restart, window, cx);
            return;
        }
        let native = pane.read(cx).native();
        let check = native.as_ref().map_or_else(
            || Task::ready(true),
            |native| native.read(cx).needs_confirm_quit(cx),
        );
        self.terminal_confirmation = true;
        cx.notify();
        cx.spawn_in(window, async move |workspace, window| {
            let needs_confirmation = check.await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.terminal_confirmation = false;
                cx.notify();
                let current = workspace.terminals.borrow().get(&id).cloned();
                if current.as_ref() != Some(&panel)
                    || panel.read(cx).terminal.as_ref() != Some(&pane)
                    || pane.read(cx).native() != native
                    || workspace.navigation_blocked(window, cx)
                {
                    return;
                }
                if !pane.read(cx).is_alive(cx) || !needs_confirmation {
                    workspace.finish_terminal_stop(id, restart, window, cx);
                } else {
                    workspace.show_terminal_stop_confirmation(id, restart, window, cx);
                }
            });
        })
        .detach();
    }
    pub(super) fn show_terminal_stop_confirmation(
        &mut self,
        id: TerminalId,
        restart: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.terminals.borrow().get(&id).cloned() else {
            return;
        };
        let neovim = panel
            .read(cx)
            .terminal
            .as_ref()
            .is_some_and(|pane| pane.read(cx).neovim_target().is_some());
        self.terminal_confirmation = true;
        let (description, ok_text) = match (neovim, restart) {
            (true, restart) => (
                "This stops Neovim. Changes it has not written are lost; other tabs are not affected.",
                if restart {
                    RESTART_NEOVIM_LABEL
                } else {
                    "Close Neovim"
                },
            ),
            (false, true) => (
                "This stops this terminal's shell and running jobs. Other terminals are not affected.",
                "Restart terminal",
            ),
            (false, false) => (
                "This stops this terminal's shell and running jobs. Other terminals are not affected.",
                "Close terminal",
            ),
        };
        let title = format!(
            "{} {}?",
            if restart { "Restart" } else { "Close" },
            panel.read(cx).panel_title(cx)
        );
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let confirm = owner.clone();
            let cancel = owner.clone();
            alert
                .title(
                    div()
                        .id("terminal-stop-confirmation")
                        .test_support()
                        .child(title.clone()),
                )
                .description(description)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok_text)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let confirm = confirm.clone();
                    window.defer(cx, move |window, cx| {
                        window.close_dialog(cx);
                        let _ = confirm.update(cx, |workspace, cx| {
                            workspace.terminal_confirmation = false;
                            workspace.finish_terminal_stop(id, restart, window, cx);
                            cx.notify();
                        });
                    });
                    false
                })
                .on_cancel(move |_, _, cx| {
                    let _ = cancel.update(cx, |workspace, cx| {
                        workspace.terminal_confirmation = false;
                        cx.notify();
                    });
                    true
                })
        });
    }
    pub(super) fn finish_terminal_stop(
        &mut self,
        id: TerminalId,
        restart: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.terminals.borrow().get(&id).cloned() else {
            return;
        };
        let Some(pane) = panel.read(cx).terminal.clone() else {
            return;
        };
        if restart {
            pane.update(cx, |pane, cx| pane.restart(window, cx));
            self.focus_panel(panel, window, cx);
            return;
        }
        pane.update(cx, |pane, cx| pane.close(cx));
        self.terminals.borrow_mut().remove(&id);
        self.space_terminals.retain(|_, terminal| *terminal != id);
        if self.active_terminal == Some(id) {
            self.active_terminal = None;
        }
        self.dock
            .update(cx, |dock, cx| dock.remove_panel(panel, window, cx));
        self.sync_thread_panels(window, cx);
        self.focus_region(Region::Thread, window, cx);
        self.flush_layout(cx);
    }
}
