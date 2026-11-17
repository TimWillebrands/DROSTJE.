use super::{
    disclosure::{disclosure, toggle},
    mono,
    rows::harness_note_hidden,
    spinner::Spinner,
    tools::{ToolOutcome, call_outcome, summary_line, tool_summary},
};
use crate::ui::workspace::Workspace;
use crate::{
    app::ConversationSession,
    harness::{ContentBlock, Message},
    model::ThreadId,
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{ActiveTheme, Icon, Sizable, h_flex},
    prelude::FluentBuilder,
    *,
};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunPart {
    Break,
    Transparent,
    Opens(usize),
    Calls(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ToolRun {
    pub(super) head: usize,
    pub(super) end: usize,
    pub(super) calls: usize,
}

impl ToolRun {
    pub(super) fn folds(self) -> bool {
        self.calls >= 2
    }
}

pub(super) fn trailing_calls(content: &[ContentBlock]) -> Option<(usize, usize)> {
    let start = content
        .iter()
        .rposition(|block| match block {
            ContentBlock::ToolCall { name, .. } => {
                name == "todo" || name.ends_with("show_artifact")
            }
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

pub(super) fn run_part(message: &Message, session: &ConversationSession) -> RunPart {
    if message.role == "toolResult" {
        return if message
            .tool_call_id
            .as_deref()
            .is_some_and(|call| session.has_tool_call(call))
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

pub(super) fn tool_run(
    part: impl Fn(usize) -> RunPart,
    len: usize,
    index: usize,
) -> Option<ToolRun> {
    let mut head = None;
    for ix in (0..=index).rev() {
        match part(ix) {
            RunPart::Break => break,
            RunPart::Opens(_) => {
                head = Some(ix);
                break;
            }
            RunPart::Calls(_) => head = Some(ix),
            RunPart::Transparent => {}
        }
    }
    let head = head?;
    let (RunPart::Opens(mut calls) | RunPart::Calls(mut calls)) = part(head) else {
        return None;
    };
    let mut end = head + 1;
    while end < len {
        match part(end) {
            RunPart::Calls(more) => calls += more,
            RunPart::Transparent => {}
            RunPart::Break | RunPart::Opens(_) => break,
        }
        end += 1;
    }
    Some(ToolRun { head, end, calls })
}

#[derive(Clone, Default)]
pub(super) struct RunCache(Rc<RefCell<RunCacheState>>);

#[derive(Default)]
struct RunCacheState {
    revision: Option<(Uuid, u64)>,
    runs: HashMap<usize, Option<ToolRun>>,
}

impl RunCache {
    pub(super) fn run(&self, session: &ConversationSession, index: usize) -> Option<ToolRun> {
        let revision = (session.generation, session.transcript_revision);
        let len = session.messages.len();
        self.find(revision, len, index, |ix| {
            run_part(&session.messages[ix], session)
        })
    }

    fn find(
        &self,
        revision: (Uuid, u64),
        len: usize,
        index: usize,
        part: impl Fn(usize) -> RunPart,
    ) -> Option<ToolRun> {
        let mut cache = self.0.borrow_mut();
        if cache.revision != Some(revision) {
            cache.revision = Some(revision);
            cache.runs.clear();
        }
        if let Some(run) = cache.runs.get(&index) {
            return *run;
        }
        let run = tool_run(part, len, index);
        match run {
            Some(found) => cache
                .runs
                .extend((found.head..found.end).map(|ix| (ix, run))),
            None => {
                cache.runs.insert(index, None);
            }
        }
        run
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the run header takes the row context message() renders with"
)]
pub(super) fn tool_run_header(
    thread: ThreadId,
    key: String,
    run: ToolRun,
    expanded: bool,
    session: &ConversationSession,
    active: bool,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let mut latest = None;
    let (mut running, mut failed, mut unknown) = (0, 0, 0);
    for message in &session.messages[run.head..run.end] {
        let Some((start, _)) = trailing_calls(&message.content) else {
            continue;
        };
        for block in &message.content[start..] {
            let ContentBlock::ToolCall {
                id,
                name,
                intent,
                input,
            } = block
            else {
                continue;
            };
            match call_outcome(session, id, active) {
                ToolOutcome::Running => running += 1,
                ToolOutcome::Failed => failed += 1,
                ToolOutcome::Unknown => unknown += 1,
                ToolOutcome::Succeeded => {}
            }
            let intent = intent.as_deref().or_else(|| {
                session
                    .tools
                    .get(id)
                    .and_then(|tool| tool.intent.as_deref())
            });
            latest = Some((intent.unwrap_or(name), name, input));
        }
    }
    let latest = latest
        .filter(|_| !expanded)
        .map(|(caption, name, input)| (caption.to_owned(), tool_summary(name, input)));
    let group: SharedString = format!("{key}:group").into();
    h_flex()
        .id(SharedString::from(format!("{key}:header")))
        .debug_selector(|| "conversation-tool-run".into())
        .test_support()
        .group(group.clone())
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .on_click(toggle(owner, Some(thread), key.clone()))
        .child(Icon::new(Lucide::Wrench).xsmall())
        .child(
            div()
                .flex_shrink_0()
                .text_color(cx.theme().foreground)
                .child(format!("{} tool calls", run.calls)),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .when_some(latest, |row, (caption, summary)| {
                    row.child(Icon::new(summary.icon).xsmall())
                        .child(
                            div()
                                .flex_shrink_0()
                                .max_w(relative(0.5))
                                .truncate()
                                .child(caption),
                        )
                        .child(
                            mono(cx)
                                .flex_1()
                                .truncate()
                                .child(summary_line(&summary.body)),
                        )
                }),
        )
        .when(unknown > 0, |row| {
            row.child(div().flex_shrink_0().text_xs().child(if unknown == 1 {
                "1 without result".to_owned()
            } else {
                format!("{unknown} without result")
            }))
        })
        .when(failed > 0, |row| {
            row.child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(Icon::new(Lucide::CircleX).xsmall())
                    .child(format!("{failed} failed")),
            )
        })
        .when(running > 0, |row| row.child(Spinner::new().xsmall()))
        .child(
            div()
                .when(!expanded, |chevron| {
                    chevron
                        .invisible()
                        .group_hover(group, |style| style.visible())
                })
                .child(disclosure(
                    key,
                    expanded,
                    owner,
                    Some(thread),
                    if expanded {
                        "Hide tool calls"
                    } else {
                        "Show tool calls"
                    },
                )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        ContentBlock, Message, RunPart, ToolRun, harness_note_hidden, tool_run, trailing_calls,
    };
    use serde_json::Value;

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

    fn call(name: &str) -> ContentBlock {
        ContentBlock::ToolCall {
            id: name.into(),
            name: name.into(),
            intent: None,
            input: Value::Null,
        }
    }

    #[test]
    fn trailing_calls_skip_reasoning_but_not_prose_todo_or_artifacts() {
        let text = |text: &str| ContentBlock::Text(text.into());
        assert_eq!(trailing_calls(&[call("bash"), call("read")]), Some((0, 2)));
        assert_eq!(
            trailing_calls(&[
                ContentBlock::Thinking("plan".into()),
                call("bash"),
                text(" "),
            ]),
            Some((0, 1))
        );
        assert_eq!(
            trailing_calls(&[text("Reading files."), call("read")]),
            Some((1, 1))
        );
        assert_eq!(trailing_calls(&[call("read"), call("todo")]), None);
        for artifact in [
            "show_artifact",
            "mcp__drostje__show_artifact",
            "drostje_show_artifact",
        ] {
            assert_eq!(
                trailing_calls(&[call("read"), call(artifact)]),
                None,
                "{artifact} keeps its own card"
            );
            assert_eq!(
                trailing_calls(&[call(artifact), call("read")]),
                Some((1, 1))
            );
        }
        assert_eq!(trailing_calls(&[call("read"), text("Done.")]), None);
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
        let mut message = custom("peer", "");
        message.content = vec![peer.clone()];
        assert!(!harness_note_hidden(&message));
        assert_eq!(trailing_calls(&[call("read"), peer.clone()]), None);
        assert_eq!(trailing_calls(&[peer, call("read")]), Some((1, 1)));
    }

    #[test]
    fn run_cache_finds_each_run_once_per_revision() {
        use super::RunCache;
        use RunPart::{Break, Calls, Opens};
        use std::cell::Cell;
        let parts = [Break, Opens(1), Calls(1), Calls(2), Break, Calls(1)];
        let looked = Cell::new(0);
        let part = |ix: usize| {
            looked.set(looked.get() + 1);
            parts[ix]
        };
        let cache = RunCache::default();
        let revision = (uuid::Uuid::nil(), 1);
        let run = Some(ToolRun {
            head: 1,
            end: 4,
            calls: 4,
        });
        assert_eq!(cache.find(revision, parts.len(), 2, part), run);
        let first = looked.get();
        for index in 1..4 {
            assert_eq!(cache.find(revision, parts.len(), index, part), run);
        }
        assert_eq!(cache.find(revision, parts.len(), 0, part), None);
        assert_eq!(cache.find(revision, parts.len(), 0, part), None);
        assert_eq!(
            looked.get(),
            first + 1,
            "rows of a found run and a row without one are not looked at again"
        );
        assert_eq!(
            cache.find((uuid::Uuid::nil(), 2), parts.len(), 3, part),
            run
        );
        assert!(looked.get() > first + 1);
    }

    #[test]
    fn runs_span_calls_across_messages_and_skip_hidden_rows() {
        use RunPart::{Break, Calls, Opens, Transparent};
        let parts = [
            Break,
            Opens(1),
            Transparent,
            Calls(1),
            Transparent,
            Calls(2),
            Break,
            Calls(1),
            Opens(1),
            Calls(1),
        ];
        let run = |index| tool_run(|ix| parts[ix], parts.len(), index);
        let first = Some(ToolRun {
            head: 1,
            end: 6,
            calls: 4,
        });
        assert_eq!(run(0), None);
        for index in 1..6 {
            assert_eq!(run(index), first, "row {index}");
        }
        assert_eq!(run(6), None);
        let lone = run(7).unwrap();
        assert_eq!((lone.head, lone.end, lone.folds()), (7, 8, false));
        assert_eq!(
            run(9),
            Some(ToolRun {
                head: 8,
                end: 10,
                calls: 2
            })
        );
        assert_eq!(tool_run(|ix| [Break, Transparent][ix], 2, 1), None);
    }
}
