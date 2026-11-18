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
        NewCodexConversation,
        NewOpenCodeConversation,
        NewConversationInWorktree,
        NewConversationInWorkspace,
        SwitchHarness,
        SwitchExecutionHost,
        RetryConversation,
        RetryRemoteFile,
        RemoveRemoteHost,
        PairPhone,
        RemovePhone,
        NewFactoryThread,
        NewTerminal,
        ToggleTerminal,
        GrowComposer,
        ShrinkComposer,
        RestartTerminal,
        NewNeovim,
        OpenConversationFile,
        NewBrowser,
        ToggleBrowser,
        BrowserBack,
        BrowserForward,
        BrowserReload,
        BrowserAddress,
        ToggleArtifact,
        FocusComposerArtifact,
        MoveArtifactIntoConversation,
        WidenSidebar,
        NarrowSidebar,
        RenameProject,
        ArchiveProject,
        RenameThread,
        RegenerateTitle,
        ArchiveThread,
        UnarchiveThread,
        RemoveWorktree,
        SwitchBranch,
        CommitChanges,
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
        JoinSpaceNetwork,
        LeaveSpaceNetwork,
        ResumeNetworkDelivery,
        ShowNetworkMembers,
        ShowNetworkMessages,
        ShowThreadAgents,
        ToggleTaskList,
        ToggleBackgroundJobs,
        RefreshConversationDiff,
        ToggleChangesLayout,
        OpenChangeInNeovim,
        ToggleReview,
        RefreshReview,
        OpenPullRequest,
        RefreshPullRequests,
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

pub const RESTART_NEOVIM_LABEL: &str = "Restart Neovim";

