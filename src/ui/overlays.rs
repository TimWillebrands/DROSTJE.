use super::workspace::Workspace;
use crate::{
    harness::{UserAnswer, UserRequest, UserRequestKind},
    model::ThreadId,
    neovim::DraftKey,
};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputState},
        v_flex,
    },
    *,
};

pub struct ExtensionForm {
    pub thread: ThreadId,
    pub request: UserRequest,
    pub input: Option<Entity<InputState>>,
    pub previous_focus: Option<FocusHandle>,
    pub pending: bool,
}
impl Workspace {
    pub fn open_extension(
        &mut self,
        thread: ThreadId,
        request: UserRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss(window, cx);
        let previous_focus = window.focused(cx);
        let input = if request.kind == UserRequestKind::Input {
            Some(cx.new(|cx| {
                let mut input = InputState::new(window, cx)
                    .placeholder(request.placeholder.clone().unwrap_or_default());
                input.set_value(request.text.clone(), window, cx);
                input
            }))
        } else {
            None
        };
        if request.kind == UserRequestKind::Editor {
            self.editor_override = Some((
                DraftKey::Extension {
                    thread,
                    request: request.id.clone(),
                },
                request.text.clone(),
            ));
        }
        if let Some(input) = &input {
            input.focus_handle(cx).focus(window, cx);
        }
        self.extension = Some(ExtensionForm {
            thread,
            request,
            input,
            previous_focus,
            pending: false,
        });
        self.sync_session(window, cx);
        if self.editor_override.is_some() {
            if let Some(editor) = &self.editor {
                editor.update(cx, |editor, cx| editor.focus(window, cx));
            }
        }
        cx.notify();
    }
    pub fn close_extension(&mut self, cancel: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = self.extension.take() {
            if cancel {
                self.state.update(cx, |state, cx| {
                    state.respond(
                        form.thread,
                        form.request.id,
                        UserAnswer::Cancelled { timed_out: false },
                    );
                    cx.notify();
                });
            }
            if let Some(focus) = form.previous_focus {
                focus.focus(window, cx);
            }
        }
        self.editor_override = None;
        self.sync_session(window, cx);
        cx.notify();
    }
    pub fn confirm_extension(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.extension else {
            return;
        };
        if form.pending {
            return;
        }
        let id = form.thread;
        let request_id = form.request.id.clone();
        if let Some(input) = &form.input {
            let value = input.read(cx).value().to_string();
            self.state.update(cx, |state, cx| {
                state.respond(id, request_id, UserAnswer::Value(value));
                cx.notify();
            });
            self.close_extension(false, window, cx);
            return;
        }
        let Some(editor) = self.state.read(cx).editor.clone() else {
            return;
        };
        if self.editor_switching
            || self
                .editor
                .as_ref()
                .is_some_and(|view| view.read(cx).has_composition())
        {
            return;
        }
        form.pending = true;
        cx.spawn_in(window, async move |workspace, window| {
            let result = editor
                .snapshot(DraftKey::Extension {
                    thread: id,
                    request: request_id.clone(),
                })
                .await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                let still_pending = workspace
                    .state
                    .read(cx)
                    .conversations
                    .get(&id)
                    .is_some_and(|session| session.requests.contains_key(&request_id));
                if !still_pending {
                    workspace.close_extension(false, window, cx);
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        workspace.state.update(cx, |state, cx| {
                            state.respond(id, request_id, UserAnswer::Value(snapshot.text));
                            cx.notify();
                        });
                        workspace.close_extension(false, window, cx);
                    }
                    Err(error) => {
                        if let Some(form) = &mut workspace.extension {
                            form.pending = false;
                        }
                        workspace.state.update(cx, |state, cx| {
                            state.notice(format!("Extension answer was not sent: {error:#}"));
                            cx.notify();
                        });
                    }
                }
            });
        })
        .detach();
    }
    pub fn render_detail_overlay(
        &self,
        owner: &WeakEntity<Workspace>,
        cx: &App,
    ) -> Option<AnyElement> {
        if let Some(image) = &self.image_preview {
            let close = owner.clone();
            return Some(
                v_flex()
                    .w(px(1000.))
                    .max_h(px(760.))
                    .p_3()
                    .gap_2()
                    .bg(rgb(0x20242B))
                    .child(
                        h_flex()
                            .child(div().flex_1().child(format!(
                                "{} × {} · {}",
                                image.width, image.height, image.mime
                            )))
                            .child(
                                Button::new("close-image")
                                    .small()
                                    .label("Close Preview")
                                    .on_click(move |_, _, cx| {
                                        let _ = close.update(cx, |workspace, cx| {
                                            workspace.image_preview = None;
                                            cx.notify();
                                        });
                                    }),
                            ),
                    )
                    .child(
                        img(image.path.clone())
                            .max_w(px(960.))
                            .max_h(px(670.))
                            .object_fit(ObjectFit::Contain),
                    )
                    .into_any_element(),
            );
        }
        if let Some(form) = &self.extension {
            let cancel = owner.clone();
            let confirm = owner.clone();
            let mut view = v_flex()
                .key_context("Dialog ExtensionRequest")
                .w(px(760.))
                .p_4()
                .gap_3()
                .bg(rgb(0x20242B))
                .border_1()
                .border_color(rgb(0x7AA2F7))
                .child(form.request.title.clone());
            if let Some(input) = &form.input {
                view = view.child(div().key_context("NativeInput").child(Input::new(input)));
            } else if let Some(editor) = &self.editor {
                view = view.child(div().h(px(300.)).child(editor.clone()));
            }
            return Some(
                view.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("cancel-extension")
                                .small()
                                .ghost()
                                .label("Cancel")
                                .on_click(move |_, window, cx| {
                                    let _ = cancel.update(cx, |workspace, cx| {
                                        workspace.close_extension(true, window, cx)
                                    });
                                }),
                        )
                        .child(
                            Button::new("confirm-extension")
                                .small()
                                .primary()
                                .label(if form.pending {
                                    "Reading editor snapshot…"
                                } else {
                                    "Send Answer"
                                })
                                .on_click(move |_, window, cx| {
                                    let _ = confirm.update(cx, |workspace, cx| {
                                        workspace.confirm_extension(window, cx)
                                    });
                                }),
                        ),
                )
                .into_any_element(),
            );
        }
        if let Some((thread, tool_id)) = &self.tool_detail {
            let tool = self
                .state
                .read(cx)
                .conversations
                .get(thread)?
                .tools
                .get(tool_id)?;
            let close = owner.clone();
            let mut body = v_flex()
                .id("tool-details-body")
                .overflow_y_scroll()
                .max_h(px(600.))
                .gap_3()
                .child(
                    div()
                        .font_family("Menlo")
                        .text_sm()
                        .whitespace_normal()
                        .child(serde_json::to_string_pretty(&tool.input).unwrap_or_default()),
                );
            for (index, block) in tool.result.iter().enumerate() {
                body = body.child(super::conversation::block(
                    block,
                    format!("tool:{thread}:{tool_id}:{index}"),
                    owner,
                    cx,
                ));
            }
            return Some(
                v_flex()
                    .w(px(900.))
                    .p_4()
                    .gap_3()
                    .bg(rgb(0x20242B))
                    .child(
                        h_flex()
                            .child(div().flex_1().child(format!(
                                "{} · {}",
                                tool.name,
                                if tool.error {
                                    "Failed"
                                } else if tool.complete {
                                    "Finished"
                                } else {
                                    "Running"
                                }
                            )))
                            .child(Button::new("close-tool").small().label("Close").on_click(
                                move |_, _, cx| {
                                    let _ = close.update(cx, |workspace, cx| {
                                        workspace.tool_detail = None;
                                        cx.notify();
                                    });
                                },
                            )),
                    )
                    .child(body)
                    .into_any_element(),
            );
        }
        None
    }
}
