use super::*;

pub(super) fn thread_of(state: &PanelState) -> Option<ThreadId> {
    match &state.info {
        PanelInfo::Panel(value) => serde_json::from_value(value.get("thread")?.clone()).ok(),
        _ => None,
    }
}
pub(super) fn terminal_of(state: &PanelState) -> Option<TerminalId> {
    match &state.info {
        PanelInfo::Panel(value) => serde_json::from_value(value.get("terminal")?.clone()).ok(),
        _ => None,
    }
}
#[derive(serde::Deserialize)]
pub(super) struct TerminalTab {
    pub(super) terminal: TerminalId,
    pub(super) project: ProjectId,
    #[serde(default)]
    pub(super) worktree: Option<WorktreeId>,
    #[serde(default)]
    pub(super) neovim: Option<NeovimTarget>,
}
pub(super) fn terminal_tab(state: &PanelState) -> Option<TerminalTab> {
    match &state.info {
        PanelInfo::Panel(value) if state.panel_name == "Terminal" => {
            serde_json::from_value(value.clone()).ok()
        }
        _ => None,
    }
}
pub(super) const LAYOUT_VERSION: usize = 4;
pub(super) const OUTDATED_LAYOUT: &str = "Layout was saved by another version";

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) struct ProjectsSidebar {
    pub(super) size: Pixels,
    pub(super) open: bool,
}

impl ProjectsSidebar {
    pub(super) fn capture(layout: &DockAreaState) -> Option<Self> {
        [&layout.left_dock, &layout.right_dock, &layout.bottom_dock]
            .into_iter()
            .flatten()
            .find(|dock| contains_panel(dock.panel(), "Projects"))
            .map(|dock| Self {
                size: dock.size(),
                open: dock.open(),
            })
    }

    pub(super) fn apply(self, layout: &mut DockAreaState) {
        for dock in [
            &mut layout.left_dock,
            &mut layout.right_dock,
            &mut layout.bottom_dock,
        ]
        .into_iter()
        .flatten()
        {
            if contains_panel(dock.panel(), "Projects") {
                *dock =
                    DockState::new(dock.panel().clone(), dock.placement(), self.size, self.open);
            }
        }
    }
}
pub(super) fn panel_visible(entity: &Entity<WorkspacePanel>, dock: &DockArea, cx: &App) -> bool {
    let panel_id = PanelId::from(entity.entity_id());
    if [
        DockPlacement::Left,
        DockPlacement::Right,
        DockPlacement::Bottom,
    ]
    .into_iter()
    .any(|placement| {
        !dock.is_dock_open(placement)
            && dock
                .layout(placement)
                .is_some_and(|tree| tree.find_panel_node(panel_id).is_some())
    }) {
        return false;
    }
    let panel = entity.read(cx);
    let Some(group) = panel.group.as_ref().and_then(WeakEntity::upgrade) else {
        return false;
    };
    let group = group.read(cx);
    !group.is_collapsed()
        && dock.zoomed_group().is_none_or(|node| node == group.node())
        && group
            .active_panel(cx)
            .is_some_and(|active| active.panel_id(cx) == panel_id)
}

