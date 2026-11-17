use std::{collections::BTreeMap, sync::Arc};

use crate::harness::{ContentBlock, Message, ToolActivity, UserRequest};
use crate::phone::view::{ItemKind, ToolState, Transcript, TranscriptItem};
use uuid::Uuid;

pub const TAIL_ITEMS: usize = 40;
pub const ITEM_CHARS: usize = 4000;

fn clip(text: &str) -> (String, bool) {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(ITEM_CHARS) {
        Some((end, _)) => (trimmed[..end].trim_end().to_owned(), true),
        None => (trimmed.to_owned(), false),
    }
}

fn item(id: String, kind: ItemKind, text: &str) -> Option<TranscriptItem> {
    let (text, clipped) = clip(text);
    if text.is_empty() && !matches!(kind, ItemKind::Tool { .. }) {
        return None;
    }
    Some(TranscriptItem {
        id,
        kind,
        text,
        clipped,
    })
}

fn tool_state(tool: Option<&Arc<ToolActivity>>) -> ToolState {
    match tool {
        Some(tool) if tool.error => ToolState::Failed,
        Some(tool) if tool.complete => ToolState::Done,
        Some(_) | None => ToolState::Running,
    }
}

fn blocks_text(blocks: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(text) => parts.push(text.clone()),
            ContentBlock::Image(_) => parts.push("[image]".into()),
            ContentBlock::ImageError(error) => parts.push(format!("[image unavailable: {error}]")),
            ContentBlock::PeerMessage(peer) => {
                parts.push(format!("{}: {}", peer.sender_name, peer.body))
            }
            ContentBlock::Thinking(_) | ContentBlock::ToolCall { .. } => {}
        }
    }
    parts.join("\n\n")
}

fn message_items(
    message: &Message,
    tools: &BTreeMap<String, Arc<ToolActivity>>,
    out: &mut Vec<TranscriptItem>,
) {
    match message.role.as_str() {
        "toolResult" => {}
        "user" => out.extend(item(
            message.id.clone(),
            ItemKind::User,
            &blocks_text(&message.content),
        )),
        "assistant" => {
            for (index, block) in message.content.iter().enumerate() {
                let id = format!("{}#{index}", message.id);
                let next = match block {
                    ContentBlock::Text(text) => item(id, ItemKind::Assistant, text),
                    ContentBlock::Thinking(text) => item(id, ItemKind::Thinking, text),
                    ContentBlock::ToolCall {
                        id: call,
                        name,
                        intent,
                        ..
                    } => item(
                        format!("tool:{call}"),
                        ItemKind::Tool {
                            name: name.clone(),
                            state: tool_state(tools.get(call)),
                        },
                        intent.as_deref().unwrap_or_default(),
                    ),
                    ContentBlock::Image(_) => item(id, ItemKind::Assistant, "[image]"),
                    ContentBlock::ImageError(error) => item(id, ItemKind::Error, error),
                    ContentBlock::PeerMessage(peer) => item(
                        id,
                        ItemKind::Notice,
                        &format!("{}: {}", peer.sender_name, peer.body),
                    ),
                };
                out.extend(next);
            }
            if message.error && message.content.is_empty() {
                out.extend(item(message.id.clone(), ItemKind::Error, "The turn failed"));
            }
        }
        _ => out.extend(item(
            message.id.clone(),
            if message.error {
                ItemKind::Error
            } else {
                ItemKind::Notice
            },
            &blocks_text(&message.content),
        )),
    }
}

