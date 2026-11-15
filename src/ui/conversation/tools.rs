use super::{
    blocks::{block, guarded_text},
    disclosure::{disclosure, is_expanded, toggle},
    markdown::{Markdown, code_block},
    mono,
    spinner::Spinner,
    todo_list::TodoList,
    todos::todo_card,
};
use crate::ui::{
    commands::{Command, FocusComposerArtifact, MoveArtifactIntoConversation},
    workspace::Workspace,
};
use crate::{
    app::ConversationSession,
    file_refs::{self, FileReference},
    harness::ContentBlock,
    model::ThreadId,
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{
        ActiveTheme, Icon, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        label::Label,
        tag::Tag,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::Value;

fn result_language<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    match name.to_ascii_lowercase().as_str() {
        "bash" | "eval" | "grep" | "glob" => Some("text"),
        "read" | "write" | "edit" => Some(
            path_field(input)
                .and_then(|path| path.rsplit_once('.'))
                .map(|(_, extension)| extension)
                .filter(|extension| {
                    !extension.is_empty() && extension.chars().all(char::is_alphanumeric)
                })
                .unwrap_or("text"),
        ),
        _ => None,
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct ToolSummary {
    pub icon: Lucide,
    pub body: String,
}
const SUMMARY_LIMIT: usize = 200;
pub(super) fn text_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input[key].as_str().filter(|value| !value.trim().is_empty())
}
fn path_field(input: &Value) -> Option<&str> {
    text_field(input, "path").or_else(|| text_field(input, "file_path"))
}
fn compact_json(input: &Value) -> String {
    const PREFIX_BYTES: usize = (SUMMARY_LIMIT + 1) * 4;
    struct Prefix(Vec<u8>);
    impl std::io::Write for Prefix {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let room = PREFIX_BYTES - self.0.len();
            if room == 0 {
                return Err(std::io::Error::other("summary prefix is complete"));
            }
            let taken = room.min(bytes.len());
            self.0.extend_from_slice(&bytes[..taken]);
            Ok(taken)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let serialized;
    let text = match input {
        Value::Null => "",
        Value::String(text) => text.as_str(),
        other => {
            let mut prefix = Prefix(Vec::with_capacity(PREFIX_BYTES));
            let _ = serde_json::to_writer(&mut prefix, other);
            serialized = match String::from_utf8(prefix.0) {
                Ok(text) => text,
                Err(error) => {
                    let valid = error.utf8_error().valid_up_to();
                    let mut bytes = error.into_bytes();
                    bytes.truncate(valid);
                    String::from_utf8(bytes).unwrap_or_default()
                }
            };
            serialized.as_str()
        }
    };
    match text.char_indices().nth(SUMMARY_LIMIT) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}
fn patch_paths(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .filter(|line| line.starts_with('[') && line.ends_with(']'))
        .map(|line| {
            let inner = &line[1..line.len() - 1];
            inner.rsplit_once('#').map_or(inner, |(path, _)| path)
        })
        .filter(|path| !path.is_empty())
        .collect()
}
pub fn tool_summary(name: &str, input: &Value) -> ToolSummary {
    let (icon, body) = match name.to_ascii_lowercase().as_str() {
        "bash" => (
            Lucide::Terminal,
            text_field(input, "command").map(str::to_owned),
        ),
        "read" => (Lucide::FileText, path_field(input).map(str::to_owned)),
        "write" => (Lucide::FilePlus, path_field(input).map(str::to_owned)),
        "edit" => (
            Lucide::FilePen,
            text_field(input, "input")
                .map(patch_paths)
                .filter(|paths| !paths.is_empty())
                .map(|paths| paths.join("\n"))
                .or_else(|| path_field(input).map(str::to_owned)),
        ),
        "grep" => (
            Lucide::Search,
            text_field(input, "pattern").map(|pattern| match path_field(input) {
                Some(path) => format!("{pattern}  in {path}"),
                None => pattern.to_owned(),
            }),
        ),
        "glob" => (
            Lucide::FolderSearch,
            text_field(input, "pattern")
                .or_else(|| path_field(input))
                .map(str::to_owned),
        ),
        "todo" => (Lucide::ListTodo, super::todo_list::describe_change(input)),
        "task" => (
            Lucide::Workflow,
            input["tasks"]
                .as_array()
                .map(|tasks| match tasks.len() {
                    1 => "1 task".to_owned(),
                    n => format!("{n} tasks"),
                })
                .or_else(|| text_field(input, "description").map(str::to_owned)),
        ),
        "eval" => (
            Lucide::FlaskConical,
            text_field(input, "title")
                .or_else(|| text_field(input, "language"))
                .map(str::to_owned),
        ),
        "ask" => (
            Lucide::CircleQuestionMark,
            input["questions"][0]["question"]
                .as_str()
                .or_else(|| text_field(input, "question"))
                .map(str::to_owned),
        ),
        "web_search" | "websearch" => {
            (Lucide::Globe, text_field(input, "query").map(str::to_owned))
        }
        "webfetch" => (Lucide::Globe, text_field(input, "url").map(str::to_owned)),
        _ => (Lucide::Wrench, None),
    };
    ToolSummary {
        icon,
        body: body.unwrap_or_else(|| compact_json(input)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolOutcome {
    Running,
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Copy)]
pub(super) struct ToolCall<'a> {
    pub(super) name: &'a str,
    pub(super) intent: Option<&'a str>,
    pub(super) input: &'a Value,
}
#[derive(Clone, Copy)]
pub(super) struct ToolResult<'a> {
    pub(super) outcome: ToolOutcome,
    pub(super) content: &'a [ContentBlock],
    pub(super) exit_code: Option<i64>,
}
impl ToolResult<'_> {
    pub(super) fn recorded(error: bool, exit_code: Option<i64>) -> ToolOutcome {
        if error || exit_code.is_some_and(|code| code != 0) {
            ToolOutcome::Failed
        } else {
            ToolOutcome::Succeeded
        }
    }
}
pub(super) fn summary_line(body: &str) -> String {
    let mut lines = body.lines().filter(|line| !line.trim().is_empty());
    let first = lines.next().unwrap_or_default().trim();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

pub(super) fn tool_card(
    key: String,
    call: ToolCall<'_>,
    result: ToolResult<'_>,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    let ToolCall {
        name,
        intent,
        input,
    } = call;
    let ToolResult {
        outcome,
        content: result,
        exit_code,
    } = result;
    if name == "todo"
        && outcome != ToolOutcome::Failed
        && let Some(list) = result.iter().find_map(|block| match block {
            ContentBlock::Text(text) => TodoList::parse(text),
            _ => None,
        })
    {
        return todo_card(key, call, &list, outcome, owner, markdown.thread(), cx);
    }
    let summary = tool_summary(name, input);
    let has_result = !result.is_empty() || exit_code.is_some();
    let has_detail = has_result || !summary.body.is_empty();
    let expanded = outcome == ToolOutcome::Failed || is_expanded(owner, &key, cx);
    let caption = intent.unwrap_or(name).to_owned();
    let mark: Option<AnyElement> = match outcome {
        ToolOutcome::Running => Some(Spinner::new().xsmall().into_any_element()),
        ToolOutcome::Failed => Some(
            Icon::new(Lucide::CircleX)
                .xsmall()
                .text_color(cx.theme().danger)
                .into_any_element(),
        ),
        ToolOutcome::Succeeded => None,
        ToolOutcome::Unknown => Some(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("No result")
                .into_any_element(),
        ),
    };
    let group: SharedString = format!("{key}:group").into();
    let mut header = h_flex()
        .id(SharedString::from(format!("{key}:header")))
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(summary.icon).xsmall())
        .child(
            div()
                .flex_shrink_0()
                .max_w(relative(0.5))
                .truncate()
                .child(caption),
        )
        .child(
            mono(cx)
                .debug_selector(|| "conversation-tool-summary-line".into())
                .flex_1()
                .truncate()
                .when(!expanded, |line| line.child(summary_line(&summary.body))),
        );
    if intent.is_some() && expanded {
        header = header.child(div().flex_shrink_0().child(name.to_owned()));
    }
    if let Some(reference) = file_refs::tool_reference(name, input) {
        let thread = markdown.thread();
        header = header.child(
            div()
                .invisible()
                .group_hover(group.clone(), |style| style.visible())
                .child(open_file_button(
                    format!("{key}:open-file"),
                    reference,
                    thread,
                )),
        );
    }
    header = header.when_some(mark, |row, mark| row.child(mark));
    if has_detail && outcome != ToolOutcome::Failed {
        header = header
            .on_click(toggle(owner, markdown.thread(), key.clone()))
            .child(
                div()
                    .when(!expanded, |chevron| {
                        chevron
                            .invisible()
                            .group_hover(group.clone(), |style| style.visible())
                    })
                    .child(disclosure(
                        key.clone(),
                        expanded,
                        owner,
                        markdown.thread(),
                        if expanded {
                            "Hide details"
                        } else {
                            "Show details"
                        },
                    )),
            );
    }
    let card_selector = format!("{key}:card");
    let mut card = v_flex()
        .id(SharedString::from(card_selector))
        .debug_selector(|| "conversation-tool-card".into())
        .test_support()
        .group(group)
        .w_full()
        .min_w_0()
        .child(header);
    let has_body = !summary.body.is_empty();
    let mut detail = v_flex()
        .w_full()
        .min_w_0()
        .mb_1()
        .px_3()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius)
        .when(expanded && has_body, |detail| {
            detail.child(mono(cx).py_2().child(summary.body))
        });
    if has_result && expanded {
        let mut output = v_flex()
            .debug_selector(|| "conversation-tool-full-result".into())
            .gap_2()
            .py_2()
            .when(has_body, |output| {
                output.border_t_1().border_color(cx.theme().border)
            });
        if let Some(code) = exit_code.filter(|code| *code != 0 || outcome == ToolOutcome::Failed) {
            output = output.child(mono(cx).child(format!("Exit code {code}")));
        }
        let language = result_language(name, input);
        for (index, block) in result.iter().enumerate() {
            let identity = format!("{key}:{index}");
            output = output.child(match (block, language) {
                (ContentBlock::Text(text), Some(language)) => {
                    let markdown_id = format!("{identity}:markdown");
                    guarded_text(identity, text, owner, markdown.thread(), cx, || {
                        code_block(markdown_id, text, language, markdown).into_any_element()
                    })
                }
                _ => self::block(block, identity, owner, markdown, cx),
            });
        }
        detail = detail.child(output);
    }
    if expanded && has_detail {
        card = card.child(detail);
    }
    card.into_any_element()
}

