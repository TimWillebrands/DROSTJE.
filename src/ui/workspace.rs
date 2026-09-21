use super::{
    commands::{self, *},
    conversation, factory,
    settings::{DialogKind, SurfaceContent, SurfaceOutcome},
    sidebar::{self, Row},
    terminal::{TerminalId, TerminalPane},
    thread_sidebar,
    title_bar::{WorkspaceDockSkin, WorkspaceTitle},
};
use crate::{
    app::{AppState, NoticeLevel, PresentationChange, SidebarTab},
    model::{PaneDirection, ProjectId, SpaceId, ThreadId, ThreadKind},
};
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
        spinner::Spinner,
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
/// What one dock panel shows. Threads and ephemeral terminals can be tabbed,
/// split and dragged. Welcome holds the center only while no content is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Projects,
    Welcome,
    Thread(ThreadId),
    Terminal(TerminalId),
}
impl PanelKind {
    pub fn region(self) -> Region {
        match self {
            Self::Projects => Region::Projects,
            Self::Welcome | Self::Thread(_) | Self::Terminal(_) => Region::Thread,
        }
    }
    /// Persisted layout name; never change it.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Projects => "Projects",
            Self::Welcome => "Welcome",
            Self::Thread(_) => "Thread",
            Self::Terminal(_) => "Terminal",
        }
    }
    fn context(self) -> &'static str {
        match self {
            Self::Projects => "List ProjectsList",
            Self::Welcome | Self::Thread(_) => "Thread",
            Self::Terminal(_) => "TerminalPane",
        }
    }
    fn debug_name(self) -> String {
        match self {
            Self::Thread(id) => format!("Thread:{id}"),
            Self::Terminal(id) => format!("Terminal:{id}"),
            other => other.name().to_owned(),
        }
    }
    fn matches(self, state: &PanelState) -> bool {
        state.panel_name == self.name()
            && match self {
                Self::Thread(id) => thread_of(state) == Some(id),
                Self::Terminal(id) => terminal_of(state) == Some(id),
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
type TerminalPanels = Rc<RefCell<BTreeMap<TerminalId, Entity<WorkspacePanel>>>>;
type ThreadPanels = Rc<RefCell<BTreeMap<ThreadId, Entity<WorkspacePanel>>>>;
/// Version 4 dropped the bottom Output dock; Output is a thread sidebar tab.
const LAYOUT_VERSION: usize = 4;
const OUTDATED_LAYOUT: &str = "Layout was saved by another version";

#[derive(Clone, Copy)]
enum SpaceChange {
    Switch(SpaceId),
    Move(ProjectId, SpaceId),
    Delete(SpaceId),
}

pub struct Workspace {
    pub state: Entity<AppState>,
    pub(super) dock: Entity<DockArea>,
    projects: Entity<WorkspacePanel>,
    /// Shared with the panel registry so a restored layout rebuilds into the same map.
    threads: ThreadPanels,
    terminals: TerminalPanels,
    /// Live layouts may refer to retained terminals; durable layouts never do.
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
    /// Two-finger swipe over Projects, reduced to Space steps by the sidebar.
    pub(super) space_swipe: super::sidebar::SpaceSwipe,
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
                // Only in-memory IDs resolve. A stale reference never spawns a shell.
                let panel = terminal_of(ctx.state())
                    .and_then(|id| terminals.borrow().get(&id).cloned())
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
        let presented_space = state.read(cx).active_space;
        let thread_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search threads…"));
        let mut workspace = Self {
            state,
            dock,
            projects,
            threads,
            terminals,
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
            thread_search,
            model_picker: None,
            effort_picker: None,
            permission_mode_picker: None,
            presented_thread: None,
            unrevealed_thread: None,
            detail_surface: None,
            palette: None,
            dialog: None,
            layout_save: None,
            presented_space,
            preparing_space_change: false,
            seen_notice_sequence: 0,
            reopening: None,
            dock_measurement: Rc::new(Cell::new(None)),
            _subscriptions: Vec::new(),
        };
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
                        workspace.sync_space(window, cx);
                        workspace.sync_session(window, cx);
                        workspace.sync_factory_form(window, cx);
                        workspace.sync_model_picker(window, cx);
                        workspace.sync_effort_picker(window, cx);
                        workspace.sync_permission_mode_picker(window, cx);
                        workspace.sync_selection_panels(window, cx);
                        workspace.sync_diff_interest(cx);
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
                    workspace.sync_diff_interest(cx);
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
        workspace.sync_diff_interest(cx);
        cx.defer_in(window, |workspace, window, cx| {
            workspace.show_notices(window, cx)
        });
        workspace.prepare_projects_cursor(cx);
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

    pub(super) fn switch_space(
        &mut self,
        id: SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if id == self.state.read(cx).active_space {
            return Ok(());
        }
        self.change_space(SpaceChange::Switch(id), window, cx)
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
        if matches!(change, SpaceChange::Move(..)) {
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
        self.layout_save.take();
        self.hide_terminals(cx);
        self.live_layouts.insert(
            self.presented_space,
            filter_layout(self.dock.read(cx).dump(cx), &|panel| {
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
            let layout: DockAreaState = saved?;
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
            self.reset_layout(window, cx);
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
        self.open_thread_ids(cx).len() + self.active_space_terminals(cx).len()
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
        let id = TerminalId::new();
        let owner = cx.entity().downgrade();
        let state = self.state.clone();
        let space = self.presented_space;
        let anchor = self
            .thread_target(cx)
            .and_then(|panel| panel.read(cx).group.clone())
            .and_then(|group| group.upgrade())
            .map(|group| group.read(cx).node());
        let pane = cx.new(|cx| TerminalPane::new(project.id, space, project.root, window, cx));
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
            _ => None,
        };
        if let Some(target) = target {
            self.focus_panel(target, window, cx);
        }
    }
    pub(super) fn close_active_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            dock.set_dock_size(DockPlacement::Left, rem * 16., window, cx);
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
        panels
    }
    fn thread_target(&self, cx: &App) -> Option<Entity<WorkspacePanel>> {
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
        if open.is_empty() && self.active_space_terminals(cx).is_empty() {
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
                    PanelKind::Thread(_) | PanelKind::Terminal(_)
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
    fn thread_focused(
        &mut self,
        id: ThreadId,
        panel: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn navigation_blocked(&self, window: &mut Window, cx: &mut App) -> bool {
        self.quit_requested
            || self.preparing_space_change
            || self.terminal_confirmation
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
        let live = self.dock.read(cx).dump(cx);
        self.live_layouts.insert(self.presented_space, live.clone());
        let layout = json!(durable_layout(live));
        if self.state.read(cx).space_layout() != Some(&layout) {
            self.state.update(cx, |state, cx| {
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
    /// Show `tab` in a thread's sidebar (tab click, Focus output).
    pub(super) fn show_sidebar_tab(
        &mut self,
        id: ThreadId,
        tab: SidebarTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            state.show_thread_sidebar(id, tab);
            cx.emit(PresentationChange::Thread(id));
        });
        self.sync_diff_interest(cx);
        let panel = self.threads.borrow().get(&id).cloned();
        if let Some(panel) = panel {
            self.focus_panel(panel, window, cx);
        }
    }
    /// Show `tab` in a thread's sidebar, or hide the sidebar when `tab` is on screen.
    fn toggle_sidebar_tab(&mut self, id: ThreadId, tab: SidebarTab, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.toggle_thread_sidebar(id, tab);
            cx.emit(PresentationChange::Thread(id));
        });
        self.sync_diff_interest(cx);
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
            PanelKind::Projects | PanelKind::Welcome => {}
        }
        focus.focus(window, cx);
        if let Some(terminal) = panel.read(cx).terminal.clone() {
            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
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
                | Command::NewFactoryThread
                | Command::NewTerminal
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
            Command::ArchiveThread => self
                .state
                .read(cx)
                .selected_thread
                .map(DialogKind::ArchiveThread),
            Command::Settings => Some(DialogKind::Settings(self.state.read(cx).selected_project)),
            Command::NewConversation
            | Command::NewOmpConversation
            | Command::NewPiConversation
            | Command::NewClaudeCodeConversation
            | Command::NewFactoryThread => {
                self.state.update(cx, |state, cx| {
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
            }
            Ok(())
        });
        match result {
            Ok(()) => {
                let kind = dialog.read(cx).kind.clone();
                super::settings::close_surface(&kind, window, cx);
                cx.notify();
            }
            Err(error) => {
                dialog.update(cx, |dialog, cx| dialog.set_error(&error, cx));
                self.dialog = Some(dialog);
            }
        }
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
    /// The sidebar Output tab scrolls on its own, beside the thread's main scroll.
    output_scroll: ScrollHandle,
    group: Option<WeakEntity<TabGroup>>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    transcript: Option<Entity<conversation::TranscriptView>>,
    terminal: Option<Entity<TerminalPane>>,
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
            _subscriptions: subscriptions,
        }
    }
    fn panel_title(&self, cx: &App) -> String {
        match self.kind {
            PanelKind::Projects => "Projects".into(),
            PanelKind::Welcome => "Threads".into(),
            PanelKind::Terminal(_) => {
                let project = self.terminal.as_ref().map(|pane| pane.read(cx).project_id);
                let name = self
                    .state
                    .read(cx)
                    .snapshot
                    .projects
                    .iter()
                    .find(|candidate| Some(candidate.id) == project)
                    .map(|project| project.name.as_str())
                    .unwrap_or("Project");
                format!("Terminal — {name}")
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
        matches!(self.kind, PanelKind::Thread(_))
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
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut state = PanelState::new(self.kind.name());
        if let PanelKind::Thread(id) = self.kind {
            state.info = PanelInfo::panel(json!({ "thread": id }));
        }
        if let PanelKind::Terminal(id) = self.kind {
            state.info = PanelInfo::panel(json!({ "terminal": id }));
        }
        state
    }
}
impl Panel for WorkspacePanel {
    /// A thread tab shows a spinner while its agent works and a close control on
    /// hover; a middle click closes it too. Closing only closes the tab.
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
        let working = state.active_threads.contains(&id);
        let titling = state.is_titling(id);
        let group = SharedString::from(format!("thread-tab-{id}"));
        let (middle, close) = (self.workspace.clone(), self.workspace.clone());
        let selector = group.clone();
        h_flex()
            .id(group.clone())
            .debug_selector(move || selector.to_string())
            .group(group.clone())
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
            .when(working, |tab| tab.child(Spinner::new().xsmall()))
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
            .into_any_element()
    }
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        // Threads draw their own tab (spinner, close control); the rest use the name.
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
                // The menu belongs to this pane: select its thread before the command runs.
                let owned = |command: Command| {
                    let owner = self.workspace.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        let _ = owner.update(cx, |workspace, cx| {
                            if !workspace.state.read(cx).thread_in_active_space(id) {
                                return;
                            }
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
                    PopupMenuItem::new(Command::CloseThread.label())
                        .disabled(disabled(Command::CloseThread))
                        .on_click(owned(Command::CloseThread)),
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
                .menu("New conversation", Box::new(NewConversation))
                .menu("New Factory thread", Box::new(NewFactoryThread))
            }
            PanelKind::Welcome => menu
                .menu("New conversation", Box::new(NewConversation))
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
                PanelKind::Projects => sidebar::render(
                    state,
                    &self.workspace,
                    workspace.cursor.filter(|_| list_focused),
                    workspace.hovered_sidebar_row,
                    &workspace.thread_search,
                    scroll,
                    cx,
                ),
                PanelKind::Welcome => conversation::welcome(state, &self.workspace, cx),
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
                        ThreadKind::Factory => h_flex()
                            .size_full()
                            .min_h_0()
                            .items_stretch()
                            .child(
                                div().flex_1().min_w_0().h_full().child(
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
                                    },
                                ),
                            )
                            .child(thread_sidebar::render(
                                state,
                                id,
                                false,
                                &self.workspace,
                                &self.output_scroll,
                                cx,
                            ))
                            .into_any_element(),
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

/// Preserve all durable leaves and repair their containers after removing live
/// terminals. Active tabs follow the nearest surviving tab; split sizes keep the
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
            PanelInfo::Tiles { metas } => {
                *metas = retained
                    .iter()
                    .filter_map(|(index, _)| metas.get(*index).copied())
                    .collect();
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
fn durable_layout(layout: DockAreaState) -> DockAreaState {
    filter_layout(layout, &|panel| panel.panel_name != "Terminal")
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
        count("Projects") == 1 && count("Welcome") <= 1,
        "Layout must contain exactly one Projects panel"
    );
    anyhow::ensure!(
        names
            .iter()
            .all(|name| matches!(*name, "Projects" | "Welcome" | "Thread")),
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
}
