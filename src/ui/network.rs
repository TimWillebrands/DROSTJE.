//! Space-local membership and mailbox inspection. Selection never retrieves an agent's mail.
use super::{
    agents::Activity,
    commands::{self, Command, button},
    workspace::{Region, Workspace},
};
use crate::{
    app::{AppState, PresentationChange},
    model::ThreadId,
    network::{Member, NetworkMessage},
};
use gpui_kit::{
    component::{
        ActiveTheme, IndexPath, Sizable,
        alert::Alert,
        h_flex,
        label::Label,
        list::{List, ListDelegate, ListEvent, ListItem, ListState},
        tab::{Tab, TabBar},
        tag::Tag,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum View {
    Members,
    Messages,
    Thread,
}

#[derive(Clone)]
enum Row {
    Member(Member),
    Message(NetworkMessage),
}
impl Row {
    fn id(&self) -> uuid::Uuid {
        match self {
            Self::Member(member) => member.thread,
            Self::Message(message) => message.id,
        }
    }
}

struct NetworkList {
    rows: Vec<Row>,
    selected: Option<IndexPath>,
    owner: WeakEntity<Workspace>,
    empty: &'static str,
}
impl ListDelegate for NetworkList {
    type Item = ListItem;
    fn items_count(&self, _: usize, _: &App) -> usize {
        self.rows.len()
    }
    fn set_selected_index(
        &mut self,
        selected: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = selected;
    }
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(self.empty)
    }
    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let row = self.rows.get(ix.row)?;
        let (title, description, status) = match row {
            Row::Member(member) => (
                member.title.clone(),
                format!(
                    "{} · {}",
                    member.project,
                    crate::harness::display_name(&member.harness)
                ),
                member.status.clone(),
            ),
            Row::Message(message) => (
                if message.broadcast {
                    format!("{} → Everyone", message.sender_name)
                } else {
                    format!(
                        "{} → {}",
                        message.sender_name,
                        message
                            .deliveries
                            .iter()
                            .map(|delivery| delivery.recipient_name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
                message.body.lines().next().unwrap_or("").to_owned(),
                delivery_summary(message),
            ),
        };
        Some(
            ListItem::new(SharedString::from(format!("network-row-{}", row.id())))
                .selected(self.selected == Some(ix))
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .text_ellipsis()
                                        .child(title),
                                )
                                .child(Tag::secondary().outline().xsmall().child(status)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .text_ellipsis()
                                .child(description),
                        ),
                ),
        )
    }
    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(Row::Member(member)) = self.selected.and_then(|ix| self.rows.get(ix.row)) else {
            return;
        };
        let thread = member.thread;
        let _ = self.owner.update(cx, |workspace, cx| {
            workspace.state.update(cx, |state, cx| {
                if state.thread_in_active_space(thread) {
                    state.select_thread(thread);
                    cx.emit(PresentationChange::Selection);
                }
            });
            workspace.focus_region(Region::Thread, window, cx);
        });
    }
}

fn delivery_summary(message: &NetworkMessage) -> String {
    let retrieved = message
        .deliveries
        .iter()
        .filter(|delivery| delivery.retrieved)
        .count();
    let pending = message
        .deliveries
        .iter()
        .filter(|delivery| !delivery.retrieved && delivery.closed.is_none())
        .count();
    if pending > 0 {
        format!("{pending} queued")
    } else if retrieved == message.deliveries.len() {
        "Retrieved".into()
    } else {
        "Closed".into()
    }
}

pub(super) struct NetworkPanel {
    id: ThreadId,
    state: Entity<AppState>,
    owner: WeakEntity<Workspace>,
    view: View,
    focus: FocusHandle,
    revision: Option<(u64, Option<crate::model::SpaceId>)>,
    list: Entity<ListState<NetworkList>>,
    inspection_focus: FocusHandle,
    inspection_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}
