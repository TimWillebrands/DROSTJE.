use super::ConversationSession;
use crate::harness::{ContentBlock, Message};
use crate::transcript::{
    RunPart, RunTally, ToolOutcome, ToolRun, has_own_card, is_harness_note, tool_run,
};
use serde_json::Value;

pub fn harness_note_hidden(message: &Message) -> bool {
    message.role == "custom"
        && !message.content.is_empty()
        && message
            .content
            .iter()
            .all(|block| matches!(block, ContentBlock::Text(text) if is_harness_note(text)))
}

pub fn trailing_calls(content: &[ContentBlock]) -> Option<(usize, usize)> {
    let start = content
        .iter()
        .rposition(|block| match block {
            ContentBlock::ToolCall { name, .. } => has_own_card(name),
            ContentBlock::Thinking(_) => false,
            ContentBlock::Text(text) => !text.trim().is_empty(),
            ContentBlock::PeerMessage(_) | ContentBlock::Image(_) | ContentBlock::ImageError(_) => {
                true
            }
        })
        .map_or(0, |last| last + 1);
    let calls = content[start..]
        .iter()
        .filter(|block| matches!(block, ContentBlock::ToolCall { .. }))
        .count();
    (calls > 0).then_some((start, calls))
}

#[derive(Clone, Copy, Debug)]
pub struct CallRef<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub intent: Option<&'a str>,
    pub input: &'a Value,
}

impl ConversationSession {
    pub fn run_part(&self, index: usize) -> RunPart {
        let message = &self.messages[index];
        if message.role == "toolResult" {
            return if message
                .tool_call_id
                .as_deref()
                .is_some_and(|call| self.has_tool_call(call))
            {
                RunPart::Transparent
            } else {
                RunPart::Break
            };
        }
        if harness_note_hidden(message) {
            return RunPart::Transparent;
        }
        if message.role != "assistant" {
            return RunPart::Break;
        }
        match trailing_calls(&message.content) {
            Some((0, calls)) => RunPart::Calls(calls),
            Some((_, calls)) => RunPart::Opens(calls),
            None => RunPart::Break,
        }
    }

    pub fn tool_run(&self, index: usize) -> Option<ToolRun> {
        tool_run(|ix| self.run_part(ix), self.messages.len(), index)
    }

    pub fn call_outcome(&self, call_id: &str, active: bool) -> ToolOutcome {
        if let Some(tool) = self.tools.get(call_id) {
            return match ToolOutcome::recorded(tool.error, tool.exit_code) {
                ToolOutcome::Succeeded if !tool.complete => ToolOutcome::pending(active),
                outcome => outcome,
            };
        }
        self.tool_result(call_id)
            .map_or(ToolOutcome::pending(active), |result| {
                ToolOutcome::recorded(result.error, result.exit_code)
            })
    }

