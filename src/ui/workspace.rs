use super::{
    commands::{self, *},
    conversation, factory,
    settings::{Dialog, DialogKind},
    sidebar::{self, Row},
};
use crate::{
    app::{AppState, NoticeLevel},
    model::PaneDirection,
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Root, Sizable, TitleBar, WindowExt,
        button::{Button, ButtonVariants},
        dock::{
            BasePanel, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSizing,
            DockSkin, Panel, PanelEvent, PanelInfo, PanelState, TabGroup, panel_handle,
            register_panel,
        },
        h_flex,
        menu::{DropdownMenu, PopupMenu, PopupMenuItem},
        notification::Notification,
        tab::{Tab, TabBar},
        tag::Tag,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::json;
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Projects,
    Thread,
    Output,
}
impl Region {
    pub const ALL: [Self; 3] = [Self::Projects, Self::Thread, Self::Output];
    pub fn index(self) -> usize {
        match self {
            Self::Projects => 0,
            Self::Thread => 1,
            Self::Output => 2,
        }
    }
    pub fn name(self) -> &'static str {
        commands::region_action(self)
    }
    fn context(self) -> &'static str {
        match self {
            Self::Projects => "List ProjectsList",
            Self::Thread => "Thread",
            Self::Output => "List",
        }
    }
}

