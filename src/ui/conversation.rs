use super::{
    commands::{Command, button},
    workspace::Workspace,
};
use crate::{
    app::{AppState, ConversationSession},
    harness::{ContentBlock, Message, UserAnswer, UserRequest, UserRequestKind},
    model::{ImageAttachment, ModelKey, ThreadId},
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{
        ActiveTheme, Disableable, Icon, IconName, Sizable,
        alert::Alert,
        attachment::{
            Attachment, AttachmentActions, AttachmentContent, AttachmentDescription,
            AttachmentMedia, AttachmentTitle,
        },
        button::{Button, ButtonVariants},
        collapsible::Collapsible,
        h_flex,
        label::Label,
        menu::{DropdownMenu, PopupMenuItem},
        searchable_list::SearchableListItem,
        select::{Select, SelectEvent, SelectState},
        spinner::Spinner,
        text::{MarkdownNode, MarkdownParseContext, MarkdownPlugin, markdown, markdown_ast},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, PartialEq)]
struct ModelOption {
    key: ModelKey,
    label: SharedString,
    name: String,
}

impl SearchableListItem for ModelOption {
    type Value = ModelKey;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &ModelKey {
        &self.key
    }
}

pub(super) struct ModelPicker {
    thread_id: ThreadId,
    options: Vec<ModelOption>,
    selected: Option<ModelKey>,
    select: Entity<SelectState<Vec<ModelOption>>>,
    _subscription: Subscription,
}

impl Workspace {
    pub(super) fn sync_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(thread) = state.conversation_thread() else {
            self.model_picker = None;
            return;
        };
        let thread_id = thread.id;
        let session = state.conversations.get(&thread_id);
        let selected = session
            .and_then(|session| session.model.as_ref())
            .or(thread.model.as_ref());
        if self.model_picker.as_ref().is_some_and(|picker| {
            picker.thread_id == thread_id
                && picker.selected.as_ref() == selected
                && picker.options.len() == session.map_or(0, |session| session.models.len())
                && session
                    .into_iter()
                    .flat_map(|session| &session.models)
                    .zip(&picker.options)
                    .all(|(model, option)| model.key == option.key && model.name == option.name)
        }) {
            return;
        }
        let options: Vec<_> = session
            .into_iter()
            .flat_map(|session| &session.models)
            .map(|model| ModelOption {
                key: model.key.clone(),
                name: model.name.clone(),
                label: format!("{} · {}/{}", model.name, model.key.provider, model.key.id).into(),
            })
            .collect();
        let selected = selected.cloned();
        if let Some(picker) = self
            .model_picker
            .as_mut()
            .filter(|picker| picker.thread_id == thread_id)
        {
            if picker.options != options || picker.selected != selected {
                picker.select.update(cx, |select, cx| {
                    if picker.options != options {
                        select.set_items(options.clone(), window, cx);
                    }
                    if let Some(selected) = &selected {
                        select.set_selected_value(selected, window, cx);
                    } else {
                        select.set_selected_index(None, window, cx);
                    }
                });
                picker.options = options;
                picker.selected = selected;
            }
            return;
        }
        let select = cx.new(|cx| {
            let mut select = SelectState::new(options.clone(), None, window, cx);
            if let Some(selected) = &selected {
                select.set_selected_value(selected, window, cx);
            }
            select
        });
        let subscription = cx.subscribe_in(
            &select,
            window,
            move |workspace, _, event: &SelectEvent<Vec<ModelOption>>, _, cx| {
                let SelectEvent::Confirm(Some(key)) = event else {
                    return;
                };
                workspace.state.update(cx, |state, cx| {
                    state.select_model(thread_id, key.clone());
                    cx.notify();
                });
            },
        );
        self.model_picker = Some(ModelPicker {
            thread_id,
            options,
            selected,
            select,
            _subscription: subscription,
        });
    }

    pub(super) fn toggle_thread_details(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tool_detail = Some((id, String::new()));
        self.sync_detail_surface(window, cx);
        cx.notify();
    }
}

// A full-width row preserves the transcript's edge scrollbar; only its content is centered.
fn content_column() -> Div {
    div().w_full().max_w(rems(60.)).px_3()
}

fn inset(content: impl IntoElement) -> AnyElement {
    h_flex()
        .w_full()
        .justify_center()
        .child(content_column().child(content))
        .into_any_element()
}

