//! Retained, resizable thread sidebar. Terminal is available for Conversations and
//! Factory threads; AppState owns tab, open state and width.

use super::{
    agents::{Activity, Event, EventKind, Outcome},
    commands::{Command, button},
    factory, theme,
    workspace::Workspace,
};
use crate::{
    app::{AppState, SidebarTab},
    model::ThreadId,
};
use gpui_kit::{
    component::{
        ActiveTheme, IconName, Sizable, h_flex,
        spinner::Spinner,
        tab::{Tab, TabBar},
        tag::Tag,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

pub(super) struct SidebarLayout {
    id: ThreadId,
    width: f32,
    open: bool,
    owner: WeakEntity<Workspace>,
    sizes: std::rc::Rc<
        std::cell::RefCell<
            std::collections::BTreeMap<
                ThreadId,
                Entity<gpui_kit::component::resizable::ResizableState>,
            >,
        >,
    >,
}

impl SidebarLayout {
    pub(super) fn new(
        state: &AppState,
        id: ThreadId,
        owner: &WeakEntity<Workspace>,
        cx: &App,
    ) -> Self {
        Self {
            id,
            width: state.thread_sidebar_width(id),
            open: state.thread_sidebar_open(id),
            sizes: owner
                .upgrade()
                .map(|owner| owner.read(cx).sidebar_sizes.clone())
                .unwrap_or_default(),
            owner: owner.clone(),
        }
    }

    pub(super) fn wrap(self, content: AnyElement, sidebar: AnyElement) -> AnyElement {
        if !self.open {
            return h_flex()
                .size_full()
                .min_w_0()
                .min_h_0()
                .items_stretch()
                .child(div().flex_1().min_w_0().h_full().child(content))
                .child(sidebar)
                .into_any_element();
        }
        SidebarSplit {
            layout: self,
            content,
            sidebar,
        }
        .into_any_element()
    }
}

#[derive(IntoElement)]
struct SidebarSplit {
    layout: SidebarLayout,
    content: AnyElement,
    sidebar: AnyElement,
}
impl RenderOnce for SidebarSplit {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
        let layout = self.layout;
        let existing = layout.sizes.borrow().get(&layout.id).cloned();
        let sizes = existing.unwrap_or_else(|| {
            let state = cx.new(|_| ResizableState::default());
            layout.sizes.borrow_mut().insert(layout.id, state.clone());
            state
        });
        h_resizable(SharedString::from(format!("thread-split-{}", layout.id)))
            .with_state(&sizes)
            .child(
                resizable_panel()
                    .size_range(px(240.)..Pixels::MAX)
                    .child(self.content),
            )
            .child(
                resizable_panel()
                    .size(px(layout.width.clamp(180., 1600.)))
                    .size_range(px(180.)..px(1600.))
                    .flex_none()
                    .child(self.sidebar),
            )
            .on_resize(move |sizes, _, cx| {
                if let Some(width) = sizes.read(cx).sizes().get(1).copied() {
                    let owner = layout.owner.clone();
                    let id = layout.id;
                    cx.defer(move |cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.state.update(cx, |state, _| {
                                state.set_thread_sidebar_width(id, f32::from(width))
                            });
                        });
                    });
                }
            })
    }
}

/// Review and Details appear only while their content exists; Agents always offers opt-in.
fn tabs(conversation: bool, review: bool, details: bool) -> Vec<SidebarTab> {
    if !conversation {
        return vec![SidebarTab::Output, SidebarTab::Terminal];
    }
    let mut tabs = vec![SidebarTab::Changes];
    if details {
        tabs.push(SidebarTab::Details);
    }
    if review {
        tabs.push(SidebarTab::Review);
    }
    tabs.push(SidebarTab::Output);
    tabs.push(SidebarTab::Terminal);
    tabs.push(SidebarTab::Agents);
    tabs.push(SidebarTab::Browser);
    tabs
}

fn label(tab: SidebarTab) -> &'static str {
    match tab {
        SidebarTab::Changes => "Changes",
        SidebarTab::Details => "Details",
        SidebarTab::Output => "Output",
        SidebarTab::Terminal => "Terminal",
        SidebarTab::Agents => "Agents",
        SidebarTab::Browser => "Browser",
        SidebarTab::Review => "Review",
    }
}

/// The command that shows or hides `tab`; the collapse control dispatches it too.
fn toggle_command(tab: SidebarTab) -> Command {
    match tab {
        SidebarTab::Changes | SidebarTab::Details => Command::ToggleConversationDiffs,
        SidebarTab::Output => Command::ToggleOutput,
        SidebarTab::Terminal => Command::ToggleTerminal,
        SidebarTab::Agents => Command::ToggleAgents,
        SidebarTab::Browser => Command::ToggleBrowser,
        SidebarTab::Review => Command::ToggleReview,
    }
}

