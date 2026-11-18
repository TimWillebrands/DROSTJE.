use std::collections::HashSet;

use crate::app::{CallRef, ConversationSession, trailing_calls};
use crate::harness::{ContentBlock, Message};
use crate::phone::view::{
    ItemKind, Prompting, ToolCallView, ToolRunView, Transcript, TranscriptItem,
};
use crate::transcript::{
    RunPart, ToolOutcome, ToolRun, jobs::system_notice_body, summary_line, tool_summary,
};
use uuid::Uuid;

pub const TAIL_ITEMS: usize = 40;
pub const ITEM_CHARS: usize = 4000;
pub const RUN_CALLS: usize = 20;
pub const CALL_CHARS: usize = 160;

fn clip_to(text: &str, limit: usize) -> (String, bool) {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(limit) {
        Some((end, _)) => (trimmed[..end].trim_end().to_owned(), true),
        None => (trimmed.to_owned(), false),
    }
}

fn item(id: String, kind: ItemKind, text: &str) -> Option<TranscriptItem> {
    let (text, clipped) = clip_to(text, ITEM_CHARS);
    (!text.is_empty()).then_some(TranscriptItem {
        id,
        kind,
        text,
        clipped,
        run: None,
    })
}

fn blocks_text(blocks: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(text) => {
                parts.push(system_notice_body(text).unwrap_or(text).to_owned())
            }
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

fn call_view(call: CallRef<'_>, outcome: ToolOutcome) -> ToolCallView {
    let summary = summary_line(&tool_summary(call.name, call.input).body);
    ToolCallView {
        name: call.name.to_owned(),
        caption: call.intent.map(str::to_owned),
        summary: clip_to(&summary, CALL_CHARS).0,
        outcome,
    }
}

fn lone_call(id: &str, view: ToolCallView) -> TranscriptItem {
    TranscriptItem::of_run(
        format!("tool:{id}"),
        ToolRunView {
            tally: [view.outcome].into_iter().collect(),
            calls: vec![view],
        },
    )
}

fn run_item(session: &ConversationSession, run: ToolRun, active: bool) -> Option<TranscriptItem> {
    let calls: Vec<CallRef<'_>> = session.run_calls(run).collect();
    let first = calls.first()?.id;
    let newest = &calls[calls.len().saturating_sub(RUN_CALLS)..];
    Some(TranscriptItem::of_run(
        format!("tool:{first}"),
        ToolRunView {
            tally: session.run_tally(run, active),
            calls: newest
                .iter()
                .map(|call| call_view(*call, session.call_outcome(call.id, active)))
                .collect(),
        },
    ))
}

fn message_items(
    session: &ConversationSession,
    message: &Message,
    blocks: &[ContentBlock],
    active: bool,
) -> Vec<TranscriptItem> {
    match message.role.as_str() {
        "toolResult" => {
            let outcome = ToolOutcome::recorded(message.error, message.exit_code);
            let summary = clip_to(&summary_line(&blocks_text(blocks)), CALL_CHARS).0;
            vec![lone_call(
                &message.id,
                ToolCallView {
                    name: "Tool result".into(),
                    caption: None,
                    summary,
                    outcome,
                },
            )]
        }
        "user" => item(message.id.clone(), ItemKind::User, &blocks_text(blocks))
            .into_iter()
            .collect(),
        "assistant" => {
            let mut items: Vec<TranscriptItem> = blocks
                .iter()
                .enumerate()
                .filter_map(|(index, block)| {
                    let id = format!("{}#{index}", message.id);
                    match block {
                        ContentBlock::Text(text) => item(id, ItemKind::Assistant, text),
                        ContentBlock::Thinking(text) => item(id, ItemKind::Thinking, text),
                        ContentBlock::ToolCall {
                            id: call,
                            name,
                            intent,
                            input,
                        } => {
                            let call = session.call(call, name, intent.as_deref(), input);
                            let outcome = session.call_outcome(call.id, active);
                            Some(lone_call(call.id, call_view(call, outcome)))
                        }
                        ContentBlock::Image(_) => item(id, ItemKind::Assistant, "[image]"),
                        ContentBlock::ImageError(error) => item(id, ItemKind::Error, error),
                        ContentBlock::PeerMessage(peer) => item(
                            id,
                            ItemKind::Notice,
                            &format!("{}: {}", peer.sender_name, peer.body),
                        ),
                    }
                })
                .collect();
            if message.error && message.content.is_empty() {
                items.extend(item(message.id.clone(), ItemKind::Error, "The turn failed"));
            }
            items
        }
        _ => item(
            message.id.clone(),
            if message.error {
                ItemKind::Error
            } else {
                ItemKind::Notice
            },
            &blocks_text(blocks),
        )
        .into_iter()
        .collect(),
    }
}

#[derive(Default)]
struct Tail {
    newest_first: Vec<TranscriptItem>,
    earlier: bool,
}

impl Tail {
    fn push(&mut self, item: TranscriptItem) {
        if self.newest_first.len() == TAIL_ITEMS {
            self.earlier = true;
        } else {
            self.newest_first.push(item);
        }
    }

    fn extend(&mut self, oldest_first: impl DoubleEndedIterator<Item = TranscriptItem>) {
        for item in oldest_first.rev() {
            self.push(item);
        }
    }
}

pub fn transcript_tail(thread: Uuid, session: &ConversationSession, active: bool) -> Transcript {
    let mut tail = Tail::default();
    let mut index = session.messages.len();
    while index > 0 && !tail.earlier {
        index -= 1;
        let message = &session.messages[index];
        match session.run_part(index) {
            RunPart::Transparent => {}
            RunPart::Break => {
                tail.extend(message_items(session, message, &message.content, active).into_iter())
            }
            RunPart::Calls(_) | RunPart::Opens(_) => {
                let Some(run) = session.tool_run(index) else {
                    continue;
                };
                tail.extend(run_item(session, run, active).into_iter());
                index = run.head;
                let head = &session.messages[index];
                let start = trailing_calls(&head.content).map_or(0, |(start, _)| start);
                tail.extend(
                    message_items(session, head, &head.content[..start], active).into_iter(),
                );
            }
        }
    }
    let Tail {
        newest_first: mut items,
        earlier,
    } = tail;
    items.reverse();
    let mut ids = HashSet::new();
    items.retain(|item| ids.insert(item.id.clone()));
    for request in session.requests.values() {
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
    if let Some(error) = session.error.as_deref() {
        items.extend(item("session-error".into(), ItemKind::Error, error));
    }
    Transcript {
        thread,
        earlier,
        items,
        prompting: Prompting::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{ToolActivity, UserRequest, UserRequestKind};
    use crate::phone::view::ToolState;
    use serde_json::json;
    use std::sync::Arc;

    fn message(id: &str, role: &str, content: Vec<ContentBlock>) -> Message {
        Message {
            id: id.into(),
            role: role.into(),
            content,
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        }
    }

    fn text(text: &str) -> ContentBlock {
        ContentBlock::Text(text.into())
    }

    fn call(id: &str, name: &str, intent: Option<&str>) -> ContentBlock {
        ContentBlock::ToolCall {
            id: id.into(),
            name: name.into(),
            intent: intent.map(str::to_owned),
            input: json!({"command": format!("run {id}")}),
        }
    }

    fn result(call: &str, exit_code: i64) -> Message {
        Message {
            tool_call_id: Some(call.into()),
            exit_code: Some(exit_code),
            ..message(&format!("r-{call}"), "toolResult", vec![text("ok")])
        }
    }

    fn session(messages: Vec<Message>) -> ConversationSession {
        let mut session = ConversationSession::new(None);
        for message in messages {
            session.message(message);
        }
        session
    }

    fn live(session: &mut ConversationSession, id: &str, complete: bool, error: bool) {
        session.tools.insert(
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
        );
    }

    fn run(item: &TranscriptItem) -> &ToolRunView {
        item.run.as_ref().expect("a tool item carries its run")
    }

    fn ids(transcript: &Transcript) -> Vec<&str> {
        transcript
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect()
    }

    #[test]
    fn user_text_answers_and_tool_calls_become_items_in_order() {
        let session = session(vec![
            message("u1", "user", vec![text("Fix the bug")]),
            message(
                "a1",
                "assistant",
                vec![
                    ContentBlock::Thinking("Look at login".into()),
                    text("I will run the tests"),
                    call("c1", "bash", Some("Run the tests")),
                ],
            ),
            result("c1", 0),
        ]);
        let tail = transcript_tail(Uuid::nil(), &session, false);
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
        let calls = &run(&tail.items[3]).calls;
        assert_eq!(calls[0].label(), "Run the tests");
        assert_eq!(calls[0].summary, "run c1");
        assert!(!tail.earlier);
    }

    #[test]
    fn a_reopened_thread_shows_recorded_tool_results_instead_of_spinners() {
        let session = session(vec![
            message("a", "assistant", vec![call("c1", "bash", None)]),
            result("c1", 0),
            message(
                "b",
                "assistant",
                vec![text("Next"), call("c2", "bash", None)],
            ),
            result("c2", 1),
            message(
                "c",
                "assistant",
                vec![text("Last"), call("c3", "bash", None)],
            ),
        ]);
        assert!(
            session.tools.is_empty(),
            "loaded history has no live activity"
        );
        let tail = transcript_tail(Uuid::nil(), &session, false);
        let outcomes: Vec<_> = tail
            .items
            .iter()
            .filter_map(|item| item.run.as_ref())
            .map(|run| run.calls[0].outcome)
            .collect();
        assert_eq!(
            outcomes,
            [
                ToolOutcome::Succeeded,
                ToolOutcome::Failed,
                ToolOutcome::Unknown
            ]
        );
    }

    #[test]
    fn harness_notes_stay_off_the_phone_and_system_notices_lose_their_wrapper() {
        let session = session(vec![
            message(
                "n",
                "custom",
                vec![text(
                    "<system-reminder>2 todo items still open.</system-reminder>",
                )],
            ),
            message(
                "s",
                "custom",
                vec![text(
                    "<system-notice>\nBackground job bg_1 has completed.\n</system-notice>",
                )],
            ),
        ]);
        let tail = transcript_tail(Uuid::nil(), &session, false);
        assert_eq!(tail.items.len(), 1, "{:?}", tail.items);
        assert_eq!(tail.items[0].kind, ItemKind::Notice);
        assert_eq!(tail.items[0].text, "Background job bg_1 has completed.");
    }

    #[test]
    fn prose_and_own_card_calls_split_runs_like_on_the_desktop() {
        let session = session(vec![
            message("u", "user", vec![text("Look around")]),
            message(
                "a1",
                "assistant",
                vec![text("Reading."), call("c1", "read", Some("Read main"))],
            ),
            result("c1", 0),
            message(
                "hint",
                "custom",
                vec![text("<system-reminder>todo</system-reminder>")],
            ),
            message(
                "a2",
                "assistant",
                vec![call("c2", "bash", None), call("c3", "todo", None)],
            ),
            message("a3", "assistant", vec![text("Done.")]),
        ]);
        let tail = transcript_tail(Uuid::nil(), &session, true);
        assert_eq!(
            ids(&tail),
            ["u", "a1#0", "tool:c1", "tool:c2", "tool:c3", "a3#0"],
            "the todo call keeps its own row, so c1 stays a run of one"
        );
        assert_eq!(run(&tail.items[2]).tally.calls, 1);
    }

    #[test]
    fn consecutive_calls_across_messages_fold_into_one_run() {
        let session = session(vec![
            message("a1", "assistant", vec![call("c1", "read", None)]),
            result("c1", 0),
            message("a2", "assistant", vec![call("c2", "bash", None)]),
            result("c2", 2),
            message("a3", "assistant", vec![call("c3", "bash", None)]),
        ]);
        let tail = transcript_tail(Uuid::nil(), &session, true);
        assert_eq!(ids(&tail), ["tool:c1"]);
        let folded = run(&tail.items[0]);
        assert!(folded.tally.folds());
        assert_eq!(
            (
                folded.tally.calls,
                folded.tally.failed,
                folded.tally.running
            ),
            (3, 1, 1)
        );
        let names: Vec<&str> = folded.calls.iter().map(|call| call.name.as_str()).collect();
        assert_eq!(names, ["read", "bash", "bash"]);
    }

    #[test]
    fn a_long_run_is_one_item_carrying_only_its_newest_calls() {
        let mut messages = vec![message("u", "user", vec![text("Go")])];
        for n in 0..(RUN_CALLS + 5) {
            messages.push(message(
                &format!("a{n}"),
                "assistant",
                vec![call(&format!("c{n}"), "bash", None)],
            ));
            messages.push(result(&format!("c{n}"), 0));
        }
        let tail = transcript_tail(Uuid::nil(), &session(messages), false);
        assert_eq!(ids(&tail), ["u", "tool:c0"]);
        let folded = run(&tail.items[1]);
        assert_eq!(folded.tally.calls, RUN_CALLS + 5);
        assert_eq!(folded.calls.len(), RUN_CALLS);
        assert_eq!(folded.calls[0].summary, "run c5");
    }

    #[test]
    fn running_failed_and_unknown_calls_are_told_apart() {
        let mut session = session(vec![message(
            "a",
            "assistant",
            vec![
                call("x", "edit", None),
                call("y", "edit", None),
                call("z", "edit", None),
            ],
        )]);
        live(&mut session, "x", false, false);
        live(&mut session, "y", true, true);
        let outcomes = |active| {
            let tail = transcript_tail(Uuid::nil(), &session, active);
            run(&tail.items[0])
                .calls
                .iter()
                .map(|call| call.outcome)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            outcomes(true),
            [
                ToolOutcome::Running,
                ToolOutcome::Failed,
                ToolOutcome::Running
            ]
        );
        assert_eq!(
            outcomes(false),
            [
                ToolOutcome::Unknown,
                ToolOutcome::Failed,
                ToolOutcome::Unknown
            ]
        );
    }

    #[test]
    fn only_the_newest_items_are_kept_and_long_text_is_clipped() {
        let mut messages = Vec::new();
        for n in 0..60 {
            messages.push(message(
                &format!("m{n}"),
                "assistant",
                vec![text(&format!("answer {n}"))],
            ));
        }
        messages.push(message(
            "long",
            "user",
            vec![text(&"é".repeat(ITEM_CHARS + 10))],
        ));
        let tail = transcript_tail(Uuid::nil(), &session(messages), false);
        assert_eq!(tail.items.len(), TAIL_ITEMS);
        assert!(tail.earlier);
        assert_eq!(tail.items[0].id, "m21#0");
        let last = tail.items.last().unwrap();
        assert!(last.clipped);
        assert_eq!(last.text.chars().count(), ITEM_CHARS);
    }

    #[test]
    fn a_tail_that_fits_exactly_has_nothing_earlier() {
        let messages = [message("blank", "assistant", vec![text("  ")])]
            .into_iter()
            .chain((0..TAIL_ITEMS).map(|n| message(&format!("m{n}"), "assistant", vec![text("a")])))
            .collect();
        let tail = transcript_tail(Uuid::nil(), &session(messages), false);
        assert_eq!(tail.items.len(), TAIL_ITEMS);
        assert!(!tail.earlier);
    }

    #[test]
    fn open_questions_and_session_errors_follow_the_messages() {
        let mut session = session(Vec::new());
        session.requests.insert(
            "q".into(),
            UserRequest {
                id: "q".into(),
                kind: UserRequestKind::Confirm,
                title: "Allow running npm install?".into(),
                options: Vec::new(),
                text: String::new(),
                placeholder: None,
                deadline: None,
            },
        );
        session.error = Some("Claude Code exited".into());
        let tail = transcript_tail(Uuid::nil(), &session, false);
        assert_eq!(
            tail.items[0].text,
            "Waiting on the desktop: Allow running npm install?"
        );
        assert_eq!(tail.items[1].kind, ItemKind::Error);
    }

    #[test]
    fn empty_text_blocks_are_left_out_and_orphan_results_keep_their_outcome() {
        let orphan = Message {
            tool_call_id: Some("gone".into()),
            exit_code: Some(3),
            ..message("r", "toolResult", vec![text("permission denied\nmore")])
        };
        let session = session(vec![
            message("a", "assistant", vec![text("   ")]),
            message("n", "custom", vec![text("Compacted")]),
            orphan,
        ]);
        let tail = transcript_tail(Uuid::nil(), &session, false);
        assert_eq!(ids(&tail), ["n", "tool:r"]);
        assert_eq!(tail.items[0].kind, ItemKind::Notice);
        let orphan = run(&tail.items[1]);
        assert_eq!(orphan.calls[0].outcome, ToolOutcome::Failed);
        assert_eq!(orphan.calls[0].summary, "permission denied …");
    }
}