/// Keep HTML and Markdown image references inert. Only decoded provider attachments
/// may load image resources; ordinary assistant prose cannot name a local file to load.
struct InertResources;
impl MarkdownPlugin for InertResources {
    fn name(&self) -> &str {
        "drostje-inert-resources"
    }
    fn is_block(&self) -> bool {
        true
    }
    fn parse(
        &self,
        node: &markdown_ast::Node,
        context: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        fn unsafe_resource(node: &markdown_ast::Node) -> bool {
            matches!(
                node,
                markdown_ast::Node::Html(_)
                    | markdown_ast::Node::Image(_)
                    | markdown_ast::Node::ImageReference(_)
            ) || node
                .children()
                .is_some_and(|children| children.iter().any(unsafe_resource))
        }
        if matches!(node, markdown_ast::Node::Root(_)) || !unsafe_resource(node) {
            return None;
        }
        let text = context.node_source(node).unwrap_or_default().to_owned();
        Some(
            MarkdownNode::new(self.name(), ())
                .text(text.clone())
                .markdown(text),
        )
    }
    fn render(&self, node: &MarkdownNode, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().whitespace_normal().child(node.as_text().to_owned())
    }
}
pub fn markdown_text(text: String) -> impl IntoElement {
    markdown(text)
        .plugin(InertResources)
        .selectable(true)
        .on_link_click(|url, event, _, cx| {
            if !event.is_right_click()
                && (url.starts_with("https://") || url.starts_with("http://"))
            {
                cx.open_url(url);
            }
        })
}
pub fn image_thumbnail(
    image: &ImageAttachment,
    owner: &WeakEntity<Workspace>,
    _cx: &App,
) -> AnyElement {
    let preview = image.clone();
    let owner = owner.clone();
    let open_preview = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
        cx.stop_propagation();
        let _ = owner.update(cx, |workspace, cx| {
            workspace.image_preview = Some(preview.clone());
            workspace.sync_detail_surface(window, cx);
            cx.notify();
        });
    };
    Attachment::new()
        .id(SharedString::from(format!("image-{}", image.id)))
        .media(
            AttachmentMedia::new().w(rems(8.)).h(rems(6.)).child(
                img(image.path.clone())
                    .w_full()
                    .h_full()
                    .object_fit(ObjectFit::Contain),
            ),
        )
        .content(
            AttachmentContent::new()
                .title(AttachmentTitle::new(format!(
                    "{} × {}",
                    image.width, image.height
                )))
                .description(AttachmentDescription::new(format!(
                    "{}{}",
                    image.mime,
                    if image.original.is_some() {
                        " · prepared"
                    } else {
                        ""
                    }
                ))),
        )
        .on_click(open_preview.clone())
        .actions(
            AttachmentActions::new().child(
                Button::new(SharedString::from(format!("preview-image-{}", image.id)))
                    .small()
                    .ghost()
                    .label("Preview")
                    .tooltip("Preview image")
                    .on_click(open_preview),
            ),
        )
        .into_any_element()
}
/// Flip a disclosure key in the workspace's `expanded` set.
fn toggle(
    owner: &WeakEntity<Workspace>,
    key: String,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + use<> {
    let owner = owner.clone();
    move |_, _, cx| {
        cx.stop_propagation();
        let _ = owner.update(cx, |workspace, cx| {
            if !workspace.expanded.remove(&key) {
                workspace.expanded.insert(key.clone());
            }
            cx.notify();
        });
    }
}
fn is_expanded(owner: &WeakEntity<Workspace>, key: &str, cx: &App) -> bool {
    owner
        .upgrade()
        .is_some_and(|workspace| workspace.read(cx).expanded.contains(key))
}
/// The one chevron control every disclosure in the transcript uses.
fn disclosure(id: String, expanded: bool, owner: &WeakEntity<Workspace>, tooltip: &str) -> Button {
    Button::new(SharedString::from(id.clone()))
        .xsmall()
        .ghost()
        .cursor_default()
        .icon(if expanded {
            Lucide::ChevronDown
        } else {
            Lucide::ChevronRight
        })
        .tooltip(tooltip.to_owned())
        .on_click(toggle(owner, id))
}
fn mono(cx: &App) -> Div {
    div()
        .min_w_0()
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(cx.theme().mono_font_size)
        .whitespace_normal()
}

pub(super) fn block(
    content: &ContentBlock,
    identity: String,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let expanded = is_expanded(owner, &identity, cx);
    match content {
        ContentBlock::Text(text) if text.len() <= 16_384 => {
            markdown_text(text.clone()).into_any_element()
        }
        ContentBlock::Text(text) => {
            let key = identity.clone();
            v_flex()
                .gap_2()
                .child(
                    Button::new(SharedString::from(identity))
                        .small()
                        .ghost()
                        .label(format!(
                            "{} large result ({} bytes)",
                            if expanded { "Collapse" } else { "Expand" },
                            text.len()
                        ))
                        .on_click(toggle(owner, key)),
                )
                .when(expanded, |view| view.child(markdown_text(text.clone())))
                .into_any_element()
        }
        ContentBlock::Thinking(text) => {
            let key = identity.clone();
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(disclosure(
                            key.clone(),
                            expanded,
                            owner,
                            if expanded {
                                "Hide thinking"
                            } else {
                                "Show thinking"
                            },
                        ))
                        .child("Thinking"),
                )
                .child(
                    Collapsible::new()
                        .open(expanded)
                        .motion_id(SharedString::from(format!("{key}:reveal")))
                        .content(
                            div()
                                .pl_6()
                                .text_color(cx.theme().muted_foreground)
                                .child(markdown_text(text.clone())),
                        ),
                )
                .into_any_element()
        }
        ContentBlock::Image(image) => image_thumbnail(image, owner, cx),
        ContentBlock::ImageError(error) => Alert::error(
            SharedString::from(identity),
            format!("Image unavailable: {error}"),
        )
        .small()
        .into_any_element(),
        // Outside a conversation there is no result to attach; show the call alone.
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
            cx,
        ),
    }
}

