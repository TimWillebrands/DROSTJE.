use super::workspace::{Region, Workspace};
use crate::app::AppState;
use gpui_kit::component::{Disableable, Selectable};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use uuid::Uuid;

gpui_kit::actions!(
    drost,
    [
        AddProject,
        NewConversation,
        RenameProject,
        ArchiveProject,
        RenameConversation,
        ArchiveConversation,
        ShowArchived,
        SendToFactory,
        OpenFactoryFolder,
        DispatchAzureWorkItem,
        RefreshFactoryRuns,
        Settings,
        ResetLayout,
        OpenPalette,
        FocusProjects,
        FocusConversation,
        FocusFactory,
        FocusOutput,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
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
        ToggleFactory,
        ToggleOutput,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    AddProject,
    NewConversation,
    RenameProject,
    ArchiveProject,
    RenameConversation,
    ArchiveConversation,
    ShowArchived,
    SendToFactory,
    OpenFactoryFolder,
    DispatchAzureWorkItem,
    RefreshFactoryRuns,
    Settings,
    ResetLayout,
}
impl Command {
    pub const ALL: [Self; 13] = [
        Self::AddProject,
        Self::NewConversation,
        Self::RenameProject,
        Self::ArchiveProject,
        Self::RenameConversation,
        Self::ArchiveConversation,
        Self::ShowArchived,
        Self::SendToFactory,
        Self::OpenFactoryFolder,
        Self::DispatchAzureWorkItem,
        Self::RefreshFactoryRuns,
        Self::Settings,
        Self::ResetLayout,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::AddProject => "Add Project",
            Self::NewConversation => "New Conversation",
            Self::RenameProject => "Rename Project",
            Self::ArchiveProject => "Archive Project",
            Self::RenameConversation => "Rename Conversation",
            Self::ArchiveConversation => "Archive Conversation",
            Self::ShowArchived => "Show Archived",
            Self::SendToFactory => "Send to Factory",
            Self::OpenFactoryFolder => "Open Factory Folder",
            Self::DispatchAzureWorkItem => "Dispatch Azure Work Item",
            Self::RefreshFactoryRuns => "Refresh Factory Runs",
            Self::Settings => "Settings",
            Self::ResetLayout => "Reset Layout",
        }
    }
    pub fn disabled_reason(self, state: &AppState) -> Option<String> {
        match self {
            Self::AddProject if state.registering => {
                Some("A working tree is being inspected.".into())
            }
            Self::AddProject if state.storage_error.is_some() => {
                Some("Storage is not writable.".into())
            }
            Self::NewConversation | Self::RenameProject => state.mutable_project_reason(),
            Self::ArchiveProject => state.archive_project_reason(),
            Self::RenameConversation => {
                state
                    .mutable_project_reason()
                    .or_else(|| match state.thread() {
                        None => Some("Select a conversation first.".into()),
                        Some(t) if t.archived => {
                            Some("Archived conversations are read-only.".into())
                        }
                        _ => None,
                    })
            }
            Self::ArchiveConversation => state.archive_thread_reason(),
            Self::SendToFactory | Self::OpenFactoryFolder | Self::DispatchAzureWorkItem => {
                state.mutable_project_reason().or_else(|| {
                    state
                        .selected_project
                        .and_then(|id| match state.repositories.get(&id) {
                            Some(Ok(_)) => None,
                            Some(Err(error)) => {
                                Some(format!("Project root is unavailable: {error}"))
                            }
                            None => Some("The project working tree has not been inspected.".into()),
                        })
                })
            }
            _ => None,
        }
    }
}