pub(super) fn render(
    state: &AppState,
    id: ThreadId,
    conversation: bool,
    owner: &WeakEntity<Workspace>,
    output_scroll: &ScrollHandle,
    network: Option<&Entity<super::network::NetworkPanel>>,
    cx: &App,
) -> AnyElement {
    let open = state.thread_sidebar_open(id);
    let has_review = state
        .snapshot
        .threads
        .iter()
        .any(|thread| thread.id == id && thread.pull_request.is_some());
    let has_details = owner.upgrade().is_some_and(|owner| {
        owner
            .read(cx)
            .tool_detail
            .as_ref()
            .is_some_and(|(thread, _)| *thread == id)
    });
    let tabs = tabs(conversation, has_review, has_details);
    let active = Some(state.thread_sidebar_tab(id))
        .filter(|tab| tabs.contains(tab))
        .unwrap_or(tabs[0]);
    let toggle = button(
        "toggle-thread-sidebar",
        toggle_command(active),
        owner,
        state,
    )
    .label("")
    .icon(if open {
        IconName::ChevronRight
    } else {
        IconName::ChevronLeft
    })
    .tooltip(if open { "Hide sidebar" } else { "Show sidebar" })
    .debug_selector(|| "toggle-thread-sidebar".into());
    if !open {
        return v_flex()
            .w(rems(2.5))
            .h_full()
            .flex_shrink_0()
            .items_center()
            .border_l_1()
            .border_color(cx.theme().border)
            .pt_2()
            .child(toggle)
            .into_any_element();
    }
    let selected = tabs.iter().position(|tab| *tab == active).unwrap_or(0);
    let tab_owner = owner.clone();
    let terminal_reason = owner.upgrade().and_then(|workspace| {
        let workspace = workspace.read(cx);
        if workspace.sidebar_terminal(id).is_some() {
            return None;
        }
        workspace
            .thread_terminals
            .get(&id)
            .and_then(|entry| entry.error.clone())
            .or_else(|| super::workspace::thread_terminal_location(state, id).err())
    });
    let loading = match active {
        SidebarTab::Changes => state.conversation_diff(id).is_none_or(|diff| diff.loading),
        SidebarTab::Review => state.review_state(id).is_some_and(|review| review.loading),
        SidebarTab::Terminal
        | SidebarTab::Details
        | SidebarTab::Output
        | SidebarTab::Agents
        | SidebarTab::Browser => false,
    };
    let header = TabBar::new("thread-sidebar-tabs")
        .menu(true)
        .selected_index(selected)
        .children(tabs.iter().map(|tab| {
            Tab::new()
                .label(label(*tab))
                .disabled(*tab == SidebarTab::Terminal && terminal_reason.is_some())
        }))
        .on_click(move |ix, window, cx| {
            let Some(tab) = tabs.get(*ix).copied() else {
                return;
            };
            let _ = tab_owner.update(cx, |workspace, cx| {
                if tab == SidebarTab::Terminal {
                    workspace.dispatch_sidebar_terminal(id, window, cx);
                } else {
                    workspace.show_sidebar_tab(id, tab, window, cx);
                }
            });
        })
        .suffix(
            h_flex()
                .h_full()
                .items_center()
                .gap_1()
                .px_1()
                .when(loading, |row| row.child(Spinner::new().xsmall()))
                .when(active == SidebarTab::Review, |row| {
                    row.child(
                        button("refresh-review", Command::RefreshReview, owner, state)
                            .label("Refresh")
                            .debug_selector(|| "refresh-review".into()),
                    )
                })
                .when(active == SidebarTab::Changes, |row| {
                    row.child(
                        button(
                            "refresh-conversation-diff",
                            Command::RefreshConversationDiff,
                            owner,
                            state,
                        )
                        .label("Refresh")
                        .debug_selector(|| "refresh-conversation-diff".into()),
                    )
                })
                .child(toggle),
        );
    let body = match active {
        SidebarTab::Changes => super::changes::render(state, id, owner, cx),
        SidebarTab::Details => owner
            .upgrade()
            .and_then(|workspace| {
                let workspace = workspace.read(cx);
                let (_, tool) = workspace
                    .tool_detail
                    .as_ref()
                    .filter(|(thread, _)| *thread == id)?;
                Some(
                    div()
                        .id("thread-details-inspector")
                        .debug_selector(|| "thread-details-inspector".into())
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .overflow_y_scroll()
                        .track_scroll(output_scroll)
                        .p_3()
                        .child(super::overlays::render_details(
                            workspace, id, tool, owner, cx,
                        ))
                        .into_any_element(),
                )
            })
            .unwrap_or_else(|| div().p_3().child("No details selected").into_any_element()),
        SidebarTab::Review => super::review::render(state, id, owner, cx),
        SidebarTab::Browser => owner
            .upgrade()
            .and_then(|owner| owner.read(cx).sidebar_browser(id))
            .map(|pane| pane.into_any_element())
            .unwrap_or_else(|| {
                v_flex()
                    .p_3()
                    .gap_2()
                    .child("No browser session")
                    .child(
                        button("open-sidebar-browser", Command::ToggleBrowser, owner, state)
                            .label("Open browser"),
                    )
                    .into_any_element()
            }),
        SidebarTab::Terminal => terminal_body(id, owner, state, cx),
        SidebarTab::Output => div()
            .flex_1()
            .min_h_0()
            .child(factory::output(state, id, owner, output_scroll, cx))
            .into_any_element(),
        SidebarTab::Agents => network
            .cloned()
            .map(|panel| panel.into_any_element())
            .unwrap_or_else(|| {
                div()
                    .p_3()
                    .child("Network view is unavailable; reopen this Thread")
                    .into_any_element()
            }),
    };
    v_flex()
        .id("thread-sidebar")
        .debug_selector(|| "thread-sidebar".into())
        .w_full()
        .h_full()
        .min_w_0()
        .min_h_0()
        .flex_shrink_0()
        .border_l_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().sidebar)
        .child(header)
        .when_some(terminal_reason, |panel, reason| {
            panel.child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(reason),
            )
        })
        .child(body)
        .into_any_element()
}