pub(super) fn filter_layout(
    mut layout: DockAreaState,
    keep: &impl Fn(&PanelState) -> bool,
) -> DockAreaState {
    fn filter(mut panel: PanelState, keep: &impl Fn(&PanelState) -> bool) -> Option<PanelState> {
        if matches!(panel.info, PanelInfo::Panel(_)) {
            return keep(&panel).then_some(panel);
        }
        let children = std::mem::take(&mut panel.children);
        let retained: Vec<_> = children
            .into_iter()
            .enumerate()
            .filter_map(|(index, child)| filter(child, keep).map(|child| (index, child)))
            .collect();
        if retained.is_empty() {
            return None;
        }
        match &mut panel.info {
            PanelInfo::Tabs { active_index } => {
                *active_index = retained
                    .iter()
                    .position(|(index, _)| *index >= *active_index)
                    .unwrap_or(retained.len() - 1);
            }
            PanelInfo::Stack { sizes, .. } => {
                if !sizes.is_empty() {
                    *sizes = retained
                        .iter()
                        .filter_map(|(index, _)| sizes.get(*index).copied())
                        .collect();
                }
            }
            PanelInfo::Panel(_) => {}
        }
        panel.children = retained.into_iter().map(|(_, child)| child).collect();
        Some(panel)
    }
    layout.center = filter(layout.center, keep).unwrap_or_else(|| PanelState {
        panel_name: "TabPanel".into(),
        info: PanelInfo::Tabs { active_index: 0 },
        children: vec![PanelState::new("Welcome")],
    });
    for edge in [
        &mut layout.left_dock,
        &mut layout.right_dock,
        &mut layout.bottom_dock,
    ] {
        *edge = edge.take().and_then(|dock| {
            filter(dock.panel().clone(), keep)
                .map(|panel| DockState::new(panel, dock.placement(), dock.size(), dock.open()))
        });
    }
    layout
}
pub(super) fn durable_layout(layout: DockAreaState) -> DockAreaState {
    filter_layout(layout, &|panel| {
        panel.panel_name != "Terminal" || terminal_tab(panel).is_some()
    })
}

pub(super) fn contains_panel(panel: &PanelState, name: &str) -> bool {
    panel.panel_name == name
        || panel
            .children
            .iter()
            .any(|child| contains_panel(child, name))
}
pub(super) fn contains_kind(panel: &PanelState, kind: PanelKind) -> bool {
    kind.matches(panel)
        || panel
            .children
            .iter()
            .any(|child| contains_kind(child, kind))
}
pub(super) fn validate_layout(layout: &DockAreaState) -> anyhow::Result<()> {
    anyhow::ensure!(layout.version == Some(LAYOUT_VERSION), OUTDATED_LAYOUT);
    let mut names = Vec::new();
    fn visit<'a>(
        panel: &'a PanelState,
        names: &mut Vec<&'a str>,
        depth: usize,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(depth < 32, "Layout nesting is too deep");
        match &panel.info {
            PanelInfo::Panel(_) => {
                anyhow::ensure!(panel.children.is_empty(), "Leaf panel contains children");
                anyhow::ensure!(
                    panel.panel_name != "Thread" || thread_of(panel).is_some(),
                    "Thread panel names no thread"
                );
                anyhow::ensure!(
                    panel.panel_name != "Terminal" || terminal_tab(panel).is_some(),
                    "Terminal panel names no project"
                );
                names.push(&panel.panel_name);
            }
            PanelInfo::Tabs { active_index } => {
                anyhow::ensure!(
                    panel.panel_name == "TabPanel"
                        && panel
                            .children
                            .iter()
                            .all(|child| matches!(child.info, PanelInfo::Panel(_))),
                    "Invalid tab group"
                );
                anyhow::ensure!(
                    !panel.children.is_empty() && *active_index < panel.children.len(),
                    "Invalid active layout tab"
                );
            }
            PanelInfo::Stack { sizes, axis } => {
                anyhow::ensure!(panel.panel_name == "StackPanel", "Invalid split container");
                anyhow::ensure!(
                    *axis <= 1
                        && !panel.children.is_empty()
                        && (sizes.is_empty() || sizes.len() == panel.children.len())
                        && sizes
                            .iter()
                            .all(|size| f32::from(*size).is_finite() && *size >= px(0.)),
                    "Invalid layout split"
                );
            }
        }
        for child in &panel.children {
            visit(child, names, depth + 1)?;
        }
        Ok(())
    }
    visit(&layout.center, &mut names, 0)?;
    for (placement, dock) in [
        (DockPlacement::Left, &layout.left_dock),
        (DockPlacement::Right, &layout.right_dock),
        (DockPlacement::Bottom, &layout.bottom_dock),
    ] {
        let Some(dock) = dock else { continue };
        anyhow::ensure!(dock.placement() == placement, "Mismatched dock placement");
        anyhow::ensure!(
            f32::from(dock.size()).is_finite() && dock.size() > px(0.),
            "Invalid dock size"
        );
        visit(dock.panel(), &mut names, 0)?;
    }
    let count = |name: &str| names.iter().filter(|candidate| **candidate == name).count();
    anyhow::ensure!(
        count("Projects") == 1 && count("Welcome") <= 1,
        "Layout must contain exactly one Projects panel"
    );
    anyhow::ensure!(
        names.iter().all(|name| matches!(
            *name,
            "Projects" | "Welcome" | "Thread" | "Terminal" | "Browser"
        )),
        "Layout contains an unknown panel"
    );
    Ok(())
}

