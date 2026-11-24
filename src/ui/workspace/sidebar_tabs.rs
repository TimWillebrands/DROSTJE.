use super::*;

pub(super) enum SidebarFocus {
    Handle(FocusHandle),
    Terminal(Entity<TerminalPane>),
    Browser(Entity<BrowserPane>),
}

impl SidebarFocus {
    pub(super) fn contains_focused(&self, window: &Window, cx: &App) -> bool {
        match self {
            Self::Handle(focus) => focus.contains_focused(window, cx),
            Self::Terminal(pane) => pane.read(cx).focus_handle(cx).contains_focused(window, cx),
            Self::Browser(pane) => pane.read(cx).contains_focused(window, cx),
        }
    }

    pub(super) fn focus(self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Handle(focus) => focus.focus(window, cx),
            Self::Terminal(pane) => pane.update(cx, |pane, cx| pane.focus(window, cx)),
            Self::Browser(pane) => pane.update(cx, |pane, cx| pane.focus(window, cx)),
        }
    }
}

impl Workspace {
    fn sidebar_focus_for(&self, id: ThreadId, tab: SidebarTab, cx: &App) -> Option<SidebarFocus> {
        match tab {
            SidebarTab::Terminal => self.sidebar_terminal(id).map(SidebarFocus::Terminal),
            SidebarTab::Browser => self.sidebar_browser(id).map(SidebarFocus::Browser),
            SidebarTab::Changes => self
                .state
                .read(cx)
                .is_conversation(id)
                .then(|| SidebarFocus::Handle(self.changes_focus(id, cx))),
            SidebarTab::Agents => self
                .network_sidebar(id, cx)
                .map(|pane| SidebarFocus::Handle(pane.read(cx).focus_handle(cx))),
            SidebarTab::Output | SidebarTab::Review | SidebarTab::Details => None,
        }
    }

    pub(super) fn sidebar_focus_target(&self, id: ThreadId, cx: &App) -> Option<SidebarFocus> {
        let state = self.state.read(cx);
        if !state.thread_sidebar_open(id) {
            return None;
        }
        self.sidebar_focus_for(id, state.thread_sidebar_tab(id), cx)
    }

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
            self.navigate_panel(panel, self.sidebar_focus_target(id, cx), window, cx);
        }
        self.sync_terminal_visibility(window, cx);
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
        let state = self.state.read(cx);
        if !state.thread_sidebar_open(id) || state.thread_sidebar_tab(id) != tab {
            self.show_sidebar_tab(id, tab, window, cx);
            return;
        }
        if self
            .sidebar_focus_target(id, cx)
            .is_some_and(|target| target.contains_focused(window, cx))
        {
            let panel = self.threads.borrow().get(&id).cloned();
            if let Some(panel) = panel {
                self.navigate_panel(panel, None, window, cx);
            }
        }
        self.state.update(cx, |state, cx| {
            state.toggle_thread_sidebar(id, tab);
            cx.emit(PresentationChange::Thread(id));
        });
        self.sync_visible_threads(cx);
        self.sync_terminal_visibility(window, cx);
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