fn terminal_body(
    id: ThreadId,
    owner: &WeakEntity<Workspace>,
    state: &AppState,
    cx: &App,
) -> AnyElement {
    let Some(workspace) = owner.upgrade() else {
        return div().into_any_element();
    };
    let workspace = workspace.read(cx);
    if let Some(entry) = workspace.thread_terminals.get(&id) {
        if let Some(pane) = &entry.pane {
            return div()
                .id("thread-terminal")
                .debug_selector(|| "thread-terminal".into())
                .relative()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(pane.clone())
                .into_any_element();
        }
        if let Some(error) = &entry.error {
            return v_flex()
                .p_3()
                .gap_2()
                .child(error.clone())
                .child(
                    button(
                        "retry-thread-terminal",
                        Command::RestartTerminal,
                        owner,
                        state,
                    )
                    .label("Retry terminal"),
                )
                .into_any_element();
        }
    }
    v_flex()
        .p_3()
        .gap_2()
        .child(Spinner::new().small())
        .child("Opening terminal…")
        .into_any_element()
}

fn caption(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// The state word of a coordination call, in the terms of what it did.
fn outcome(event: &Event) -> Option<Tag> {
    let background = matches!(
        event.kind,
        EventKind::Spawn {
            background: true,
            ..
        }
    );
    let tag = match (event.outcome, &event.kind) {
        (Outcome::Pending, EventKind::Message { .. }) => Tag::secondary().child("Sending"),
        (Outcome::Pending, _) => Tag::secondary().child("Running"),
        // A background spawn answers once the subagent has started, not finished.
        (Outcome::Done, EventKind::Spawn { .. }) if background => Tag::secondary().child("Started"),
        (Outcome::Done, EventKind::Spawn { .. }) => Tag::secondary().child("Finished"),
        // Delivered to the session; whether its agent read it is not reported.
        (Outcome::Done, EventKind::Message { .. }) => Tag::secondary().child("Delivered"),
        (Outcome::Done, EventKind::Listing { .. }) => return None,
        (Outcome::Failed, _) => Tag::danger().child("Failed"),
        (Outcome::Unknown, _) => Tag::secondary().outline().child("Unknown"),
    };
    Some(tag.xsmall())
}

fn event_row(event: &Event, cx: &App) -> AnyElement {
    let (icon, title, body) = match &event.kind {
        EventKind::Spawn {
            description,
            agent_type,
            background,
        } => (
            IconName::Bot,
            format!(
                "Started {}{}",
                agent_type.as_deref().unwrap_or("a subagent"),
                if *background {
                    " in the background"
                } else {
                    ""
                }
            ),
            description.clone(),
        ),
        EventKind::Message {
            to,
            text,
            notify_when_idle,
        } => (
            IconName::ArrowRight,
            if to.is_empty() {
                "Message without a recipient".to_owned()
            } else {
                format!("Message to {to}")
            },
            Some(if *notify_when_idle && text.trim().is_empty() {
                "Asked to hear when it goes idle".to_owned()
            } else {
                text.clone()
            }),
        ),
        EventKind::Listing { sessions } => (
            IconName::Search,
            match sessions {
                Some(1) => "Listed 1 session".to_owned(),
                Some(count) => format!("Listed {count} sessions"),
                None => "Listed sessions".to_owned(),
            },
            None,
        ),
    };
    let failed = event.outcome == Outcome::Failed;
    h_flex()
        .id(SharedString::from(format!("agent-event-{}", event.call_id)))
        .w_full()
        .min_w_0()
        .items_start()
        .gap_2()
        .py_1()
        .child(
            div()
                .pt_0p5()
                .text_color(cx.theme().muted_foreground)
                .child(gpui_kit::component::Icon::new(icon).xsmall()),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .justify_between()
                        .child(div().min_w_0().text_sm().text_ellipsis().child(title))
                        .children(outcome(event)),
                )
                .children(body.filter(|body| !body.trim().is_empty()).map(|body| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .line_clamp(6)
                        .child(body)
                }))
                // A failure says why; other results repeat what the row shows.
                .when(failed, |column| {
                    column.children(event.result.clone().map(|result| caption(result, cx)))
                }),
        )
        .into_any_element()
}