fn open_file_button(id: String, reference: FileReference, thread: Option<ThreadId>) -> Button {
    Button::new(SharedString::from(id))
        .xsmall()
        .ghost()
        .cursor_default()
        .tab_stop(false)
        .icon(Lucide::FileCode)
        .tooltip("Open in Neovim")
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(
                Box::new(crate::ui::commands::OpenFileReference {
                    reference: reference.clone(),
                    thread,
                }),
                cx,
            );
        })
}

pub(super) fn call_outcome(
    session: &ConversationSession,
    call_id: &str,
    active: bool,
) -> ToolOutcome {
    let pending = if active {
        ToolOutcome::Running
    } else {
        ToolOutcome::Unknown
    };
    if let Some(tool) = session.tools.get(call_id) {
        return if tool.error || tool.exit_code.is_some_and(|code| code != 0) {
            ToolOutcome::Failed
        } else if tool.complete {
            ToolOutcome::Succeeded
        } else {
            pending
        };
    }
    session.tool_result(call_id).map_or(pending, |result| {
        ToolResult::recorded(result.error, result.exit_code)
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "a row helper takes the row context message() renders with"
)]
pub(super) fn tool_call_row(
    thread: ThreadId,
    call_id: &str,
    call: ToolCall<'_>,
    session: &ConversationSession,
    active: bool,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    if call.name.ends_with("show_artifact") {
        let recorded = session.tool_result(call_id);
        let blocks = session
            .tools
            .get(call_id)
            .map(|tool| tool.result.as_slice())
            .or_else(|| recorded.map(|result| result.content.as_slice()))
            .unwrap_or(&[]);
        let browser = blocks.iter().find_map(|block| {
            let ContentBlock::Text(text) = block else {
                return None;
            };
            let value: Value = serde_json::from_str(text).ok()?;
            let id = value
                .get("id")
                .or_else(|| value.get("result")?.get("id"))?
                .as_str()?;
            uuid::Uuid::parse_str(id).ok()
        });
        if let Some((pane, placement)) =
            browser.and_then(|id| owner.upgrade()?.read(cx).artifact_browser(thread, id))
        {
            return match placement {
                crate::browser_tools::ArtifactPlacement::Conversation => pane.into_any_element(),
                crate::browser_tools::ArtifactPlacement::Composer => {
                    docked_artifact_stub(call_id, &pane, owner, cx)
                }
            };
        }
    }
    let key = format!("tool:{thread}:{call_id}");
    let outcome = call_outcome(session, call_id, active);
    if let Some(tool) = session.tools.get(call_id) {
        return tool_card(
            key,
            ToolCall {
                intent: call.intent.or(tool.intent.as_deref()),
                ..call
            },
            ToolResult {
                outcome,
                content: &tool.result,
                exit_code: tool.exit_code,
            },
            owner,
            markdown,
            cx,
        );
    }
    let result = match session.tool_result(call_id) {
        Some(result) => ToolResult {
            outcome,
            content: &result.content,
            exit_code: result.exit_code,
        },
        None => ToolResult {
            outcome,
            content: &[],
            exit_code: None,
        },
    };
    tool_card(key, call, result, owner, markdown, cx)
}

pub(super) fn artifact_rows(session: &ConversationSession) -> Vec<usize> {
    let artifact = |name: &str| name.ends_with("show_artifact");
    if !session
        .tool_call_order()
        .iter()
        .any(|(name, _)| artifact(name))
    {
        return Vec::new();
    }
    session
        .messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolCall { name, .. } if artifact(name)))
        })
        .map(|(index, _)| index)
        .collect()
}

