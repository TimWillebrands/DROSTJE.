use super::{
    blocks::block,
    disclosure::{is_expanded, toggle},
    job_cards::{job_card, system_notice},
    job_notice::JobNotice,
    markdown::Markdown,
    tool_runs::{RunCache, tool_run_header, trailing_calls},
    tools::{ToolCall, ToolResult, tool_call_row, tool_card},
    {inset, mono},
};
use crate::ui::{theme, workspace::Workspace};
use crate::{
    app::ConversationSession,
    harness::{ContentBlock, Message},
    model::ThreadId,
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{
        ActiveTheme, IconName, Sizable,
        button::{Button, ButtonVariants},
        h_flex, v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::Value;
use std::sync::Arc;

pub(super) fn prose(content: AnyElement, cx: &App) -> AnyElement {
    let prose = theme::prose(cx);
    div()
        .debug_selector(|| "transcript-prose".into())
        .w_full()
        .min_w_0()
        .when_some(prose.family, |prose, family| prose.font_family(family))
        .when_some(prose.size, |prose, size| prose.text_size(size))
        .when_some(prose.line_height, |prose, height| {
            prose.line_height(relative(height))
        })
        .child(content)
        .into_any_element()
}

pub(super) fn harness_note_hidden(message: &Message) -> bool {
    const HIDDEN: [(&str, &str); 3] = [
        ("<system-reminder>", "</system-reminder>"),
        ("<irc>", "</irc>"),
        ("<critical>", "</critical>"),
    ];
    message.role == "custom"
        && !message.content.is_empty()
        && message.content.iter().all(|block| match block {
            ContentBlock::Text(text) => {
                let text = text.trim();
                HIDDEN
                    .iter()
                    .any(|(open, close)| text.starts_with(open) && text.ends_with(close))
            }
            _ => false,
        })
}

#[expect(
    clippy::too_many_arguments,
    reason = "a transcript row takes its position as well as the row context"
)]
pub(super) fn message(
    message: Arc<Message>,
    index: usize,
    id: ThreadId,
    session: &ConversationSession,
    active: bool,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    runs: &RunCache,
    cx: &App,
) -> AnyElement {
    if message.role == "toolResult" {
        if message
            .tool_call_id
            .as_deref()
            .is_some_and(|call| session.has_tool_call(call))
        {
            return div().into_any_element();
        }
        return inset(tool_card(
            format!("orphan-result:{id}:{}", message.id),
            ToolCall {
                name: "Tool result",
                intent: None,
                input: &Value::Null,
            },
            ToolResult {
                outcome: ToolResult::recorded(message.error, message.exit_code),
                content: &message.content,
                exit_code: message.exit_code,
            },
            owner,
            markdown,
            cx,
        ));
    }
    if harness_note_hidden(&message) {
        return div().into_any_element();
    }
    let run = runs.run(session, index).filter(|run| run.folds());
    let run_key = run.map(|run| format!("toolrun:{id}:{}", session.messages[run.head].id));
    let run_expanded = run_key
        .as_deref()
        .is_some_and(|key| is_expanded(owner, key, cx));
    let run_start = run
        .and_then(|_| trailing_calls(&message.content))
        .map(|(start, _)| start);
    let head = run.is_some_and(|run| run.head == index);
    if run.is_some() && !head && !run_expanded {
        return div().into_any_element();
    }
    let user_message = message.role == "user";
    let raw_key = format!("raw:{id}:{}", message.id);
    let raw_expanded = message.raw.is_some() && is_expanded(owner, &raw_key, cx);
    let group: SharedString = format!("message-group-{id}-{}", message.id).into();
    let copy_message = message.clone();
    let mut actions = h_flex()
        .absolute()
        .top_0()
        .right_0()
        .gap_1()
        .rounded(cx.theme().radius)
        .bg(cx.theme().background)
        .invisible()
        .group_hover(group.clone(), |style| style.visible())
        .child(
            Button::new(SharedString::from(format!(
                "copy-message-{id}-{}",
                message.id
            )))
            .xsmall()
            .ghost()
            .cursor_default()
            .icon(IconName::Copy)
            .tooltip("Copy message")
            .on_click(move |_, _, cx| {
                let copy = copy_message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.as_str()),
                        ContentBlock::PeerMessage(peer) => Some(peer.body.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                cx.write_to_clipboard(ClipboardItem::new_string(copy));
            }),
        );
    if message.raw.is_some() {
        actions = actions.child(
            Button::new(SharedString::from(format!(
                "toggle-raw-{id}-{}",
                message.id
            )))
            .xsmall()
            .ghost()
            .cursor_default()
            .icon(Lucide::Braces)
            .tooltip(if raw_expanded {
                "Hide diagnostic payload"
            } else {
                "Show diagnostic payload"
            })
            .on_click(toggle(owner, Some(id), raw_key)),
        );
    }
    let mut body = v_flex().w_full().min_w_0().gap_2();
    let mut images = h_flex().flex_wrap().gap_2();
    let mut has_images = false;
    let mut folded = v_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .pl_3()
        .border_l_1()
        .border_color(cx.theme().border);
    for (part, item) in message.content.iter().enumerate() {
        let identity = format!("{id}:{}:{part}", message.id);
        if let Some(start) = run_start.filter(|start| part >= *start) {
            if part == start
                && head
                && let (Some(run), Some(key)) = (run, run_key.clone())
            {
                body = body.child(tool_run_header(
                    id,
                    key,
                    run,
                    run_expanded,
                    session,
                    active,
                    owner,
                    cx,
                ));
            }
            if run_expanded {
                folded = folded.child(match item {
                    ContentBlock::ToolCall {
                        id: call_id,
                        name,
                        intent,
                        input,
                    } => tool_call_row(
                        id,
                        call_id,
                        ToolCall {
                            name,
                            intent: intent.as_deref(),
                            input,
                        },
                        session,
                        active,
                        owner,
                        markdown,
                        cx,
                    ),
                    _ => block(item, identity, owner, markdown, cx),
                });
            }
            continue;
        }
        match item {
            ContentBlock::ToolCall {
                id: call_id,
                name,
                intent,
                input,
            } => {
                body = body.child(tool_call_row(
                    id,
                    call_id,
                    ToolCall {
                        name,
                        intent: intent.as_deref(),
                        input,
                    },
                    session,
                    active,
                    owner,
                    markdown,
                    cx,
                ));
            }
            ContentBlock::Image(_) if user_message => {
                has_images = true;
                images = images.child(block(item, identity, owner, markdown, cx));
            }
            ContentBlock::Text(text) if message.role == "custom" => match JobNotice::parse(text) {
                Some(notice) => {
                    for job in &notice.jobs {
                        body = body.child(job_card(
                            format!("job:{id}:{}:{}", message.id, job.id),
                            job,
                            owner,
                            markdown,
                            cx,
                        ));
                    }
                }
                None => {
                    body = body.child(match super::job_notice::system_notice_body(text) {
                        Some(text) => system_notice(identity, text, owner, markdown, cx),
                        None => block(item, identity, owner, markdown, cx),
                    });
                }
            },
            ContentBlock::Text(_) if message.role == "assistant" => {
                body = body.child(prose(block(item, identity, owner, markdown, cx), cx));
            }
            _ => body = body.child(block(item, identity, owner, markdown, cx)),
        }
    }
    if has_images {
        body = body.child(images);
    }
    if run_expanded {
        body = body.child(if head { folded.pb_1() } else { folded.py_1() });
    }
    if let Some(raw) = message.raw.as_ref().filter(|_| raw_expanded) {
        body = body.child(mono(cx).child(raw.to_string()));
    }
    let body = if user_message {
        body.px_3()
            .py_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .bg(cx.theme().background)
    } else if !matches!(message.role.as_str(), "assistant") {
        body.text_color(cx.theme().muted_foreground)
    } else {
        body
    };
    inset(
        v_flex()
            .w_full()
            .min_w_0()
            .id(SharedString::from(format!("message-{id}-{}", message.id)))
            .group(group)
            .relative()
            .map(|row| match (user_message, run_expanded, head) {
                (true, ..) => row.py_2(),
                (false, true, true) => row.pt_1(),
                (false, true, false) => row,
                (false, false, _) => row.py_1(),
            })
            .child(body)
            .child(actions),
    )
}

