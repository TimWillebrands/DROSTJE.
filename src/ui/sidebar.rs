use super::{
    commands::{
        AddProject, ArchiveThread, CollapseProject, Command, Dismiss, ExpandProject, RenameThread,
        SearchThreads, SelectNext, ShowArchived, ToggleProjects,
    },
    workspace::Workspace,
};
use crate::{
    app::AppState,
    factory::{Liveness, ReportedState},
    harness::{ConversationState, TurnOutcome},
    model::{Project, ProjectId, Thread, ThreadId, ThreadKind},
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{
        ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, TITLE_BAR_HEIGHT,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputState},
        list::ListItem,
        menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
        shimmer::ShimmerText,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::collections::HashSet;

/// External geometry: the macOS traffic lights sit over the leading edge of the left dock,
/// the same inset `TitleBar` reserves for them.
const TRAFFIC_LIGHTS_WIDTH: Pixels = px(80.);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Project(ProjectId),
    Thread(ThreadId),
}
/// Visible sidebar rows in display order. A non-empty `query` keeps the threads
/// whose title (or whose project's name) contains it, ignoring collapse so no
/// match is hidden, and drops projects without a match.
pub fn rows(state: &AppState, query: &str) -> Vec<Row> {
    visible_rows(
        state
            .snapshot
            .projects
            .iter()
            .filter(|project| state.project_in_active_space(project.id)),
        &state.snapshot.threads,
        &state.collapsed,
        state.show_archived,
        query,
    )
}

fn visible_rows<'a>(
    projects: impl IntoIterator<Item = &'a Project>,
    threads: &[Thread],
    collapsed: &HashSet<ProjectId>,
    show_archived: bool,
    query: &str,
) -> Vec<Row> {
    let query = query.trim().to_lowercase();
    let mut rows = Vec::new();
    for project in projects
        .into_iter()
        .filter(|p| show_archived || !p.archived)
    {
        let threads = threads
            .iter()
            .filter(|t| t.project_id == project.id && (show_archived || !t.archived));
        if query.is_empty() {
            rows.push(Row::Project(project.id));
            if !collapsed.contains(&project.id) {
                rows.extend(threads.map(|t| Row::Thread(t.id)));
            }
            continue;
        }
        let project_matches = project.name.to_lowercase().contains(&query);
        let matches: Vec<Row> = threads
            .filter(|t| project_matches || t.title.to_lowercase().contains(&query))
            .map(|t| Row::Thread(t.id))
            .collect();
        if project_matches || !matches.is_empty() {
            rows.push(Row::Project(project.id));
            rows.extend(matches);
        }
    }
    rows
}