pub fn bind_keys(cx: &mut App) {
    let navigation =
        Some("Workspace && !Input && !NativeInput && !CommandPalette && !Dialog && !NvimEditor");
    let list = Some("List && !Input && !NativeInput && !CommandPalette && !Dialog && !NvimEditor");
    let conversation =
        Some("Conversation && !Input && !NativeInput && !CommandPalette && !Dialog && !NvimEditor");
    cx.bind_keys(vec![
        KeyBinding::new("cmd-shift-p", OpenPalette, Some("Workspace")),
        KeyBinding::new("cmd-1", FocusProjects, Some("Workspace")),
        KeyBinding::new("cmd-2", FocusConversation, Some("Workspace")),
        KeyBinding::new("cmd-3", FocusFactory, Some("Workspace")),
        KeyBinding::new("cmd-4", FocusOutput, Some("Workspace")),
        KeyBinding::new(
            "cmd-n",
            NewConversation,
            Some("Workspace && !Dialog && !CommandPalette"),
        ),
        KeyBinding::new("cmd-,", Settings, Some("Workspace")),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("ctrl-h", FocusLeft, navigation),
        KeyBinding::new("ctrl-j", FocusDown, navigation),
        KeyBinding::new("ctrl-k", FocusUp, navigation),
        KeyBinding::new("ctrl-l", FocusRight, navigation),
        KeyBinding::new("j", SelectNext, list),
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("k", SelectPrevious, list),
        KeyBinding::new("up", SelectPrevious, list),
        KeyBinding::new("g g", SelectFirst, list),
        KeyBinding::new("shift-g", SelectLast, list),
        KeyBinding::new(
            "h",
            CollapseProject,
            Some("ProjectsList && !Input && !NativeInput"),
        ),
        KeyBinding::new(
            "left",
            CollapseProject,
            Some("ProjectsList && !Input && !NativeInput"),
        ),
        KeyBinding::new(
            "l",
            ExpandProject,
            Some("ProjectsList && !Input && !NativeInput"),
        ),
        KeyBinding::new(
            "right",
            ExpandProject,
            Some("ProjectsList && !Input && !NativeInput"),
        ),
        KeyBinding::new("enter", OpenSelection, list),
        KeyBinding::new("j", SelectNext, conversation),
        KeyBinding::new("down", SelectNext, conversation),
        KeyBinding::new("k", SelectPrevious, conversation),
        KeyBinding::new("up", SelectPrevious, conversation),
        KeyBinding::new("g g", SelectFirst, conversation),
        KeyBinding::new("shift-g", SelectLast, conversation),
        KeyBinding::new("i", FocusComposer, conversation),
        KeyBinding::new("cmd-escape", LeaveEditor, Some("NvimEditor")),
        KeyBinding::new("cmd-enter", Submit, Some("Conversation || NvimEditor")),
        KeyBinding::new("cmd-.", Interrupt, Some("Conversation || NvimEditor")),
        KeyBinding::new(
            "escape",
            Dismiss,
            Some("Dialog || CommandPalette || (Dialog > Input) || (CommandPalette > Input)"),
        ),
        KeyBinding::new("cmd-enter", Confirm, Some("Dialog || (Dialog > Input)")),
        KeyBinding::new(
            "down",
            SelectNext,
            Some("CommandPalette || (CommandPalette > Input)"),
        ),
        KeyBinding::new(
            "up",
            SelectPrevious,
            Some("CommandPalette || (CommandPalette > Input)"),
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

#[derive(Clone)]
pub enum PaletteTarget {
    Command(Command),
    Project(Uuid),
    Thread(Uuid),
}
#[derive(Clone)]
pub struct PaletteItem {
    pub title: String,
    pub detail: String,
    pub target: PaletteTarget,
    pub disabled: Option<String>,
}
pub struct Palette {
    pub input: Entity<InputState>,
    pub selected: usize,
    pub previous_focus: Option<FocusHandle>,
    pub subscription: Subscription,
}
impl Palette {
    pub fn items(&self, state: &AppState, cx: &App) -> Vec<PaletteItem> {
        let query = self.input.read(cx).value().to_lowercase();
        let mut items: Vec<_> = Command::ALL
            .into_iter()
            .map(|command| PaletteItem {
                title: command.label().into(),
                detail: "Command".into(),
                target: PaletteTarget::Command(command),
                disabled: command.disabled_reason(state),
            })
            .collect();
        items.extend(
            state
                .snapshot
                .projects
                .iter()
                .filter(|p| state.show_archived || !p.archived)
                .map(|p| PaletteItem {
                    title: p.name.clone(),
                    detail: format!(
                        "Project · {}{}",
                        p.root.display(),
                        if p.archived { " · Archived" } else { "" }
                    ),
                    target: PaletteTarget::Project(p.id),
                    disabled: None,
                }),
        );
        items.extend(
            state
                .snapshot
                .threads
                .iter()
                .filter(|t| {
                    state.show_archived
                        || (!t.archived
                            && state
                                .snapshot
                                .projects
                                .iter()
                                .any(|p| p.id == t.project_id && !p.archived))
                })
                .map(|t| PaletteItem {
                    title: t.title.clone(),
                    detail: format!(
                        "Conversation{}",
                        if t.archived { " · Archived" } else { "" }
                    ),
                    target: PaletteTarget::Thread(t.id),
                    disabled: None,
                }),
        );
        items.retain(|item| {
            query.split_whitespace().all(|term| {
                item.title.to_lowercase().contains(term)
                    || item.detail.to_lowercase().contains(term)
            })
        });
        items
    }
    pub fn render(&self, state: &AppState, owner: &WeakEntity<Workspace>, cx: &App) -> AnyElement {
        let items = self.items(state, cx);
        let mut list = v_flex()
            .id("palette-results")
            .max_h(px(420.))
            .overflow_y_scroll()
            .gap_1();
        if items.is_empty() {
            list = list.child(
                div()
                    .p_3()
                    .text_color(rgb(0x8B95A5))
                    .child("No matching commands, projects or conversations."),
            );
        }
        for (index, item) in items.into_iter().enumerate() {
            let owner = owner.clone();
            let target = item.target.clone();
            list = list.child(
                Button::new(("palette-item", index))
                    .w_full()
                    .ghost()
                    .disabled(item.disabled.is_some())
                    .when_some(item.disabled, |button, reason| button.tooltip(reason))
                    .label(item.title)
                    .selected(index == self.selected)
                    .on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.run_palette_target(target.clone(), window, cx)
                        });
                    }),
            );
        }
        v_flex()
            .key_context("CommandPalette")
            .w(px(650.))
            .p_3()
            .gap_3()
            .bg(rgb(0x20242B))
            .border_1()
            .border_color(rgb(0x343A46))
            .rounded_lg()
            .child(Input::new(&self.input))
            .child(list)
            .child(
                h_flex()
                    .text_xs()
                    .text_color(rgb(0x8B95A5))
                    .child("↑ ↓ select · Return open · Escape dismiss"),
            )
            .into_any_element()
    }
}

pub fn region_action(region: Region) -> &'static str {
    match region {
        Region::Projects => "Projects",
        Region::Conversation => "Conversation",
        Region::Factory => "Factory",
        Region::Output => "Output",
    }
}