/// What a tool card says about its call, derived from the tool name and input only.
#[derive(Debug, PartialEq, Eq)]
pub struct ToolSummary {
    pub icon: Lucide,
    pub body: String,
}
const SUMMARY_LIMIT: usize = 200;
fn text_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input[key].as_str().filter(|value| !value.trim().is_empty())
}
fn compact_json(input: &Value) -> String {
    let text = match input {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    if text.chars().count() > SUMMARY_LIMIT {
        let cut: String = text.chars().take(SUMMARY_LIMIT).collect();
        format!("{cut}…")
    } else {
        text
    }
}
/// Files named in an `edit` patch header line such as `[path/to/file.rs#69AE]`.
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
    let (icon, body) = match name {
        "bash" => (
            Lucide::Terminal,
            text_field(input, "command").map(str::to_owned),
        ),
        "read" => (
            Lucide::FileText,
            text_field(input, "path").map(str::to_owned),
        ),
        "write" => (
            Lucide::FilePlus,
            text_field(input, "path").map(str::to_owned),
        ),
        "edit" => (
            Lucide::FilePen,
            text_field(input, "input")
                .map(patch_paths)
                .filter(|paths| !paths.is_empty())
                .map(|paths| paths.join("\n"))
                .or_else(|| text_field(input, "path").map(str::to_owned)),
        ),
        "grep" => (
            Lucide::Search,
            text_field(input, "pattern").map(|pattern| match text_field(input, "path") {
                Some(path) => format!("{pattern}  in {path}"),
                None => pattern.to_owned(),
            }),
        ),
        "glob" => (
            Lucide::FolderSearch,
            text_field(input, "pattern")
                .or_else(|| text_field(input, "path"))
                .map(str::to_owned),
        ),
        "todo" => (
            Lucide::ListTodo,
            text_field(input, "op").map(|op| {
                match text_field(input, "task").or_else(|| text_field(input, "phase")) {
                    Some(subject) => format!("{op}  {subject}"),
                    None => op.to_owned(),
                }
            }),
        ),
        "task" => (
            Lucide::Workflow,
            input["tasks"].as_array().map(|tasks| match tasks.len() {
                1 => "1 task".to_owned(),
                n => format!("{n} tasks"),
            }),
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
        "web_search" => (Lucide::Globe, text_field(input, "query").map(str::to_owned)),
        _ => (Lucide::Wrench, None),
    };
    ToolSummary {
        icon,
        body: body.unwrap_or_else(|| compact_json(input)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolOutcome {
    Running,
    Succeeded,
    Failed,
    /// No result was recorded and nothing is running; DROSTJE. does not guess.
    Unknown,
}

/// The call half of a tool card, taken from the assistant message.
#[derive(Clone, Copy)]
struct ToolCall<'a> {
    name: &'a str,
    intent: Option<&'a str>,
    input: &'a Value,
}
/// The result half, taken from live activity or the historical result message.
#[derive(Clone, Copy)]
struct ToolResult<'a> {
    outcome: ToolOutcome,
    content: &'a [ContentBlock],
    exit_code: Option<i64>,
}
impl ToolResult<'_> {
    fn recorded(error: bool, exit_code: Option<i64>) -> ToolOutcome {
        if error || exit_code.is_some_and(|code| code != 0) {
            ToolOutcome::Failed
        } else {
            ToolOutcome::Succeeded
        }
    }
}

/// One card for a tool call, live or historical. A failed card opens its result
/// automatically, otherwise the disclosure does.
fn tool_card(
    key: String,
    call: ToolCall<'_>,
    result: ToolResult<'_>,
    owner: &WeakEntity<Workspace>,
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
    let summary = tool_summary(name, input);
    let has_result = !result.is_empty() || exit_code.is_some();
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
    let mut header = h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .child(
            Icon::new(summary.icon)
                .xsmall()
                .text_color(cx.theme().muted_foreground),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(caption),
        );
    if intent.is_some() {
        header = header.child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(name.to_owned()),
        );
    }
    header = header.when_some(mark, |row, mark| row.child(mark));
    if has_result && outcome != ToolOutcome::Failed {
        header = header.child(disclosure(
            key.clone(),
            expanded,
            owner,
            if expanded {
                "Hide result"
            } else {
                "Show result"
            },
        ));
    }
    let mut card = v_flex()
        .id(SharedString::from(format!("{key}:card")))
        .w_full()
        .min_w_0()
        .gap_1()
        .p_3()
        .rounded(cx.theme().radius)
        .bg(cx.theme().muted)
        .child(header)
        .child(mono(cx).child(summary.body));
    if has_result {
        card = card.on_click(toggle(owner, key.clone()));
    }
    if has_result && expanded {
        let mut output = v_flex()
            .gap_2()
            .pt_2()
            .mt_1()
            .border_t_1()
            .border_color(cx.theme().border);
        if let Some(code) = exit_code.filter(|code| *code != 0 || outcome == ToolOutcome::Failed) {
            output = output.child(mono(cx).child(format!("Exit code {code}")));
        }
        for (index, block) in result.iter().enumerate() {
            output = output.child(self::block(block, format!("{key}:{index}"), owner, cx));
        }
        card = card.child(output);
    }
    card.into_any_element()
}

