use super::{
    disclosure::{disclosure, is_expanded, toggle},
    images::image_thumbnail,
    markdown::{Markdown, markdown_text},
    tools::{ToolCall, ToolOutcome, ToolResult, tool_card},
};
use crate::harness::{ContentBlock, ContextOrigin, PeerMessage};
use crate::model::ThreadId;
use crate::ui::workspace::Workspace;
use gpui_kit::{
    assets::IconName as Lucide,
    component::{
        ActiveTheme, Icon, Sizable,
        alert::Alert,
        button::{Button, ButtonVariants},
        h_flex,
        label::Label,
        tag::Tag,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

fn thinking_for_display(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some((marker, width)) = fence {
            let marker_width = trimmed.chars().take_while(|ch| *ch == marker).count();
            if marker_width >= width && trimmed.chars().all(|ch| ch == marker) {
                fence = None;
            }
            continue;
        }
        if matches!(trimmed, "<!-- -->" | "<!-->") {
            continue;
        }
        let marker = trimmed.chars().next();
        let marker_width = marker
            .filter(|marker| matches!(marker, '`' | '~'))
            .map_or(0, |marker| {
                trimmed.chars().take_while(|ch| *ch == marker).count()
            });
        if marker_width >= 3 {
            fence = marker.map(|marker| (marker, marker_width));
            if let Some(last) = lines.iter_mut().rev().find(|line| !line.trim().is_empty()) {
                let trimmed = last.trim_end();
                *last = if trimmed.ends_with("...") {
                    trimmed.to_owned()
                } else if let Some(prefix) = trimmed.strip_suffix('.') {
                    format!("{prefix}...")
                } else {
                    format!("{trimmed}...")
                };
            } else {
                lines.push("...".into());
            }
            continue;
        }
        lines.push(line.to_owned());
    }
    let display = lines.join("\n").trim().to_owned();
    if display
        .chars()
        .all(|ch| ch.is_whitespace() || matches!(ch, '.' | '…'))
    {
        String::new()
    } else {
        display
    }
}

fn thinking_titles(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(['.', '…']);
            let title = line.strip_prefix("**")?.strip_suffix("**")?.trim();
            (!title.is_empty() && !title.contains("**")).then_some(title)
        })
        .collect()
}

fn thinking_block(
    identity: String,
    display: SharedString,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    let key = format!("{identity}:thinking");
    let expanded = is_expanded(owner, &key, cx);
    let group: SharedString = format!("{key}:group").into();
    let header = h_flex()
        .id(SharedString::from(format!("{key}:header")))
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .on_click(toggle(owner, markdown.thread(), key.clone()))
        .child(Icon::new(Lucide::Lightbulb).xsmall())
        .child(div().flex_1().child("Thinking"))
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
                        "Hide reasoning"
                    } else {
                        "Show reasoning"
                    },
                )),
        );
    let detail: Option<AnyElement> = if expanded {
        let markdown_id = format!("{identity}:markdown");
        Some(guarded_content(
            identity,
            &display,
            owner,
            markdown.thread(),
            cx,
            "thinking",
            || markdown_text(markdown_id, &display, markdown).into_any_element(),
        ))
    } else {
        let titles = thinking_titles(&display);
        if titles.is_empty() {
            display
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(|line| {
                    div()
                        .truncate()
                        .child(line.trim().to_owned())
                        .into_any_element()
                })
        } else {
            Some(
                v_flex()
                    .gap_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .children(
                        titles
                            .into_iter()
                            .map(|title| div().whitespace_normal().child(title.to_owned())),
                    )
                    .into_any_element(),
            )
        }
    };
    v_flex()
        .id(SharedString::from(format!("{key}:block")))
        .debug_selector(|| "conversation-thinking".into())
        .group(group)
        .w_full()
        .min_w_0()
        .child(header)
        .when_some(detail, |block, detail| {
            block.child(
                div()
                    .ml_1p5()
                    .pl_3()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .text_color(cx.theme().muted_foreground)
                    .child(detail),
            )
        })
        .into_any_element()
}

