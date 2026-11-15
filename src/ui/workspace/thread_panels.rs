//! Thread panels: one dock panel per open thread, kept in sync with `AppState`,
//! plus tab cycling and closing.

use super::*;

/// Get or build the panel for a thread; the registry and the workspace share the map.
pub(super) fn thread_panel(
    threads: &ThreadPanels,
    id: ThreadId,
    state: &Entity<AppState>,
    owner: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<WorkspacePanel> {
    if let Some(panel) = threads.borrow().get(&id) {
        return panel.clone();
    }
    let panel = cx.new(|cx| {
        WorkspacePanel::new(
            PanelKind::Thread(id),
            state.clone(),
            owner.clone(),
            window,
            cx,
        )
    });
    threads.borrow_mut().insert(id, panel.clone());
    panel
}
pub(super) fn welcome_panel(
    welcome: &Rc<RefCell<Option<Entity<WorkspacePanel>>>>,
    state: &Entity<AppState>,
    owner: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<WorkspacePanel> {
    if let Some(panel) = welcome.borrow().as_ref() {
        return panel.clone();
    }
    let panel = cx.new(|cx| {
        WorkspacePanel::new(PanelKind::Welcome, state.clone(), owner.clone(), window, cx)
    });
    *welcome.borrow_mut() = Some(panel.clone());
    panel
}

impl Workspace {
    pub(in crate::ui) fn center_panel_count(&self, cx: &App) -> usize {
        self.open_thread_ids(cx).len()
            + self.active_space_terminals(cx).len()
            + self.browser_tab_count(cx)
    }
    pub(in crate::ui) fn cycle_panel(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut panels = Vec::new();
        fn collect(state: &PanelState, kinds: &mut Vec<PanelKind>) {
            if let Some(id) = thread_of(state).filter(|_| state.panel_name == "Thread") {
                kinds.push(PanelKind::Thread(id));
            }
            if let Some(id) = terminal_of(state).filter(|_| state.panel_name == "Terminal") {
                kinds.push(PanelKind::Terminal(id));
            }
            if let Some(id) = browser_of(state).filter(|_| state.panel_name == "Browser") {
                kinds.push(PanelKind::Browser(id));
            }
            for child in &state.children {
                collect(child, kinds);
            }
        }
        let layout = self.dock.read(cx).dump(cx);
        collect(&layout.center, &mut panels);
        for dock in [&layout.left_dock, &layout.right_dock, &layout.bottom_dock]
            .into_iter()
            .flatten()
        {
            collect(dock.panel(), &mut panels);
        }
        if panels.is_empty() {
            return;
        }
        let current = self.thread_target(cx).map(|panel| panel.read(cx).kind);
        let index = panels.iter().position(|kind| Some(*kind) == current);
        let next = match index {
            Some(index) if forward => (index + 1) % panels.len(),
            Some(index) => (index + panels.len() - 1) % panels.len(),
            None if forward => 0,
            None => panels.len() - 1,
        };
        let target = match panels[next] {
            PanelKind::Thread(id) => self.threads.borrow().get(&id).cloned(),
            PanelKind::Terminal(id) => self.terminals.borrow().get(&id).cloned(),
            PanelKind::Browser(id) => self.browsers.panels.borrow().get(&id).cloned(),
            _ => None,
        };
        if let Some(target) = target {
            self.focus_panel(target, window, cx);
        }
    }
    pub(in crate::ui) fn close_active_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.browsers.active {
            self.close_browser(id, window, cx);
            return;
        }
        if let Some(id) = self.active_terminal {
            self.close_terminal_tab(id, window, cx);
        } else if let Some(id) = self.state.read(cx).selected_thread {
            self.close_thread_tab(id, window, cx);
        }
    }
    pub(super) fn open_thread_ids(&self, cx: &App) -> Vec<ThreadId> {
        let state = self.state.read(cx);
        state
            .open_threads
            .iter()
            .copied()
            .filter(|id| state.thread_in_active_space(*id))
            .collect()
    }
    pub(super) fn all_panels(&self) -> Vec<Entity<WorkspacePanel>> {
        let mut panels = vec![self.projects.clone()];
        panels.extend(self.welcome.borrow().iter().cloned());
        panels.extend(self.threads.borrow().values().cloned());
        panels.extend(self.terminals.borrow().values().cloned());
        panels.extend(self.browsers.panels.borrow().values().cloned());
        panels
    }
    pub(super) fn thread_target(&self, cx: &App) -> Option<Entity<WorkspacePanel>> {
        if let Some(panel) = self
            .browsers
            .active
            .and_then(|id| self.browsers.panels.borrow().get(&id).cloned())
        {
            return Some(panel);
        }
        self.active_terminal
            .and_then(|id| self.terminals.borrow().get(&id).cloned())
            .or_else(|| {
                self.state
                    .read(cx)
                    .selected_thread
                    .and_then(|id| self.threads.borrow().get(&id).cloned())
            })
            .or_else(|| {
                self.terminals
                    .borrow()
                    .values()
                    .find(|panel| {
                        panel
                            .read(cx)
                            .terminal
                            .as_ref()
                            .is_some_and(|pane| pane.read(cx).space_id == self.presented_space)
                    })
                    .cloned()
            })
            .or_else(|| self.threads.borrow().values().next().cloned())
            .or_else(|| self.welcome.borrow().clone())
    }

    /// One panel per open thread, beside the selected thread's group; the Welcome
    /// panel only while nothing is open. Dock edits that close a thread come back
    /// through `thread_panel_removed`, so this is the single owner of the map.
    pub(in crate::ui) fn sync_thread_panels(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rebuilding || self.state.read(cx).shutting_down {
            return;
        }
        if self.active_terminal.is_some() && !self.has_active_terminal(cx) {
            self.active_terminal = None;
        }
        let open = self.open_thread_ids(cx);
        let stale: Vec<ThreadId> = self
            .threads
            .borrow()
            .keys()
            .copied()
            .filter(|id| !open.contains(id))
            .collect();
        let mut lost_focus = false;
        for id in stale {
            let removed = self.threads.borrow_mut().remove(&id);
            if let Some(panel) = removed {
                lost_focus |= panel.read(cx).focus.contains_focused(window, cx);
                self.dock
                    .update(cx, |dock, cx| dock.remove_panel(panel, window, cx));
            }
        }
        let owner = cx.entity().downgrade();
        if open.is_empty()
            && self.active_space_terminals(cx).is_empty()
            && self.browser_tab_count(cx) == 0
        {
            if self.welcome.borrow().is_none() {
                let welcome = welcome_panel(&self.welcome, &self.state, &owner, window, cx);
                self.dock.update(cx, |dock, cx| {
                    dock.add_panel_view(
                        panel_handle(welcome),
                        DockPlacement::Center,
                        None,
                        window,
                        cx,
                    )
                });
            }
        } else {
            let welcome = self.welcome.borrow_mut().take();
            if let Some(welcome) = welcome {
                self.dock
                    .update(cx, |dock, cx| dock.remove_panel(welcome, window, cx));
            }
        }
        let selected = self.state.read(cx).selected_thread;
        let anchor = self
            .focused_panel(window, cx)
            .filter(|panel| {
                matches!(
                    panel.read(cx).kind,
                    PanelKind::Thread(_) | PanelKind::Terminal(_) | PanelKind::Browser(_)
                )
            })
            .or_else(|| selected.and_then(|id| self.threads.borrow().get(&id).cloned()))
            .and_then(|panel| panel.read(cx).group.clone())
            .and_then(|group| group.upgrade())
            .map(|group| group.read(cx).node());
        for id in open {
            if self.threads.borrow().contains_key(&id) {
                continue;
            }
            let panel = thread_panel(&self.threads, id, &self.state, &owner, window, cx);
            let panel_id = PanelId::from(panel.entity_id());
            self.dock.update(cx, |dock, cx| {
                // Wrapped so the tab bar draws the panel's own title, not its layout name.
                dock.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
                if let Some(node) = anchor {
                    dock.move_panel(
                        panel_id,
                        InsertTarget::Tabs {
                            node,
                            ix: None,
                            activate: selected == Some(id),
                        },
                        window,
                        cx,
                    );
                }
            });
        }
        for panel in self.terminals.borrow().values() {
            let current = panel
                .read(cx)
                .terminal
                .as_ref()
                .is_some_and(|pane| pane.read(cx).space_id == self.presented_space);
            let attached = panel
                .read(cx)
                .group
                .as_ref()
                .and_then(WeakEntity::upgrade)
                .is_some();
            if current && !attached {
                self.dock.update(cx, |dock, cx| {
                    dock.add_panel_view(
                        panel_handle(panel.clone()),
                        DockPlacement::Center,
                        None,
                        window,
                        cx,
                    )
                });
            } else if !current && attached {
                self.dock
                    .update(cx, |dock, cx| dock.remove_panel(panel.clone(), window, cx));
            }
        }
        self.sync_browser_tabs(window, cx);
        self.sync_sidebar_terminals(window, cx);
        self.release_closed_threads(cx);
        if lost_focus {
            // The closed pane held focus; keyboard control continues in the next thread.
            if let Some(target) = self.thread_target(cx) {
                self.focus_panel(target, window, cx);
            }
        }
        cx.notify();
    }
    /// A thread closed in every Space keeps no view state, as its panel and transcript
    /// go with its tab; runs only when the open threads changed. Session data stays in
    /// `AppState`.
    fn release_closed_threads(&mut self, cx: &App) {
        let state = self.state.read(cx);
        // The active Space's open threads live on `AppState`, the others' in their sessions.
        let mut open = state.open_threads.clone();
        for space in state
            .spaces
            .iter()
            .filter(|space| space.id != state.active_space)
        {
            if let Some(session) = state.space_session(space.id) {
                open.extend_from_slice(&session.open_threads);
            }
        }
        if open == self.released_open_threads {
            return;
        }
        release_closed_thread_state(
            &open,
            &state.active_threads,
            &mut self.changes_lists.borrow_mut(),
            &mut self.agent_activity,
            &mut self.expanded,
        );
        self.released_open_threads = open;
    }
    pub(super) fn sync_selection_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.state.read(cx).selected_thread;
        if selected.is_some() {
            self.active_terminal = None;
        }
        if selected != self.presented_thread {
            for id in self.presented_thread.into_iter().chain(selected) {
                if let Some(panel) = self.threads.borrow().get(&id) {
                    panel.update(cx, |_, cx| cx.notify());
                }
            }
            self.presented_thread = selected;
            self.unrevealed_thread = selected;
            self.projects.update(cx, |_, cx| cx.notify());
        }
        if let Some(id) = self.unrevealed_thread
            && self.reveal_thread_tab(id, window, cx)
        {
            self.unrevealed_thread = None;
        }
    }
    /// Display a thread's tab in its group without moving focus, so a thread
    /// selected from Projects, the palette or a close is the one on screen; the
    /// tab bar scrolls a newly displayed tab into view. False while the thread
    /// has no panel in a group yet.
    pub(super) fn reveal_thread_tab(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.rebuilding || self.preparing_space_change || self.state.read(cx).shutting_down {
            return false;
        }
        let Some(panel) = self.threads.borrow().get(&id).cloned() else {
            return false;
        };
        let Some(group) = panel
            .read(cx)
            .group
            .clone()
            .and_then(|group| group.upgrade())
        else {
            return false;
        };
        let panel_id = PanelId::from(panel.entity_id());
        let Some(index) = group
            .read(cx)
            .panels()
            .iter()
            .position(|candidate| candidate.panel_id(cx) == panel_id)
        else {
            return false;
        };
        if index != group.read(cx).active_ix() {
            // `select_tab` focuses the displayed panel. Focus elsewhere (Projects,
            // another pane) stays there; focus in the tab being hidden follows the tab.
            let previous = window.focused(cx).filter(|_| {
                !group
                    .read(cx)
                    .panels()
                    .iter()
                    .any(|candidate| candidate.focus_handle(cx).contains_focused(window, cx))
            });
            group.update(cx, |group, cx| group.select_tab(index, window, cx));
            if let Some(previous) = previous {
                previous.focus(window, cx);
            }
        }
        true
    }

    /// Every thread pane on screen opens its Conversation, not only the selected one, so
    /// restored tabs beside the selection show their history without a click.
    pub(super) fn sync_visible_threads(&mut self, cx: &mut Context<Self>) {
        let dock = self.dock.read(cx);
        let visible: Vec<_> = self
            .threads
            .borrow()
            .iter()
            .filter(|(_, panel)| panel_visible(panel, dock, cx))
            .map(|(id, _)| *id)
            .collect();
        self.state.update(cx, |state, cx| {
            for &id in &visible {
                state.ensure_conversation(id);
            }
            state.set_visible_conversations(visible, cx);
        });
    }

    /// A pane that gained focus (click, tab, directional shortcut) is the selected thread.
    pub(super) fn thread_focused(
        &mut self,
        id: ThreadId,
        panel: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browsers.active = None;
        self.browsers.focused = None;
        if self.rebuilding
            || self.preparing_space_change
            || !self.state.read(cx).thread_in_active_space(id)
            || self.state.read(cx).selected_thread == Some(id)
        {
            return;
        }
        if !self
            .threads
            .borrow()
            .get(&id)
            .is_some_and(|registered| registered.entity_id() == panel)
        {
            return;
        }
        if !self
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .any(|t| t.id == id)
        {
            return;
        }
        self.state.update(cx, |state, cx| {
            // Selecting a thread leaves any previously active terminal intact.
            state.select_thread(id);
            cx.emit(PresentationChange::Selection);
        });
        self.sync_session(window, cx);
        self.active_terminal = None;
        cx.notify();
    }
    /// The dock dropped a thread panel (tab close button or a closed split).
    /// Called from inside the dock's own update, so focus moves on the next tick.
    /// A thread panel the dock closed on its own (tab close button, dock edit).
    /// Panels the workspace removed itself are already gone from the map, so
    /// only a panel that is still the registered one for `id` closes the thread.
    pub(super) fn thread_panel_removed(
        &mut self,
        id: ThreadId,
        panel: &Entity<WorkspacePanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rebuilding || !self.state.read(cx).thread_in_active_space(id) {
            return;
        }
        let registered = self.threads.borrow().get(&id).is_some_and(|p| p == panel);
        if !registered {
            return;
        }
        self.threads.borrow_mut().remove(&id);
        self.drop_sidebar_terminal(id, cx);
        if self.state.read(cx).open_threads.contains(&id) {
            self.state.update(cx, |state, cx| {
                state.close_thread(id);
                cx.notify();
            });
            cx.defer_in(window, |workspace, window, cx| {
                workspace.focus_region(Region::Thread, window, cx);
            });
        }
        cx.notify();
    }

    /// Close a thread's tab. The thread stays in its project, and a running
    /// turn keeps running.
    pub(in crate::ui) fn close_thread_tab(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preparing_space_change
            || self.state.read(cx).shutting_down
            || !self.state.read(cx).thread_in_active_space(id)
        {
            return;
        }
        if self.confirm_sidebar_terminal_stop(id, false, window, cx) {
            return;
        }
        self.drop_sidebar_terminal(id, cx);
        self.state.update(cx, |state, cx| {
            state.close_thread(id);
            cx.notify();
        });
        self.sync_thread_panels(window, cx);
    }
    /// Archive `id` without selecting it: its tab closes, and a neighbouring tab takes over
    /// only when it was the selected one.
    pub(in crate::ui) fn archive_thread(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preparing_space_change
            || self.extension.is_some()
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        self.state.update(cx, |state, cx| {
            if let Err(error) = state.archive_thread(id) {
                state.notice(crate::app::NoticeLevel::Warning, format!("{error:#}"));
            }
            cx.notify();
        });
        self.sync_thread_panels(window, cx);
    }
    pub(in crate::ui) fn unarchive_thread(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if let Err(error) = state.unarchive_thread(id) {
                state.notice(crate::app::NoticeLevel::Warning, format!("{error:#}"));
            }
            cx.emit(PresentationChange::Thread(id));
            cx.notify();
        });
    }
}

