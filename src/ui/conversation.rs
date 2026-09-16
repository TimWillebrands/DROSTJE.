use super::{
    commands::{Command, button},
    workspace::Workspace,
};
use crate::{
    app::AppState,
    harness::{ContentBlock, Message, UserAnswer, UserRequestKind},
    model::{ImageAttachment, ModelKey, ThreadId},
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, IconName, Sizable,
        alert::Alert,
        attachment::{
            Attachment, AttachmentActions, AttachmentContent, AttachmentDescription,
            AttachmentMedia, AttachmentTitle,
        },
        bubble::{Bubble, BubbleVariant},
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        message::{
            Message as MessageView, MessageAlignment, MessageContent, MessageFooter, MessageHeader,
        },
        searchable_list::SearchableListItem,
        select::{Select, SelectEvent, SelectState},
        text::{MarkdownNode, MarkdownParseContext, MarkdownPlugin, markdown, markdown_ast},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
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
        "drost-inert-resources"
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
pub(super) fn block(
    content: &ContentBlock,
    identity: String,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let expanded = owner
        .upgrade()
        .is_some_and(|workspace| workspace.read(cx).expanded.contains(&identity));
    match content {
        ContentBlock::Text(text) if text.len() <= 16_384 => {
            markdown_text(text.clone()).into_any_element()
        }
        ContentBlock::Text(text) | ContentBlock::Thinking(text) => {
            let key = identity.clone();
            let owner = owner.clone();
            v_flex()
                .gap_2()
                .child(
                    Button::new(SharedString::from(identity))
                        .small()
                        .ghost()
                        .label(format!(
                            "{} {} ({} bytes)",
                            if expanded { "Collapse" } else { "Expand" },
                            if matches!(content, ContentBlock::Thinking(_)) {
                                "thinking"
                            } else {
                                "large result"
                            },
                            text.len()
                        ))
                        .on_click(move |_, _, cx| {
                            let _ = owner.update(cx, |workspace, cx| {
                                if !workspace.expanded.remove(&key) {
                                    workspace.expanded.insert(key.clone());
                                }
                                cx.notify();
                            });
                        }),
                )
                .when(expanded, |view| view.child(markdown_text(text.clone())))
                .into_any_element()
        }
        ContentBlock::Image(image) => image_thumbnail(image, owner, cx),
        ContentBlock::ImageError(error) => Alert::error(
            SharedString::from(identity),
            format!("Image unavailable: {error}"),
        )
        .small()
        .into_any_element(),
    }
}
fn message(
    message: Arc<Message>,
    id: ThreadId,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let copy_message = message.clone();
    let has_raw = message.raw.is_some();
    let raw_key = format!("raw:{id}:{}", message.id);
    let expanded = owner
        .upgrade()
        .is_some_and(|owner| owner.read(cx).expanded.contains(&raw_key));
    let user_message = message.role == "user";
    let tool_result = message.role == "toolResult";
    let result_key = format!("tool-result:{id}:{}", message.id);
    let show_result = !tool_result
        || owner
            .upgrade()
            .is_some_and(|owner| owner.read(cx).expanded.contains(&result_key));
    let menu_owner = owner.clone();
    let actions = Button::new(SharedString::from(format!(
        "message-actions-{id}-{}",
        message.id
    )))
    .small()
    .ghost()
    .icon(IconName::Ellipsis)
    .tooltip("Message actions")
    .dropdown_menu(move |menu, _, _| {
        let copy_message = copy_message.clone();
        let owner = menu_owner.clone();
        let key = raw_key.clone();
        let result_owner = menu_owner.clone();
        let result_key = result_key.clone();
        menu.item(PopupMenuItem::new("Copy").on_click(move |_, _, cx| {
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
        }))
        .when(tool_result, |menu| {
            menu.item(
                PopupMenuItem::new(if show_result {
                    "Collapse tool result"
                } else {
                    "Show tool result"
                })
                .on_click(move |_, _, cx| {
                    let _ = result_owner.update(cx, |workspace, cx| {
                        if !workspace.expanded.remove(&result_key) {
                            workspace.expanded.insert(result_key.clone());
                        }
                        cx.notify();
                    });
                }),
            )
        })
        .when(has_raw, |menu| {
            menu.item(
                PopupMenuItem::new("Diagnostic payload").on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |workspace, cx| {
                        if !workspace.expanded.remove(&key) {
                            workspace.expanded.insert(key.clone());
                        }
                        cx.notify();
                    });
                }),
            )
        })
    });
    let mut content = MessageContent::new();
    if show_result {
        for (part, item) in message.content.iter().enumerate() {
            let rendered = block(item, format!("{id}:{}:{part}", message.id), owner, cx);
            content = if user_message && matches!(item, ContentBlock::Text(_)) {
                content.bubble(
                    Bubble::new()
                        .with_variant(BubbleVariant::Secondary)
                        .child(rendered),
                )
            } else if matches!(item, ContentBlock::Image(_)) {
                content.child(rendered)
            } else {
                content.child(div().w_full().min_w_0().child(rendered))
            };
        }
    }
    if expanded {
        if let Some(raw) = &message.raw {
            content = content.child(
                div()
                    .min_w_0()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .whitespace_normal()
                    .child(raw.to_string()),
            );
        }
    }
    inset(
        div()
            .w_full()
            .min_w_0()
            .id(SharedString::from(format!("message-{id}-{}", message.id)))
            .child(
                MessageView::new()
                    .py_3()
                    .alignment(if user_message {
                        MessageAlignment::End
                    } else {
                        MessageAlignment::Start
                    })
                    .header(
                        MessageHeader::new()
                            .content_inset(user_message)
                            .child(message.role.clone()),
                    )
                    .content(content)
                    .footer(
                        MessageFooter::new()
                            .content_inset(user_message)
                            .child(actions),
                    ),
            ),
    )
}

