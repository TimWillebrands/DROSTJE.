use super::sidebar_tabs::SidebarFocus;
use super::*;

impl Workspace {
    pub(in crate::ui) fn navigation_blocked(&self, window: &mut Window, cx: &mut App) -> bool {
        self.quit_requested
            || self.preparing_space_change
            || self.terminal_confirmation
            || self.state.read(cx).shutting_down
            || self.palette.is_some()
            || self.has_open_form(cx)
            || self.theme_editor.is_some()
            || self
                .extension
                .as_ref()
                .is_some_and(|form| !form.inline_answer)
            || self.image_preview.is_some()
            || window.has_active_dialog(cx)
            || window.has_active_sheet(cx)
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
    }

    pub fn focused_panel(&self, window: &Window, cx: &App) -> Option<Entity<WorkspacePanel>> {
        self.all_panels()
            .into_iter()
            .find(|panel| panel.read(cx).focus.contains_focused(window, cx))
    }
    pub fn focused_region(&self, window: &Window, cx: &App) -> Region {
        self.focused_panel(window, cx)
            .map_or(Region::Thread, |panel| panel.read(cx).kind.region())
    }
    pub fn focused_thread(&self, window: &Window, cx: &App) -> Option<ThreadId> {
        match self.focused_panel(window, cx)?.read(cx).kind {
            PanelKind::Thread(id) => Some(id),
            _ => None,
        }
    }
    pub fn focus_region(&mut self, region: Region, window: &mut Window, cx: &mut Context<Self>) {
        if region == Region::Thread {
            self.sync_thread_panels(window, cx);
        }
        let target = match region {
            Region::Projects => Some(self.projects.clone()),
            Region::Thread => self.thread_target(cx),
        };
        if let Some(panel) = target {
            self.navigate_panel(panel, None, window, cx);
        }
    }
    pub(super) fn focus_panel(
        &mut self,
        panel: Entity<WorkspacePanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        self.pending_prompt_focus = None;
        let kind = panel.read(cx).kind;
        if kind == PanelKind::Projects {
            self.prepare_projects_cursor(cx);
        }
        let layout = self.dock.read(cx).dump(cx);
        let mut opened = false;
        for (placement, dock) in [
            (DockPlacement::Left, layout.left_dock),
            (DockPlacement::Right, layout.right_dock),
            (DockPlacement::Bottom, layout.bottom_dock),
        ] {
            if dock
                .as_ref()
                .is_some_and(|dock| contains_kind(dock.panel(), kind) && !dock.open())
            {
                opened = true;
                self.dock
                    .update(cx, |dock, cx| dock.toggle_dock(placement, window, cx));
            }
        }
        if opened {
            self.invalidate_panel_bounds(cx);
            self.normalize_layout(Some(kind.region()), window, cx);
        }
        let group = panel.read(cx).group.clone();
        let focus = panel.read(cx).focus.clone();
        let panel_id = PanelId::from(panel.entity_id());
        if let Some(group) = group.and_then(|group| group.upgrade()) {
            let node = group.read(cx).node();
            if self
                .dock
                .read(cx)
                .zoomed_group()
                .is_some_and(|zoomed| zoomed != node)
            {
                self.dock
                    .update(cx, |dock, cx| dock.set_zoomed_out(window, cx));
            }
            group.update(cx, |group, cx| {
                if let Some(index) = group
                    .panels()
                    .iter()
                    .position(|candidate| candidate.panel_id(cx) == panel_id)
                    .filter(|index| *index != group.active_ix())
                {
                    group.select_tab(index, window, cx);
                }
            });
        }
        match kind {
            PanelKind::Thread(id) => self.thread_focused(id, panel.entity_id(), window, cx),
            PanelKind::Terminal(id) => self.terminal_focused(id, window, cx),
            PanelKind::Browser(id) => self.browser_focused(id, cx),
            PanelKind::Projects | PanelKind::Welcome => {}
        }
        focus.focus(window, cx);
        if let Some(terminal) = panel.read(cx).terminal.clone() {
            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
        }
        if let Some(browser) = panel.read(cx).browser.clone() {
            browser.update(cx, |browser, cx| browser.focus(window, cx));
        }
        self.sync_terminal_visibility(window, cx);
        cx.notify();
    }