pub(in crate::ui) fn block(
    content: &ContentBlock,
    identity: String,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    match content {
        ContentBlock::Text(text) => {
            let markdown_id = format!("{identity}:markdown");
            guarded_text(identity, text, owner, markdown.thread(), cx, || {
                markdown_text(markdown_id, text, markdown).into_any_element()
            })
        }
        ContentBlock::PeerMessage(peer) if peer.origin == ContextOrigin::Artifact => {
            artifact_event(&identity, peer, cx)
        }
        ContentBlock::PeerMessage(peer) => {
            let markdown_id = format!("{identity}:peer-markdown");
            let body = guarded_text(
                identity.clone(),
                &peer.body,
                owner,
                markdown.thread(),
                cx,
                || markdown_text(markdown_id, &peer.body, markdown).into_any_element(),
            );
            v_flex()
                .id(SharedString::from(format!("{identity}:peer:{}", peer.id)))
                .debug_selector(|| "conversation-peer-message".into())
                .w_full()
                .min_w_0()
                .gap_2()
                .pl_3()
                .border_l_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .child(Tag::secondary().outline().xsmall().child("Peer"))
                        .child(Label::new(peer.sender_name.clone()).text_sm()),
                )
                .child(
                    Label::new("Peer-authored context, not a user prompt or permission grant")
                        .text_xs()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(body)
                .into_any_element()
        }
        ContentBlock::Thinking(text) => {
            let display = match markdown {
                Markdown::Row(states, _) => {
                    states.derived(&format!("{identity}:thinking"), text, thinking_for_display)
                }
                Markdown::Element => thinking_for_display(text).into(),
            };
            if display.is_empty() {
                return div().into_any_element();
            }
            thinking_block(identity, display, owner, markdown, cx)
        }
        ContentBlock::Image(image) => image_thumbnail(image, owner, cx),
        ContentBlock::ImageError(error) => Alert::error(
            SharedString::from(identity),
            format!("Image unavailable: {error}"),
        )
        .small()
        .into_any_element(),
        ContentBlock::ToolCall {
            name,
            intent,
            input,
            ..
        } => tool_card(
            identity,
            ToolCall {
                name,
                intent: intent.as_deref(),
                input,
            },
            ToolResult {
                outcome: ToolOutcome::Unknown,
                content: &[],
                exit_code: None,
            },
            owner,
            markdown,
            cx,
        ),
    }
}

fn artifact_event(identity: &str, peer: &PeerMessage, cx: &App) -> AnyElement {
    let (kind, fields) = artifact_event_fields(&peer.body);
    let mono = cx.theme().mono_font_family.clone();
    v_flex()
        .id(SharedString::from(format!(
            "{identity}:artifact:{}",
            peer.id
        )))
        .debug_selector(|| "conversation-artifact-event".into())
        .w_full()
        .min_w_0()
        .gap_1()
        .pl_3()
        .border_l_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .min_w_0()
                .gap_2()
                .child(Tag::secondary().outline().xsmall().child("Artifact"))
                .child(Label::new(peer.sender_name.clone()).text_sm())
                .child(
                    Label::new(kind)
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(cx.theme().muted_foreground),
                ),
        )
        .children(fields.into_iter().map(|(name, value)| {
            h_flex()
                .min_w_0()
                .gap_2()
                .items_start()
                .child(
                    Label::new(name)
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().min_w_0().flex_1().child(Label::new(value).text_sm()))
        }))
        .into_any_element()
}

fn artifact_event_fields(body: &str) -> (String, Vec<(String, String)>) {
    const LIMIT: usize = 400;
    fn text(value: &serde_json::Value) -> String {
        let text = match value {
            serde_json::Value::Null => "—".to_owned(),
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(items) => {
                items.iter().map(text).collect::<Vec<_>>().join(", ")
            }
            other => other.to_string(),
        };
        if text.chars().count() > LIMIT {
            let mut cut: String = text.chars().take(LIMIT).collect();
            cut.push('…');
            cut
        } else {
            text
        }
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return ("event".into(), vec![("body".into(), text(&body.into()))]);
    };
    let kind = value["type"].as_str().unwrap_or("event").to_owned();
    let fields = match &value["data"] {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(name, value)| (name.clone(), text(value)))
            .collect(),
        serde_json::Value::Null => Vec::new(),
        other => vec![("value".into(), text(other))],
    };
    (kind, fields)
}

pub(super) fn guarded_text(
    identity: String,
    text: &str,
    owner: &WeakEntity<Workspace>,
    thread: Option<ThreadId>,
    cx: &App,
    render: impl FnOnce() -> AnyElement,
) -> AnyElement {
    guarded_content(identity, text, owner, thread, cx, "result", render)
}

