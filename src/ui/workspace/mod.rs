mod actions;
pub(super) mod activity;
mod browser;
mod dialogs;
mod execute;
mod focus;
mod layout;
mod navigation;
mod neovim;
pub mod overlays;
mod panel;
pub mod projects;
mod rename;
mod sidebar_tabs;
mod space;
mod terminal;
mod terminal_tabs;
mod thread_panels;
pub(super) mod thread_sidebar;
mod title_bar;

use super::browser::{BrowserPane, OpenBrowserUrl};
use super::{
    commands::{self, *},
    conversation, factory,
    settings::{DialogKind, SurfaceContent, SurfaceOutcome},
    terminal::{NeovimExited, NeovimTarget, TerminalId, TerminalPane},
};
use crate::{
    app::{AppState, NoticeLevel, PresentationChange, SidebarTab, ThreadActivity},
    model::{PaneDirection, ProjectId, SpaceId, ThreadId, ThreadKind, WorktreeId},
};
use browser::{Browsers, browser_of};
use gpui_kit::{
    component::{
        ActiveTheme, IconName, Sizable, TITLE_BAR_HEIGHT, TitleBar, WindowExt,
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
use layout::*;
use navigation::*;
use panel::*;
use projects::Row;
use rename::*;
use serde_json::json;
use space::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};
use terminal::ThreadTerminal;
pub(super) use terminal::thread_terminal_location;
use terminal_tabs::*;
use thread_panels::*;
use title_bar::{WorkspaceDockSkin, WorkspaceTitle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Motion {
    Line(isize),
    Page(isize),
    First,
    Last,
}

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Projects,
    Welcome,
    Thread(ThreadId),
    Terminal(TerminalId),
    Browser(crate::browser::BrowserId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RenameTarget {
    Project(ProjectId),
    Space(SpaceId),
    Thread(ThreadId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RenameSurface {
    Sidebar,
    Tab,
    Header,
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
type TerminalPanels = Rc<RefCell<BTreeMap<TerminalId, Entity<WorkspacePanel>>>>;
type ThreadPanels = Rc<RefCell<BTreeMap<ThreadId, Entity<WorkspacePanel>>>>;

pub struct Workspace {
    pub state: Entity<AppState>,
    pub(super) dock: Entity<DockArea>,
    projects: Entity<WorkspacePanel>,
    threads: ThreadPanels,
    terminals: TerminalPanels,
    pub(super) thread_terminals: BTreeMap<ThreadId, ThreadTerminal>,
    pub(super) browsers: Browsers,
    pub(super) sidebar_sizes:
        Rc<RefCell<BTreeMap<ThreadId, Entity<gpui_kit::component::resizable::ResizableState>>>>,
    pub(super) composer_sizes:
        BTreeMap<ThreadId, Entity<gpui_kit::component::resizable::ResizableState>>,
    pub(super) changes_lists: super::changes::ChangesLists,
    live_layouts: BTreeMap<SpaceId, DockAreaState>,
    active_terminal: Option<TerminalId>,
    space_terminals: BTreeMap<SpaceId, TerminalId>,
    pub(super) terminal_confirmation: bool,
    inline_rename: Option<InlineRename>,
    welcome: Rc<RefCell<Option<Entity<WorkspacePanel>>>>,
    rebuilding: bool,
    focus: FocusHandle,
    pub cursor: Option<Row>,
    hovered_sidebar_row: Option<Row>,
    space_swipe: projects::SpaceSwipe,
    space_slide: SpaceSlide,
    space_swipe_idle: Option<Task<()>>,
    space_direction: Option<f32>,
    thread_search: Entity<InputState>,
    pub palette: Option<Entity<Palette>>,
    pub dialog: Option<Entity<SurfaceContent>>,
    surface_drafts: Vec<Entity<SurfaceContent>>,
    background_worktree_request: Option<uuid::Uuid>,
    pub(super) commit_panel_thread: Option<ThreadId>,
    pub(super) theme_editor: Option<Entity<crate::ui::theme::editor::ThemeEditor>>,
    agent_activity: BTreeMap<ThreadId, crate::ui::network::agents::Follow>,
    pub(super) editor: Option<Entity<crate::ui::conversation::editor::PromptEditor>>,
    pub(super) editor_selection: Option<crate::model::DraftKey>,
    pub(super) editor_override: Option<(crate::model::DraftKey, String)>,
    pub image_preview: Option<crate::model::ImageAttachment>,
    pub expanded: std::collections::HashSet<String>,
    pub extension: Option<overlays::ExtensionForm>,
    pub tool_detail: Option<(crate::model::ThreadId, String)>,
    pub(super) tool_detail_text: overlays::ToolDetailText,
    pub factory_form: Option<Entity<crate::ui::factory::form::FactoryThreadForm>>,
    pub(super) model_picker: Option<conversation::ModelPicker>,
    pub(super) effort_picker: Option<conversation::EffortPicker>,
    pub(super) permission_mode_picker: Option<conversation::PermissionModePicker>,
    pub(super) detail_surface: Option<overlays::SurfaceKey>,
    pub(super) quit_requested: bool,
    layout_save: Option<Task<()>>,
    presented_space: SpaceId,
    preparing_space_change: bool,
    seen_notice_sequence: u64,
    reopening: Option<Region>,
    dock_measurement: Rc<Cell<Option<Bounds<Pixels>>>>,
    presented_thread: Option<ThreadId>,
    unrevealed_thread: Option<ThreadId>,
    pub(super) state_revision: u64,
    released_open_threads: Vec<ThreadId>,
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
            thread_terminals: BTreeMap::new(),
            browsers,
            sidebar_sizes: Rc::default(),
            composer_sizes: BTreeMap::new(),
            changes_lists: Rc::default(),
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
            tool_detail_text: Default::default(),
            factory_form: None,
            quit_requested: false,
            cursor: None,
            hovered_sidebar_row: None,
            space_swipe: Default::default(),
            space_slide: SpaceSlide::default(),
            space_swipe_idle: None,
            space_direction: None,
            thread_search,
            inline_rename: None,
            model_picker: None,
            effort_picker: None,
            permission_mode_picker: None,
            presented_thread: None,
            unrevealed_thread: None,
            detail_surface: None,
            palette: None,
            dialog: None,
            surface_drafts: Vec::new(),
            background_worktree_request: None,
            commit_panel_thread: None,
            theme_editor: None,
            agent_activity: BTreeMap::new(),
            layout_save: None,
            presented_space,
            preparing_space_change: false,
            seen_notice_sequence: 0,
            reopening: None,
            dock_measurement: Rc::new(Cell::new(None)),
            state_revision: 0,
            released_open_threads: Vec::new(),
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
            workspace
                .dock
                .update(cx, |dock, cx| dock.load(layout, window, cx))
        });
        match restored {
            Some(Ok(())) => {}
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
                    let rows = projects::rows(workspace.state.read(cx), &query);
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
                workspace.state_revision += 1;
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
                workspace.state_revision += 1;
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
                        workspace.sync_sidebar_terminals(window, cx);
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
                    workspace.sync_sidebar_terminals(window, cx);
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
}

pub struct WorkspaceRoot {
    workspace: Entity<Workspace>,
    layers_cleared_for_shutdown: bool,
}

impl WorkspaceRoot {
    pub fn new(workspace: Entity<Workspace>) -> Self {
        Self {
            workspace,
            layers_cleared_for_shutdown: false,
        }
    }
}

impl Render for WorkspaceRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.workspace.update(cx, |workspace, cx| {
            workspace.sync_terminal_visibility(window, cx)
        });
        if self.workspace.read(cx).state.read(cx).shutting_down && !self.layers_cleared_for_shutdown
        {
            self.layers_cleared_for_shutdown = true;
            window.defer(cx, |window, cx| {
                window.close_all_dialogs(cx);
                window.close_sheet(cx);
                window.clear_notifications(cx);
            });
        }
        div().relative().size_full().child(self.workspace.clone())
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let state = self.state.read(cx);
        let dock_measurement = self.dock_measurement.clone();
        let geometry_owner = owner.clone();
        let root = v_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);
        let mut root = Self::bind_actions(root, cx).child(
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
                                        workspace.normalize_layout(workspace.reopening, window, cx);
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

#[cfg(all(test, feature = "runtime-qa"))]
#[path = "shutdown_ui_tests.rs"]
mod shutdown_ui_tests;