#[derive(Clone, PartialEq, Action)]
#[action(no_json)]
pub struct OpenFileReference {
    pub reference: crate::file_refs::FileReference,
    pub thread: Option<Uuid>,
}

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
    NewCodexConversation,
    NewOpenCodeConversation,
    NewConversationInWorktree,
    NewConversationInWorkspace,
    SwitchHarness,
    SwitchExecutionHost,
    RetryConversation,
    RetryRemoteFile,
    RemoveRemoteHost,
    PairPhone,
    RemovePhone,
    NewFactoryThread,
    NewTerminal,
    ToggleTerminal,
    GrowComposer,
    ShrinkComposer,
    RestartTerminal,
    NewNeovim,
    OpenConversationFile,
    NewBrowser,
    ToggleBrowser,
    BrowserBack,
    BrowserForward,
    BrowserReload,
    BrowserAddress,
    ToggleArtifact,
    FocusComposerArtifact,
    MoveArtifactIntoConversation,
    WidenSidebar,
    NarrowSidebar,
    RenameProject,
    ArchiveProject,
    RenameThread,
    RegenerateTitle,
    ArchiveThread,
    UnarchiveThread,
    RemoveWorktree,
    SwitchBranch,
    CommitChanges,
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
    JoinSpaceNetwork,
    LeaveSpaceNetwork,
    ResumeNetworkDelivery,
    ShowNetworkMembers,
    ShowNetworkMessages,
    ShowThreadAgents,
    ToggleTaskList,
    ToggleBackgroundJobs,
    RefreshConversationDiff,
    ToggleChangesLayout,
    OpenChangeInNeovim,
    ToggleReview,
    RefreshReview,
    OpenPullRequest,
    RefreshPullRequests,
    CancelEditorCommand,
    CopyDraftRecovery,
    PasteImage,
    ClearQueuedMessages,
    SendQueuedNow,
    EditQueuedMessage,
    InsertSlashCommand,
}
impl Command {
    pub const ALL: [Self; 91] = [
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
        Self::NewCodexConversation,
        Self::NewOpenCodeConversation,
        Self::NewConversationInWorktree,
        Self::NewConversationInWorkspace,
        Self::SwitchHarness,
        Self::SwitchExecutionHost,
        Self::RetryConversation,
        Self::RetryRemoteFile,
        Self::RemoveRemoteHost,
        Self::PairPhone,
        Self::RemovePhone,
        Self::NewFactoryThread,
        Self::NewTerminal,
        Self::ToggleTerminal,
        Self::GrowComposer,
        Self::ShrinkComposer,
        Self::RestartTerminal,
        Self::NewNeovim,
        Self::OpenConversationFile,
        Self::NewBrowser,
        Self::ToggleBrowser,
        Self::BrowserBack,
        Self::BrowserForward,
        Self::BrowserReload,
        Self::BrowserAddress,
        Self::ToggleArtifact,
        Self::FocusComposerArtifact,
        Self::MoveArtifactIntoConversation,
        Self::WidenSidebar,
        Self::NarrowSidebar,
        Self::RenameProject,
        Self::ArchiveProject,
        Self::RenameThread,
        Self::RegenerateTitle,
        Self::ArchiveThread,
        Self::UnarchiveThread,
        Self::RemoveWorktree,
        Self::SwitchBranch,
        Self::CommitChanges,
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
        Self::JoinSpaceNetwork,
        Self::LeaveSpaceNetwork,
        Self::ResumeNetworkDelivery,
        Self::ShowNetworkMembers,
        Self::ShowNetworkMessages,
        Self::ShowThreadAgents,
        Self::ToggleTaskList,
        Self::ToggleBackgroundJobs,
        Self::RefreshConversationDiff,
        Self::ToggleChangesLayout,
        Self::OpenChangeInNeovim,
        Self::ToggleReview,
        Self::RefreshReview,
        Self::OpenPullRequest,
        Self::RefreshPullRequests,
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
            Self::NewCodexConversation => "New Codex conversation",
            Self::NewOpenCodeConversation => "New OpenCode conversation",
            Self::NewConversationInWorktree => "New conversation in new worktree…",
            Self::NewConversationInWorkspace => "New conversation in workspace…",
            Self::SwitchHarness => "Switch harness…",
            Self::SwitchExecutionHost => "Switch execution Host…",
            Self::RetryConversation => "Reconnect conversation",
            Self::RetryRemoteFile => "Read Host file again",
            Self::RemoveRemoteHost => "Remove configured Host…",
            Self::PairPhone => "Pair phone…",
            Self::RemovePhone => "Remove phone…",
            Self::NewFactoryThread => "New Factory thread",
            Self::NewTerminal => "New terminal",
            Self::ToggleTerminal => "Toggle terminal sidebar",
            Self::GrowComposer => "Increase prompt area",
            Self::ShrinkComposer => "Decrease prompt area",
            Self::RestartTerminal => "Restart terminal",
            Self::NewNeovim => "New Neovim tab",
            Self::OpenConversationFile => "Open file from conversation…",
            Self::NewBrowser => "Open browser",
            Self::ToggleBrowser => "Toggle browser sidebar",
            Self::BrowserBack => "Browser back",
            Self::BrowserForward => "Browser forward",
            Self::BrowserReload => "Reload browser",
            Self::BrowserAddress => "Focus browser address",
            Self::ToggleArtifact => "Collapse or expand artifact",
            Self::FocusComposerArtifact => "Focus artifact above the prompt",
            Self::MoveArtifactIntoConversation => "Move artifact into conversation",
            Self::WidenSidebar => "Widen thread sidebar",
            Self::NarrowSidebar => "Narrow thread sidebar",
            Self::RenameProject => "Rename project…",
            Self::ArchiveProject => "Archive project",
            Self::RenameThread => "Rename thread…",
            Self::RegenerateTitle => "Regenerate conversation title",
            Self::ArchiveThread => "Archive thread",
            Self::UnarchiveThread => "Unarchive thread",
            Self::RemoveWorktree => "Remove worktree…",
            Self::SwitchBranch => "Switch branch…",
            Self::CommitChanges => "Commit changes…",
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
            Self::JoinSpaceNetwork => "Join network (allow automatic paid turns)",
            Self::LeaveSpaceNetwork => "Leave network",
            Self::ResumeNetworkDelivery => "Resume network delivery (allow automatic paid turns)",
            Self::ShowNetworkMembers => "Show network members",
            Self::ShowNetworkMessages => "Show network messages",
            Self::ShowThreadAgents => "Show Thread agent activity",
            Self::ToggleTaskList => "Toggle task list",
            Self::ToggleBackgroundJobs => "Toggle background jobs",
            Self::RefreshConversationDiff => "Refresh conversation changes",
            Self::ToggleChangesLayout => "Toggle side-by-side changes",
            Self::OpenChangeInNeovim => "Open change in Neovim",
            Self::ToggleReview => "Toggle pull request review",
            Self::RefreshReview => "Refresh pull request review",
            Self::OpenPullRequest => "Open pull request in browser",
            Self::RefreshPullRequests => "Refresh pull requests",
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
            Self::NewCodexConversation => Box::new(NewCodexConversation),
            Self::NewOpenCodeConversation => Box::new(NewOpenCodeConversation),
            Self::NewConversationInWorktree => Box::new(NewConversationInWorktree),
            Self::NewConversationInWorkspace => Box::new(NewConversationInWorkspace),
            Self::SwitchHarness => Box::new(SwitchHarness),
            Self::SwitchExecutionHost => Box::new(SwitchExecutionHost),
            Self::RetryConversation => Box::new(RetryConversation),
            Self::RetryRemoteFile => Box::new(RetryRemoteFile),
            Self::RemoveRemoteHost => Box::new(RemoveRemoteHost),
            Self::PairPhone => Box::new(PairPhone),
            Self::RemovePhone => Box::new(RemovePhone),
            Self::NewFactoryThread => Box::new(NewFactoryThread),
            Self::NewTerminal => Box::new(NewTerminal),
            Self::ToggleTerminal => Box::new(ToggleTerminal),
            Self::GrowComposer => Box::new(GrowComposer),
            Self::ShrinkComposer => Box::new(ShrinkComposer),
            Self::RestartTerminal => Box::new(RestartTerminal),
            Self::NewNeovim => Box::new(NewNeovim),
            Self::OpenConversationFile => Box::new(OpenConversationFile),
            Self::NewBrowser => Box::new(NewBrowser),
            Self::ToggleBrowser => Box::new(ToggleBrowser),
            Self::BrowserBack => Box::new(BrowserBack),
            Self::BrowserForward => Box::new(BrowserForward),
            Self::BrowserReload => Box::new(BrowserReload),
            Self::BrowserAddress => Box::new(BrowserAddress),
            Self::ToggleArtifact => Box::new(ToggleArtifact),
            Self::FocusComposerArtifact => Box::new(FocusComposerArtifact),
            Self::MoveArtifactIntoConversation => Box::new(MoveArtifactIntoConversation),
            Self::WidenSidebar => Box::new(WidenSidebar),
            Self::NarrowSidebar => Box::new(NarrowSidebar),
            Self::RenameProject => Box::new(RenameProject),
            Self::ArchiveProject => Box::new(ArchiveProject),
            Self::RenameThread => Box::new(RenameThread),
            Self::RegenerateTitle => Box::new(RegenerateTitle),
            Self::ArchiveThread => Box::new(ArchiveThread),
            Self::UnarchiveThread => Box::new(UnarchiveThread),
            Self::RemoveWorktree => Box::new(RemoveWorktree),
            Self::SwitchBranch => Box::new(SwitchBranch),
            Self::CommitChanges => Box::new(CommitChanges),
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
            Self::JoinSpaceNetwork => Box::new(JoinSpaceNetwork),
            Self::LeaveSpaceNetwork => Box::new(LeaveSpaceNetwork),
            Self::ResumeNetworkDelivery => Box::new(ResumeNetworkDelivery),
            Self::ShowNetworkMembers => Box::new(ShowNetworkMembers),
            Self::ShowNetworkMessages => Box::new(ShowNetworkMessages),
            Self::ShowThreadAgents => Box::new(ShowThreadAgents),
            Self::ToggleTaskList => Box::new(ToggleTaskList),
            Self::ToggleBackgroundJobs => Box::new(ToggleBackgroundJobs),
            Self::RefreshConversationDiff => Box::new(RefreshConversationDiff),
            Self::ToggleChangesLayout => Box::new(ToggleChangesLayout),
            Self::OpenChangeInNeovim => Box::new(OpenChangeInNeovim),
            Self::ToggleReview => Box::new(ToggleReview),
            Self::RefreshReview => Box::new(RefreshReview),
            Self::OpenPullRequest => Box::new(OpenPullRequest),
            Self::RefreshPullRequests => Box::new(RefreshPullRequests),
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
            return if state.thread().is_some_and(|thread| {
                state.thread_sidebar_tab(thread.id) == crate::app::SidebarTab::Terminal
                    && super::workspace::thread_terminal_location(state, thread.id).is_ok()
            }) {
                None
            } else {
                Some("Select a terminal first.".into())
            };
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
            | Self::NewCodexConversation
            | Self::NewOpenCodeConversation
            | Self::RenameProject => state.mutable_project_reason(),
            Self::PairPhone => state.pair_phone_reason(),
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
            Self::NewConversationInWorktree | Self::NewConversationInWorkspace => {
                state.create_worktree_reason()
            }
            Self::SwitchHarness => state.switch_harness_reason(),
            Self::SwitchExecutionHost => match state.conversation_thread() {
                Some(thread) => state.thread_host_selection_reason(thread.id),
                None => Some("Select a Conversation first.".into()),
            },
            Self::RetryConversation => match state.conversation_thread() {
                None => Some("Select a Conversation first.".into()),
                Some(thread) if thread.archived => Some("Archived threads are read-only.".into()),
                Some(thread)
                    if thread.remote.is_none() && state.active_threads.contains(&thread.id) =>
                {
                    Some("Wait for the active turn or interrupt it before reconnecting.".into())
                }
                Some(thread)
                    if state
                        .conversations
                        .get(&thread.id)
                        .is_some_and(|session| session.error.is_none()) =>
                {
                    Some("The Conversation is connected or connecting.".into())
                }
                Some(_) => None,
            },
            Self::RetryRemoteFile => match &state.remote_file {
                None => Some("Open a Host file first.".into()),
                Some(file) if file.loading => Some("The Host file is loading.".into()),
                Some(_) => None,
            },
            Self::RemoveWorktree => state.remove_worktree_reason(),
            Self::SwitchBranch => state.switch_branch_reason(),
            Self::CommitChanges => state.commit_reason(),
            Self::ToggleTerminal => state.thread().map_or_else(
                || Some("Select a thread first".into()),
                |thread| {
                    if state.thread_sidebar_open(thread.id)
                        && state.thread_sidebar_tab(thread.id) == crate::app::SidebarTab::Terminal
                    {
                        None
                    } else {
                        super::workspace::thread_terminal_location(state, thread.id).err()
                    }
                },
            ),
            Self::GrowComposer | Self::ShrinkComposer => {
                match state.conversation_thread() {
                    None => Some("Select a conversation first".into()),
                    Some(thread)
                        if thread.archived
                            || state.snapshot.projects.iter().any(|project| {
                                project.id == thread.project_id && project.archived
                            }) =>
                    {
                        Some("Archived conversations have no prompt area".into())
                    }
                    Some(_) => None,
                }
            }
            Self::CloseThread | Self::FocusOutput | Self::ToggleOutput
                if state.thread().is_none() =>
            {
                Some("Select a thread first.".into())
            }
            Self::NextThread | Self::PreviousThread if state.open_threads.len() < 2 => {
                Some("Open at least two threads.".into())
            }
            Self::NewTerminal | Self::NewNeovim
                if state
                    .conversation_thread()
                    .is_some_and(|thread| thread.remote.is_some()) =>
            {
                state
                    .conversation_thread()
                    .and_then(|thread| state.remote_action_reason(thread.id))
            }
            Self::NewTerminal | Self::NewNeovim => state.mutable_project_reason().or_else(|| {
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
            | Self::OpenConversationFile
            | Self::ToggleBrowser
            | Self::ToggleAgents
            | Self::ShowNetworkMembers
            | Self::ShowNetworkMessages
            | Self::ShowThreadAgents
            | Self::ToggleTaskList
            | Self::ToggleBackgroundJobs
            | Self::RefreshConversationDiff
            | Self::ToggleChangesLayout
            | Self::OpenChangeInNeovim
                if state.conversation_thread().is_none() =>
            {
                Some("Select a conversation first.".into())
            }
            Self::JoinSpaceNetwork | Self::LeaveSpaceNetwork | Self::ResumeNetworkDelivery => {
                match state.conversation_thread() {
                    None => Some("Select a Conversation first".into()),
                    Some(thread) => match self {
                        Self::JoinSpaceNetwork => state.network_join_reason(thread.id),
                        Self::ResumeNetworkDelivery => state.network_resume_reason(thread.id),
                        Self::LeaveSpaceNetwork => {
                            state.network_membership_reason(thread.id).or_else(|| {
                                (!state.network_joined(thread.id))
                                    .then(|| "This agent has not joined the Space network".into())
                            })
                        }
                        _ => unreachable!(),
                    },
                }
            }
            Self::ToggleReview | Self::RefreshReview | Self::OpenPullRequest => {
                state.pull_request_reason()
            }
            Self::RefreshPullRequests => state.refresh_pull_requests_reason(),
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

    pub fn disabled_reason_in_workspace(self, workspace: &Workspace, cx: &App) -> Option<String> {
        let state = workspace.state.read(cx);
        if state.shutting_down {
            return Some("Application is shutting down.".into());
        }
        match self {
            Self::RemoveRemoteHost => workspace.selected_remote_host(cx).map_or_else(
                || Some("Choose a configured Host in Settings first.".into()),
                |host| state.remove_remote_host_reason(&host),
            ),
            Self::RemovePhone => workspace.selected_phone(cx).map_or_else(
                || Some("Choose a paired phone in Settings › Phones first.".into()),
                |phone| state.remove_phone_reason(phone),
            ),
            Self::OpenChangeInNeovim => workspace.open_change_reason(cx),
            Self::WidenSidebar | Self::NarrowSidebar if state.thread().is_none() => {
                Some("Select a thread first".into())
            }
            Self::CloseThread if workspace.browsers.active.is_some() => None,
            Self::BrowserBack
            | Self::BrowserForward
            | Self::BrowserReload
            | Self::BrowserAddress
            | Self::ToggleArtifact => match workspace.selected_browser(cx) {
                Some(pane) => pane.read(cx).disabled_reason(self),
                None => Some("Open a browser first".into()),
            },
            Self::FocusComposerArtifact | Self::MoveArtifactIntoConversation => {
                match state.conversation_thread() {
                    None => Some("Select a Conversation first".into()),
                    Some(thread) => workspace
                        .docked_artifact(thread.id)
                        .is_none()
                        .then(|| "No artifact is shown above the prompt".into()),
                }
            }
            Self::CloseThread if workspace.has_active_terminal(cx) => None,
            Self::NextThread | Self::PreviousThread => {
                (workspace.center_panel_count(cx) < 2).then(|| "Open at least two tabs.".into())
            }
            Self::RestartTerminal
                if workspace.has_active_terminal(cx)
                    || state.thread().is_some_and(|thread| {
                        state.thread_sidebar_tab(thread.id) == crate::app::SidebarTab::Terminal
                            && workspace.thread_terminals.contains_key(&thread.id)
                    }) =>
            {
                None
            }
            Self::ToggleTerminal => state.thread().map_or_else(
                || Some("Select a thread first".into()),
                |thread| {
                    if workspace.sidebar_terminal(thread.id).is_some()
                        || (state.thread_sidebar_open(thread.id)
                            && state.thread_sidebar_tab(thread.id)
                                == crate::app::SidebarTab::Terminal)
                    {
                        None
                    } else {
                        workspace
                            .thread_terminals
                            .get(&thread.id)
                            .and_then(|entry| entry.error.clone())
                            .or_else(|| self.disabled_reason(state))
                    }
                },
            ),
            _ => self.disabled_reason(state),
        }
    }
}

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

pub const PASTE_IMAGE_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-v"
} else {
    "ctrl-shift-v"
};

pub const CLOSE_THREAD_KEY: &str = if cfg!(target_os = "macos") {
    "cmd-w"
} else {
    "ctrl-shift-w"
};

#[cfg(target_os = "macos")]
macro_rules! secondary {
    ($keys:literal) => {
        concat!("cmd-", $keys)
    };
}

#[cfg(not(target_os = "macos"))]
macro_rules! secondary {
    ($keys:literal) => {
        concat!("ctrl-", $keys)
    };
}

pub fn bind_keys(cx: &mut App) {
    let workspace = Some("Workspace && !Command && !PopupMenu && !Sheet && !Dialog");
    let command = Some("DrostjePalette");
    let picker = Some("SpacePicker || HarnessPicker || HostPicker");
    let navigation = Some(
        "Workspace && !Input && !NativeInput && !Browser && !Command && !PopupMenu && !Sheet && !Dialog",
    );
    let pane_navigation = Some(
        "Workspace && !Command && !DrostjePalette && !SpacePicker && !HarnessPicker && !HostPicker && !PopupMenu && !Sheet && !Dialog",
    );
    let list =
        Some("List && !Input && !NativeInput && !Command && !PopupMenu && !Sheet && !Dialog");
    let changes = Some(
        "ChangesList && !Input && !NativeInput && !Command && !PopupMenu && !Sheet && !Dialog",
    );
    let projects = Some(
        "ProjectsList && !Input && !NativeInput && !Command && !PopupMenu && !Sheet && !Dialog",
    );
    let thread = Some(
        "Thread && !Input && !NativeInput && !Browser && !Command && !PopupMenu && !Sheet && !Dialog",
    );
    let submit = Some(
        "(Thread || PromptEditor || FactoryThreadForm) && (!NativeSurface || PromptEditor) && !Terminal && !TerminalPane && !Browser && !BrowserPane && !Command && !PopupMenu && !Sheet && !Dialog",
    );
    let caret_popup = Some("PromptEditor && CaretPopup && !Command && !Sheet && !Dialog");
    cx.bind_keys(vec![
        KeyBinding::new(secondary!("shift-p"), OpenPalette, workspace),
        KeyBinding::new(secondary!("alt-]"), NextSpace, navigation),
        KeyBinding::new(secondary!("alt-["), PreviousSpace, navigation),
        KeyBinding::new("j", SelectNext, picker),
        KeyBinding::new("k", SelectPrevious, picker),
        KeyBinding::new("g g", SelectFirst, picker),
        KeyBinding::new("shift-g", SelectLast, picker),
        KeyBinding::new("home", SelectFirst, picker),
        KeyBinding::new("end", SelectLast, picker),
        KeyBinding::new(secondary!("n"), NewConversation, Some("HarnessPicker")),
        KeyBinding::new("ctrl-tab", NextThread, workspace),
        KeyBinding::new("ctrl-shift-tab", PreviousThread, workspace),
        KeyBinding::new(CLOSE_THREAD_KEY, CloseThread, workspace),
        KeyBinding::new(secondary!("1"), FocusProjects, workspace),
        KeyBinding::new(secondary!("2"), FocusThread, workspace),
        KeyBinding::new(secondary!("3"), FocusOutput, workspace),
        KeyBinding::new(secondary!("b"), ToggleProjects, workspace),
        KeyBinding::new(secondary!("j"), ToggleOutput, workspace),
        KeyBinding::new(secondary!("n"), NewConversation, workspace),
        KeyBinding::new(secondary!("alt-n"), NewConversationInWorktree, workspace),
        KeyBinding::new(secondary!("shift-n"), NewFactoryThread, workspace),
        KeyBinding::new(secondary!("alt-t"), NewTerminal, workspace),
        KeyBinding::new(secondary!("alt-o"), OpenConversationFile, workspace),
        KeyBinding::new(secondary!("l"), BrowserAddress, Some("BrowserPane || Browser")),
        KeyBinding::new(secondary!("r"), BrowserReload, Some("BrowserPane || Browser")),
        KeyBinding::new(secondary!("shift-a"), ToggleArtifact, Some("BrowserPane")),
        KeyBinding::new("enter", ToggleArtifact, Some("ArtifactDisclosure")),
        KeyBinding::new("space", ToggleArtifact, Some("ArtifactDisclosure")),
        KeyBinding::new(secondary!("escape"), LeaveEditor, Some("Browser")),
        KeyBinding::new(secondary!(","), Settings, workspace),
        KeyBinding::new(secondary!("q"), Quit, None),
        KeyBinding::new(secondary!("1"), FocusProjects, command),
        KeyBinding::new(secondary!("2"), FocusThread, command),
        KeyBinding::new(secondary!("3"), FocusOutput, command),
        KeyBinding::new(secondary!("b"), ToggleProjects, command),
        KeyBinding::new(secondary!("j"), ToggleOutput, command),
        KeyBinding::new(secondary!("n"), NewConversation, command),
        KeyBinding::new(secondary!("alt-n"), NewConversationInWorktree, command),
        KeyBinding::new(secondary!("shift-n"), NewFactoryThread, command),
        KeyBinding::new(secondary!("alt-t"), NewTerminal, command),
        KeyBinding::new(secondary!("alt-o"), OpenConversationFile, command),
        KeyBinding::new(secondary!(","), Settings, command),
        KeyBinding::new("ctrl-tab", NextThread, command),
        KeyBinding::new("ctrl-shift-tab", PreviousThread, command),
        KeyBinding::new(CLOSE_THREAD_KEY, CloseThread, command),
        KeyBinding::new(
            "tab",
            NoAction,
            Some("Terminal || (PromptEditor && !CaretPopup)"),
        ),
        KeyBinding::new("shift-tab", NoAction, Some("Terminal || PromptEditor")),
        KeyBinding::new(secondary!("alt-h"), FocusLeft, pane_navigation),
        KeyBinding::new(secondary!("alt-j"), FocusDown, pane_navigation),
        KeyBinding::new(secondary!("alt-k"), FocusUp, pane_navigation),
        KeyBinding::new(secondary!("alt-l"), FocusRight, pane_navigation),
        KeyBinding::new(secondary!("alt-left"), FocusLeft, pane_navigation),
        KeyBinding::new(secondary!("alt-down"), FocusDown, pane_navigation),
        KeyBinding::new(secondary!("alt-up"), FocusUp, pane_navigation),
        KeyBinding::new(secondary!("alt-right"), FocusRight, pane_navigation),
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
        KeyBinding::new("enter", OpenChangeInNeovim, changes),
        KeyBinding::new("o", OpenChangeInNeovim, changes),
        KeyBinding::new("s", ToggleChangesLayout, changes),
        KeyBinding::new("escape", Dismiss, Some("Network && !Command && !PopupMenu && !Sheet && !Dialog")),
        KeyBinding::new("h", Dismiss, Some("Network && !Command && !PopupMenu && !Sheet && !Dialog")),
        KeyBinding::new("left", Dismiss, Some("Network && !Command && !PopupMenu && !Sheet && !Dialog")),
        KeyBinding::new("l", OpenSelection, Some("Network && !Command && !PopupMenu && !Sheet && !Dialog")),
        KeyBinding::new("right", OpenSelection, Some("Network && !Command && !PopupMenu && !Sheet && !Dialog")),
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
        KeyBinding::new(PASTE_IMAGE_KEY, PasteImage, thread),
        KeyBinding::new(
            secondary!("escape"),
            LeaveEditor,
            Some("NativeSurface && !Command && !PopupMenu && !Dialog && !Sheet"),
        ),
        KeyBinding::new(secondary!("enter"), Submit, submit),
        KeyBinding::new("ctrl-n", NextCompletion, caret_popup),
        KeyBinding::new("down", NextCompletion, caret_popup),
        KeyBinding::new("ctrl-p", PreviousCompletion, caret_popup),
        KeyBinding::new("up", PreviousCompletion, caret_popup),
        KeyBinding::new("tab", AcceptCompletion, caret_popup),
        KeyBinding::new("enter", AcceptCompletion, caret_popup),
        KeyBinding::new("ctrl-e", DismissCompletion, caret_popup),
        KeyBinding::new("escape", DismissCompletion, caret_popup),
        KeyBinding::new(
            secondary!("enter"),
            Submit,
            Some("Input && PromptEditor && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            secondary!("."),
            Interrupt,
            Some("Input && PromptEditor && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            secondary!("."),
            Interrupt,
            Some("(Thread || PromptEditor) && !Browser && !BrowserPane && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            secondary!("alt-backspace"),
            ClearQueuedMessages,
            Some("Input && PromptEditor && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            secondary!("alt-backspace"),
            ClearQueuedMessages,
            Some("(Thread || PromptEditor) && !Browser && !BrowserPane && !Command && !PopupMenu && !Sheet && !Dialog"),
        ),
        KeyBinding::new(
            secondary!("enter"),
            gpui_kit::base::actions::Confirm { secondary: false },
            Some("Dialog && !DrostjeForm && !Command && !PopupMenu"),
        ),
        KeyBinding::new(
            secondary!("enter"),
            Confirm,
            Some("DrostjeForm && !Command && !PopupMenu"),
        ),
        KeyBinding::new(
            secondary!("enter"),
            Confirm,
            Some("Input && PromptEditor && DrostjeForm && !Command && !PopupMenu"),
        ),
        KeyBinding::new("escape", Dismiss, Some("InlineRename > Input")),
        KeyBinding::new(secondary!("enter"), Confirm, Some("InlineRename > Input")),
        KeyBinding::new(
            secondary!("enter"),
            Confirm,
            Some("CommitMessage > Input && !Command && !PopupMenu"),
        ),
    ]);
    crate::ui::theme::editor::bind_keys(cx);
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

struct FilePicker {
    thread: Uuid,
    references: Vec<crate::file_refs::FileReference>,
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
    hosted: bool,
    spaces: Option<SpacePicker>,
    slash_commands: Option<Uuid>,
    harnesses: Option<Uuid>,
    hosts: Option<Uuid>,
    files: Option<FilePicker>,
    _subscriptions: Vec<Subscription>,
}

impl Palette {
    pub fn open(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        Self::open_with_spaces(owner, state, None, None, None, None, window, cx)
    }

    pub fn open_harnesses(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        thread: Uuid,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        Self::open_with_spaces(owner, state, None, None, Some(thread), None, window, cx)
    }

    pub fn open_hosts(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        thread: Uuid,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let palette = Self::open(owner, state, window, cx);
        palette.update(cx, |palette, cx| {
            palette.hosts = Some(thread);
            cx.notify();
        });
        palette
    }

    pub fn open_slash_commands(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        thread: Uuid,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        Self::open_with_spaces(owner, state, None, Some(thread), None, None, window, cx)
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
        Self::open_with_spaces(owner, state, Some(picker), None, None, None, window, cx)
    }

    pub fn open_files(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        thread: Uuid,
        references: Vec<crate::file_refs::FileReference>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let files = FilePicker { thread, references };
        Self::open_with_spaces(owner, state, None, None, None, Some(files), window, cx)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one constructor for every picker mode"
    )]
    fn open_with_spaces(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        spaces: Option<SpacePicker>,
        slash_commands: Option<Uuid>,
        harnesses: Option<Uuid>,
        files: Option<FilePicker>,
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
                hosted: false,
                hosts: None,
                spaces,
                slash_commands,
                harnesses,
                files,
                _subscriptions: subscriptions,
            }
        });
        palette.update(cx, |palette, cx| palette.reopen(window, cx));
        palette
    }

    pub(super) fn hosted(&self) -> bool {
        self.hosted
    }

    pub(super) fn reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed || self.hosted {
            return;
        }
        self.hosted = true;
        let content = cx.entity();
        let close_owner = content.downgrade();
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
        let focus_owner = cx.entity().downgrade();
        let host_focus = window.focused(cx);
        window.on_next_frame(move |window, cx| {
            if let Some(palette) = focus_owner.upgrade() {
                let palette = palette.read(cx);
                if palette.hosted
                    && !palette.closed
                    && host_focus
                        .as_ref()
                        .is_some_and(|focus| focus.contains_focused(window, cx))
                {
                    palette.command.focus_handle(cx).focus(window, cx);
                }
            }
        });
    }

    pub(super) fn suspend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.hosted {
            self.hosted = false;
            window.close_dialog(cx);
        }
    }

    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.closed {
            self.closed = true;
            self.suspend(window, cx);
        }
    }

