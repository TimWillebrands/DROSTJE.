use super::workspace::{Region, Workspace};
use crate::app::AppState;
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        command::{Command as NativeCommand, CommandGroup, CommandItem, CommandState},
    },
    prelude::FluentBuilder,
    *,
};
use std::rc::Rc;
use uuid::Uuid;

gpui_kit::actions!(
    drostje,
    [
        AddProject,
        NewConversation,
        NewFactoryThread,
        RenameProject,
        ArchiveProject,
        RenameThread,
        ArchiveThread,
        CloseThread,
        NextThread,
        PreviousThread,
        ShowArchived,
        RefreshFactoryRuns,
        Settings,
        ResetLayout,
        FocusProjects,
        FocusThread,
        FocusOutput,
        ToggleProjects,
        ToggleOutput,
        ToggleConversationDiffs,
        RefreshConversationDiff,
        RestartEditor,
        CancelEditorCommand,
        CopyDraftRecovery,
        PasteImage,
        OpenPalette,
        FocusLeft,
        FocusDown,
        FocusUp,
        FocusRight,
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        CollapseProject,
        ExpandProject,
        OpenSelection,
        FocusComposer,
        LeaveEditor,
        Submit,
        Interrupt,
        Dismiss,
        Confirm,
        Quit,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    AddProject,
    NewConversation,
    NewFactoryThread,
    RenameProject,
    ArchiveProject,
    RenameThread,
    ArchiveThread,
    CloseThread,
    NextThread,
    PreviousThread,
    ShowArchived,
    RefreshFactoryRuns,
    Settings,
    ResetLayout,
    FocusProjects,
    FocusThread,
    FocusOutput,
    ToggleProjects,
    ToggleOutput,
    RestartEditor,
    ToggleConversationDiffs,
    RefreshConversationDiff,
    CancelEditorCommand,
    CopyDraftRecovery,
    PasteImage,
}
impl Command {
    pub const ALL: [Self; 25] = [
        Self::AddProject,
        Self::NewConversation,
        Self::NewFactoryThread,
        Self::RenameProject,
        Self::ArchiveProject,
        Self::RenameThread,
        Self::ArchiveThread,
        Self::CloseThread,
        Self::NextThread,
        Self::PreviousThread,
        Self::ShowArchived,
        Self::RefreshFactoryRuns,
        Self::Settings,
        Self::ResetLayout,
        Self::FocusProjects,
        Self::FocusThread,
        Self::FocusOutput,
        Self::ToggleProjects,
        Self::ToggleOutput,
        Self::ToggleConversationDiffs,
        Self::RefreshConversationDiff,
        Self::RestartEditor,
        Self::CancelEditorCommand,
        Self::CopyDraftRecovery,
        Self::PasteImage,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::AddProject => "Add project…",
            Self::NewConversation => "New conversation",
            Self::NewFactoryThread => "New Factory thread",
            Self::RenameProject => "Rename project…",
            Self::ArchiveProject => "Archive project…",
            Self::RenameThread => "Rename thread…",
            Self::ArchiveThread => "Archive thread…",
            Self::CloseThread => "Close thread",
            Self::NextThread => "Next thread",
            Self::PreviousThread => "Previous thread",
            Self::ShowArchived => "Show archived threads",
            Self::RefreshFactoryRuns => "Refresh Factory runs",
            Self::Settings => "Settings…",
            Self::ResetLayout => "Reset layout",
            Self::FocusProjects => "Focus projects",
            Self::FocusThread => "Focus thread",
            Self::FocusOutput => "Focus output",
            Self::ToggleProjects => "Toggle projects",
            Self::ToggleOutput => "Toggle output",
            Self::ToggleConversationDiffs => "Toggle conversation changes",
            Self::RefreshConversationDiff => "Refresh conversation changes",
            Self::RestartEditor => "Restart Neovim editor",
            Self::CancelEditorCommand => "Cancel editor command",
            Self::CopyDraftRecovery => "Copy draft recovery",
            Self::PasteImage => "Paste image from clipboard",
        }
    }

    pub fn action(self) -> Box<dyn Action> {
        match self {
            Self::AddProject => Box::new(AddProject),
            Self::NewConversation => Box::new(NewConversation),
            Self::NewFactoryThread => Box::new(NewFactoryThread),
            Self::RenameProject => Box::new(RenameProject),
            Self::ArchiveProject => Box::new(ArchiveProject),
            Self::RenameThread => Box::new(RenameThread),
            Self::ArchiveThread => Box::new(ArchiveThread),
            Self::CloseThread => Box::new(CloseThread),
            Self::NextThread => Box::new(NextThread),
            Self::PreviousThread => Box::new(PreviousThread),
            Self::ShowArchived => Box::new(ShowArchived),
            Self::RefreshFactoryRuns => Box::new(RefreshFactoryRuns),
            Self::Settings => Box::new(Settings),
            Self::ResetLayout => Box::new(ResetLayout),
            Self::FocusProjects => Box::new(FocusProjects),
            Self::FocusThread => Box::new(FocusThread),
            Self::FocusOutput => Box::new(FocusOutput),
            Self::ToggleProjects => Box::new(ToggleProjects),
            Self::ToggleOutput => Box::new(ToggleOutput),
            Self::RestartEditor => Box::new(RestartEditor),
            Self::ToggleConversationDiffs => Box::new(ToggleConversationDiffs),
            Self::RefreshConversationDiff => Box::new(RefreshConversationDiff),
            Self::CancelEditorCommand => Box::new(CancelEditorCommand),
            Self::CopyDraftRecovery => Box::new(CopyDraftRecovery),
            Self::PasteImage => Box::new(PasteImage),
        }
    }

    pub fn disabled_reason(self, state: &AppState) -> Option<String> {
        if state.shutting_down {
            return Some("Application is shutting down.".into());
        }
        match self {
            Self::AddProject if state.registering => {
                Some("A working tree is being inspected.".into())
            }
            Self::AddProject if state.storage_error.is_some() => {
                Some("Storage is not writable.".into())
            }
            Self::NewConversation | Self::RenameProject => state.mutable_project_reason(),
            Self::ArchiveProject => state.archive_project_reason(),
            Self::RenameThread => state
                .mutable_project_reason()
                .or_else(|| match state.thread() {
                    None => Some("Select a thread first.".into()),
                    Some(thread) if thread.archived => {
                        Some("Archived threads are read-only.".into())
                    }
                    _ => None,
                }),
            Self::ArchiveThread => state.archive_thread_reason(),
            Self::CloseThread if state.thread().is_none() => Some("Select a thread first.".into()),
            Self::NextThread | Self::PreviousThread if state.open_threads.len() < 2 => {
                Some("Open at least two threads.".into())
            }
            Self::NewFactoryThread => state.mutable_project_reason().or_else(|| {
                state
                    .selected_project
                    .and_then(|id| match state.repositories.get(&id) {
                        Some(Ok(_)) => None,
                        Some(Err(error)) => Some(format!("Project root is unavailable: {error}")),
                        None => Some("The project working tree has not been inspected.".into()),
                    })
            }),
            Self::ToggleConversationDiffs | Self::RefreshConversationDiff
                if state.conversation_thread().is_none() =>
            {
                Some("Select a conversation first.".into())
            }
            Self::PasteImage => {
                state
                    .mutable_project_reason()
                    .or_else(|| match state.conversation_thread() {
                        None => Some("Select a conversation first.".into()),
                        Some(thread) if thread.archived => {
                            Some("Archived threads are read-only.".into())
                        }
                        _ => None,
                    })
            }
            _ => None,
        }
    }
}

