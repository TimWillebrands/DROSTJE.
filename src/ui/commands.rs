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
        NewSpace,
        RenameSpace,
        DeleteSpace,
        SwitchSpace,
        NextSpace,
        PreviousSpace,
        MoveProjectToSpace,
        NewConversation,
        NewOmpConversation,
        NewPiConversation,
        NewClaudeCodeConversation,
        NewConversationInWorktree,
        SwitchHarness,
        NewFactoryThread,
        NewTerminal,
        RestartTerminal,
        RenameProject,
        ArchiveProject,
        RenameThread,
        RegenerateTitle,
        ArchiveThread,
        UnarchiveThread,
        RemoveWorktree,
        SwitchBranch,
        CloseThread,
        NextThread,
        PreviousThread,
        ShowArchived,
        SearchThreads,
        RefreshFactoryRuns,
        Settings,
        CustomizeTheme,
        InstallPi,
        ResetLayout,
        FocusProjects,
        FocusThread,
        FocusOutput,
        ToggleProjects,
        ToggleOutput,
        ToggleConversationDiffs,
        ToggleAgents,
        ToggleTaskList,
        ToggleBackgroundJobs,
        RefreshConversationDiff,
        RestartEditor,
        CancelEditorCommand,
        CopyDraftRecovery,
        PasteImage,
        ClearQueuedMessages,
        SendQueuedNow,
        EditQueuedMessage,
        InsertSlashCommand,
        NextCompletion,
        PreviousCompletion,
        AcceptCompletion,
        DismissCompletion,
        OpenPalette,
        FocusLeft,
        FocusDown,
        FocusUp,
        FocusRight,
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        SelectPageUp,
        SelectPageDown,
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
    NewSpace,
    RenameSpace,
    DeleteSpace,
    SwitchSpace,
    NextSpace,
    PreviousSpace,
    MoveProjectToSpace,
    NewConversation,
    NewOmpConversation,
    NewPiConversation,
    NewClaudeCodeConversation,
    NewConversationInWorktree,
    SwitchHarness,
    NewFactoryThread,
    NewTerminal,
    RestartTerminal,
    RenameProject,
    ArchiveProject,
    RenameThread,
    RegenerateTitle,
    ArchiveThread,
    UnarchiveThread,
    RemoveWorktree,
    SwitchBranch,
    CloseThread,
    NextThread,
    PreviousThread,
    ShowArchived,
    SearchThreads,
    RefreshFactoryRuns,
    Settings,
    CustomizeTheme,
    InstallPi,
    ResetLayout,
    FocusProjects,
    FocusThread,
    FocusOutput,
    ToggleProjects,
    ToggleOutput,
    RestartEditor,
    ToggleConversationDiffs,
    ToggleAgents,
    ToggleTaskList,
    ToggleBackgroundJobs,
    RefreshConversationDiff,
    CancelEditorCommand,
    CopyDraftRecovery,
    PasteImage,
    ClearQueuedMessages,
    SendQueuedNow,
    EditQueuedMessage,
    InsertSlashCommand,
}
impl Command {
    pub const ALL: [Self; 53] = [
        Self::NewSpace,
        Self::RenameSpace,
        Self::DeleteSpace,
        Self::SwitchSpace,
        Self::NextSpace,
        Self::PreviousSpace,
        Self::MoveProjectToSpace,
        Self::AddProject,
        Self::NewConversation,
        Self::NewOmpConversation,
        Self::NewPiConversation,
        Self::NewClaudeCodeConversation,
        Self::NewConversationInWorktree,
        Self::SwitchHarness,
        Self::NewFactoryThread,
        Self::NewTerminal,
        Self::RestartTerminal,
        Self::RenameProject,
        Self::ArchiveProject,
        Self::RenameThread,
        Self::RegenerateTitle,
        Self::ArchiveThread,
        Self::UnarchiveThread,
        Self::RemoveWorktree,
        Self::SwitchBranch,
        Self::CloseThread,
        Self::NextThread,
        Self::PreviousThread,
        Self::ShowArchived,
        Self::SearchThreads,
        Self::RefreshFactoryRuns,
        Self::Settings,
        Self::CustomizeTheme,
        Self::InstallPi,
        Self::ResetLayout,
        Self::FocusProjects,
        Self::FocusThread,
        Self::FocusOutput,
        Self::ToggleProjects,
        Self::ToggleOutput,
        Self::ToggleConversationDiffs,
        Self::ToggleAgents,
        Self::ToggleTaskList,
        Self::ToggleBackgroundJobs,
        Self::RefreshConversationDiff,
        Self::RestartEditor,
        Self::CancelEditorCommand,
        Self::CopyDraftRecovery,
        Self::PasteImage,
        Self::InsertSlashCommand,
        Self::SendQueuedNow,
        Self::EditQueuedMessage,
        Self::ClearQueuedMessages,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::AddProject => "Add project…",
            Self::NewSpace => "New space…",
            Self::RenameSpace => "Rename space…",
            Self::DeleteSpace => "Delete space…",
            Self::SwitchSpace => "Switch space…",
            Self::NextSpace => "Next space",
            Self::PreviousSpace => "Previous space",
            Self::MoveProjectToSpace => "Move project to space…",
            Self::NewConversation => "New conversation",
            Self::NewOmpConversation => "New OMP conversation",
            Self::NewPiConversation => "New Pi conversation",
            Self::NewClaudeCodeConversation => "New Claude Code conversation",
            Self::NewConversationInWorktree => "New conversation in new worktree…",
            Self::SwitchHarness => "Switch harness…",
            Self::NewFactoryThread => "New Factory thread",
            Self::NewTerminal => "New terminal",
            Self::RestartTerminal => "Restart terminal",
            Self::RenameProject => "Rename project…",
            Self::ArchiveProject => "Archive project…",
            Self::RenameThread => "Rename thread…",
            Self::RegenerateTitle => "Regenerate conversation title",
            Self::ArchiveThread => "Archive thread",
            Self::UnarchiveThread => "Unarchive thread",
            Self::RemoveWorktree => "Remove worktree…",
            Self::SwitchBranch => "Switch branch…",
            Self::CloseThread => "Close tab",
            Self::NextThread => "Next tab",
            Self::PreviousThread => "Previous tab",
            Self::ShowArchived => "Show archived threads",
            Self::SearchThreads => "Search threads…",
            Self::RefreshFactoryRuns => "Refresh Factory runs",
            Self::Settings => "Settings…",
            Self::CustomizeTheme => "Customize theme…",
            Self::InstallPi => "Install Pi",
            Self::ResetLayout => "Reset layout",
            Self::FocusProjects => "Focus projects",
            Self::FocusThread => "Focus thread",
            Self::FocusOutput => "Focus output",
            Self::ToggleProjects => "Toggle projects",
            Self::ToggleOutput => "Toggle output",
            Self::ToggleConversationDiffs => "Toggle conversation changes",
            Self::ToggleAgents => "Toggle agents",
            Self::ToggleTaskList => "Toggle task list",
            Self::ToggleBackgroundJobs => "Toggle background jobs",
            Self::RefreshConversationDiff => "Refresh conversation changes",
            Self::RestartEditor => "Restart Neovim editor",
            Self::CancelEditorCommand => "Cancel editor command",
            Self::CopyDraftRecovery => "Copy draft recovery",
            Self::PasteImage => "Paste image from clipboard",
            Self::ClearQueuedMessages => "Clear queued messages",
            Self::SendQueuedNow => "Send next queued message now",
            Self::EditQueuedMessage => "Edit last queued message",
            Self::InsertSlashCommand => "Insert slash command…",
        }
    }

    pub fn action(self) -> Box<dyn Action> {
        match self {
            Self::AddProject => Box::new(AddProject),
            Self::NewSpace => Box::new(NewSpace),
            Self::RenameSpace => Box::new(RenameSpace),
            Self::DeleteSpace => Box::new(DeleteSpace),
            Self::SwitchSpace => Box::new(SwitchSpace),
            Self::NextSpace => Box::new(NextSpace),
            Self::PreviousSpace => Box::new(PreviousSpace),
            Self::MoveProjectToSpace => Box::new(MoveProjectToSpace),
            Self::NewConversation => Box::new(NewConversation),
            Self::NewOmpConversation => Box::new(NewOmpConversation),
            Self::NewPiConversation => Box::new(NewPiConversation),
            Self::NewClaudeCodeConversation => Box::new(NewClaudeCodeConversation),
            Self::NewConversationInWorktree => Box::new(NewConversationInWorktree),
            Self::SwitchHarness => Box::new(SwitchHarness),
            Self::NewFactoryThread => Box::new(NewFactoryThread),
            Self::NewTerminal => Box::new(NewTerminal),
            Self::RestartTerminal => Box::new(RestartTerminal),
            Self::RenameProject => Box::new(RenameProject),
            Self::ArchiveProject => Box::new(ArchiveProject),
            Self::RenameThread => Box::new(RenameThread),
            Self::RegenerateTitle => Box::new(RegenerateTitle),
            Self::ArchiveThread => Box::new(ArchiveThread),
            Self::UnarchiveThread => Box::new(UnarchiveThread),
            Self::RemoveWorktree => Box::new(RemoveWorktree),
            Self::SwitchBranch => Box::new(SwitchBranch),
            Self::CloseThread => Box::new(CloseThread),
            Self::NextThread => Box::new(NextThread),
            Self::PreviousThread => Box::new(PreviousThread),
            Self::ShowArchived => Box::new(ShowArchived),
            Self::SearchThreads => Box::new(SearchThreads),
            Self::RefreshFactoryRuns => Box::new(RefreshFactoryRuns),
            Self::Settings => Box::new(Settings),
            Self::CustomizeTheme => Box::new(CustomizeTheme),
            Self::InstallPi => Box::new(InstallPi),
            Self::ResetLayout => Box::new(ResetLayout),
            Self::FocusProjects => Box::new(FocusProjects),
            Self::FocusThread => Box::new(FocusThread),
            Self::FocusOutput => Box::new(FocusOutput),
            Self::ToggleProjects => Box::new(ToggleProjects),
            Self::ToggleOutput => Box::new(ToggleOutput),
            Self::RestartEditor => Box::new(RestartEditor),
            Self::ToggleConversationDiffs => Box::new(ToggleConversationDiffs),
            Self::ToggleAgents => Box::new(ToggleAgents),
            Self::ToggleTaskList => Box::new(ToggleTaskList),
            Self::ToggleBackgroundJobs => Box::new(ToggleBackgroundJobs),
            Self::RefreshConversationDiff => Box::new(RefreshConversationDiff),
            Self::CancelEditorCommand => Box::new(CancelEditorCommand),
            Self::CopyDraftRecovery => Box::new(CopyDraftRecovery),
            Self::PasteImage => Box::new(PasteImage),
            Self::ClearQueuedMessages => Box::new(ClearQueuedMessages),
            Self::SendQueuedNow => Box::new(SendQueuedNow),
            Self::EditQueuedMessage => Box::new(EditQueuedMessage),
            Self::InsertSlashCommand => Box::new(InsertSlashCommand),
        }
    }

    pub fn disabled_reason(self, state: &AppState) -> Option<String> {
        if state.shutting_down {
            return Some("Application is shutting down.".into());
        }
        if self == Self::RestartTerminal {
            return Some("Select a terminal first.".into());
        }
        match self {
            Self::DeleteSpace if state.active_space == state.default_space_id() => {
                Some("Default is the recovery space and cannot be deleted.".into())
            }
            Self::SwitchSpace | Self::NextSpace | Self::PreviousSpace if state.spaces.len() < 2 => {
                Some("Create another space first.".into())
            }
            Self::MoveProjectToSpace if state.selected_project.is_none() => {
                Some("Select a project first.".into())
            }
            Self::MoveProjectToSpace if state.spaces.len() < 2 => {
                Some("Create another space to move this project into.".into())
            }
            Self::NewSpace
            | Self::RenameSpace
            | Self::DeleteSpace
            | Self::SwitchSpace
            | Self::NextSpace
            | Self::PreviousSpace
            | Self::MoveProjectToSpace => state.space_mutation_reason(),
            Self::AddProject if state.registering => {
                Some("A working tree is being inspected.".into())
            }
            Self::AddProject if state.storage_error.is_some() => {
                Some("Storage is not writable.".into())
            }
            Self::NewConversation
            | Self::NewOmpConversation
            | Self::NewPiConversation
            | Self::NewClaudeCodeConversation
            | Self::RenameProject => state.mutable_project_reason(),
            Self::InstallPi if state.installing_pi => {
                Some("Pi installation is already running.".into())
            }
            Self::InstallPi if state.storage_error.is_some() => {
                Some("Storage is not writable.".into())
            }
            Self::InstallPi
                if state
                    .diagnostics
                    .get("pi")
                    .is_some_and(|status| status.starts_with("Resolved:")) =>
            {
                Some("Pi is already available.".into())
            }
            Self::ArchiveProject => state.archive_project_reason(),
            Self::RenameThread => state.rename_thread_reason(),
            Self::RegenerateTitle => state.regenerate_title_reason(),
            Self::ArchiveThread => state.archive_thread_reason(),
            Self::UnarchiveThread => state.unarchive_thread_reason(),
            Self::NewConversationInWorktree => state.create_worktree_reason(),
            Self::SwitchHarness => state.switch_harness_reason(),
            Self::RemoveWorktree => state.remove_worktree_reason(),
            Self::SwitchBranch => state.switch_branch_reason(),
            Self::CloseThread | Self::FocusOutput | Self::ToggleOutput
                if state.thread().is_none() =>
            {
                Some("Select a thread first.".into())
            }
            Self::NextThread | Self::PreviousThread if state.open_threads.len() < 2 => {
                Some("Open at least two threads.".into())
            }
            Self::NewTerminal => state.mutable_project_reason().or_else(|| {
                state
                    .selected_project
                    .and_then(|id| match state.repositories.get(&id) {
                        Some(Ok(_)) => None,
                        Some(Err(error)) => Some(format!("Project root is unavailable: {error}")),
                        None => Some("The project working tree has not been inspected.".into()),
                    })
            }),
            Self::NewFactoryThread => state.mutable_project_reason().or_else(|| {
                state
                    .selected_project
                    .and_then(|id| match state.repositories.get(&id) {
                        Some(Ok(_)) => None,
                        Some(Err(error)) => Some(format!("Project root is unavailable: {error}")),
                        None => Some("The project working tree has not been inspected.".into()),
                    })
            }),
            Self::ToggleConversationDiffs
            | Self::ToggleAgents
            | Self::ToggleTaskList
            | Self::ToggleBackgroundJobs
            | Self::RefreshConversationDiff
                if state.conversation_thread().is_none() =>
            {
                Some("Select a conversation first.".into())
            }
            Self::ToggleAgents
                if state
                    .conversation_thread()
                    .and_then(|thread| state.conversations.get(&thread.id))
                    .is_none_or(|session| super::agents::coordination_calls(session) == 0) =>
            {
                Some("This conversation's agent has not worked with other agents yet.".into())
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
            Self::ClearQueuedMessages | Self::EditQueuedMessage => state
                .mutable_project_reason()
                .or_else(|| match state.conversation_thread() {
                    None => Some("Select a conversation first.".into()),
                    Some(thread) if thread.archived => {
                        Some("Archived threads are read-only.".into())
                    }
                    Some(thread) if thread.queued.is_empty() => {
                        Some("No messages are queued.".into())
                    }
                    _ => None,
                }),
            Self::InsertSlashCommand => match state.conversation_thread() {
                None => Some("Select a conversation first.".into()),
                Some(thread) if thread.archived => Some("Archived threads are read-only.".into()),
                Some(thread) => state
                    .slash_command_reason(thread.id)
                    .map(|reason| format!("{reason}.")),
            },
            Self::SendQueuedNow => match state.conversation_thread() {
                None => Some("Select a conversation first.".into()),
                // During a turn, Send now stops that turn first; the row button agrees.
                Some(thread)
                    if !thread.queued.is_empty() && state.active_threads.contains(&thread.id) =>
                {
                    None
                }
                Some(thread) => state
                    .queued_dispatch_reason(thread.id)
                    .map(|reason| format!("{reason}.")),
            },
            _ => None,
        }
    }

    /// Tab commands depend on ephemeral panes as well as persisted threads.
    pub fn disabled_reason_in_workspace(self, workspace: &Workspace, cx: &App) -> Option<String> {
        let state = workspace.state.read(cx);
        if state.shutting_down {
            return Some("Application is shutting down.".into());
        }
        match self {
            Self::CloseThread if workspace.has_active_terminal(cx) => None,
            Self::NextThread | Self::PreviousThread => {
                (workspace.center_panel_count(cx) < 2).then(|| "Open at least two tabs.".into())
            }
            Self::RestartTerminal if workspace.has_active_terminal(cx) => None,
            _ => self.disabled_reason(state),
        }
    }
}