impl Workspace {
    pub(super) fn restorable_layout(&self, layout: DockAreaState, cx: &App) -> DockAreaState {
        let state = self.state.read(cx);
        filter_layout(layout, &|panel| {
            if panel.panel_name == "Browser" {
                return browser_of(panel)
                    .is_some_and(|id| self.browsers.panels.borrow().contains_key(&id));
            }
            panel.panel_name != "Terminal"
                || terminal_of(panel).is_some_and(|id| self.terminals.borrow().contains_key(&id))
                || terminal_tab(panel).is_some_and(|tab| {
                    state.project_in_space(tab.project, state.active_space)
                        && state.snapshot.projects.iter().any(|p| p.id == tab.project)
                })
        })
    }
    pub fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_layout_with_sidebar(None, window, cx);
    }

    pub(super) fn reset_layout_with_sidebar(
        &mut self,
        sidebar: Option<ProjectsSidebar>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reopening = None;
        let open = self.open_thread_ids(cx);
        let selected = self.state.read(cx).selected_thread;
        let owner = cx.entity().downgrade();
        let mut center = DockLayout::tabs();
        let terminals = self.active_space_terminals(cx);
        if open.is_empty() && terminals.is_empty() {
            let welcome = welcome_panel(&self.welcome, &self.state, &owner, window, cx);
            center = center.panel_view(panel_handle(welcome), cx);
        } else {
            for (ix, id) in open.iter().enumerate() {
                let panel = thread_panel(&self.threads, *id, &self.state, &owner, window, cx);
                center = center.panel_view(panel_handle(panel), cx);
                if Some(*id) == selected {
                    center = center.active_index(ix);
                }
            }
        }
        for terminal in terminals {
            center = center.panel_view(panel_handle(terminal), cx);
        }
        let left = DockLayout::tabs().panel_view(panel_handle(self.projects.clone()), cx);
        let rem = window.rem_size();
        self.rebuilding = true;
        self.dock.update(cx, |dock, cx| {
            dock.set_version(Some(LAYOUT_VERSION), cx);
            dock.remove_dock(DockPlacement::Right, window, cx);
            dock.remove_dock(DockPlacement::Bottom, window, cx);
            dock.set_center(center, window, cx);
            dock.set_dock(DockPlacement::Left, left, window, cx);
            dock.set_dock_size(
                DockPlacement::Left,
                sidebar.map_or(rem * 16., |sidebar| sidebar.size),
                window,
                cx,
            );
            let open = sidebar.is_none_or(|sidebar| sidebar.open);
            if dock.is_dock_open(DockPlacement::Left) != open {
                dock.toggle_dock(DockPlacement::Left, window, cx);
            }
            dock.set_dock_collapsible(DockPlacement::Left, true, window, cx);
        });
        self.rebuilding = false;
        self.sync_thread_panels(window, cx);
        self.invalidate_panel_bounds(cx);
        self.normalize_layout(None, window, cx);
        self.flush_layout(cx);
        cx.notify();
    }

    pub(super) fn invalidate_panel_bounds(&self, cx: &App) {
        for panel in self.all_panels() {
            panel.read(cx).bounds.set(None);
        }
    }

    pub(super) fn toggle_region(
        &mut self,
        region: Region,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        let layout = self.dock.read(cx).dump(cx);
        for (placement, dock) in [
            (DockPlacement::Left, &layout.left_dock),
            (DockPlacement::Right, &layout.right_dock),
            (DockPlacement::Bottom, &layout.bottom_dock),
        ] {
            if dock
                .as_ref()
                .is_some_and(|dock| contains_panel(dock.panel(), region.name()))
            {
                let opening = !self.dock.read(cx).is_dock_open(placement);
                let was_focused = self.focused_region(window, cx) == region;
                self.dock
                    .update(cx, |dock, cx| dock.toggle_dock(placement, window, cx));
                self.invalidate_panel_bounds(cx);
                self.normalize_layout(opening.then_some(region), window, cx);
                if !opening && was_focused {
                    self.focus_region(Region::Thread, window, cx);
                }
                return;
            }
        }
        self.focus_region(region, window, cx);
    }

    pub(super) fn normalize_layout(
        &mut self,
        priority: Option<Region>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if priority.is_some() {
            self.reopening = priority;
        }
        let dock = self.dock.read(cx);
        let layout = dock.dump(cx);
        let rem = window.rem_size();
        let mut area = dock.bounds();
        if area.size.width <= px(0.) || area.size.height <= px(0.) {
            area = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
        }
        let edges = [
            (DockPlacement::Left, layout.left_dock.as_ref()),
            (DockPlacement::Right, layout.right_dock.as_ref()),
            (DockPlacement::Bottom, layout.bottom_dock.as_ref()),
        ];
        let minima = [rem * 12., rem * 12., rem * 8.];
        let mut sizes = [px(0.); 3];
        let mut open = [false; 3];
        for (i, (_, edge)) in edges.iter().enumerate() {
            if let Some(edge) = edge {
                sizes[i] = edge.size().max(minima[i]).min(rem * 24.);
                open[i] = edge.open();
            }
        }
        let available = [
            area.size.width - rem * 28.,
            area.size.height - TITLE_BAR_HEIGHT - rem * 16.,
        ];
        let fits = |open: &[bool; 3], sizes: &[Pixels; 3]| {
            (if open[0] { sizes[0] } else { px(0.) }) + (if open[1] { sizes[1] } else { px(0.) })
                <= available[0]
                && (!open[2] || sizes[2] <= available[1])
        };
        if !fits(&open, &sizes) {
            for i in 0..3 {
                if open[i] {
                    sizes[i] = minima[i];
                }
            }
        }
        for region in [Region::Projects] {
            if fits(&open, &sizes) {
                break;
            }
            if priority == Some(region) {
                continue;
            }
            for (i, (_, edge)) in edges.iter().enumerate() {
                if edge.is_some_and(|edge| {
                    contains_panel(edge.panel(), region.name())
                        && !contains_panel(edge.panel(), Region::Thread.name())
                        && priority
                            .is_none_or(|priority| !contains_panel(edge.panel(), priority.name()))
                }) {
                    open[i] = false;
                }
            }
        }
        for (i, (placement, edge)) in edges.iter().enumerate() {
            let Some(edge) = edge else { continue };
            let opposite = match i {
                0 if open[1] => sizes[1],
                1 if open[0] => sizes[0],
                _ => px(0.),
            };
            let sizing = DockSizing::new(*placement)
                .with_area_bounds(area)
                .with_opposite_dock_size(opposite);
            let reserve_cap = if i == 2 {
                available[1]
            } else {
                available[0] - opposite
            };
            sizes[i] = sizing.clamp(sizes[i].min(reserve_cap.max(minima[i])));
            if sizes[i] != edge.size() || open[i] != edge.open() {
                self.dock.update(cx, |dock, cx| {
                    if sizes[i] != edge.size() {
                        dock.set_dock_size(*placement, sizes[i], window, cx);
                    }
                    if open[i] != edge.open() {
                        dock.toggle_dock(*placement, window, cx);
                    }
                });
            }
        }
    }

    pub(super) fn schedule_layout_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rebuilding || self.presented_space != self.state.read(cx).active_space {
            return;
        }
        let space = self.presented_space;
        self.layout_save = Some(cx.spawn_in(window, async move |workspace, window| {
            smol::Timer::after(std::time::Duration::from_millis(250)).await;
            let _ = workspace.update_in(window, |workspace, _, cx| {
                if workspace.presented_space == space
                    && workspace.state.read(cx).active_space == space
                {
                    workspace.flush_layout(cx);
                }
            });
        }));
    }

    pub(in crate::ui) fn flush_layout(&mut self, cx: &mut Context<Self>) {
        if self.state.read(cx).shutting_down
            || self.rebuilding
            || self.presented_space != self.state.read(cx).active_space
        {
            return;
        }
        self.save_browsers(cx);
        let live = self.dock.read(cx).dump(cx);
        self.live_layouts.insert(self.presented_space, live.clone());
        let sidebar = ProjectsSidebar::capture(&live).map(|sidebar| json!(sidebar));
        let sidebar_changed = sidebar.as_ref().is_some_and(|sidebar| {
            self.state
                .read(cx)
                .snapshot
                .settings
                .get("projects_sidebar")
                != Some(sidebar)
        });
        let layout = json!(durable_layout(live));
        if sidebar_changed || self.state.read(cx).space_layout() != Some(&layout) {
            self.state.update(cx, |state, cx| {
                if let Some(sidebar) = sidebar {
                    state.set_setting("projects_sidebar", sidebar);
                }
                state.set_space_layout(layout);
                cx.notify();
            });
        }
    }
}