pub fn bind_keys(cx: &mut App) {
    let workspace = Some("Workspace && !Command && !PopupMenu && !Sheet && !Dialog");
    let command = Some("DrostjePalette");
    let navigation =
        Some("Workspace && !Input && !NativeInput && !Command && !PopupMenu && !Sheet && !Dialog");
    // The prompt buffer is the one native input whose Ctrl-W belongs to DROSTJE., not to the
    // editor's: a single-window Neovim has no window to move to.
    let editor_navigation = Some("PromptEditor && !Command && !PopupMenu && !Sheet && !Dialog");
    let list =
        Some("List && !Input && !NativeInput && !Command && !PopupMenu && !Sheet && !Dialog");
    let projects = Some(
        "ProjectsList && !Input && !NativeInput && !Command && !PopupMenu && !Sheet && !Dialog",
    );
    let thread =
        Some("Thread && !Input && !NativeInput && !Command && !PopupMenu && !Sheet && !Dialog");
    let submit = Some(
        "(Thread || PromptEditor || FactoryThreadForm) && !Command && !PopupMenu && !Sheet && !Dialog",
    );
    cx.bind_keys(vec![
        KeyBinding::new("cmd-shift-p", OpenPalette, workspace),
        KeyBinding::new("ctrl-tab", NextThread, workspace),
        KeyBinding::new("ctrl-shift-tab", PreviousThread, workspace),
        KeyBinding::new("cmd-w", CloseThread, workspace),
        KeyBinding::new("cmd-1", FocusProjects, workspace),
        KeyBinding::new("cmd-2", FocusThread, workspace),
        KeyBinding::new("cmd-3", FocusOutput, workspace),
        KeyBinding::new("cmd-b", ToggleProjects, workspace),
        KeyBinding::new("cmd-j", ToggleOutput, workspace),
        KeyBinding::new("cmd-n", NewConversation, workspace),
        KeyBinding::new("cmd-shift-n", NewFactoryThread, workspace),
        KeyBinding::new("cmd-,", Settings, workspace),
        KeyBinding::new("cmd-q", Quit, None),
        // The native Command rows resolve hints against the palette's own action host.
        KeyBinding::new("cmd-1", FocusProjects, command),
        KeyBinding::new("cmd-2", FocusThread, command),
        KeyBinding::new("cmd-3", FocusOutput, command),
        KeyBinding::new("cmd-b", ToggleProjects, command),
        KeyBinding::new("cmd-j", ToggleOutput, command),
        KeyBinding::new("cmd-n", NewConversation, command),
        KeyBinding::new("cmd-shift-n", NewFactoryThread, command),
        KeyBinding::new("cmd-,", Settings, command),
        KeyBinding::new("ctrl-tab", NextThread, command),
        KeyBinding::new("ctrl-shift-tab", PreviousThread, command),
        KeyBinding::new("cmd-w", CloseThread, command),
        KeyBinding::new("ctrl-w h", FocusLeft, navigation),
        KeyBinding::new("ctrl-w j", FocusDown, navigation),
        KeyBinding::new("ctrl-w k", FocusUp, navigation),
        KeyBinding::new("ctrl-w l", FocusRight, navigation),
        KeyBinding::new("ctrl-w ctrl-h", FocusLeft, navigation),
        KeyBinding::new("ctrl-w ctrl-j", FocusDown, navigation),
        KeyBinding::new("ctrl-w ctrl-k", FocusUp, navigation),
        KeyBinding::new("ctrl-w ctrl-l", FocusRight, navigation),
        KeyBinding::new("ctrl-w h", FocusLeft, editor_navigation),
        KeyBinding::new("ctrl-w j", FocusDown, editor_navigation),
        KeyBinding::new("ctrl-w k", FocusUp, editor_navigation),
        KeyBinding::new("ctrl-w l", FocusRight, editor_navigation),
        KeyBinding::new("ctrl-w ctrl-h", FocusLeft, editor_navigation),
        KeyBinding::new("ctrl-w ctrl-j", FocusDown, editor_navigation),
        KeyBinding::new("ctrl-w ctrl-k", FocusUp, editor_navigation),
        KeyBinding::new("ctrl-w ctrl-l", FocusRight, editor_navigation),
        KeyBinding::new("j", SelectNext, list),
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("k", SelectPrevious, list),
        KeyBinding::new("up", SelectPrevious, list),
        KeyBinding::new("g g", SelectFirst, list),
        KeyBinding::new("shift-g", SelectLast, list),
        KeyBinding::new("h", CollapseProject, projects),
        KeyBinding::new("left", CollapseProject, projects),
        KeyBinding::new("l", ExpandProject, projects),
        KeyBinding::new("right", ExpandProject, projects),
        KeyBinding::new("enter", OpenSelection, list),
        KeyBinding::new("j", SelectNext, thread),
        KeyBinding::new("down", SelectNext, thread),
        KeyBinding::new("k", SelectPrevious, thread),
        KeyBinding::new("up", SelectPrevious, thread),
        KeyBinding::new("g g", SelectFirst, thread),
        KeyBinding::new("shift-g", SelectLast, thread),
        KeyBinding::new("i", FocusComposer, thread),
        // Inside the composer Neovim owns Cmd-V and hands image clipboards to the same Action.
        KeyBinding::new("cmd-v", PasteImage, thread),
        KeyBinding::new(
            "cmd-escape",
            LeaveEditor,
            Some("PromptEditor && !Dialog && !Sheet"),
        ),
        KeyBinding::new("cmd-enter", Submit, submit),
        KeyBinding::new(
            "cmd-enter",
            Submit,
            Some("Input && PromptEditor && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            "cmd-.",
            Interrupt,
            Some("Input && PromptEditor && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            "cmd-.",
            Interrupt,
            Some("(Thread || PromptEditor) && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            "cmd-enter",
            Confirm,
            Some("DrostjeForm && !Command && !PopupMenu"),
        ),
        KeyBinding::new(
            "cmd-enter",
            Confirm,
            Some("Input && PromptEditor && DrostjeForm && !Command && !PopupMenu"),
        ),
    ]);
}

pub fn button(
    id: impl Into<ElementId>,
    command: Command,
    workspace: &WeakEntity<Workspace>,
    state: &AppState,
) -> Button {
    let owner = workspace.clone();
    let reason = command.disabled_reason(state);
    Button::new(id)
        .label(command.label())
        .small()
        .ghost()
        .disabled(reason.is_some())
        .when_some(reason, |button, reason| button.tooltip(reason))
        .on_click(move |_, window, cx| {
            let _ = owner.update(cx, |workspace, cx| workspace.execute(command, window, cx));
        })
}

#[derive(Clone, Copy)]
pub enum PaletteTarget {
    Command(Command),
    Project(Uuid),
    Thread(Uuid),
}

pub struct Palette {
    pub command: Entity<CommandState>,
    owner: WeakEntity<Workspace>,
    state: Entity<AppState>,
    closed: bool,
    _subscriptions: Vec<Subscription>,
}

impl Palette {
    pub fn open(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let palette = cx.new(|cx| {
            let command = cx.new(|cx| CommandState::new(window, cx));
            let subscriptions = vec![
                cx.observe(&state, |_, _, cx| cx.notify()),
                cx.subscribe(
                    &state,
                    |_, _, change: &crate::app::PresentationChange, cx| {
                        if matches!(
                            change,
                            crate::app::PresentationChange::Thread(_)
                                | crate::app::PresentationChange::Draft(_)
                                | crate::app::PresentationChange::Selection
                                | crate::app::PresentationChange::Projects
                        ) {
                            cx.notify();
                        }
                    },
                ),
            ];
            Self {
                command,
                owner,
                state,
                closed: false,
                _subscriptions: subscriptions,
            }
        });
        let content = palette.clone();
        let close_owner = palette.downgrade();
        // Dialog's pixel-only width API is resolved at the window/rem boundary.
        let width =
            (window.rem_size() * 40.).min(window.viewport_size().width - window.rem_size() * 2.);
        window.open_dialog(cx, move |dialog, _, _| {
            let close_owner = close_owner.clone();
            dialog
                .close_button(false)
                .p_0()
                .width(width)
                .child(content.clone())
                .on_close(move |_, _, cx| {
                    let _ = close_owner.update(cx, |palette, cx| palette.cleanup(cx));
                })
        });
        palette.read(cx).command.focus_handle(cx).focus(window, cx);
        palette
    }

    /// Workspace takes its retained palette before asking the native host to close.
    /// Root alone owns restoring the trigger focus.
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closed {
            self.closed = true;
            window.close_dialog(cx);
        }
    }

    fn cleanup(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        let id = cx.entity().entity_id();
        let _ = self.owner.update(cx, |workspace, cx| {
            if workspace
                .palette
                .as_ref()
                .is_some_and(|palette| palette.entity_id() == id)
            {
                workspace.palette = None;
                cx.notify();
            }
        });
    }

    fn run(&mut self, target: PaletteTarget, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if self.closed || self.state.read(cx).shutting_down {
            return;
        }
        if let PaletteTarget::Command(command) = target
            && command.disabled_reason(self.state.read(cx)).is_some()
        {
            return;
        }
        self.closed = true;
        let id = cx.entity().entity_id();
        let _ = self.owner.update(cx, |workspace, cx| {
            if !workspace
                .palette
                .as_ref()
                .is_some_and(|palette| palette.entity_id() == id)
            {
                return;
            }
            workspace.palette = None;
            // Close only this host, before a command can open Settings or another dialog.
            window.close_dialog(cx);
            match target {
                PaletteTarget::Command(command) => workspace.execute(command, window, cx),
                target => workspace.run_palette_target(target, window, cx),
            }
            cx.notify();
        });
    }
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let mut targets = [Vec::new(), Vec::new(), Vec::new()];
        let mut commands = CommandGroup::new().label("Commands");
        for command in Command::ALL {
            let reason = command.disabled_reason(state);
            let label = match &reason {
                Some(reason) => format!("{} — {reason}", command.label()),
                None => command.label().to_owned(),
            };
            commands = commands.item(
                CommandItem::new()
                    .label(label)
                    .action(command.action())
                    .disabled(reason.is_some()),
            );
            targets[0].push(PaletteTarget::Command(command));
        }
        let mut projects = CommandGroup::new().label("Projects");
        for project in state
            .snapshot
            .projects
            .iter()
            .filter(|project| state.show_archived || !project.archived)
        {
            projects = projects.item(
                CommandItem::new().label(project.name.clone()).keywords([
                    project.root.display().to_string(),
                    if project.archived {
                        "archived project"
                    } else {
                        "project"
                    }
                    .to_owned(),
                ]),
            );
            targets[1].push(PaletteTarget::Project(project.id));
        }
        let mut threads = CommandGroup::new().label("Threads");
        for thread in state.snapshot.threads.iter().filter(|thread| {
            state.show_archived
                || (!thread.archived
                    && state
                        .snapshot
                        .projects
                        .iter()
                        .any(|project| project.id == thread.project_id && !project.archived))
        }) {
            let project = state
                .snapshot
                .projects
                .iter()
                .find(|project| project.id == thread.project_id);
            threads = threads.item(
                CommandItem::new().label(thread.title.clone()).keywords([
                    format!(
                        "{:?} thread{}",
                        thread.kind,
                        if thread.archived { " archived" } else { "" }
                    ),
                    project
                        .map(|project| project.name.clone())
                        .unwrap_or_default(),
                ]),
            );
            targets[2].push(PaletteTarget::Thread(thread.id));
        }
        // Capture the exact unfiltered model installed by this render. Native filtering
        // preserves group/row coordinates, including when a group has no visible rows.
        let targets = Rc::new(targets);
        let confirm_owner = cx.entity().downgrade();
        let cancel_owner = cx.entity().downgrade();
        let command = NativeCommand::new(&self.command)
            .bordered(false)
            .placeholder("Search commands, projects and threads…")
            .max_h(rems(26.))
            .group(commands)
            .group(projects)
            .group(threads)
            .empty(|_, _, cx| {
                div()
                    .p_3()
                    .text_color(cx.theme().muted_foreground)
                    .child("No matching commands, projects or threads.")
            })
            .on_confirm(move |index, window, cx| {
                let Some(target) = targets
                    .get(index.section)
                    .and_then(|group| group.get(index.row))
                    .copied()
                else {
                    return;
                };
                // Action dispatch precedes this deferred callback. Never execute or close
                // again: the action may already have opened a different native surface.
                if matches!(target, PaletteTarget::Command(_)) {
                    return;
                }
                let _ = confirm_owner.update(cx, |palette, cx| palette.run(target, window, cx));
            })
            .on_cancel(move |_, cx| {
                // Empty-query Cancel continues to Dialog, which owns the native pop.
                let _ = cancel_owner.update(cx, |palette, cx| palette.cleanup(cx));
            });
        div()
            .key_context("DrostjePalette")
            .on_action(cx.listener(|this, _: &AddProject, w, cx| {
                this.run(PaletteTarget::Command(Command::AddProject), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewConversation, w, cx| {
                this.run(PaletteTarget::Command(Command::NewConversation), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewFactoryThread, w, cx| {
                this.run(PaletteTarget::Command(Command::NewFactoryThread), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RenameProject, w, cx| {
                this.run(PaletteTarget::Command(Command::RenameProject), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ArchiveProject, w, cx| {
                this.run(PaletteTarget::Command(Command::ArchiveProject), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RenameThread, w, cx| {
                this.run(PaletteTarget::Command(Command::RenameThread), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ArchiveThread, w, cx| {
                this.run(PaletteTarget::Command(Command::ArchiveThread), w, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseThread, w, cx| {
                this.run(PaletteTarget::Command(Command::CloseThread), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NextThread, w, cx| {
                this.run(PaletteTarget::Command(Command::NextThread), w, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousThread, w, cx| {
                this.run(PaletteTarget::Command(Command::PreviousThread), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowArchived, w, cx| {
                this.run(PaletteTarget::Command(Command::ShowArchived), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RefreshFactoryRuns, w, cx| {
                this.run(PaletteTarget::Command(Command::RefreshFactoryRuns), w, cx)
            }))
            .on_action(cx.listener(|this, _: &Settings, w, cx| {
                this.run(PaletteTarget::Command(Command::Settings), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ResetLayout, w, cx| {
                this.run(PaletteTarget::Command(Command::ResetLayout), w, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusProjects, w, cx| {
                this.run(PaletteTarget::Command(Command::FocusProjects), w, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusThread, w, cx| {
                this.run(PaletteTarget::Command(Command::FocusThread), w, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusOutput, w, cx| {
                this.run(PaletteTarget::Command(Command::FocusOutput), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleProjects, w, cx| {
                this.run(PaletteTarget::Command(Command::ToggleProjects), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleConversationDiffs, w, cx| {
                this.run(
                    PaletteTarget::Command(Command::ToggleConversationDiffs),
                    w,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &RefreshConversationDiff, w, cx| {
                this.run(
                    PaletteTarget::Command(Command::RefreshConversationDiff),
                    w,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &ToggleOutput, w, cx| {
                this.run(PaletteTarget::Command(Command::ToggleOutput), w, cx)
            }))
            .w_full()
            .child(command)
    }
}

pub fn region_action(region: Region) -> &'static str {
    match region {
        Region::Projects => "Projects",
        Region::Thread => "Thread",
        Region::Output => "Output",
    }
}