pub struct Workspace {
    pub state: Entity<AppState>,
    pub(super) dock: Entity<DockArea>,
    _skin: Rc<DockSkin>,
    panels: Vec<Entity<WorkspacePanel>>,
    focus: FocusHandle,
    pub cursor: Option<Row>,
    pub palette: Option<Entity<Palette>>,
    pub dialog: Option<Dialog>,
    pub(super) editor: Option<Entity<super::editor::PromptEditor>>,
    pub(super) editor_selection: Option<crate::model::DraftKey>,
    pub(super) editor_override: Option<(crate::model::DraftKey, String)>,
    pub image_preview: Option<crate::model::ImageAttachment>,
    pub expanded: std::collections::HashSet<String>,
    pub extension: Option<super::overlays::ExtensionForm>,
    pub tool_detail: Option<(crate::model::ThreadId, String)>,
    pub factory_form: Option<super::factory_form::FactoryThreadForm>,
    pub(super) model_picker: Option<conversation::ModelPicker>,
    pub(super) detail_surface: Option<super::overlays::SurfaceKey>,
    pub(super) quit_requested: bool,
    scrolls: [ScrollHandle; 3],
    thread_tab_scroll: ScrollHandle,
    layout_save: Option<Task<()>>,
    seen_notice_sequence: u64,
    reopening: Option<Region>,
    dock_measurement: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub(super) _subscriptions: Vec<Subscription>,
}
impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let owner = cx.entity().downgrade();
        let panels: Vec<_> = Region::ALL
            .into_iter()
            .map(|region| {
                let state = state.clone();
                let owner = owner.clone();
                let focus = cx.focus_handle();
                cx.new(|cx| WorkspacePanel::new(region, state, owner, focus, cx))
            })
            .collect();
        for (region, panel) in Region::ALL.into_iter().zip(&panels) {
            let panel = panel.clone();
            register_panel(cx, region.name(), move |_, _, _| {
                panel_handle(panel.clone())
            });
        }
        let (dock, skin) = DockSkin::dock_area("drost-workspace", Some(2), window, cx);
        let saved = state.read(cx).snapshot.settings.get("layout").cloned();
        let mut workspace = Self {
            state,
            dock,
            _skin: skin,
            panels,
            focus,
            editor: None,
            editor_selection: None,
            editor_override: None,
            image_preview: None,
            expanded: std::collections::HashSet::new(),
            extension: None,
            tool_detail: None,
            factory_form: None,
            quit_requested: false,
            cursor: None,
            model_picker: None,
            detail_surface: None,
            palette: None,
            dialog: None,
            scrolls: std::array::from_fn(|_| ScrollHandle::new()),
            thread_tab_scroll: ScrollHandle::new(),
            layout_save: None,
            seen_notice_sequence: 0,
            reopening: None,
            dock_measurement: Rc::new(Cell::new(None)),
            _subscriptions: Vec::new(),
        };
        let restored = saved.map(|saved| {
            let layout: DockAreaState = serde_json::from_value(saved)?;
            validate_layout(&layout)?;
            workspace
                .dock
                .update(cx, |dock, cx| dock.load(layout, window, cx))
        });
        match restored {
            Some(Ok(())) => {}
            other => {
                workspace.reset_layout(window, cx);
                if let Some(Err(error)) = other {
                    workspace.state.update(cx, |state, cx| {
                        state.notice(
                            crate::app::NoticeLevel::Error,
                            format!("Only the saved layout was reset: {error:#}"),
                        );
                        cx.notify();
                    });
                }
            }
        }
        workspace.dock.update(cx, |dock, cx| {
            for placement in [
                DockPlacement::Left,
                DockPlacement::Right,
                DockPlacement::Bottom,
            ] {
                dock.set_dock_collapsible(placement, true, window, cx);
            }
        });
        workspace._subscriptions.push(cx.observe_in(
            &workspace.state,
            window,
            |workspace, _, window, cx| {
                if workspace.state.read(cx).shutdown_error.is_some() {
                    workspace.quit_requested = false;
                }
                workspace.sync_session(window, cx);
                workspace.sync_factory_form(window, cx);
                workspace.sync_model_picker(window, cx);
                workspace.sync_detail_surface(window, cx);
                workspace.show_notices(window, cx);
                cx.notify();
            },
        ));
        workspace._subscriptions.push(cx.subscribe_in(
            &workspace.dock,
            window,
            |workspace, _, event: &DockEvent, window, cx| {
                if matches!(event, DockEvent::LayoutChanged)
                    && !workspace.state.read(cx).shutting_down
                {
                    workspace.invalidate_panel_bounds(cx);
                    workspace.normalize_layout(workspace.reopening, window, cx);
                    workspace.schedule_layout_save(window, cx);
                }
            },
        ));
        workspace
            ._subscriptions
            .push(cx.observe_window_bounds(window, |workspace, window, cx| {
                workspace.reopening = None;
                workspace.invalidate_panel_bounds(cx);
                workspace.normalize_layout(None, window, cx);
                workspace.schedule_layout_save(window, cx);
            }));
        workspace
            ._subscriptions
            .push(cx.observe_global_in::<gpui_kit::component::Theme>(
                window,
                |workspace, window, cx| {
                    workspace.reopening = None;
                    workspace.invalidate_panel_bounds(cx);
                    workspace.normalize_layout(None, window, cx);
                    workspace.schedule_layout_save(window, cx);
                },
            ));
        workspace
            ._subscriptions
            .extend(super::theme::attach(workspace.state.clone(), window, cx));
        workspace.normalize_layout(None, window, cx);
        workspace.sync_factory_form(window, cx);
        workspace.sync_model_picker(window, cx);
        cx.defer_in(window, |workspace, window, cx| {
            workspace.show_notices(window, cx)
        });
        let projects_focus = workspace.panels[Region::Projects.index()]
            .read(cx)
            .focus
            .clone();
        projects_focus.focus(window, cx);
        workspace.start_editor(window, cx);
        let quit_owner = cx.entity().downgrade();
        let quit_window = window.window_handle();
        App::on_action(cx, move |_: &Quit, cx| {
            let owner = quit_owner.clone();
            cx.defer(move |cx| {
                let _ = cx.update_window(quit_window, |_, window, cx| {
                    let _ = owner.update(cx, |workspace, cx| workspace.quit_review(window, cx));
                });
            });
        });
        let closing = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = closing.update(cx, |workspace, cx| workspace.quit_review(window, cx));
            false
        });
        workspace
    }

    fn cycle_thread(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            let count = state.open_threads.len();
            if count == 0 {
                return;
            }
            let current = state
                .open_threads
                .iter()
                .position(|id| Some(*id) == state.selected_thread);
            let next = match current {
                Some(ix) if forward => (ix + 1) % count,
                Some(ix) => (ix + count - 1) % count,
                None if forward => 0,
                None => count - 1,
            };
            state.select_thread(state.open_threads[next]);
            cx.notify();
        });
        self.focus_region(Region::Thread, window, cx);
    }

    pub fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reopening = None;
        let tabs = |index: usize, cx: &App| {
            DockLayout::tabs().panel_view(panel_handle(self.panels[index].clone()), cx)
        };
        let center = tabs(1, cx);
        let left = tabs(0, cx);
        let bottom = tabs(2, cx);
        // Dock APIs consume pixels; application geometry is resolved from rem here.
        let rem = window.rem_size();
        self.dock.update(cx, |dock, cx| {
            dock.set_version(Some(2), cx);
            dock.remove_dock(DockPlacement::Right, window, cx);
            dock.set_center(center, window, cx);
            for (placement, layout, size) in [
                (DockPlacement::Left, left, rem * 16.),
                (DockPlacement::Bottom, bottom, rem * 12.),
            ] {
                dock.set_dock(placement, layout, window, cx);
                dock.set_dock_size(placement, size, window, cx);
                dock.set_dock_collapsible(placement, true, window, cx);
            }
            if dock.is_dock_open(DockPlacement::Bottom) {
                dock.toggle_dock(DockPlacement::Bottom, window, cx);
            }
        });
        self.invalidate_panel_bounds(cx);
        self.normalize_layout(None, window, cx);
        self.flush_layout(cx);
        cx.notify();
    }

    fn navigation_blocked(&self, window: &mut Window, cx: &mut App) -> bool {
        self.quit_requested
            || self.state.read(cx).shutting_down
            || self.palette.is_some()
            || self.dialog.is_some()
            || self.extension.is_some()
            || self.image_preview.is_some()
            || self.tool_detail.is_some()
            || window.has_active_dialog(cx)
            || window.has_active_sheet(cx)
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
    }

    fn invalidate_panel_bounds(&self, cx: &App) {
        for panel in &self.panels {
            panel.read(cx).bounds.set(None);
        }
    }

    fn toggle_region(&mut self, region: Region, window: &mut Window, cx: &mut Context<Self>) {
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
                self.dock
                    .update(cx, |dock, cx| dock.toggle_dock(placement, window, cx));
                self.invalidate_panel_bounds(cx);
                self.normalize_layout(opening.then_some(region), window, cx);
                return;
            }
        }
        // A panel moved into the center is a native tab, not an auxiliary dock.
        self.focus_region(region, window, cx);
    }

    fn normalize_layout(
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
            // First layout has no dock measurement: allow for the native title bar.
            area.size.height = (area.size.height - rem * 2.).max(px(0.));
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
        let available = [area.size.width - rem * 28., area.size.height - rem * 16.];
        let fits = |open: &[bool; 3], sizes: &[Pixels; 3]| {
            (if open[0] { sizes[0] } else { px(0.) }) + (if open[1] { sizes[1] } else { px(0.) })
                <= available[0]
                && (!open[2] || sizes[2] <= available[1])
        };
        if !fits(&open, &sizes) {
            // Shrink allocations before collapsing either auxiliary pane.
            for i in 0..3 {
                if open[i] {
                    sizes[i] = minima[i];
                }
            }
        }
        let order = if priority == Some(Region::Output) {
            [Region::Projects, Region::Output]
        } else {
            [Region::Output, Region::Projects]
        };
        for region in order {
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

    fn schedule_layout_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layout_save = Some(cx.spawn_in(window, async move |workspace, window| {
            smol::Timer::after(std::time::Duration::from_millis(250)).await;
            let _ = workspace.update_in(window, |workspace, _, cx| workspace.flush_layout(cx));
        }));
    }

    pub(super) fn flush_layout(&mut self, cx: &mut Context<Self>) {
        if self.state.read(cx).shutting_down {
            return;
        }
        let layout = json!(self.dock.read(cx).dump(cx));
        if self.state.read(cx).snapshot.settings.get("layout") != Some(&layout) {
            self.state.update(cx, |state, cx| {
                state.set_setting("layout", layout);
                cx.notify();
            });
        }
    }

    fn show_notices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let count = state
            .notice_sequence
            .wrapping_sub(self.seen_notice_sequence) as usize;
        self.seen_notice_sequence = state.notice_sequence;
        if count == 0 {
            return;
        }
        let notes: Vec<_> = state
            .notices
            .iter()
            .skip(state.notices.len().saturating_sub(count))
            .filter(|notice| matches!(notice.level, NoticeLevel::Warning | NoticeLevel::Error))
            .filter(|notice| {
                ![
                    state.storage_error.as_deref(),
                    state.shutdown_error.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|error| notice.text.contains(error))
            })
            .filter(|notice| {
                !state
                    .conversation_thread()
                    .and_then(|thread| state.conversations.get(&thread.id))
                    .and_then(|session| session.error.as_deref())
                    .is_some_and(|error| notice.text.contains(error))
            })
            .cloned()
            .collect();
        for notice in notes {
            window.push_notification(
                match notice.level {
                    NoticeLevel::Warning => Notification::warning(notice.text),
                    _ => Notification::error(notice.text),
                },
                cx,
            );
        }
    }

    pub fn focused_region(&self, window: &Window, cx: &App) -> Region {
        Region::ALL
            .into_iter()
            .find(|region| {
                self.panels[region.index()]
                    .read(cx)
                    .focus
                    .contains_focused(window, cx)
            })
            .unwrap_or(Region::Thread)
    }
    pub fn focus_region(&mut self, region: Region, window: &mut Window, cx: &mut Context<Self>) {
        if self.navigation_blocked(window, cx) {
            return;
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
                .is_some_and(|dock| contains_panel(dock.panel(), region.name()) && !dock.open())
            {
                opened = true;
                self.dock
                    .update(cx, |dock, cx| dock.toggle_dock(placement, window, cx));
            }
        }
        if opened {
            self.invalidate_panel_bounds(cx);
            self.normalize_layout(Some(region), window, cx);
        }
        let panel = self.panels[region.index()].read(cx);
        let group = panel.group.clone();
        let focus = panel.focus.clone();
        if let Some(group) = group {
            if let Some(group) = group.upgrade() {
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
            }
            let _ = group.update(cx, |group, cx| {
                if let Some(index) = group
                    .panels()
                    .iter()
                    .position(|panel| panel.panel_name(cx) == region.name())
                {
                    group.select_tab(index, window, cx);
                }
            });
        }
        focus.focus(window, cx);
        cx.notify();
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
        let current = self.focused_region(window, cx);
        let dock = self.dock.read(cx);
        let layout = dock.dump(cx);
        let zoomed = dock.zoomed_group();
        let visible_bounds = |region: Region| {
            if [&layout.left_dock, &layout.right_dock, &layout.bottom_dock]
                .into_iter()
                .flatten()
                .any(|dock| !dock.open() && contains_panel(dock.panel(), region.name()))
            {
                return None;
            }
            let panel = self.panels[region.index()].read(cx);
            let group = panel.group.as_ref()?.upgrade()?;
            let group = group.read(cx);
            if group.is_collapsed()
                || zoomed.is_some_and(|node| node != group.node())
                || group.active_panel(cx)?.panel_name(cx) != region.name()
            {
                return None;
            }
            let bounds = panel.bounds.get()?;
            (bounds.size.width > px(0.)
                && bounds.size.height > px(0.)
                && dock.bounds().contains(&bounds.center()))
            .then_some(bounds)
        };
        let Some(origin) = visible_bounds(current).map(|bounds| bounds.center()) else {
            return;
        };
        let next = Region::ALL
            .into_iter()
            .filter(|region| *region != current)
            .filter_map(|region| {
                let center = visible_bounds(region)?.center();
                let dx = f32::from(center.x - origin.x);
                let dy = f32::from(center.y - origin.y);
                let (forward, cross) = match direction {
                    PaneDirection::Left => (-dx, dy),
                    PaneDirection::Right => (dx, dy),
                    PaneDirection::Up => (-dy, dx),
                    PaneDirection::Down => (dy, dx),
                };
                (forward > 1.).then_some((region, forward + cross.abs() * 2.))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(region, _)| region);
        if let Some(next) = next {
            self.focus_region(next, window, cx);
        }
    }

    pub fn execute(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).shutting_down {
            return;
        }
        if matches!(
            command,
            Command::NextThread
                | Command::PreviousThread
                | Command::CloseThread
                | Command::NewConversation
                | Command::NewFactoryThread
        ) && self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        if let Some(reason) = command.disabled_reason(self.state.read(cx)) {
            self.state.update(cx, |state, cx| {
                state.notice(crate::app::NoticeLevel::Warning, reason);
                cx.notify();
            });
            return;
        }
        let kind = match command {
            Command::RestartEditor => {
                self.restart_editor(window, cx);
                None
            }
            Command::CancelEditorCommand => {
                self.cancel_editor_command(window, cx);
                None
            }
            Command::CopyDraftRecovery => {
                self.copy_draft_recovery(cx);
                None
            }
            Command::AddProject => Some(DialogKind::AddProject),
            Command::RenameProject => self
                .state
                .read(cx)
                .selected_project
                .map(DialogKind::RenameProject),
            Command::ArchiveProject => self
                .state
                .read(cx)
                .selected_project
                .map(DialogKind::ArchiveProject),
            Command::RenameThread => self
                .state
                .read(cx)
                .selected_thread
                .map(DialogKind::RenameThread),
            Command::ArchiveThread => self
                .state
                .read(cx)
                .selected_thread
                .map(DialogKind::ArchiveThread),
            Command::Settings => Some(DialogKind::Settings(self.state.read(cx).selected_project)),
            Command::NewConversation | Command::NewFactoryThread => {
                let kind = if command == Command::NewConversation {
                    crate::model::ThreadKind::Conversation
                } else {
                    crate::model::ThreadKind::Factory
                };
                self.state.update(cx, |state, cx| {
                    if let Err(error) = state.new_thread(kind) {
                        state.notice(crate::app::NoticeLevel::Error, format!("{error:#}"));
                    }
                    cx.notify();
                });
                self.focus_region(Region::Thread, window, cx);
                None
            }
            Command::ShowArchived => {
                self.state.update(cx, |state, cx| {
                    state.show_archived = !state.show_archived;
                    state.set_setting("show_archived", json!(state.show_archived));
                    cx.notify();
                });
                None
            }
            Command::ResetLayout => {
                self.reset_layout(window, cx);
                None
            }
            Command::RefreshFactoryRuns => {
                self.state.update(cx, |state, cx| {
                    state.refresh_factory_runs();
                    cx.notify();
                });
                None
            }
            Command::CloseThread => {
                self.state.update(cx, |state, cx| {
                    if let Some(id) = state.selected_thread {
                        state.close_thread(id);
                    }
                    cx.notify();
                });
                None
            }
            Command::NextThread | Command::PreviousThread => {
                self.cycle_thread(command == Command::NextThread, window, cx);
                None
            }
            Command::FocusProjects | Command::FocusThread | Command::FocusOutput => {
                self.focus_region(
                    match command {
                        Command::FocusProjects => Region::Projects,
                        Command::FocusOutput => Region::Output,
                        _ => Region::Thread,
                    },
                    window,
                    cx,
                );
                None
            }
            Command::ToggleProjects => {
                self.toggle_region(Region::Projects, window, cx);
                None
            }
            Command::ToggleOutput => {
                self.toggle_region(Region::Output, window, cx);
                None
            }
        };
        if let Some(kind) = kind {
            self.open_dialog(kind, window, cx);
        }
        cx.notify();
    }
    fn open_dialog(&mut self, kind: DialogKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.extension.is_some()
            || self.state.read(cx).shutting_down
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        self.dismiss(window, cx);
        if self.detail_surface.is_some() || self.dialog.is_some() || self.palette.is_some() {
            return;
        }
        self.dialog = Some(
            self.state
                .update(cx, |state, cx| Dialog::new(kind, state, window, cx)),
        );
        super::settings::open_surface(cx.entity().downgrade(), window, cx);
        cx.notify();
    }
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        if self.extension.is_some() {
            self.close_extension(true, window, cx);
            if self.extension.is_some() {
                return;
            }
        }
        if !self.close_detail_surface(window, cx) {
            return;
        }
        self.image_preview = None;
        self.tool_detail = None;
        if let Some(palette) = self.palette.take() {
            palette.update(cx, |palette, cx| palette.dismiss(window, cx));
        }
        if let Some(dialog) = self.dialog.take() {
            super::settings::close_surface(&dialog.kind, window, cx);
        }
        cx.notify();
    }
    pub fn confirm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.extension.is_some() {
            self.confirm_extension(window, cx);
            return;
        }
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        let result = self.state.update(cx, |state, cx| {
            let result = dialog.apply(state, cx);
            cx.notify();
            result
        });
        match result {
            Ok(true) => {
                super::settings::close_surface(&dialog.kind, window, cx);
            }
            Ok(false) => self.dialog = Some(dialog),
            Err(error) => {
                let mut dialog = dialog;
                dialog.set_error(&error);
                self.dialog = Some(dialog);
            }
        }
        cx.notify();
    }
    pub fn pick_project_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add Project".into()),
        });
        cx.spawn_in(window, async move |workspace, window| {
            let result = picked.await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next()
                            && let Some(dialog) = &workspace.dialog
                            && let Some(field) =
                                dialog.fields.iter().find(|field| field.key == "path")
                        {
                            field.input.update(cx, |input, cx| {
                                input.set_value(path.to_string_lossy().into_owned(), window, cx)
                            });
                        }
                    }
                    Ok(Ok(None)) => {}
                    other => {
                        if let Some(dialog) = &mut workspace.dialog {
                            dialog.error = Some(format!("Folder picker failed: {other:?}"));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub fn select_row(
        &mut self,
        row: Row,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cursor = Some(row);
        self.state.update(cx, |state, cx| {
            match row {
                Row::Project(id) => state.select_project(id),
                Row::Thread(id) if open => state.select_thread(id),
                _ => {}
            }
            cx.notify();
        });
        self.focus_region(Region::Projects, window, cx);
    }
    fn navigate(
        &mut self,
        delta: isize,
        endpoint: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        let region = self.focused_region(window, cx);
        if region == Region::Thread {
            let state = self.state.read(cx);
            if let Some(session) = state
                .selected_thread
                .and_then(|id| state.conversations.get(&id))
            {
                if endpoint {
                    if delta < 0 {
                        session.list.scroll_to_reveal_item(0);
                    } else {
                        session.list.scroll_to_end();
                    }
                } else {
                    session
                        .list
                        .scroll_by(window.rem_size() * (delta as f32 * 2.25));
                }
                cx.notify();
                return;
            }
        }
        if region != Region::Projects {
            let scroll = &self.scrolls[region.index()];
            if endpoint {
                if delta < 0 {
                    scroll.set_offset(point(px(0.), px(0.)));
                } else {
                    scroll.scroll_to_bottom();
                }
            } else {
                let offset = scroll.offset();
                scroll.set_offset(point(
                    offset.x,
                    (offset.y - window.rem_size() * (delta as f32 * 2.25)).min(px(0.)),
                ));
            }
            cx.notify();
            return;
        }
        let rows = sidebar::rows(self.state.read(cx));
        if rows.is_empty() {
            return;
        }
        let index = rows
            .iter()
            .position(|row| Some(*row) == self.cursor)
            .unwrap_or(0);
        let next = if endpoint {
            if delta < 0 { 0 } else { rows.len() - 1 }
        } else {
            (index as isize + delta).clamp(0, rows.len().saturating_sub(1) as isize) as usize
        };
        self.cursor = rows.get(next).copied();
        self.scrolls[0].scroll_to_item(next);
        cx.notify();
    }
    fn fold(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        let id = match self.cursor {
            Some(Row::Project(id)) => Some(id),
            Some(Row::Thread(id)) => self
                .state
                .read(cx)
                .snapshot
                .threads
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.project_id),
            None => self.state.read(cx).selected_project,
        };
        if let Some(id) = id {
            self.state.update(cx, |state, cx| {
                if collapsed {
                    state.collapsed.insert(id);
                } else {
                    state.collapsed.remove(&id);
                }
                cx.notify();
            });
            self.cursor = Some(Row::Project(id));
        }
        cx.notify();
    }
    fn open_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        if self.focused_region(window, cx) != Region::Projects {
            return;
        }
        if let Some(row) = self.cursor {
            self.select_row(row, true, window, cx);
            if matches!(row, Row::Thread(_)) {
                self.focus_region(Region::Thread, window, cx);
            }
        }
    }
    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).shutting_down
            || self.extension.is_some()
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        self.dismiss(window, cx);
        if self.detail_surface.is_some() || self.dialog.is_some() || self.palette.is_some() {
            return;
        }
        self.palette = Some(Palette::open(
            cx.entity().downgrade(),
            self.state.clone(),
            window,
            cx,
        ));
        cx.notify();
    }
    pub fn run_palette_target(
        &mut self,
        target: PaletteTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            PaletteTarget::Command(command) => self.execute(command, window, cx),
            PaletteTarget::Project(id) => self.select_row(Row::Project(id), true, window, cx),
            PaletteTarget::Thread(id) => {
                self.select_row(Row::Thread(id), true, window, cx);
                self.focus_region(Region::Thread, window, cx);
            }
        }
    }
}

