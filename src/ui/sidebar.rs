use super::{
    commands::{CollapseProject, Command, ExpandProject},
    workspace::Workspace,
};
use crate::{
    app::AppState,
    factory::{Liveness, ReportedState},
    harness::{ConversationState, TurnOutcome},
    model::{ProjectId, Thread, ThreadId, ThreadKind},
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Icon, IconName, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        list::ListItem,
        menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Project(ProjectId),
    Thread(ThreadId),
}
pub fn rows(state: &AppState) -> Vec<Row> {
    let mut rows = Vec::new();
    for project in state
        .snapshot
        .projects
        .iter()
        .filter(|p| state.show_archived || !p.archived)
    {
        rows.push(Row::Project(project.id));
        if !state.collapsed.contains(&project.id) {
            rows.extend(
                state
                    .snapshot
                    .threads
                    .iter()
                    .filter(|t| t.project_id == project.id && (state.show_archived || !t.archived))
                    .map(|t| Row::Thread(t.id)),
            );
        }
    }
    rows
}

fn exceptional_state(state: &AppState, thread: &Thread) -> Option<(&'static str, bool)> {
    match thread.kind {
        ThreadKind::Conversation => {
            if let Some(session) = state.conversations.get(&thread.id) {
                if !session.requests.is_empty()
                    || session.state == ConversationState::WaitingForInput
                {
                    return Some(("Waiting", false));
                }
                match session.state {
                    ConversationState::Starting => return Some(("Starting", false)),
                    ConversationState::Running => return Some(("Running", false)),
                    ConversationState::Interrupting => return Some(("Stopping", false)),
                    ConversationState::Failed => return Some(("Failed", true)),
                    ConversationState::Disconnected => return Some(("Disconnected", true)),
                    _ => {}
                }
                if session.error.is_some() || session.outcome == Some(TurnOutcome::Failed) {
                    return Some(("Failed", true));
                }
                if session.outcome == Some(TurnOutcome::Unknown) {
                    return Some(("Unresolved", true));
                }
            }
            thread.submission.as_ref().map(|_| ("Unresolved", true))
        }
        ThreadKind::Factory => {
            if state.pending_factory.contains(&thread.id) {
                return Some(("Starting", false));
            }
            let launch = state
                .snapshot
                .launches
                .iter()
                .find(|launch| launch.factory_thread_id == thread.id)?;
            if let Some(run) = state.factory_runs.get(&launch.id) {
                match run.reported_state {
                    ReportedState::Completed => return None,
                    ReportedState::Failed => return Some(("Failed", true)),
                    ReportedState::Paused => return Some(("Paused", false)),
                    _ => {}
                }
                if run.launch.launch_error.is_some() {
                    return Some(("Failed", true));
                }
                return Some(match run.liveness {
                    Liveness::OwnedRunning if run.reported_state == ReportedState::Pending => {
                        ("Waiting", false)
                    }
                    Liveness::OwnedRunning => ("Running", false),
                    Liveness::Exited(_) | Liveness::Unverified => ("Unresolved", true),
                });
            }
            Some(if launch.launch_error.is_some() {
                ("Failed", true)
            } else if state.owned_launches.contains(&launch.id) {
                ("Running", false)
            } else {
                ("Unresolved", true)
            })
        }
    }
}

fn command_item(
    label: impl Into<SharedString>,
    command: Command,
    state: &AppState,
) -> PopupMenuItem {
    PopupMenuItem::new(label)
        .action(command.action())
        .disabled(command.disabled_reason(state).is_some())
}

fn row_menu(
    menu: PopupMenu,
    row: Row,
    owner: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    // Context actions operate on the clicked row, not the keyboard cursor or
    // previously opened thread, using the same selection path as a normal click.
    let _ = owner.update(cx, |workspace, cx| {
        workspace.select_row(row, true, window, cx)
    });
    let Some(workspace) = owner.upgrade() else {
        return menu;
    };
    let state = workspace.read(cx).state.read(cx);
    match row {
        Row::Project(_) => menu
            .item(command_item(
                "New conversation",
                Command::NewConversation,
                state,
            ))
            .item(command_item(
                "New Factory thread",
                Command::NewFactoryThread,
                state,
            ))
            .separator()
            .item(command_item(
                "Rename project…",
                Command::RenameProject,
                state,
            ))
            .item(command_item(
                "Archive project…",
                Command::ArchiveProject,
                state,
            )),
        Row::Thread(_) => menu
            .item(command_item("Rename thread…", Command::RenameThread, state))
            .item(command_item(
                "Archive thread…",
                Command::ArchiveThread,
                state,
            ))
            .separator()
            .item(command_item("Close thread", Command::CloseThread, state)),
    }
}

