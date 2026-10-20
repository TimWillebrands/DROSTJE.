use super::{
    activity,
    commands::{
        AddProject, ArchiveThread, CollapseProject, Command, Dismiss, ExpandProject, NextSpace,
        PreviousSpace, RenameThread, SearchThreads, SelectNext, ShowArchived, ToggleProjects,
    },
    workspace::Workspace,
};
use crate::{
    app::AppState,
    factory::{Liveness, ReportedState},
    harness::{ConversationState, TurnOutcome},
    model::{Project, ProjectId, PullRequestLink, SpaceId, Thread, ThreadId, ThreadKind},
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
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

/// External geometry: the macOS traffic lights sit over the leading edge of the left dock,
/// the same inset `TitleBar` reserves for them.
const TRAFFIC_LIGHTS_WIDTH: Pixels = px(80.);

/// Minimum net travel on release; crossing it while touching never commits.
const SPACE_SWIPE_DISTANCE: f32 = 80.;
/// Platforms without touch phases use an idle gap as release.
pub(super) const SPACE_SWIPE_IDLE: Duration = Duration::from_millis(150);

/// One direct-manipulation gesture, followed by ignored momentum.
#[derive(Debug, Default)]
pub struct SpaceSwipe {
    travelled: Point<f32>,
    stepped: bool,
    touching: bool,
    last_event: Option<Instant>,
}

impl SpaceSwipe {
    /// Positive x follows fingers right, towards the previous Space.
    pub fn feed(&mut self, delta: Point<f32>, phase: TouchPhase, now: Instant) -> Option<Command> {
        let idle = self
            .last_event
            .is_none_or(|last| now.duration_since(last) > SPACE_SWIPE_IDLE);
        if phase == TouchPhase::Started || (idle && !self.touching) {
            *self = Self::default();
            self.touching = phase == TouchPhase::Started;
        }
        self.last_event = Some(now);
        if self.stepped {
            return None;
        }
        if phase == TouchPhase::Cancelled {
            self.finish();
            return None;
        }
        self.travelled.x += delta.x;
        self.travelled.y += delta.y;
        if phase == TouchPhase::Ended {
            return self.release();
        }
        None
    }

    /// Pixels, not an animation fraction: the list stays attached to the fingers.
    pub(super) fn progress(&self) -> Option<f32> {
        if self.stepped {
            return None;
        }
        Some(if self.travelled.x.abs() >= 2. * self.travelled.y.abs() {
            self.travelled.x
        } else {
            0.
        })
    }

    pub(super) fn needs_idle_release(&self) -> bool {
        !self.touching && !self.stepped
    }

    pub(super) fn release(&mut self) -> Option<Command> {
        let distance = self.progress().unwrap_or(0.);
        self.finish();
        (distance.abs() >= SPACE_SWIPE_DISTANCE).then_some(if distance > 0. {
            Command::PreviousSpace
        } else {
            Command::NextSpace
        })
    }

    pub(super) fn finish(&mut self) {
        self.stepped = true;
        self.touching = false;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Project(ProjectId),
    Thread(ThreadId),
}
/// Visible sidebar rows in display order. Each project's threads are listed most
/// recently touched first (`updated_at`); unreadable timestamps sort last and ties
/// keep stored (creation) order. A non-empty `query` keeps the threads whose title
/// (or whose project's name) contains it, ignoring collapse so no match is hidden,
/// and drops projects without a match.
pub fn rows(state: &AppState, query: &str) -> Vec<Row> {
    visible_rows(
        state
            .snapshot
            .projects
            .iter()
            .filter(|project| state.project_in_active_space(project.id)),
        state
            .snapshot
            .threads
            .iter()
            .filter(|thread| state.thread_listed_in(thread, state.active_space)),
        &state.collapsed,
        state.show_archived,
        query,
    )
}

fn visible_rows<'a>(
    projects: impl IntoIterator<Item = &'a Project>,
    threads: impl IntoIterator<Item = &'a Thread> + Clone,
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
        let mut threads: Vec<&Thread> = threads
            .clone()
            .into_iter()
            .filter(|t| t.project_id == project.id && (show_archived || !t.archived))
            .collect();
        threads.sort_by_key(|t| std::cmp::Reverse(unix_seconds(&t.updated_at)));
        let threads = threads.into_iter();
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

/// The tracker references of a thread row: its pull request as `!id` and work items as
/// `#id`, at most two of them with the rest counted. A Factory thread names the work item it
/// was launched from.
fn tracker_reference(
    pull_request: Option<&PullRequestLink>,
    factory_work_item: Option<i32>,
) -> Option<String> {
    let mut parts = Vec::new();
    let mut work_items: Vec<String> = factory_work_item
        .map(|id| format!("#{id}"))
        .into_iter()
        .collect();
    if let Some(pr) = pull_request {
        parts.push(format!("!{}", pr.id));
        work_items.extend(pr.work_items.iter().map(|id| format!("#{id}")));
    }
    let extra = work_items.len().saturating_sub(2);
    work_items.truncate(2);
    parts.extend(work_items);
    if extra > 0 {
        parts.push(format!("+{extra}"));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
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

/// A Projects menu item for one thread row: it shows `command`'s label and key, and runs
/// `run` on that thread instead of dispatching the selection-based action.
fn thread_item(
    command: Command,
    reason: Option<String>,
    owner: &WeakEntity<Workspace>,
    run: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> PopupMenuItem {
    let owner = owner.clone();
    PopupMenuItem::new(match &reason {
        Some(reason) => SharedString::from(format!("{} — {reason}", command.label())),
        None => command.label().into(),
    })
    .action(command.action())
    .disabled(reason.is_some())
    .on_click(move |_, window, cx| {
        let _ = owner.update(cx, |workspace, cx| run(workspace, window, cx));
    })
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
    // Context actions operate on the clicked row, not the keyboard cursor or previously
    // opened thread. A project row is selected like a click; a thread row only moves the
    // cursor, so renaming, archiving or closing it never opens its tab first.
    let _ = owner.update(cx, |workspace, cx| {
        workspace.select_row(row, matches!(row, Row::Project(_)), window, cx)
    });
    let Some(workspace) = owner.upgrade() else {
        return menu;
    };
    match row {
        Row::Project(_) => {
            let state = workspace.read(cx).state.read(cx);
            menu.item(command_item(
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
                Command::NewConversationInWorktree.label(),
                Command::NewConversationInWorktree,
                state,
            ))
            .item(command_item(
                "New Factory thread",
                Command::NewFactoryThread,
                state,
            ))
            .item(command_item(
                Command::NewTerminal.label(),
                Command::NewTerminal,
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
            ))
        }
        Row::Thread(thread) => {
            let (destinations, reason) = {
                let state = workspace.read(cx).state.read(cx);
                let destinations = state
                    .spaces
                    .iter()
                    .filter(|space| space.id != state.active_space)
                    .cloned()
                    .collect::<Vec<_>>();
                let reason = state.space_mutation_reason().or_else(|| {
                    destinations
                        .is_empty()
                        .then(|| "Create another space first.".into())
                });
                (destinations, reason)
            };
            let mut menu = {
                let state = workspace.read(cx).state.read(cx);
                let row_thread = state.snapshot.threads.iter().find(|c| c.id == thread);
                let in_worktree = row_thread.is_some_and(|c| c.worktree_id.is_some());
                let conversation = row_thread.is_some_and(|c| c.kind == ThreadKind::Conversation);
                let archived = row_thread.is_some_and(|c| c.archived);
                // Checkout commands work on the open Conversation, so they open this one.
                let opened = |command: Command| {
                    let reason = if state.selected_thread == Some(thread) {
                        command.disabled_reason(state)
                    } else {
                        None
                    };
                    thread_item(command, reason, owner, move |workspace, window, cx| {
                        workspace.select_row(row, true, window, cx);
                        workspace.execute(command, window, cx);
                    })
                };
                menu.item(thread_item(
                    Command::RenameThread,
                    state.rename_thread_reason_for(thread),
                    owner,
                    move |workspace, window, cx| workspace.rename_thread(thread, window, cx),
                ))
                .map(|menu| {
                    if archived {
                        menu.item(thread_item(
                            Command::UnarchiveThread,
                            state.unarchive_thread_reason_for(thread),
                            owner,
                            move |workspace, _, cx| workspace.unarchive_thread(thread, cx),
                        ))
                    } else {
                        menu.item(thread_item(
                            Command::ArchiveThread,
                            state.archive_thread_reason_for(thread),
                            owner,
                            move |workspace, window, cx| {
                                workspace.archive_thread(thread, window, cx)
                            },
                        ))
                    }
                })
                .when(conversation, |menu| {
                    menu.item(opened(Command::SwitchBranch))
                        .item(opened(Command::CommitChanges))
                })
                .when(in_worktree, |menu| {
                    menu.item(opened(Command::RemoveWorktree))
                })
            };
            if let Some(reason) = reason {
                menu =
                    menu.item(PopupMenuItem::new(format!("Move thread — {reason}")).disabled(true));
            } else {
                for destination in destinations {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("Move thread to {}", destination.name))
                            .on_click(move |_, window, cx| {
                                let _ = owner.update(cx, |workspace, cx| {
                                    if let Err(error) = workspace.move_thread_to_space(
                                        thread,
                                        destination.id,
                                        window,
                                        cx,
                                    ) {
                                        workspace.state.update(cx, |state, cx| {
                                            state.notice(
                                                crate::app::NoticeLevel::Warning,
                                                format!("{error:#}"),
                                            );
                                            cx.notify();
                                        });
                                    }
                                });
                            }),
                    );
                }
            }
            let state = workspace.read(cx).state.read(cx);
            menu.separator().item(thread_item(
                Command::CloseThread,
                (!state.open_threads.contains(&thread)).then(|| "This thread is not open.".into()),
                owner,
                move |workspace, window, cx| workspace.close_thread_tab(thread, window, cx),
            ))
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct SidebarPresentation<'a> {
    pub cursor: Option<Row>,
    pub hovered: Option<Row>,
    pub slide: f32,
    pub neighbor: Option<SpaceId>,
    pub neighbor_query: &'a str,
    pub neighbor_scroll: Option<&'a ScrollHandle>,
}

fn render_list(
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    space: SpaceId,
    presentation: SidebarPresentation<'_>,
    query: &str,
    scroll: Option<&ScrollHandle>,
    cx: &App,
) -> AnyElement {
    let SidebarPresentation {
        cursor,
        hovered: hovered_row,
        ..
    } = presentation;
    let theme = cx.theme();
    let active = space == state.active_space;
    let session = state.space_session(space);
    let collapsed_projects = if active {
        &state.collapsed
    } else {
        session.map_or(&state.collapsed, |session| &session.collapsed)
    };
    let show_archived = if active {
        state.show_archived
    } else {
        session.is_some_and(|session| session.show_archived)
    };
    let selected_project = if active {
        state.selected_project
    } else {
        session.and_then(|session| session.selected_project)
    };
    let selected_thread = if active {
        state.selected_thread
    } else {
        session.and_then(|session| session.selected_thread)
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let mut list = v_flex()
        .id(if active {
            "project-rows"
        } else {
            "project-rows-preview"
        })
        .test_support()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        // A sideways swipe switches Space; it must not also scroll the rows.
        .restrict_scroll_to_axis()
        .when_some(scroll, |list, scroll| list.track_scroll(scroll));
    let rows = visible_rows(
        state
            .snapshot
            .projects
            .iter()
            .filter(|project| state.project_in_space(project.id, space)),
        state
            .snapshot
            .threads
            .iter()
            .filter(|thread| state.thread_listed_in(thread, space)),
        collapsed_projects,
        show_archived,
        query,
    );
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
                let collapsed = collapsed_projects.contains(&id) && query.trim().is_empty();
                let selected = selected_project == Some(id) && selected_thread.is_none();
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
                                .border_l_2()
                                .border_color(if keyboard_cursor {
                                    theme.muted_foreground
                                } else {
                                    theme.transparent
                                })
                                .when(keyboard_cursor, |item| {
                                    item.bg(theme.list_active).text_color(theme.foreground)
                                })
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
                let worktree = state.thread_worktree(thread);
                let tooltip = format!(
                    "{kind}: {}{}{}{}",
                    thread.title,
                    if thread.archived { " · Archived" } else { "" },
                    thread
                        .pull_request
                        .as_ref()
                        .map(|pr| format!(" · Pull request !{}: {}", pr.id, pr.title))
                        .unwrap_or_default(),
                    match worktree {
                        Some(worktree) if worktree.is_live() => format!(
                            " · Worktree {} at {}",
                            worktree.codename,
                            worktree.path.display()
                        ),
                        Some(worktree) => format!(" · Worktree {} (removed)", worktree.codename),
                        None => String::new(),
                    }
                );
                let status = exceptional_state(state, thread);
                // Factory rows name their target branch; Conversations in a worktree name
                // the worktree's branch the same way.
                let branch = thread
                    .factory
                    .as_ref()
                    .map(|config| config.target_branch.trim())
                    .filter(|branch| !branch.is_empty())
                    .map(str::to_owned)
                    .or_else(|| worktree.map(|worktree| state.worktree_branch_label(worktree)));
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
                let work_item = state
                    .snapshot
                    .launches
                    .iter()
                    .find(|launch| launch.factory_thread_id == id)
                    .and_then(|launch| launch.work_item_id);
                if let Some(reference) = tracker_reference(thread.pull_request.as_ref(), work_item)
                {
                    details = details
                        .when(!first_detail, |view| view.child(separator()))
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(theme.mono_font_family.clone())
                                .child(reference),
                        );
                    first_detail = false;
                }
                if thread.archived {
                    details = details
                        .when(!first_detail, |view| view.child(separator()))
                        .child(div().flex_shrink_0().child("Archived"));
                    first_detail = false;
                }
                // An untouched thread is a placeholder that leaves Projects with its tab.
                let untouched = state.is_untouched(thread);
                if untouched {
                    details = details
                        .when(!first_detail, |view| view.child(separator()))
                        .child(div().flex_shrink_0().child("Not started"));
                } else if let Some(age) = relative_age(&thread.updated_at, now) {
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
                        .selected(selected_thread == Some(id))
                        .border_l_2()
                        .border_color(if keyboard_cursor {
                            theme.muted_foreground
                        } else {
                            theme.transparent
                        })
                        .when(keyboard_cursor, |item| item.bg(theme.list_active))
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
                                        .child(div().flex_1().min_w_0().truncate().when(untouched, |label| {
                                            label.italic().text_color(theme.muted_foreground)
                                        }).map(|label| {
                                            match state.is_titling(id) {
                                                true => label
                                                    .child(ShimmerText::new(thread.title.clone())),
                                                false => label.child(thread.title.clone()),
                                            }
                                        }))
                                        // Pointer shortcuts only: the context menu and palette
                                        // carry the same commands. The lane keeps its width so
                                        // long titles do not jump when the pointer enters; the
                                        // activity mark holds it otherwise.
                                        .when(!thread.archived, |view| {
                                            view.child(
                                                h_flex()
                                                    .w(rems(2.75))
                                                    .flex_shrink_0()
                                                    .justify_end()
                                                    .when(!pointer_hovered, |lane| {
                                                        lane.pr_1().children(activity::mark(
                                                            SharedString::from(format!(
                                                                "thread-activity-{}-{id}",
                                                                if active { "rows" } else { "preview" }
                                                            )),
                                                            state.thread_activity(id),
                                                            cx,
                                                        ))
                                                    })
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
                                                                                        workspace.rename_thread(id, window, cx);
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
                                                                                        workspace.archive_thread(id, window, cx);
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
    let empty = state
        .snapshot
        .projects
        .iter()
        .filter(|project| state.project_in_space(project.id, space))
        .all(|project| project.archived && !show_archived);
    if empty {
        list = list.child(
            v_flex()
                .p_3()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "“{}” has no visible projects. Add an existing Git working tree to begin.",
                    state
                        .spaces
                        .iter()
                        .find(|item| item.id == space)
                        .map_or("", |item| item.name.as_str())
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
    list.into_any_element()
}

pub(super) fn render(
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    presentation: SidebarPresentation<'_>,
    search: &Entity<InputState>,
    scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let query = search.read(cx).value();
    let slide = presentation.slide;
    let list = render_list(
        state,
        owner,
        state.active_space,
        presentation,
        &query,
        Some(scroll),
        cx,
    );
    let preview = presentation.neighbor.filter(|_| slide != 0.).map(|space| {
        render_list(
            state,
            owner,
            space,
            SidebarPresentation {
                cursor: None,
                hovered: None,
                ..presentation
            },
            presentation.neighbor_query,
            presentation.neighbor_scroll,
            cx,
        )
    });
    let new_owner = owner.clone();
    let search_owner = owner.clone();
    let search_next_owner = owner.clone();
    let space_menu_owner = owner.clone();
    let swipe_owner = owner.clone();
    v_flex()
        .size_full()
        .relative()
        .bg(theme.sidebar)
        .text_color(theme.sidebar_foreground)
        .children(super::grain::layer(super::theme::grain(cx)))
        // Mouse back/forward buttons; macOS also reports three-finger swipes this way.
        .on_mouse_down(
            MouseButton::Navigate(NavigationDirection::Back),
            |_, window, cx| window.dispatch_action(Box::new(PreviousSpace), cx),
        )
        .on_mouse_down(
            MouseButton::Navigate(NavigationDirection::Forward),
            |_, window, cx| window.dispatch_action(Box::new(NextSpace), cx),
        )
        .on_scroll_wheel(move |event, window, cx| {
            let _ = swipe_owner.update(cx, |workspace, cx| {
                workspace.swipe_space(event, window, cx);
            });
        })
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
                            Input::new(search)
                                .small()
                                .appearance(false)
                                .cleanable(true)
                                .prefix(
                                    Icon::new(IconName::Search)
                                        .small()
                                        .text_color(theme.muted_foreground),
                                ),
                        ),
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
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(
                    v_flex()
                        .relative()
                        .size_full()
                        .left(relative(slide))
                        .child(list),
                )
                .when_some(preview, |view, preview| {
                    view.child(
                        v_flex()
                            .absolute()
                            .top_0()
                            .size_full()
                            .left(relative(slide - slide.signum()))
                            .child(preview),
                    )
                })
                // Neither outgoing nor preview controls may act on a moving Space.
                .when(slide != 0., |view| {
                    view.child(div().absolute().inset_0().block_mouse_except_scroll())
                }),
        )
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
                        .tooltip("New tab…")
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
                            .item(command_item(
                                Command::NewTerminal.label(),
                                Command::NewTerminal,
                                state,
                            ))
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
            worktree_id: None,
            pull_request: None,
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
    fn threads_are_ordered_by_last_touched_within_each_project() {
        let (dungeon, lighting) = (project("masters-of-the-dungeon"), project("lighting"));
        let touched = |mut thread: Thread, at: &str| {
            thread.updated_at = at.into();
            thread
        };
        let threads = [
            touched(thread(&dungeon, "Oldest", false), "2026-09-10T08:00:00Z"),
            touched(thread(&dungeon, "Unreadable", false), ""),
            touched(thread(&dungeon, "Newest", false), "2026-09-21T17:30:00Z"),
            touched(thread(&lighting, "Lighting", false), "2026-09-22T09:00:00Z"),
            touched(thread(&dungeon, "Middle", false), "2026-09-15T12:00:00Z"),
        ];
        let projects = [dungeon.clone(), lighting.clone()];
        let collapsed = HashSet::new();

        let expected = [
            Row::Project(dungeon.id),
            Row::Thread(threads[2].id),
            Row::Thread(threads[4].id),
            Row::Thread(threads[0].id),
            Row::Thread(threads[1].id),
            Row::Project(lighting.id),
            Row::Thread(threads[3].id),
        ];
        assert_eq!(
            visible_rows(&projects, &threads, &collapsed, false, ""),
            expected
        );
        assert_eq!(
            visible_rows(&projects, &threads, &collapsed, false, "e"),
            [
                Row::Project(dungeon.id),
                Row::Thread(threads[2].id),
                Row::Thread(threads[4].id),
                Row::Thread(threads[0].id),
                Row::Thread(threads[1].id),
            ]
        );
    }

    #[test]
    fn sideways_swipe_steps_one_space_per_gesture() {
        use super::{Command, SpaceSwipe};
        use gpui_kit::{TouchPhase, point};
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut swipe = SpaceSwipe::default();

        // Crossing the threshold stays attached until release, including a held pause.
        assert_eq!(swipe.feed(point(50., 2.), TouchPhase::Started, at(0)), None);
        assert_eq!(swipe.feed(point(40., 1.), TouchPhase::Moved, at(16)), None);
        assert_eq!(swipe.progress(), Some(90.));
        assert!(!swipe.needs_idle_release());
        assert_eq!(swipe.feed(point(10., 0.), TouchPhase::Moved, at(500)), None);
        assert_eq!(swipe.progress(), Some(100.));
        assert_eq!(
            swipe.feed(point(0., 0.), TouchPhase::Ended, at(516)),
            Some(Command::PreviousSpace)
        );
        assert_eq!(
            swipe.feed(point(200., 0.), TouchPhase::Moved, at(532)),
            None
        );
        assert_eq!(swipe.release(), None);

        // Without touch phases (Wayland), the owner releases after the idle gap.
        assert_eq!(
            swipe.feed(point(-90., 0.), TouchPhase::Moved, at(700)),
            None
        );
        assert!(swipe.needs_idle_release());
        assert_eq!(swipe.release(), Some(Command::NextSpace));

        // Vertical scrolling with sideways wobble never steps.
        assert_eq!(
            swipe.feed(point(90., 300.), TouchPhase::Started, at(800)),
            None
        );
        assert_eq!(swipe.feed(point(90., 0.), TouchPhase::Moved, at(816)), None);
        assert_eq!(swipe.release(), None);

        // Short travel is abandoned once the gesture goes idle.
        assert_eq!(
            swipe.feed(point(-60., 0.), TouchPhase::Moved, at(1200)),
            None
        );
        assert_eq!(swipe.release(), None);
        assert_eq!(
            swipe.feed(point(-60., 0.), TouchPhase::Moved, at(1600)),
            None
        );
        assert_eq!(swipe.release(), None);
    }

    #[test]
    fn swipe_preview_reverses_and_cancels_without_momentum_switching() {
        use super::SpaceSwipe;
        use gpui_kit::{TouchPhase, point};
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let mut swipe = SpaceSwipe::default();
        let mut feed = |x, phase, ms| {
            let command = swipe.feed(point(x, 0.), phase, start + Duration::from_millis(ms));
            (command, swipe.progress())
        };
        assert_eq!(feed(-100., TouchPhase::Started, 0), (None, Some(-100.)));
        assert_eq!(feed(80., TouchPhase::Moved, 16), (None, Some(-20.)));
        assert_eq!(feed(0., TouchPhase::Ended, 32), (None, None));
        assert_eq!(feed(-100., TouchPhase::Moved, 48), (None, None));
        assert_eq!(feed(20., TouchPhase::Started, 64), (None, Some(20.)));
        swipe.finish();
        assert_eq!(swipe.progress(), None);
        assert_eq!(
            swipe.feed(
                point(100., 0.),
                TouchPhase::Moved,
                start + Duration::from_millis(80)
            ),
            None
        );
    }

    #[test]
    fn tracker_reference_is_compact() {
        use crate::model::{PullRequestLink, PullRequestState};
        let pr = |work_items: Vec<u64>| PullRequestLink {
            id: 3170,
            url: String::new(),
            title: String::new(),
            state: PullRequestState::Active,
            work_items,
            archived_on_close: false,
        };
        assert_eq!(super::tracker_reference(None, None), None);
        assert_eq!(
            super::tracker_reference(None, Some(42)).as_deref(),
            Some("#42")
        );
        assert_eq!(
            super::tracker_reference(Some(&pr(vec![])), None).as_deref(),
            Some("!3170")
        );
        assert_eq!(
            super::tracker_reference(Some(&pr(vec![2596])), None).as_deref(),
            Some("!3170 #2596")
        );
        assert_eq!(
            super::tracker_reference(Some(&pr(vec![1, 2, 3, 4])), None).as_deref(),
            Some("!3170 #1 #2 +2")
        );
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