    pub(super) fn navigate_panel(
        &mut self,
        panel: Entity<WorkspacePanel>,
        sidebar: Option<SidebarFocus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        if let Some(current) = self.focused_panel(window, cx) {
            let kind = current.read(cx).kind;
            if let PanelKind::Thread(id) = kind
                && !self
                    .sidebar_focus_target(id, cx)
                    .is_some_and(|target| target.contains_focused(window, cx))
            {
                let restore = self
                    .prompt_focus_handle(id, cx)
                    .is_some_and(|focus| focus.contains_focused(window, cx));
                let waiting = self
                    .pending_prompt_focus
                    .as_ref()
                    .is_some_and(|(_, anchor)| anchor.is_focused(window));
                current.update(cx, |panel, _| {
                    panel.restore_prompt_focus = restore || waiting
                });
            }
        }
        let kind = panel.read(cx).kind;
        let restore = panel.read(cx).restore_prompt_focus;
        let anchor = panel.read(cx).focus.clone();
        self.focus_panel(panel, window, cx);
        if let Some(sidebar) = sidebar {
            sidebar.focus(window, cx);
        } else if restore && let PanelKind::Thread(id) = kind {
            self.pending_prompt_focus = Some((id, anchor));
            self.restore_pending_prompt_focus(window, cx);
        }
    }

    pub(in crate::ui) fn restore_pending_prompt_focus(
        &mut self,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some((id, anchor)) = self.pending_prompt_focus.clone() else {
            return;
        };
        let blocked = self.navigation_blocked(window, cx);
        if !anchor.is_focused(window)
            || self.state.read(cx).selected_thread != Some(id)
            || self.state.read(cx).editor_error.is_some()
            || blocked
        {
            self.pending_prompt_focus = None;
        } else if let Some(focus) = self.prompt_focus_handle(id, cx) {
            self.pending_prompt_focus = None;
            focus.focus(window, cx);
        }
    }
    pub fn spatial_focus(
        &mut self,
        direction: PaneDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        let Some(current) = self.focused_panel(window, cx) else {
            return;
        };
        let dock = self.dock.read(cx);
        let visible_bounds = |entity: &Entity<WorkspacePanel>| {
            if !panel_visible(entity, dock, cx) {
                return None;
            }
            let panel = entity.read(cx);
            let bounds = panel.bounds.get()?;
            (bounds.size.width > px(0.)
                && bounds.size.height > px(0.)
                && dock.bounds().contains(&bounds.center()))
            .then_some(bounds)
        };
        let mut surfaces = Vec::new();
        let mut origin_is_sidebar = false;
        for panel in self.all_panels() {
            let Some(mut bounds) = visible_bounds(&panel) else {
                continue;
            };
            if let PanelKind::Thread(id) = panel.read(cx).kind
                && let Some(target) = self.sidebar_focus_target(id, cx)
                && let Some(width) = self
                    .sidebar_sizes
                    .borrow()
                    .get(&id)
                    .and_then(|sizes| sizes.read(cx).sizes().get(1).copied())
            {
                let width = width.clamp(px(0.), bounds.size.width);
                let sidebar_bounds = Bounds::new(
                    point(bounds.right() - width, bounds.top()),
                    size(width, bounds.size.height),
                );
                bounds.size.width -= width;
                if panel == current {
                    origin_is_sidebar = target.contains_focused(window, cx);
                }
                surfaces.push((panel.clone(), Some(target), sidebar_bounds));
            }
            surfaces.push((panel, None, bounds));
        }
        let Some(origin) = surfaces
            .iter()
            .find(|(panel, sidebar, _)| *panel == current && sidebar.is_some() == origin_is_sidebar)
            .map(|(_, _, bounds)| *bounds)
        else {
            return;
        };
        let next = surfaces
            .into_iter()
            .filter(|(panel, sidebar, _)| {
                *panel != current || sidebar.is_some() != origin_is_sidebar
            })
            .filter_map(|(panel, sidebar, bounds)| {
                let overlap = origin.intersect(&bounds).size;
                let delta = bounds.center() - origin.center();
                let (gap, overlap, cross) = match direction {
                    PaneDirection::Left => {
                        (origin.left() - bounds.right(), overlap.height, delta.y)
                    }
                    PaneDirection::Right => {
                        (bounds.left() - origin.right(), overlap.height, delta.y)
                    }
                    PaneDirection::Up => (origin.top() - bounds.bottom(), overlap.width, delta.x),
                    PaneDirection::Down => (bounds.top() - origin.bottom(), overlap.width, delta.x),
                };
                (gap >= px(-1.) && overlap > px(1.)).then_some((
                    (panel, sidebar),
                    f32::from(gap).max(0.),
                    f32::from(cross).abs(),
                ))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.2.total_cmp(&b.2)))
            .map(|(panel, _, _)| panel);
        if let Some((next, sidebar)) = next {
            self.navigate_panel(next, sidebar, window, cx);
        }
    }
}
