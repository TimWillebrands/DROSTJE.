#[path = "workspace_browser.rs"]
mod browser_workspace;
use super::browser::{BrowserPane, OpenBrowserUrl};
use super::{
    activity,
    commands::{self, *},
    conversation, factory,
    settings::{DialogKind, SurfaceContent, SurfaceOutcome},
    sidebar::{self, Row},
    terminal::{TerminalId, TerminalPane},
    thread_sidebar,
    title_bar::{WorkspaceDockSkin, WorkspaceTitle},
};
use crate::{
    app::{AppState, NoticeLevel, PresentationChange, SidebarTab, ThreadActivity},
    model::{PaneDirection, ProjectId, SpaceId, ThreadId, ThreadKind, WorktreeId},
};
use browser_workspace::{Browsers, browser_of};
use gpui_kit::{
    component::{
        ActiveTheme, IconName, Root, Sizable, TITLE_BAR_HEIGHT, TitleBar, WindowExt,
        button::{Button, ButtonVariants},
        dialog::DialogButtonProps,
        dock::{
            BasePanel, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSizing,
            DockSkin, DockState, InsertTarget, Panel, PanelEvent, PanelId, PanelInfo, PanelState,
            PanelStyle, TabGroup, panel_handle, register_panel,
        },
        h_flex,
        input::{InputEvent, InputState},
        menu::{PopupMenu, PopupMenuItem},
        notification::Notification,
        shimmer::ShimmerText,
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
    time::{Duration, Instant},
};

/// A list or scroll motion, shared by the Projects list, thread panels and the transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Motion {
    Line(isize),
    Page(isize),
    First,
    Last,
}