fn session_rows(activity: &Activity, cx: &App) -> Vec<AnyElement> {
    let Some(sessions) = &activity.sessions else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    if let Some(own) = &sessions.own {
        rows.push(
            caption(format!("This conversation is reachable as {own}"), cx).into_any_element(),
        );
    }
    if sessions.groups.is_empty() {
        // Not the listing's usual shape: show it as the harness wrote it.
        rows.push(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .whitespace_normal()
                .child(sessions.raw.clone())
                .into_any_element(),
        );
    }
    for group in &sessions.groups {
        rows.push(caption(group.title.clone(), cx).pt_1().into_any_element());
        for row in &group.rows {
            let messages = activity.messages_to(&row.name);
            rows.push(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .py_0p5()
                    .child(
                        // Busy sessions carry the same mark as this app's running turns.
                        div()
                            .size_1p5()
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(if row.is_busy() {
                                theme::signature(cx)
                            } else {
                                cx.theme().border
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .text_sm()
                                            .text_ellipsis()
                                            .child(row.name.clone()),
                                    )
                                    .children(row.reference.clone().map(|reference| {
                                        div()
                                            .flex_shrink_0()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(reference)
                                    })),
                            )
                            .child(caption(row.details.join(" · "), cx).text_ellipsis()),
                    )
                    .when(messages > 0, |row| {
                        row.child(caption(
                            if messages == 1 {
                                "1 message".to_owned()
                            } else {
                                format!("{messages} messages")
                            },
                            cx,
                        ))
                    })
                    .into_any_element(),
            );
        }
    }
    rows
}

pub(super) fn agent_activity(activity: &Activity, scroll: &ScrollHandle, cx: &App) -> AnyElement {
    let spawns = activity
        .events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::Spawn { .. }))
        .count();
    let messages = activity
        .events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::Message { .. }))
        .count();
    let plural = |count: usize, one: &str, many: &str| {
        if count == 1 {
            format!("1 {one}")
        } else {
            format!("{count} {many}")
        }
    };
    let mut summary = Vec::new();
    if spawns > 0 {
        summary.push(plural(spawns, "subagent", "subagents"));
    }
    if messages > 0 {
        summary.push(plural(messages, "message", "messages"));
    }
    if let Some(sessions) = &activity.sessions {
        summary.push(plural(sessions.len(), "session", "sessions"));
    }
    let content = if activity.is_empty() {
        div()
            .flex_1()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Subagents this conversation starts, messages it sends, and the sessions it can reach appear here.")
            .into_any_element()
    } else {
        let session_rows = session_rows(activity, cx);
        v_flex()
            .id("agent-activity")
            .debug_selector(|| "agent-activity".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(scroll)
            .px_2()
            .pb_2()
            .gap_3()
            .child(
                v_flex()
                    .child(caption("Activity, newest first", cx).py_1())
                    .children(
                        activity
                            .events
                            .iter()
                            .rev()
                            .map(|event| event_row(event, cx)),
                    ),
            )
            .when(!session_rows.is_empty(), |list| {
                list.child(
                    v_flex()
                        .child(caption("Sessions, as last listed", cx).py_1())
                        .children(session_rows),
                )
            })
            .into_any_element()
    };
    v_flex()
        .id("agent-sidebar")
        .debug_selector(|| "agent-sidebar".into())
        .flex_1()
        .min_h_0()
        .child(
            div()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if summary.is_empty() {
                    "No agent activity".to_owned()
                } else {
                    summary.join(" · ")
                }),
        )
        .child(content)
        .into_any_element()
}
