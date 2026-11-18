use super::{
    blocks::{block, guarded_text},
    disclosure::{disclosure, is_expanded, toggle},
    markdown::{Markdown, code_block},
    mono,
    spinner::Spinner,
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
    transcript::{
        ToolKind, ToolOutcome, result_language, summary_line, todo::TodoList, tool_summary,
    },
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

pub(super) fn tool_icon(kind: ToolKind) -> Lucide {
    match kind {
        ToolKind::Shell => Lucide::Terminal,
        ToolKind::Read => Lucide::FileText,
        ToolKind::Write => Lucide::FilePlus,
        ToolKind::Edit => Lucide::FilePen,
        ToolKind::Grep => Lucide::Search,
        ToolKind::Glob => Lucide::FolderSearch,
        ToolKind::Todo => Lucide::ListTodo,
        ToolKind::Task => Lucide::Workflow,
        ToolKind::Eval => Lucide::FlaskConical,
        ToolKind::Ask => Lucide::CircleQuestionMark,
        ToolKind::WebSearch | ToolKind::WebFetch => Lucide::Globe,
        ToolKind::Other => Lucide::Wrench,
    }
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
        .child(Icon::new(tool_icon(summary.kind)).xsmall())
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
    let outcome = session.call_outcome(call_id, active);
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
    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn compact_timeline_states_render_on_the_native_surface(cx: &mut gpui_kit::TestAppContext) {
        use super::{
            ContentBlock, Markdown, ToolCall, ToolOutcome, ToolResult, Value, Workspace, block,
            tool_card, v_flex,
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
}
