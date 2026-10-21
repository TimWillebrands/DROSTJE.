//! Space-local membership and delivery inspection. Rendering never delivers or acknowledges mail.
use super::{
    agents::Activity,
    commands::{self, Command, button},
    workspace::{Region, Workspace},
};
use crate::{
    app::{AppState, PresentationChange},
    model::ThreadId,
    network::{DeliveryState, Member, NetworkMessage},
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
    state: Entity<AppState>,
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
                    "{} · {} · Native: {}",
                    member.project,
                    crate::harness::display_name(&member.harness),
                    member.status
                ),
                network_readiness(member, self.state.read(cx)).to_owned(),
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

fn network_readiness(member: &Member, state: &AppState) -> &'static str {
    if !member.automatic {
        "Network: consent needed"
    } else if !member.delivery_enabled {
        "Network: paused"
    } else if state.network_delivery_reason(member.thread).is_some() {
        "Network: blocked"
    } else {
        "Network: ready"
    }
}

fn delivery_label(state: &DeliveryState) -> &'static str {
    match state {
        DeliveryState::Queued => "Queued",
        DeliveryState::Delivering => "Delivering",
        DeliveryState::Delivered => "Delivered",
        DeliveryState::Failed { .. } => "Failed",
        DeliveryState::Unknown { .. } => "Unknown",
        DeliveryState::Closed { .. } => "Closed",
    }
}

fn delivery_summary(message: &NetworkMessage) -> String {
    let mut counts = [0; 6];
    for delivery in &message.deliveries {
        let index = match &delivery.state {
            DeliveryState::Queued => 0,
            DeliveryState::Delivering => 1,
            DeliveryState::Delivered => 2,
            DeliveryState::Failed { .. } => 3,
            DeliveryState::Unknown { .. } => 4,
            DeliveryState::Closed { .. } => 5,
        };
        counts[index] += 1;
    }
    [
        "Queued",
        "Delivering",
        "Delivered",
        "Failed",
        "Unknown",
        "Closed",
    ]
    .into_iter()
    .zip(counts)
    .filter(|(_, count)| *count > 0)
    .map(|(label, count)| format!("{label}: {count}"))
    .collect::<Vec<_>>()
    .join(" · ")
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
                    state: state.clone(),
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
            self.list.update(cx, |_, cx| cx.notify());
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
                .map(Row::Member)
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
                delivery.recipient == self.id && matches!(&delivery.state, DeliveryState::Queued)
            })
            .count();
        let delivery_enabled = state.network_delivery_enabled(self.id);
        let delivery_reason = joined
            .then(|| state.network_delivery_reason(self.id))
            .flatten();
        let header = v_flex().flex_shrink_0().p_2().gap_2()
            .child(h_flex().min_w_0().gap_2()
                .child(div().flex_1().min_w_0().text_ellipsis().child(Label::new(format!("Space network · {name}")).text_sm()))
                .when(joined && !delivery_enabled, |header| {
                    header.child(button("resume-network-delivery", Command::ResumeNetworkDelivery, &self.owner, state)
                        .label("Resume")
                        .loading(pending).debug_selector(|| "resume-network-delivery".into()))
                })
                .child(button("toggle-space-network", if joined { Command::LeaveSpaceNetwork } else { Command::JoinSpaceNetwork }, &self.owner, state)
                    .label(if joined { "Leave" } else { "Join" })
                    .loading(pending).debug_selector(|| "toggle-space-network".into())))
            .child(Label::new(if joined {
                format!("{} · {pending_messages} queued", if delivery_enabled { "Automatic delivery enabled" } else { "Automatic delivery paused" })
            } else {
                "Not joined".into()
            }).text_xs())
            .when_some(delivery_reason, |header, reason| {
                header.child(Label::new(reason).text_xs().text_color(cx.theme().muted_foreground))
            })
            .child(Label::new("Join or Resume consents to automatic delivery at safe boundaries while busy and paid-turn wakeups while idle. Drafts and conversation history stay private. Stopped or closed agents never restart from a message; paused legacy members need Resume consent.")
                .text_xs().text_color(cx.theme().muted_foreground))
            .child(Label::new("Delivered means the native runtime accepted the message, not that the model read or acted on it. Failed and Unknown deliveries are not retried automatically.")
                .text_xs().text_color(cx.theme().muted_foreground))
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
                        state,
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
    state: &AppState,
    cx: &App,
) -> AnyElement {
    let content = match row {
        Row::Member(member) => {
            let readiness = network_readiness(&member, state);
            let reason = if !member.automatic {
                "Automatic delivery needs new consent. Open this Thread and choose Resume network delivery.".into()
            } else {
                state.network_delivery_reason(member.thread).unwrap_or_else(|| {
                    if member.delivery_enabled {
                        "Eligible for automatic delivery at native safe boundaries or an idle paid turn.".into()
                    } else {
                        "Open this Thread and choose Resume network delivery.".into()
                    }
                })
            };
            v_flex()
                .gap_1()
                .child(Label::new(member.title).secondary(member.project))
                .child(
                    div()
                        .font_family(cx.theme().mono_font_family.clone())
                        .whitespace_normal()
                        .child(member.worktree),
                )
                .child(Label::new("Native state").secondary(format!(
                    "{} · {}",
                    crate::harness::display_name(&member.harness),
                    member.status
                )))
                .child(Label::new(readiness).secondary(reason))
                .child(Label::new(
                    "Return opens this Thread; absence of a message is not a file reservation",
                ))
                .into_any_element()
        }
        Row::Message(message) => v_flex()
            .gap_2()
            .child(Label::new(message.sender_name).secondary(message.created_at))
            .child("Return focuses this message; j/k scroll, Escape returns to the list")
            .child(div().whitespace_normal().child(message.body))
            .children(message.deliveries.into_iter().map(|delivery| {
                let detail = match &delivery.state {
                    DeliveryState::Queued => state
                        .network_delivery_reason(delivery.recipient)
                        .unwrap_or_else(|| {
                            "Waiting for the next native safe boundary or idle turn".into()
                        }),
                    DeliveryState::Delivering => "Awaiting native runtime acceptance".to_owned(),
                    DeliveryState::Delivered => {
                        "Accepted by the native runtime; not proof of model action".to_owned()
                    }
                    DeliveryState::Failed { reason }
                    | DeliveryState::Unknown { reason }
                    | DeliveryState::Closed { reason } => reason.clone(),
                };
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(Label::new(delivery.recipient_name))
                            .child(
                                Tag::secondary()
                                    .outline()
                                    .xsmall()
                                    .child(delivery_label(&delivery.state)),
                            ),
                    )
                    .child(Label::new(detail).text_color(cx.theme().muted_foreground))
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