pub fn transcript_tail<'a>(
    thread: Uuid,
    messages: &[Arc<Message>],
    tools: &BTreeMap<String, Arc<ToolActivity>>,
    requests: impl IntoIterator<Item = &'a UserRequest>,
    error: Option<&str>,
) -> Transcript {
    let mut items = Vec::new();
    let mut earlier = false;
    for message in messages.iter().rev() {
        let mut part = Vec::new();
        message_items(message, tools, &mut part);
        if items.len() + part.len() > TAIL_ITEMS {
            earlier = true;
            let room = TAIL_ITEMS - items.len();
            items.extend(part.into_iter().rev().take(room));
            break;
        }
        items.extend(part.into_iter().rev());
    }
    items.reverse();
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    items.retain(|item| ids.insert(item.id.clone()));
    for request in requests {
        let text = [request.title.as_str(), request.text.as_str()]
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let text = if text.is_empty() {
            "The agent is waiting for an answer on the desktop".to_owned()
        } else {
            text
        };
        items.extend(item(
            format!("request:{}", request.id),
            ItemKind::Notice,
            &format!("Waiting on the desktop: {text}"),
        ));
    }
    if let Some(error) = error {
        items.extend(item("session-error".into(), ItemKind::Error, error));
    }
    Transcript {
        thread,
        earlier,
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::UserRequestKind;
    use serde_json::json;

    fn message(id: &str, role: &str, content: Vec<ContentBlock>) -> Arc<Message> {
        Arc::new(Message {
            id: id.into(),
            role: role.into(),
            content,
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        })
    }

    fn tool(id: &str, complete: bool, error: bool) -> (String, Arc<ToolActivity>) {
        (
            id.into(),
            Arc::new(ToolActivity {
                id: id.into(),
                name: "bash".into(),
                intent: None,
                input: json!({}),
                result: Vec::new(),
                error,
                exit_code: None,
                complete,
                raw: None,
            }),
        )
    }

    #[test]
    fn user_text_answers_and_tool_calls_become_items_in_order() {
        let messages = vec![
            message("u1", "user", vec![ContentBlock::Text("Fix the bug".into())]),
            message(
                "a1",
                "assistant",
                vec![
                    ContentBlock::Thinking("Look at login".into()),
                    ContentBlock::Text("I will run the tests".into()),
                    ContentBlock::ToolCall {
                        id: "c1".into(),
                        name: "bash".into(),
                        intent: Some("Run the tests".into()),
                        input: json!({}),
                    },
                ],
            ),
            message("r1", "toolResult", vec![ContentBlock::Text("ok".into())]),
        ];
        let tools = BTreeMap::from([tool("c1", true, false)]);
        let tail = transcript_tail(Uuid::nil(), &messages, &tools, [], None);
        let kinds: Vec<_> = tail.items.iter().map(|item| item.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                ItemKind::User,
                ItemKind::Thinking,
                ItemKind::Assistant,
                ItemKind::Tool {
                    name: "bash".into(),
                    state: ToolState::Done
                },
            ]
        );
        assert_eq!(tail.items[3].id, "tool:c1");
        assert_eq!(tail.items[3].text, "Run the tests");
        assert!(!tail.earlier);
    }

    #[test]
    fn only_the_newest_items_are_kept_and_long_text_is_clipped() {
        let mut messages = Vec::new();
        for n in 0..60 {
            messages.push(message(
                &format!("m{n}"),
                "assistant",
                vec![ContentBlock::Text(format!("answer {n}"))],
            ));
        }
        messages.push(message(
            "long",
            "user",
            vec![ContentBlock::Text("é".repeat(ITEM_CHARS + 10))],
        ));
        let tail = transcript_tail(Uuid::nil(), &messages, &BTreeMap::new(), [], None);
        assert_eq!(tail.items.len(), TAIL_ITEMS);
        assert!(tail.earlier);
        assert_eq!(tail.items[0].id, "m21#0");
        let last = tail.items.last().unwrap();
        assert!(last.clipped);
        assert_eq!(last.text.chars().count(), ITEM_CHARS);
    }

    #[test]
    fn running_and_failed_tools_are_told_apart() {
        let call = |id: &str| ContentBlock::ToolCall {
            id: id.into(),
            name: "edit".into(),
            intent: None,
            input: json!({}),
        };
        let messages = vec![message(
            "a",
            "assistant",
            vec![call("x"), call("y"), call("z")],
        )];
        let tools = BTreeMap::from([tool("x", false, false), tool("y", true, true)]);
        let tail = transcript_tail(Uuid::nil(), &messages, &tools, [], None);
        let states: Vec<_> = tail
            .items
            .iter()
            .map(|item| match &item.kind {
                ItemKind::Tool { state, .. } => *state,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            states,
            vec![ToolState::Running, ToolState::Failed, ToolState::Running]
        );
    }

    #[test]
    fn open_questions_and_session_errors_follow_the_messages() {
        let request = UserRequest {
            id: "q".into(),
            kind: UserRequestKind::Confirm,
            title: "Allow running npm install?".into(),
            options: Vec::new(),
            text: String::new(),
            placeholder: None,
            deadline: None,
        };
        let tail = transcript_tail(
            Uuid::nil(),
            &[],
            &BTreeMap::new(),
            [&request],
            Some("Claude Code exited"),
        );
        assert_eq!(
            tail.items[0].text,
            "Waiting on the desktop: Allow running npm install?"
        );
        assert_eq!(tail.items[1].kind, ItemKind::Error);
    }

    #[test]
    fn empty_text_blocks_and_notices_are_left_out() {
        let messages = vec![
            message("a", "assistant", vec![ContentBlock::Text("   ".into())]),
            message("n", "custom", vec![ContentBlock::Text("Compacted".into())]),
        ];
        let tail = transcript_tail(Uuid::nil(), &messages, &BTreeMap::new(), [], None);
        assert_eq!(tail.items.len(), 1);
        assert_eq!(tail.items[0].kind, ItemKind::Notice);
    }
}