/// The picker row a motion lands on among `count` rows: `delta` steps from `current` (wrapping;
/// with no highlight, down starts at the top and up at the bottom), or the first or last row
/// with `edge`. A `skipped` row (the current harness) is stepped over.
fn picker_row(
    count: usize,
    current: Option<usize>,
    delta: isize,
    edge: bool,
    skipped: Option<usize>,
) -> usize {
    let step =
        |index: usize, delta: isize| (index as isize + delta).rem_euclid(count as isize) as usize;
    let next = if edge {
        if delta < 0 { 0 } else { count - 1 }
    } else {
        current.map_or(if delta < 0 { count - 1 } else { 0 }, |index| {
            step(index, delta)
        })
    };
    match skipped {
        Some(skipped) if skipped == next && count > 1 => step(
            next,
            if edge {
                -delta.signum()
            } else {
                delta.signum()
            },
        ),
        _ => next,
    }
}

// Workspace chords use GPUI's `secondary-` modifier: Cmd on macOS, Ctrl elsewhere.
// Two chords cannot follow that rule on Linux, where Ctrl also belongs to Vim and
// to the Ctrl-W pane chord; they keep the terminal convention Ctrl-Shift instead.

/// The native paste chord: Cmd-V on macOS; Ctrl-Shift-V elsewhere, where
/// Ctrl-V is Vim's blockwise visual and literal-insert key.
pub const PASTE_IMAGE_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-v"
} else {
    "ctrl-shift-v"
};