/// Share of the viewport one page motion covers, so a line of context survives the jump.
pub(super) const PAGE_SHARE: f32 = 0.9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Projects,
    Thread,
}
impl Region {
    pub fn name(self) -> &'static str {
        commands::region_action(self)
    }
}
/// What one dock panel shows. Threads and terminals can be tabbed,
/// split and dragged. Welcome holds the center only while no content is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Projects,
    Welcome,
    Thread(ThreadId),
    Terminal(TerminalId),
    Browser(crate::browser::BrowserId),
}
impl PanelKind {
    pub fn region(self) -> Region {
        match self {
            Self::Projects => Region::Projects,
            Self::Welcome | Self::Thread(_) | Self::Terminal(_) | Self::Browser(_) => {
                Region::Thread
            }
        }
    }
    /// Persisted layout name; never change it.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Projects => "Projects",
            Self::Welcome => "Welcome",
            Self::Thread(_) => "Thread",
            Self::Terminal(_) => "Terminal",
            Self::Browser(_) => "Browser",
        }
    }
    fn context(self) -> &'static str {
        match self {
            Self::Projects => "List ProjectsList",
            Self::Welcome | Self::Thread(_) => "Thread",
            Self::Terminal(_) => "TerminalPane",
            Self::Browser(_) => "BrowserPane",
        }
    }
    fn debug_name(self) -> String {
        match self {
            Self::Thread(id) => format!("Thread:{id}"),
            Self::Terminal(id) => format!("Terminal:{id}"),
            Self::Browser(id) => format!("Browser:{id}"),
            other => other.name().to_owned(),
        }
    }
    fn matches(self, state: &PanelState) -> bool {
        state.panel_name == self.name()
            && match self {
                Self::Thread(id) => thread_of(state) == Some(id),
                Self::Terminal(id) => terminal_of(state) == Some(id),
                Self::Browser(id) => browser_of(state) == Some(id),
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
fn terminal_of(state: &PanelState) -> Option<TerminalId> {
    match &state.info {
        PanelInfo::Panel(value) => serde_json::from_value(value.get("terminal")?.clone()).ok(),
        _ => None,
    }
}
/// What a saved `Terminal` leaf reopens with: a new shell where the tab worked.
/// Neither the old process nor its scrollback is ever restored.
#[derive(serde::Deserialize)]
struct TerminalTab {
    terminal: TerminalId,
    project: ProjectId,
    #[serde(default)]
    worktree: Option<WorktreeId>,
}
fn terminal_tab(state: &PanelState) -> Option<TerminalTab> {
    match &state.info {
        PanelInfo::Panel(value) if state.panel_name == "Terminal" => {
            serde_json::from_value(value.clone()).ok()
        }
        _ => None,
    }
}
type TerminalPanels = Rc<RefCell<BTreeMap<TerminalId, Entity<WorkspacePanel>>>>;
type ThreadPanels = Rc<RefCell<BTreeMap<ThreadId, Entity<WorkspacePanel>>>>;
/// Version 4 dropped the bottom Output dock; Output is a thread sidebar tab.
const LAYOUT_VERSION: usize = 4;
const OUTDATED_LAYOUT: &str = "Layout was saved by another version";

#[derive(Clone, Copy)]
enum SpaceChange {
    Switch(SpaceId),
    Move(ProjectId, SpaceId),
    MoveThread(ThreadId, SpaceId),
    Delete(SpaceId),
}

/// Presentation only: never changes a Space or its retained editors/layout.
#[derive(Default)]
struct SpaceSlide {
    offset: f32,
    started: Option<Instant>,
    neighbor: Option<SpaceId>,
    neighbor_query: SharedString,
    neighbor_scroll: Option<ScrollHandle>,
}

impl SpaceSlide {
    const DURATION: Duration = Duration::from_millis(160);

    fn drag(&mut self, progress: f32) {
        self.offset = progress;
        self.started = None;
    }

    fn settle(&mut self, now: Instant) {
        if self.started.is_none() && self.offset != 0. {
            self.started = Some(now);
        }
    }

    fn sample(&self, now: Instant) -> (f32, bool) {
        let Some(started) = self.started else {
            return (self.offset, false);
        };
        let elapsed =
            now.saturating_duration_since(started).as_secs_f32() / Self::DURATION.as_secs_f32();
        if elapsed >= 1. {
            return (0., false);
        }
        (self.offset * (1. - elapsed).powi(3), true)
    }
}

/// Projects is one retained sidebar. Space layouts own threads, not its geometry.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct ProjectsSidebar {
    size: Pixels,
    open: bool,
}

impl ProjectsSidebar {
    fn capture(layout: &DockAreaState) -> Option<Self> {
        [&layout.left_dock, &layout.right_dock, &layout.bottom_dock]
            .into_iter()
            .flatten()
            .find(|dock| contains_panel(dock.panel(), "Projects"))
            .map(|dock| Self {
                size: dock.size(),
                open: dock.open(),
            })
    }

    fn apply(self, layout: &mut DockAreaState) {
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

pub struct Workspace {
    pub state: Entity<AppState>,
    pub(super) dock: Entity<DockArea>,
    projects: Entity<WorkspacePanel>,
    /// Shared with the panel registry so a restored layout rebuilds into the same map.
    threads: ThreadPanels,
    terminals: TerminalPanels,
    pub(super) browsers: Browsers,
    pub(super) sidebar_sizes:
        Rc<RefCell<BTreeMap<ThreadId, Entity<gpui_kit::component::resizable::ResizableState>>>>,
    /// Live layouts reattach retained terminals; durable layouts reopen them with a new shell.
    live_layouts: BTreeMap<SpaceId, DockAreaState>,
    active_terminal: Option<TerminalId>,
    space_terminals: BTreeMap<SpaceId, TerminalId>,
    terminal_confirmation: bool,
    welcome: Rc<RefCell<Option<Entity<WorkspacePanel>>>>,
    /// While a layout is loaded or reset, panel callbacks must not touch thread state.
    rebuilding: bool,
    focus: FocusHandle,
    pub cursor: Option<Row>,
    hovered_sidebar_row: Option<Row>,
    /// Gesture progress and settling are presentation-only; switching still saves drafts first.
    space_swipe: super::sidebar::SpaceSwipe,
    space_slide: SpaceSlide,
    space_swipe_idle: Option<Task<()>>,
    space_direction: Option<f32>,
    /// Filter text for the Projects list; empty shows every visible row.
    thread_search: Entity<InputState>,
    pub palette: Option<Entity<Palette>>,
    pub dialog: Option<Entity<SurfaceContent>>,
    pub(super) theme_editor: Option<Entity<super::theme_editor::ThemeEditor>>,
    /// When to open each Conversation's Agents tab.
    agent_activity: BTreeMap<ThreadId, super::agents::Follow>,
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
    pub(super) permission_mode_picker: Option<conversation::PermissionModePicker>,
    pub(super) detail_surface: Option<super::overlays::SurfaceKey>,
    pub(super) quit_requested: bool,
    layout_save: Option<Task<()>>,
    presented_space: SpaceId,
    preparing_space_change: bool,
    seen_notice_sequence: u64,
    reopening: Option<Region>,
    dock_measurement: Rc<Cell<Option<Bounds<Pixels>>>>,
    presented_thread: Option<ThreadId>,
    /// A selected thread whose tab has not been displayed yet, because its
    /// panel is added by the next `sync_thread_panels`.
    unrevealed_thread: Option<ThreadId>,
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
        let threads: ThreadPanels = Rc::default();
        let terminals: TerminalPanels = Rc::default();
        let browsers = Browsers::new(&state, &owner, window, cx);
        let welcome: Rc<RefCell<Option<Entity<WorkspacePanel>>>> = Rc::default();
        {
            let panel = projects.clone();
            register_panel(cx, PanelKind::Projects.name(), move |_, _, _| {
                panel_handle(panel.clone())
            });
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
        {
            let (terminals, welcome, state, owner) = (
                terminals.clone(),
                welcome.clone(),
                state.clone(),
                owner.clone(),
            );
            register_panel(cx, "Terminal", move |ctx, window, cx| {
                // A live ID reattaches its shell; a saved tab reopens a new one.
                // Layouts are filtered by `restorable_layout` first, so Welcome is a backstop.
                let panel = terminal_of(ctx.state())
                    .and_then(|id| terminals.borrow().get(&id).cloned())
                    .or_else(|| {
                        reopen_terminal(ctx.state(), &terminals, &state, &owner, window, cx)
                    })
                    .unwrap_or_else(|| welcome_panel(&welcome, &state, &owner, window, cx));
                panel_handle(panel)
            });
        }
        let title = cx.new(|cx| WorkspaceTitle::new(state.clone(), owner.clone(), cx));
        let mut inner = None;
        let dock = cx.new(|cx| {
            let skin = DockSkin::new(cx);
            inner = Some(skin.clone());
            DockArea::new("drostje-workspace", Some(LAYOUT_VERSION), window, cx)
                .with_renderer(Rc::new(WorkspaceDockSkin { inner: skin, title }))
        });
        let inner = inner.expect("DockSkin::new ran inside the DockArea constructor");
        // A lone thread keeps its tab, like Zed, instead of a larger title.
        inner.set_panel_style(PanelStyle::TabBar, cx);
        let saved = state.read(cx).space_layout().cloned();
        let shared_sidebar = state
            .read(cx)
            .snapshot
            .settings
            .get("projects_sidebar")
            .and_then(|value| serde_json::from_value::<ProjectsSidebar>(value.clone()).ok())
            .filter(|sidebar| f32::from(sidebar.size).is_finite() && sidebar.size > px(0.));
        let presented_space = state.read(cx).active_space;
        let thread_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search threads…"));
        let mut workspace = Self {
            state,
            dock,
            projects,
            threads,
            terminals,
            browsers,
            sidebar_sizes: Rc::default(),
            live_layouts: BTreeMap::new(),
            active_terminal: None,
            space_terminals: BTreeMap::new(),
            terminal_confirmation: false,
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
            hovered_sidebar_row: None,
            space_swipe: Default::default(),
            space_slide: SpaceSlide::default(),
            space_swipe_idle: None,
            space_direction: None,
            thread_search,
            model_picker: None,
            effort_picker: None,
            permission_mode_picker: None,
            presented_thread: None,
            unrevealed_thread: None,
            detail_surface: None,
            palette: None,
            dialog: None,
            theme_editor: None,
            agent_activity: BTreeMap::new(),
            layout_save: None,
            presented_space,
            preparing_space_change: false,
            seen_notice_sequence: 0,
            reopening: None,
            dock_measurement: Rc::new(Cell::new(None)),
            _subscriptions: Vec::new(),
        };
        workspace.start_browser_tools(window, cx);
        workspace.observe_browsers(cx);
        workspace._subscriptions.push(cx.on_focus_in(
            &workspace.focus,
            window,
            |workspace, window, cx| {
                workspace.sync_terminal_visibility(window, cx);
                cx.notify();
            },
        ));
        workspace._subscriptions.push(cx.on_focus_out(
            &workspace.focus,
            window,
            |workspace, _, window, cx| {
                workspace.sync_terminal_visibility(window, cx);
                cx.notify();
            },
        ));
        let restored = saved.map(|saved| {
            let mut layout: DockAreaState = serde_json::from_value(saved)?;
            if let Some(sidebar) = shared_sidebar {
                sidebar.apply(&mut layout);
            }
            let layout = workspace.restorable_layout(durable_layout(layout), cx);
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
                workspace.reset_layout_with_sidebar(shared_sidebar, window, cx);
            }
            other => {
                workspace.reset_layout_with_sidebar(shared_sidebar, window, cx);
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
            for placement in [DockPlacement::Left, DockPlacement::Right] {
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
                workspace.sync_space(window, cx);
                workspace.sync_terminal_visibility(window, cx);
                workspace.sync_thread_panels(window, cx);
                workspace.sync_session(window, cx);
                workspace.sync_factory_form(window, cx);
                workspace.sync_model_picker(window, cx);
                workspace.sync_effort_picker(window, cx);
                workspace.sync_permission_mode_picker(window, cx);
                workspace.sync_detail_surface(window, cx);
                workspace.show_notices(window, cx);
                workspace.sync_selection_panels(window, cx);
                workspace.sync_visible_threads(cx);
                cx.notify();
            },
        ));
        workspace._subscriptions.push(cx.subscribe_in(
            &workspace.state,
            window,
            |workspace, _, event: &PresentationChange, window, cx| {
                if let PresentationChange::Thread(id) = *event {
                    workspace.sync_agent_activity(id, cx);
                }
                match *event {
                    PresentationChange::Selection => {
                        workspace.sync_space(window, cx);
                        workspace.sync_session(window, cx);
                        workspace.sync_factory_form(window, cx);
                        workspace.sync_model_picker(window, cx);
                        workspace.sync_effort_picker(window, cx);
                        workspace.sync_permission_mode_picker(window, cx);
                        workspace.sync_selection_panels(window, cx);
                        workspace.sync_visible_threads(cx);
                    }
                    PresentationChange::Thread(id)
                        if workspace.state.read(cx).selected_thread == Some(id) =>
                    {
                        workspace.sync_model_picker(window, cx);
                        workspace.sync_effort_picker(window, cx);
                        workspace.sync_permission_mode_picker(window, cx);
                        workspace.sync_detail_surface(window, cx);
                        workspace.show_notices(window, cx);
                    }
                    PresentationChange::WorktreeRequest(request) => {
                        workspace.settle_worktree_dialog(request, window, cx);
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
                    && !workspace.rebuilding
                    && workspace.presented_space == workspace.state.read(cx).active_space
                {
                    workspace.invalidate_panel_bounds(cx);
                    workspace.normalize_layout(workspace.reopening, window, cx);
                    workspace.schedule_layout_save(window, cx);
                    workspace.sync_visible_threads(cx);
                    workspace.sync_terminal_visibility(window, cx);
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
        workspace.sync_permission_mode_picker(window, cx);
        workspace.sync_selection_panels(window, cx);
        workspace.sync_visible_threads(cx);
        cx.defer_in(window, |workspace, window, cx| {
            workspace.show_notices(window, cx)
        });
        workspace.prepare_projects_cursor(cx);
        let initial_panel = if ProjectsSidebar::capture(&workspace.dock.read(cx).dump(cx))
            .is_some_and(|sidebar| !sidebar.open)
        {
            workspace.thread_target(cx)
        } else {
            Some(workspace.projects.clone())
        };
        initial_panel
            .map(|panel| panel.read(cx).focus.clone())
            .unwrap_or_else(|| workspace.focus.clone())
            .focus(window, cx);
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

    fn space_transition_reason(&self, cx: &App) -> Option<String> {
        self.state.read(cx).space_mutation_reason().or_else(|| {
            if self.terminal_confirmation {
                return Some(
                    "Finish or cancel the terminal confirmation before changing spaces.".into(),
                );
            }
            if self.preparing_space_change {
                Some("Wait for the editor draft to finish saving before changing spaces.".into())
            } else if self.extension.is_some() {
                Some("Finish or dismiss the agent request before changing spaces.".into())
            } else if self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
            {
                Some("Complete or cancel input composition before changing spaces.".into())
            } else {
                None
            }
        })
    }

    pub(super) fn swipe_space(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ScrollDelta::Pixels(delta) = event.delta else {
            return;
        };
        if self.dialog.is_some()
            || self.palette.is_some()
            || self.theme_editor.is_some()
            || window.has_active_dialog(cx)
            || window.has_active_sheet(cx)
            || self.state.read(cx).spaces.len() < 2
            || self.space_transition_reason(cx).is_some()
        {
            self.space_swipe.finish();
            self.space_swipe_idle.take();
            if self.space_slide.offset != 0. && self.space_slide.started.is_none() {
                self.space_slide.settle(Instant::now());
                self.projects.update(cx, |_, cx| cx.notify());
            }
            return;
        }
        let now = Instant::now();
        let command =
            self.space_swipe
                .feed(delta.map(|axis| axis.as_f32()), event.touch_phase, now);
        if let Some(command) = command {
            self.space_swipe_idle.take();
            self.execute(command, window, cx);
            if !self.preparing_space_change {
                self.space_slide.settle(now);
            }
        } else if let Some(distance) = self.space_swipe.progress() {
            // Ordinary vertical scrolling must not redraw or start release timers.
            if distance == 0. && self.space_slide.sample(now).0 == 0. {
                return;
            }
            let width = self
                .projects
                .read(cx)
                .bounds
                .get()
                .map_or(1., |bounds| bounds.size.width.as_f32())
                .max(1.);
            let progress = (distance / width).clamp(-1., 1.);
            self.space_slide.drag(progress);
            let state = self.state.read(cx);
            let current = state
                .spaces
                .iter()
                .position(|space| space.id == state.active_space)
                .unwrap_or(0);
            let count = state.spaces.len();
            let adjacent = if progress > 0. {
                (current + count - 1) % count
            } else {
                (current + 1) % count
            };
            self.space_slide.neighbor = (progress != 0.).then_some(state.spaces[adjacent].id);
            self.space_slide.neighbor_query = SharedString::default();
            self.space_slide.neighbor_scroll = None;
            self.space_swipe_idle.take();
            if self.space_swipe.needs_idle_release() {
                self.space_swipe_idle =
                    Some(cx.spawn_in(window, async move |workspace, window| {
                        smol::Timer::after(sidebar::SPACE_SWIPE_IDLE).await;
                        let _ = workspace.update_in(window, |workspace, window, cx| {
                            if let Some(command) = workspace.space_swipe.release() {
                                workspace.execute(command, window, cx);
                            }
                            if !workspace.preparing_space_change {
                                workspace.space_slide.settle(Instant::now());
                            }
                            workspace.projects.update(cx, |_, cx| cx.notify());
                        });
                    }));
            }
        } else {
            self.space_swipe_idle.take();
            self.space_slide.settle(now);
        }
        if cx.reduce_motion() {
            self.space_slide = SpaceSlide::default();
        }
        self.projects.update(cx, |_, cx| cx.notify());
    }

    pub(super) fn switch_space(
        &mut self,
        id: SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if id == self.state.read(cx).active_space {
            return Ok(());
        }
        let result = self.change_space(SpaceChange::Switch(id), window, cx);
        if result.is_err() {
            self.space_direction = None;
            self.space_slide.settle(Instant::now());
            cx.notify();
        }
        result
    }

    pub(super) fn move_project_to_space(
        &mut self,
        project: ProjectId,
        target: SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        self.change_space(SpaceChange::Move(project, target), window, cx)
    }

    pub(super) fn move_thread_to_space(
        &mut self,
        thread: ThreadId,
        target: SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        self.change_space(SpaceChange::MoveThread(thread, target), window, cx)
    }

    fn change_space(
        &mut self,
        change: SpaceChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if let Some(reason) = self.space_transition_reason(cx) {
            anyhow::bail!("{reason}");
        }
        let editor = self.editor.clone().and_then(|editor| {
            let key = editor.read(cx).active_key().cloned()?;
            Some((editor, key))
        });
        let Some((editor, key)) = editor else {
            return self.apply_space_change(change, window, cx);
        };
        let snapshot = editor.update(cx, |editor, cx| editor.snapshot(&key, window, cx));
        let origin = self.state.read(cx).active_space;
        self.preparing_space_change = true;
        cx.spawn_in(window, async move |workspace, window| {
            let result = snapshot.await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.preparing_space_change = false;
                let result = result.and_then(|draft| {
                    workspace.state.update(cx, |state, _| {
                        let thread = match &draft.key {
                            crate::model::DraftKey::Thread(id) | crate::model::DraftKey::Extension { thread: id, .. } => *id,
                        };
                        state.record_draft(draft);
                        state.commit_thread_edits(thread);
                    });
                    anyhow::ensure!(workspace.state.read(cx).active_space == origin,
                        "Space changed while saving the draft; choose the destination again");
                    if let Some(reason) = workspace.space_transition_reason(cx) {
                        anyhow::bail!("{reason}");
                    }
                    workspace.apply_space_change(change, window, cx)
                });
                if let Err(error) = result {
                    workspace.space_direction = None;
                    workspace.space_slide.settle(Instant::now());
                    workspace.space_error(error.context("Space was not changed; finish or cancel the editor command and try again"), cx);
                }
                cx.notify();
            });
        }).detach();
        Ok(())
    }

    fn apply_space_change(
        &mut self,
        change: SpaceChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        self.layout_save.take();
        self.flush_layout(cx);
        self.state.update(cx, |state, _| -> anyhow::Result<()> {
            match change {
                SpaceChange::Switch(id) => state.switch_space(id)?,
                SpaceChange::Move(project, target) => {
                    state.move_project_to_space(project, target)?
                }
                SpaceChange::MoveThread(thread, target) => {
                    state.move_thread_to_space(thread, target)?
                }
                SpaceChange::Delete(id) => {
                    anyhow::ensure!(id != state.default_space_id(), "Default cannot be deleted");
                    if state.active_space == id {
                        state.switch_space(state.default_space_id())?;
                    }
                    state.delete_space(id)?;
                }
            }
            Ok(())
        })?;
        for panel in self.terminals.borrow().values() {
            if let Some(pane) = panel.read(cx).terminal.clone() {
                let target = match change {
                    SpaceChange::Move(project, target) if pane.read(cx).project_id == project => {
                        Some(target)
                    }
                    SpaceChange::Delete(space) if pane.read(cx).space_id == space => {
                        Some(self.state.read(cx).default_space_id())
                    }
                    _ => None,
                };
                if let Some(target) = target {
                    pane.update(cx, |pane, cx| {
                        pane.space_id = target;
                        pane.set_visible(false, cx);
                    });
                }
            }
        }
        // A moved terminal is reattached in its project's destination, not duplicated
        // by a stale cached layout. The process and working directory stay intact.
        let terminal_spaces: BTreeMap<_, _> = self
            .terminals
            .borrow()
            .iter()
            .filter_map(|(id, panel)| {
                panel
                    .read(cx)
                    .terminal
                    .as_ref()
                    .map(|pane| (*id, pane.read(cx).space_id))
            })
            .collect();
        for (space, layout) in &mut self.live_layouts {
            *layout = filter_layout(layout.clone(), &|panel| {
                terminal_of(panel).is_none_or(|id| terminal_spaces.get(&id) == Some(space))
            });
        }
        self.sync_space(window, cx);
        self.sync_thread_panels(window, cx);
        self.sync_session(window, cx);
        if matches!(change, SpaceChange::Move(..) | SpaceChange::MoveThread(..)) {
            self.cursor = None;
            self.flush_layout(cx);
            self.focus_region(Region::Projects, window, cx);
        }
        self.state.update(cx, |_, cx| cx.notify());
        Ok(())
    }

    fn sync_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.state.read(cx).active_space;
        if active == self.presented_space {
            return;
        }
        let outgoing_layout = self.dock.read(cx).dump(cx);
        let shared_sidebar = ProjectsSidebar::capture(&outgoing_layout);
        let direction = self.space_direction.take().unwrap_or_else(|| {
            let spaces = &self.state.read(cx).spaces;
            let old = spaces
                .iter()
                .position(|space| space.id == self.presented_space);
            let new = spaces.iter().position(|space| space.id == active);
            if new > old { -1. } else { 1. }
        });
        self.space_swipe.finish();
        self.space_swipe_idle.take();
        let now = Instant::now();
        let offset = if self.space_slide.neighbor == Some(active) {
            // Rebase the same two strips: destination becomes active without a jump.
            self.space_slide.sample(now).0 - direction
        } else {
            -direction
        };
        self.space_slide.neighbor = Some(self.presented_space);
        self.space_slide.neighbor_query = self.thread_search.read(cx).value();
        self.space_slide.neighbor_scroll = Some(
            self.projects
                .update(cx, |panel, _| std::mem::take(&mut panel.scroll)),
        );
        self.space_slide.drag(offset);
        self.space_slide.settle(now);
        if cx.reduce_motion() {
            self.space_slide = SpaceSlide::default();
        }
        self.layout_save.take();
        self.hide_terminals(cx);
        self.live_layouts.insert(
            self.presented_space,
            filter_layout(outgoing_layout, &|panel| {
                terminal_of(panel).is_none_or(|id| {
                    self.terminals
                        .borrow()
                        .get(&id)
                        .and_then(|panel| panel.read(cx).terminal.as_ref())
                        .is_some_and(|pane| pane.read(cx).space_id == self.presented_space)
                })
            }),
        );
        if let Some(id) = self.active_terminal {
            self.space_terminals.insert(self.presented_space, id);
        }
        self.active_terminal = self.space_terminals.get(&active).copied();
        self.presented_space = active;
        self.rebuilding = true;
        self.threads.borrow_mut().clear();
        self.welcome.borrow_mut().take();
        self.cursor = None;
        self.presented_thread = None;
        self.unrevealed_thread = None;
        self.reopening = None;
        self.thread_search
            .update(cx, |input, cx| input.set_value("", window, cx));
        let saved = self.live_layouts.get(&active).cloned().map(Ok).or_else(|| {
            self.state
                .read(cx)
                .space_layout()
                .cloned()
                .map(serde_json::from_value)
        });
        let restored = saved.map(|saved| -> anyhow::Result<()> {
            let mut layout: DockAreaState = saved?;
            if let Some(sidebar) = shared_sidebar {
                sidebar.apply(&mut layout);
            }
            let layout = self.restorable_layout(layout, cx);
            validate_layout(&durable_layout(layout.clone()))?;
            self.dock
                .update(cx, |dock, cx| dock.load(layout, window, cx))
        });
        self.rebuilding = false;
        if !matches!(restored, Some(Ok(()))) {
            if let Some(Err(error)) = restored
                && error.to_string() != OUTDATED_LAYOUT
            {
                self.state.update(cx, |state, _| {
                    state.notice(
                        NoticeLevel::Warning,
                        format!("Only this space's saved layout was reset: {error:#}"),
                    )
                });
            }
            self.reset_layout_with_sidebar(shared_sidebar, window, cx);
        } else {
            self.sync_thread_panels(window, cx);
            self.invalidate_panel_bounds(cx);
            self.normalize_layout(None, window, cx);
        }
        self.sync_session(window, cx);
        self.sync_factory_form(window, cx);
        self.sync_selection_panels(window, cx);
        self.projects.update(cx, |_, cx| cx.notify());
        cx.defer_in(window, move |workspace, window, cx| {
            if workspace.presented_space == active {
                let region = if workspace.state.read(cx).selected_thread.is_some()
                    || workspace.has_active_terminal(cx)
                    || ProjectsSidebar::capture(&workspace.dock.read(cx).dump(cx))
                        .is_some_and(|sidebar| !sidebar.open)
                {
                    Region::Thread
                } else {
                    Region::Projects
                };
                workspace.focus_region(region, window, cx);
            }
        });
        cx.notify();
    }

    fn space_error(&self, error: anyhow::Error, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.notice(NoticeLevel::Warning, format!("{error:#}"));
            cx.notify();
        });
    }

    fn open_harness_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
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
        self.palette = Some(Palette::open_harnesses(
            cx.entity().downgrade(),
            self.state.clone(),
            id,
            window,
            cx,
        ));
        cx.notify();
    }

    fn open_space_picker(&mut self, moving: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(reason) = self.space_transition_reason(cx) {
            self.space_error(anyhow::anyhow!(reason), cx);
            return;
        }
        self.dismiss(window, cx);
        if self.navigation_blocked(window, cx) {
            return;
        }
        self.palette = Some(Palette::open_spaces(
            cx.entity().downgrade(),
            self.state.clone(),
            moving,
            window,
            cx,
        ));
    }
    /// Drop Terminal leaves the Terminal registry could not resolve for the active Space:
    /// neither a live terminal nor a saved tab whose project is still in this Space.
    fn restorable_layout(&self, layout: DockAreaState, cx: &App) -> DockAreaState {
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
    pub(super) fn has_active_terminal(&self, cx: &App) -> bool {
        self.active_terminal(cx).is_some()
    }
    pub(super) fn active_terminal(&self, cx: &App) -> Option<Entity<TerminalPane>> {
        self.active_terminal
            .and_then(|id| self.terminals.borrow().get(&id).cloned())
            .and_then(|panel| panel.read(cx).terminal.clone())
            .filter(|pane| pane.read(cx).space_id == self.presented_space)
    }
    pub(super) fn center_panel_count(&self, cx: &App) -> usize {
        self.open_thread_ids(cx).len()
            + self.active_space_terminals(cx).len()
            + self.browser_tab_count(cx)
    }
    #[cfg(all(test, feature = "runtime-qa"))]
    pub(super) fn terminal_count(&self, _: &App) -> usize {
        self.terminals.borrow().len()
    }
    pub(super) fn has_running_terminals(&self, cx: &App) -> bool {
        self.terminals.borrow().values().any(|panel| {
            panel
                .read(cx)
                .terminal
                .as_ref()
                .is_some_and(|pane| pane.read(cx).is_alive(cx))
        })
    }
    pub(super) fn close_all_terminals(&mut self, cx: &mut Context<Self>) {
        for (_, panel) in std::mem::take(&mut *self.terminals.borrow_mut()) {
            if let Some(pane) = panel.read(cx).terminal.clone() {
                pane.update(cx, |pane, cx| pane.close(cx));
            }
        }
        self.active_terminal = None;
        self.space_terminals.clear();
        self.live_layouts.clear();
    }
    fn active_space_terminals(&self, cx: &App) -> Vec<Entity<WorkspacePanel>> {
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
    fn hide_terminals(&self, cx: &mut App) {
        for panel in self.terminals.borrow().values() {
            if let Some(pane) = panel.read(cx).terminal.clone() {
                pane.update(cx, |pane, cx| pane.set_visible(false, cx));
            }
        }
    }
    fn sync_terminal_visibility(&self, _: &mut Window, cx: &mut App) {
        // Ghostty frames are GPUI scene content: overlays cover them normally.
        // Only inactive tabs/Spaces are occluded; focus independently gates input.
        let dock = self.dock.read(cx);
        let layout = dock.dump(cx);
        let panes: Vec<_> = self
            .terminals
            .borrow()
            .values()
            .filter_map(|panel| {
                let pane = panel.read(cx).terminal.clone()?;
                let visible = pane.read(cx).space_id == self.presented_space
                    && panel_visible(panel, dock, &layout, cx);
                Some((pane, visible))
            })
            .collect();
        for (pane, visible) in panes {
            pane.update(cx, |pane, cx| pane.set_visible(visible, cx));
        }
    }
    fn terminal_focused(&mut self, id: TerminalId, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        let Some(project) = self
            .state
            .read(cx)
            .selected_project
            .and_then(|id| {
                self.state
                    .read(cx)
                    .snapshot
                    .projects
                    .iter()
                    .find(|project| project.id == id)
            })
            .cloned()
        else {
            return;
        };
        // A terminal opened from a Conversation starts where that Conversation works.
        let (root, worktree) = {
            let state = self.state.read(cx);
            state
                .conversation_thread()
                .filter(|thread| thread.project_id == project.id)
                .and_then(|thread| state.thread_worktree(thread))
                .filter(|worktree| worktree.is_live())
                .map(|worktree| (worktree.path.clone(), Some(worktree.id)))
                .unwrap_or((project.root.clone(), None))
        };
        let id = TerminalId::new();
        let owner = cx.entity().downgrade();
        let state = self.state.clone();
        let space = self.presented_space;
        let anchor = self
            .thread_target(cx)
            .and_then(|panel| panel.read(cx).group.clone())
            .and_then(|group| group.upgrade())
            .map(|group| group.read(cx).node());
        let pane = cx.new(|cx| TerminalPane::new(project.id, worktree, space, root, window, cx));
        let panel = cx.new(|cx| {
            let mut panel = WorkspacePanel::new(PanelKind::Terminal(id), state, owner, window, cx);
            panel.terminal = Some(pane);
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
    pub(super) fn cycle_panel(
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
    pub(super) fn close_active_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn restart_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active_terminal {
            self.confirm_terminal_stop(id, true, window, cx);
        }
    }
    fn close_terminal_tab(&mut self, id: TerminalId, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_terminal_stop(id, false, window, cx);
    }
    fn confirm_terminal_stop(
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
        self.terminal_confirmation = true;
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
                .title(div().id("terminal-stop-confirmation").test_support().child(title.clone()))
                .description("This stops this terminal's shell and foreground jobs. Other terminals are not affected.")
                .button_props(DialogButtonProps::default()
                    .ok_text(if restart { "Restart terminal" } else { "Close terminal" })
                    .cancel_text("Cancel").show_cancel(true))
                .on_ok(move |_, window, cx| {
                    let confirm = confirm.clone();
                    // Own close ordering: restore modal focus before replacing its terminal.
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
    fn finish_terminal_stop(
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

    pub fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_layout_with_sidebar(None, window, cx);
    }

    fn reset_layout_with_sidebar(
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
        // Dock APIs consume pixels; application geometry is resolved from rem here.
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

    fn open_thread_ids(&self, cx: &App) -> Vec<ThreadId> {
        let state = self.state.read(cx);
        state
            .open_threads
            .iter()
            .copied()
            .filter(|id| state.thread_in_active_space(*id))
            .collect()
    }
    fn all_panels(&self) -> Vec<Entity<WorkspacePanel>> {
        let mut panels = vec![self.projects.clone()];
        panels.extend(self.welcome.borrow().iter().cloned());
        panels.extend(self.threads.borrow().values().cloned());
        panels.extend(self.terminals.borrow().values().cloned());
        panels.extend(self.browsers.panels.borrow().values().cloned());
        panels
    }
    fn thread_target(&self, cx: &App) -> Option<Entity<WorkspacePanel>> {
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
    pub(super) fn sync_thread_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        if lost_focus {
            // The closed pane held focus; keyboard control continues in the next thread.
            if let Some(target) = self.thread_target(cx) {
                self.focus_panel(target, window, cx);
            }
        }
        cx.notify();
    }
    fn sync_selection_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn reveal_thread_tab(
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
    fn sync_visible_threads(&mut self, cx: &mut Context<Self>) {
        let dock = self.dock.read(cx);
        let layout = dock.dump(cx);
        let visible: Vec<_> = self
            .threads
            .borrow()
            .iter()
            .filter(|(_, panel)| panel_visible(panel, dock, &layout, cx))
            .map(|(id, _)| *id)
            .collect();
        self.state.update(cx, |state, cx| {
            for &id in &visible {
                state.ensure_conversation(id);
            }
            state.set_visible_conversations(visible, cx);
        });
    }

    /// A pane that gained focus (click, tab, Ctrl-W) is the selected thread.
    fn thread_focused(
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
    fn thread_panel_removed(
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

    pub(super) fn navigation_blocked(&self, window: &mut Window, cx: &mut App) -> bool {
        self.quit_requested
            || self.preparing_space_change
            || self.terminal_confirmation
            || self.state.read(cx).shutting_down
            || self.palette.is_some()
            || self.dialog.is_some()
            || self.theme_editor.is_some()
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

    fn schedule_layout_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn flush_layout(&mut self, cx: &mut Context<Self>) {
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
    /// Close a thread's tab. The thread stays in its project, and a running
    /// turn keeps running.
    pub(super) fn close_thread_tab(
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
        self.state.update(cx, |state, cx| {
            state.close_thread(id);
            cx.notify();
        });
        self.sync_thread_panels(window, cx);
    }
    /// Archive `id` without selecting it: its tab closes, and a neighbouring tab takes over
    /// only when it was the selected one.
    pub(super) fn archive_thread(
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
    pub(super) fn unarchive_thread(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            if let Err(error) = state.unarchive_thread(id) {
                state.notice(crate::app::NoticeLevel::Warning, format!("{error:#}"));
            }
            cx.emit(PresentationChange::Thread(id));
            cx.notify();
        });
    }
    /// Rename `id` from Projects or its tab without opening or selecting it.
    pub(super) fn rename_thread(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(reason) = self.state.read(cx).rename_thread_reason_for(id) {
            self.state.update(cx, |state, cx| {
                state.notice(crate::app::NoticeLevel::Warning, reason);
                cx.notify();
            });
            return;
        }
        self.open_dialog(DialogKind::RenameThread(id), window, cx);
    }
    /// Show `tab` in a thread's sidebar (tab click, Focus output).
    pub(super) fn show_sidebar_tab(
        &mut self,
        id: ThreadId,
        tab: SidebarTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tab == SidebarTab::Browser {
            self.ensure_sidebar_browser(id, window, cx);
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
    }
    /// Open a Conversation's Agents tab when its running turn starts working with other
    /// agents (see `agents::Follow`). Focus stays where it is.
    fn sync_agent_activity(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(session) = state.conversations.get(&id) else {
            return;
        };
        let count = super::agents::coordination_calls(session);
        let running = state.active_threads.contains(&id);
        let open = self
            .agent_activity
            .entry(id)
            .or_insert_with(|| super::agents::Follow::new(count))
            .observe(count, running);
        if open {
            self.state.update(cx, |state, cx| {
                state.show_thread_sidebar(id, SidebarTab::Agents);
                cx.emit(PresentationChange::Thread(id));
            });
            self.sync_visible_threads(cx);
        }
    }
    /// Show `tab` in a thread's sidebar, or hide the sidebar when `tab` is on screen.
    fn toggle_sidebar_tab(&mut self, id: ThreadId, tab: SidebarTab, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.toggle_thread_sidebar(id, tab);
            cx.emit(PresentationChange::Thread(id));
        });
        self.sync_visible_threads(cx);
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
    /// Anchor Projects keyboard navigation to the current selection before focus moves in.
    /// Without a cursor the pane receives focus but draws no focus indication, so the
    /// first `j`/`k` starts from an unrelated row.
    fn prepare_projects_cursor(&mut self, cx: &mut Context<Self>) {
        let query = self.thread_search.read(cx).value();
        let (rows, selected) = {
            let state = self.state.read(cx);
            let rows = sidebar::rows(state, &query);
            let selected = state
                .selected_thread
                .map(Row::Thread)
                .filter(|row| rows.contains(row))
                .or_else(|| {
                    state
                        .selected_project
                        .map(Row::Project)
                        .filter(|row| rows.contains(row))
                });
            (rows, selected)
        };
        let target = selected
            .or_else(|| self.cursor.filter(|row| rows.contains(row)))
            .or_else(|| first_thread_row(&rows));
        self.cursor = target;
        if let Some(index) = rows.iter().position(|row| Some(*row) == target) {
            self.projects.read(cx).scroll.scroll_to_item(index);
        }
        self.projects.update(cx, |_, cx| cx.notify());
    }

    pub fn focus_region(&mut self, region: Region, window: &mut Window, cx: &mut Context<Self>) {
        // A thread selected in this same update has no panel until the sync runs.
        if region == Region::Thread {
            self.sync_thread_panels(window, cx);
        }
        let target = match region {
            Region::Projects => Some(self.projects.clone()),
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
        // A newly activated tab has not entered the rendered focus tree yet.
        // Commit selection here; focus callbacks cover already-visible panes.
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
        let Some(origin) = visible_bounds(&current) else {
            return;
        };
        let next = self
            .all_panels()
            .into_iter()
            .filter(|panel| *panel != current)
            .filter_map(|panel| {
                let bounds = visible_bounds(&panel)?;
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
                // Neighbours must lie beyond this edge, not merely have a centre
                // somewhere in that direction. Allow one pixel of layout rounding.
                (gap >= px(-1.) && overlap > px(1.)).then_some((
                    panel,
                    f32::from(gap).max(0.),
                    f32::from(cross).abs(),
                ))
            })
            // Edge distance takes precedence: never skip an intervening pane
            // because a farther pane's centre happens to line up better.
            .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.2.total_cmp(&b.2)))
            .map(|(panel, _, _)| panel);
        if let Some(next) = next {
            self.focus_panel(next, window, cx);
        }
    }

    pub fn execute(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).selected_thread.is_some() {
            self.active_terminal = None;
        }
        if self.state.read(cx).shutting_down {
            return;
        }
        if self.preparing_space_change
            && !matches!(
                command,
                Command::CancelEditorCommand | Command::CopyDraftRecovery
            )
        {
            self.space_error(
                anyhow::anyhow!("Wait for the editor draft to finish saving before navigating"),
                cx,
            );
            return;
        }
        if matches!(
            command,
            Command::NextThread
                | Command::PreviousThread
                | Command::CloseThread
                | Command::NewConversation
                | Command::NewOmpConversation
                | Command::NewPiConversation
                | Command::NewClaudeCodeConversation
                | Command::NewConversationInWorktree
                | Command::NewFactoryThread
                | Command::NewTerminal
                | Command::NewBrowser
                | Command::RestartTerminal
        ) && self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        if let Some(reason) = command.disabled_reason_in_workspace(self, cx) {
            self.state.update(cx, |state, cx| {
                state.notice(crate::app::NoticeLevel::Warning, reason);
                cx.notify();
            });
            return;
        }
        let kind = match command {
            Command::WidenSidebar | Command::NarrowSidebar => {
                if let Some(id) = self.state.read(cx).selected_thread {
                    let sizes = self.sidebar_sizes.borrow().get(&id).cloned();
                    if let Some(sizes) = sizes {
                        let width = sizes.read(cx).sizes().get(1).copied();
                        if let Some(width) = width {
                            let delta = if command == Command::WidenSidebar {
                                48.
                            } else {
                                -48.
                            };
                            sizes.update(cx, |sizes, cx| {
                                sizes.resize_panel(1, width + px(delta), window, cx)
                            });
                            if let Some(width) = sizes.read(cx).sizes().get(1).copied() {
                                self.state.update(cx, |state, _| {
                                    state.set_thread_sidebar_width(id, f32::from(width))
                                });
                            }
                        }
                    }
                }
                None
            }
            Command::NewBrowser => {
                self.open_browser("about:blank".into(), None, None, window, cx);
                None
            }
            Command::ToggleBrowser => {
                if let Some(id) = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .map(|thread| thread.id)
                {
                    if self.sidebar_browser(id).is_none() {
                        self.ensure_sidebar_browser(id, window, cx);
                    } else {
                        self.toggle_sidebar_tab(id, SidebarTab::Browser, cx);
                    }
                }
                None
            }
            Command::BrowserBack
            | Command::BrowserForward
            | Command::BrowserReload
            | Command::BrowserAddress => {
                if let Some(pane) = self.selected_browser(cx) {
                    pane.update(cx, |pane, cx| pane.command(command, window, cx));
                }
                None
            }
            Command::NewTerminal => {
                self.new_terminal(window, cx);
                None
            }
            Command::RestartTerminal => {
                self.restart_terminal(window, cx);
                None
            }
            Command::NewSpace => Some(DialogKind::NewSpace),
            Command::RenameSpace => Some(DialogKind::RenameSpace(self.state.read(cx).active_space)),
            Command::DeleteSpace => Some(DialogKind::DeleteSpace(self.state.read(cx).active_space)),
            Command::SwitchSpace | Command::MoveProjectToSpace => {
                self.open_space_picker(command == Command::MoveProjectToSpace, window, cx);
                None
            }
            Command::NextSpace | Command::PreviousSpace => {
                let state = self.state.read(cx);
                let count = state.spaces.len();
                let current = state
                    .spaces
                    .iter()
                    .position(|space| space.id == state.active_space)
                    .unwrap_or(0);
                let index = if command == Command::NextSpace {
                    (current + 1) % count
                } else {
                    (current + count - 1) % count
                };
                let target = state.spaces[index].id;
                self.space_direction = Some(if command == Command::NextSpace {
                    -1.
                } else {
                    1.
                });
                if let Err(error) = self.switch_space(target, window, cx) {
                    self.space_error(error, cx);
                }
                None
            }
            Command::InstallPi => {
                self.state.update(cx, |state, cx| {
                    state.install_pi();
                    cx.notify();
                });
                None
            }
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
            Command::InsertSlashCommand => {
                self.open_slash_command_picker(window, cx);
                None
            }
            Command::ClearQueuedMessages => {
                self.clear_queued_messages(window, cx);
                None
            }
            Command::SendQueuedNow => {
                if let Some((id, message)) = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .and_then(|thread| thread.queued.first().map(|message| (thread.id, message.id)))
                {
                    self.send_queued_now(id, message, cx);
                }
                None
            }
            Command::EditQueuedMessage => {
                if let Some((id, message)) = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .and_then(|thread| thread.queued.last().map(|message| (thread.id, message.id)))
                {
                    self.edit_queued(id, message, window, cx);
                }
                None
            }
            Command::ToggleConversationDiffs => {
                if let Some(id) = self.state.read(cx).conversation_thread().map(|t| t.id) {
                    self.toggle_sidebar_tab(id, SidebarTab::Changes, cx);
                }
                None
            }
            Command::ToggleAgents => {
                if let Some(id) = self.state.read(cx).conversation_thread().map(|t| t.id) {
                    self.toggle_sidebar_tab(id, SidebarTab::Agents, cx);
                }
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
            Command::RegenerateTitle => {
                self.state
                    .update(cx, |state, cx| state.regenerate_title(cx));
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
            Command::NewConversationInWorktree => self
                .state
                .read(cx)
                .selected_project
                .map(DialogKind::NewWorktreeConversation),
            Command::SwitchHarness => {
                self.open_harness_picker(window, cx);
                None
            }
            Command::RemoveWorktree => self
                .state
                .read(cx)
                .conversation_thread()
                .and_then(|thread| thread.worktree_id)
                .map(DialogKind::RemoveWorktree),
            Command::SwitchBranch => self
                .state
                .read(cx)
                .conversation_thread()
                .map(|thread| DialogKind::SwitchBranch(thread.id)),
            Command::ArchiveThread => {
                if self.extension.is_some()
                    || self
                        .editor
                        .as_ref()
                        .is_some_and(|editor| editor.read(cx).has_composition())
                {
                    return;
                }
                self.dismiss(window, cx);
                if self.detail_surface.is_some()
                    || self.dialog.is_some()
                    || self.palette.is_some()
                    || self.theme_editor.is_some()
                {
                    return;
                }
                if let Some(id) = self.state.read(cx).thread().map(|thread| thread.id) {
                    self.archive_thread(id, window, cx);
                }
                None
            }
            Command::UnarchiveThread => {
                if let Some(id) = self.state.read(cx).thread().map(|thread| thread.id) {
                    self.unarchive_thread(id, cx);
                }
                None
            }
            Command::Settings => Some(DialogKind::Settings(self.state.read(cx).selected_project)),
            Command::CustomizeTheme => {
                self.open_theme_editor(window, cx);
                None
            }
            // Asking again from a Conversation nobody used yet would only stack another
            // blank one; offer its harness instead, so New conversation twice switches it.
            Command::NewConversation
                if self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .is_some_and(|thread| {
                        let state = self.state.read(cx);
                        state.is_untouched(thread) && state.switch_harness_reason().is_none()
                    }) =>
            {
                self.open_harness_picker(window, cx);
                None
            }
            Command::NewConversation
            | Command::NewOmpConversation
            | Command::NewPiConversation
            | Command::NewClaudeCodeConversation
            | Command::NewFactoryThread => {
                let created = self.state.update(cx, |state, cx| {
                    let result = if command == Command::NewConversation {
                        state.new_thread(ThreadKind::Conversation)
                    } else if command == Command::NewOmpConversation {
                        state.new_conversation("omp")
                    } else if command == Command::NewPiConversation {
                        state.new_conversation("pi")
                    } else if command == Command::NewClaudeCodeConversation {
                        state.new_conversation("claude-code")
                    } else {
                        state.new_thread(ThreadKind::Factory)
                    };
                    let created = result.is_ok();
                    if let Err(error) = result {
                        state.notice(crate::app::NoticeLevel::Error, format!("{error:#}"));
                    }
                    cx.notify();
                    created
                });
                if created {
                    self.focus_region(Region::Thread, window, cx);
                }
                None
            }
            Command::ShowArchived => {
                self.state.update(cx, |state, cx| {
                    state.set_show_archived(!state.show_archived);
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
                self.close_active_panel(window, cx);
                None
            }
            Command::NextThread | Command::PreviousThread => {
                self.cycle_panel(command == Command::NextThread, window, cx);
                None
            }
            Command::FocusProjects | Command::FocusThread => {
                self.focus_region(
                    match command {
                        Command::FocusProjects => Region::Projects,
                        _ => Region::Thread,
                    },
                    window,
                    cx,
                );
                None
            }
            Command::FocusOutput => {
                if let Some(id) = self.state.read(cx).thread().map(|thread| thread.id) {
                    self.show_sidebar_tab(id, SidebarTab::Output, window, cx);
                }
                None
            }
            Command::ToggleProjects => {
                self.toggle_region(Region::Projects, window, cx);
                None
            }
            Command::ToggleOutput => {
                if let Some(id) = self.state.read(cx).thread().map(|thread| thread.id) {
                    self.toggle_sidebar_tab(id, SidebarTab::Output, cx);
                }
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
        if self.detail_surface.is_some()
            || self.dialog.is_some()
            || self.palette.is_some()
            || self.theme_editor.is_some()
        {
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
        if let Some(editor) = self.theme_editor.take() {
            editor.update(cx, |editor, cx| editor.dismiss(window, cx));
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
        // A request already running owns the dialog until it settles.
        if dialog.read(cx).request.is_some() {
            self.dialog = Some(dialog);
            return;
        }
        if matches!(
            dialog.read(cx).kind,
            DialogKind::NewSpace | DialogKind::DeleteSpace(_)
        ) && let Some(reason) = self.space_transition_reason(cx)
        {
            dialog.update(cx, |dialog, cx| {
                dialog.set_error(&anyhow::anyhow!(reason), cx)
            });
            self.dialog = Some(dialog);
            return;
        }
        let result = self.state.update(cx, |state, cx| {
            let result = dialog.read(cx).apply(state, cx);
            if result.is_ok() {
                cx.notify();
            }
            result
        });
        let result = result.and_then(|outcome| {
            match outcome {
                SurfaceOutcome::Applied => {}
                SurfaceOutcome::SpaceCreated(id) => self.switch_space(id, window, cx)?,
                SurfaceOutcome::DeleteSpace(id) => {
                    self.change_space(SpaceChange::Delete(id), window, cx)?
                }
                SurfaceOutcome::Pending(request) => return Ok(Some(request)),
            }
            Ok(None)
        });
        match result {
            Ok(None) => {
                let kind = dialog.read(cx).kind.clone();
                super::settings::close_surface(&kind, window, cx);
                cx.notify();
            }
            Ok(Some(request)) => {
                dialog.update(cx, |dialog, cx| dialog.begin_request(request, cx));
                self.dialog = Some(dialog);
            }
            Err(error) => {
                dialog.update(cx, |dialog, cx| dialog.set_error(&error, cx));
                self.dialog = Some(dialog);
            }
        }
    }
    /// Closes the dialog that started `request` once the App layer reports it done; failures
    /// stay in the dialog, which shows them next to the form.
    fn settle_worktree_dialog(
        &mut self,
        request: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let done = self
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).request == Some(request))
            && self
                .state
                .read(cx)
                .worktree_request
                .as_ref()
                .is_some_and(|pending| {
                    pending.id == request
                        && matches!(pending.status, crate::app::WorktreeRequestStatus::Done(_))
                });
        if !done {
            return;
        }
        if let Some(dialog) = self.dialog.take() {
            let kind = dialog.read(cx).kind.clone();
            super::settings::close_surface(&kind, window, cx);
        }
        self.focus_region(Region::Thread, window, cx);
        cx.notify();
    }
    pub(super) fn set_sidebar_row_hovered(
        &mut self,
        row: Row,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let next = if hovered {
            Some(row)
        } else if self.hovered_sidebar_row == Some(row) {
            None
        } else {
            return;
        };
        if self.hovered_sidebar_row != next {
            self.hovered_sidebar_row = next;
            self.projects.update(cx, |_, cx| cx.notify());
        }
    }
    pub fn select_row(
        &mut self,
        row: Row,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let member = match row {
            Row::Project(id) => self.state.read(cx).project_in_active_space(id),
            Row::Thread(id) => self.state.read(cx).thread_in_active_space(id),
        };
        if !member || self.preparing_space_change {
            return;
        }
        self.cursor = Some(row);
        self.state.update(cx, |state, cx| {
            match row {
                Row::Project(id) => state.select_project(id),
                Row::Thread(id) if open => state.select_thread(id),
                _ => {}
            }
            cx.emit(PresentationChange::Selection);
        });
        if let Row::Thread(id) = row
            && open
        {
            // Reopening the selected thread shows its tab even when another tab
            // in its group is displayed.
            self.unrevealed_thread = Some(id);
            self.sync_selection_panels(window, cx);
        }
        self.focus_region(Region::Projects, window, cx);
    }
    fn navigate(&mut self, motion: Motion, window: &mut Window, cx: &mut Context<Self>) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        let region = self.focused_region(window, cx);
        if region == Region::Thread
            && let Some(transcript) = self
                .focused_panel(window, cx)
                .and_then(|panel| panel.read(cx).transcript.clone())
        {
            transcript.update(cx, |transcript, cx| transcript.scroll(motion, window, cx));
            return;
        }
        if region != Region::Projects {
            let Some(panel) = self.focused_panel(window, cx) else {
                return;
            };
            let scroll = panel.read(cx).scroll.clone();
            let scroll_by = |distance: Pixels| {
                let offset = scroll.offset();
                scroll.set_offset(point(offset.x, (offset.y - distance).min(px(0.))));
            };
            match motion {
                Motion::First => scroll.set_offset(point(px(0.), px(0.))),
                Motion::Last => scroll.scroll_to_bottom(),
                Motion::Line(delta) => scroll_by(window.rem_size() * (delta as f32 * 2.25)),
                Motion::Page(delta) => {
                    scroll_by(scroll.bounds().size.height * (delta as f32 * PAGE_SHARE))
                }
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
        let last = rows.len() - 1;
        let step = |delta: isize| (index as isize + delta).clamp(0, last as isize) as usize;
        let next = match motion {
            Motion::First => 0,
            Motion::Last => last,
            Motion::Line(delta) => step(delta),
            Motion::Page(delta) => {
                let scroll = &self.projects.read(cx).scroll;
                // Rows share one height; an unmeasured list falls back to a short page.
                let page = scroll
                    .bounds_for_item(index)
                    .filter(|row| row.size.height > px(0.))
                    .map_or(10, |row| {
                        (scroll.bounds().size.height * PAGE_SHARE / row.size.height) as isize
                    });
                step(delta * page.max(1))
            }
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
                state.set_project_collapsed(id, collapsed);
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
        if self.detail_surface.is_some()
            || self.dialog.is_some()
            || self.palette.is_some()
            || self.theme_editor.is_some()
        {
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
    fn open_theme_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        if self.detail_surface.is_some()
            || self.dialog.is_some()
            || self.palette.is_some()
            || self.theme_editor.is_some()
        {
            return;
        }
        self.theme_editor = Some(super::theme_editor::ThemeEditor::open(
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
        self.workspace.update(cx, |workspace, cx| {
            workspace.sync_terminal_visibility(window, cx)
        });
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_terminal_visibility(window, cx);
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
                .on_action(cx.listener(|this, _: &NewTerminal, w, cx| {
                    this.execute(Command::NewTerminal, w, cx)
                }))
                .on_action(cx.listener(|this, _: &RestartTerminal, w, cx| {
                    this.execute(Command::RestartTerminal, w, cx)
                }))
                .on_action(cx.listener(|this, action: &OpenBrowserUrl, w, cx| {
                    this.open_browser(action.url.clone(), None, None, w, cx);
                }))
                .on_action(cx.listener(|this, _: &NewBrowser, w, cx| {
                    this.execute(Command::NewBrowser, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ToggleBrowser, w, cx| {
                    this.execute(Command::ToggleBrowser, w, cx)
                }))
                .on_action(cx.listener(|this, _: &BrowserBack, w, cx| {
                    this.execute(Command::BrowserBack, w, cx)
                }))
                .on_action(cx.listener(|this, _: &BrowserForward, w, cx| {
                    this.execute(Command::BrowserForward, w, cx)
                }))
                .on_action(cx.listener(|this, _: &BrowserReload, w, cx| {
                    this.execute(Command::BrowserReload, w, cx)
                }))
                .on_action(cx.listener(|this, _: &BrowserAddress, w, cx| {
                    this.execute(Command::BrowserAddress, w, cx)
                }))
                .on_action(cx.listener(|this, _: &WidenSidebar, w, cx| {
                    this.execute(Command::WidenSidebar, w, cx)
                }))
                .on_action(cx.listener(|this, _: &NarrowSidebar, w, cx| {
                    this.execute(Command::NarrowSidebar, w, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &NewSpace, w, cx| this.execute(Command::NewSpace, w, cx)),
                )
                .on_action(cx.listener(|this, _: &RenameSpace, w, cx| {
                    this.execute(Command::RenameSpace, w, cx)
                }))
                .on_action(cx.listener(|this, _: &DeleteSpace, w, cx| {
                    this.execute(Command::DeleteSpace, w, cx)
                }))
                .on_action(cx.listener(|this, _: &SwitchSpace, w, cx| {
                    this.execute(Command::SwitchSpace, w, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &NextSpace, w, cx| {
                        this.execute(Command::NextSpace, w, cx)
                    }),
                )
                .on_action(cx.listener(|this, _: &PreviousSpace, w, cx| {
                    this.execute(Command::PreviousSpace, w, cx)
                }))
                .on_action(cx.listener(|this, _: &MoveProjectToSpace, w, cx| {
                    this.execute(Command::MoveProjectToSpace, w, cx)
                }))
                .on_action(cx.listener(|this, _: &AddProject, w, cx| {
                    this.execute(Command::AddProject, w, cx)
                }))
                .on_action(cx.listener(|this, _: &NewConversation, w, cx| {
                    this.execute(Command::NewConversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &NewOmpConversation, w, cx| {
                    this.execute(Command::NewOmpConversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &NewPiConversation, w, cx| {
                    this.execute(Command::NewPiConversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &NewClaudeCodeConversation, w, cx| {
                    this.execute(Command::NewClaudeCodeConversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &NewConversationInWorktree, w, cx| {
                    this.execute(Command::NewConversationInWorktree, w, cx)
                }))
                .on_action(cx.listener(|this, _: &SwitchHarness, w, cx| {
                    this.execute(Command::SwitchHarness, w, cx)
                }))
                .on_action(cx.listener(|this, _: &RemoveWorktree, w, cx| {
                    this.execute(Command::RemoveWorktree, w, cx)
                }))
                .on_action(cx.listener(|this, _: &SwitchBranch, w, cx| {
                    this.execute(Command::SwitchBranch, w, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &InstallPi, w, cx| {
                        this.execute(Command::InstallPi, w, cx)
                    }),
                )
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
                .on_action(cx.listener(|this, _: &UnarchiveThread, w, cx| {
                    this.execute(Command::UnarchiveThread, w, cx)
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
                .on_action(cx.listener(|this, _: &ToggleAgents, w, cx| {
                    this.execute(Command::ToggleAgents, w, cx)
                }))
                .on_action(cx.listener(|this, _: &CustomizeTheme, w, cx| {
                    this.execute(Command::CustomizeTheme, w, cx)
                }))
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
                .on_action(cx.listener(|this, _: &InsertSlashCommand, w, cx| {
                    this.execute(Command::InsertSlashCommand, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ClearQueuedMessages, w, cx| {
                    this.execute(Command::ClearQueuedMessages, w, cx)
                }))
                .on_action(cx.listener(|this, _: &SendQueuedNow, w, cx| {
                    this.execute(Command::SendQueuedNow, w, cx)
                }))
                .on_action(cx.listener(|this, _: &EditQueuedMessage, w, cx| {
                    this.execute(Command::EditQueuedMessage, w, cx)
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
                .on_action(cx.listener(|this, _: &RegenerateTitle, w, cx| {
                    this.execute(Command::RegenerateTitle, w, cx)
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
                    this.execute(Command::FocusOutput, w, cx)
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
                    cx.listener(|this, _: &SelectNext, w, cx| {
                        this.navigate(Motion::Line(1), w, cx)
                    }),
                )
                .on_action(cx.listener(|this, _: &SelectPrevious, w, cx| {
                    this.navigate(Motion::Line(-1), w, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &SelectFirst, w, cx| this.navigate(Motion::First, w, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &SelectLast, w, cx| this.navigate(Motion::Last, w, cx)),
                )
                .on_action(cx.listener(|this, _: &SelectPageUp, w, cx| {
                    this.navigate(Motion::Page(-1), w, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectPageDown, w, cx| {
                    this.navigate(Motion::Page(1), w, cx)
                }))
                .on_action(cx.listener(|this, _: &CollapseProject, _, cx| this.fold(true, cx)))
                .on_action(cx.listener(|this, _: &ExpandProject, _, cx| this.fold(false, cx)))
                .on_action(cx.listener(|this, _: &OpenSelection, w, cx| this.open_selection(w, cx)))
                .on_action(cx.listener(|this, _: &FocusComposer, w, cx| this.focus_composer(w, cx)))
                .on_action(cx.listener(|this, _: &LeaveEditor, w, cx| {
                    if let Some(panel) = this
                        .active_terminal
                        .and_then(|id| this.terminals.borrow().get(&id).cloned())
                    {
                        panel.read(cx).focus.clone().focus(w, cx);
                    } else {
                        this.focus_region(Region::Thread, w, cx)
                    }
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
                        .overflow_hidden()
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
/// Reopen a saved terminal tab with a new shell in its project, or in its worktree
/// while that is still live. None when the tab's project left the active Space.
fn reopen_terminal(
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
    let pane = cx.new(|cx| TerminalPane::new(tab.project, worktree, space, root, window, cx));
    let panel = cx.new(|cx| {
        let mut panel = WorkspacePanel::new(
            PanelKind::Terminal(tab.terminal),
            state.clone(),
            owner.clone(),
            window,
            cx,
        );
        panel.terminal = Some(pane);
        panel
    });
    terminals.borrow_mut().insert(tab.terminal, panel.clone());
    Some(panel)
}

pub struct WorkspacePanel {
    kind: PanelKind,
    state: Entity<AppState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// The sidebar Output tab scrolls on its own, beside the thread's main scroll.
    output_scroll: ScrollHandle,
    group: Option<WeakEntity<TabGroup>>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    transcript: Option<Entity<conversation::TranscriptView>>,
    terminal: Option<Entity<TerminalPane>>,
    browser: Option<Entity<BrowserPane>>,
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
            cx.on_focus_in(&focus, window, |_, _, cx| cx.notify()),
            cx.on_focus_out(&focus, window, |_, _, _, cx| cx.notify()),
            // Moving between the Projects list and its native search input stays
            // inside the pane, but must still move the row's keyboard focus cue.
            cx.on_focus(&focus, window, |_, _, cx| cx.notify()),
            cx.on_blur(&focus, window, |_, _, cx| cx.notify()),
            cx.observe_window_activation(window, |_, _, cx| cx.notify()),
            cx.subscribe(&state, |panel, _, event: &PresentationChange, cx| {
                let affected = match (*event, panel.kind) {
                    (
                        PresentationChange::Draft(id) | PresentationChange::Thread(id),
                        PanelKind::Thread(thread),
                    ) => id == thread,
                    (PresentationChange::Diff(key), PanelKind::Thread(thread)) => {
                        let state = panel.state.read(cx);
                        state
                            .snapshot
                            .threads
                            .iter()
                            .find(|candidate| candidate.id == thread)
                            .and_then(|candidate| state.working_tree_of(candidate))
                            .is_some_and(|(tree, _)| tree == key)
                    }
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
            let entity_id = cx.entity().entity_id();
            subscriptions.push(cx.on_focus_in(&focus, window, move |_, window, cx| {
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.thread_focused(id, entity_id, window, cx)
                });
            }));
        }
        if let PanelKind::Terminal(id) = kind {
            let owner = workspace.clone();
            subscriptions.push(cx.on_focus_in(&focus, window, move |_, window, cx| {
                let owner = owner.clone();
                window.defer(cx, move |window, cx| {
                    let _ = owner.update(cx, |workspace, cx| {
                        if workspace
                            .focused_panel(window, cx)
                            .is_some_and(|panel| panel.read(cx).kind == PanelKind::Terminal(id))
                        {
                            workspace.terminal_focused(id, window, cx);
                            if let Some(pane) = workspace.active_terminal(cx) {
                                pane.update(cx, |pane, cx| pane.focus(window, cx));
                            }
                        }
                    });
                });
            }));
        }
        if let PanelKind::Browser(id) = kind {
            let owner = workspace.clone();
            subscriptions.push(cx.on_focus_in(&focus, window, move |_, _, cx| {
                let owner = owner.clone();
                cx.defer(move |cx| {
                    let _ = owner.update(cx, |workspace, cx| workspace.browser_focused(id, cx));
                });
            }));
        }
        Self {
            kind,
            state,
            workspace,
            focus,
            scroll: ScrollHandle::new(),
            output_scroll: ScrollHandle::new(),
            group: None,
            bounds: Rc::new(Cell::new(None)),
            transcript,
            terminal: None,
            browser: None,
            _subscriptions: subscriptions,
        }
    }
    fn panel_title(&self, cx: &App) -> String {
        match self.kind {
            PanelKind::Projects => "Projects".into(),
            PanelKind::Welcome => "Threads".into(),
            PanelKind::Browser(_) => self
                .browser
                .as_ref()
                .map(|pane| pane.read(cx).title.clone())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| "Browser".into()),
            PanelKind::Terminal(_) => {
                let (project, worktree) = self
                    .terminal
                    .as_ref()
                    .map(|pane| {
                        let pane = pane.read(cx);
                        (Some(pane.project_id), pane.worktree)
                    })
                    .unwrap_or((None, None));
                let state = self.state.read(cx);
                let name = state
                    .snapshot
                    .projects
                    .iter()
                    .find(|candidate| Some(candidate.id) == project)
                    .map(|project| project.name.as_str())
                    .unwrap_or("Project");
                match worktree.and_then(|id| state.worktree(id)) {
                    Some(worktree) => format!("Terminal — {name} · {}", worktree.codename),
                    None => format!("Terminal — {name}"),
                }
            }
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
        matches!(self.kind, PanelKind::Thread(_) | PanelKind::Browser(_))
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = &self.terminal {
            terminal.update(cx, |terminal, cx| terminal.set_visible(false, cx));
            let owner = self.workspace.clone();
            // Do not re-lease this panel: visibility dumps every live dock leaf.
            window.defer(cx, move |window, cx| {
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.sync_terminal_visibility(window, cx)
                });
            });
        }
        if !active {
            self.bounds.set(None);
        }
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
        self.bounds.set(None);
    }
    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.group = None;
        self.bounds.set(None);
        if let Some(terminal) = &self.terminal {
            terminal.update(cx, |terminal, cx| terminal.set_visible(false, cx));
        }
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
        if let PanelKind::Browser(id) = self.kind {
            cx.defer_in(window, move |panel, window, cx| {
                if panel.group.is_none() {
                    let _ = panel.workspace.update(cx, |workspace, cx| {
                        workspace.browser_panel_removed(id, window, cx)
                    });
                }
            });
        }
    }
    fn dump(&self, cx: &App) -> PanelState {
        let mut state = PanelState::new(self.kind.name());
        if let PanelKind::Thread(id) = self.kind {
            state.info = PanelInfo::panel(json!({ "thread": id }));
        }
        if let PanelKind::Terminal(id) = self.kind {
            // Enough to reopen the tab after a restart; see `TerminalTab`.
            let pane = self.terminal.as_ref().map(|pane| pane.read(cx));
            state.info = PanelInfo::panel(json!({
                "terminal": id,
                "project": pane.map(|pane| pane.project_id),
                "worktree": pane.and_then(|pane| pane.worktree),
            }));
        }
        if let PanelKind::Browser(id) = self.kind {
            state.info = PanelInfo::panel(
                json!({ "browser": id, "url": self.browser.as_ref().map(|pane| pane.read(cx).url.clone()) }),
            );
        }
        state
    }
}
impl Panel for WorkspacePanel {
    /// A thread tab shows its activity mark, with a sweeping underline while its agent
    /// works, and a close control on hover; a middle click closes it too. Closing only
    /// closes the tab.
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.panel_title(cx);
        if let PanelKind::Terminal(id) = self.kind {
            let close = self.workspace.clone();
            let middle = close.clone();
            return h_flex()
                .gap_1()
                .items_center()
                .min_w_0()
                .id(SharedString::from(format!("terminal-tab-{id}")))
                .on_aux_click(move |event, window, cx| {
                    if event.is_middle_click() {
                        cx.stop_propagation();
                        let _ = middle.update(cx, |workspace, cx| {
                            workspace.close_terminal_tab(id, window, cx)
                        });
                    }
                })
                .child(title)
                .child(
                    Button::new(SharedString::from(format!("close-terminal-{id}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Close tab")
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let _ = close.update(cx, |workspace, cx| {
                                workspace.close_terminal_tab(id, window, cx)
                            });
                        }),
                )
                .into_any_element();
        }
        let PanelKind::Thread(id) = self.kind else {
            return title.into_any_element();
        };
        let state = self.state.read(cx);
        let activity = state.thread_activity(id);
        let titling = state.is_titling(id);
        let group = SharedString::from(format!("thread-tab-{id}"));
        let (middle, close) = (self.workspace.clone(), self.workspace.clone());
        let selector = group.clone();
        h_flex()
            .id(group.clone())
            .debug_selector(move || selector.to_string())
            .group(group.clone())
            .relative()
            .items_center()
            .gap_1()
            .min_w_0()
            .on_aux_click(move |event, window, cx| {
                if event.is_middle_click() {
                    cx.stop_propagation();
                    let _ = middle.update(cx, |workspace, cx| {
                        workspace.close_thread_tab(id, window, cx)
                    });
                }
            })
            .children(activity::mark(
                SharedString::from(format!("thread-tab-activity-{id}")),
                activity,
                cx,
            ))
            .map(|tab| match titling {
                true => tab.child(ShimmerText::new(title)),
                false => tab.child(title),
            })
            .child(
                div()
                    .invisible()
                    .group_hover(group, |style| style.visible())
                    .child(
                        Button::new(SharedString::from(format!("close-thread-tab-{id}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Close tab")
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                let _ = close.update(cx, |workspace, cx| {
                                    workspace.close_thread_tab(id, window, cx)
                                });
                            }),
                    ),
            )
            // Inside the tab's clipped inner box, just below the label.
            .when(activity == ThreadActivity::Running, |tab| {
                let selector = format!("thread-tab-sweep-{id}");
                tab.child(
                    div()
                        .debug_selector(move || selector)
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(rems(-0.25))
                        .child(activity::sweep(cx)),
                )
            })
            .into_any_element()
    }
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        // Threads draw their own tab (activity mark, close control); the rest use the name.
        match self.kind {
            PanelKind::Thread(_) | PanelKind::Terminal(_) => None,
            _ => Some(self.panel_title(cx).into()),
        }
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
            PanelKind::Terminal(id) => {
                let owned = |command: Command| {
                    let owner = self.workspace.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.terminal_focused(id, window, cx);
                            workspace.execute(command, window, cx);
                        });
                    }
                };
                menu.item(
                    PopupMenuItem::new(Command::NewTerminal.label())
                        .on_click(owned(Command::NewTerminal)),
                )
                .item(
                    PopupMenuItem::new(Command::RestartTerminal.label())
                        .on_click(owned(Command::RestartTerminal)),
                )
                .item(
                    PopupMenuItem::new(Command::CloseThread.label())
                        .on_click(owned(Command::CloseThread)),
                )
            }
            PanelKind::Thread(id) => {
                // Rename, archive and close act on this tab's thread without selecting it.
                let targeted =
                    |run: fn(&mut Workspace, ThreadId, &mut Window, &mut Context<Workspace>)| {
                        let owner = self.workspace.clone();
                        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                            let _ = owner.update(cx, |workspace, cx| {
                                if workspace.state.read(cx).thread_in_active_space(id) {
                                    run(workspace, id, window, cx);
                                }
                            });
                        }
                    };
                let archived = state
                    .snapshot
                    .threads
                    .iter()
                    .any(|thread| thread.id == id && thread.archived);
                let details = self.workspace.clone();
                menu.item(
                    PopupMenuItem::new(Command::RenameThread.label())
                        .action(Command::RenameThread.action())
                        .disabled(state.rename_thread_reason_for(id).is_some())
                        .on_click(targeted(|workspace, id, window, cx| {
                            workspace.rename_thread(id, window, cx)
                        })),
                )
                .map(|menu| {
                    if archived {
                        menu.item(
                            PopupMenuItem::new(Command::UnarchiveThread.label())
                                .action(Command::UnarchiveThread.action())
                                .disabled(state.unarchive_thread_reason_for(id).is_some())
                                .on_click(targeted(|workspace, id, _, cx| {
                                    workspace.unarchive_thread(id, cx)
                                })),
                        )
                    } else {
                        menu.item(
                            PopupMenuItem::new(Command::ArchiveThread.label())
                                .action(Command::ArchiveThread.action())
                                .disabled(state.archive_thread_reason_for(id).is_some())
                                .on_click(targeted(|workspace, id, window, cx| {
                                    workspace.archive_thread(id, window, cx)
                                })),
                        )
                    }
                })
                .item(
                    PopupMenuItem::new(Command::CloseThread.label())
                        .action(Command::CloseThread.action())
                        .on_click(targeted(|workspace, id, window, cx| {
                            workspace.close_thread_tab(id, window, cx)
                        })),
                )
                .item(
                    PopupMenuItem::new("Thread details").on_click(move |_, window, cx| {
                        let _ = details.update(cx, |workspace, cx| {
                            if !workspace.state.read(cx).thread_in_active_space(id) {
                                return;
                            }
                            workspace.toggle_thread_details(id, window, cx)
                        });
                    }),
                )
                .separator()
                .menu(Command::SwitchBranch.label(), Box::new(SwitchBranch))
                .separator()
                .menu("New conversation", Box::new(NewConversation))
                .menu(
                    Command::NewConversationInWorktree.label(),
                    Box::new(NewConversationInWorktree),
                )
                .menu("New Factory thread", Box::new(NewFactoryThread))
            }
            PanelKind::Browser(_) => menu
                .menu(Command::NewBrowser.label(), Box::new(NewBrowser))
                .menu(Command::BrowserAddress.label(), Box::new(BrowserAddress))
                .menu(Command::BrowserReload.label(), Box::new(BrowserReload))
                .menu(Command::CloseThread.label(), Box::new(CloseThread)),
            PanelKind::Welcome => menu
                .menu("New conversation", Box::new(NewConversation))
                .menu(
                    Command::NewConversationInWorktree.label(),
                    Box::new(NewConversationInWorktree),
                )
                .menu("New Factory thread", Box::new(NewFactoryThread)),
            PanelKind::Projects => menu
                .menu("Add project…", Box::new(AddProject))
                .menu("Show archived", Box::new(ShowArchived)),
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
        let pane_focused = window.is_window_active() && self.focus.contains_focused(window, cx);
        let list_focused = window.is_window_active() && self.focus.is_focused(window);
        let content = self.workspace.upgrade().map(|owner| {
            let workspace = owner.read(cx);
            let state = self.state.read(cx);
            let scroll = &self.scroll;
            match self.kind {
                PanelKind::Projects => {
                    let (slide, animating) = if cx.reduce_motion() {
                        (0., false)
                    } else {
                        workspace.space_slide.sample(Instant::now())
                    };
                    if animating {
                        window.request_animation_frame();
                    }
                    sidebar::render(
                        state,
                        &self.workspace,
                        sidebar::SidebarPresentation {
                            cursor: workspace.cursor.filter(|_| list_focused),
                            hovered: workspace.hovered_sidebar_row,
                            slide,
                            neighbor: workspace.space_slide.neighbor,
                            neighbor_query: &workspace.space_slide.neighbor_query,
                            neighbor_scroll: workspace.space_slide.neighbor_scroll.as_ref(),
                        },
                        &workspace.thread_search,
                        scroll,
                        cx,
                    )
                }
                PanelKind::Welcome => conversation::welcome(state, &self.workspace, cx),
                PanelKind::Browser(_) => self
                    .browser
                    .as_ref()
                    .map(|pane| pane.clone().into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
                PanelKind::Terminal(_) => self
                    .terminal
                    .as_ref()
                    .map(|terminal| terminal.clone().into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
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
                                    &self.output_scroll,
                                    transcript,
                                    cx,
                                )
                            } else {
                                div().into_any_element()
                            }
                        }
                        ThreadKind::Factory => {
                            thread_sidebar::SidebarLayout::new(state, id, &self.workspace, cx).wrap(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(if factory::launch_for(state, id).is_some() {
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
                                    })
                                    .into_any_element(),
                                thread_sidebar::render(
                                    state,
                                    id,
                                    false,
                                    &self.workspace,
                                    &self.output_scroll,
                                    cx,
                                ),
                            )
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
            .when(pane_focused, |pane| {
                pane.child(
                    div()
                        .id("pane-focus")
                        .test_support()
                        .absolute()
                        .inset_0()
                        .border_1()
                        .border_color(cx.theme().input),
                )
            })
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

/// Preserve the kept leaves and repair their containers after removing the
/// others (terminals of another Space, unrestorable tabs). Active tabs follow the nearest surviving tab; split sizes keep the
/// same association with their surviving children.
fn filter_layout(mut layout: DockAreaState, keep: &impl Fn(&PanelState) -> bool) -> DockAreaState {
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
/// Terminal tabs persist with their project so they reopen after a restart; a leaf
/// that cannot name its project (an older layout) is dropped.
fn durable_layout(layout: DockAreaState) -> DockAreaState {
    filter_layout(layout, &|panel| {
        panel.panel_name != "Terminal" || terminal_tab(panel).is_some()
    })
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
        // A leaf from before terminal tabs were saved names no project.
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