fn docked_artifact_stub(
    call_id: &str,
    pane: &Entity<crate::ui::browser::BrowserPane>,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let title = pane
        .read(cx)
        .artifact_title()
        .unwrap_or("HTML artifact")
        .to_owned();
    let focus = owner.clone();
    let inline = owner.clone();
    h_flex()
        .id(SharedString::from(format!(
            "docked-artifact-stub-{call_id}"
        )))
        .debug_selector(|| "docked-artifact-stub".into())
        .test_support()
        .w_full()
        .min_w_0()
        .gap_2()
        .px_2()
        .py_1()
        .items_center()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius)
        .child(
            Tag::secondary()
                .outline()
                .xsmall()
                .child("Above the prompt"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Label::new(title).text_sm().truncate()),
        )
        .child(
            Button::new(SharedString::from(format!(
                "docked-artifact-focus-{call_id}"
            )))
            .ghost()
            .xsmall()
            .label("Focus")
            .tooltip_with_action(
                Command::FocusComposerArtifact.label(),
                &FocusComposerArtifact,
                Some("Workspace"),
            )
            .on_click(move |_, window, cx| {
                let _ = focus.update(cx, |workspace, cx| {
                    workspace.execute(Command::FocusComposerArtifact, window, cx)
                });
            }),
        )
        .child(
            Button::new(SharedString::from(format!(
                "docked-artifact-inline-{call_id}"
            )))
            .ghost()
            .xsmall()
            .label("Show here")
            .tooltip_with_action(
                Command::MoveArtifactIntoConversation.label(),
                &MoveArtifactIntoConversation,
                Some("Workspace"),
            )
            .on_click(move |_, window, cx| {
                let _ = inline.update(cx, |workspace, cx| {
                    workspace.execute(Command::MoveArtifactIntoConversation, window, cx)
                });
            }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        Lucide, SUMMARY_LIMIT, ToolSummary, Value, result_language, summary_line, tool_summary,
    };
    use serde_json::json;

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn compact_timeline_states_render_on_the_native_surface(cx: &mut gpui_kit::TestAppContext) {
        use super::{
            ContentBlock, Markdown, ToolCall, ToolOutcome, ToolResult, Workspace, block, tool_card,
            v_flex,
        };
        use gpui_kit::{
            AppContext, Context, IntoElement, ParentElement, Render, Styled, VisualTestContext,
            WeakEntity, Window, component::Root,
        };

        struct TimelineHarness;
        impl Render for TimelineHarness {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let owner = WeakEntity::<Workspace>::new_invalid();
                let reasoning =
                    ContentBlock::Thinking("**Planning**\n\nInspect the relevant files".into());
                let result = [ContentBlock::Text(
                    "one\ntwo\nthree\nfour\nfive\nsix\nseven".into(),
                )];
                let failure = [ContentBlock::Text("permission denied".into())];
                v_flex()
                    .gap_2()
                    .child(block(
                        &reasoning,
                        "reasoning".into(),
                        &owner,
                        Markdown::Element,
                        cx,
                    ))
                    .child(tool_card(
                        "preview".into(),
                        ToolCall {
                            name: "grep",
                            intent: Some("Finding timeline rendering"),
                            input: &Value::Null,
                        },
                        ToolResult {
                            outcome: ToolOutcome::Succeeded,
                            content: &result,
                            exit_code: Some(0),
                        },
                        &owner,
                        Markdown::Element,
                        cx,
                    ))
                    .child(tool_card(
                        "failure".into(),
                        ToolCall {
                            name: "bash",
                            intent: Some("Running validation"),
                            input: &Value::Null,
                        },
                        ToolResult {
                            outcome: ToolOutcome::Failed,
                            content: &failure,
                            exit_code: Some(1),
                        },
                        &owner,
                        Markdown::Element,
                        cx,
                    ))
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let window = cx.add_window(|window, cx| {
            let content = cx.new(|_| TimelineHarness);
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
        assert!(visual.debug_bounds("conversation-thinking").is_some());
        assert!(
            visual
                .debug_bounds("conversation-tool-summary-line")
                .is_some()
        );
        assert!(
            visual
                .debug_bounds("conversation-tool-full-result")
                .is_some()
        );
    }

    #[test]
    fn collapsed_tools_show_one_line_of_the_call() {
        assert_eq!(summary_line("cargo test"), "cargo test");
        assert_eq!(summary_line("\n  src/a.rs\nsrc/b.rs"), "src/a.rs …");
        assert_eq!(summary_line(""), "");
    }

    #[test]
    fn plain_output_tools_get_a_code_block_in_the_file_language() {
        assert_eq!(result_language("bash", &Value::Null), Some("text"));
        assert_eq!(
            result_language("read", &json!({"path":"src/main.rs"})),
            Some("rs")
        );
        assert_eq!(
            result_language("read", &json!({"path":"Makefile"})),
            Some("text")
        );
        assert_eq!(
            result_language("write", &json!({"path":"notes.tar.gz"})),
            Some("gz")
        );
        assert_eq!(result_language("task", &Value::Null), None);
        assert_eq!(result_language("web_search", &Value::Null), None);
    }

    #[test]
    fn compact_json_stops_serializing_at_the_cut() {
        use super::compact_json;
        let whole = |input: &Value| {
            let text = match input {
                Value::Null => String::new(),
                Value::String(text) => text.clone(),
                other => serde_json::to_string(other).unwrap(),
            };
            if text.chars().count() > SUMMARY_LIMIT {
                let cut: String = text.chars().take(SUMMARY_LIMIT).collect();
                format!("{cut}…")
            } else {
                text
            }
        };
        let wide = "é".repeat(SUMMARY_LIMIT);
        let emoji = "🦀".repeat(3 * SUMMARY_LIMIT);
        for input in [
            Value::Null,
            json!("short"),
            json!("x".repeat(SUMMARY_LIMIT)),
            json!("x".repeat(SUMMARY_LIMIT + 1)),
            json!({"op": "cancel", "ids": ["bg_7"]}),
            json!({"text": wide}),
            json!({"text": emoji}),
            json!({"rows": (0..10_000).collect::<Vec<_>>()}),
            json!([wide, "x".repeat(SUMMARY_LIMIT * 8)]),
        ] {
            assert_eq!(compact_json(&input), whole(&input), "{input:.40}");
        }
    }

    #[test]
    fn tool_summary_prefers_command_path_pattern_and_falls_back_to_json() {
        assert_eq!(
            tool_summary("bash", &json!({"command":"ls -la","cwd":"/tmp","i":"List"})),
            ToolSummary {
                icon: Lucide::Terminal,
                body: "ls -la".into()
            }
        );
        assert_eq!(
            tool_summary("read", &json!({"path":"src/main.rs"})).body,
            "src/main.rs"
        );
        assert_eq!(
            tool_summary(
                "edit",
                &json!({"input":"*** Begin Patch\n[src/a.rs#69AE]\nPUT >1:\n+x\n[src/b.rs]\n"})
            ),
            ToolSummary {
                icon: Lucide::FilePen,
                body: "src/a.rs\nsrc/b.rs".into()
            }
        );
        assert_eq!(
            tool_summary("grep", &json!({"pattern":"fn main","path":"src"})).body,
            "fn main  in src"
        );
        assert_eq!(
            tool_summary("ask", &json!({"questions":[{"question":"Which one?"}]})),
            ToolSummary {
                icon: Lucide::CircleQuestionMark,
                body: "Which one?".into()
            }
        );
        assert_eq!(
            tool_summary("Read", &json!({"file_path":"src/effects/pools.ts"})),
            ToolSummary {
                icon: Lucide::FileText,
                body: "src/effects/pools.ts".into()
            }
        );
        assert_eq!(
            tool_summary("Bash", &json!({"command":"ls","description":"List"})).body,
            "ls"
        );
        let unknown = tool_summary("hub", &json!({"op":"cancel","ids":["bg_7"]}));
        assert_eq!(unknown.icon, Lucide::Wrench);
        assert_eq!(unknown.body, r#"{"op":"cancel","ids":["bg_7"]}"#);
        assert_eq!(tool_summary("bash", &Value::Null).body, "");
        let long = tool_summary("other", &json!({"text": "x".repeat(500)}));
        assert!(long.body.ends_with('…'));
        assert_eq!(long.body.chars().count(), SUMMARY_LIMIT + 1);
    }
}