#[cfg(test)]
mod terminal_layout_tests {
    use super::{DockAreaState, LAYOUT_VERSION, durable_layout, validate_layout};
    use serde_json::json;

    #[test]
    fn saving_mixed_layout_preserves_thread_selection_and_split_sizes() {
        let first = crate::model::ThreadId::new_v4();
        let second = crate::model::ThreadId::new_v4();
        let thread = |id: crate::model::ThreadId| {
            json!({
                "panel_name": "Thread", "children": [], "info": {"panel": {"thread": id}}
            })
        };
        let terminal = json!({
            "panel_name": "Terminal", "children": [], "info": {"panel": {"terminal": "ephemeral"}}
        });
        let tabs = |children, active| {
            json!({
                "panel_name": "TabPanel", "children": children, "info": {"tabs": {"active_index": active}}
            })
        };
        let layout: DockAreaState = serde_json::from_value(json!({
            "version": LAYOUT_VERSION,
            "center": {
                "panel_name": "StackPanel",
                "children": [
                    tabs(json!([thread(first), terminal, thread(second)]), 1),
                    tabs(json!([terminal]), 0),
                    tabs(json!([thread(first)]), 0)
                ],
                "info": {"stack": {"sizes": [350., 200., 450.], "axis": 0}}
            },
            "left_dock": {
                "panel": tabs(json!([{"panel_name": "Projects", "children": [], "info": {"panel": null}}]), 0),
                "placement": "left", "size": 240., "open": true
            }
        })).unwrap();
        let saved = durable_layout(layout);
        validate_layout(&saved).unwrap();
        let saved = serde_json::to_value(saved).unwrap();
        assert_eq!(
            saved["center"]["info"]["stack"]["sizes"],
            json!([350., 450.])
        );
        assert_eq!(
            saved["center"]["children"][0],
            tabs(json!([thread(first), thread(second)]), 1)
        );
        assert_eq!(
            saved["center"]["children"][1],
            tabs(json!([thread(first)]), 0)
        );
    }

