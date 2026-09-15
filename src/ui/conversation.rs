use super::{
    commands::{Command, button},
    workspace::Workspace,
};
use crate::{
    app::AppState,
    harness::{ContentBlock, Message, UserAnswer, UserRequestKind},
    model::{ImageAttachment, ThreadId},
};
use gpui_kit::{
    component::{
        Disableable, Selectable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        text::{MarkdownNode, MarkdownParseContext, MarkdownPlugin, markdown, markdown_ast},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::sync::Arc;

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
pub fn image_thumbnail(image: &ImageAttachment, owner: &WeakEntity<Workspace>) -> AnyElement {
    let preview = image.clone();
    let owner = owner.clone();
    v_flex()
        .gap_1()
        .child(
            div()
                .id(SharedString::from(format!("image-{}", image.id)))
                .cursor_pointer()
                .child(
                    img(image.path.clone())
                        .w(px(128.))
                        .h(px(96.))
                        .object_fit(ObjectFit::Contain),
                )
                .on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |workspace, cx| {
                        workspace.image_preview = Some(preview.clone());
                        cx.notify();
                    });
                }),
        )
        .child(div().text_xs().text_color(rgb(0x8B95A5)).child(format!(
            "{} × {} · {}{}",
            image.width,
            image.height,
            image.mime,
            if image.original.is_some() {
                " · prepared"
            } else {
                ""
            }
        )))
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
        ContentBlock::Image(image) => image_thumbnail(image, owner),
        ContentBlock::ImageError(error) => div()
            .text_color(rgb(0xE6A0A0))
            .child(format!("Image unavailable: {error}"))
            .into_any_element(),
    }
}
fn message(
    message: &Message,
    index: usize,
    id: ThreadId,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let text = message
        .content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::Text(text) = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let handoff = owner.clone();
    let copy = text.clone();
    let mut body = v_flex()
        .id(SharedString::from(format!("message-{id}-{}", message.id)))
        .p_3()
        .gap_2()
        .border_b_1()
        .border_color(rgb(0x343A46))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(0x8B95A5))
                        .child(message.role.clone()),
                )
                .child(
                    Button::new(("message-factory", index))
                        .small()
                        .ghost()
                        .label("Send to Factory")
                        .disabled(text.trim().is_empty())
                        .on_click(move |_, window, cx| {
                            let _ = handoff.update(cx, |workspace, cx| {
                                workspace.handoff_message = Some((id, index, text.clone()));
                                workspace.execute(Command::SendToFactory, window, cx);
                            });
                        }),
                )
                .child(
                    Button::new(("message-copy", index))
                        .small()
                        .ghost()
                        .label("Copy")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        }),
                ),
        );
    for (part, content) in message.content.iter().enumerate() {
        body = body.child(block(
            content,
            format!("{id}:{}:{part}", message.id),
            owner,
            cx,
        ));
    }
    if let Some(raw) = &message.raw {
        let key = format!("raw:{id}:{}", message.id);
        let opened = owner
            .upgrade()
            .is_some_and(|owner| owner.read(cx).expanded.contains(&key));
        let click = owner.clone();
        body = body.child(
            Button::new(SharedString::from(key.clone()))
                .small()
                .ghost()
                .label("Diagnostic payload")
                .on_click(move |_, _, cx| {
                    let _ = click.update(cx, |workspace, cx| {
                        if !workspace.expanded.remove(&key) {
                            workspace.expanded.insert(key.clone());
                        }
                        cx.notify();
                    });
                }),
        );
        if opened {
            body = body.child(
                div()
                    .font_family("Menlo")
                    .text_xs()
                    .whitespace_normal()
                    .child(raw.to_string()),
            );
        }
    }
    body.into_any_element()
}