fn tool_call_row(
    thread: ThreadId,
    call_id: &str,
    call: ToolCall<'_>,
    session: &ConversationSession,
    active: bool,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let key = format!("tool:{thread}:{call_id}");
    let pending = if active {
        ToolOutcome::Running
    } else {
        ToolOutcome::Unknown
    };
    if let Some(tool) = session.tools.get(call_id) {
        let outcome = if tool.error || tool.exit_code.is_some_and(|code| code != 0) {
            ToolOutcome::Failed
        } else if tool.complete {
            ToolOutcome::Succeeded
        } else {
            pending
        };
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
            cx,
        );
    }
    let result = match session.tool_result(call_id) {
        Some(result) => ToolResult {
            outcome: ToolResult::recorded(result.error, result.exit_code),
            content: &result.content,
            exit_code: result.exit_code,
        },
        None => ToolResult {
            outcome: pending,
            content: &[],
            exit_code: None,
        },
    };
    tool_card(key, call, result, owner, cx)
}

fn message(
    message: Arc<Message>,
    id: ThreadId,
    session: &ConversationSession,
    active: bool,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    // A tool result answers a call rendered earlier; that card shows its content.
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
            cx,
        ));
    }
    let user_message = message.role == "user";
    let raw_key = format!("raw:{id}:{}", message.id);
    let raw_expanded = message.raw.is_some() && is_expanded(owner, &raw_key, cx);
    let group: SharedString = format!("message-group-{id}-{}", message.id).into();
    let copy_message = message.clone();
    // Actions stay out of the way until the pointer is over the message; each
    // one is an icon button with its own tooltip rather than a hidden menu.
    let mut actions = h_flex()
        .w_full()
        .justify_end()
        .gap_1()
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
            .on_click(toggle(owner, raw_key)),
        );
    }
    let mut body = v_flex().w_full().min_w_0().gap_2();
    let mut images = h_flex().flex_wrap().gap_2();
    let mut has_images = false;
    for (part, item) in message.content.iter().enumerate() {
        let identity = format!("{id}:{}:{part}", message.id);
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
                    cx,
                ));
            }
            ContentBlock::Image(_) if user_message => {
                has_images = true;
                images = images.child(block(item, identity, owner, cx));
            }
            _ => body = body.child(block(item, identity, owner, cx)),
        }
    }
    if has_images {
        body = body.child(images);
    }
    if let Some(raw) = message.raw.as_ref().filter(|_| raw_expanded) {
        body = body.child(mono(cx).child(raw.to_string()));
    }
    let body = if user_message {
        body.p_3()
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
            .py_2()
            .child(actions)
            .child(body),
    )
}