    #[test]
    fn saving_keeps_terminal_tabs_with_their_project() {
        let thread = crate::model::ThreadId::new_v4();
        let project = crate::model::ProjectId::new_v4();
        let worktree = crate::model::WorktreeId::new_v4();
        let terminal = json!({
            "panel_name": "Terminal", "children": [],
            "info": {"panel": {
                "terminal": uuid::Uuid::new_v4(), "project": project, "worktree": worktree
            }}
        });
        let layout: DockAreaState = serde_json::from_value(json!({
            "version": LAYOUT_VERSION,
            "center": {
                "panel_name": "TabPanel",
                "children": [
                    {"panel_name": "Thread", "children": [], "info": {"panel": {"thread": thread}}},
                    terminal
                ],
                "info": {"tabs": {"active_index": 1}}
            },
            "left_dock": {
                "panel": {"panel_name": "TabPanel", "children": [
                    {"panel_name": "Projects", "children": [], "info": {"panel": null}}
                ], "info": {"tabs": {"active_index": 0}}},
                "placement": "left", "size": 240., "open": true
            }
        }))
        .unwrap();
        let saved = durable_layout(layout);
        validate_layout(&saved).unwrap();
        let tab = super::terminal_tab(&saved.center.children[1]).unwrap();
        assert_eq!((tab.project, tab.worktree), (project, Some(worktree)));
        let saved = serde_json::to_value(saved).unwrap();
        assert_eq!(saved["center"]["children"][1], terminal);
        assert_eq!(saved["center"]["info"]["tabs"]["active_index"], json!(1));
    }

