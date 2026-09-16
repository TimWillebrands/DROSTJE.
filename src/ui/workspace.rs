use super::{
    commands::{self, *},
    conversation, factory,
    settings::{DialogKind, SurfaceContent},
    sidebar::{self, Row},
    title_bar::{WorkspaceDockSkin, WorkspaceTitle},
};
use crate::{
    app::{AppState, NoticeLevel, PresentationChange},
    model::{PaneDirection, ThreadId, ThreadKind},
};
use gpui_kit::{
    component::{
        ActiveTheme, Root, TITLE_BAR_HEIGHT, TitleBar, WindowExt,
        button::Button,
        dock::{
            BasePanel, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSizing,
            DockSkin, InsertTarget, Panel, PanelEvent, PanelId, PanelInfo, PanelState, TabGroup,
            panel_handle, register_panel,
        },
        h_flex,
        input::{InputEvent, InputState},
        menu::{PopupMenu, PopupMenuItem},
        notification::Notification,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Projects,
    Thread,
    Output,
}
impl Region {
    pub fn name(self) -> &'static str {
        commands::region_action(self)
    }
}

/// What one dock panel shows. Projects and Output exist once; every open thread
/// is its own panel so it can be tabbed, split and dragged like a Zed pane; the
/// Welcome panel holds the centre only while no thread is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Projects,
    Welcome,
    Thread(ThreadId),
    Output,
}
impl PanelKind {
    pub fn region(self) -> Region {
        match self {
            Self::Projects => Region::Projects,
            Self::Welcome | Self::Thread(_) => Region::Thread,
            Self::Output => Region::Output,
        }
    }
    /// Persisted layout name; never change it.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Projects => "Projects",
            Self::Welcome => "Welcome",
            Self::Thread(_) => "Thread",
            Self::Output => "Output",
        }
    }
    fn context(self) -> &'static str {
        match self {
            Self::Projects => "List ProjectsList",
            Self::Welcome | Self::Thread(_) => "Thread",
            Self::Output => "List",
        }
    }
    fn debug_name(self) -> String {
        match self {
            Self::Thread(id) => format!("Thread:{id}"),
            other => other.name().to_owned(),
        }
    }
    fn matches(self, state: &PanelState) -> bool {
        state.panel_name == self.name()
            && match self {
                Self::Thread(id) => thread_of(state) == Some(id),
                _ => true,
            }
    }
}
/// The thread a persisted `Thread` leaf names.
fn thread_of(state: &PanelState) -> Option<ThreadId> {
    match &state.info {
        PanelInfo::Panel(value) => serde_json::from_value(value.get("thread")?.clone()).ok(),
        _ => None,
    }
}
type ThreadPanels = Rc<RefCell<BTreeMap<ThreadId, Entity<WorkspacePanel>>>>;
const LAYOUT_VERSION: usize = 3;
const OUTDATED_LAYOUT: &str = "Layout was saved by another version";

