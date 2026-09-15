use super::{
    commands::{self, *},
    conversation, factory,
    settings::{Dialog, DialogKind},
    sidebar::{self, Row},
};
use crate::app::AppState;
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        dock::{
            BasePanel, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSkin,
            Panel, PanelEvent, PanelInfo, PanelState, TabGroup, panel_handle, register_panel,
        },
        h_flex,
        input::{InputEvent, InputState},
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
    Conversation,
    Factory,
    Output,
}
impl Region {
    pub const ALL: [Self; 4] = [
        Self::Projects,
        Self::Conversation,
        Self::Factory,
        Self::Output,
    ];
    pub fn index(self) -> usize {
        match self {
            Self::Projects => 0,
            Self::Conversation => 1,
            Self::Factory => 2,
            Self::Output => 3,
        }
    }
    pub fn name(self) -> &'static str {
        commands::region_action(self)
    }
    fn context(self) -> &'static str {
        match self {
            Self::Projects => "List ProjectsList",
            Self::Factory | Self::Output => "List",
            Self::Conversation => "Conversation",
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
    pub palette: Option<Palette>,
    pub dialog: Option<Dialog>,
    pub editor: Option<Entity<crate::neovim::NvimView>>,
    pub(super) opening_editor: bool,
    pub(super) editor_selection: Option<crate::neovim::DraftKey>,
    pub(super) editor_switching: bool,
    pub(super) editor_override: Option<(crate::neovim::DraftKey, String)>,
    pub image_preview: Option<crate::model::ImageAttachment>,
    pub expanded: std::collections::HashSet<String>,
    pub handoff_message: Option<(crate::model::ThreadId, usize, String)>,
    pub extension: Option<super::overlays::ExtensionForm>,
    pub tool_detail: Option<(crate::model::ThreadId, String)>,
    pub factory_form: Option<super::factory_form::FactoryForm>,
    pub(super) quit_requested: bool,
    scrolls: [ScrollHandle; 4],
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
        let (dock, skin) = DockSkin::dock_area("drost-workspace", Some(1), window, cx);
        let saved = state.read(cx).snapshot.settings.get("layout").cloned();
        let mut workspace = Self {
            state,
            dock,
            _skin: skin,
            panels,
            focus,
            editor: None,
            opening_editor: false,
            editor_selection: None,
            editor_switching: false,
            editor_override: None,
            image_preview: None,
            expanded: std::collections::HashSet::new(),
            handoff_message: None,
            extension: None,
            tool_detail: None,
            factory_form: None,
            quit_requested: false,
            cursor: None,
            palette: None,
            dialog: None,
            scrolls: std::array::from_fn(|_| ScrollHandle::new()),
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
                        state.notice(format!("Only the saved layout was reset: {error:#}"));
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
                cx.notify();
            },
        ));
        workspace._subscriptions.push(cx.subscribe_in(
            &workspace.dock,
            window,
            |workspace, dock, event: &DockEvent, _, cx| {
                if matches!(event, DockEvent::LayoutChanged)
                    && !workspace.state.read(cx).shutting_down
                {
                    let layout = dock.read(cx).dump(cx);
                    workspace.state.update(cx, |state, cx| {
                        state.set_setting("layout", json!(layout));
                        cx.notify();
                    });
                }
            },
        ));
        workspace.focus_region(Region::Projects, window, cx);
        workspace.start_editor(window, cx);
        let closing = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = closing.update(cx, |workspace, cx| workspace.quit_review(window, cx));
            false
        });
        workspace
    }

    pub fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = |index: usize, cx: &App| {
            DockLayout::tabs().panel_view(panel_handle(self.panels[index].clone()), cx)
        };
        let center = tabs(1, cx);
        let left = tabs(0, cx);
        let right = tabs(2, cx);
        let bottom = tabs(3, cx);
        self.dock.update(cx, |dock, cx| {
            dock.set_version(Some(1), cx);
            dock.set_center(center, window, cx);
            for (placement, layout, width) in [
                (DockPlacement::Left, left, 260.),
                (DockPlacement::Right, right, 340.),
                (DockPlacement::Bottom, bottom, 240.),
            ] {
                dock.set_dock(placement, layout, window, cx);
                dock.set_dock_size(placement, px(width), window, cx);
                dock.set_dock_collapsible(placement, true, window, cx);
            }
            if dock.is_dock_open(DockPlacement::Bottom) {
                dock.toggle_dock(DockPlacement::Bottom, window, cx);
            }
        });
        let layout = self.dock.read(cx).dump(cx);
        self.state.update(cx, |state, cx| {
            state.set_setting("layout", json!(layout));
            cx.notify();
        });
        cx.notify();
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
            .unwrap_or(Region::Conversation)
    }
    pub fn focus_region(&mut self, region: Region, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = &self.factory_form {
            form.focus.focus(window, cx);
            return;
        }
        if self.dialog.is_some() || self.palette.is_some() {
            self.dismiss(window, cx);
        }
        let layout = self.dock.read(cx).dump(cx);
        for (placement, dock) in [
            (DockPlacement::Left, layout.left_dock),
            (DockPlacement::Right, layout.right_dock),
            (DockPlacement::Bottom, layout.bottom_dock),
        ] {
            if dock
                .as_ref()
                .is_some_and(|dock| contains_panel(dock.panel(), region.name()) && !dock.open())
            {
                self.dock
                    .update(cx, |dock, cx| dock.toggle_dock(placement, window, cx));
            }
        }
        let panel = self.panels[region.index()].read(cx);
        let group = panel.group.clone();
        let focus = panel.focus.clone();
        if let Some(group) = group {
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
    fn spatial_focus(&mut self, direction: Region, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.focused_region(window, cx);
        let Some(origin) = self.panels[current.index()].read(cx).bounds.get() else {
            return;
        };
        let origin = origin.center();
        let layout = self.dock.read(cx).dump(cx);
        let next = Region::ALL
            .into_iter()
            .filter(|region| *region != current)
            .filter_map(|region| {
                if [&layout.left_dock, &layout.right_dock, &layout.bottom_dock]
                    .into_iter()
                    .flatten()
                    .any(|dock| !dock.open() && contains_panel(dock.panel(), region.name()))
                {
                    return None;
                }
                let panel = self.panels[region.index()].read(cx);
                let group = panel.group.as_ref()?.upgrade()?;
                if group.read(cx).active_panel(cx)?.panel_name(cx) != region.name() {
                    return None;
                }
                let center = panel.bounds.get()?.center();
                let dx = f32::from(center.x - origin.x);
                let dy = f32::from(center.y - origin.y);
                let (forward, cross) = match direction {
                    Region::Projects => (-dx, dy),
                    Region::Factory => (dx, dy),
                    Region::Conversation => (-dy, dx),
                    Region::Output => (dy, dx),
                };
                (forward > 1.).then_some((region, forward + cross.abs() * 2.))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(region, _)| region);
        if let Some(next) = next {
            self.focus_region(next, window, cx);
        }
    }
    fn toggle_dock(
        &mut self,
        placement: DockPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock
            .update(cx, |dock, cx| dock.toggle_dock(placement, window, cx));
    }

    pub fn execute(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).shutting_down {
            return;
        }
        if let Some(reason) = command.disabled_reason(self.state.read(cx)) {
            self.state.update(cx, |state, cx| {
                state.notice(reason);
                cx.notify();
            });
            return;
        }
        let kind = match command {
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
            Command::RenameConversation => self
                .state
                .read(cx)
                .selected_thread
                .map(DialogKind::RenameThread),
            Command::ArchiveConversation => self
                .state
                .read(cx)
                .selected_thread
                .map(DialogKind::ArchiveThread),
            Command::Settings => Some(DialogKind::Settings(self.state.read(cx).selected_project)),
            Command::NewConversation => {
                self.state.update(cx, |state, cx| {
                    if let Err(error) = state.new_thread() {
                        state.notice(format!("{error:#}"));
                    }
                    cx.notify();
                });
                self.focus_region(Region::Conversation, window, cx);
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
            Command::SendToFactory => {
                self.open_factory_form(super::factory_form::FactoryFormKind::Task, window, cx);
                None
            }
            Command::OpenFactoryFolder => {
                self.open_factory_form(super::factory_form::FactoryFormKind::Folder, window, cx);
                None
            }
            Command::DispatchAzureWorkItem => {
                self.open_factory_form(super::factory_form::FactoryFormKind::Azure, window, cx);
                None
            }
            Command::RefreshFactoryRuns => {
                self.state.update(cx, |state, cx| {
                    state.refresh_factory_runs();
                    cx.notify();
                });
                None
            }
        };
        if let Some(kind) = kind {
            self.open_dialog(kind, window, cx);
        }
        cx.notify();
    }
    fn open_dialog(&mut self, kind: DialogKind, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
        self.dialog = Some(
            self.state
                .update(cx, |state, cx| Dialog::new(kind, state, window, cx)),
        );
        cx.notify();
    }
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.image_preview = None;
        self.tool_detail = None;
        if self.factory_form.is_some() {
            self.close_factory_form(window, cx);
        }
        if self.extension.is_some() {
            self.close_extension(true, window, cx);
        }
        let focus = self
            .palette
            .take()
            .and_then(|palette| palette.previous_focus)
            .or_else(|| self.dialog.take().and_then(|dialog| dialog.previous_focus));
        if let Some(focus) = focus {
            focus.focus(window, cx);
        }
        cx.notify();
    }
    pub fn confirm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.extension.is_some() {
            self.confirm_extension(window, cx);
            return;
        }
        if self.factory_form.is_some() {
            self.review_factory_task(window, cx);
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
                if let Some(focus) = dialog.previous_focus {
                    focus.focus(window, cx);
                }
            }
            Ok(false) => self.dialog = Some(dialog),
            Err(error) => {
                let mut dialog = dialog;
                dialog.error = Some(format!("{error:#}"));
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
                        if let Some(path) = paths.into_iter().next() {
                            if let Some(dialog) = &workspace.dialog {
                                if let Some(field) =
                                    dialog.fields.iter().find(|field| field.key == "path")
                                {
                                    field.input.update(cx, |input, cx| {
                                        input.set_value(
                                            path.to_string_lossy().into_owned(),
                                            window,
                                            cx,
                                        )
                                    });
                                }
                            }
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
        if let Some(palette) = &mut self.palette {
            let count = palette.items(self.state.read(cx), cx).len();
            palette.selected = (palette.selected as isize + delta)
                .clamp(0, count.saturating_sub(1) as isize) as usize;
            cx.notify();
            return;
        }
        let region = self.focused_region(window, cx);
        if region == Region::Conversation {
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
                    session.list.scroll_by(px(delta as f32 * 36.));
                }
                cx.notify();
                return;
            }
        }
        if region == Region::Factory {
            let state = self.state.read(cx);
            let launches: Vec<_> = state
                .snapshot
                .launches
                .iter()
                .filter(|launch| Some(launch.project_id) == state.selected_project)
                .map(|launch| launch.id)
                .collect();
            if !launches.is_empty() {
                let index = launches
                    .iter()
                    .position(|id| Some(*id) == state.selected_launch)
                    .unwrap_or(0);
                let next = if endpoint {
                    if delta < 0 { 0 } else { launches.len() - 1 }
                } else {
                    (index as isize + delta).clamp(0, launches.len() as isize - 1) as usize
                };
                self.state.update(cx, |state, cx| {
                    state.select_factory_launch(launches[next], cx)
                });
                self.scrolls[region.index()].scroll_to_item(next);
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
                    (offset.y - px(delta as f32 * 36.)).min(px(0.)),
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
        if self.palette.is_some() {
            self.confirm_palette(window, cx);
            return;
        }
        if self.focused_region(window, cx) == Region::Factory {
            if self.state.read(cx).selected_launch.is_some() {
                self.focus_region(Region::Output, window, cx);
            }
            return;
        }
        if self.focused_region(window, cx) != Region::Projects {
            return;
        }
        if let Some(row) = self.cursor {
            self.select_row(row, true, window, cx);
            if matches!(row, Row::Thread(_)) {
                self.focus_region(Region::Conversation, window, cx);
            }
        }
    }
    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
        let previous_focus = window.focused(cx);
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Commands, projects, conversations…")
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |workspace, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if let Some(palette) = &mut workspace.palette {
                        palette.selected = 0;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => workspace.confirm_palette(window, cx),
                _ => {}
            },
        );
        input.focus_handle(cx).focus(window, cx);
        self.palette = Some(Palette {
            input,
            selected: 0,
            previous_focus,
            subscription,
        });
        cx.notify();
    }
    fn confirm_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let item = self.palette.as_ref().and_then(|palette| {
            palette
                .items(self.state.read(cx), cx)
                .get(palette.selected)
                .cloned()
        });
        if let Some(item) = item {
            if item.disabled.is_none() {
                self.run_palette_target(item.target, window, cx);
            }
        }
    }
    pub fn run_palette_target(
        &mut self,
        target: PaletteTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss(window, cx);
        match target {
            PaletteTarget::Command(command) => self.execute(command, window, cx),
            PaletteTarget::Project(id) => self.select_row(Row::Project(id), true, window, cx),
            PaletteTarget::Thread(id) => {
                self.select_row(Row::Thread(id), true, window, cx);
                self.focus_region(Region::Conversation, window, cx);
            }
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let state = self.state.read(cx);
        let region = if self.palette.is_some() {
            "CommandPalette"
        } else if self.dialog.is_some() || self.factory_form.is_some() {
            "Dialog / NativeInput"
        } else {
            self.focused_region(window, cx).name()
        };
        let branch = state
            .selected_project
            .and_then(|id| state.repositories.get(&id))
            .map(|result| match result {
                Ok(info) => format!(
                    "{}{}",
                    info.branch.as_deref().unwrap_or("Detached HEAD"),
                    if info.dirty { " · dirty" } else { "" }
                ),
                Err(_) => "Root unavailable".into(),
            })
            .unwrap_or_else(|| "No project".into());
        let pending = if state.pending_writes > 0 {
            format!("Saving {} changes", state.pending_writes)
        } else {
            "Saved".into()
        };
        let palette_owner = owner.clone();
        let factory_owner = owner.clone();
        let output_owner = owner.clone();
        let recovery_owner = owner.clone();
        let dismiss_recovery = owner.clone();
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
                .bg(rgb(0x181B20))
                .text_color(rgb(0xD8DEE9))
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
                .on_action(cx.listener(|this, _: &RenameConversation, w, cx| {
                    this.execute(Command::RenameConversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ArchiveConversation, w, cx| {
                    this.execute(Command::ArchiveConversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ShowArchived, w, cx| {
                    this.execute(Command::ShowArchived, w, cx)
                }))
                .on_action(cx.listener(|this, _: &SendToFactory, w, cx| {
                    this.execute(Command::SendToFactory, w, cx)
                }))
                .on_action(cx.listener(|this, _: &OpenFactoryFolder, w, cx| {
                    this.execute(Command::OpenFactoryFolder, w, cx)
                }))
                .on_action(cx.listener(|this, _: &DispatchAzureWorkItem, w, cx| {
                    this.execute(Command::DispatchAzureWorkItem, w, cx)
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
                .on_action(cx.listener(|this, _: &OpenPalette, w, cx| this.open_palette(w, cx)))
                .on_action(cx.listener(|this, _: &FocusProjects, w, cx| {
                    this.focus_region(Region::Projects, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusConversation, w, cx| {
                    this.focus_region(Region::Conversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusFactory, w, cx| {
                    this.focus_region(Region::Factory, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusOutput, w, cx| {
                    this.focus_region(Region::Output, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusLeft, w, cx| {
                    this.spatial_focus(Region::Projects, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusRight, w, cx| {
                    this.spatial_focus(Region::Factory, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusUp, w, cx| {
                    this.spatial_focus(Region::Conversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &FocusDown, w, cx| {
                    this.spatial_focus(Region::Output, w, cx)
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
                    this.focus_region(Region::Conversation, w, cx)
                }))
                .on_action(cx.listener(|this, _: &Submit, w, cx| this.submit_prompt(w, cx)))
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
                .on_action(cx.listener(|this, _: &ToggleFactory, w, cx| {
                    this.toggle_dock(DockPlacement::Right, w, cx)
                }))
                .on_action(cx.listener(|this, _: &ToggleOutput, w, cx| {
                    this.toggle_dock(DockPlacement::Bottom, w, cx)
                }))
                .child(
                    h_flex()
                        .h(px(38.))
                        .px_3()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(rgb(0x343A46))
                        .child(div().flex_1().child("Drost"))
                        .child(
                            Button::new("workspace-palette")
                                .small()
                                .ghost()
                                .label("Commands  ⌘⇧P")
                                .on_click(move |_, w, cx| {
                                    let _ = palette_owner
                                        .update(cx, |this, cx| this.open_palette(w, cx));
                                }),
                        )
                        .child(
                            Button::new("workspace-factory")
                                .small()
                                .ghost()
                                .label("Factory")
                                .on_click(move |_, w, cx| {
                                    let _ = factory_owner.update(cx, |this, cx| {
                                        this.toggle_dock(DockPlacement::Right, w, cx)
                                    });
                                }),
                        )
                        .child(
                            Button::new("workspace-output")
                                .small()
                                .ghost()
                                .label("Output")
                                .on_click(move |_, w, cx| {
                                    let _ = output_owner.update(cx, |this, cx| {
                                        this.toggle_dock(DockPlacement::Bottom, w, cx)
                                    });
                                }),
                        )
                        .child(button(
                            "workspace-settings",
                            Command::Settings,
                            &owner,
                            state,
                        )),
                )
                .when_some(state.storage_error.clone(), |view, error| {
                    view.child(div().px_3().py_2().bg(rgb(0x4A292C)).child(format!(
                        "Storage failure — new work is disabled. In-memory drafts retained. {error}"
                    )))
                })
                .when(
                    has_saved_recovery
                        || state.storage_error.is_some()
                        || state.editor_error.is_some()
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
                .when_some(state.notices.back().cloned(), |view, notice| {
                    view.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .border_b_1()
                            .border_color(rgb(0x343A46))
                            .child(notice),
                    )
                })
                .child(div().flex_1().min_h_0().child(self.dock.clone()))
                .child(
                    h_flex()
                        .h(px(26.))
                        .px_3()
                        .gap_4()
                        .items_center()
                        .text_xs()
                        .text_color(rgb(0x8B95A5))
                        .border_t_1()
                        .border_color(rgb(0x343A46))
                        .child(region)
                        .child(format!(
                            "Neovim: {}",
                            self.editor
                                .as_ref()
                                .map(|view| view.read(cx).mode())
                                .unwrap_or("not connected")
                        ))
                        .child(branch)
                        .child(div().flex_1())
                        .child(format!(
                            "{} interactive · {} Factory",
                            state.active_threads.len(),
                            state.owned_launches.len()
                        ))
                        .child(pending),
                );
        let overlay = self
            .render_detail_overlay(&owner, cx)
            .or_else(|| {
                self.factory_form
                    .as_ref()
                    .map(|form| super::factory_form::render(form, self, &owner, window, cx))
            })
            .or_else(|| {
                self.palette
                    .as_ref()
                    .map(|palette| palette.render(state, &owner, cx))
            })
            .or_else(|| {
                self.dialog
                    .as_ref()
                    .map(|dialog| dialog.render(state, &owner, window))
            });
        if let Some(overlay) = overlay {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(rgba(0x00000088))
                    .flex()
                    .justify_center()
                    .items_start()
                    .pt(px(68.))
                    .child(overlay),
            );
        }
        if state.shutting_down {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(rgba(0x181B20EE))
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
    }
    fn on_removed(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.group = None;
    }
}
impl Panel for WorkspacePanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.region.name()
    }
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.region.name().into())
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl Focusable for WorkspacePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl EventEmitter<PanelEvent> for WorkspacePanel {}
impl Render for WorkspacePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.workspace.upgrade().map(|owner| {
            let workspace = owner.read(cx);
            let state = self.state.read(cx);
            let scroll = &workspace.scrolls[self.region.index()];
            match self.region {
                Region::Projects => {
                    sidebar::render(state, &self.workspace, workspace.cursor, scroll)
                }
                Region::Conversation => conversation::render(state, &self.workspace, scroll, cx),
                Region::Factory => factory::render(state, &self.workspace, scroll),
                Region::Output => factory::output(state, &self.workspace, scroll),
            }
        });
        let focus = self.focus.clone();
        let bounds = self.bounds.clone();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .key_context(self.region.context())
            .track_focus(&self.focus)
            .bg(rgb(0x20242B))
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
    anyhow::ensure!(layout.version == Some(1), "Unsupported layout version");
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
                    !panel.children.is_empty() && *active_index < panel.children.len(),
                    "Invalid active layout tab"
                );
            }
            PanelInfo::Stack { sizes, axis } => {
                anyhow::ensure!(
                    *axis <= 1
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
    for dock in [&layout.left_dock, &layout.right_dock, &layout.bottom_dock]
        .into_iter()
        .flatten()
    {
        anyhow::ensure!(
            f32::from(dock.size()).is_finite() && dock.size() > px(0.),
            "Invalid dock size"
        );
        visit(dock.panel(), &mut names, 0)?;
    }
    anyhow::ensure!(
        names.len() == 4
            && Region::ALL.into_iter().all(|region| names
                .iter()
                .filter(|name| **name == region.name())
                .count()
                == 1),
        "Layout must contain exactly one Projects, Conversation, Factory and Output panel"
    );
    Ok(())
}

pub struct Recovery {
    pub error: String,
}
impl Render for Recovery {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let copy = self.error.clone();
        v_flex().size_full().p_8().gap_4().bg(rgb(0x181B20)).text_color(rgb(0xD8DEE9))
            .child(div().text_xl().child("Drost could not open its workspace"))
            .child("The database has not been replaced or reset. Quit Drost before inspecting or restoring a backup. A newer schema requires a compatible Drost version; a corrupt database requires recovery from a known-good backup.")
            .child(div().font_family("Menlo").text_color(rgb(0xE6A0A0)).child(self.error.clone()))
            .child(h_flex().gap_2().child(Button::new("copy-recovery-error").label("Copy Error").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
                .child(Button::new("quit-recovery").label("Quit Drost").on_click(|_, _, cx| cx.quit())))
    }
}