/// Close the active thread tab: Cmd-W on macOS; Ctrl-Shift-W elsewhere, because
/// a lone Ctrl-W is the pending prefix of the Ctrl-W h/j/k/l pane chord.
pub const CLOSE_THREAD_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-w"
} else {
    "ctrl-shift-w"
};

pub fn bind_keys(cx: &mut App) {
    let workspace = Some("Workspace && !Command && !PopupMenu && !Sheet && !Dialog");
    let command = Some("DrostjePalette");
    let picker = Some("SpacePicker || HarnessPicker");
    let navigation =
        Some("Workspace && !Input && !NativeInput && !Command && !PopupMenu && !Sheet && !Dialog");
    // Embedded editors and terminals reserve the same pane-navigation prefix.
    let editor_navigation = Some(
        "(PromptEditor || Terminal || TerminalPane) && !Command && !PopupMenu && !Sheet && !Dialog",
    );
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
    let caret_popup = Some("PromptEditor && CaretPopup && !Command && !Sheet && !Dialog");
    cx.bind_keys(vec![
        KeyBinding::new("secondary-shift-p", OpenPalette, workspace),
        KeyBinding::new("secondary-alt-]", NextSpace, navigation),
        KeyBinding::new("secondary-alt-[", PreviousSpace, navigation),
        KeyBinding::new("j", SelectNext, picker),
        KeyBinding::new("k", SelectPrevious, picker),
        KeyBinding::new("g g", SelectFirst, picker),
        KeyBinding::new("shift-g", SelectLast, picker),
        KeyBinding::new("home", SelectFirst, picker),
        KeyBinding::new("end", SelectLast, picker),
        // New conversation again from its harness picker steps to the next harness.
        KeyBinding::new("secondary-n", NewConversation, Some("HarnessPicker")),
        KeyBinding::new("ctrl-tab", NextThread, workspace),
        KeyBinding::new("ctrl-shift-tab", PreviousThread, workspace),
        KeyBinding::new(CLOSE_THREAD_KEY, CloseThread, workspace),
        KeyBinding::new("secondary-1", FocusProjects, workspace),
        KeyBinding::new("secondary-2", FocusThread, workspace),
        KeyBinding::new("secondary-3", FocusOutput, workspace),
        KeyBinding::new("secondary-b", ToggleProjects, workspace),
        KeyBinding::new("secondary-j", ToggleOutput, workspace),
        KeyBinding::new("secondary-n", NewConversation, workspace),
        KeyBinding::new("secondary-alt-n", NewConversationInWorktree, workspace),
        KeyBinding::new("secondary-alt-h", SwitchHarness, workspace),
        KeyBinding::new("secondary-shift-n", NewFactoryThread, workspace),
        KeyBinding::new("secondary-alt-t", NewTerminal, workspace),
        KeyBinding::new("secondary-,", Settings, workspace),
        KeyBinding::new("secondary-q", Quit, None),
        // The native Command rows resolve hints against the palette's own action host.
        KeyBinding::new("secondary-1", FocusProjects, command),
        KeyBinding::new("secondary-2", FocusThread, command),
        KeyBinding::new("secondary-3", FocusOutput, command),
        KeyBinding::new("secondary-b", ToggleProjects, command),
        KeyBinding::new("secondary-j", ToggleOutput, command),
        KeyBinding::new("secondary-n", NewConversation, command),
        KeyBinding::new("secondary-alt-n", NewConversationInWorktree, command),
        KeyBinding::new("secondary-alt-h", SwitchHarness, command),
        KeyBinding::new("secondary-shift-n", NewFactoryThread, command),
        KeyBinding::new("secondary-alt-t", NewTerminal, command),
        KeyBinding::new("secondary-,", Settings, command),
        KeyBinding::new("ctrl-tab", NextThread, command),
        KeyBinding::new("ctrl-shift-tab", PreviousThread, command),
        KeyBinding::new(CLOSE_THREAD_KEY, CloseThread, command),
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
        KeyBinding::new("home", SelectFirst, list),
        KeyBinding::new("end", SelectLast, list),
        KeyBinding::new("pageup", SelectPageUp, list),
        KeyBinding::new("pagedown", SelectPageDown, list),
        KeyBinding::new("h", CollapseProject, projects),
        KeyBinding::new("left", CollapseProject, projects),
        KeyBinding::new("l", ExpandProject, projects),
        KeyBinding::new("right", ExpandProject, projects),
        KeyBinding::new("/", SearchThreads, projects),
        // The thread filter is a native input: Escape clears it and Down moves into the
        // rows, both only after the input itself declined the key.
        KeyBinding::new(
            "escape",
            Dismiss,
            Some("ProjectSearch && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            "down",
            SelectNext,
            Some("ProjectSearch && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new("enter", OpenSelection, list),
        KeyBinding::new("j", SelectNext, thread),
        KeyBinding::new("down", SelectNext, thread),
        KeyBinding::new("k", SelectPrevious, thread),
        KeyBinding::new("up", SelectPrevious, thread),
        KeyBinding::new("g g", SelectFirst, thread),
        KeyBinding::new("shift-g", SelectLast, thread),
        KeyBinding::new("home", SelectFirst, thread),
        KeyBinding::new("end", SelectLast, thread),
        KeyBinding::new("pageup", SelectPageUp, thread),
        KeyBinding::new("pagedown", SelectPageDown, thread),
        KeyBinding::new("i", FocusComposer, thread),
        // Inside the composer the Neovim view owns the same chord and hands image
        // clipboards to this Action.
        KeyBinding::new(PASTE_IMAGE_KEY, PasteImage, thread),
        KeyBinding::new(
            "secondary-escape",
            LeaveEditor,
            Some("(PromptEditor || Terminal) && !Dialog && !Sheet"),
        ),
        KeyBinding::new("secondary-enter", Submit, submit),
        // The composer's caret popup follows Vim's insert-mode completion menu. Its context
        // is deeper than Root's Tab focus navigation and the workspace's Escape.
        KeyBinding::new("ctrl-n", NextCompletion, caret_popup),
        KeyBinding::new("down", NextCompletion, caret_popup),
        KeyBinding::new("ctrl-p", PreviousCompletion, caret_popup),
        KeyBinding::new("up", PreviousCompletion, caret_popup),
        KeyBinding::new("tab", AcceptCompletion, caret_popup),
        KeyBinding::new("enter", AcceptCompletion, caret_popup),
        KeyBinding::new("ctrl-e", DismissCompletion, caret_popup),
        KeyBinding::new("escape", DismissCompletion, caret_popup),
        KeyBinding::new(
            "secondary-enter",
            Submit,
            Some("Input && PromptEditor && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            "secondary-.",
            Interrupt,
            Some("Input && PromptEditor && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            "secondary-.",
            Interrupt,
            Some("(Thread || PromptEditor) && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            "secondary-alt-backspace",
            ClearQueuedMessages,
            Some("Input && PromptEditor && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            "secondary-alt-backspace",
            ClearQueuedMessages,
            Some("(Thread || PromptEditor) && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            "secondary-enter",
            gpui_kit::base::actions::Confirm { secondary: false },
            Some("Dialog && !DrostjeForm && !Command && !PopupMenu"),
        ),
        KeyBinding::new(
            "secondary-enter",
            Confirm,
            Some("DrostjeForm && !Command && !PopupMenu"),
        ),
        KeyBinding::new(
            "secondary-enter",
            Confirm,
            Some("Input && PromptEditor && DrostjeForm && !Command && !PopupMenu"),
        ),
    ]);
    super::theme_editor::bind_keys(cx);
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

struct SpacePicker {
    project: Option<Uuid>,
    spaces: Vec<crate::model::Space>,
}

pub struct Palette {
    pub command: Entity<CommandState>,
    owner: WeakEntity<Workspace>,
    state: Entity<AppState>,
    closed: bool,
    spaces: Option<SpacePicker>,
    /// The Conversation whose harness commands this palette lists instead of the app's.
    slash_commands: Option<Uuid>,
    /// The Conversation this palette offers to move to another harness.
    harnesses: Option<Uuid>,
    _subscriptions: Vec<Subscription>,
}

impl Palette {
    pub fn open(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        Self::open_with_spaces(owner, state, None, None, None, window, cx)
    }

    pub fn open_harnesses(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        thread: Uuid,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        Self::open_with_spaces(owner, state, None, None, Some(thread), window, cx)
    }

    pub fn open_slash_commands(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        thread: Uuid,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        Self::open_with_spaces(owner, state, None, Some(thread), None, window, cx)
    }

    pub fn open_spaces(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        moving: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let app = state.read(cx);
        let picker = SpacePicker {
            project: if moving { app.selected_project } else { None },
            spaces: app
                .spaces
                .iter()
                .filter(|space| !moving || space.id != app.active_space)
                .cloned()
                .collect(),
        };
        Self::open_with_spaces(owner, state, Some(picker), None, None, window, cx)
    }

    fn open_with_spaces(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        spaces: Option<SpacePicker>,
        slash_commands: Option<Uuid>,
        harnesses: Option<Uuid>,
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
                spaces,
                slash_commands,
                harnesses,
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
        let focus_owner = palette.downgrade();
        window.on_next_frame(move |window, cx| {
            if let Some(palette) = focus_owner.upgrade() {
                palette.read(cx).command.focus_handle(cx).focus(window, cx);
            }
        });
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
            && self.owner.upgrade().is_none_or(|owner| {
                command
                    .disabled_reason_in_workspace(owner.read(cx), cx)
                    .is_some()
            })
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

    fn navigate_picker(
        &mut self,
        delta: isize,
        edge: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        let count = match (&self.spaces, self.harnesses) {
            (Some(picker), _) => picker.spaces.len(),
            (None, Some(_)) => crate::harness::CONVERSATION_HARNESSES.len(),
            (None, None) => 0,
        };
        if count == 0 {
            return;
        }
        let current = self
            .command
            .read(cx)
            .selected_index()
            .map(|index| index.row);
        // The harness picker's current harness is disabled; motion steps over it.
        let skipped = self.harnesses.and_then(|thread| {
            let state = self.state.read(cx);
            let harness = &state
                .snapshot
                .threads
                .iter()
                .find(|t| t.id == thread)?
                .harness;
            crate::harness::CONVERSATION_HARNESSES
                .iter()
                .position(|candidate| candidate == harness)
        });
        let next = picker_row(count, current, delta, edge, skipped);
        self.command.update(cx, |command, cx| {
            command.set_selected_index(
                Some(gpui_kit::component::IndexPath::new(0).row(next)),
                window,
                cx,
            );
        });
    }

    fn run_space(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        let Some(picker) = &self.spaces else { return };
        let Some(target) = picker.spaces.get(row).map(|space| space.id) else {
            return;
        };
        let project = picker.project;
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
            window.close_dialog(cx);
            let result = if let Some(project) = project {
                workspace.move_project_to_space(project, target, window, cx)
            } else {
                workspace.switch_space(target, window, cx)
            };
            if let Err(error) = result {
                workspace.state.update(cx, |state, cx| {
                    state.notice(crate::app::NoticeLevel::Warning, format!("{error:#}"));
                    cx.notify();
                });
            }
            cx.notify();
        });
    }

    fn run_harness(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(thread), Some(harness)) = (
            self.harnesses,
            crate::harness::CONVERSATION_HARNESSES.get(row).copied(),
        ) else {
            return;
        };
        if self.closed {
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
            window.close_dialog(cx);
            workspace.state.update(cx, |state, cx| {
                if let Err(error) = state.switch_harness(thread, harness) {
                    state.notice(crate::app::NoticeLevel::Warning, format!("{error:#}"));
                }
                cx.notify();
            });
            cx.notify();
        });
    }

    fn run_slash_command(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.slash_commands else {
            return;
        };
        if self.closed {
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
            window.close_dialog(cx);
            workspace.insert_slash_command(thread, &name, window, cx);
            cx.notify();
        });
    }

    fn render_slash_commands(&self, thread: Uuid, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let catalog = state
            .conversations
            .get(&thread)
            .map(|session| &session.slash_commands);
        let harness = state
            .snapshot
            .threads
            .iter()
            .find(|candidate| candidate.id == thread)
            .map(|thread| crate::harness::display_name(&thread.harness).to_owned())
            .unwrap_or_default();
        let reason = catalog.map_or_else(
            || Some("Open the conversation harness first".to_owned()),
            |catalog| catalog.unavailable_reason(),
        );
        let commands: Vec<_> = catalog
            .map(|catalog| super::slash_commands::matches(catalog, ""))
            .unwrap_or_default()
            .into_iter()
            .cloned()
            .collect();
        let mut group = CommandGroup::new().label(format!("{harness} commands"));
        for command in &commands {
            let shown = command.clone();
            group = group.item(
                CommandItem::new()
                    .label(format!("/{}", command.name))
                    // The native filter does not rank, so descriptions would bury the
                    // command whose name was typed.
                    .keywords(command.aliases.iter().map(|alias| format!("/{alias}")))
                    .child(move |_, cx| super::slash_commands::row(&shown, cx)),
            );
        }
        let names: Rc<Vec<String>> =
            Rc::new(commands.into_iter().map(|command| command.name).collect());
        let confirm = cx.entity().downgrade();
        let cancel = cx.entity().downgrade();
        div()
            .id("slash-command-picker")
            .key_context("SlashCommandPicker")
            .w_full()
            .child(
                NativeCommand::new(&self.command)
                    .bordered(false)
                    .placeholder("Search commands and skills…")
                    .max_h(rems(26.))
                    .group(group)
                    .empty(move |_, _, cx| {
                        div()
                            .p_3()
                            .text_color(cx.theme().muted_foreground)
                            .child(reason.clone().unwrap_or_else(|| "No commands match".into()))
                    })
                    .on_confirm(move |index, window, cx| {
                        let Some(name) = names.get(index.row).cloned() else {
                            return;
                        };
                        let _ = confirm.update(cx, |palette, cx| {
                            palette.run_slash_command(name, window, cx)
                        });
                    })
                    .on_cancel(move |_, cx| {
                        let _ = cancel.update(cx, |palette, cx| palette.cleanup(cx));
                    }),
            )
            .into_any_element()
    }

    fn render_harnesses(&self, thread: Uuid, cx: &mut Context<Self>) -> AnyElement {
        let current = self
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .find(|candidate| candidate.id == thread)
            .map(|thread| thread.harness.clone())
            .unwrap_or_default();
        // The current harness is checked and disabled, so the highlight opens on the next one:
        // New conversation then Return switches a fresh Conversation in two keystrokes.
        let mut group = CommandGroup::new().label("Switch harness");
        for harness in crate::harness::CONVERSATION_HARNESSES {
            let name = crate::harness::display_name(harness);
            group = group.item(
                CommandItem::new()
                    .label(name)
                    .checked(harness == current)
                    .disabled(harness == current),
            );
        }
        let confirm = cx.entity().downgrade();
        let cancel = cx.entity().downgrade();
        div()
            .id("harness-picker")
            .key_context("HarnessPicker")
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| {
                this.navigate_picker(1, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                this.navigate_picker(-1, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, window, cx| {
                this.navigate_picker(-1, true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLast, window, cx| {
                this.navigate_picker(1, true, window, cx)
            }))
            // Repeating New conversation steps through the harnesses.
            .on_action(cx.listener(|this, _: &NewConversation, window, cx| {
                this.navigate_picker(1, false, window, cx)
            }))
            .child(
                NativeCommand::new(&self.command)
                    .searchable(false)
                    .bordered(false)
                    .max_h(rems(26.))
                    .group(group)
                    .on_confirm(move |index, window, cx| {
                        let _ = confirm
                            .update(cx, |palette, cx| palette.run_harness(index.row, window, cx));
                    })
                    .on_cancel(move |_, cx| {
                        let _ = cancel.update(cx, |palette, cx| palette.cleanup(cx));
                    }),
            )
            .into_any_element()
    }

    fn render_spaces(&self, picker: &SpacePicker, cx: &mut Context<Self>) -> AnyElement {
        let confirm = cx.entity().downgrade();
        let cancel = cx.entity().downgrade();
        let mut group = CommandGroup::new().label(if picker.project.is_some() {
            "Move project and all its threads to"
        } else {
            "Switch space"
        });
        for space in &picker.spaces {
            let current = space.id == self.state.read(cx).active_space;
            group = group.item(CommandItem::new().label(if current {
                format!("{} — Current", space.name)
            } else {
                space.name.clone()
            }));
        }
        div()
            .id("space-picker")
            .key_context("SpacePicker")
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| {
                this.navigate_picker(1, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                this.navigate_picker(-1, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, window, cx| {
                this.navigate_picker(-1, true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLast, window, cx| {
                this.navigate_picker(1, true, window, cx)
            }))
            .child(
                NativeCommand::new(&self.command)
                    .searchable(false)
                    .bordered(false)
                    .max_h(rems(26.))
                    .group(group)
                    .on_confirm(move |index, window, cx| {
                        let _ = confirm
                            .update(cx, |palette, cx| palette.run_space(index.row, window, cx));
                    })
                    .on_cancel(move |_, cx| {
                        let _ = cancel.update(cx, |palette, cx| palette.cleanup(cx));
                    }),
            )
            .into_any_element()
    }
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(picker) = &self.spaces {
            return self.render_spaces(picker, cx);
        }
        if let Some(thread) = self.harnesses {
            return self.render_harnesses(thread, cx);
        }
        if let Some(thread) = self.slash_commands {
            return self.render_slash_commands(thread, cx);
        }
        let state = self.state.read(cx);
        let mut targets = [Vec::new(), Vec::new(), Vec::new()];
        let mut commands = CommandGroup::new().label("Commands");
        for command in Command::ALL {
            let reason = self.owner.upgrade().map_or_else(
                || command.disabled_reason(state),
                |owner| command.disabled_reason_in_workspace(owner.read(cx), cx),
            );
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
            .filter(|project| state.project_in_active_space(project.id))
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
        for thread in
            state.snapshot.threads.iter().filter(|thread| {
                state.thread_listed_in(thread, state.active_space)
                    && (state.show_archived
                        || (!thread.archived
                            && state.snapshot.projects.iter().any(|project| {
                                project.id == thread.project_id && !project.archived
                            })))
            })
        {
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
            .on_action(cx.listener(|this, _: &NewSpace, w, cx| {
                this.run(PaletteTarget::Command(Command::NewSpace), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RenameSpace, w, cx| {
                this.run(PaletteTarget::Command(Command::RenameSpace), w, cx)
            }))
            .on_action(cx.listener(|this, _: &DeleteSpace, w, cx| {
                this.run(PaletteTarget::Command(Command::DeleteSpace), w, cx)
            }))
            .on_action(cx.listener(|this, _: &SwitchSpace, w, cx| {
                this.run(PaletteTarget::Command(Command::SwitchSpace), w, cx)
            }))
            .on_action(cx.listener(|this, _: &InsertSlashCommand, w, cx| {
                this.run(PaletteTarget::Command(Command::InsertSlashCommand), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NextSpace, w, cx| {
                this.run(PaletteTarget::Command(Command::NextSpace), w, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousSpace, w, cx| {
                this.run(PaletteTarget::Command(Command::PreviousSpace), w, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveProjectToSpace, w, cx| {
                this.run(PaletteTarget::Command(Command::MoveProjectToSpace), w, cx)
            }))
            .on_action(cx.listener(|this, _: &AddProject, w, cx| {
                this.run(PaletteTarget::Command(Command::AddProject), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewConversation, w, cx| {
                this.run(PaletteTarget::Command(Command::NewConversation), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewOmpConversation, w, cx| {
                this.run(PaletteTarget::Command(Command::NewOmpConversation), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewPiConversation, w, cx| {
                this.run(PaletteTarget::Command(Command::NewPiConversation), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewClaudeCodeConversation, w, cx| {
                this.run(
                    PaletteTarget::Command(Command::NewClaudeCodeConversation),
                    w,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &NewConversationInWorktree, w, cx| {
                this.run(
                    PaletteTarget::Command(Command::NewConversationInWorktree),
                    w,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &SwitchHarness, w, cx| {
                this.run(PaletteTarget::Command(Command::SwitchHarness), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RemoveWorktree, w, cx| {
                this.run(PaletteTarget::Command(Command::RemoveWorktree), w, cx)
            }))
            .on_action(cx.listener(|this, _: &SwitchBranch, w, cx| {
                this.run(PaletteTarget::Command(Command::SwitchBranch), w, cx)
            }))
            .on_action(cx.listener(|this, _: &InstallPi, w, cx| {
                this.run(PaletteTarget::Command(Command::InstallPi), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewFactoryThread, w, cx| {
                this.run(PaletteTarget::Command(Command::NewFactoryThread), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewTerminal, w, cx| {
                this.run(PaletteTarget::Command(Command::NewTerminal), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RestartTerminal, w, cx| {
                this.run(PaletteTarget::Command(Command::RestartTerminal), w, cx)
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
            .on_action(cx.listener(|this, _: &RegenerateTitle, w, cx| {
                this.run(PaletteTarget::Command(Command::RegenerateTitle), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ArchiveThread, w, cx| {
                this.run(PaletteTarget::Command(Command::ArchiveThread), w, cx)
            }))
            .on_action(cx.listener(|this, _: &UnarchiveThread, w, cx| {
                this.run(PaletteTarget::Command(Command::UnarchiveThread), w, cx)
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
            .on_action(cx.listener(|this, _: &SearchThreads, w, cx| {
                this.run(PaletteTarget::Command(Command::SearchThreads), w, cx)
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
            .on_action(cx.listener(|this, _: &ToggleTaskList, w, cx| {
                this.run(PaletteTarget::Command(Command::ToggleTaskList), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleBackgroundJobs, w, cx| {
                this.run(PaletteTarget::Command(Command::ToggleBackgroundJobs), w, cx)
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
            .into_any_element()
    }
}

pub fn region_action(region: Region) -> &'static str {
    match region {
        Region::Projects => "Projects",
        Region::Thread => "Thread",
    }
}

#[cfg(test)]
mod tests {
    use super::picker_row;

    #[test]
    fn harness_picker_motion_steps_over_the_current_harness() {
        // Three harnesses, the second is current: repeated steps alternate the other two.
        assert_eq!(picker_row(3, Some(0), 1, false, Some(1)), 2);
        assert_eq!(picker_row(3, Some(2), 1, false, Some(1)), 0);
        assert_eq!(picker_row(3, Some(2), -1, false, Some(1)), 0);
        assert_eq!(picker_row(3, None, 1, false, Some(0)), 1);
        assert_eq!(picker_row(3, None, -1, false, Some(2)), 1);
        // Edges land on the nearest selectable row.
        assert_eq!(picker_row(3, Some(1), -1, true, Some(0)), 1);
        assert_eq!(picker_row(3, Some(0), 1, true, Some(2)), 1);
        // Without a skipped row motion wraps plainly.
        assert_eq!(picker_row(3, Some(2), 1, false, None), 0);
    }
}