fn request_card(
    id: ThreadId,
    request: &UserRequest,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let mut card = v_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .p_3()
        .border_1()
        .border_color(cx.theme().ring)
        .rounded(cx.theme().radius)
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(request.title.clone()),
        );
    if !request.text.is_empty() {
        card = card.child(div().text_sm().child(request.text.clone()));
    }
    let mut choices = v_flex().w_full().gap_1();
    let mut answers: Vec<(String, Option<String>, UserAnswer)> = Vec::new();
    match request.kind {
        UserRequestKind::Confirm => {
            answers.push(("Confirm".into(), None, UserAnswer::Confirmed(true)));
            answers.push(("Decline".into(), None, UserAnswer::Confirmed(false)));
        }
        UserRequestKind::Select => {
            for option in &request.options {
                answers.push((
                    option.value.clone(),
                    option.description.clone(),
                    UserAnswer::Value(option.value.clone()),
                ));
            }
        }
        UserRequestKind::Input | UserRequestKind::Editor => {
            let owner = owner.clone();
            let request = request.clone();
            choices = choices.child(
                Button::new(SharedString::from(format!("answer-{}", request.id)))
                    .small()
                    .outline()
                    .w_full()
                    .label("Answer…")
                    .on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.open_extension(id, request.clone(), window, cx)
                        });
                    }),
            );
        }
    }
    for (index, (label, description, answer)) in answers.into_iter().enumerate() {
        let owner = owner.clone();
        let request_id = request.id.clone();
        let mut label = Label::new(label);
        if let Some(description) = description {
            label = label.secondary(description);
        }
        choices = choices.child(
            Button::new(SharedString::from(format!(
                "request-{}-{index}",
                request.id
            )))
            .small()
            .outline()
            .w_full()
            .child(label)
            .on_click(move |_, _, cx| {
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.state.update(cx, |state, cx| {
                        state.respond(id, request_id.clone(), answer.clone());
                        cx.notify();
                    })
                });
            }),
        );
    }
    let cancel = owner.clone();
    let request_id = request.id.clone();
    card.child(choices)
        .child(
            h_flex().justify_end().child(
                Button::new(SharedString::from(format!("request-{}-cancel", request.id)))
                    .small()
                    .ghost()
                    .label("Cancel")
                    .on_click(move |_, _, cx| {
                        let _ = cancel.update(cx, |workspace, cx| {
                            workspace.state.update(cx, |state, cx| {
                                state.respond(
                                    id,
                                    request_id.clone(),
                                    UserAnswer::Cancelled { timed_out: false },
                                );
                                cx.notify();
                            })
                        });
                    }),
            ),
        )
        .into_any_element()
}