    #[test]
    fn saving_keeps_neovim_tabs_with_their_file_and_line() {
        let project = crate::model::ProjectId::new_v4();
        let leaf = |neovim: serde_json::Value| {
            json!({
                "panel_name": "Terminal", "children": [],
                "info": {"panel": {
                    "terminal": uuid::Uuid::new_v4(), "project": project, "worktree": null,
                    "neovim": neovim
                }}
            })
        };
        let layout: DockAreaState = serde_json::from_value(json!({
            "version": LAYOUT_VERSION,
            "center": {"panel_name": "TabPanel", "children": [
                leaf(json!({"file": "/work/src/browser_tools.rs", "line": 149, "column": null})),
                leaf(json!(null))
            ], "info": {"tabs": {"active_index": 0}}},
            "left_dock": {
                "panel": {"panel_name": "TabPanel", "children": [
                    {"panel_name": "Projects", "children": [], "info": {"panel": null}}
                ], "info": {"tabs": {"active_index": 0}}},
                "placement": "left", "size": 240., "open": true
            }
        }))
        .unwrap();
        let saved = durable_layout(layout);
        validate_layout(&saved).unwrap();
        let neovim = super::terminal_tab(&saved.center.children[0]).unwrap();
        assert_eq!(
            neovim.neovim,
            Some(super::NeovimTarget {
                file: Some("/work/src/browser_tools.rs".into()),
                line: Some(149),
                column: None,
            })
        );
        let shell = super::terminal_tab(&saved.center.children[1]).unwrap();
        assert_eq!((shell.project, shell.neovim), (project, None));
    }

    #[test]
    fn terminal_leaf_without_project_is_invalid() {
        let layout: DockAreaState = serde_json::from_value(json!({
            "version": LAYOUT_VERSION,
            "center": {"panel_name": "TabPanel", "children": [
                {"panel_name": "Terminal", "children": [],
                 "info": {"panel": {"terminal": uuid::Uuid::new_v4()}}}
            ], "info": {"tabs": {"active_index": 0}}},
            "left_dock": {
                "panel": {"panel_name": "TabPanel", "children": [
                    {"panel_name": "Projects", "children": [], "info": {"panel": null}}
                ], "info": {"tabs": {"active_index": 0}}},
                "placement": "left", "size": 240., "open": true
            }
        }))
        .unwrap();
        assert!(validate_layout(&layout).is_err());
        validate_layout(&durable_layout(layout)).unwrap();
    }
}