#[cfg(test)]
mod tests {
    use super::{ContentBlock, Message, harness_note_hidden};

    fn custom(role: &str, text: &str) -> Message {
        Message {
            id: "m".into(),
            role: role.into(),
            content: vec![ContentBlock::Text(text.into())],
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        }
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn agent_prose_fills_its_column_in_every_theme(cx: &mut gpui_kit::TestAppContext) {
        use super::prose;
        use gpui_kit::{
            AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render, Styled,
            VisualTestContext, Window, component::Root, div, px,
        };
        use serde_json::json;
        use std::collections::BTreeMap;

        struct Reply;
        impl Render for Reply {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .debug_selector(|| "reply-pane".into())
                    .size_full()
                    .child(prose(div().w_full().h(px(10.)).into_any_element(), cx))
            }
        }

        let theme = |cx: &mut gpui_kit::TestAppContext, light: &str| {
            let settings = BTreeMap::from([
                ("theme_mode".to_owned(), json!("light")),
                ("theme_light".to_owned(), json!(light)),
            ]);
            cx.update(|cx| crate::ui::theme::initialize(Some(&settings), None, cx));
        };
        cx.update(gpui_kit::init);
        theme(cx, "Ink Light");
        let window = cx.add_window(|window, cx| Root::new(cx.new(|_| Reply), window, cx));
        let width = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
            let mut visual = VisualTestContext::from_window(window.into(), cx);
            let prose = visual
                .debug_bounds("transcript-prose")
                .expect("prose renders");
            let pane = visual.debug_bounds("reply-pane").unwrap().size.width;
            (prose.size.width, pane)
        };

        let (ink, pane) = width(cx);
        assert_eq!(ink, pane, "Ink prose fills its column");

        theme(cx, "Delft Light");
        let (delft, pane) = width(cx);
        assert_eq!(delft, pane, "Delft prose fills its column");
    }

    #[test]
    fn harness_notes_hide_only_when_injected_by_the_harness() {
        assert!(harness_note_hidden(&custom(
            "custom",
            "<system-reminder>\n2 todo items still open.\n</system-reminder>"
        )));
        assert!(harness_note_hidden(&custom(
            "custom",
            "<irc>\nIncoming IRC message from agent `Review`:\n</irc>\n"
        )));
        assert!(harness_note_hidden(&custom(
            "custom",
            "<critical>\nPlan mode active.\n</critical>"
        )));
        assert!(!harness_note_hidden(&custom(
            "user",
            "<system-reminder>look</system-reminder>"
        )));
        assert!(!harness_note_hidden(&custom(
            "custom",
            "<system-notice>\nBackground job bg_1 has completed.\n</system-notice>"
        )));
        assert!(!harness_note_hidden(&custom(
            "custom",
            "<irc>partial</irc> and a trailing remark"
        )));
        let mut empty = custom("custom", "");
        empty.content.clear();
        assert!(!harness_note_hidden(&empty));
    }
}
