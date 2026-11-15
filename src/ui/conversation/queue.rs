use super::disclosure::{disclosure, is_expanded};
use crate::ui::{commands::ClearQueuedMessages, workspace::Workspace};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{
        ActiveTheme, Disableable, Icon, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        tag::Tag,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

/// Rem height the queue bar adds to the composer: a header plus up to four rows; longer
/// queues scroll inside the bar.
pub(super) fn queued_messages_height(
    thread: &crate::model::Thread,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> f32 {
    if thread.queued.is_empty() {
        return 0.;
    }
    if is_expanded(owner, &format!("queue-collapsed:{}", thread.id), cx) {
        return QUEUE_HEADER_HEIGHT;
    }
    QUEUE_HEADER_HEIGHT + QUEUE_ROW_HEIGHT * thread.queued.len().min(QUEUE_VISIBLE_ROWS) as f32
}
const QUEUE_HEADER_HEIGHT: f32 = 2.25;
const QUEUE_ROW_HEIGHT: f32 = 2.;
const QUEUE_VISIBLE_ROWS: usize = 4;

/// Zed-style queue above the composer: a collapsible header with the count and Clear all,
/// then one row per waiting prompt with Remove, Edit and Send now. The front row is the
/// one the queue sends next. `dispatch_reason` disables Send now while the conversation is
/// idle but unable to start a turn; during a turn, Send now stops that turn first.
pub(super) fn queued_messages(
    thread: &crate::model::Thread,
    active: bool,
    dispatch_reason: Option<String>,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let id = thread.id;
    let collapsed_key = format!("queue-collapsed:{id}");
    let collapsed = is_expanded(owner, &collapsed_key, cx);
    let count = thread.queued.len();
    let clear = owner.clone();
    let header = h_flex()
        .items_center()
        .gap_1()
        .px_2()
        .h(rems(QUEUE_HEADER_HEIGHT))
        .child(disclosure(
            collapsed_key,
            !collapsed,
            owner,
            Some(id),
            if collapsed {
                "Show queued messages"
            } else {
                "Hide queued messages"
            },
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(match count {
                    1 => "1 queued message".to_owned(),
                    count => format!("{count} queued messages"),
                }),
        )
        .child(
            Button::new("clear-queued")
                .xsmall()
                .ghost()
                .label("Clear all")
                .tooltip_with_action(
                    "Remove every queued message…",
                    &ClearQueuedMessages,
                    Some("Thread"),
                )
                .on_click(move |_, window, cx| {
                    let _ = clear.update(cx, |workspace, cx| {
                        workspace.clear_queued_messages(window, cx)
                    });
                }),
        );
    let mut bar = v_flex()
        .id(SharedString::from(format!("queued-messages-{id}")))
        .debug_selector(|| "queued-messages".into())
        .test_support()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(header);
    if collapsed {
        return bar.into_any_element();
    }
    let mut rows = v_flex()
        .id(SharedString::from(format!("queued-rows-{id}")))
        .max_h(rems(QUEUE_ROW_HEIGHT * QUEUE_VISIBLE_ROWS as f32))
        .overflow_y_scroll()
        .w_full()
        .min_w_0();
    for (index, message) in thread.queued.iter().enumerate() {
        let message_id = message.id;
        let remove = owner.clone();
        let edit = owner.clone();
        let send = owner.clone();
        let next = index == 0;
        let send_reason = if active {
            None
        } else {
            dispatch_reason.clone()
        };
        let attachments = message.attachments.len();
        rows = rows.child(
            h_flex()
                .id(SharedString::from(format!("queued-{message_id}")))
                .debug_selector(move || {
                    if next {
                        "queued-row-next".into()
                    } else {
                        "queued-row".into()
                    }
                })
                .items_center()
                .gap_2()
                .px_2()
                .h(rems(QUEUE_ROW_HEIGHT))
                .child(Icon::new(Lucide::CircleDot).xsmall().text_color(if next {
                    cx.theme().primary
                } else {
                    cx.theme().muted_foreground
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(message.preview()),
                )
                .when(attachments > 0, |row| {
                    row.child(Tag::secondary().xsmall().child(match attachments {
                        1 => "1 image".to_owned(),
                        count => format!("{count} images"),
                    }))
                })
                .child(
                    Button::new(SharedString::from(format!("remove-queued-{message_id}")))
                        .xsmall()
                        .ghost()
                        .icon(Lucide::Trash)
                        .tooltip("Remove")
                        .on_click(move |_, _, cx| {
                            let _ = remove.update(cx, |workspace, cx| {
                                workspace.state.update(cx, |state, cx| {
                                    state.remove_queued(id, message_id);
                                    cx.notify();
                                })
                            });
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("edit-queued-{message_id}")))
                        .xsmall()
                        .ghost()
                        .icon(Lucide::Pencil)
                        .tooltip("Edit in composer")
                        .on_click(move |_, window, cx| {
                            let _ = edit.update(cx, |workspace, cx| {
                                workspace.edit_queued(id, message_id, window, cx)
                            });
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("send-queued-{message_id}")))
                        .xsmall()
                        .outline()
                        .label("Send now")
                        .disabled(send_reason.is_some())
                        .tooltip(send_reason.unwrap_or_else(|| {
                            if active {
                                "Stop the running turn and send this next".to_owned()
                            } else {
                                "Send this now".to_owned()
                            }
                        }))
                        .on_click(move |_, _, cx| {
                            let _ = send.update(cx, |workspace, cx| {
                                workspace.send_queued_now(id, message_id, cx)
                            });
                        }),
                ),
        );
    }
    bar = bar.child(rows);
    bar.into_any_element()
}

#[cfg(test)]
mod tests {

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn queued_messages_render_a_bar_with_the_next_row_first(cx: &mut gpui_kit::TestAppContext) {
        use super::{Workspace, queued_messages, queued_messages_height, v_flex};
        use crate::model::{QueuedMessage, Thread, ThreadKind};
        use gpui_kit::{
            AppContext, Context, IntoElement, ParentElement, Render, Styled, VisualTestContext,
            WeakEntity, Window, component::Root,
        };
        use uuid::Uuid;

        fn thread(queued: usize) -> Thread {
            Thread {
                id: Uuid::from_u128(7),
                project_id: Uuid::from_u128(1),
                kind: ThreadKind::Conversation,
                factory: None,
                title: "Queue".into(),
                harness: "omp".into(),
                session_ref: None,
                remote: None,
                execution_started: false,
                model: None,
                effort: None,
                permission_mode: None,
                worktree_id: None,
                pull_request: None,
                draft: String::new(),
                attachments: Vec::new(),
                submission: None,
                queued: (0..queued)
                    .map(|index| QueuedMessage {
                        id: Uuid::from_u128(100 + index as u128),
                        text: format!("Queued prompt {index}"),
                        attachments: Vec::new(),
                    })
                    .collect(),
                archived: false,
                created_at: "2026-09-17T12:00:00Z".into(),
                updated_at: "2026-09-17T12:00:00Z".into(),
            }
        }

        struct QueueHarness;
        impl Render for QueueHarness {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let owner = WeakEntity::<Workspace>::new_invalid();
                v_flex().w(gpui_kit::px(480.)).child(queued_messages(
                    &thread(2),
                    true,
                    None,
                    &owner,
                    cx,
                ))
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
            let owner = WeakEntity::<Workspace>::new_invalid();
            assert_eq!(queued_messages_height(&thread(0), &owner, cx), 0.);
            assert!(
                queued_messages_height(&thread(6), &owner, cx)
                    > queued_messages_height(&thread(1), &owner, cx)
            );
            assert_eq!(
                queued_messages_height(&thread(6), &owner, cx),
                queued_messages_height(&thread(4), &owner, cx),
                "longer queues scroll instead of growing the composer"
            );
        });
        let window = cx.add_window(|window, cx| {
            let content = cx.new(|_| QueueHarness);
            Root::new(content, window, cx)
        });
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();

        let mut visual = VisualTestContext::from_window(window.into(), cx);
        let bar = visual
            .debug_bounds("queued-messages")
            .expect("queue bar should render");
        let next = visual
            .debug_bounds("queued-row-next")
            .expect("the next message renders as the first row");
        let later = visual
            .debug_bounds("queued-row")
            .expect("later messages render as plain rows");
        assert!(next.top() >= bar.top() && next.top() < later.top());
    }
}