impl NetworkPanel {
    pub(super) fn new(
        id: ThreadId,
        state: Entity<AppState>,
        owner: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let list = cx.new(|cx| {
            ListState::new(
                NetworkList {
                    rows: Vec::new(),
                    selected: None,
                    owner: owner.clone(),
                    empty: "No agents have joined this Space network",
                },
                window,
                cx,
            )
        });
        let inspection_focus = cx.focus_handle();
        let subscriptions = vec![
            cx.subscribe_in(
                &state,
                window,
                move |this, _, event: &PresentationChange, window, cx| {
                    if matches!(event, PresentationChange::Thread(thread) if *thread == id) {
                        this.sync(window, cx);
                        cx.notify();
                    }
                },
            ),
            cx.observe_in(&state, window, |this, _, window, cx| {
                this.sync(window, cx);
                cx.notify();
            }),
            cx.subscribe(&list, |this, _, _: &ListEvent, cx| {
                this.inspection_scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
            }),
            cx.on_focus(&inspection_focus, window, |_, _, cx| cx.notify()),
            cx.on_blur(&inspection_focus, window, |_, _, cx| cx.notify()),
        ];
        let mut panel = Self {
            id,
            state,
            owner,
            view: View::Members,
            focus: cx.focus_handle(),
            revision: None,
            list,
            inspection_focus,
            inspection_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        panel.sync(window, cx);
        panel
    }
    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus_handle(cx).focus(window, cx);
    }
    pub(super) fn show_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.view = view;
        self.revision = None;
        self.inspection_scroll.set_offset(point(px(0.), px(0.)));
        self.sync(window, cx);
        self.focus(window, cx);
        cx.notify();
    }
    /// Follow the current turn's coordination activity without moving keyboard focus.
    pub(super) fn follow_thread_activity(&mut self, cx: &mut Context<Self>) {
        self.view = View::Thread;
        self.revision = None;
        self.inspection_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let space = state.thread_space(self.id);
        let revision = (state.network.revision, space);
        if self.revision == Some(revision) {
            return;
        }
        self.revision = Some(revision);
        let rows = match self.view {
            View::Members => state
                .network
                .snapshot
                .members
                .iter()
                .filter(|member| {
                    Some(member.space) == space
                        && state.thread_in_space(member.thread, member.space)
                })
                .cloned()
                .map(|mut member| {
                    if state.network.snapshot.waiting.contains(&member.thread) {
                        member.status = "Waiting for message".into();
                    }
                    Row::Member(member)
                })
                .collect(),
            View::Messages => state
                .network
                .snapshot
                .messages
                .iter()
                .rev()
                .filter(|message| Some(message.space) == space)
                .cloned()
                .map(Row::Message)
                .collect(),
            View::Thread => Vec::new(),
        };
        let empty = match self.view {
            View::Members => "No agents have joined this Space network",
            View::Messages => "Direct messages and broadcasts will appear here",
            View::Thread => "No local agent activity",
        };
        self.list.update(cx, |list, cx| {
            let selected = list
                .delegate()
                .selected
                .and_then(|ix| list.delegate().rows.get(ix.row))
                .map(Row::id);
            let delegate = list.delegate_mut();
            delegate.rows = rows;
            delegate.empty = empty;
            let selection = selected
                .and_then(|id| delegate.rows.iter().position(|row| row.id() == id))
                .or_else(|| (!delegate.rows.is_empty()).then_some(0))
                .map(IndexPath::new);
            list.set_selected_index(selection, window, cx);
            cx.notify();
        });
    }
    fn navigate(&mut self, delta: isize, edge: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.view == View::Thread || self.inspection_focus.is_focused(window) {
            if edge {
                if delta < 0 {
                    self.inspection_scroll.set_offset(point(px(0.), px(0.)));
                } else {
                    self.inspection_scroll.scroll_to_bottom();
                }
            } else {
                let offset = self.inspection_scroll.offset();
                let distance = window.rem_size() * (delta as f32 * 2.25);
                self.inspection_scroll
                    .set_offset(point(offset.x, (offset.y - distance).min(px(0.))));
            }
            cx.notify();
            return;
        }
        self.list.update(cx, |list, cx| {
            let count = list.delegate().rows.len();
            if count == 0 {
                return;
            }
            let current = list.selected_index().map_or(0, |ix| ix.row);
            let row = if edge {
                if delta < 0 { 0 } else { count - 1 }
            } else {
                (current as isize + delta).clamp(0, count as isize - 1) as usize
            };
            let ix = IndexPath::new(row);
            list.set_selected_index(Some(ix), window, cx);
            list.scroll_to_item(ix, ScrollStrategy::Center, window, cx);
        });
        cx.notify();
    }
}
impl Focusable for NetworkPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.view == View::Thread {
            self.focus.clone()
        } else {
            self.list.focus_handle(cx)
        }
    }
}
impl Render for NetworkPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let joined = state.network_joined(self.id);
        let pending = state.network.pending.contains(&self.id);
        let space = state.thread_space(self.id);
        let name = space
            .and_then(|space| state.spaces.iter().find(|candidate| candidate.id == space))
            .map_or("Space", |space| space.name.as_str());
        let pending_messages = state
            .network
            .snapshot
            .messages
            .iter()
            .filter(|message| Some(message.space) == space)
            .flat_map(|message| &message.deliveries)
            .filter(|delivery| {
                delivery.recipient == self.id && !delivery.retrieved && delivery.closed.is_none()
            })
            .count();
        let header = v_flex().flex_shrink_0().p_2().gap_2()
            .child(h_flex().min_w_0().gap_2()
                .child(div().flex_1().min_w_0().text_ellipsis().child(Label::new(format!("Space network · {name}")).text_sm()))
                .child(button("toggle-space-network", if joined { Command::LeaveSpaceNetwork } else { Command::JoinSpaceNetwork }, &self.owner, state)
                    .loading(pending).debug_selector(|| "toggle-space-network".into())))
            .child(div().text_xs().text_color(cx.theme().muted_foreground)
                .child(if joined {
                    format!("Joined · {pending_messages} queued. network wait pauses the current turn; inbox retrieves mail. Messages never start turns.")
                } else {
                    "Join to make this agent discoverable and exchange messages in this Space. Conversation history and Drafts stay private.".into()
                }))
            .when_some(state.network.error.clone(), |header, error| header.child(Alert::error("network-error", error).small()));
        let selected_view = match self.view {
            View::Members => 0,
            View::Messages => 1,
            View::Thread => 2,
        };
        let owner = self.owner.clone();
        let thread = self.id;
        let tabs = TabBar::new("agent-network-views")
            .segmented()
            .selected_index(selected_view)
            .children([
                Tab::new().label("Members"),
                Tab::new().label("Messages"),
                Tab::new().label("Thread activity"),
            ])
            .on_click(move |ix, window, cx| {
                let command = match ix {
                    0 => Command::ShowNetworkMembers,
                    1 => Command::ShowNetworkMessages,
                    _ => Command::ShowThreadAgents,
                };
                let _ = owner.update(cx, |workspace, cx| {
                    workspace
                        .state
                        .update(cx, |state, _| state.select_thread(thread));
                    workspace.execute(command, window, cx);
                });
            });
        let content = if self.view == View::Thread {
            let activity = state
                .conversations
                .get(&self.id)
                .map(|session| {
                    Activity::from_session(session, state.active_threads.contains(&self.id))
                })
                .unwrap_or_default();
            super::thread_sidebar::agent_activity(&activity, &self.inspection_scroll, cx)
                .into_any_element()
        } else {
            let detail = self
                .list
                .read(cx)
                .delegate()
                .selected
                .and_then(|ix| self.list.read(cx).delegate().rows.get(ix.row))
                .cloned();
            v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(List::new(&self.list).small()),
                )
                .when_some(detail, |body, row| {
                    body.child(detail_view(
                        row,
                        &self.inspection_focus,
                        &self.inspection_scroll,
                        window,
                        cx,
                    ))
                })
                .into_any_element()
        };
        let selector = format!("space-network-sidebar-{}", self.id);
        v_flex()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .test_support()
            .track_focus(&self.focus)
            .key_context("Network List")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .on_action(cx.listener(|this, _: &commands::SelectNext, window, cx| {
                this.navigate(1, false, window, cx);
                cx.stop_propagation();
            }))
            .on_action(
                cx.listener(|this, _: &commands::SelectPrevious, window, cx| {
                    this.navigate(-1, false, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_action(cx.listener(|this, _: &commands::SelectFirst, window, cx| {
                this.navigate(-1, true, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &commands::SelectLast, window, cx| {
                this.navigate(1, true, window, cx);
                cx.stop_propagation();
            }))
            .on_action(
                cx.listener(|this, _: &commands::SelectPageDown, window, cx| {
                    this.navigate(8, false, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_action(cx.listener(|this, _: &commands::SelectPageUp, window, cx| {
                this.navigate(-8, false, window, cx);
                cx.stop_propagation();
            }))
            .on_action(
                cx.listener(|this, _: &commands::OpenSelection, window, cx| {
                    let is_message = this
                        .list
                        .read(cx)
                        .delegate()
                        .selected
                        .and_then(|ix| this.list.read(cx).delegate().rows.get(ix.row))
                        .is_some_and(|row| matches!(row, Row::Message(_)));
                    if is_message {
                        this.inspection_focus.focus(window, cx);
                    } else {
                        this.list.update(cx, |list, cx| {
                            list.delegate_mut().confirm(false, window, cx)
                        });
                    }
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_action(cx.listener(|this, _: &commands::Dismiss, window, cx| {
                if this.inspection_focus.is_focused(window) {
                    this.focus(window, cx);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .child(tabs)
            .child(content)
    }
}

fn detail_view(
    row: Row,
    focus: &FocusHandle,
    scroll: &ScrollHandle,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let content = match row {
        Row::Member(member) => v_flex()
            .gap_1()
            .child(Label::new(member.title).secondary(member.project))
            .child(
                div()
                    .font_family(cx.theme().mono_font_family.clone())
                    .whitespace_normal()
                    .child(member.worktree),
            )
            .child(format!(
                "{} · {}",
                crate::harness::display_name(&member.harness),
                member.status
            ))
            .child("Return opens this Thread; absence of a message is not a file reservation")
            .into_any_element(),
        Row::Message(message) => v_flex()
            .gap_2()
            .child(Label::new(message.sender_name).secondary(message.created_at))
            .child("Return focuses this message; j/k scroll, Escape returns to the list")
            .child(div().whitespace_normal().child(message.body))
            .children(message.deliveries.into_iter().map(|delivery| {
                let status = if delivery.retrieved {
                    "Retrieved by agent".to_owned()
                } else if let Some(reason) = delivery.closed {
                    format!("Closed: {reason}")
                } else {
                    "Queued; not retrieved".into()
                };
                Label::new(delivery.recipient_name).secondary(status)
            }))
            .into_any_element(),
    };
    div()
        .id("network-selection-detail")
        .debug_selector(|| "network-selection-detail".into())
        .test_support()
        .key_context("NetworkDetail")
        .track_focus(focus)
        .track_scroll(scroll)
        .bg(if focus.is_focused(window) {
            cx.theme().list_active
        } else {
            cx.theme().sidebar
        })
        .on_mouse_down(MouseButton::Left, {
            let focus = focus.clone();
            move |_, window, cx| {
                focus.focus(window, cx);
                cx.stop_propagation();
            }
        })
        .max_h(rems(16.))
        .min_h_0()
        .overflow_y_scroll()
        .border_t_1()
        .border_color(cx.theme().border)
        .p_3()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(content)
        .into_any_element()
}