/// Native overlay layers are siblings of Workspace, so their callbacks never lease
/// Workspace while its render is running and actions route through their own hosts.
pub struct WorkspaceRoot {
    pub workspace: Entity<Workspace>,
}

impl Render for WorkspaceRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_layers = !self.workspace.read(cx).state.read(cx).shutting_down;
        let sheet = Root::render_sheet_layer(window, cx);
        let dialog = Root::render_dialog_layer(window, cx);
        let notifications = Root::render_notification_layer(window, cx);
        div()
            .relative()
            .size_full()
            .child(self.workspace.clone())
            .when(show_layers, |view| {
                view.children(sheet)
                    .children(dialog)
                    .children(notifications)
            })
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let state = self.state.read(cx);
        let project_name = state
            .project()
            .map(|project| project.name.clone())
            .unwrap_or_else(|| "Drost".into());
        let pending =
            (state.pending_writes > 0).then(|| format!("Saving {} changes", state.pending_writes));
        let palette_owner = owner.clone();
        let recovery_owner = owner.clone();
        let dismiss_recovery = owner.clone();
        let dock_measurement = self.dock_measurement.clone();
        let geometry_owner = owner.clone();
        let close_owner = owner.clone();
        let has_saved_recovery = state
            .snapshot
            .settings
            .get("recovered_composition")
            .is_some_and(|value| !value.is_null());
        let mut root =
            v_flex()
                .id("workspace")
                .key_context("Workspace")
                .track_focus(&self.focus)
                .size_full()
                .relative()
                .bg(cx.theme().background)
                .text_color(cx.theme().foreground)
                .on_action(cx.listener(|this, _: &AddProject, w, cx| {
                    this.execute(Command::AddProject, w, cx)
                }))
                .on_action(cx.listener(|this, _: &NewConversation, w, cx| {
                    this.execute(Command::NewConversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &RenameProject, w, cx| {
                    this.execute(Command::RenameProject, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ArchiveProject, w, cx| {
                    this.execute(Command::ArchiveProject, w, cx)
                }))
                .on_action(cx.listener(|this, _: &RenameThread, w, cx| {
                    this.execute(Command::RenameThread, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ArchiveThread, w, cx| {
                    this.execute(Command::ArchiveThread, w, cx)
                }))
                .on_action(cx.listener(|this, _: &CloseThread, w, cx| {
                    this.execute(Command::CloseThread, w, cx)
                }))
                .on_action(cx.listener(|this, _: &NextThread, w, cx| {
                    this.execute(Command::NextThread, w, cx)
                }))
                .on_action(cx.listener(|this, _: &PreviousThread, w, cx| {
                    this.execute(Command::PreviousThread, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ShowArchived, w, cx| {
                    this.execute(Command::ShowArchived, w, cx)
                }))
                .on_action(cx.listener(|this, _: &NewFactoryThread, w, cx| {
                    this.execute(Command::NewFactoryThread, w, cx)
                }))
                .on_action(cx.listener(|this, _: &RefreshFactoryRuns, w, cx| {
                    this.execute(Command::RefreshFactoryRuns, w, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &Settings, w, cx| this.execute(Command::Settings, w, cx)),
                )
                .on_action(cx.listener(|this, _: &ResetLayout, w, cx| {
                    this.execute(Command::ResetLayout, w, cx)
                }))
                .on_action(cx.listener(|this, _: &RestartEditor, w, cx| {
                    this.execute(Command::RestartEditor, w, cx)
                }))
                .on_action(cx.listener(|this, _: &CancelEditorCommand, w, cx| {
                    this.execute(Command::CancelEditorCommand, w, cx)
                }))
                .on_action(cx.listener(|this, _: &CopyDraftRecovery, w, cx| {
                    this.execute(Command::CopyDraftRecovery, w, cx)
                }))
                .on_action(cx.listener(|this, _: &OpenPalette, w, cx| this.open_palette(w, cx)))
                .on_action(cx.listener(|this, _: &FocusProjects, w, cx| {
                    this.focus_region(Region::Projects, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusThread, w, cx| {
                    this.focus_region(Region::Thread, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusOutput, w, cx| {
                    this.focus_region(Region::Output, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusLeft, w, cx| {
                    this.spatial_focus(PaneDirection::Left, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusRight, w, cx| {
                    this.spatial_focus(PaneDirection::Right, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusUp, w, cx| {
                    this.spatial_focus(PaneDirection::Up, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusDown, w, cx| {
                    this.spatial_focus(PaneDirection::Down, w, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &SelectNext, w, cx| this.navigate(1, false, w, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &SelectPrevious, w, cx| this.navigate(-1, false, w, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &SelectFirst, w, cx| this.navigate(-1, true, w, cx)),
                )
                .on_action(cx.listener(|this, _: &SelectLast, w, cx| this.navigate(1, true, w, cx)))
                .on_action(cx.listener(|this, _: &CollapseProject, _, cx| this.fold(true, cx)))
                .on_action(cx.listener(|this, _: &ExpandProject, _, cx| this.fold(false, cx)))
                .on_action(cx.listener(|this, _: &OpenSelection, w, cx| this.open_selection(w, cx)))
                .on_action(cx.listener(|this, _: &FocusComposer, w, cx| this.focus_composer(w, cx)))
                .on_action(cx.listener(|this, _: &LeaveEditor, w, cx| {
                    this.focus_region(Region::Thread, w, cx)
                }))
                .on_action(cx.listener(|this, _: &Submit, w, cx| {
                    if this.state.read(cx).factory_thread().is_some() {
                        this.start_factory_run(w, cx);
                    } else {
                        this.submit_prompt(w, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &Interrupt, _, cx| {
                    this.state.update(cx, |state, cx| {
                        if let Some(id) = state.selected_thread {
                            state.interrupt(id);
                        }
                        cx.notify();
                    });
                }))
                .on_action(cx.listener(|this, _: &Dismiss, w, cx| this.dismiss(w, cx)))
                .on_action(cx.listener(|this, _: &Confirm, w, cx| this.confirm_dialog(w, cx)))
                .on_action(cx.listener(|this, _: &Quit, w, cx| this.quit_review(w, cx)))
                .on_action(cx.listener(|this, _: &ToggleProjects, w, cx| {
                    this.execute(Command::ToggleProjects, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ToggleOutput, w, cx| {
                    this.execute(Command::ToggleOutput, w, cx)
                }))
                .child(
                    TitleBar::new()
                        .on_close_window(move |_, window, cx| {
                            let _ = close_owner
                                .update(cx, |workspace, cx| workspace.quit_review(window, cx));
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_ellipsis()
                                .child(project_name),
                        )
                        .when_some(pending, |bar, pending| {
                            bar.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(pending),
                            )
                        })
                        .when(
                            state.storage_error.is_some() || state.shutdown_error.is_some(),
                            |bar| bar.child(Tag::danger().child("Unsaved changes")),
                        )
                        .child(
                            Button::new("workspace-palette")
                                .small()
                                .ghost()
                                .cursor_default()
                                .icon(gpui_kit::component::IconName::Search)
                                .label("Search")
                                .tooltip("Search commands, projects and threads")
                                .on_click(move |_, w, cx| {
                                    let _ = palette_owner
                                        .update(cx, |this, cx| this.open_palette(w, cx));
                                }),
                        )
                        .child(
                            Button::new("workspace-menu")
                                .small()
                                .ghost()
                                .cursor_default()
                                .icon(gpui_kit::component::IconName::Ellipsis)
                                .tooltip("Workspace menu")
                                .dropdown_menu(|menu, _, _| {
                                    menu.menu("Add project…", Box::new(AddProject))
                                        .menu("New conversation", Box::new(NewConversation))
                                        .menu("New Factory thread", Box::new(NewFactoryThread))
                                        .separator()
                                        .menu("Toggle projects", Box::new(ToggleProjects))
                                        .menu("Toggle output", Box::new(ToggleOutput))
                                        .menu("Show archived", Box::new(ShowArchived))
                                        .separator()
                                        .menu("Settings…", Box::new(Settings))
                                        .menu("Reset layout", Box::new(ResetLayout))
                                }),
                        ),
                )
                .when_some(state.storage_error.clone(), |view, error| {
                    view.child(div().px_3().py_2().text_color(cx.theme().danger).child(format!(
                        "Storage failure — new work is disabled. In-memory drafts retained. {error}"
                    )))
                })
                .when(
                    has_saved_recovery
                        || state.storage_error.is_some()
                        || state.shutdown_error.is_some(),
                    |view| {
                        view.child(
                            h_flex()
                                .px_3()
                                .py_1()
                                .gap_2()
                                .child("Draft recovery remains separate from submitted text.")
                                .child(
                                    Button::new("copy-draft-recovery")
                                        .small()
                                        .ghost()
                                        .label("Copy Draft Recovery")
                                        .on_click(move |_, _, cx| {
                                            let _ = recovery_owner.update(cx, |workspace, cx| {
                                                workspace.copy_draft_recovery(cx)
                                            });
                                        }),
                                )
                                .when(has_saved_recovery, |view| {
                                    view.child(
                                        Button::new("dismiss-saved-recovery")
                                            .small()
                                            .ghost()
                                            .label("Dismiss Saved Preedit")
                                            .on_click(move |_, _, cx| {
                                                let _ =
                                                    dismiss_recovery.update(cx, |workspace, cx| {
                                                        workspace.state.update(cx, |state, cx| {
                                                            state.set_setting(
                                                                "recovered_composition",
                                                                serde_json::Value::Null,
                                                            );
                                                            cx.notify();
                                                        })
                                                    });
                                            }),
                                    )
                                }),
                        )
                    },
                )
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(self.dock.clone())
                        .child(
                            canvas(
                                move |bounds, window, cx| {
                                    if dock_measurement.replace(Some(bounds)) != Some(bounds) {
                                        let owner = geometry_owner.clone();
                                        window.defer(cx, move |window, cx| {
                                            let _ = owner.update(cx, |workspace, cx| {
                                                workspace.normalize_layout(
                                                    workspace.reopening,
                                                    window,
                                                    cx,
                                                );
                                                workspace.schedule_layout_save(window, cx);
                                            });
                                        });
                                    }
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .inset_0()
                            .size_full(),
                        ),
                );
        if state.shutting_down {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(cx.theme().background)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        v_flex()
                            .gap_2()
                            .child("Saving drafts and closing interactive sessions…")
                            .child("Factory jobs already started continue independently."),
                    ),
            );
        }
        root
    }
}

struct WorkspacePanel {
    region: Region,
    state: Entity<AppState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    group: Option<WeakEntity<TabGroup>>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _subscriptions: Vec<Subscription>,
}
impl WorkspacePanel {
    fn new(
        region: Region,
        state: Entity<AppState>,
        workspace: WeakEntity<Workspace>,
        focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![cx.observe(&state, |_, _, cx| cx.notify())];
        if let Some(owner) = workspace.upgrade() {
            subscriptions.push(cx.observe(&owner, |_, _, cx| cx.notify()));
        }
        Self {
            region,
            state,
            workspace,
            focus,
            group: None,
            bounds: Rc::new(Cell::new(None)),
            _subscriptions: subscriptions,
        }
    }
    fn panel_title(&self, _: &App) -> String {
        match self.region {
            Region::Thread => "Threads".into(),
            Region::Projects | Region::Output => self.region.name().into(),
        }
    }
}
impl BasePanel for WorkspacePanel {
    fn panel_name(&self) -> &'static str {
        self.region.name()
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
        self.bounds.set(None);
    }
    fn on_removed(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.group = None;
        self.bounds.set(None);
    }
}
impl Panel for WorkspacePanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.panel_title(cx)
    }
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        Some(self.panel_title(cx).into())
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let state = self.state.read(cx);
        let owner = self.workspace.clone();
        let thread_id = state.selected_thread;
        match self.region {
            Region::Thread => menu
                .menu_with_disabled(
                    "Rename thread…",
                    Box::new(RenameThread),
                    Command::RenameThread.disabled_reason(state).is_some(),
                )
                .menu_with_disabled(
                    "Archive thread…",
                    Box::new(ArchiveThread),
                    Command::ArchiveThread.disabled_reason(state).is_some(),
                )
                .menu_with_disabled(
                    "Close thread",
                    Box::new(CloseThread),
                    Command::CloseThread.disabled_reason(state).is_some(),
                )
                .item(
                    PopupMenuItem::new("Thread details")
                        .disabled(thread_id.is_none())
                        .on_click(move |_, window, cx| {
                            if let Some(id) = thread_id {
                                let _ = owner.update(cx, |workspace, cx| {
                                    workspace.toggle_thread_details(id, window, cx)
                                });
                            }
                        }),
                )
                .separator()
                .menu("New conversation", Box::new(NewConversation))
                .menu("New Factory thread", Box::new(NewFactoryThread)),
            Region::Projects => menu
                .menu("Add project…", Box::new(AddProject))
                .menu("Show archived", Box::new(ShowArchived)),
            Region::Output => menu.menu("Refresh Factory runs", Box::new(RefreshFactoryRuns)),
        }
    }
}
impl Focusable for WorkspacePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl EventEmitter<PanelEvent> for WorkspacePanel {}
impl Render for WorkspacePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.workspace.upgrade().map(|owner| {
            let workspace = owner.read(cx);
            let state = self.state.read(cx);
            let tabs =
                (self.region == Region::Thread && !state.open_threads.is_empty()).then(|| {
                    TabBar::new("thread-tabs")
                        .track_scroll(&workspace.thread_tab_scroll)
                        .menu(true)
                        .when_some(
                            state
                                .open_threads
                                .iter()
                                .position(|id| Some(*id) == state.selected_thread),
                            |bar, index| bar.selected_index(index),
                        )
                        .children(state.open_threads.iter().filter_map(|id| {
                            let thread = state
                                .snapshot
                                .threads
                                .iter()
                                .find(|thread| thread.id == *id)?;
                            let id = *id;
                            let select_owner = self.workspace.clone();
                            let close_owner = self.workspace.clone();
                            Some(
                                Tab::new()
                                    .label(thread.title.clone())
                                    .aria_label(format!(
                                        "{} · {}",
                                        thread.title,
                                        match thread.kind {
                                            crate::model::ThreadKind::Conversation =>
                                                "Conversation",
                                            crate::model::ThreadKind::Factory => "Factory thread",
                                        }
                                    ))
                                    .disabled(state.shutting_down)
                                    .on_click(move |_, window, cx| {
                                        let _ = select_owner.update(cx, |workspace, cx| {
                                            if workspace.navigation_blocked(window, cx) {
                                                return;
                                            }
                                            workspace.state.update(cx, |state, cx| {
                                                state.select_thread(id);
                                                cx.notify();
                                            });
                                            workspace.focus_region(Region::Thread, window, cx);
                                        });
                                    })
                                    .suffix(
                                        Button::new(SharedString::from(format!(
                                            "close-thread-tab-{id}"
                                        )))
                                        .icon(gpui_kit::component::IconName::Close)
                                        .ghost()
                                        .small()
                                        .tooltip("Close thread")
                                        .disabled(state.shutting_down)
                                        .on_click(
                                            move |_, window, cx| {
                                                cx.stop_propagation();
                                                let _ = close_owner.update(cx, |workspace, cx| {
                                                    if workspace.navigation_blocked(window, cx) {
                                                        return;
                                                    }
                                                    workspace.state.update(cx, |state, cx| {
                                                        state.close_thread(id);
                                                        cx.notify();
                                                    });
                                                    workspace.focus_region(
                                                        Region::Thread,
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            },
                                        ),
                                    ),
                            )
                        }))
                });
            let scroll = &workspace.scrolls[self.region.index()];
            let content = match self.region {
                Region::Projects => {
                    sidebar::render(state, &self.workspace, workspace.cursor, scroll, cx)
                }
                Region::Thread => {
                    if state.factory_thread().is_some() {
                        if state.selected_launch.is_some() {
                            factory::render(state, &self.workspace, scroll, cx)
                        } else if let Some(form) = &workspace.factory_form {
                            super::factory_form::render(
                                form,
                                workspace,
                                &self.workspace,
                                window,
                                cx,
                            )
                        } else {
                            div()
                                .p_3()
                                .child("Loading Factory configuration…")
                                .into_any_element()
                        }
                    } else {
                        conversation::render(state, &self.workspace, scroll, cx)
                    }
                }
                Region::Output => factory::output(state, &self.workspace, scroll, cx),
            };
            v_flex()
                .size_full()
                .min_w_0()
                .min_h_0()
                .children(tabs)
                .child(div().flex_1().min_w_0().min_h_0().child(content))
                .into_any_element()
        });
        let focus = self.focus.clone();
        let bounds = self.bounds.clone();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .id(self.region.name())
            .debug_selector(|| self.region.name().to_owned())
            .key_context(self.region.context())
            .track_focus(&self.focus)
            .bg(if self.region == Region::Projects {
                cx.theme().sidebar
            } else {
                cx.theme().background
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                if !focus.contains_focused(window, cx) {
                    focus.focus(window, cx);
                }
            })
            .children(content)
            .child(
                canvas(
                    move |measured, _, _| bounds.set(Some(measured)),
                    |_, _, _, _| {},
                )
                .absolute()
                // Measure the pane, not the static position after its content.
                .inset_0()
                .size_full(),
            )
    }
}

fn contains_panel(panel: &PanelState, name: &str) -> bool {
    panel.panel_name == name
        || panel
            .children
            .iter()
            .any(|child| contains_panel(child, name))
}
fn validate_layout(layout: &DockAreaState) -> anyhow::Result<()> {
    anyhow::ensure!(layout.version == Some(2), "Unsupported layout version");
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
            PanelInfo::Tiles { .. } => {
                anyhow::bail!("Floating tile layout is not supported by this workspace")
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
    anyhow::ensure!(
        names.len() == 3
            && Region::ALL.into_iter().all(|region| names
                .iter()
                .filter(|name| **name == region.name())
                .count()
                == 1),
        "Layout must contain exactly one Projects, Thread and Output panel"
    );
    Ok(())
}

pub struct Recovery {
    pub error: String,
}
impl Render for Recovery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let copy = self.error.clone();
        v_flex().size_full().bg(cx.theme().background).text_color(cx.theme().foreground)
            .child(TitleBar::new().child("Drost"))
            .child(v_flex().flex_1().p_8().gap_4()
                .child(div().text_xl().child("Drost could not open its workspace"))
                .child("The database has not been replaced or reset. Quit Drost before inspecting or restoring a backup. A newer schema requires a compatible Drost version; a corrupt database requires recovery from a known-good backup.")
                .child(div().font_family(cx.theme().mono_font_family.clone()).text_color(cx.theme().danger).child(self.error.clone()))
                .child(h_flex().gap_2().child(Button::new("copy-recovery-error").label("Copy error").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
                    .child(Button::new("quit-recovery").label("Quit Drost").on_click(|_, _, cx| cx.quit()))))
    }
}