fn guarded_content(
    identity: String,
    text: &str,
    owner: &WeakEntity<Workspace>,
    thread: Option<ThreadId>,
    cx: &App,
    kind: &str,
    render: impl FnOnce() -> AnyElement,
) -> AnyElement {
    if text.len() <= 16_384 {
        return render();
    }
    let expanded = is_expanded(owner, &identity, cx);
    let key = identity.clone();
    v_flex()
        .gap_2()
        .child(
            Button::new(SharedString::from(identity))
                .small()
                .ghost()
                .label(format!(
                    "{} large {kind} ({} bytes)",
                    if expanded { "Collapse" } else { "Expand" },
                    text.len()
                ))
                .on_click(toggle(owner, thread, key)),
        )
        .when(expanded, |view| view.child(render()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{artifact_event_fields, thinking_for_display, thinking_titles};

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn separate_message_text_blocks_select_their_own_text(cx: &mut gpui_kit::TestAppContext) {
        use super::{ContentBlock, Markdown, Workspace, block, v_flex};
        use gpui_kit::{
            AppContext, Context, InteractiveElement, IntoElement, Modifiers, MouseButton,
            MouseDownEvent, MouseUpEvent, ParentElement, Render, Styled, TestSupportExt,
            VisualTestContext, Window, component::Root,
        };

        struct MarkdownHarness;
        impl Render for MarkdownHarness {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let owner = gpui_kit::WeakEntity::<Workspace>::new_invalid();
                let first = ContentBlock::Text("First paragraph".into());
                let second = ContentBlock::Text(
                    "Second paragraph with enough different words to expose shared rich-text state."
                        .into(),
                );
                v_flex()
                    .id("markdown-blocks")
                    .debug_selector(|| "markdown-blocks".into())
                    .test_support()
                    .w(gpui_kit::px(420.))
                    .p_3()
                    .gap_3()
                    .child(block(
                        &first,
                        "message:0".into(),
                        &owner,
                        Markdown::Element,
                        cx,
                    ))
                    .child(block(
                        &second,
                        "message:1".into(),
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
            let content = cx.new(|_| MarkdownHarness);
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
        let blocks = visual
            .debug_bounds("markdown-blocks")
            .expect("Markdown blocks should render");
        let position = gpui_kit::point(
            blocks.left() + gpui_kit::px(20.),
            blocks.top() + gpui_kit::px(20.),
        );
        visual.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 3,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 3,
        });
        visual.update(|window, cx| {
            assert_eq!(
                gpui_kit::base::TextSelection::selected_text(window, cx).trim(),
                "First paragraph"
            );
        });
        visual.update(|window, cx| {
            gpui_kit::base::TextSelection::clear(window, cx);
        });
        let position = gpui_kit::point(
            blocks.left() + gpui_kit::px(20.),
            blocks.bottom() - gpui_kit::px(20.),
        );
        visual.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 3,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 3,
        });
        visual.update(|window, cx| {
            assert_eq!(
                gpui_kit::base::TextSelection::selected_text(window, cx).trim(),
                "Second paragraph with enough different words to expose shared rich-text state."
            );
        });
    }

    #[test]
    fn reasoning_reads_like_omp_instead_of_hiding_behind_a_generic_label() {
        assert_eq!(
            thinking_for_display(
                "**Planning the change**\n\n<!-- -->\n\n```rust\nlet hidden = true;\n```\n\nChecking the result"
            ),
            "**Planning the change**...\n\n\n\nChecking the result"
        );
        assert_eq!(thinking_for_display(" ... \n…"), "");
    }

    #[test]
    fn folded_reasoning_lists_its_step_titles() {
        assert_eq!(
            thinking_titles(
                "**Planning the change**...\n\nprose with **bold** inside\n\n  **Checking the result**  \n****"
            ),
            ["Planning the change", "Checking the result"]
        );
        assert!(thinking_titles("Just prose.").is_empty());
    }

    #[test]
    fn artifact_events_show_their_type_and_answered_fields() {
        let (kind, fields) = artifact_event_fields(
            r#"{"v":1,"id":"a1","type":"answer","data":{"scope":"crate","tags":["ui","tests"],"why":null}}"#,
        );
        assert_eq!(kind, "answer");
        assert_eq!(
            fields,
            [
                ("scope".to_owned(), "crate".to_owned()),
                ("tags".to_owned(), "ui, tests".to_owned()),
                ("why".to_owned(), "—".to_owned()),
            ]
        );
        let (kind, fields) =
            artifact_event_fields(r#"{"v":1,"id":"a2","type":"choose","data":"b"}"#);
        assert_eq!((kind.as_str(), fields[0].1.as_str()), ("choose", "b"));
        let (kind, fields) = artifact_event_fields("not json");
        assert_eq!((kind.as_str(), fields[0].1.as_str()), ("event", "not json"));
        let long = format!(
            r#"{{"v":1,"id":"a3","type":"note","data":{{"text":"{}"}}}}"#,
            "x".repeat(900)
        );
        assert_eq!(artifact_event_fields(&long).1[0].1.chars().count(), 401);
    }
}