/// The centre pane while no thread is open: name the next step and offer it.
pub fn welcome(state: &AppState, owner: &WeakEntity<Workspace>, cx: &App) -> AnyElement {
    let has_project = state.project().is_some();
    let actions = h_flex()
        .gap_2()
        .flex_wrap()
        .justify_center()
        .when(has_project, |row| {
            row.child(
                button(
                    "empty-new-conversation",
                    Command::NewConversation,
                    owner,
                    state,
                )
                .outline()
                .debug_selector(|| "empty-new-conversation".into()),
            )
            .child(
                button("empty-new-factory", Command::NewFactoryThread, owner, state)
                    .outline()
                    .debug_selector(|| "empty-new-factory".into()),
            )
        })
        .when(!has_project, |row| {
            row.child(
                button("empty-add-project", Command::AddProject, owner, state)
                    .outline()
                    .debug_selector(|| "empty-add-project".into()),
            )
        });
    v_flex()
        .id("conversation-welcome")
        .size_full()
        .min_w_0()
        .min_h_0()
        .text_color(cx.theme().foreground)
        .items_center()
        .justify_center()
        .p_6()
        .child(
            v_flex()
                .w_full()
                .max_w(rems(30.))
                .items_center()
                .gap_3()
                .text_center()
                .child(div().text_lg().child(if has_project {
                    "No thread selected"
                } else {
                    "Open a project"
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if has_project {
                            "Choose a thread from Projects, or start a new one."
                        } else {
                            "Add a Git working tree to start a Conversation or Factory thread."
                        }),
                )
                .child(actions),
        )
        .into_any_element()
}

pub fn render(
    state: &AppState,
    id: ThreadId,
    owner: &WeakEntity<Workspace>,
    _scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    let drop_owner = owner.clone();
    let mut root = v_flex()
        .id(SharedString::from(format!("conversation-view-{id}")))
        .size_full()
        .min_w_0()
        .min_h_0()
        .text_color(cx.theme().foreground)
        .gap_2()
        .on_drop(move |paths: &ExternalPaths, window, cx| {
            let _ = drop_owner.update(cx, |workspace, cx| {
                if workspace.editor_override.is_none()
                    && workspace.state.read(cx).selected_thread == Some(id)
                {
                    workspace.import_images(id, paths.paths().to_vec(), window, cx);
                }
            });
        });
    let Some(thread) = state.snapshot.threads.iter().find(|thread| thread.id == id) else {
        return root
            .child(div().p_3().child("This thread is no longer available."))
            .into_any_element();
    };
    let Some(project) = state
        .snapshot
        .projects
        .iter()
        .find(|project| project.id == thread.project_id)
    else {
        return root
            .child(
                div()
                    .p_3()
                    .child("This thread's project is no longer available."),
            )
            .into_any_element();
    };
    let active = state.active_threads.contains(&id);
    if let Some(Err(error)) = state.repositories.get(&project.id) {
        root = root.child(
            Alert::error(
                "conversation-root-error",
                format!("Root unavailable: {error}"),
            )
            .small(),
        );
    }
    let mut statuses: Vec<String> = Vec::new();
    if let Some(session) = state.conversations.get(&id) {
        if let Some(error) = &session.error {
            root = root.child(inset(
                Alert::error("conversation-session-error", error.clone()).small(),
            ));
        }
        statuses = session.statuses.values().cloned().collect();
        let state_entity = owner.upgrade().map(|owner| owner.read(cx).state.clone());
        let history_owner = owner.clone();
        root = root.child(
            list(session.list.clone(), move |index, _, cx| {
                let cx: &App = cx;
                let Some(state) = state_entity.as_ref().map(|state| state.read(cx)) else {
                    return div().into_any_element();
                };
                let Some(session) = state.conversations.get(&id) else {
                    return div().into_any_element();
                };
                let active = state.active_threads.contains(&id);
                session
                    .messages
                    .get(index)
                    .cloned()
                    .map(|current| message(current, id, session, active, &history_owner, cx))
                    .unwrap_or_else(|| div().into_any_element())
            })
            .flex_1()
            .min_h(rems(4.)),
        );
        for request in session.requests.values() {
            root = root.child(inset(request_card(id, request, owner, cx)));
        }
        for (index, (key, lines)) in session.widgets.iter().enumerate() {
            if key.starts_with("Proposed editor replacement") {
                let owner = owner.clone();
                let text = lines.join("\n");
                root = root.child(inset(
                    Button::new(("apply-editor-proposal", index))
                        .small()
                        .ghost()
                        .label("Review replacement…")
                        .on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |workspace, cx| {
                                workspace.replace_draft_reviewed(
                                    id,
                                    text.clone(),
                                    None,
                                    false,
                                    window,
                                    cx,
                                )
                            });
                        }),
                ));
                continue;
            }
            // Widgets are OMP's live panels (plans, progress); open unless folded away.
            let folded_key = format!("widget-folded:{id}:{key}");
            let folded = is_expanded(owner, &folded_key, cx);
            root = root.child(inset(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(disclosure(
                                folded_key,
                                !folded,
                                owner,
                                if folded { "Show widget" } else { "Hide widget" },
                            ))
                            .child(key.clone()),
                    )
                    .child(
                        Collapsible::new()
                            .open(!folded)
                            .content(mono(cx).pl_6().child(lines.join("\n"))),
                    ),
            ));
        }
    } else {
        root = root.child(div().flex_1().p_4().child(if thread.archived {
            "Archived conversation has no provider history."
        } else {
            "Opening native OMP session…"
        }));
    }
    // A submission that is simply in flight is already visible through the
    // disabled Send control and the sidebar state; only an unresolved one after
    // a failure or restart needs attention here.
    let unresolved_submission = thread.submission.as_ref().filter(|submission| {
        let submission_state = submission
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        !active || !matches!(submission_state, "pending" | "accepted")
    });
    if let Some(submission) = unresolved_submission {
        let restore = owner.clone();
        let dismiss = owner.clone();
        let submission_state = submission
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let detail = submission
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map(|error| format!(" {error}."))
            .unwrap_or_default();
        root = root.child(inset(
            v_flex()
                .gap_1()
                .child(
                    Alert::warning(
                        "unresolved-submission",
                        format!("Submission {submission_state}.{detail} The draft was not resent."),
                    )
                    .small(),
                )
                .when(!active, |view| {
                    view.child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("restore-submitted")
                                    .small()
                                    .ghost()
                                    .label("Restore submitted draft…")
                                    .on_click(move |_, window, cx| {
                                        let _ = restore.update(cx, |workspace, cx| {
                                            workspace.restore_submitted(id, window, cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("dismiss-submitted")
                                    .small()
                                    .ghost()
                                    .label("Keep current draft")
                                    .on_click(move |_, _, cx| {
                                        let _ = dismiss.update(cx, |workspace, cx| {
                                            workspace.state.update(cx, |state, cx| {
                                                state.acknowledge_recovery(id);
                                                cx.notify();
                                            })
                                        });
                                    }),
                            ),
                    )
                }),
        ));
    }
    if thread.archived || project.archived {
        return root
            .child(inset(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Archived history · read-only"),
            ))
            .into_any_element();
    }
    // One region under a single divider: attachments, the editor and its
    // toolbar stack without a box of their own.
    let mut composer = v_flex()
        .id("composer-content")
        .debug_selector(|| "composer-content".into())
        .min_h_0();
    let mut previews = h_flex()
        .id("draft-attachments")
        .overflow_x_scroll()
        .gap_3()
        .px_2()
        .pt_2();
    for image in &thread.attachments {
        let remove = owner.clone();
        let attachment_id = image.id;
        previews = previews.child(
            v_flex()
                .gap_1()
                .child(image_thumbnail(image, owner, cx))
                .child(
                    Button::new(SharedString::from(format!("remove-{attachment_id}")))
                        .small()
                        .ghost()
                        .label("Remove")
                        .on_click(move |_, _, cx| {
                            let _ = remove.update(cx, |workspace, cx| {
                                workspace.state.update(cx, |state, cx| {
                                    if let Some(mut thread) =
                                        state.snapshot.threads.iter().find(|t| t.id == id).cloned()
                                    {
                                        thread.attachments.retain(|a| a.id != attachment_id);
                                        state.save_thread(thread);
                                    }
                                    cx.notify();
                                })
                            });
                        }),
                ),
        );
    }
    if !thread.attachments.is_empty() {
        composer = composer.child(previews);
    }
    // One Neovim instance: the selected conversation hosts it, every other visible
    // conversation shows its saved draft and how to take the editor over.
    let hosts_editor = owner.upgrade().is_some_and(|workspace| {
        let workspace = workspace.read(cx);
        workspace.editor.is_some()
            && workspace.editor_override.is_none()
            && workspace.editor_selection == Some(crate::model::DraftKey::Thread(id))
    });
    let editor = owner
        .upgrade()
        .and_then(|workspace| workspace.read(cx).editor.clone())
        .filter(|_| hosts_editor);
    composer = composer.child(match editor {
        Some(editor) => div()
            .h(rems(8.))
            .min_h(rems(2.))
            .flex_shrink(1.)
            .child(editor)
            .into_any_element(),
        None => div()
            .id(SharedString::from(format!("draft-preview-{id}")))
            .h(rems(8.))
            .min_h(rems(2.))
            .flex_shrink(1.)
            .overflow_y_scroll()
            .px_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .whitespace_normal()
            .child(if thread.draft.trim().is_empty() {
                "No draft · press i to write here".to_owned()
            } else {
                format!("{}\n\nPress i to edit", thread.draft)
            })
            .into_any_element(),
    });
    let mut controls = h_flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .px_2()
        .py_1();
    let attach = owner.clone();
    let menu_owner = owner.clone();
    let no_images = thread.attachments.is_empty();
    controls = controls
        .child(
            Button::new("attach-images")
                .small()
                .ghost()
                .icon(Lucide::Paperclip)
                .tooltip("Attach images…")
                .disabled(state.storage_error.is_some())
                .on_click(move |_, window, cx| {
                    let _ = attach.update(cx, |workspace, cx| workspace.attach_images(window, cx));
                }),
        )
        .child(
            Button::new("composer-actions")
                .small()
                .ghost()
                .icon(IconName::Ellipsis)
                .tooltip("Conversation actions")
                .dropdown_menu(move |menu, _, _| {
                    let details = menu_owner.clone();
                    let optimize = menu_owner.clone();
                    let recovery = menu_owner.clone();
                    menu.item(PopupMenuItem::new("Thread details").on_click(
                        move |_, window, cx| {
                            let _ = details.update(cx, |workspace, cx| {
                                workspace.toggle_thread_details(id, window, cx)
                            });
                        },
                    ))
                    .item(
                        PopupMenuItem::new("Optimize images")
                            .disabled(no_images)
                            .on_click(move |_, window, cx| {
                                let _ = optimize.update(cx, |workspace, cx| {
                                    workspace.optimize_images(window, cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Copy recovery").on_click(move |_, _, cx| {
                            let _ = recovery
                                .update(cx, |workspace, cx| workspace.copy_draft_recovery(cx));
                        }),
                    )
                }),
        );
    // OMP's status bar (context use, mode, …) sits where Zed shows its mode text.
    controls = controls.child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_xs()
            .text_align(TextAlign::Right)
            .text_color(cx.theme().muted_foreground)
            .child(statuses.join(" · ")),
    );
    if active {
        controls = controls.child(Spinner::new().xsmall());
    }
    if let Some(workspace) = owner.upgrade() {
        if let Some(picker) = workspace
            .read(cx)
            .model_picker
            .as_ref()
            .filter(|picker| picker.thread_id == id)
        {
            if !picker.options.is_empty() {
                controls = controls.child(
                    div()
                        .min_w_0()
                        .max_w(rems(20.))
                        .key_context("NativeInput")
                        .child(
                            Select::new(&picker.select)
                                .small()
                                .disabled(active)
                                .placeholder("Select model"),
                        ),
                );
            }
        }
    }
    let unavailable = state
        .conversations
        .get(&id)
        .is_none_or(|session| session.models.is_empty() || session.error.is_some());
    if unavailable {
        let retry = owner.clone();
        controls = controls.child(
            Button::new("retry-model-discovery")
                .small()
                .ghost()
                .label("Models unavailable · Retry")
                .disabled(active)
                .on_click(move |_, _, cx| {
                    let _ = retry.update(cx, |workspace, cx| {
                        workspace.state.update(cx, |state, cx| {
                            state.retry_conversation(id);
                            cx.notify();
                        });
                    });
                }),
        );
    }
    if active {
        let stop = owner.clone();
        controls = controls.child(
            Button::new("interrupt-prompt")
                .small()
                .danger()
                .outline()
                .icon(Lucide::Square)
                .label("Stop")
                .tooltip("Interrupt turn")
                .on_click(move |_, _, cx| {
                    let _ = stop.update(cx, |workspace, cx| {
                        workspace.state.update(cx, |state, cx| {
                            state.interrupt(id);
                            cx.notify();
                        });
                    });
                }),
        );
    } else {
        let send = owner.clone();
        let reason = state.submit_reason(id).or_else(|| {
            (!hosts_editor).then(|| "Focus this conversation to edit and send".to_owned())
        });
        controls = controls.child(
            Button::new("send-prompt")
                .small()
                .primary()
                .icon(Lucide::ArrowUp)
                .label("Send")
                .disabled(reason.is_some())
                .when_some(reason, |button, reason| button.tooltip(reason))
                .on_click(move |_, window, cx| {
                    let _ = send.update(cx, |workspace, cx| workspace.submit_prompt(window, cx));
                }),
        );
    }
    // A definite bounded height propagates through the inset wrapper so the editor shrinks,
    // rather than overflowing the actions or introducing a second transcript scroller.
    let preferred_height = 11.
        + if thread.attachments.is_empty() {
            0.
        } else {
            9.
        };
    root.child(
        h_flex()
            .w_full()
            .justify_center()
            .h(rems(preferred_height))
            .max_h(relative(0.5))
            .min_h_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .pt_2()
            .pb_2()
            .child(
                content_column()
                    .h_full()
                    .min_h_0()
                    .child(composer.h_full().child(controls)),
            ),
    )
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Lucide, SUMMARY_LIMIT, ToolSummary, Value, tool_summary};
    use serde_json::json;

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
        let unknown = tool_summary("hub", &json!({"op":"cancel","ids":["bg_7"]}));
        assert_eq!(unknown.icon, Lucide::Wrench);
        assert_eq!(unknown.body, r#"{"op":"cancel","ids":["bg_7"]}"#);
        // Missing fields never panic and never show an empty card body silently.
        assert_eq!(tool_summary("bash", &Value::Null).body, "");
        let long = tool_summary("other", &json!({"text": "x".repeat(500)}));
        assert!(long.body.ends_with('…'));
        assert_eq!(long.body.chars().count(), SUMMARY_LIMIT + 1);
    }
}