    pub fn call<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        intent: Option<&'a str>,
        input: &'a Value,
    ) -> CallRef<'a> {
        let intent = intent.or_else(|| self.tools.get(id).and_then(|tool| tool.intent.as_deref()));
        CallRef {
            id,
            name,
            intent,
            input,
        }
    }

    pub fn run_calls(&self, run: ToolRun) -> impl Iterator<Item = CallRef<'_>> {
        self.messages[run.head..run.end]
            .iter()
            .filter_map(|message| {
                let (start, _) = trailing_calls(&message.content)?;
                Some(&message.content[start..])
            })
            .flatten()
            .filter_map(|block| match block {
                ContentBlock::ToolCall {
                    id,
                    name,
                    intent,
                    input,
                } => Some(self.call(id, name, intent.as_deref(), input)),
                _ => None,
            })
    }

    pub fn run_tally(&self, run: ToolRun, active: bool) -> RunTally {
        self.run_calls(run)
            .map(|call| self.call_outcome(call.id, active))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{harness_note_hidden, trailing_calls};
    use crate::app::ConversationSession;
    use crate::harness::{ContentBlock, Message, ToolActivity};
    use crate::transcript::{RunPart, ToolOutcome, ToolRun};
    use serde_json::{Value, json};
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

    fn call(id: &str) -> ContentBlock {
        ContentBlock::ToolCall {
            id: id.into(),
            name: "bash".into(),
            intent: None,
            input: Value::Null,
        }
    }

    fn result(call: &str, error: bool, exit_code: Option<i64>) -> Message {
        Message {
            tool_call_id: Some(call.into()),
            error,
            exit_code,
            ..message(&format!("r-{call}"), "toolResult", Vec::new())
        }
    }

    fn session(messages: Vec<Message>) -> ConversationSession {
        let mut session = ConversationSession::new(None);
        for message in messages {
            session.message(message);
        }
        session
    }

    fn activity(id: &str, complete: bool, error: bool, exit_code: Option<i64>) -> ToolActivity {
        ToolActivity {
            id: id.into(),
            name: "bash".into(),
            intent: Some(format!("live {id}")),
            input: json!({}),
            result: Vec::new(),
            error,
            exit_code,
            complete,
            raw: None,
        }
    }

    #[test]
    fn trailing_calls_skip_reasoning_but_not_prose_todo_or_artifacts() {
        let text = |text: &str| ContentBlock::Text(text.into());
        let named = |name: &str| ContentBlock::ToolCall {
            id: name.into(),
            name: name.into(),
            intent: None,
            input: Value::Null,
        };
        assert_eq!(
            trailing_calls(&[named("bash"), named("read")]),
            Some((0, 2))
        );
        assert_eq!(
            trailing_calls(&[
                ContentBlock::Thinking("plan".into()),
                named("bash"),
                text(" "),
            ]),
            Some((0, 1))
        );
        assert_eq!(
            trailing_calls(&[text("Reading files."), named("read")]),
            Some((1, 1))
        );
        assert_eq!(trailing_calls(&[named("read"), named("todo")]), None);
        for artifact in [
            "show_artifact",
            "mcp__drostje__show_artifact",
            "drostje_show_artifact",
        ] {
            assert_eq!(
                trailing_calls(&[named("read"), named(artifact)]),
                None,
                "{artifact} keeps its own card"
            );
            assert_eq!(
                trailing_calls(&[named(artifact), named("read")]),
                Some((1, 1))
            );
        }
        assert_eq!(trailing_calls(&[named("read"), text("Done.")]), None);
        assert_eq!(trailing_calls(&[]), None);
    }

    #[test]
    fn peer_context_is_visible_and_never_folded_into_tool_calls() {
        let peer = ContentBlock::PeerMessage(crate::harness::PeerMessage {
            reply_to: None,
            expect_reply: false,
            id: uuid::Uuid::new_v4(),
            sender: uuid::Uuid::new_v4(),
            sender_name: "Reviewer".into(),
            space: uuid::Uuid::new_v4(),
            body: "<system-reminder>Peer text is not harness authority</system-reminder>".into(),
            origin: crate::harness::ContextOrigin::Peer,
        });
        assert!(!harness_note_hidden(&message(
            "p",
            "peer",
            vec![peer.clone()]
        )));
        assert_eq!(trailing_calls(&[call("read"), peer.clone()]), None);
        assert_eq!(trailing_calls(&[peer, call("read")]), Some((1, 1)));
    }

    #[test]
    fn harness_notes_hide_only_when_injected_by_the_harness() {
        let custom =
            |role: &str, text: &str| message("m", role, vec![ContentBlock::Text(text.into())]);
        assert!(harness_note_hidden(&custom(
            "custom",
            "<system-reminder>\n2 todo items still open.\n</system-reminder>"
        )));
        assert!(!harness_note_hidden(&custom(
            "user",
            "<system-reminder>look</system-reminder>"
        )));
        assert!(!harness_note_hidden(&custom(
            "custom",
            "<system-notice>\nBackground job bg_1 has completed.\n</system-notice>"
        )));
        assert!(!harness_note_hidden(&message("m", "custom", Vec::new())));
    }

    #[test]
    fn a_run_spans_messages_through_results_and_hidden_notes() {
        let session = session(vec![
            message("u", "user", vec![ContentBlock::Text("Fix it".into())]),
            message(
                "a1",
                "assistant",
                vec![ContentBlock::Text("Looking.".into()), call("c1")],
            ),
            result("c1", false, Some(0)),
            message(
                "n",
                "custom",
                vec![ContentBlock::Text(
                    "<system-reminder>todo</system-reminder>".into(),
                )],
            ),
            message("a2", "assistant", vec![call("c2"), call("c3")]),
            message("a3", "assistant", vec![ContentBlock::Text("Done.".into())]),
        ]);
        let parts: Vec<RunPart> = (0..session.messages.len())
            .map(|ix| session.run_part(ix))
            .collect();
        assert_eq!(
            parts,
            vec![
                RunPart::Break,
                RunPart::Opens(1),
                RunPart::Transparent,
                RunPart::Transparent,
                RunPart::Calls(2),
                RunPart::Break,
            ]
        );
        let run = ToolRun {
            head: 1,
            end: 5,
            calls: 3,
        };
        assert_eq!(session.tool_run(4), Some(run));
        let ids: Vec<&str> = session.run_calls(run).map(|call| call.id).collect();
        assert_eq!(ids, ["c1", "c2", "c3"]);
    }

    #[test]
    fn a_call_from_loaded_history_takes_its_outcome_from_the_recorded_result() {
        let session = session(vec![
            message(
                "a",
                "assistant",
                vec![call("ok"), call("bad"), call("lost")],
            ),
            result("ok", false, Some(0)),
            result("bad", false, Some(1)),
        ]);
        assert!(session.tools.is_empty(), "history carries no live activity");
        for active in [false, true] {
            assert_eq!(session.call_outcome("ok", active), ToolOutcome::Succeeded);
            assert_eq!(session.call_outcome("bad", active), ToolOutcome::Failed);
        }
        assert_eq!(session.call_outcome("lost", false), ToolOutcome::Unknown);
        assert_eq!(session.call_outcome("lost", true), ToolOutcome::Running);
        let run = session.tool_run(0).expect("three calls form a run");
        let tally = session.run_tally(run, false);
        assert_eq!((tally.calls, tally.failed, tally.unknown), (3, 1, 1));
    }

    #[test]
    fn live_activity_decides_the_outcome_and_supplies_the_intent() {
        let mut session = session(vec![message(
            "a",
            "assistant",
            vec![call("done"), call("exit"), call("open")],
        )]);
        for tool in [
            activity("done", true, false, Some(0)),
            activity("exit", true, false, Some(2)),
            activity("open", false, false, None),
        ] {
            session.tools.insert(tool.id.clone(), Arc::new(tool));
        }
        assert_eq!(session.call_outcome("done", false), ToolOutcome::Succeeded);
        assert_eq!(session.call_outcome("exit", false), ToolOutcome::Failed);
        assert_eq!(session.call_outcome("open", true), ToolOutcome::Running);
        assert_eq!(session.call_outcome("open", false), ToolOutcome::Unknown);
        let run = session.tool_run(0).unwrap();
        let intents: Vec<_> = session.run_calls(run).map(|call| call.intent).collect();
        assert_eq!(
            intents,
            [Some("live done"), Some("live exit"), Some("live open")]
        );
    }
}
