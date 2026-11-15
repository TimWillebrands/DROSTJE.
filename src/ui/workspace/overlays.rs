use super::Workspace;
use crate::{
    harness::{ToolActivity, UserAnswer, UserRequest, UserRequestKind},
    model::{DraftKey, ThreadId},
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, StyledExt, WindowExt,
        button::{Button, ButtonVariants},
        form::Field,
        h_flex,
        input::{Input, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{cell::RefCell, path::PathBuf, sync::Arc};

pub struct ExtensionForm {
    pub thread: ThreadId,
    pub request: UserRequest,
    pub input: Option<Entity<InputState>>,
    pub(in crate::ui) error: Option<String>,
    pub(in crate::ui) submitting: bool,
    pub inline_answer: bool,
    return_focus: Option<FocusHandle>,
}
#[derive(Clone, PartialEq, Eq)]
pub(in crate::ui) enum SurfaceKey {
    Image(PathBuf),
    Extension(ThreadId, String),
}

#[derive(Default)]
pub(in crate::ui) struct ToolDetailText(RefCell<Option<ToolText>>);

struct ToolText {
    thread: ThreadId,
    tool: Arc<ToolActivity>,
    input: SharedString,
    result_keys: Vec<String>,
    raw: Option<SharedString>,
}

impl ToolDetailText {
    fn get(
        &self,
        thread: ThreadId,
        tool: &Arc<ToolActivity>,
        with_raw: bool,
    ) -> (SharedString, Vec<String>, Option<SharedString>) {
        let mut slot = self.0.borrow_mut();
        if !slot
            .as_ref()
            .is_some_and(|text| text.thread == thread && Arc::ptr_eq(&text.tool, tool))
        {
            *slot = Some(ToolText {
                thread,
                input: serde_json::to_string_pretty(&tool.input)
                    .unwrap_or_default()
                    .into(),
                result_keys: (0..tool.result.len())
                    .map(|index| format!("tool:{thread}:{}:{index}", tool.id))
                    .collect(),
                raw: None,
                tool: tool.clone(),
            });
        }
        let text = slot.as_mut().expect("filled above");
        let raw = match (&text.tool.raw, with_raw) {
            (Some(raw_value), true) => Some(
                text.raw
                    .get_or_insert_with(|| {
                        serde_json::to_string_pretty(raw_value)
                            .unwrap_or_default()
                            .into()
                    })
                    .clone(),
            ),
            _ => None,
        };
        (text.input.clone(), text.result_keys.clone(), raw)
    }
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
        let inline_answer = request.kind == UserRequestKind::Input;
        if !inline_answer {
            self.dismiss(window, cx);
        }
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
            inline_answer,
            return_focus: window.focused(cx),
        });
        if !inline_answer {
            self.sync_detail_surface(window, cx);
        }
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
        request: UserRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if request.kind != UserRequestKind::Question
            || self.state.read(cx).shutting_down
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
            || self.extension.is_some()
            || !self
                .state
                .read(cx)
                .conversations
                .get(&thread)
                .is_some_and(|session| session.requests.contains_key(&request.id))
        {
            return;
        }
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Type an answer");
            input.set_value("", window, cx);
            input
        });
        self.extension = Some(ExtensionForm {
            thread,
            request,
            input: Some(input.clone()),
            error: None,
            submitting: false,
            inline_answer: true,
            return_focus: window.focused(cx),
        });
        input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub fn back_from_custom_answer(
        &mut self,
        thread: ThreadId,
        request_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.extension_busy(cx)
            && self.extension.as_ref().is_some_and(|form| {
                form.inline_answer && form.thread == thread && form.request.id == request_id
            })
        {
            let focus = self
                .extension
                .as_ref()
                .and_then(|form| form.return_focus.clone());
            self.clear_extension(false, cx);
            if let Some(focus) = focus {
                focus.focus(window, cx);
            }
            cx.notify();
        }
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
        form.error = None;
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

    pub(in crate::ui) fn close_detail_surface(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(self.detail_surface, Some(SurfaceKey::Extension(..))) && self.extension_busy(cx)
        {
            return false;
        }
        if self.detail_surface.take().is_some() {
            window.close_dialog(cx);
        }
        true
    }

    pub(in crate::ui) fn open_details(
        &mut self,
        thread: ThreadId,
        tool: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .extension
            .as_ref()
            .is_some_and(|form| !form.inline_answer)
            || !self.close_detail_surface(window, cx)
        {
            return;
        }
        self.image_preview = None;
        self.tool_detail = Some((thread, tool));
        if let Some(palette) = self.palette.take() {
            palette.update(cx, |palette, cx| palette.dismiss(window, cx));
        }
        if let Some(dialog) = self.dialog.as_ref() {
            let (kind, hosted) = {
                let dialog = dialog.read(cx);
                (dialog.kind.clone(), dialog.hosted())
            };
            if hosted {
                dialog.update(cx, |dialog, cx| dialog.unhost(cx));
                crate::ui::settings::close_surface(&kind, window, cx);
            }
        }
        self.state.update(cx, |state, cx| {
            state.show_thread_sidebar(thread, crate::app::SidebarTab::Details);
            cx.emit(crate::app::PresentationChange::Thread(thread));
        });
        cx.notify();
    }

    pub(in crate::ui) fn sync_detail_surface(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = if let Some(form) = self.extension.as_ref().filter(|form| !form.inline_answer) {
            Some(SurfaceKey::Extension(form.thread, form.request.id.clone()))
        } else {
            self.image_preview
                .as_ref()
                .map(|image| SurfaceKey::Image(image.path.clone()))
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
        if let Some(palette) = self.palette.take() {
            palette.update(cx, |palette, cx| palette.dismiss(window, cx));
        }
        if let Some(dialog) = self.dialog.as_ref() {
            let (kind, hosted) = {
                let dialog = dialog.read(cx);
                (dialog.kind.clone(), dialog.hosted())
            };
            if hosted {
                dialog.update(cx, |dialog, cx| dialog.unhost(cx));
                crate::ui::settings::close_surface(&kind, window, cx);
            }
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
                            SurfaceKey::Extension(thread, _) => Some(*thread),
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
        window.open_dialog(cx, move |dialog, window, cx| {
            let key = content.read(cx).key.clone();
            let cancel = owner.clone();
            let close = owner.clone();
            let current = owner.upgrade();
            let workspace = current.as_ref().map(|owner| owner.read(cx));
            let extension = workspace
                .and_then(|workspace| workspace.extension.as_ref())
                .filter(|form| {
                    matches!(&key, SurfaceKey::Extension(thread, request)
                    if form.thread == *thread && form.request.id == *request)
                });
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
                                .loading(extension.is_some_and(|form| form.submitting))
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

    fn detail_closed(&mut self, key: &SurfaceKey, window: &mut Window, cx: &mut Context<Self>) {
        if self.detail_surface.as_ref() != Some(key) {
            return;
        }
        self.detail_surface = None;
        match key {
            SurfaceKey::Image(_) => {
                self.image_preview = None;
                self.sync_detail_surface(window, cx);
            }
            SurfaceKey::Extension(..) => {
                self.clear_extension(false, cx);
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
                    .on_action(move |_: &crate::ui::commands::Confirm, window, cx| {
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
        }
    }
}

pub(in crate::ui) fn render_details(
    workspace: &Workspace,
    thread_id: ThreadId,
    tool_id: &str,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    DetailContent::details(workspace, thread_id, tool_id, owner, cx)
}

impl DetailContent {
    fn details(
        workspace: &Workspace,
        thread_id: ThreadId,
        tool_id: &str,
        owner: &WeakEntity<Workspace>,
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
            let raw_identity = format!("tool-raw:{thread_id}:{tool_id}");
            let (input, result_keys, raw_text) = workspace.tool_detail_text.get(
                thread_id,
                tool,
                workspace.expanded.contains(&raw_identity),
            );
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
                        .child(input),
                );
            for (block, key) in tool.result.iter().zip(result_keys) {
                body = body.child(crate::ui::conversation::block(
                    block,
                    key,
                    owner,
                    crate::ui::conversation::Markdown::Element,
                    cx,
                ));
            }
            if tool.raw.is_some() {
                body = body.child(Self::disclosure(
                    workspace,
                    raw_identity,
                    "Raw tool data",
                    raw_text.unwrap_or_default(),
                    owner,
                    cx,
                ));
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
                let url = url.clone();
                let owner = owner.clone();
                body = body.child(
                    Button::new(("session-link", index))
                        .ghost()
                        .label(instructions.clone().unwrap_or_else(|| url.clone()))
                        .on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |workspace, cx| workspace.dismiss(window, cx));
                            window.dispatch_action(
                                Box::new(crate::ui::browser::OpenBrowserUrl { url: url.clone() }),
                                cx,
                            );
                        }),
                );
            }
        }
        for (index, tool) in session.tools.values().enumerate() {
            let owner = owner.clone();
            let tool_id = tool.id.clone();
            let summary = crate::ui::conversation::tool_summary(&tool.name, &tool.input);
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
                            workspace.open_details(thread_id, tool_id.clone(), window, cx);
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
                    .into()
            } else {
                SharedString::default()
            };
            body = body.child(Self::disclosure(
                workspace,
                identity,
                "Raw diagnostics / unknown data",
                diagnostics,
                owner,
                cx,
            ));
        }
        body.into_any_element()
    }

    fn disclosure(
        workspace: &Workspace,
        identity: String,
        label: &str,
        text: SharedString,
        owner: &WeakEntity<Workspace>,
        cx: &App,
    ) -> AnyElement {
        let expanded = workspace.expanded.contains(&identity);
        let owner = owner.clone();
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

#[cfg(test)]
mod tests {
    use super::ToolDetailText;
    use crate::{harness::ToolActivity, model::ThreadId};
    use serde_json::json;
    use std::sync::Arc;

    fn tool(input: serde_json::Value) -> Arc<ToolActivity> {
        Arc::new(ToolActivity {
            id: "call-1".into(),
            name: "read".into(),
            intent: None,
            input,
            result: Vec::new(),
            error: false,
            exit_code: None,
            complete: true,
            raw: Some(json!({"wire": {"method": "tool_execution_end"}})),
        })
    }

    #[test]
    fn detail_text_is_built_once_per_tool_update() {
        let cache = ToolDetailText::default();
        let thread = ThreadId::new_v4();
        let first = tool(json!({"path": "src/ui/workspace/overlays.rs"}));
        let (input, _, raw) = cache.get(thread, &first, false);
        assert_eq!(input, serde_json::to_string_pretty(&first.input).unwrap());
        assert_eq!(raw, None);
        let (again, _, raw) = cache.get(thread, &first, true);
        assert_eq!(again.as_ptr(), input.as_ptr(), "same tool, same text");
        let raw = raw.expect("raw data while open");
        let (_, _, raw_again) = cache.get(thread, &first, true);
        assert_eq!(raw_again.unwrap().as_ptr(), raw.as_ptr());

        let updated = tool(json!({"path": "src/ui/workspace/projects.rs"}));
        let (input, _, _) = cache.get(thread, &updated, false);
        assert_eq!(input, serde_json::to_string_pretty(&updated.input).unwrap());
        assert_ne!(input.as_ptr(), again.as_ptr(), "a replaced tool is rebuilt");
    }
}