    fn cleanup(&mut self, cx: &mut Context<Self>) {
        if self.closed || !self.hosted {
            return;
        }
        self.closed = true;
        self.hosted = false;
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
        if self.closed || !self.hosted || self.state.read(cx).shutting_down {
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
        self.hosted = false;
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
        let hosts = self.hosts.map(|_| self.state.read(cx).remote_hosts());
        let count = if let Some(hosts) = &hosts {
            hosts.len() + 2
        } else {
            match (&self.spaces, self.harnesses) {
                (Some(picker), _) => picker.spaces.len(),
                (None, Some(_)) => crate::harness::CONVERSATION_HARNESSES.len(),
                (None, None) => 0,
            }
        };
        if count == 0 {
            return;
        }
        let current = self
            .command
            .read(cx)
            .selected_index()
            .map(|index| index.row);
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
        let mut next = picker_row(count, current, delta, edge, skipped);
        if let (Some(thread), Some(hosts)) = (self.hosts, &hosts) {
            let project = self
                .state
                .read(cx)
                .snapshot
                .threads
                .iter()
                .find(|candidate| candidate.id == thread)
                .map(|thread| thread.project_id.to_string());
            while next > 0
                && next <= hosts.len()
                && project
                    .as_ref()
                    .is_none_or(|project| !hosts[next - 1].projects.contains_key(project))
            {
                next = picker_row(count, Some(next), delta, false, None);
            }
        }
        self.command.update(cx, |command, cx| {
            command.set_selected_index(
                Some(gpui_kit::component::IndexPath::new(0).row(next)),
                window,
                cx,
            );
        });
    }

    fn run_space(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed || !self.hosted {
            return;
        }
        let Some(picker) = &self.spaces else { return };
        let Some(target) = picker.spaces.get(row).map(|space| space.id) else {
            return;
        };
        let project = picker.project;
        self.closed = true;
        self.hosted = false;
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
        if self.closed || !self.hosted {
            return;
        }
        self.closed = true;
        self.hosted = false;
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

    fn run_host(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.hosts else { return };
        let hosts = self.state.read(cx).remote_hosts();
        if row == hosts.len() + 1 {
            self.run(PaletteTarget::Command(Command::Settings), window, cx);
            return;
        }
        let host = if row == 0 {
            None
        } else {
            let Some(host) = hosts.get(row - 1) else {
                return;
            };
            Some(host.id.clone())
        };
        if self.closed || !self.hosted {
            return;
        }
        let result = self.state.update(cx, |state, cx| {
            let result = state.set_thread_host(thread, host.as_deref());
            if let Err(error) = &result {
                state.notice(crate::app::NoticeLevel::Warning, format!("{error:#}"));
            }
            cx.notify();
            result
        });
        if result.is_ok() {
            self.closed = true;
            self.hosted = false;
            let id = cx.entity().entity_id();
            let _ = self.owner.update(cx, |workspace, cx| {
                if workspace
                    .palette
                    .as_ref()
                    .is_some_and(|palette| palette.entity_id() == id)
                {
                    workspace.palette = None;
                    window.close_dialog(cx);
                    cx.notify();
                }
            });
        }
    }

    fn render_hosts(&self, thread: Uuid, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let conversation = state
            .snapshot
            .threads
            .iter()
            .find(|candidate| candidate.id == thread);
        let current = conversation.and_then(|thread| thread.remote.as_ref());
        let mut group = CommandGroup::new()
            .label("Execution location · fixed after the first turn")
            .item(
                CommandItem::new()
                    .label("Local · this computer")
                    .checked(current.is_none()),
            );
        for host in state.remote_hosts() {
            let mapping =
                conversation.and_then(|thread| host.projects.get(&thread.project_id.to_string()));
            let label = match mapping {
                Some(project) => format!("{} · {} · {project}", host.name, host.id),
                None => format!("{} · map this Project in Settings first", host.name),
            };
            group = group.item(
                CommandItem::new()
                    .label(label)
                    .checked(current.is_some_and(|location| location.host == host.id))
                    .disabled(mapping.is_none()),
            );
        }
        group = group.item(CommandItem::new().label("Configure Hosts in Settings…"));
        let confirm = cx.entity().downgrade();
        let cancel = cx.entity().downgrade();
        div()
            .id("execution-host-picker")
            .test_support()
            .key_context("HostPicker")
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
                            .update(cx, |palette, cx| palette.run_host(index.row, window, cx));
                    })
                    .on_cancel(move |_, cx| {
                        let _ = cancel.update(cx, |palette, cx| palette.cleanup(cx));
                    }),
            )
            .into_any_element()
    }

    fn run_slash_command(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.slash_commands else {
            return;
        };
        if self.closed || !self.hosted {
            return;
        }
        self.closed = true;
        self.hosted = false;
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

    fn run_file(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread, reference)) = self
            .files
            .as_ref()
            .and_then(|picker| Some((picker.thread, picker.references.get(row)?.clone())))
        else {
            return;
        };
        if self.closed || !self.hosted {
            return;
        }
        self.closed = true;
        self.hosted = false;
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
            workspace.open_file_reference(reference, Some(thread), window, cx);
            cx.notify();
        });
    }

    fn render_files(&self, picker: &FilePicker, cx: &mut Context<Self>) -> AnyElement {
        let mut group = CommandGroup::new().label("Files in this conversation");
        for reference in &picker.references {
            group = group.item(CommandItem::new().label(reference.label()));
        }
        let confirm = cx.entity().downgrade();
        let cancel = cx.entity().downgrade();
        div()
            .id("conversation-file-picker")
            .key_context("ConversationFilePicker")
            .w_full()
            .child(
                NativeCommand::new(&self.command)
                    .bordered(false)
                    .placeholder("Search files…")
                    .max_h(rems(26.))
                    .group(group)
                    .empty(|_, _, cx| {
                        div()
                            .p_3()
                            .text_color(cx.theme().muted_foreground)
                            .child("No files mentioned or touched in this conversation yet")
                    })
                    .on_confirm(move |index, window, cx| {
                        let _ = confirm
                            .update(cx, |palette, cx| palette.run_file(index.row, window, cx));
                    })
                    .on_cancel(move |_, cx| {
                        let _ = cancel.update(cx, |palette, cx| palette.cleanup(cx));
                    }),
            )
            .into_any_element()
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
            .map(|catalog| crate::ui::conversation::slash_commands::matches(catalog, ""))
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
                    .keywords(command.aliases.iter().map(|alias| format!("/{alias}")))
                    .child(move |_, cx| crate::ui::conversation::slash_commands::row(&shown, cx)),
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
        if let Some(thread) = self.hosts {
            return self.render_hosts(thread, cx);
        }
        if let Some(picker) = &self.spaces {
            return self.render_spaces(picker, cx);
        }
        if let Some(picker) = &self.files {
            return self.render_files(picker, cx);
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
                if matches!(target, PaletteTarget::Command(_)) {
                    return;
                }
                let _ = confirm_owner.update(cx, |palette, cx| palette.run(target, window, cx));
            })
            .on_cancel(move |_, cx| {
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
            .on_action(cx.listener(|this, _: &NewCodexConversation, w, cx| {
                this.run(PaletteTarget::Command(Command::NewCodexConversation), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewOpenCodeConversation, w, cx| {
                this.run(
                    PaletteTarget::Command(Command::NewOpenCodeConversation),
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
            .on_action(cx.listener(|this, _: &NewConversationInWorkspace, w, cx| {
                this.run(
                    PaletteTarget::Command(Command::NewConversationInWorkspace),
                    w,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &SwitchHarness, w, cx| {
                this.run(PaletteTarget::Command(Command::SwitchHarness), w, cx)
            }))
            .on_action(cx.listener(|this, _: &SwitchExecutionHost, w, cx| {
                this.run(PaletteTarget::Command(Command::SwitchExecutionHost), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RetryConversation, w, cx| {
                this.run(PaletteTarget::Command(Command::RetryConversation), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RetryRemoteFile, w, cx| {
                this.run(PaletteTarget::Command(Command::RetryRemoteFile), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RemoveRemoteHost, w, cx| {
                this.run(PaletteTarget::Command(Command::RemoveRemoteHost), w, cx)
            }))
            .on_action(cx.listener(|this, _: &PairPhone, w, cx| {
                this.run(PaletteTarget::Command(Command::PairPhone), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RemovePhone, w, cx| {
                this.run(PaletteTarget::Command(Command::RemovePhone), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RemoveWorktree, w, cx| {
                this.run(PaletteTarget::Command(Command::RemoveWorktree), w, cx)
            }))
            .on_action(cx.listener(|this, _: &SwitchBranch, w, cx| {
                this.run(PaletteTarget::Command(Command::SwitchBranch), w, cx)
            }))
            .on_action(cx.listener(|this, _: &CommitChanges, w, cx| {
                this.run(PaletteTarget::Command(Command::CommitChanges), w, cx)
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
            .on_action(cx.listener(|this, _: &ToggleTerminal, w, cx| {
                this.run(PaletteTarget::Command(Command::ToggleTerminal), w, cx)
            }))
            .on_action(cx.listener(|this, _: &GrowComposer, w, cx| {
                this.run(PaletteTarget::Command(Command::GrowComposer), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ShrinkComposer, w, cx| {
                this.run(PaletteTarget::Command(Command::ShrinkComposer), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RestartTerminal, w, cx| {
                this.run(PaletteTarget::Command(Command::RestartTerminal), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewNeovim, w, cx| {
                this.run(PaletteTarget::Command(Command::NewNeovim), w, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenConversationFile, w, cx| {
                this.run(PaletteTarget::Command(Command::OpenConversationFile), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewBrowser, w, cx| {
                this.run(PaletteTarget::Command(Command::NewBrowser), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleBrowser, w, cx| {
                this.run(PaletteTarget::Command(Command::ToggleBrowser), w, cx)
            }))
            .on_action(cx.listener(|this, _: &BrowserBack, w, cx| {
                this.run(PaletteTarget::Command(Command::BrowserBack), w, cx)
            }))
            .on_action(cx.listener(|this, _: &BrowserForward, w, cx| {
                this.run(PaletteTarget::Command(Command::BrowserForward), w, cx)
            }))
            .on_action(cx.listener(|this, _: &BrowserReload, w, cx| {
                this.run(PaletteTarget::Command(Command::BrowserReload), w, cx)
            }))
            .on_action(cx.listener(|this, _: &BrowserAddress, w, cx| {
                this.run(PaletteTarget::Command(Command::BrowserAddress), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleArtifact, w, cx| {
                this.run(PaletteTarget::Command(Command::ToggleArtifact), w, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusComposerArtifact, w, cx| {
                this.run(
                    PaletteTarget::Command(Command::FocusComposerArtifact),
                    w,
                    cx,
                )
            }))
            .on_action(
                cx.listener(|this, _: &MoveArtifactIntoConversation, w, cx| {
                    this.run(
                        PaletteTarget::Command(Command::MoveArtifactIntoConversation),
                        w,
                        cx,
                    )
                }),
            )
            .on_action(cx.listener(|this, _: &WidenSidebar, w, cx| {
                this.run(PaletteTarget::Command(Command::WidenSidebar), w, cx)
            }))
            .on_action(cx.listener(|this, _: &NarrowSidebar, w, cx| {
                this.run(PaletteTarget::Command(Command::NarrowSidebar), w, cx)
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
            .on_action(cx.listener(|this, _: &ToggleAgents, w, cx| {
                this.run(PaletteTarget::Command(Command::ToggleAgents), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleChangesLayout, w, cx| {
                this.run(PaletteTarget::Command(Command::ToggleChangesLayout), w, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenChangeInNeovim, w, cx| {
                this.run(PaletteTarget::Command(Command::OpenChangeInNeovim), w, cx)
            }))
            .on_action(cx.listener(|this, _: &JoinSpaceNetwork, w, cx| {
                this.run(PaletteTarget::Command(Command::JoinSpaceNetwork), w, cx)
            }))
            .on_action(cx.listener(|this, _: &LeaveSpaceNetwork, w, cx| {
                this.run(PaletteTarget::Command(Command::LeaveSpaceNetwork), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ResumeNetworkDelivery, w, cx| {
                this.run(
                    PaletteTarget::Command(Command::ResumeNetworkDelivery),
                    w,
                    cx,
                )
            }))
            .on_action(cx.listener(|this, _: &ShowNetworkMembers, w, cx| {
                this.run(PaletteTarget::Command(Command::ShowNetworkMembers), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowNetworkMessages, w, cx| {
                this.run(PaletteTarget::Command(Command::ShowNetworkMessages), w, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowThreadAgents, w, cx| {
                this.run(PaletteTarget::Command(Command::ShowThreadAgents), w, cx)
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
            .on_action(cx.listener(|this, _: &ToggleReview, w, cx| {
                this.run(PaletteTarget::Command(Command::ToggleReview), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RefreshReview, w, cx| {
                this.run(PaletteTarget::Command(Command::RefreshReview), w, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenPullRequest, w, cx| {
                this.run(PaletteTarget::Command(Command::OpenPullRequest), w, cx)
            }))
            .on_action(cx.listener(|this, _: &RefreshPullRequests, w, cx| {
                this.run(PaletteTarget::Command(Command::RefreshPullRequests), w, cx)
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

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn native_surface_keys_bypass_workspace_navigation(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::{
            AppContext, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
            ParentElement, Render, TestSupportExt, Window, component::Root, div,
            test::TestWindowExt,
        };

        struct NativeInputs {
            context: &'static str,
            input: FocusHandle,
            neighbours: [FocusHandle; 4],
            navigation: Vec<&'static str>,
            keys: Vec<String>,
        }
        impl Render for NativeInputs {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .key_context(if self.context.starts_with("Terminal") {
                        "Workspace Thread List TerminalPane"
                    } else {
                        "Workspace Thread List"
                    })
                    .on_action(cx.listener(|this, _: &super::OpenPalette, _, _| {
                        this.keys.push("palette".into());
                    }))
                    .on_action(cx.listener(|this, _: &super::LeaveEditor, window, cx| {
                        this.keys.push("leave".into());
                        this.neighbours[0].focus(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &super::FocusLeft, window, cx| {
                        this.navigation.push("left");
                        this.neighbours[0].focus(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &super::FocusDown, window, cx| {
                        this.navigation.push("down");
                        this.neighbours[1].focus(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &super::FocusUp, window, cx| {
                        this.navigation.push("up");
                        this.neighbours[2].focus(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &super::FocusRight, window, cx| {
                        this.navigation.push("right");
                        this.neighbours[3].focus(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &super::Submit, _, _| {
                        this.keys.push("submit".into());
                    }))
                    .child(
                        div()
                            .id("native-input")
                            .test_support()
                            .key_context(self.context)
                            .track_focus(&self.input)
                            .on_action(cx.listener(|this, _: &super::AcceptCompletion, _, _| {
                                this.keys.push("accept".into());
                            }))
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                this.keys
                                    .push(event.keystroke.unparse(cx.platform_info().os));
                                cx.stop_propagation();
                            })),
                    )
                    .children(self.neighbours.iter().enumerate().map(|(index, focus)| {
                        div()
                            .id(("neighbour", index))
                            .test_support()
                            .track_focus(focus)
                    }))
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
            super::bind_keys(cx);
        });
        let shortcuts = [
            ("secondary-alt-h", "left", 0),
            ("secondary-alt-j", "down", 1),
            ("secondary-alt-k", "up", 2),
            ("secondary-alt-l", "right", 3),
            ("secondary-alt-left", "left", 0),
            ("secondary-alt-down", "down", 1),
            ("secondary-alt-up", "up", 2),
            ("secondary-alt-right", "right", 3),
        ];
        for (context, tab, navigation_allowed) in [
            ("Terminal NativeInput NativeSurface", "tab", true),
            ("NativeInput NativeSurface PromptEditor", "tab", true),
            (
                "NativeInput NativeSurface PromptEditor CaretPopup",
                "accept",
                true,
            ),
            ("NativeInput NativeSurface Command", "tab", false),
            ("NativeInput NativeSurface DrostjePalette", "tab", false),
            ("NativeInput NativeSurface SpacePicker", "tab", false),
            ("NativeInput NativeSurface HarnessPicker", "tab", false),
            ("NativeInput NativeSurface HostPicker", "tab", false),
            ("NativeInput NativeSurface PopupMenu", "tab", false),
            ("NativeInput NativeSurface Sheet", "tab", false),
            ("NativeInput NativeSurface Dialog", "tab", false),
        ] {
            let mut view = None;
            let window = cx.add_window(|window, cx| {
                let inputs = cx.new(|cx| NativeInputs {
                    context,
                    input: cx.focus_handle().tab_stop(true),
                    neighbours: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
                    navigation: Vec::new(),
                    keys: Vec::new(),
                });
                view = Some(inputs.clone());
                Root::new(inputs, window, cx)
            });
            let view = view.unwrap();
            if !navigation_allowed {
                cx.update_window(window.into(), |_, window, cx| {
                    window.render_frame(cx);
                    let input = view.read(cx).input.clone();
                    input.focus(window, cx);
                    for (key, _, _) in shortcuts {
                        window.press(key, cx);
                        assert!(
                            !window.has_pending_keystrokes(),
                            "{context}: {key} started a chord"
                        );
                        assert!(
                            input.is_focused(window),
                            "{context}: {key} moved overlay focus"
                        );
                    }
                    assert!(view.read(cx).navigation.is_empty(), "{context}");
                    assert_eq!(
                        view.read(cx).keys,
                        shortcuts.map(|(key, _, _)| {
                            let os = cx.platform_info().os;
                            gpui_kit::Keystroke::parse_with_os(key, os)
                                .unwrap()
                                .unparse(os)
                        }),
                        "{context}: navigation keystrokes must remain overlay-owned"
                    );
                })
                .unwrap();
                continue;
            }
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                let input = view.read(cx).input.clone();
                input.focus(window, cx);
                for key in [
                    "tab",
                    "shift-tab",
                    "ctrl-w",
                    "h",
                    "ctrl-w",
                    "ctrl-h",
                    ":",
                    "shift-g",
                    "secondary-enter",
                    "secondary-shift-p",
                ] {
                    window.press(key, cx);
                    assert!(
                        !window.has_pending_keystrokes(),
                        "{key} started an app chord"
                    );
                    assert!(
                        input.is_focused(window),
                        "{key} moved focus out of {context}"
                    );
                }
                for (key, direction, neighbour) in shortcuts {
                    input.focus(window, cx);
                    let destination = view.read(cx).neighbours[neighbour].clone();
                    let before = view.read(cx).navigation.len();
                    let native_keys = view.read(cx).keys.clone();
                    window.press(key, cx);
                    assert!(
                        !window.has_pending_keystrokes(),
                        "{key} started an app chord"
                    );
                    assert!(
                        destination.is_focused(window),
                        "{context}: {key} did not focus {direction}"
                    );
                    assert!(
                        !input.is_focused(window),
                        "{context}: {key} retained native focus"
                    );
                    assert_eq!(
                        &view.read(cx).navigation[before..],
                        &[direction],
                        "{context}: {key}"
                    );
                    assert_eq!(
                        view.read(cx).keys,
                        native_keys,
                        "{context}: {key} reached native input"
                    );
                }
                input.focus(window, cx);
                window.press("secondary-escape", cx);
                assert!(!window.has_pending_keystrokes());
                assert!(view.read(cx).neighbours[0].is_focused(window));
                assert!(
                    !input.is_focused(window),
                    "explicit escape retained native focus"
                );
            })
            .unwrap();
            cx.read(|cx| {
                let os = cx.platform_info().os;
                let submit_key = gpui_kit::Keystroke::parse_with_os("secondary-enter", os)
                    .unwrap()
                    .unparse(os);
                let submit = if context.starts_with("Terminal") {
                    submit_key.as_str()
                } else {
                    "submit"
                };
                assert_eq!(
                    view.read(cx).keys,
                    [
                        tab,
                        "shift-tab",
                        "ctrl-w",
                        "h",
                        "ctrl-w",
                        "ctrl-h",
                        ":",
                        "shift-g",
                        submit,
                        "palette",
                        "leave"
                    ],
                    "{context}"
                );
            });
        }
    }

    #[test]
    fn palette_offers_a_new_conversation_command_for_every_harness() {
        for harness in crate::harness::CONVERSATION_HARNESSES {
            let label = format!("New {} conversation", crate::harness::display_name(harness));
            assert!(
                super::Command::ALL
                    .iter()
                    .any(|command| command.label() == label),
                "the palette has no {label:?}"
            );
        }
    }

    #[test]
    fn harness_picker_motion_steps_over_the_current_harness() {
        assert_eq!(picker_row(3, Some(0), 1, false, Some(1)), 2);
        assert_eq!(picker_row(3, Some(2), 1, false, Some(1)), 0);
        assert_eq!(picker_row(3, Some(2), -1, false, Some(1)), 0);
        assert_eq!(picker_row(3, None, 1, false, Some(0)), 1);
        assert_eq!(picker_row(3, None, -1, false, Some(2)), 1);
        assert_eq!(picker_row(3, Some(1), -1, true, Some(0)), 1);
        assert_eq!(picker_row(3, Some(0), 1, true, Some(2)), 1);
        assert_eq!(picker_row(3, Some(2), 1, false, None), 0);
    }
}
