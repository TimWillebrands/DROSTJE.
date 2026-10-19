//! Retained, resizable thread sidebar: Changes, Output, Browser and optional Agents
//! for Conversations; Output for Factory threads. AppState owns tab, open state and width.

use super::{
    agents::{self, Activity, Event, EventKind, Outcome},
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
        ActiveTheme, IconName, Sizable,
        alert::Alert,
        h_flex,
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

/// Tabs in display order; a Factory thread has no working-tree Changes, and Agents
/// appears only once a Conversation's agent has worked with another.
fn tabs(conversation: bool, agents: bool) -> &'static [SidebarTab] {
    match (conversation, agents) {
        (true, true) => &[
            SidebarTab::Changes,
            SidebarTab::Output,
            SidebarTab::Agents,
            SidebarTab::Browser,
        ],
        (true, false) => &[SidebarTab::Changes, SidebarTab::Output, SidebarTab::Browser],
        (false, _) => &[SidebarTab::Output],
    }
}

fn label(tab: SidebarTab) -> &'static str {
    match tab {
        SidebarTab::Changes => "Changes",
        SidebarTab::Output => "Output",
        SidebarTab::Agents => "Agents",
        SidebarTab::Browser => "Browser",
    }
}

/// The command that shows or hides `tab`; the collapse control dispatches it too.
fn toggle_command(tab: SidebarTab) -> Command {
    match tab {
        SidebarTab::Changes => Command::ToggleConversationDiffs,
        SidebarTab::Output => Command::ToggleOutput,
        SidebarTab::Agents => Command::ToggleAgents,
        SidebarTab::Browser => Command::ToggleBrowser,
    }
}

pub(super) fn render(
    state: &AppState,
    id: ThreadId,
    conversation: bool,
    owner: &WeakEntity<Workspace>,
    output_scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    let open = state.thread_sidebar_open(id);
    let session = conversation.then(|| state.conversations.get(&id)).flatten();
    let has_agents = session.is_some_and(|session| agents::coordination_calls(session) > 0);
    let tabs = tabs(conversation, has_agents);
    // A saved Agents tab whose activity is not loaded yet shows the first tab meanwhile.
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
    let loading = active == SidebarTab::Changes
        && state.conversation_diff(id).is_none_or(|diff| diff.loading);
    let header = TabBar::new("thread-sidebar-tabs")
        .selected_index(selected)
        .children(tabs.iter().map(|tab| Tab::new().label(label(*tab))))
        .on_click(move |ix, window, cx| {
            let Some(tab) = tabs.get(*ix).copied() else {
                return;
            };
            let _ = tab_owner.update(cx, |workspace, cx| {
                workspace.show_sidebar_tab(id, tab, window, cx)
            });
        })
        .suffix(
            h_flex()
                .h_full()
                .items_center()
                .gap_1()
                .px_1()
                .when(loading, |row| row.child(Spinner::new().xsmall()))
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
        SidebarTab::Changes => changes(state, id, cx),
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
        SidebarTab::Output => div()
            .flex_1()
            .min_h_0()
            .child(factory::output(state, id, owner, output_scroll, cx))
            .into_any_element(),
        SidebarTab::Agents => agent_activity(
            &session
                .map(|session| Activity::from_session(session, state.active_threads.contains(&id)))
                .unwrap_or_default(),
            cx,
        ),
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
        .child(body)
        .into_any_element()
}

fn changes(state: &AppState, id: ThreadId, cx: &App) -> AnyElement {
    let diff = state.conversation_diff(id);
    let loading = diff.is_none_or(|diff| diff.loading);
    let result = diff.and_then(|diff| diff.result.as_ref());
    let summary = match result {
        Some(Ok(diff)) if diff.changed_files > 0 => format!(
            "{} files · +{} −{}",
            diff.changed_files, diff.additions, diff.deletions
        ),
        Some(Ok(_)) => "Working tree clean".to_owned(),
        Some(Err(_)) => "Changes unavailable".to_owned(),
        None if loading => "Loading changes…".to_owned(),
        None => "Changes not loaded".to_owned(),
    };
    let content = match result {
        Some(Ok(diff)) if diff.patch.is_empty() => div()
            .flex_1()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No uncommitted changes")
            .into_any_element(),
        Some(Ok(diff)) => div()
            .id("conversation-diff-patch")
            .debug_selector(|| "conversation-diff-patch".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .whitespace_normal()
            .child(diff.patch.clone())
            .into_any_element(),
        Some(Err(error)) => v_flex()
            .flex_1()
            .gap_2()
            .p_3()
            .text_sm()
            .child(
                Alert::error(
                    "conversation-diff-error",
                    format!("Changes could not be read: {error}"),
                )
                .small(),
            )
            .child("Retry after restoring access to the working tree.")
            .into_any_element(),
        None => div()
            .flex_1()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(if loading {
                "Reading working tree changes…"
            } else {
                "Open or refresh this conversation to read changes."
            })
            .into_any_element(),
    };
    v_flex()
        .id("conversation-diff-sidebar")
        .debug_selector(|| "conversation-diff-sidebar".into())
        .flex_1()
        .min_h_0()
        .child(
            div()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(summary),
        )
        .child(content)
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

fn agent_activity(activity: &Activity, cx: &App) -> AnyElement {
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

#[cfg(test)]
mod tests {
    use super::{SidebarTab, tabs};

    #[test]
    fn factory_threads_only_offer_output() {
        assert_eq!(
            tabs(true, false),
            &[SidebarTab::Changes, SidebarTab::Output, SidebarTab::Browser]
        );
        assert_eq!(tabs(false, false), &[SidebarTab::Output]);
        assert_eq!(tabs(false, true), &[SidebarTab::Output]);
    }

    #[test]
    fn agents_appear_only_with_agent_activity() {
        assert_eq!(
            tabs(true, true),
            &[
                SidebarTab::Changes,
                SidebarTab::Output,
                SidebarTab::Agents,
                SidebarTab::Browser
            ]
        );
    }
}