pub fn render(
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    _scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    let mut tabs = h_flex()
        .id("conversation-tabs")
        .overflow_x_scroll()
        .gap_1()
        .border_b_1()
        .border_color(rgb(0x343A46))
        .pb_2();
    for id in &state.open_threads {
        let Some(thread) = state
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == *id)
        else {
            continue;
        };
        let select = owner.clone();
        let close = owner.clone();
        let id = *id;
        tabs = tabs.child(
            h_flex()
                .gap_1()
                .child(
                    Button::new(SharedString::from(format!("session-{id}")))
                        .small()
                        .ghost()
                        .label(thread.title.clone())
                        .selected(state.selected_thread == Some(id))
                        .on_click(move |_, window, cx| {
                            let _ = select.update(cx, |workspace, cx| {
                                workspace.state.update(cx, |state, cx| {
                                    state.select_thread(id);
                                    cx.notify();
                                });
                                workspace.focus_region(
                                    super::workspace::Region::Conversation,
                                    window,
                                    cx,
                                );
                            });
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("close-session-{id}")))
                        .small()
                        .ghost()
                        .label("×")
                        .tooltip("Close idle session; active turns remain reachable in Projects")
                        .on_click(move |_, _, cx| {
                            let _ = close.update(cx, |workspace, cx| {
                                workspace.state.update(cx, |state, cx| {
                                    state.close_thread(id);
                                    cx.notify();
                                })
                            });
                        }),
                ),
        );
    }
    let drop_owner = owner.clone();
    let mut root = v_flex()
        .id("conversation-view")
        .size_full()
        .min_w_0()
        .p_2()
        .gap_2()
        .child(tabs)
        .on_drop(move |paths: &ExternalPaths, window, cx| {
            let _ = drop_owner.update(cx, |workspace, cx| {
                if workspace.editor_override.is_none() {
                    if let Some(id) = workspace.state.read(cx).selected_thread {
                        workspace.import_images(id, paths.paths().to_vec(), window, cx);
                    }
                }
            });
        });
    let Some(project) = state.project() else {
        return root
            .child(
                div()
                    .p_5()
                    .child("Register a Git working tree to start coding."),
            )
            .child(button(
                "empty-add-project",
                Command::AddProject,
                owner,
                state,
            ))
            .into_any_element();
    };
    root = root.child(div().text_xs().text_color(rgb(0x8B95A5)).child(format!(
        "{} · {}",
        project.name,
        project.root.display()
    )));
    let Some(thread) = state.thread() else {
        return root
            .child("Start a conversation or prepare a Factory task")
            .child(button(
                "empty-new-conversation",
                Command::NewConversation,
                owner,
                state,
            ))
            .child(button(
                "empty-send-factory",
                Command::SendToFactory,
                owner,
                state,
            ))
            .into_any_element();
    };
    let id = thread.id;
    root = root.child(h_flex().gap_2().child(div().flex_1().child(thread.title.clone())).child(button("rename-thread", Command::RenameConversation, owner, state)).child(button("archive-thread", Command::ArchiveConversation, owner, state)))
        .child(div().text_xs().text_color(rgb(0x8B95A5)).child("OMP runs with your local user privileges and configured tool/approval policy, not Factory's Docker sandbox."));
    if let Some(Err(error)) = state.repositories.get(&project.id) {
        root = root.child(
            div()
                .text_color(rgb(0xE6A0A0))
                .child(format!("Root unavailable: {error}")),
        );
    }
    for launch in state
        .snapshot
        .launches
        .iter()
        .filter(|launch| launch.thread_id == Some(id))
    {
        let handoff = owner.clone();
        let launch_id = launch.id;
        let status = state
            .factory_runs
            .get(&launch_id)
            .map(|snapshot| snapshot.status_text())
            .unwrap_or_else(|| "Recovering launch state".into());
        root = root.child(
            Button::new(SharedString::from(format!("thread-launch-{launch_id}")))
                .small()
                .ghost()
                .label(format!("Factory handoff · {status}"))
                .on_click(move |_, window, cx| {
                    let _ = handoff.update(cx, |workspace, cx| {
                        workspace
                            .state
                            .update(cx, |state, cx| state.select_factory_launch(launch_id, cx));
                        workspace.focus_region(super::workspace::Region::Output, window, cx);
                    });
                }),
        );
    }
    if let Some(session) = state.conversations.get(&id) {
        let retry = owner.clone();
        root = root.child(
            h_flex()
                .gap_2()
                .child(format!("{:?}", session.state))
                .when_some(session.outcome.as_ref(), |row, outcome| {
                    row.child(format!("Last turn: {outcome:?}"))
                })
                .child(
                    Button::new("retry-omp")
                        .small()
                        .ghost()
                        .label("Retry Connection / Model Discovery")
                        .disabled(state.active_threads.contains(&id))
                        .on_click(move |_, _, cx| {
                            let _ = retry.update(cx, |workspace, cx| {
                                workspace.state.update(cx, |state, cx| {
                                    state.retry_conversation(id);
                                    cx.notify();
                                })
                            });
                        }),
                ),
        );
        if let Some(error) = &session.error {
            root = root.child(div().text_color(rgb(0xE6A0A0)).child(error.clone()));
        }
        let mut models = h_flex().id("model-catalog").overflow_x_scroll().gap_1();
        for (index, model) in session.models.iter().enumerate() {
            let owner = owner.clone();
            let key = model.key.clone();
            models = models.child(
                Button::new(("model", index))
                    .small()
                    .ghost()
                    .selected(session.model.as_ref() == Some(&model.key))
                    .disabled(state.active_threads.contains(&id))
                    .label(model.name.clone())
                    .tooltip(format!(
                        "{} / {} · {}",
                        model.key.provider,
                        model.key.id,
                        model.input.join(", ")
                    ))
                    .on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.state.update(cx, |state, cx| {
                                state.select_model(id, key.clone());
                                cx.notify();
                            })
                        });
                    }),
            );
        }
        root = root.child(models);
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
                    .map(|current| message(&current, index, id, &history_owner, cx))
                    .unwrap_or_else(|| div().into_any_element())
            })
            .flex_1()
            .min_h(px(80.)),
        );
        if !session.tools.is_empty() {
            let keys: Vec<_> = session.tools.keys().cloned().collect();
            let state_entity = owner.upgrade().map(|owner| owner.read(cx).state.clone());
            let tool_owner = owner.clone();
            root = root.child(
                uniform_list("tool-activity", keys.len(), move |range, _, cx| {
                    range
                        .map(|index| {
                            let tool = state_entity
                                .as_ref()
                                .and_then(|state| state.read(cx).conversations.get(&id))
                                .and_then(|session| session.tools.get(&keys[index]))
                                .cloned();
                            let owner = tool_owner.clone();
                            if let Some(tool) = tool {
                                Button::new(("tool", index))
                                    .small()
                                    .ghost()
                                    .label(format!(
                                        "{} · {} · {}",
                                        tool.name,
                                        if tool.error {
                                            "Failed"
                                        } else if tool.complete {
                                            "Finished"
                                        } else {
                                            "Running"
                                        },
                                        tool.id
                                    ))
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |workspace, cx| {
                                            workspace.tool_detail = Some((id, tool.id.clone()));
                                            cx.notify();
                                        });
                                    })
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .h(px(80.)),
            );
        }
        for request in session.requests.values() {
            let mut row = v_flex()
                .gap_1()
                .p_2()
                .border_1()
                .border_color(rgb(0x7AA2F7))
                .child(request.title.clone());
            if !request.text.is_empty() {
                row = row.child(div().text_xs().child(request.text.clone()));
            }
            let mut choices = h_flex().gap_1();
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
            root = root.child(row.child(choices));
        }
        for (key, text) in &session.statuses {
            root = root.child(div().text_xs().child(format!("{key}: {text}")));
        }
        for (index, (key, lines)) in session.widgets.iter().enumerate() {
            root = root.child(
                div()
                    .text_xs()
                    .child(format!("{key}: {}", lines.join("\n"))),
            );
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
        for (index, (url, instructions)) in session.links.iter().enumerate() {
            let url = url.clone();
            let label = instructions.clone().unwrap_or_else(|| url.clone());
            root = root.child(
                Button::new(("offered-link", index))
                    .small()
                    .ghost()
                    .label(label)
                    .on_click(move |_, _, cx| {
                        if url.starts_with("https://") || url.starts_with("http://") {
                            cx.open_url(&url);
                        }
                    }),
            );
        }
        if !session.diagnostics.is_empty() {
            let identity = format!("session-diagnostics:{id}");
            let expanded = owner
                .upgrade()
                .is_some_and(|workspace| workspace.read(cx).expanded.contains(&identity));
            let toggle = owner.clone();
            root = root.child(
                Button::new("session-diagnostics")
                    .small()
                    .ghost()
                    .label(if expanded {
                        "Hide raw diagnostics"
                    } else {
                        "Show raw diagnostics / unknown data"
                    })
                    .on_click(move |_, _, cx| {
                        let _ = toggle.update(cx, |workspace, cx| {
                            if !workspace.expanded.remove(&identity) {
                                workspace.expanded.insert(identity.clone());
                            }
                            cx.notify();
                        });
                    }),
            );
            if expanded {
                for (message, raw) in &session.diagnostics {
                    root = root.child(
                        div()
                            .font_family("Menlo")
                            .text_xs()
                            .child(format!("{message}\n{raw}")),
                    );
                }
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
        root = root.child(
            v_flex()
                .gap_1()
                .p_2()
                .border_1()
                .border_color(rgb(0xE5C07B))
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
        );
    }
    if thread.archived || project.archived {
        return root
            .child("Archived history · read-only")
            .into_any_element();
    }
    let mut previews = h_flex().id("draft-attachments").overflow_x_scroll().gap_3();
    for image in &thread.attachments {
        let remove = owner.clone();
        let attachment_id = image.id;
        previews = previews.child(
            v_flex().gap_1().child(image_thumbnail(image, owner)).child(
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
    root = root.child(previews);
    if let Some(workspace) = owner.upgrade() {
        if let Some(editor) = workspace.read(cx).editor.clone() {
            if workspace.read(cx).editor_override.is_none() {
                root = root.child(
                    div()
                        .h(px(180.))
                        .border_1()
                        .border_color(rgb(0x343A46))
                        .child(editor),
                );
            }
        }
    }
    if let Some(error) = &state.editor_error {
        root = root.child(div().text_color(rgb(0xE6A0A0)).child(error.clone()));
    }
    let attach = owner.clone();
    let optimize = owner.clone();
    let send = owner.clone();
    let stop = owner.clone();
    let restart = owner.clone();
    let cancel_editor = owner.clone();
    let reason = state.submit_reason(id);
    root.child(
        h_flex()
            .flex_wrap()
            .gap_1()
            .child(
                Button::new("attach-images")
                    .small()
                    .ghost()
                    .label("Attach Images…")
                    .disabled(state.storage_error.is_some())
                    .on_click(move |_, window, cx| {
                        let _ =
                            attach.update(cx, |workspace, cx| workspace.attach_images(window, cx));
                    }),
            )
            .child(
                Button::new("optimize-images")
                    .small()
                    .ghost()
                    .label("Optimize images for sending")
                    .disabled(thread.attachments.is_empty())
                    .on_click(move |_, window, cx| {
                        let _ = optimize
                            .update(cx, |workspace, cx| workspace.optimize_images(window, cx));
                    }),
            )
            .child(
                Button::new("restart-editor")
                    .small()
                    .ghost()
                    .label("Restart Editor")
                    .on_click(move |_, window, cx| {
                        let _ = restart
                            .update(cx, |workspace, cx| workspace.restart_editor(window, cx));
                    }),
            )
            .child(
                Button::new("cancel-editor-command")
                    .small()
                    .ghost()
                    .label("Cancel Editor Command")
                    .on_click(move |_, window, cx| {
                        let _ = cancel_editor.update(cx, |workspace, cx| {
                            workspace.cancel_editor_command(window, cx)
                        });
                    }),
            )
            .child(
                Button::new("send-prompt")
                    .small()
                    .primary()
                    .label("Send  ⌘↵")
                    .disabled(reason.is_some())
                    .when_some(reason, |button, reason| button.tooltip(reason))
                    .on_click(move |_, window, cx| {
                        let _ =
                            send.update(cx, |workspace, cx| workspace.submit_prompt(window, cx));
                    }),
            )
            .child(
                Button::new("interrupt-prompt")
                    .small()
                    .ghost()
                    .label("Stop  ⌘.")
                    .disabled(!state.active_threads.contains(&id))
                    .on_click(move |_, _, cx| {
                        let _ = stop.update(cx, |workspace, cx| {
                            workspace.state.update(cx, |state, cx| {
                                state.interrupt(id);
                                cx.notify();
                            })
                        });
                    }),
            ),
    )
    .into_any_element()
}
