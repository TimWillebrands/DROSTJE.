use super::*;

impl Workspace {
    pub(in crate::ui) fn show_sidebar_tab(
        &mut self,
        id: ThreadId,
        tab: SidebarTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tab == SidebarTab::Browser {
            self.ensure_sidebar_browser(id, window, cx);
        }
        if tab == SidebarTab::Terminal {
            self.ensure_sidebar_terminal(id, window, cx);
        }
        self.state.update(cx, |state, cx| {
            state.show_thread_sidebar(id, tab);
            cx.emit(PresentationChange::Thread(id));
        });
        self.sync_visible_threads(cx);
        let panel = self.threads.borrow().get(&id).cloned();
        if let Some(panel) = panel {
            self.focus_panel(panel, window, cx);
        }
        self.sync_terminal_visibility(window, cx);
        if tab == SidebarTab::Terminal
            && let Some(pane) = self.sidebar_terminal(id)
        {
            pane.update(cx, |pane, cx| pane.focus(window, cx));
        }
        if tab == SidebarTab::Changes && self.state.read(cx).is_conversation(id) {
            self.changes_focus(id, cx).focus(window, cx);
        }
        if tab == SidebarTab::Agents
            && let Some(network) = self.network_sidebar(id, cx)
        {
            let focus = network.focus_handle(cx);
            focus.focus(window, cx);
        }
    }

    pub(in crate::ui) fn network_sidebar(
        &self,
        id: ThreadId,
        cx: &App,
    ) -> Option<Entity<crate::ui::network::NetworkPanel>> {
        self.threads
            .borrow()
            .get(&id)
            .and_then(|panel| panel.read(cx).network.clone())
    }
    pub(super) fn sync_agent_activity(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(session) = state.conversations.get(&id) else {
            return;
        };
        let count = crate::ui::network::agents::coordination_calls(session);
        let running = state.active_threads.contains(&id);
        let open = self
            .agent_activity
            .entry(id)
            .or_insert_with(|| crate::ui::network::agents::Follow::new(count))
            .observe(count, running);
        if open {
            if let Some(network) = self.network_sidebar(id, cx) {
                network.update(cx, |network, cx| network.follow_thread_activity(cx));
            }
            self.state.update(cx, |state, cx| {
                state.show_thread_sidebar(id, SidebarTab::Agents);
                cx.emit(PresentationChange::Thread(id));
            });
            self.sync_visible_threads(cx);
        }
    }
    pub(super) fn toggle_sidebar_tab(
        &mut self,
        id: ThreadId,
        tab: SidebarTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let leave_terminal = self
            .sidebar_terminal(id)
            .is_some_and(|pane| pane.read(cx).focus_handle(cx).contains_focused(window, cx));
        self.state.update(cx, |state, cx| {
            state.toggle_thread_sidebar(id, tab);
            cx.emit(PresentationChange::Thread(id));
        });
        self.sync_visible_threads(cx);
        self.sync_terminal_visibility(window, cx);
        if leave_terminal {
            let panel = self.threads.borrow().get(&id).cloned();
            if let Some(panel) = panel {
                self.focus_panel(panel, window, cx);
            }
        }
    }
    pub(super) fn toggle_dock(&mut self, dock: &str, cx: &mut Context<Self>) {
        let Some(id) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| thread.id)
        else {
            return;
        };
        let key = format!("{dock}:{id}");
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        cx.notify();
    }
}
