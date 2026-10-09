use super::workspace::Workspace;
use crate::{
    harness::{UserAnswer, UserRequest, UserRequestKind},
    model::{DraftKey, ThreadId},
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, StyledExt, WindowExt,
        button::{Button, ButtonVariants},
        form::Field,
        h_flex,
        input::{Input, InputState},
        link::Link,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::path::PathBuf;

pub struct ExtensionForm {
    pub thread: ThreadId,
    pub request: UserRequest,
    pub input: Option<Entity<InputState>>,
    error: Option<String>,
    submitting: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum SurfaceKey {
    Image(PathBuf),
    Extension(ThreadId, String),
    // An empty tool id identifies the thread's details.
    Details(ThreadId, String),
}

struct DetailContent {
    owner: WeakEntity<Workspace>,
    key: SurfaceKey,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    fn extension_busy(&self, cx: &App) -> bool {
        self.extension.as_ref().is_some_and(|form| {
            form.submitting
                || (form.request.kind == UserRequestKind::Editor
                    && self
                        .editor
                        .as_ref()
                        .is_some_and(|editor| editor.read(cx).has_composition()))
        })
    }

    pub fn open_extension(
        &mut self,
        thread: ThreadId,
        request: UserRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).shutting_down
            || self.extension.is_some()
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        if !self
            .state
            .read(cx)
            .conversations
            .get(&thread)
            .is_some_and(|session| session.requests.contains_key(&request.id))
        {
            return;
        }
        self.dismiss(window, cx);
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
        self.extension = Some(ExtensionForm {
            thread,
            request,
            input,
            error: None,
            submitting: false,
        });
        // Open before focusing the input/editor so Root remembers the actual trigger.
        self.sync_detail_surface(window, cx);
        self.sync_session(window, cx);
        if let Some(input) = self.extension.as_ref().and_then(|form| form.input.as_ref()) {
            input.focus_handle(cx).focus(window, cx);
        } else if let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| editor.focus(window, cx));
        }
        cx.notify();
    }

    pub fn open_custom_answer(
        &mut self,
        thread: ThreadId,
        mut request: UserRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if request.kind != UserRequestKind::Question {
            return;
        }
        request.kind = UserRequestKind::Input;
        request.title = "Custom answer".into();
        request.text.clear();
        request.placeholder = Some("Type an answer".into());
        self.open_extension(thread, request, window, cx);
    }

    pub fn close_extension(&mut self, cancel: bool, window: &mut Window, cx: &mut Context<Self>) {
        if cancel && self.extension_busy(cx) {
            return;
        }
        self.clear_extension(cancel, cx);
        if matches!(self.detail_surface, Some(SurfaceKey::Extension(..))) {
            self.close_detail_surface(window, cx);
        }
        self.sync_session(window, cx);
        cx.notify();
    }

    fn clear_extension(&mut self, cancel: bool, cx: &mut Context<Self>) {
        if let Some(form) = self.extension.take()
            && cancel
        {
            self.state.update(cx, |state, cx| {
                state.respond(
                    form.thread,
                    form.request.id,
                    UserAnswer::Cancelled { timed_out: false },
                );
                cx.notify();
            });
        }
        self.editor_override = None;
    }

    pub fn confirm_extension(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.extension_busy(cx) {
            return;
        }
        let Some(form) = &mut self.extension else {
            return;
        };
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
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let key = DraftKey::Extension {
            thread: id,
            request: request_id.clone(),
        };
        if editor.read(cx).active_key() != Some(&key) {
            form.error = Some("Complete or cancel composition in the previous draft first".into());
            cx.notify();
            return;
        }
        let result = editor.update(cx, |editor, cx| editor.snapshot(&key, window, cx));
        form.submitting = true;
        cx.notify();
        cx.spawn_in(window, async move |workspace, window| {
            let result = result.await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                if !workspace
                    .extension
                    .as_ref()
                    .is_some_and(|form| form.thread == id && form.request.id == request_id)
                {
                    return;
                }
                if let Some(form) = &mut workspace.extension {
                    form.submitting = false;
                }
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
                            form.error = Some(format!("Extension answer was not sent: {error:#}"));
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Closes only our current native host. Callers own clearing application data.
    pub(super) fn close_detail_surface(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(self.detail_surface, Some(SurfaceKey::Extension(..))) && self.extension_busy(cx)
        {
            return false;
        }
        match self.detail_surface.take() {
            Some(SurfaceKey::Details(..)) => window.close_sheet(cx),
            Some(_) => window.close_dialog(cx),
            None => {}
        }
        true
    }

    pub(super) fn sync_detail_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // An outstanding answer is exclusive; detail actions cannot abandon its input.
        if self.extension.is_some() {
            self.image_preview = None;
            self.tool_detail = None;
        }
        // A clicked image replaces the details sheet which contained it.
        if self.image_preview.is_some() {
            self.tool_detail = None;
        }
        let key = match (&self.extension, &self.image_preview, &self.tool_detail) {
            (Some(form), None, None) => {
                Some(SurfaceKey::Extension(form.thread, form.request.id.clone()))
            }
            (None, Some(image), None) => Some(SurfaceKey::Image(image.path.clone())),
            (None, None, Some((thread, tool))) => Some(SurfaceKey::Details(*thread, tool.clone())),
            _ => None,
        };
        if self.detail_surface == key {
            return;
        }
        if !self.close_detail_surface(window, cx) {
            return;
        }
        let Some(key) = key else {
            return;
        };
        if self.state.read(cx).shutting_down {
            return;
        }
        // Other application hosts must close before this one captures trigger focus.
        if let Some(palette) = self.palette.take() {
            palette.update(cx, |palette, cx| palette.dismiss(window, cx));
        }
        if let Some(dialog) = self.dialog.take() {
            let kind = dialog.read(cx).kind.clone();
            super::settings::close_surface(&kind, window, cx);
        }
        let workspace_entity = cx.entity();
        let owner = workspace_entity.downgrade();
        let state = self.state.clone();
        let content = cx.new(|cx: &mut Context<DetailContent>| DetailContent {
            owner: owner.clone(),
            key: key.clone(),
            _subscriptions: vec![
                cx.observe(&workspace_entity, |_, _, cx| cx.notify()),
                cx.observe(&state, |_, _, cx| cx.notify()),
                cx.subscribe(
                    &state,
                    |content, _, change: &crate::app::PresentationChange, cx| {
                        let thread = match &content.key {
                            SurfaceKey::Extension(thread, _) | SurfaceKey::Details(thread, _) => {
                                Some(*thread)
                            }
                            SurfaceKey::Image(_) => None,
                        };
                        if matches!(change, crate::app::PresentationChange::Thread(id)
                        | crate::app::PresentationChange::Transcript(id)
                        | crate::app::PresentationChange::Draft(id) if Some(*id) == thread)
                        {
                            cx.notify();
                        }
                    },
                ),
            ],
        });
        self.detail_surface = Some(key.clone());
        match key {
            SurfaceKey::Details(..) => {
                window.open_sheet(cx, move |sheet, window, cx| {
                    let close = owner.clone();
                    let key = content.read(cx).key.clone();
                    let width = (window.rem_size() * 42.).min(window.viewport_size().width);
                    sheet
                        .title("Details")
                        .size(width)
                        .child(content.clone())
                        .on_close(move |_, window, cx| {
                            let _ = close.update(cx, |workspace, cx| {
                                workspace.detail_closed(&key, window, cx)
                            });
                        })
                });
            }
            _ => {
                window.open_dialog(cx, move |dialog, window, cx| {
                    let key = content.read(cx).key.clone();
                    let cancel = owner.clone();
                    let close = owner.clone();
                    let current = owner.upgrade();
                    let workspace = current.as_ref().map(|owner| owner.read(cx));
                    let extension = workspace.and_then(|workspace| workspace.extension.as_ref());
                    let title = extension
                        .map(|form| form.request.title.clone())
                        .unwrap_or_else(|| "Image preview".into());
                    let preferred = if extension.is_some() { 46. } else { 64. };
                    let width = (window.rem_size() * preferred)
                        .min(window.viewport_size().width - window.rem_size() * 2.);
                    let mut dialog = dialog.title(title).width(width).child(content.clone());
                    if extension.is_some() {
                        let answer = owner.clone();
                        let answer_key = key.clone();
                        dialog = dialog.on_ok(move |_, window, cx| {
                            let _ = answer.update(cx, |workspace, cx| {
                                if workspace.detail_surface.as_ref() == Some(&answer_key) {
                                    workspace.confirm_extension(window, cx);
                                }
                            });
                            false
                        });
                        let busy = workspace.is_some_and(|workspace| workspace.extension_busy(cx));
                        let confirm = owner.clone();
                        let cancel_button = owner.clone();
                        dialog = dialog.overlay_closable(false).footer(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-extension")
                                        .label("Cancel")
                                        .disabled(busy)
                                        .on_click(move |_, window, cx| {
                                            let _ = cancel_button.update(cx, |workspace, cx| {
                                                workspace.close_extension(true, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("confirm-extension")
                                        .primary()
                                        .label("Send answer")
                                        .disabled(
                                            busy || workspace.is_some_and(|workspace| {
                                                workspace.editor_override.is_some()
                                                    && workspace.editor.is_none()
                                            }),
                                        )
                                        .on_click(move |_, window, cx| {
                                            let _ = confirm.update(cx, |workspace, cx| {
                                                workspace.confirm_extension(window, cx)
                                            });
                                        }),
                                ),
                        );
                    }
                    dialog
                        .on_cancel(move |_, _, cx| {
                            cancel
                                .upgrade()
                                .is_none_or(|owner| !owner.read(cx).extension_busy(cx))
                        })
                        .on_close(move |_, window, cx| {
                            let _ = close.update(cx, |workspace, cx| {
                                workspace.detail_closed(&key, window, cx)
                            });
                        })
                });
            }
        }
    }

    // Native user dismissal already pops/restores focus: do application cleanup only.
    fn detail_closed(&mut self, key: &SurfaceKey, window: &mut Window, cx: &mut Context<Self>) {
        if self.detail_surface.as_ref() != Some(key) {
            return;
        }
        self.detail_surface = None;
        match key {
            SurfaceKey::Image(_) => self.image_preview = None,
            SurfaceKey::Details(..) => self.tool_detail = None,
            SurfaceKey::Extension(..) => {
                self.clear_extension(true, cx);
                self.sync_session(window, cx);
            }
        }
        cx.notify();
    }
}

impl Render for DetailContent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div().into_any_element();
        };
        let workspace = owner.read(cx);
        if workspace.detail_surface.as_ref() != Some(&self.key) {
            return div().into_any_element();
        }
        match &self.key {
            SurfaceKey::Image(path) => {
                let Some(image) = workspace
                    .image_preview
                    .as_ref()
                    .filter(|image| &image.path == path)
                else {
                    return div().into_any_element();
                };
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} × {} · {}",
                                image.width, image.height, image.mime
                            )),
                    )
                    .child(
                        img(image.path.clone())
                            .w_full()
                            .max_h(
                                (window.viewport_size().height * 0.65).min(window.rem_size() * 42.),
                            )
                            .object_fit(ObjectFit::Contain),
                    )
                    .into_any_element()
            }
            SurfaceKey::Extension(thread, request) => {
                let Some(form) = workspace
                    .extension
                    .as_ref()
                    .filter(|form| form.thread == *thread && form.request.id == *request)
                else {
                    return div().into_any_element();
                };
                let answer = self.owner.clone();
                let answer_key = self.key.clone();
                let mut body = v_flex()
                    .id("extension-body")
                    .key_context("DrostjeForm ExtensionRequest")
                    .on_action(move |_: &super::commands::Confirm, window, cx| {
                        cx.stop_propagation();
                        let _ = answer.update(cx, |workspace, cx| {
                            if workspace.detail_surface.as_ref() == Some(&answer_key) {
                                workspace.confirm_extension(window, cx);
                            }
                        });
                    })
                    .gap_3()
                    .max_h(window.viewport_size().height * 0.6)
                    .overflow_y_scroll();
                if let Some(input) = &form.input {
                    body = body.child(
                        Field::new()
                            .label("Answer")
                            .child(div().key_context("NativeInput").child(Input::new(input))),
                    );
                } else if let Some(editor) = &workspace.editor {
                    body = body.child(
                        div()
                            .h((window.rem_size() * 20.).min(window.viewport_size().height * 0.45))
                            .child(editor.clone()),
                    );
                }
                if let Some(error) = &form.error {
                    body = body.child(div().text_color(cx.theme().danger).child(error.clone()));
                }
                body.into_any_element()
            }
            SurfaceKey::Details(thread, tool) => self.details(workspace, *thread, tool, cx),
        }
    }
}