pub fn render(
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    _scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    let drop_owner = owner.clone();
    let mut root = v_flex()
        .id("conversation-view")
        .size_full()
        .min_w_0()
        .min_h_0()
        .text_color(cx.theme().foreground)
        .gap_2()
        .on_drop(move |paths: &ExternalPaths, window, cx| {
            let _ = drop_owner.update(cx, |workspace, cx| {
                if workspace.editor_override.is_none() {
                    if let Some(id) = workspace
                        .state
                        .read(cx)
                        .conversation_thread()
                        .map(|thread| thread.id)
                    {
                        workspace.import_images(id, paths.paths().to_vec(), window, cx);
                    }
                }
            });
        });
    let (Some(project), Some(thread)) = (state.project(), state.conversation_thread()) else {
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
        return root
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
            .into_any_element();
    };
    let id = thread.id;
    if let Some(Err(error)) = state.repositories.get(&project.id) {
        root = root.child(
            Alert::error(
                "conversation-root-error",
                format!("Root unavailable: {error}"),
            )
            .small(),
        );
    }
    if let Some(session) = state.conversations.get(&id) {
        if let Some(error) = &session.error {
            root = root.child(inset(
                Alert::error("conversation-session-error", error.clone()).small(),
            ));
        }
        let state_entity = owner.upgrade().map(|owner| owner.read(cx).state.clone());
        let history_owner = owner.clone();
        root = root.child(
            list(session.list.clone(), move |index, _, cx| {
                let current: Option<Arc<Message>> = state_entity
                    .as_ref()
                    .and_then(|state| state.read(cx).conversations.get(&id))
                    .and_then(|session| session.messages.get(index))
                    .cloned();
                current
                    .map(|current| message(current, id, &history_owner, cx))
                    .unwrap_or_else(|| div().into_any_element())
            })
            .flex_1()
            .min_h(rems(4.)),
        );
        // Finished activity lives in Thread details; only outstanding tools take space here.
        for tool in session.tools.values().filter(|tool| !tool.complete) {
            let tool_id = tool.id.clone();
            let tool_owner = owner.clone();
            root = root.child(inset(
                Button::new(SharedString::from(format!("active-tool-{}", tool.id)))
                    .small()
                    .ghost()
                    .label(format!(
                        "{} · {}",
                        tool.name,
                        if tool.error { "Failed" } else { "Running" }
                    ))
                    .on_click(move |_, window, cx| {
                        let _ = tool_owner.update(cx, |workspace, cx| {
                            workspace.tool_detail = Some((id, tool_id.clone()));
                            workspace.sync_detail_surface(window, cx);
                            cx.notify();
                        });
                    }),
            ));
        }
        for request in session.requests.values() {
            let mut row = v_flex()
                .gap_1()
                .p_2()
                .border_1()
                .border_color(cx.theme().ring)
                .rounded(cx.theme().radius)
                .child(request.title.clone());
            if !request.text.is_empty() {
                row = row.child(div().text_xs().child(request.text.clone()));
            }
            let mut choices = h_flex().flex_wrap().gap_1();
            let mut answers = Vec::new();
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
                            .label("Answer…")
                            .on_click(move |_, window, cx| {
                                let _ = owner.update(cx, |workspace, cx| {
                                    workspace.open_extension(id, request.clone(), window, cx)
                                });
                            }),
                    );
                }
            }
            answers.push((
                "Cancel".into(),
                None,
                UserAnswer::Cancelled { timed_out: false },
            ));
            for (index, (label, description, answer)) in answers.into_iter().enumerate() {
                let owner = owner.clone();
                let request_id = request.id.clone();
                choices = choices.child(
                    Button::new(SharedString::from(format!(
                        "request-{}-{index}",
                        request.id
                    )))
                    .small()
                    .ghost()
                    .label(label)
                    .when_some(description, |button, description| {
                        button.tooltip(description)
                    })
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
            root = root.child(inset(row.child(choices)));
        }
        for (index, (key, lines)) in session.widgets.iter().enumerate() {
            if key.starts_with("Proposed editor replacement") {
                let owner = owner.clone();
                let text = lines.join("\n");
                root = root.child(
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
                );
            }
        }
    } else {
        root = root.child(div().flex_1().p_4().child(if thread.archived {
            "Archived conversation has no provider history."
        } else {
            "Opening native OMP session…"
        }));
    }
    if let Some(submission) = &thread.submission {
        let restore = owner.clone();
        let dismiss = owner.clone();
        root = root.child(inset(
            v_flex()
                .gap_1()
                .p_2()
                .border_1()
                .border_color(cx.theme().warning)
                .rounded(cx.theme().radius)
                .child(format!(
                    "Submission: {}. Never automatically resent.",
                    submission
                        .get("state")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                ))
                .when(!state.active_threads.contains(&id), |view| {
                    view.child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("restore-submitted")
                                    .small()
                                    .ghost()
                                    .label("Restore Submitted Draft…")
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
                                    .label("Keep Current Draft; Dismiss Recovery")
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
            .child("Archived history · read-only")
            .into_any_element();
    }
    let mut composer = v_flex()
        .id("composer-content")
        .debug_selector(|| "composer-content".into())
        .min_h_0()
        .gap_2()
        .pb_3();
    let mut previews = h_flex().id("draft-attachments").overflow_x_scroll().gap_3();
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
    if let Some(workspace) = owner.upgrade() {
        if let Some(editor) = workspace.read(cx).editor.clone() {
            if workspace.read(cx).editor_override.is_none()
                && workspace.read(cx).editor_selection == Some(crate::model::DraftKey::Thread(id))
            {
                composer = composer.child(
                    div()
                        .h(rems(8.))
                        .min_h(rems(2.))
                        .flex_shrink(1.)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .child(editor),
                );
            }
        }
    }
    let active = state.active_threads.contains(&id);
    let mut controls = h_flex().flex_wrap().flex_shrink_0().gap_1();
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
                        .max_w(rems(24.))
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
    let attach = owner.clone();
    let menu_owner = owner.clone();
    let no_images = thread.attachments.is_empty();
    controls = controls
        .child(
            Button::new("attach-images")
                .small()
                .ghost()
                .label("Attach…")
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
        )
        .child(div().flex_1());
    if active {
        let stop = owner.clone();
        controls = controls.child(
            Button::new("interrupt-prompt")
                .small()
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
        let reason = state.submit_reason(id);
        controls = controls.child(
            Button::new("send-prompt")
                .small()
                .primary()
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
            .child(
                content_column()
                    .h_full()
                    .min_h_0()
                    .child(composer.h_full().child(controls)),
            ),
    )
    .into_any_element()
}