/// Seconds since the Unix epoch for a stored `YYYY-MM-DDTHH:MM:SSZ` timestamp.
fn unix_seconds(timestamp: &str) -> Option<i64> {
    let field = |range: std::ops::Range<usize>| timestamp.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from civil date (Howard Hinnant), proleptic Gregorian calendar.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Compact age of a stored timestamp relative to `now` (Unix seconds): `now`,
/// `5m`, `3h`, `2d`, `3w`, `2mo`, `1y`. `None` when the timestamp is unreadable.
pub fn relative_age(timestamp: &str, now: i64) -> Option<String> {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let elapsed = (now - unix_seconds(timestamp)?).max(0);
    Some(match elapsed {
        s if s < MINUTE => "now".to_owned(),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < 7 * DAY => format!("{}d", s / DAY),
        s if s < 30 * DAY => format!("{}w", s / (7 * DAY)),
        s if s < 365 * DAY => format!("{}mo", s / (30 * DAY)),
        s => format!("{}y", s / (365 * DAY)),
    })
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
    let label = label.into();
    let reason = command.disabled_reason(state);
    PopupMenuItem::new(match &reason {
        Some(reason) => SharedString::from(format!("{label} — {reason}")),
        None => label,
    })
    .action(command.action())
    .disabled(reason.is_some())
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
                "New OMP conversation",
                Command::NewOmpConversation,
                state,
            ))
            .item(command_item(
                "New Pi conversation",
                Command::NewPiConversation,
                state,
            ))
            .item(command_item(
                "New Claude Code conversation",
                Command::NewClaudeCodeConversation,
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
                Command::MoveProjectToSpace.label(),
                Command::MoveProjectToSpace,
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
    hovered_row: Option<Row>,
    search: &Entity<InputState>,
    scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let query = search.read(cx).value();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let mut list = v_flex()
        .id("project-rows")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(scroll);
    let rows = rows(state, &query);
    let mut first_project = true;
    for row in rows.iter().copied() {
        let action_owner = owner.clone();
        let menu_owner = owner.clone();
        let keyboard_cursor = cursor == Some(row);
        let pointer_hovered = hovered_row == Some(row);
        match row {
            Row::Project(id) => {
                let project = state
                    .snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == id)
                    .expect("project row");
                let collapsed = state.collapsed.contains(&id) && query.trim().is_empty();
                let selected =
                    state.selected_project == Some(id) && state.selected_thread.is_none();
                // Row actions stay quiet until the header is hovered, selected or under
                // the keyboard cursor; each has a key or context-menu equivalent.
                let revealed = keyboard_cursor || selected;
                let group: SharedString = format!("project-header-{id}").into();
                let toggle_owner = owner.clone();
                let new_owner = owner.clone();
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
                let reveal = |button: Button| {
                    button.when(!revealed, |button| {
                        button
                            .invisible()
                            .group_hover(group.clone(), |style| style.visible())
                    })
                };
                list = list.child(
                    div()
                        .group(group.clone())
                        .when(!first_project, |view| {
                            view.mt_1().border_t_1().border_color(theme.border)
                        })
                        .pt_1()
                        .child(
                            ListItem::new(SharedString::from(format!("project-{id}")))
                                .px_2()
                                .py_0p5()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .cursor_default()
                                .selected(selected)
                                .border_2()
                                .border_color(if keyboard_cursor {
                                    theme.ring
                                } else {
                                    theme.transparent
                                })
                                .when(keyboard_cursor, |item| item.bg(theme.sidebar_accent))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tooltip.clone()).build(window, cx)
                                })
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(div().flex_1().min_w_0().truncate().child(title))
                                        .when(error.is_some(), |view| {
                                            view.child(
                                                Icon::new(IconName::TriangleAlert)
                                                    .small()
                                                    .text_color(theme.danger),
                                            )
                                        })
                                        .child(
                                            reveal(
                                                Button::new(SharedString::from(format!(
                                                    "new-conversation-{id}"
                                                )))
                                                .ghost()
                                                .xsmall()
                                                .cursor_default()
                                                .icon(IconName::Plus)
                                                .debug_selector(move || {
                                                    format!("new-conversation-{id}")
                                                })
                                                .disabled(
                                                    project.archived
                                                        || state.storage_error.is_some(),
                                                )
                                                .tooltip(if project.archived {
                                                    "Archived project is read-only"
                                                } else {
                                                    "New conversation…"
                                                }),
                                            )
                                            .dropdown_menu(move |menu, window, cx| {
                                                let _ = new_owner.update(cx, |workspace, cx| {
                                                    workspace.select_row(row, false, window, cx)
                                                });
                                                let Some(workspace) = new_owner.upgrade() else {
                                                    return menu;
                                                };
                                                let state = workspace.read(cx).state.read(cx);
                                                menu.item(command_item(
                                                    "OMP",
                                                    Command::NewOmpConversation,
                                                    state,
                                                ))
                                                .item(command_item(
                                                    "Pi",
                                                    Command::NewPiConversation,
                                                    state,
                                                ))
                                                .item(command_item(
                                                    "Claude Code",
                                                    Command::NewClaudeCodeConversation,
                                                    state,
                                                ))
                                            }),
                                        )
                                        .child({
                                            let button = Button::new(SharedString::from(format!(
                                                "collapse-{id}"
                                            )))
                                            .ghost()
                                            .xsmall()
                                            .cursor_default()
                                            .icon(if collapsed {
                                                IconName::ChevronRight
                                            } else {
                                                IconName::ChevronDown
                                            })
                                            .tooltip_with_action(
                                                if collapsed {
                                                    "Expand project"
                                                } else {
                                                    "Collapse project"
                                                },
                                                if collapsed {
                                                    &ExpandProject as &dyn Action
                                                } else {
                                                    &CollapseProject
                                                },
                                                Some("ProjectsList"),
                                            )
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
                                            });
                                            // A collapsed project keeps its chevron so the
                                            // hidden threads stay discoverable.
                                            if collapsed { button } else { reveal(button) }
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
                        ),
                );
                first_project = false;
            }
            Row::Thread(id) => {
                let thread = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == id)
                    .expect("thread row");
                let (kind, icon) = match thread.kind {
                    ThreadKind::Conversation if thread.harness == "claude-code" => {
                        ("Claude Code conversation", Icon::new(IconName::Asterisk))
                    }
                    ThreadKind::Conversation if thread.harness == "pi" => {
                        ("Pi conversation", Icon::new(IconName::Bot))
                    }
                    ThreadKind::Conversation => ("OMP conversation", Icon::new(IconName::Bot)),
                    ThreadKind::Factory => ("Factory thread", Icon::new(IconName::Cpu)),
                };
                let tooltip = format!(
                    "{kind}: {}{}",
                    thread.title,
                    if thread.archived { " · Archived" } else { "" }
                );
                let status = exceptional_state(state, thread);
                let branch = thread
                    .factory
                    .as_ref()
                    .map(|config| config.target_branch.trim())
                    .filter(|branch| !branch.is_empty());
                let separator = || div().flex_shrink_0().child("•");
                let mut details = h_flex()
                    .pl_6()
                    .gap_1()
                    .items_center()
                    .min_w_0()
                    .text_xs()
                    .text_color(theme.muted_foreground);
                let mut first_detail = true;
                if let Some((label, failed)) = status {
                    details = details.child(
                        div()
                            .flex_shrink_0()
                            .when(failed, |view| view.text_color(theme.danger))
                            .child(label),
                    );
                    first_detail = false;
                }
                if let Some(branch) = branch {
                    details = details
                        .when(!first_detail, |view| view.child(separator()))
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_1()
                                .items_center()
                                .child(Icon::new(Lucide::GitBranch).xsmall())
                                .child(div().min_w_0().truncate().child(branch.to_owned())),
                        );
                    first_detail = false;
                }
                if thread.archived {
                    details = details
                        .when(!first_detail, |view| view.child(separator()))
                        .child(div().flex_shrink_0().child("Archived"));
                    first_detail = false;
                }
                if let Some(age) = relative_age(&thread.updated_at, now) {
                    details = details
                        .when(!first_detail, |view| view.child(separator()))
                        .child(div().flex_shrink_0().child(age));
                }
                let hover_owner = owner.clone();
                let rename_owner = owner.clone();
                let rename_reason = state.rename_thread_reason_for(id);
                let archive_owner = owner.clone();
                let archive_reason = state.archive_thread_reason_for(id);
                list = list.child(
                    ListItem::new(SharedString::from(format!("thread-{id}")))
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(theme.sidebar_foreground)
                        .cursor_default()
                        .selected(state.selected_thread == Some(id))
                        .border_2()
                        .border_color(if keyboard_cursor { theme.ring } else { theme.transparent })
                        .when(keyboard_cursor, |item| item.bg(theme.sidebar_accent))
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .child(
                            v_flex()
                                .min_w_0()
                                .gap_0p5()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            h_flex().w_4().flex_shrink_0().justify_center().child(
                                                icon.small().text_color(theme.muted_foreground),
                                            ),
                                        )
                                        .child(div().flex_1().min_w_0().truncate().map(|label| {
                                            match state.is_titling(id) {
                                                true => label
                                                    .child(ShimmerText::new(thread.title.clone())),
                                                false => label.child(thread.title.clone()),
                                            }
                                        }))
                                        // Pointer shortcuts only: the context menu and palette
                                        // carry the same commands. The lane keeps its width so
                                        // long titles do not jump when the pointer enters.
                                        .when(!thread.archived, |view| {
                                            view.child(
                                                h_flex()
                                                    .w(rems(2.75))
                                                    .flex_shrink_0()
                                                    .justify_end()
                                                    .when(pointer_hovered, |lane| {
                                                        lane.child(
                                                            h_flex()
                                                                .id(SharedString::from(format!(
                                                                    "thread-actions-{id}"
                                                                )))
                                                                .test_support()
                                                                .gap_0p5()
                                                                .child(
                                                                    Button::new(
                                                                        SharedString::from(format!(
                                                                            "rename-thread-{id}"
                                                                        )),
                                                                    )
                                                                    .ghost()
                                                                    .xsmall()
                                                                    .cursor_default()
                                                                    .icon(Lucide::Pencil)
                                                                    .disabled(
                                                                        rename_reason.is_some(),
                                                                    )
                                                                    .map(|button| {
                                                                        match rename_reason {
                                                                            Some(reason) => button
                                                                                .tooltip(reason),
                                                                            None => button
                                                                                .tooltip_with_action(
                                                                                    Command::RenameThread.label(),
                                                                                    &RenameThread,
                                                                                    Some("Workspace"),
                                                                                ),
                                                                        }
                                                                    })
                                                                    .on_click(
                                                                        move |_, window, cx| {
                                                                            cx.stop_propagation();
                                                                            let _ =
                                                                                rename_owner.update(
                                                                                    cx,
                                                                                    |workspace, cx| {
                                                                                        workspace
                                                                                            .select_row(
                                                                                                row,
                                                                                                true,
                                                                                                window,
                                                                                                cx,
                                                                                            );
                                                                                        workspace
                                                                                            .execute(
                                                                                                Command::RenameThread,
                                                                                                window,
                                                                                                cx,
                                                                                            );
                                                                                    },
                                                                                );
                                                                        },
                                                                    ),
                                                                )
                                                                .child(
                                                                    Button::new(
                                                                        SharedString::from(format!(
                                                                            "archive-thread-{id}"
                                                                        )),
                                                                    )
                                                                    .ghost()
                                                                    .xsmall()
                                                                    .cursor_default()
                                                                    .icon(Lucide::Archive)
                                                                    .disabled(
                                                                        archive_reason.is_some(),
                                                                    )
                                                                    .map(|button| {
                                                                        match archive_reason {
                                                                            Some(reason) => button
                                                                                .tooltip(reason),
                                                                            None => button
                                                                                .tooltip_with_action(
                                                                                    Command::ArchiveThread.label(),
                                                                                    &ArchiveThread,
                                                                                    Some("Workspace"),
                                                                                ),
                                                                        }
                                                                    })
                                                                    .on_click(
                                                                        move |_, window, cx| {
                                                                            cx.stop_propagation();
                                                                            let _ =
                                                                                archive_owner.update(
                                                                                    cx,
                                                                                    |workspace, cx| {
                                                                                        workspace
                                                                                            .select_row(
                                                                                                row,
                                                                                                true,
                                                                                                window,
                                                                                                cx,
                                                                                            );
                                                                                        workspace
                                                                                            .execute(
                                                                                                Command::ArchiveThread,
                                                                                                window,
                                                                                                cx,
                                                                                            );
                                                                                    },
                                                                                );
                                                                        },
                                                                    ),
                                                                ),
                                                        )
                                                    }),
                                            )
                                        }),
                                )
                                .child(details),
                        )
                        .on_hover(move |hovered, _, cx| {
                            let _ = hover_owner.update(cx, |workspace, cx| {
                                workspace.set_sidebar_row_hovered(row, *hovered, cx)
                            });
                        })
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
    let empty = state
        .snapshot
        .projects
        .iter()
        .filter(|project| state.project_in_active_space(project.id))
        .all(|project| project.archived && !state.show_archived);
    if empty {
        list = list.child(
            v_flex()
                .p_3()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "“{}” has no visible projects. Add an existing Git working tree to begin.",
                    state.active_space_name()
                ))
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
    } else if rows.is_empty() {
        list = list.child(
            div()
                .p_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(format!("No threads match “{}”", query.trim())),
        );
    }
    let search_owner = owner.clone();
    let search_next_owner = owner.clone();
    let space_menu_owner = owner.clone();
    v_flex()
        .size_full()
        .bg(theme.sidebar)
        .text_color(theme.sidebar_foreground)
        .child(
            h_flex()
                .id("thread-search-row")
                .flex_shrink_0()
                // Level with the title bar beside it, bottom border included, so the
                // Projects column reads as one full-height pane.
                .h(TITLE_BAR_HEIGHT)
                .when(cfg!(target_os = "macos"), |row| {
                    row.pl(TRAFFIC_LIGHTS_WIDTH)
                })
                .pr_1()
                .items_center()
                .border_b_1()
                .border_color(theme.title_bar_border)
                .child(
                    div()
                        .id("thread-search")
                        .flex_1()
                        .min_w_0()
                        .key_context("NativeInput ProjectSearch")
                        // Escape clears the filter and hands focus back to the list; Down
                        // moves into the filtered rows.
                        .on_action(move |_: &Dismiss, window, cx| {
                            let _ = search_owner.update(cx, |workspace, cx| {
                                workspace.leave_thread_search(true, window, cx)
                            });
                        })
                        .on_action(move |_: &SelectNext, window, cx| {
                            let _ = search_next_owner.update(cx, |workspace, cx| {
                                workspace.leave_thread_search(false, window, cx)
                            });
                        })
                        .child(
                            Input::new(search).small().cleanable(true).prefix(
                                Icon::new(IconName::Search)
                                    .small()
                                    .text_color(theme.muted_foreground),
                            ),
                        ),
                )
                .child(
                    Button::new("projects-menu")
                        .ghost()
                        .small()
                        .cursor_default()
                        .icon(IconName::Ellipsis)
                        .tooltip("Projects menu")
                        .dropdown_menu(|menu, _, _| {
                            menu.menu(Command::AddProject.label(), Box::new(AddProject))
                                .menu(Command::ShowArchived.label(), Box::new(ShowArchived))
                        }),
                ),
        )
        .child(
            h_flex()
                .id("space-toolbar")
                .flex_shrink_0()
                .gap_1()
                .p_2()
                .items_center()
                .child(
                    super::commands::button("space-switcher", Command::SwitchSpace, owner, state)
                        .label(state.active_space_name().to_owned())
                        .icon(IconName::ChevronDown)
                        .flex_1()
                        .min_w_0()
                        .debug_selector(|| "space-switcher".to_owned()),
                )
                .child(
                    super::commands::button("new-space", Command::NewSpace, owner, state)
                        .label("")
                        .icon(IconName::Plus)
                        .tooltip(
                            Command::NewSpace
                                .disabled_reason(state)
                                .unwrap_or_else(|| Command::NewSpace.label().into()),
                        )
                        .debug_selector(|| "new-space".to_owned()),
                )
                .child(
                    Button::new("space-menu")
                        .ghost()
                        .small()
                        .icon(IconName::Ellipsis)
                        .tooltip("Manage space")
                        .dropdown_menu(move |menu, _, cx| {
                            let Some(workspace) = space_menu_owner.upgrade() else {
                                return menu;
                            };
                            let state = workspace.read(cx).state.read(cx);
                            menu.item(command_item(
                                Command::RenameSpace.label(),
                                Command::RenameSpace,
                                state,
                            ))
                            .item(command_item(
                                Command::MoveProjectToSpace.label(),
                                Command::MoveProjectToSpace,
                                state,
                            ))
                            .separator()
                            .item(command_item(
                                Command::DeleteSpace.label(),
                                Command::DeleteSpace,
                                state,
                            ))
                        }),
                ),
        )
        .when(state.registering, |view| {
            view.child(
                div()
                    .px_3()
                    .py_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Inspecting working tree…"),
            )
        })
        .child(list)
        .child(
            h_flex()
                .id("sidebar-toolbar")
                .flex_shrink_0()
                .gap_1()
                .p_1()
                .items_center()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    Button::new("toggle-projects")
                        .ghost()
                        .small()
                        .cursor_default()
                        .icon(IconName::PanelLeftClose)
                        .tooltip_with_action(
                            Command::ToggleProjects.label(),
                            &ToggleProjects,
                            Some("Workspace"),
                        )
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ToggleProjects), cx)
                        }),
                )
                .child(
                    Button::new("show-archived")
                        .ghost()
                        .small()
                        .cursor_default()
                        .icon(Lucide::Archive)
                        .selected(state.show_archived)
                        .toggled(state.show_archived)
                        .tooltip(Command::ShowArchived.label())
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ShowArchived), cx)
                        }),
                )
                .child(
                    Button::new("search-threads")
                        .ghost()
                        .small()
                        .cursor_default()
                        .icon(IconName::Search)
                        .tooltip_with_action(
                            Command::SearchThreads.label(),
                            &SearchThreads,
                            Some("ProjectsList"),
                        )
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(SearchThreads), cx)
                        }),
                )
                .child(div().flex_1())
                .child(
                    Button::new("add-project")
                        .ghost()
                        .small()
                        .cursor_default()
                        .icon(Lucide::FolderPlus)
                        .disabled(Command::AddProject.disabled_reason(state).is_some())
                        .tooltip(
                            Command::AddProject
                                .disabled_reason(state)
                                .unwrap_or_else(|| Command::AddProject.label().to_owned()),
                        )
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(AddProject), cx)),
                )
                .child(
                    Button::new("new-thread")
                        .ghost()
                        .small()
                        .cursor_default()
                        .icon(IconName::Plus)
                        .tooltip("New thread…")
                        .dropdown_menu(move |menu, _, cx| {
                            let Some(workspace) = new_owner.upgrade() else {
                                return menu;
                            };
                            let state = workspace.read(cx).state.read(cx);
                            menu.item(
                                command_item(
                                    "OMP conversation",
                                    Command::NewOmpConversation,
                                    state,
                                )
                                .icon(IconName::Bot),
                            )
                            .item(
                                command_item("Pi conversation", Command::NewPiConversation, state)
                                    .icon(IconName::Bot),
                            )
                            .item(
                                command_item(
                                    "Claude Code conversation",
                                    Command::NewClaudeCodeConversation,
                                    state,
                                )
                                .icon(IconName::Asterisk),
                            )
                            .item(
                                command_item("Factory thread", Command::NewFactoryThread, state)
                                    .icon(IconName::Cpu),
                            )
                        }),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Row, relative_age, visible_rows};
    use crate::model::{Project, ProjectDefaults, Thread, ThreadKind};
    use std::collections::HashSet;
    use uuid::Uuid;

    fn project(name: &str) -> Project {
        Project {
            id: Uuid::new_v4(),
            name: name.into(),
            root: "/nonexistent".into(),
            archived: false,
            defaults: ProjectDefaults::default(),
        }
    }

    fn thread(project: &Project, title: &str, archived: bool) -> Thread {
        Thread {
            id: Uuid::new_v4(),
            project_id: project.id,
            kind: ThreadKind::Conversation,
            factory: None,
            title: title.into(),
            harness: "omp".into(),
            session_ref: None,
            model: None,
            effort: None,
            permission_mode: None,
            draft: String::new(),
            attachments: Vec::new(),
            submission: None,
            queued: Vec::new(),
            archived,
            created_at: "2026-09-16T12:00:00Z".into(),
            updated_at: "2026-09-16T12:00:00Z".into(),
        }
    }

    #[test]
    fn search_filters_threads_and_ignores_collapse() {
        let (dungeon, lighting) = (project("masters-of-the-dungeon"), project("lighting"));
        let threads = [
            thread(&dungeon, "Make the spritesheet pluggable", false),
            thread(&dungeon, "Simplify staged changes", false),
            thread(&dungeon, "Archived sprite work", true),
            thread(&lighting, "Implement handoff", false),
        ];
        let projects = [dungeon.clone(), lighting.clone()];
        let collapsed = HashSet::from([dungeon.id]);

        assert_eq!(
            visible_rows(&projects, &threads, &collapsed, false, ""),
            [
                Row::Project(dungeon.id),
                Row::Project(lighting.id),
                Row::Thread(threads[3].id),
            ]
        );
        assert_eq!(
            visible_rows(&projects, &threads, &collapsed, false, "  SPRITE "),
            [Row::Project(dungeon.id), Row::Thread(threads[0].id)]
        );
        assert_eq!(
            visible_rows(&projects, &threads, &collapsed, true, "sprite"),
            [
                Row::Project(dungeon.id),
                Row::Thread(threads[0].id),
                Row::Thread(threads[2].id),
            ]
        );
        assert_eq!(
            visible_rows(&projects, &threads, &collapsed, false, "light"),
            [Row::Project(lighting.id), Row::Thread(threads[3].id)]
        );
        assert!(visible_rows(&projects, &threads, &collapsed, false, "nothing").is_empty());
    }

    #[test]
    fn relative_age_uses_compact_units() {
        let base = "2026-09-16T12:00:00Z";
        let now = super::unix_seconds(base).expect("valid timestamp");
        assert_eq!(now, 1_789_560_000);
        let age = |seconds| relative_age(base, now + seconds);
        assert_eq!(age(-30).as_deref(), Some("now"));
        assert_eq!(age(59).as_deref(), Some("now"));
        assert_eq!(age(5 * 60).as_deref(), Some("5m"));
        assert_eq!(age(3 * 3_600).as_deref(), Some("3h"));
        assert_eq!(age(2 * 86_400).as_deref(), Some("2d"));
        assert_eq!(age(21 * 86_400).as_deref(), Some("3w"));
        assert_eq!(age(65 * 86_400).as_deref(), Some("2mo"));
        assert_eq!(age(400 * 86_400).as_deref(), Some("1y"));
    }

    #[test]
    fn unreadable_timestamps_have_no_age() {
        assert_eq!(relative_age("", 0), None);
        assert_eq!(relative_age("yesterday", 0), None);
        assert_eq!(relative_age("2026-13-01T00:00:00Z", 0), None);
    }
}