pub struct Workspace {
    pub state: Entity<AppState>,
    pub(super) dock: Entity<DockArea>,
    projects: Entity<WorkspacePanel>,
    output: Entity<WorkspacePanel>,
    /// Shared with the panel registry so a restored layout rebuilds into the same map.
    threads: ThreadPanels,
    welcome: Rc<RefCell<Option<Entity<WorkspacePanel>>>>,
    /// While a layout is loaded or reset, panel callbacks must not touch thread state.
    rebuilding: bool,
    focus: FocusHandle,
    pub cursor: Option<Row>,
    /// Filter text for the Projects list; empty shows every visible row.
    thread_search: Entity<InputState>,
    pub palette: Option<Entity<Palette>>,
    pub dialog: Option<Entity<SurfaceContent>>,
    pub(super) editor: Option<Entity<super::editor::PromptEditor>>,
    pub(super) editor_selection: Option<crate::model::DraftKey>,
    pub(super) editor_override: Option<(crate::model::DraftKey, String)>,
    pub image_preview: Option<crate::model::ImageAttachment>,
    pub expanded: std::collections::HashSet<String>,
    pub extension: Option<super::overlays::ExtensionForm>,
    pub tool_detail: Option<(crate::model::ThreadId, String)>,
    pub factory_form: Option<Entity<super::factory_form::FactoryThreadForm>>,
    pub(super) model_picker: Option<conversation::ModelPicker>,
    pub(super) effort_picker: Option<conversation::EffortPicker>,
    pub(super) detail_surface: Option<super::overlays::SurfaceKey>,
    pub(super) quit_requested: bool,
    layout_save: Option<Task<()>>,
    seen_notice_sequence: u64,
    reopening: Option<Region>,
    dock_measurement: Rc<Cell<Option<Bounds<Pixels>>>>,
    presented_thread: Option<ThreadId>,
    pub(super) _subscriptions: Vec<Subscription>,
}
impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let owner = cx.entity().downgrade();
        let projects = cx.new(|cx| {
            WorkspacePanel::new(
                PanelKind::Projects,
                state.clone(),
                owner.clone(),
                window,
                cx,
            )
        });
        let output = cx.new(|cx| {
            WorkspacePanel::new(PanelKind::Output, state.clone(), owner.clone(), window, cx)
        });
        let threads: ThreadPanels = Rc::default();
        let welcome: Rc<RefCell<Option<Entity<WorkspacePanel>>>> = Rc::default();
        for (kind, panel) in [
            (PanelKind::Projects, projects.clone()),
            (PanelKind::Output, output.clone()),
        ] {
            register_panel(cx, kind.name(), move |_, _, _| panel_handle(panel.clone()));
        }
        {
            let (welcome, state, owner) = (welcome.clone(), state.clone(), owner.clone());
            register_panel(cx, PanelKind::Welcome.name(), move |_, window, cx| {
                panel_handle(welcome_panel(&welcome, &state, &owner, window, cx))
            });
        }
        {
            let (threads, state, owner) = (threads.clone(), state.clone(), owner.clone());
            register_panel(
                cx,
                PanelKind::Thread(ThreadId::nil()).name(),
                move |ctx, window, cx| {
                    // Validation guarantees an ID; a nil one is pruned by the next sync.
                    let id = thread_of(ctx.state()).unwrap_or_else(ThreadId::nil);
                    panel_handle(thread_panel(&threads, id, &state, &owner, window, cx))
                },
            );
        }
        let title = cx.new(|cx| WorkspaceTitle::new(state.clone(), owner.clone(), cx));
        let dock = cx.new(|cx| {
            let inner = DockSkin::new(cx);
            DockArea::new("drostje-workspace", Some(LAYOUT_VERSION), window, cx)
                .with_renderer(Rc::new(WorkspaceDockSkin { inner, title }))
        });
        let saved = state.read(cx).snapshot.settings.get("layout").cloned();
        let thread_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search threads…"));
        let mut workspace = Self {
            state,
            dock,
            projects,
            output,
            threads,
            welcome,
            rebuilding: false,
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
            thread_search,
            model_picker: None,
            effort_picker: None,
            presented_thread: None,
            detail_surface: None,
            palette: None,
            dialog: None,
            layout_save: None,
            seen_notice_sequence: 0,
            reopening: None,
            dock_measurement: Rc::new(Cell::new(None)),
            _subscriptions: Vec::new(),
        };
        let restored = saved.map(|saved| {
            let layout: DockAreaState = serde_json::from_value(saved)?;
            validate_layout(&layout)?;
            // Nothing is leased during construction, so no callback can reach the
            // workspace here; `rebuilding` matters for later resets only.
            workspace
                .dock
                .update(cx, |dock, cx| dock.load(layout, window, cx))
        });
        match restored {
            Some(Ok(())) => {}
            // A layout from another version is simply rebuilt; only a corrupt one is reported.
            Some(Err(error)) if error.to_string() == OUTDATED_LAYOUT => {
                workspace.reset_layout(window, cx);
            }
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
        workspace._subscriptions.push(cx.subscribe_in(
            &workspace.thread_search,
            window,
            |workspace, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let query = workspace.thread_search.read(cx).value();
                    let rows = sidebar::rows(workspace.state.read(cx), &query);
                    if !workspace
                        .cursor
                        .is_some_and(|cursor| rows.contains(&cursor))
                    {
                        workspace.cursor = first_thread_row(&rows);
                    }
                    workspace.projects.update(cx, |_, cx| cx.notify());
                }
                InputEvent::PressEnter { .. } => workspace.leave_thread_search(false, window, cx),
                InputEvent::Focus | InputEvent::Blur => {}
            },
        ));
        workspace._subscriptions.push(cx.observe_in(
            &workspace.state,
            window,
            |workspace, _, window, cx| {
                if workspace.state.read(cx).shutdown_error.is_some() {
                    workspace.quit_requested = false;
                }
                workspace.sync_thread_panels(window, cx);
                workspace.sync_session(window, cx);
                workspace.sync_factory_form(window, cx);
                workspace.sync_model_picker(window, cx);
                workspace.sync_effort_picker(window, cx);
                workspace.sync_detail_surface(window, cx);
                workspace.show_notices(window, cx);
                workspace.sync_selection_panels(cx);
                workspace.sync_diff_interest(cx);
                cx.notify();
            },
        ));
        workspace._subscriptions.push(cx.subscribe_in(
            &workspace.state,
            window,
            |workspace, _, event: &PresentationChange, window, cx| {
                match *event {
                    PresentationChange::Selection => {
                        workspace.sync_session(window, cx);
                        workspace.sync_factory_form(window, cx);
                        workspace.sync_model_picker(window, cx);
                        workspace.sync_effort_picker(window, cx);
                        workspace.sync_selection_panels(cx);
                        workspace.sync_diff_interest(cx);
                    }
                    PresentationChange::Thread(id)
                        if workspace.state.read(cx).selected_thread == Some(id) =>
                    {
                        workspace.sync_model_picker(window, cx);
                        workspace.sync_effort_picker(window, cx);
                        workspace.sync_detail_surface(window, cx);
                        workspace.show_notices(window, cx);
                    }
                    _ => return,
                }
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
                    workspace.sync_diff_interest(cx);
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
        workspace.sync_thread_panels(window, cx);
        workspace.normalize_layout(None, window, cx);
        workspace.sync_factory_form(window, cx);
        workspace.sync_model_picker(window, cx);
        workspace.sync_effort_picker(window, cx);
        workspace.sync_selection_panels(cx);
        workspace.sync_diff_interest(cx);
        cx.defer_in(window, |workspace, window, cx| {
            workspace.show_notices(window, cx)
        });
        let projects_focus = workspace.projects.read(cx).focus.clone();
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
            cx.emit(PresentationChange::Selection);
        });
        self.focus_region(Region::Thread, window, cx);
    }

    pub fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reopening = None;
        let open = self.open_thread_ids(cx);
        let selected = self.state.read(cx).selected_thread;
        let owner = cx.entity().downgrade();
        let mut center = DockLayout::tabs();
        if open.is_empty() {
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
        let left = DockLayout::tabs().panel_view(panel_handle(self.projects.clone()), cx);
        let bottom = DockLayout::tabs().panel_view(panel_handle(self.output.clone()), cx);
        // Dock APIs consume pixels; application geometry is resolved from rem here.
        let rem = window.rem_size();
        self.rebuilding = true;
        self.dock.update(cx, |dock, cx| {
            dock.set_version(Some(LAYOUT_VERSION), cx);
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
        self.rebuilding = false;
        self.sync_thread_panels(window, cx);
        self.invalidate_panel_bounds(cx);
        self.normalize_layout(None, window, cx);
        self.flush_layout(cx);
        cx.notify();
    }

    fn open_thread_ids(&self, cx: &App) -> Vec<ThreadId> {
        let state = self.state.read(cx);
        state
            .open_threads
            .iter()
            .copied()
            .filter(|id| state.snapshot.threads.iter().any(|thread| thread.id == *id))
            .collect()
    }
    fn all_panels(&self) -> Vec<Entity<WorkspacePanel>> {
        let mut panels = vec![self.projects.clone(), self.output.clone()];
        panels.extend(self.welcome.borrow().iter().cloned());
        panels.extend(self.threads.borrow().values().cloned());
        panels
    }
    fn thread_target(&self, cx: &App) -> Option<Entity<WorkspacePanel>> {
        self.state
            .read(cx)
            .selected_thread
            .and_then(|id| self.threads.borrow().get(&id).cloned())
            .or_else(|| self.welcome.borrow().clone())
    }

    /// One panel per open thread, beside the selected thread's group; the Welcome
    /// panel only while nothing is open. Dock edits that close a thread come back
    /// through `thread_panel_removed`, so this is the single owner of the map.
    pub(super) fn sync_thread_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rebuilding || self.state.read(cx).shutting_down {
            return;
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
        if open.is_empty() {
            if self.welcome.borrow().is_none() {
                let welcome = welcome_panel(&self.welcome, &self.state, &owner, window, cx);
                self.dock.update(cx, |dock, cx| {
                    dock.add_panel(welcome, DockPlacement::Center, None, window, cx)
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
            .filter(|panel| matches!(panel.read(cx).kind, PanelKind::Thread(_)))
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
                dock.add_panel(panel, DockPlacement::Center, None, window, cx);
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
        if lost_focus {
            // The closed pane held focus; keyboard control continues in the next thread.
            if let Some(target) = self.thread_target(cx) {
                self.focus_panel(target, window, cx);
            }
        }
        cx.notify();
    }
    fn sync_selection_panels(&mut self, cx: &mut Context<Self>) {
        let selected = self.state.read(cx).selected_thread;
        if selected != self.presented_thread {
            for id in self.presented_thread.into_iter().chain(selected) {
                if let Some(panel) = self.threads.borrow().get(&id) {
                    panel.update(cx, |_, cx| cx.notify());
                }
            }
            self.presented_thread = selected;
            self.projects.update(cx, |_, cx| cx.notify());
        }
    }

    fn sync_diff_interest(&mut self, cx: &mut Context<Self>) {
        let dock = self.dock.read(cx);
        let layout = dock.dump(cx);
        let visible: Vec<_> = self
            .threads
            .borrow()
            .iter()
            .filter(|(_, panel)| panel_visible(panel, dock, &layout, cx))
            .map(|(id, _)| *id)
            .collect();
        self.state
            .update(cx, |state, cx| state.set_visible_conversations(visible, cx));
    }

    /// A pane that gained focus (click, tab, Ctrl-W) is the selected thread.
    fn thread_focused(&mut self, id: ThreadId, window: &mut Window, cx: &mut Context<Self>) {
        if self.rebuilding || self.state.read(cx).selected_thread == Some(id) {
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
            state.select_thread(id);
            cx.emit(PresentationChange::Selection);
        });
        self.sync_session(window, cx);
        cx.notify();
    }
    /// The dock dropped a thread panel (tab close button or a closed split).
    /// Called from inside the dock's own update, so focus moves on the next tick.
    /// A thread panel the dock closed on its own (tab close button, dock edit).
    /// Panels the workspace removed itself are already gone from the map, so
    /// only a panel that is still the registered one for `id` closes the thread.
    fn thread_panel_removed(
        &mut self,
        id: ThreadId,
        panel: &Entity<WorkspacePanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rebuilding {
            return;
        }
        let registered = self.threads.borrow().get(&id).is_some_and(|p| p == panel);
        if !registered {
            return;
        }
        self.threads.borrow_mut().remove(&id);
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
        for panel in self.all_panels() {
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
            // First layout has no dock measurement; the dock fills the window.
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
        // The title bar heads the centre column, above the bottom dock.
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
    /// Fold or unfold one of the conversation's sticky docks (task list, background jobs).
    fn toggle_dock(&mut self, dock: &str, cx: &mut Context<Self>) {
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
    pub fn focus_region(&mut self, region: Region, window: &mut Window, cx: &mut Context<Self>) {
        // A thread selected in this same update has no panel until the sync runs.
        if region == Region::Thread {
            self.sync_thread_panels(window, cx);
        }
        let target = match region {
            Region::Projects => Some(self.projects.clone()),
            Region::Output => Some(self.output.clone()),
            Region::Thread => self.thread_target(cx),
        };
        if let Some(panel) = target {
            self.focus_panel(panel, window, cx);
        }
    }
    /// Reveal a panel (open its dock, zoom out, select its tab) and focus it.
    fn focus_panel(
        &mut self,
        panel: Entity<WorkspacePanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        let kind = panel.read(cx).kind;
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
        let Some(current) = self.focused_panel(window, cx) else {
            return;
        };
        let dock = self.dock.read(cx);
        let layout = dock.dump(cx);
        let visible_bounds = |entity: &Entity<WorkspacePanel>| {
            if !panel_visible(entity, dock, &layout, cx) {
                return None;
            }
            let panel = entity.read(cx);
            let bounds = panel.bounds.get()?;
            (bounds.size.width > px(0.)
                && bounds.size.height > px(0.)
                && dock.bounds().contains(&bounds.center()))
            .then_some(bounds)
        };
        let Some(origin) = visible_bounds(&current).map(|bounds| bounds.center()) else {
            return;
        };
        let next = self
            .all_panels()
            .into_iter()
            .filter(|panel| *panel != current)
            .filter_map(|panel| {
                let center = visible_bounds(&panel)?.center();
                let dx = f32::from(center.x - origin.x);
                let dy = f32::from(center.y - origin.y);
                let (forward, cross) = match direction {
                    PaneDirection::Left => (-dx, dy),
                    PaneDirection::Right => (dx, dy),
                    PaneDirection::Up => (-dy, dx),
                    PaneDirection::Down => (dy, dx),
                };
                (forward > 1.).then_some((panel, forward + cross.abs() * 2.))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(panel, _)| panel);
        if let Some(next) = next {
            self.focus_panel(next, window, cx);
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
                | Command::NewOmpConversation
                | Command::NewClaudeCodeConversation
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
            Command::PasteImage => {
                self.paste_image(window, cx);
                None
            }
            Command::ToggleConversationDiffs => {
                self.state.update(cx, |state, cx| {
                    if let Some(id) = state.conversation_thread().map(|thread| thread.id) {
                        let open = !state.conversation_diff_open(id);
                        state.set_conversation_diff_open(id, open);
                        cx.emit(PresentationChange::Thread(id));
                    }
                });
                self.sync_diff_interest(cx);
                None
            }
            Command::ToggleTaskList => {
                self.toggle_dock("dock-todo", cx);
                None
            }
            Command::ToggleBackgroundJobs => {
                self.toggle_dock("dock-jobs", cx);
                None
            }
            Command::RefreshConversationDiff => {
                self.state.update(cx, |state, cx| {
                    if let Some(id) = state.conversation_thread().map(|thread| thread.id) {
                        state.refresh_conversation_diff(id, cx);
                    }
                });
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
            Command::NewConversation
            | Command::NewOmpConversation
            | Command::NewClaudeCodeConversation
            | Command::NewFactoryThread => {
                self.state.update(cx, |state, cx| {
                    let result = if command == Command::NewConversation {
                        state.new_thread(ThreadKind::Conversation)
                    } else if command == Command::NewOmpConversation {
                        state.new_conversation("omp")
                    } else if command == Command::NewClaudeCodeConversation {
                        state.new_conversation("claude-code")
                    } else {
                        state.new_thread(ThreadKind::Factory)
                    };
                    if let Err(error) = result {
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
            Command::SearchThreads => {
                self.focus_region(Region::Projects, window, cx);
                if self.focused_region(window, cx) == Region::Projects {
                    self.thread_search
                        .update(cx, |input, cx| input.focus(window, cx));
                }
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
        let state = self.state.clone();
        let owner = cx.entity().downgrade();
        self.dialog = Some(cx.new(|cx| SurfaceContent::new(kind, state, owner, window, cx)));
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
            let kind = dialog.read(cx).kind.clone();
            super::settings::close_surface(&kind, window, cx);
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
            let result = dialog.read(cx).apply(state, cx);
            if result.is_ok() {
                cx.notify();
            }
            result
        });
        match result {
            Ok(true) => {
                let kind = dialog.read(cx).kind.clone();
                super::settings::close_surface(&kind, window, cx);
                cx.notify();
            }
            Ok(false) => self.dialog = Some(dialog),
            Err(error) => {
                dialog.update(cx, |dialog, cx| dialog.set_error(&error, cx));
                self.dialog = Some(dialog);
            }
        }
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
            cx.emit(PresentationChange::Selection);
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
        if region == Region::Thread
            && let Some(transcript) = self
                .focused_panel(window, cx)
                .and_then(|panel| panel.read(cx).transcript.clone())
        {
            transcript.update(cx, |transcript, cx| {
                transcript.scroll(delta, endpoint, window, cx)
            });
            return;
        }
        if region != Region::Projects {
            let Some(panel) = self.focused_panel(window, cx) else {
                return;
            };
            let scroll = panel.read(cx).scroll.clone();
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
            panel.update(cx, |_, cx| cx.notify());
            return;
        }
        let query = self.thread_search.read(cx).value();
        let rows = sidebar::rows(self.state.read(cx), &query);
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
        self.projects.read(cx).scroll.scroll_to_item(next);
        self.projects.update(cx, |_, cx| cx.notify());
    }
    /// Return focus from the thread filter to the Projects list, optionally clearing
    /// the filter, with the cursor on the first matching thread.
    pub fn leave_thread_search(
        &mut self,
        clear: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if clear {
            self.thread_search
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        let query = self.thread_search.read(cx).value();
        let rows = sidebar::rows(self.state.read(cx), &query);
        if !clear || !self.cursor.is_some_and(|cursor| rows.contains(&cursor)) {
            self.cursor = first_thread_row(&rows);
        }
        if let Some(index) = rows.iter().position(|row| Some(*row) == self.cursor) {
            self.projects.read(cx).scroll.scroll_to_item(index);
        }
        let focus = self.projects.read(cx).focus.clone();
        focus.focus(window, cx);
        self.projects.update(cx, |_, cx| cx.notify());
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
                cx.emit(PresentationChange::Projects);
            });
            self.cursor = Some(Row::Project(id));
        }
        self.projects.update(cx, |_, cx| cx.notify());
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
        let dock_measurement = self.dock_measurement.clone();
        let geometry_owner = owner.clone();
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
                .on_action(cx.listener(|this, _: &NewOmpConversation, w, cx| {
                    this.execute(Command::NewOmpConversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &NewClaudeCodeConversation, w, cx| {
                    this.execute(Command::NewClaudeCodeConversation, w, cx)
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
                .on_action(cx.listener(|this, _: &SearchThreads, w, cx| {
                    this.execute(Command::SearchThreads, w, cx)
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
                .on_action(cx.listener(|this, _: &PasteImage, w, cx| {
                    this.execute(Command::PasteImage, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ToggleConversationDiffs, w, cx| {
                    this.execute(Command::ToggleConversationDiffs, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ToggleTaskList, w, cx| {
                    this.execute(Command::ToggleTaskList, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ToggleBackgroundJobs, w, cx| {
                    this.execute(Command::ToggleBackgroundJobs, w, cx)
                }))
                .on_action(cx.listener(|this, _: &RefreshConversationDiff, w, cx| {
                    this.execute(Command::RefreshConversationDiff, w, cx)
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
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(
                            self.dock
                                .clone()
                                .cached(StyleRefinement::default().size_full()),
                        )
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

/// Get or build the panel for a thread; the registry and the workspace share the map.
fn thread_panel(
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
fn welcome_panel(
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

pub struct WorkspacePanel {
    kind: PanelKind,
    state: Entity<AppState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    group: Option<WeakEntity<TabGroup>>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    transcript: Option<Entity<conversation::TranscriptView>>,
    _subscriptions: Vec<Subscription>,
}
impl WorkspacePanel {
    fn new(
        kind: PanelKind,
        state: Entity<AppState>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let mut subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.observe_global::<gpui_kit::component::Theme>(|_, cx| cx.notify()),
            cx.subscribe(&state, |panel, _, event: &PresentationChange, cx| {
                let affected = match (*event, panel.kind) {
                    (
                        PresentationChange::Draft(id) | PresentationChange::Thread(id),
                        PanelKind::Thread(thread),
                    ) => id == thread,
                    (PresentationChange::Diff(project), PanelKind::Thread(thread)) => panel
                        .state
                        .read(cx)
                        .snapshot
                        .threads
                        .iter()
                        .any(|candidate| candidate.id == thread && candidate.project_id == project),
                    (
                        PresentationChange::Projects | PresentationChange::Selection,
                        PanelKind::Projects,
                    ) => true,
                    _ => false,
                };
                if affected {
                    cx.notify();
                }
            }),
        ];
        let transcript =
            match kind {
                PanelKind::Thread(id)
                    if state.read(cx).snapshot.threads.iter().any(|thread| {
                        thread.id == id && thread.kind == ThreadKind::Conversation
                    }) =>
                {
                    Some(cx.new(|cx| {
                        conversation::TranscriptView::new(id, state.clone(), workspace.clone(), cx)
                    }))
                }
                _ => None,
            };
        if let PanelKind::Thread(id) = kind {
            // Focus, however it arrived (click, tab, Ctrl-W), is what selects a thread.
            let owner = workspace.clone();
            subscriptions.push(cx.on_focus_in(&focus, window, move |_, window, cx| {
                let _ = owner.update(cx, |workspace, cx| workspace.thread_focused(id, window, cx));
            }));
        }
        Self {
            kind,
            state,
            workspace,
            focus,
            scroll: ScrollHandle::new(),
            group: None,
            bounds: Rc::new(Cell::new(None)),
            transcript,
            _subscriptions: subscriptions,
        }
    }
    fn panel_title(&self, cx: &App) -> String {
        match self.kind {
            PanelKind::Projects => "Projects".into(),
            PanelKind::Output => "Output".into(),
            PanelKind::Welcome => "Threads".into(),
            PanelKind::Thread(id) => self
                .state
                .read(cx)
                .snapshot
                .threads
                .iter()
                .find(|thread| thread.id == id)
                .map(|thread| thread.title.clone())
                .unwrap_or_else(|| "Thread".into()),
        }
    }
}
impl BasePanel for WorkspacePanel {
    fn panel_name(&self) -> &'static str {
        self.kind.name()
    }
    fn closable(&self, _: &App) -> bool {
        matches!(self.kind, PanelKind::Thread(_))
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
        self.bounds.set(None);
    }
    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.group = None;
        self.bounds.set(None);
        if let PanelKind::Thread(id) = self.kind {
            // The dock often fires this from inside a `Workspace` update
            // (`sync_thread_panels`, `reset_layout`), and updating an entity that
            // is already leased panics. Report after that update has finished;
            // a panel the dock has since re-added is not a close.
            cx.defer_in(window, move |panel, window, cx| {
                if panel.group.is_some() {
                    return;
                }
                let removed = cx.entity();
                let _ = panel.workspace.update(cx, |workspace, cx| {
                    workspace.thread_panel_removed(id, &removed, window, cx)
                });
            });
        }
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut state = PanelState::new(self.kind.name());
        if let PanelKind::Thread(id) = self.kind {
            state.info = PanelInfo::panel(json!({ "thread": id }));
        }
        state
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
        match self.kind {
            PanelKind::Thread(id) => {
                // The menu belongs to this pane: select its thread before the command runs.
                let owned = |command: Command| {
                    let owner = self.workspace.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.state.update(cx, |state, cx| {
                                state.select_thread(id);
                                cx.notify();
                            });
                            workspace.execute(command, window, cx);
                        });
                    }
                };
                let disabled = |command: Command| {
                    state.selected_thread == Some(id) && command.disabled_reason(state).is_some()
                };
                let details = self.workspace.clone();
                menu.item(
                    PopupMenuItem::new("Rename thread…")
                        .disabled(disabled(Command::RenameThread))
                        .on_click(owned(Command::RenameThread)),
                )
                .item(
                    PopupMenuItem::new("Archive thread…")
                        .disabled(disabled(Command::ArchiveThread))
                        .on_click(owned(Command::ArchiveThread)),
                )
                .item(
                    PopupMenuItem::new("Close thread")
                        .disabled(disabled(Command::CloseThread))
                        .on_click(owned(Command::CloseThread)),
                )
                .item(
                    PopupMenuItem::new("Thread details").on_click(move |_, window, cx| {
                        let _ = details.update(cx, |workspace, cx| {
                            workspace.toggle_thread_details(id, window, cx)
                        });
                    }),
                )
                .separator()
                .menu("New conversation", Box::new(NewConversation))
                .menu("New Factory thread", Box::new(NewFactoryThread))
            }
            PanelKind::Welcome => menu
                .menu("New conversation", Box::new(NewConversation))
                .menu("New Factory thread", Box::new(NewFactoryThread)),
            PanelKind::Projects => menu
                .menu("Add project…", Box::new(AddProject))
                .menu("Show archived", Box::new(ShowArchived)),
            PanelKind::Output => menu.menu("Refresh Factory runs", Box::new(RefreshFactoryRuns)),
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.workspace.upgrade().map(|owner| {
            let workspace = owner.read(cx);
            let state = self.state.read(cx);
            let scroll = &self.scroll;
            match self.kind {
                PanelKind::Projects => sidebar::render(
                    state,
                    &self.workspace,
                    workspace.cursor,
                    &workspace.thread_search,
                    scroll,
                    cx,
                ),
                PanelKind::Welcome => conversation::welcome(state, &self.workspace, cx),
                PanelKind::Output => factory::output(state, &self.workspace, scroll, cx),
                PanelKind::Thread(id) => {
                    let Some(thread) = state.snapshot.threads.iter().find(|t| t.id == id) else {
                        return div()
                            .p_3()
                            .text_color(cx.theme().muted_foreground)
                            .child("This thread is no longer available.")
                            .into_any_element();
                    };
                    match thread.kind {
                        ThreadKind::Conversation => {
                            if let Some(transcript) = &self.transcript {
                                conversation::render(
                                    state,
                                    id,
                                    &self.workspace,
                                    scroll,
                                    transcript,
                                    cx,
                                )
                            } else {
                                div().into_any_element()
                            }
                        }
                        ThreadKind::Factory => {
                            if factory::launch_for(state, id).is_some() {
                                factory::render(state, id, &self.workspace, scroll, cx)
                            } else if let Some(form) = workspace
                                .factory_form
                                .as_ref()
                                .filter(|form| form.read(cx).thread_id == id)
                            {
                                form.clone()
                                    .cached(StyleRefinement::default().size_full())
                                    .into_any_element()
                            } else {
                                div()
                                    .p_3()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if state.selected_thread == Some(id) {
                                        "Loading Factory configuration…"
                                    } else {
                                        "Focus this pane to configure the Factory thread."
                                    })
                                    .into_any_element()
                            }
                        }
                    }
                }
            }
        });
        let focus = self.focus.clone();
        let bounds = self.bounds.clone();
        let name = self.kind.debug_name();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .id(SharedString::from(name.clone()))
            .debug_selector(move || name.clone())
            .test_support()
            .key_context(self.kind.context())
            .track_focus(&self.focus)
            .bg(if self.kind == PanelKind::Projects {
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

fn panel_visible(
    entity: &Entity<WorkspacePanel>,
    dock: &DockArea,
    layout: &DockAreaState,
    cx: &App,
) -> bool {
    let panel = entity.read(cx);
    if [&layout.left_dock, &layout.right_dock, &layout.bottom_dock]
        .into_iter()
        .flatten()
        .any(|edge| !edge.open() && contains_kind(edge.panel(), panel.kind))
    {
        return false;
    }
    let Some(group) = panel.group.as_ref().and_then(WeakEntity::upgrade) else {
        return false;
    };
    let group = group.read(cx);
    !group.is_collapsed()
        && dock.zoomed_group().is_none_or(|node| node == group.node())
        && group
            .active_panel(cx)
            .is_some_and(|active| active.panel_id(cx) == PanelId::from(entity.entity_id()))
}

fn contains_panel(panel: &PanelState, name: &str) -> bool {
    panel.panel_name == name
        || panel
            .children
            .iter()
            .any(|child| contains_panel(child, name))
}
fn contains_kind(panel: &PanelState, kind: PanelKind) -> bool {
    kind.matches(panel)
        || panel
            .children
            .iter()
            .any(|child| contains_kind(child, kind))
}
fn validate_layout(layout: &DockAreaState) -> anyhow::Result<()> {
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
    let count = |name: &str| names.iter().filter(|candidate| **candidate == name).count();
    anyhow::ensure!(
        count("Projects") == 1 && count("Output") == 1 && count("Welcome") <= 1,
        "Layout must contain exactly one Projects and one Output panel"
    );
    anyhow::ensure!(
        names
            .iter()
            .all(|name| matches!(*name, "Projects" | "Output" | "Welcome" | "Thread")),
        "Layout contains an unknown panel"
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
            .child(TitleBar::new().child("DROSTJE."))
            .child(v_flex().flex_1().p_8().gap_4()
                .child(div().text_xl().child("DROSTJE. could not open its workspace"))
                .child("The database has not been replaced or reset. Quit DROSTJE. before inspecting or restoring a backup. A newer schema requires a compatible DROSTJE. version; a corrupt database requires recovery from a known-good backup.")
                .child(div().font_family(cx.theme().mono_font_family.clone()).text_color(cx.theme().danger).child(self.error.clone()))
                .child(h_flex().gap_2().child(Button::new("copy-recovery-error").label("Copy error").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
                    .child(Button::new("quit-recovery").label("Quit DROSTJE.").on_click(|_, _, cx| cx.quit()))))
    }
}

/// Where the keyboard cursor lands after filtering: the first thread, else the first row.
fn first_thread_row(rows: &[Row]) -> Option<Row> {
    rows.iter()
        .copied()
        .find(|row| matches!(row, Row::Thread(_)))
        .or_else(|| rows.first().copied())
}