impl DetailContent {
    fn details(
        &self,
        workspace: &Workspace,
        thread_id: ThreadId,
        tool_id: &str,
        cx: &App,
    ) -> AnyElement {
        let state = workspace.state.read(cx);
        let mut body = v_flex().gap_3().pb_4();
        let Some(thread) = state
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
        else {
            return body
                .child("This thread is no longer available.")
                .into_any_element();
        };
        if tool_id.is_empty() {
            body = body.child(div().font_semibold().child(thread.title.clone()));
            if let Some(project) = state
                .snapshot
                .projects
                .iter()
                .find(|project| project.id == thread.project_id)
            {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{}\n{}", project.name, project.root.display())),
                );
            }
            body = body.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("OMP runs with your local user privileges and configured tool/approval policy, not Factory's Docker sandbox."));
        }
        let Some(session) = state.conversations.get(&thread_id) else {
            return body
                .child("No active conversation session.")
                .into_any_element();
        };
        if !tool_id.is_empty() {
            let Some(tool) = session.tools.get(tool_id) else {
                return body
                    .child("This tool is no longer available.")
                    .into_any_element();
            };
            body = body
                .child(div().font_semibold().child(format!(
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
                .when_some(tool.intent.clone(), |body, intent| {
                    body.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(intent),
                    )
                })
                .child(
                    div()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_sm()
                        .whitespace_normal()
                        .child(serde_json::to_string_pretty(&tool.input).unwrap_or_default()),
                );
            for (index, block) in tool.result.iter().enumerate() {
                body = body.child(super::conversation::block(
                    block,
                    format!("tool:{thread_id}:{tool_id}:{index}"),
                    &self.owner,
                    super::conversation::Markdown::Element,
                    cx,
                ));
            }
            if let Some(raw) = &tool.raw {
                let identity = format!("tool-raw:{thread_id}:{tool_id}");
                let text = if workspace.expanded.contains(&identity) {
                    serde_json::to_string_pretty(raw).unwrap_or_default()
                } else {
                    String::new()
                };
                body = body.child(self.disclosure(workspace, identity, "Raw tool data", text, cx));
            }
            return body.into_any_element();
        }
        body = body.child(
            div()
                .text_sm()
                .child(format!("Session: {:?}", session.state)),
        );
        if let Some(outcome) = &session.outcome {
            body = body.child(div().text_sm().child(format!("Last turn: {outcome:?}")));
        }
        if let Some(error) = &session.error {
            body = body.child(div().text_color(cx.theme().danger).child(error.clone()));
        }
        for (key, text) in &session.statuses {
            body = body.child(div().text_sm().child(format!("{key}: {text}")));
        }
        for (key, lines) in &session.widgets {
            if !key.starts_with("Proposed editor replacement") {
                body = body.child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .child(format!("{key}: {}", lines.join("\n"))),
                );
            }
        }
        for (index, (url, instructions)) in session.links.iter().enumerate() {
            if url.starts_with("https://") || url.starts_with("http://") {
                body = body.child(
                    Link::new(("session-link", index))
                        .href(url.clone())
                        .child(instructions.clone().unwrap_or_else(|| url.clone())),
                );
            }
        }
        for (index, tool) in session.tools.values().enumerate() {
            let owner = self.owner.clone();
            let tool_id = tool.id.clone();
            let summary = super::conversation::tool_summary(&tool.name, &tool.input);
            body = body.child(
                Button::new(("detail-tool", index))
                    .small()
                    .ghost()
                    .label(format!(
                        "{} · {}",
                        tool.intent.as_deref().unwrap_or(&tool.name),
                        if tool.error {
                            "Failed"
                        } else if tool.complete {
                            "Finished"
                        } else {
                            "Running"
                        }
                    ))
                    .when(!summary.body.is_empty(), |button| {
                        button.tooltip(summary.body)
                    })
                    .on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.tool_detail = Some((thread_id, tool_id.clone()));
                            workspace.sync_detail_surface(window, cx);
                            cx.notify();
                        });
                    }),
            );
        }
        if !session.diagnostics.is_empty() {
            let identity = format!("session-diagnostics:{thread_id}");
            let diagnostics = if workspace.expanded.contains(&identity) {
                session
                    .diagnostics
                    .iter()
                    .map(|(message, raw)| format!("{message}\n{raw}"))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            } else {
                String::new()
            };
            body = body.child(self.disclosure(
                workspace,
                identity,
                "Raw diagnostics / unknown data",
                diagnostics,
                cx,
            ));
        }
        body.into_any_element()
    }

    fn disclosure(
        &self,
        workspace: &Workspace,
        identity: String,
        label: &str,
        text: String,
        cx: &App,
    ) -> AnyElement {
        let expanded = workspace.expanded.contains(&identity);
        let owner = self.owner.clone();
        v_flex()
            .gap_2()
            .child(
                Button::new(SharedString::from(identity.clone()))
                    .small()
                    .ghost()
                    .label(format!(
                        "{} {label}",
                        if expanded { "Hide" } else { "Show" }
                    ))
                    .on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            if !workspace.expanded.remove(&identity) {
                                workspace.expanded.insert(identity.clone());
                            }
                            cx.notify();
                        });
                    }),
            )
            .when(expanded, |body| {
                body.child(
                    div()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_sm()
                        .whitespace_normal()
                        .child(text),
                )
            })
            .into_any_element()
    }
}