pub fn render(
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    cursor: Option<Row>,
    scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let mut list = v_flex()
        .id("project-rows")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(scroll);
    for row in rows(state) {
        let action_owner = owner.clone();
        let menu_owner = owner.clone();
        let keyboard_cursor = cursor == Some(row);
        match row {
            Row::Project(id) => {
                let project = state
                    .snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == id)
                    .expect("project row");
                let collapsed = state.collapsed.contains(&id);
                let toggle_owner = owner.clone();
                let error = state
                    .repositories
                    .get(&id)
                    .and_then(|repo| repo.as_ref().err());
                let title = format!(
                    "{}{}",
                    project.name,
                    if project.archived { " · Archived" } else { "" }
                );
                let tooltip = error
                    .map(|error| format!("{} — Root unavailable: {error}", project.name))
                    .unwrap_or_else(|| title.clone());
                list = list.child(
                    ListItem::new(SharedString::from(format!("project-{id}")))
                        .px_1()
                        .text_sm()
                        .text_color(theme.sidebar_foreground)
                        .cursor_default()
                        .selected(
                            state.selected_project == Some(id) && state.selected_thread.is_none(),
                        )
                        .border_l_2()
                        .border_color(if keyboard_cursor {
                            theme.ring
                        } else {
                            theme.transparent
                        })
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Button::new(SharedString::from(format!("collapse-{id}")))
                                        .ghost()
                                        .small()
                                        .cursor_default()
                                        .icon(if collapsed {
                                            IconName::ChevronRight
                                        } else {
                                            IconName::ChevronDown
                                        })
                                        .tooltip(if collapsed {
                                            "Expand project"
                                        } else {
                                            "Collapse project"
                                        })
                                        .on_click(move |_, window, cx| {
                                            cx.stop_propagation();
                                            let _ = toggle_owner.update(cx, |workspace, cx| {
                                                workspace.select_row(row, false, window, cx)
                                            });
                                            let action: Box<dyn Action> = if collapsed {
                                                Box::new(ExpandProject)
                                            } else {
                                                Box::new(CollapseProject)
                                            };
                                            window.dispatch_action(action, cx);
                                        }),
                                )
                                .child(
                                    Icon::new(if collapsed {
                                        IconName::FolderClosed
                                    } else {
                                        IconName::FolderOpen
                                    })
                                    .small()
                                    .text_color(theme.muted_foreground),
                                )
                                .child(div().flex_1().min_w_0().truncate().child(title))
                                .when(error.is_some(), |view| {
                                    view.child(
                                        Icon::new(IconName::TriangleAlert)
                                            .small()
                                            .text_color(theme.danger),
                                    )
                                }),
                        )
                        .on_click(move |_, window, cx| {
                            let _ = action_owner.update(cx, |workspace, cx| {
                                workspace.select_row(row, false, window, cx)
                            });
                        })
                        .context_menu(move |menu, window, cx| {
                            row_menu(menu, row, &menu_owner, window, cx)
                        }),
                );
            }
            Row::Thread(id) => {
                let thread = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == id)
                    .expect("thread row");
                let (kind, icon) = match thread.kind {
                    ThreadKind::Conversation => ("Conversation", IconName::Bot),
                    ThreadKind::Factory => ("Factory", IconName::Cpu),
                };
                let tooltip = format!(
                    "{kind}: {}{}",
                    thread.title,
                    if thread.archived { " · Archived" } else { "" }
                );
                list = list.child(
                    ListItem::new(SharedString::from(format!("thread-{id}")))
                        .px_1()
                        .text_sm()
                        .text_color(theme.sidebar_foreground)
                        .cursor_default()
                        .selected(state.selected_thread == Some(id))
                        .border_l_2()
                        .border_color(if keyboard_cursor {
                            theme.ring
                        } else {
                            theme.transparent
                        })
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().w_6().flex_shrink_0())
                                .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(thread.title.clone()),
                                )
                                .when_some(
                                    exceptional_state(state, thread),
                                    |view, (label, failed)| {
                                        view.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_xs()
                                                .text_color(if failed {
                                                    theme.danger
                                                } else {
                                                    theme.muted_foreground
                                                })
                                                .child(label),
                                        )
                                    },
                                ),
                        )
                        .on_click(move |_, window, cx| {
                            let _ = action_owner.update(cx, |workspace, cx| {
                                workspace.select_row(row, true, window, cx)
                            });
                        })
                        .context_menu(move |menu, window, cx| {
                            row_menu(menu, row, &menu_owner, window, cx)
                        }),
                );
            }
        }
    }
    let new_owner = owner.clone();
    let projects_owner = owner.clone();
    let empty = state
        .snapshot
        .projects
        .iter()
        .all(|project| project.archived && !state.show_archived);
    if empty {
        list = list.child(
            v_flex()
                .p_3()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Add an existing Git working tree to begin.")
                .child(
                    Button::new("add-project-empty")
                        .small()
                        .ghost()
                        .cursor_default()
                        .label("Add project…")
                        .disabled(Command::AddProject.disabled_reason(state).is_some())
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Command::AddProject.action(), cx)
                        }),
                ),
        );
    }
    v_flex()
        .size_full()
        .bg(theme.sidebar)
        .text_color(theme.sidebar_foreground)
        .gap_1()
        .p_1()
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .justify_between()
                .child(
                    Button::new("new-thread")
                        .small()
                        .ghost()
                        .cursor_default()
                        .icon(IconName::Plus)
                        .label("New thread")
                        .dropdown_menu(move |menu, _, cx| {
                            let Some(workspace) = new_owner.upgrade() else {
                                return menu;
                            };
                            let state = workspace.read(cx).state.read(cx);
                            menu.item(
                                command_item("Conversation", Command::NewConversation, state)
                                    .icon(IconName::Bot),
                            )
                            .item(
                                command_item("Factory", Command::NewFactoryThread, state)
                                    .icon(IconName::Cpu),
                            )
                        }),
                )
                .child(
                    Button::new("projects-menu")
                        .small()
                        .ghost()
                        .cursor_default()
                        .icon(IconName::Ellipsis)
                        .tooltip("Projects menu")
                        .dropdown_menu(move |menu, _, cx| {
                            let Some(workspace) = projects_owner.upgrade() else {
                                return menu;
                            };
                            let state = workspace.read(cx).state.read(cx);
                            menu.item(command_item("Add project…", Command::AddProject, state))
                                .item(
                                    command_item(
                                        "Show archived history",
                                        Command::ShowArchived,
                                        state,
                                    )
                                    .checked(state.show_archived),
                                )
                        }),
                ),
        )
        .when(state.registering, |view| {
            view.child(
                div()
                    .px_3()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Inspecting working tree…"),
            )
        })
        .child(list)
        .into_any_element()
}