/// The thread whose view owns a disclosure key: every key is `<kind>:<thread>[:…]`.
fn disclosure_thread(key: &str) -> Option<ThreadId> {
    key.split(':').nth(1)?.parse().ok()
}

/// Keep per-thread view state only for threads open in some Space: the Changes list
/// (patch, cursor, focus), the disclosure keys, and the Agents follow state, which a
/// running turn keeps so it still opens the Agents tab at most once per turn.
fn release_closed_thread_state<V>(
    open: &[ThreadId],
    running: &std::collections::HashSet<ThreadId>,
    changes: &mut std::collections::HashMap<ThreadId, V>,
    follows: &mut BTreeMap<ThreadId, crate::ui::network::agents::Follow>,
    expanded: &mut std::collections::HashSet<String>,
) {
    changes.retain(|id, _| open.contains(id));
    follows.retain(|id, _| open.contains(id) || running.contains(id));
    expanded.retain(|key| disclosure_thread(key).is_none_or(|id| open.contains(&id)));
}

#[cfg(test)]
mod tests {
    use super::{disclosure_thread, release_closed_thread_state};
    use crate::{model::ThreadId, ui::network::agents::Follow};
    use std::collections::{BTreeMap, HashMap, HashSet};

    #[test]
    fn closing_a_thread_releases_its_view_state() {
        let (open, closed, running) = (ThreadId::new_v4(), ThreadId::new_v4(), ThreadId::new_v4());
        let mut changes: HashMap<ThreadId, ()> = [(open, ()), (closed, ()), (running, ())].into();
        let mut follows: BTreeMap<ThreadId, Follow> = [open, closed, running]
            .into_iter()
            .map(|id| (id, Follow::new(0)))
            .collect();
        let mut expanded: HashSet<String> = [
            format!("dock-todo:{open}"),
            format!("tool-raw:{open}:call-1"),
            format!("dock-jobs:{closed}"),
            format!("raw:{closed}:{}", ThreadId::new_v4()),
            format!("queue-collapsed:{running}"),
            "unscoped".to_owned(),
        ]
        .into();
        release_closed_thread_state(
            &[open],
            &[running].into(),
            &mut changes,
            &mut follows,
            &mut expanded,
        );
        assert_eq!(changes.keys().copied().collect::<Vec<_>>(), [open]);
        assert_eq!(
            follows.keys().copied().collect::<HashSet<_>>(),
            [open, running].into()
        );
        assert_eq!(
            expanded,
            [
                format!("dock-todo:{open}"),
                format!("tool-raw:{open}:call-1"),
                "unscoped".to_owned(),
            ]
            .into()
        );
    }

    #[test]
    fn disclosure_keys_name_their_thread_second() {
        let id = ThreadId::new_v4();
        assert_eq!(
            disclosure_thread(&format!("toolrun:{id}:{}", ThreadId::new_v4())),
            Some(id)
        );
        assert_eq!(disclosure_thread(&format!("dock-todo:{id}")), Some(id));
        assert_eq!(disclosure_thread("dock-todo"), None);
        assert_eq!(disclosure_thread("tool:not-a-thread:1"), None);
    }
}
