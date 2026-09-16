use super::{
    nvim::{NvimView, NvimViewEvent},
    workspace::{Region, Workspace},
};
use crate::{
    app::{AppState, NoticeLevel},
    model::*,
    neovim::{NvimConfig, NvimEditor},
    platform,
};
use gpui_kit::component::{ActiveTheme, alert::Alert, label::Label};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;

/// Owns activation and presentation; Neovim owns every editing revision.
pub(super) struct PromptEditor {
    state: Entity<AppState>,
    view: Option<Entity<NvimView>>,
    desired: Option<(DraftKey, String)>,
    active: Option<DraftKey>,
    switching: bool,
    opening: bool,
    composition: bool,
    window_inactive: bool,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    switch_task: Option<Task<()>>,
}

pub(super) struct PromptEditorChanged;
impl EventEmitter<PromptEditorChanged> for PromptEditor {}

impl PromptEditor {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self {
            state,
            view: None,
            desired: None,
            active: None,
            switching: false,
            opening: false,
            composition: false,
            window_inactive: false,
            focus: cx.focus_handle(),
            _subscriptions: Vec::new(),
            switch_task: None,
        }
    }

    pub fn active_key(&self) -> Option<&DraftKey> {
        self.active.as_ref()
    }

    pub fn has_composition(&self) -> bool {
        self.composition
    }

    pub fn recovery_text(&self, cx: &App) -> String {
        self.view
            .as_ref()
            .map(|view| view.read(cx).recovery_text())
            .unwrap_or_default()
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle(cx).focus(window, cx);
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.opening || self.view.is_some() {
            return;
        }
        self.opening = true;
        let explicit = self
            .state
            .read(cx)
            .snapshot
            .settings
            .get("executables")
            .and_then(|value| value.get("nvim"))
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .map(str::to_owned);
        cx.spawn_in(window, async move |this, window| {
            let result = async {
                let path = smol::unblock(move || platform::resolve_executable("nvim", explicit.as_deref())).await?;
                NvimEditor::open(NvimConfig::new(path)).await
            }.await;
            let _ = this.update_in(window, |this, window, cx| {
                this.opening = false;
                match result {
                    Ok(editor) => {
                        this.state.update(cx, |state, cx| {
                            state.editor = Some(editor.clone());
                            state.editor_error = None;
                            cx.notify();
                        });
                        let view = cx.new(|cx| NvimView::new(editor, window, cx));
                        this._subscriptions.push(cx.subscribe_in(&view, window, |this, _, event: &NvimViewEvent, window, cx| {
                            match event {
                                NvimViewEvent::DraftActivated(draft) => {
                                    this.active = Some(draft.key.clone());
                                    let id = match &draft.key {
                                        DraftKey::Thread(id) | DraftKey::Extension { thread: id, .. } => *id,
                                    };
                                    this.state.update(cx, |state, cx| {
                                        state.record_draft(draft.clone());
                                        cx.emit(crate::app::PresentationChange::Draft(id));
                                    });
                                }
                                NvimViewEvent::DraftChanged(draft) => {
                                    let id = match &draft.key {
                                        DraftKey::Thread(id) | DraftKey::Extension { thread: id, .. } => *id,
                                    };
                                    let unfocused = this.window_inactive || !this.focus_handle(cx).is_focused(window);
                                    this.state.update(cx, |state, cx| {
                                        state.record_draft(draft.clone());
                                        if unfocused && state.commit_thread_edits(id) {
                                            cx.emit(crate::app::PresentationChange::Persistence);
                                        }
                                        cx.emit(crate::app::PresentationChange::Draft(id));
                                    });
                                    return;
                                }
                                NvimViewEvent::CompositionChanged(composing) => {
                                    this.composition = *composing;
                                    if !composing { this.sync(window, cx); }
                                }
                                NvimViewEvent::Ready => {
                                    this.state.update(cx, |state, cx| { state.editor_error = None; cx.notify(); });
                                    this.sync(window, cx);
                                }
                                NvimViewEvent::Failed(error) => this.failed(error.clone(), cx),
                                NvimViewEvent::InputRejected(error) => this.state.update(cx, |state, cx| {
                                    state.notice(NoticeLevel::Warning, format!("Editor input: {error}")); cx.notify();
                                }),
                            }
                            cx.emit(PromptEditorChanged);
                            cx.notify();
                        }));
                        let focus = view.focus_handle(cx);
                        this._subscriptions.push(cx.on_blur(&focus, window, |this, _, cx| {
                            this.commit_on_focus_loss(cx);
                        }));
                        this._subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
                            // An inactive window can retain the editor's focus handle.
                            this.window_inactive = !window.is_window_active();
                            if this.window_inactive {
                                this.commit_on_focus_loss(cx);
                            }
                        }));
                        if this.focus.is_focused(window) {
                            view.update(cx, |view, cx| view.focus(window, cx));
                        }
                        this.view = Some(view);
                        this.sync(window, cx);
                    }
                    Err(error) => this.failed(format!("Neovim could not start: {error:#}. Configure Neovim in Settings, then restart the editor."), cx),
                }
                cx.notify();
            });
        }).detach();
    }

    fn failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.editor_error = Some(error.clone());
            state.notice(NoticeLevel::Error, error);
            cx.notify();
        });
        cx.emit(PromptEditorChanged);
        cx.notify();
    }

    fn commit_on_focus_loss(&mut self, cx: &mut Context<Self>) {
        let Some(DraftKey::Thread(id)) = self.active.as_ref() else {
            return;
        };
        let id = *id;
        if self.switching {
            // The switch owns the previous buffer's semantic snapshot and commit.
            return;
        }
        // Do not take a semantic snapshot here: it exits insert mode. Late
        // DraftChanged events commit after blur; IME recovery stays separate.
        self.state.update(cx, |state, cx| {
            if state.commit_thread_edits(id) {
                cx.emit(crate::app::PresentationChange::Persistence);
            }
        });
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switching
            || self.has_composition()
            || self.state.read(cx).editor_error.is_some()
            || self.desired.as_ref().map(|(key, _)| key) == self.active.as_ref()
        {
            return;
        }
        let Some(editor) = self.state.read(cx).editor.clone() else {
            return;
        };
        let desired = self.desired.clone();
        let previous = self.active.clone();
        self.switching = true;
        if let Some(view) = &self.view {
            view.update(cx, |view, cx| view.set_writable(false, cx));
        }
        self.switch_task = Some(cx.spawn_in(window, async move |this, window| {
            let result = async {
                let previous = if let Some(key) = previous { Some(editor.snapshot(key).await?) } else { None };
                if let Some((key, text)) = &desired {
                    editor.open_draft(key.clone(), text.clone()).await?;
                    editor.focus_draft(key.clone()).await?;
                }
                Ok::<_, anyhow::Error>(previous)
            }.await;
            let _ = this.update_in(window, |this, window, cx| {
                this.switching = false;
                match result {
                    Ok(previous) => {
                        if let Some(draft) = previous {
                            this.state.update(cx, |state, cx| {
                                let id = match &draft.key {
                                    DraftKey::Thread(id) | DraftKey::Extension { thread: id, .. } => *id,
                                };
                                state.record_draft(draft);
                                if state.commit_thread_edits(id) {
                                    cx.emit(crate::app::PresentationChange::Persistence);
                                }
                            });
                        }
                        this.active = desired.map(|(key, _)| key);
                        this.sync(window, cx);
                    }
                    Err(error) => this.failed(format!("Draft switch cancelled: {error:#}. Cancel the editor command or restart Neovim."), cx),
                }
                cx.emit(PromptEditorChanged);
                cx.notify();
            });
        }));
    }

    fn activate(
        &mut self,
        desired: Option<(DraftKey, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.desired = desired;
        self.sync(window, cx);
    }

    pub fn snapshot(
        &self,
        key: &DraftKey,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl Future<Output = anyhow::Result<DraftSnapshot>> + use<> {
        let key = key.clone();
        let editor = self.state.read(cx).editor.clone();
        let blocked =
            self.has_composition() || self.switching || self.active.as_ref() != Some(&key);
        async move {
            anyhow::ensure!(
                !blocked,
                "Wait for draft activation and complete or cancel input composition"
            );
            editor
                .ok_or_else(|| anyhow::anyhow!("Neovim is not available; restart the editor"))?
                .snapshot(key)
                .await
        }
    }
}

impl Focusable for PromptEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view
            .as_ref()
            .map(|view| view.focus_handle(cx))
            .unwrap_or_else(|| self.focus.clone())
    }
}

impl Render for PromptEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let writable =
            !self.switching
                && self.active.as_ref().is_some_and(|key| {
                    let (DraftKey::Thread(id) | DraftKey::Extension { thread: id, .. }) = key;
                    !state.shutting_down
                        && state.storage_error.is_none()
                        && state.editor_error.is_none()
                        && state.snapshot.threads.iter().any(|thread| {
                            thread.id == *id
                                && !thread.archived
                                && state.snapshot.projects.iter().any(|project| {
                                    project.id == thread.project_id && !project.archived
                                })
                        })
                });
        let error = state.editor_error.clone();
        if let Some(view) = &self.view {
            view.update(cx, |view, cx| view.set_writable(writable, cx));
        }
        div()
            .id("prompt-editor")
            .key_context("NativeInput PromptEditor")
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if self.focus_handle(cx).is_focused(window) {
                cx.theme().ring
            } else {
                cx.theme().input
            })
            .bg(cx.theme().background)
            .overflow_hidden()
            .when_some(self.view.clone(), |surface, view| {
                surface.child(view.cached(StyleRefinement::default().size_full()))
            })
            .when(self.view.is_none(), |surface| {
                if let Some(error) = error {
                    surface.child(Alert::error("neovim-startup-error", error))
                } else {
                    surface.child(Label::new("Starting Neovim…"))
                }
            })
    }
}

impl Workspace {
    fn can_edit_conversation(&self, id: ThreadId, cx: &App) -> bool {
        let state = self.state.read(cx);
        !state.shutting_down
            && state.storage_error.is_none()
            && state.snapshot.threads.iter().any(|thread| {
                thread.id == id
                    && thread.kind == ThreadKind::Conversation
                    && !thread.archived
                    && state
                        .snapshot
                        .projects
                        .iter()
                        .any(|project| project.id == thread.project_id && !project.archived)
            })
    }

    pub fn start_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_none() && !self.state.read(cx).shutting_down {
            let state = self.state.clone();
            let editor = cx.new(|cx| PromptEditor::new(state, cx));
            editor.update(cx, |editor, cx| editor.start(window, cx));
            let recovery_editor = editor.clone();
            let recovery_state = self.state.clone();
            App::on_app_quit(cx, move |cx| {
                let editor = recovery_editor.read(cx);
                let text = editor.recovery_text(cx);
                let key = editor.active_key().cloned();
                if !recovery_state.read(cx).shutdown_complete && !text.is_empty() {
                    recovery_state.update(cx, |state, _| {
                        state.set_setting(
                            "recovered_composition",
                            serde_json::json!({ "key": key, "text": text }),
                        );
                    });
                }
                async {}
            })
            .detach();
            self._subscriptions.push(cx.subscribe_in(
                &editor,
                window,
                |workspace, _, _: &PromptEditorChanged, window, cx| {
                    workspace.sync_session(window, cx);
                    workspace.state.update(cx, |state, cx| {
                        if let Some(id) = state.selected_thread {
                            cx.emit(crate::app::PresentationChange::Draft(id));
                        }
                    });
                },
            ));
            self.editor = Some(editor);
        }
        self.sync_session(window, cx);
    }

    pub fn sync_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).shutting_down {
            return;
        }
        if self.extension.as_ref().is_some_and(|form| {
            !self
                .state
                .read(cx)
                .conversations
                .get(&form.thread)
                .is_some_and(|session| session.requests.contains_key(&form.request.id))
        }) {
            self.extension = None;
            self.editor_override = None;
        }
        if let Some(id) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| thread.id)
        {
            self.state
                .update(cx, |state, _| state.ensure_conversation(id));
        }
        let desired = self.editor_override.clone().or_else(|| {
            self.state
                .read(cx)
                .conversation_thread()
                .filter(|thread| !thread.archived)
                .map(|thread| {
                    let key = DraftKey::Thread(thread.id);
                    let text = if self.editor_selection.as_ref() == Some(&key) {
                        String::new()
                    } else {
                        thread.draft.clone()
                    };
                    (key, text)
                })
        });
        if self.editor.is_none() {
            return;
        }
        if let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| editor.activate(desired, window, cx));
            self.editor_selection = editor.read(cx).active_key().cloned();
        }
    }

    pub fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self
            .state
            .read(cx)
            .conversation_thread()
            .is_some_and(|thread| self.can_edit_conversation(thread.id, cx))
        {
            return;
        }
        self.focus_region(Region::Thread, window, cx);
        self.sync_session(window, cx);
        if let Some(editor) = &self.editor {
            editor.update(cx, |view, cx| view.focus(window, cx));
        } else {
            self.start_editor(window, cx);
        }
    }
    pub fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.extension.is_some() {
            self.confirm_extension(window, cx);
            return;
        }
        let Some(id) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| thread.id)
        else {
            return;
        };
        if self.editor_selection != Some(DraftKey::Thread(id)) || self.editor_override.is_some() {
            self.state.update(cx, |state, cx| {
                state.notice(
                    NoticeLevel::Warning,
                    "Wait for the selected conversation draft to activate",
                );
                cx.notify();
            });
            return;
        }
        if let Some(reason) = self.state.read(cx).submit_reason(id) {
            self.state.update(cx, |state, cx| {
                state.notice(NoticeLevel::Warning, reason);
                cx.notify();
            });
            return;
        }
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let snapshot = editor.update(cx, |editor, cx| {
            editor.snapshot(&DraftKey::Thread(id), window, cx)
        });
        self.state.update(cx, |state, cx| {
            if let Some(session) = state.conversations.get_mut(&id) {
                session.preparing = true;
                session.error = None;
            }
            state.active_threads.insert(id);
            cx.notify();
        });
        cx.spawn_in(window, async move |workspace, window| {
            let result = snapshot.await;
            let _ = workspace.update_in(window, |workspace, _, cx| {
                workspace.state.update(cx, |state, cx| {
                    match result {
                        Ok(snapshot) => state.submit_snapshot(id, snapshot, cx),
                        Err(error) => {
                            state.submission_failed(id, format!("Nothing was sent: {error:#}"))
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        editor.update(cx, |editor, cx| editor.focus(window, cx));
    }

    pub fn restore_submitted(&mut self, id: ThreadId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit_conversation(id, cx) {
            return;
        }
        let Some(submission) = self
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.submission.clone())
        else {
            return;
        };
        let text = submission
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let attachments: Vec<ImageAttachment> = submission
            .get("attachments")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        self.replace_draft_reviewed(id, text, Some(attachments), true, window, cx);
    }
    pub fn replace_draft_reviewed(
        &mut self,
        id: ThreadId,
        text: String,
        attachments: Option<Vec<ImageAttachment>>,
        recovery: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit_conversation(id, cx) {
            return;
        }
        let Some(view) = self.editor.clone() else {
            return;
        };
        let Some(editor) = self.state.read(cx).editor.clone() else {
            return;
        };
        let key = DraftKey::Thread(id);
        let reviewed = view.update(cx, |view, cx| view.snapshot(&key, window, cx));
        let reviewed_attachments: Vec<_> = self
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .into_iter()
            .flat_map(|thread| &thread.attachments)
            .map(|image| image.id)
            .collect();
        cx.spawn_in(window, async move |workspace, window| {
            let result = async {
                let reviewed = reviewed.await?;
                let approval = workspace.update_in(window, |_, window, cx| {
                    window.prompt(
                        PromptLevel::Warning,
                        "Replace this conversation draft?",
                        Some("New typing after this review will not be overwritten."),
                        &["Replace draft", "Cancel"],
                        cx,
                    )
                })?;
                if approval.await != Ok(0) {
                    return Ok(false);
                }
                let unchanged = workspace.update_in(window, |workspace, _, cx| {
                    workspace.can_edit_conversation(id, cx)
                        && workspace
                            .state
                            .read(cx)
                            .snapshot
                            .threads
                            .iter()
                            .find(|thread| thread.id == id)
                            .is_some_and(|thread| {
                                thread
                                    .attachments
                                    .iter()
                                    .map(|image| image.id)
                                    .eq(reviewed_attachments.iter().copied())
                            })
                })?;
                anyhow::ensure!(unchanged, "The draft or attachments are no longer editable");
                anyhow::ensure!(
                    editor
                        .replace_if_revision(key, reviewed.revision(), text)
                        .await?,
                    "Draft changed during review; newer text was preserved"
                );
                Ok::<_, anyhow::Error>(true)
            }
            .await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.state.update(cx, |state, cx| {
                    match result {
                        Ok(true) => {
                            if let Some(mut thread) = state
                                .snapshot
                                .threads
                                .iter()
                                .find(|thread| thread.id == id)
                                .cloned()
                                && thread
                                    .attachments
                                    .iter()
                                    .map(|image| image.id)
                                    .eq(reviewed_attachments.iter().copied())
                            {
                                if let Some(attachments) = attachments {
                                    thread.attachments = attachments;
                                }
                                if recovery {
                                    thread.submission = None;
                                }
                                state.save_thread(thread);
                            }
                        }
                        Ok(false) => {}
                        Err(error) => state.notice(
                            NoticeLevel::Warning,
                            format!("Draft was not replaced: {error:#}"),
                        ),
                    }
                    cx.notify();
                });
                if workspace.editor_selection == Some(DraftKey::Thread(id)) {
                    view.update(cx, |view, cx| view.focus(window, cx));
                }
            });
        })
        .detach();
    }

    pub fn attach_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| thread.id)
        else {
            return;
        };
        if !self.can_edit_conversation(id, cx) {
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach PNG, JPEG or WebP images".into()),
        });
        cx.spawn_in(window, async move |workspace, window| {
            let result = picked.await;
            let _ = workspace.update_in(window, |workspace, window, cx| match result {
                Ok(Ok(Some(paths))) => workspace.import_images(id, paths, window, cx),
                Ok(Ok(None)) => {}
                other => workspace.state.update(cx, |state, cx| {
                    state.notice(
                        NoticeLevel::Error,
                        format!("Image selection failed: {other:?}"),
                    );
                    cx.notify();
                }),
            });
        })
        .detach();
    }
    /// Attaches whatever the native clipboard holds: copied image bytes or
    /// copied image files. Text is left to the editor.
    pub fn paste_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| thread.id)
        else {
            return;
        };
        if !self.can_edit_conversation(id, cx) {
            return;
        }
        let mut images = Vec::new();
        let mut paths = Vec::new();
        if let Some(item) = cx.read_from_clipboard() {
            for entry in item.into_entries() {
                match entry {
                    ClipboardEntry::Image(image) => images.push(image.bytes),
                    ClipboardEntry::ExternalPaths(external) => {
                        paths.extend(external.paths().iter().cloned())
                    }
                    ClipboardEntry::String(_) => {}
                }
            }
        }
        if images.is_empty() && paths.is_empty() {
            self.state.update(cx, |state, cx| {
                state.notice(
                    NoticeLevel::Warning,
                    "The clipboard holds no image. Copy an image or use Attach…",
                );
                cx.notify();
            });
            return;
        }
        if !paths.is_empty() {
            self.import_images(id, paths, window, cx);
        }
        if !images.is_empty() {
            let data = self.state.read(cx).data_dir.clone();
            self.import_attachments(
                id,
                move || {
                    images
                        .iter()
                        .map(|bytes| crate::attachments::import_pasted(bytes, &data))
                        .collect()
                },
                window,
                cx,
            );
        }
    }
    pub fn import_images(
        &mut self,
        id: ThreadId,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let data = self.state.read(cx).data_dir.clone();
        self.import_attachments(
            id,
            move || {
                paths
                    .iter()
                    .map(|path| crate::attachments::import(path, &data))
                    .collect()
            },
            window,
            cx,
        );
    }
    /// Runs `import` off the GPUI thread and appends accepted images to the
    /// thread's draft attachments; rejections become notices.
    fn import_attachments(
        &mut self,
        id: ThreadId,
        import: impl FnOnce() -> Vec<anyhow::Result<ImageAttachment>> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit_conversation(id, cx) {
            return;
        }
        cx.spawn_in(window, async move |workspace, window| {
            let results = smol::unblock(import).await;
            let _ = workspace.update_in(window, |workspace, _, cx| {
                if !workspace.can_edit_conversation(id, cx) {
                    return;
                }
                workspace.state.update(cx, |state, cx| {
                    if let Some(mut thread) = state
                        .snapshot
                        .threads
                        .iter()
                        .find(|thread| thread.id == id)
                        .cloned()
                    {
                        for result in results {
                            match result {
                                Ok(image) => thread.attachments.push(image),
                                Err(error) => state.notice(
                                    NoticeLevel::Error,
                                    format!("Attachment rejected: {error:#}"),
                                ),
                            }
                        }
                        state.save_thread(thread);
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }
    pub fn optimize_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.state.read(cx).conversation_thread().cloned() else {
            return;
        };
        if !self.can_edit_conversation(thread.id, cx) {
            return;
        }
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let snapshot = editor.update(cx, |editor, cx| {
            editor.snapshot(&DraftKey::Thread(thread.id), window, cx)
        });
        let data = self.state.read(cx).data_dir.clone();
        let original_ids: Vec<_> = thread.attachments.iter().map(|image| image.id).collect();
        cx.spawn_in(window, async move |workspace, window| {
            let result = async {
                let snapshot = snapshot.await?;
                smol::unblock(move || crate::attachments::optimize(uuid::Uuid::new_v4(), &snapshot.text, &thread.attachments, &data)).await
            }.await;
            let _ = workspace.update_in(window, |workspace, _, cx| {
                if !workspace.can_edit_conversation(thread.id, cx) {
                    return;
                }
                workspace.state.update(cx, |state, cx| {
                    match result {
                        Ok(prepared) => if let Some(mut current) = state.snapshot.threads.iter().find(|t| t.id == thread.id).cloned() {
                            if current.attachments.iter().map(|image| image.id).collect::<Vec<_>>() == original_ids { current.attachments = prepared; state.save_thread(current); state.notice(NoticeLevel::Info, "Prepared JPEG copies are shown below. Review their actual dimensions and previews before Send; originals are retained."); }
                            else { state.notice(NoticeLevel::Warning, "Attachments changed during optimization; current attachments were preserved"); }
                        },
                        Err(error) => state.notice(NoticeLevel::Error, format!("Images were not prepared: {error:#}")),
                    }
                    cx.notify();
                });
            });
        }).detach();
    }
}

impl Workspace {
    pub fn restart_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.editor.clone() else {
            self.start_editor(window, cx);
            return;
        };
        let Some(editor) = self.state.read(cx).editor.clone() else {
            view.update(cx, |view, cx| view.start(window, cx));
            return;
        };
        view.update(cx, |view, _| {
            // Cancel queued continuations before restarting the RPC generation.
            view.switch_task = None;
            view.switching = false;
        });
        self.state.update(cx, |state, cx| {
            state.editor_error = Some("Restarting Neovim…".into());
            cx.notify();
        });
        cx.spawn_in(window, async move |workspace, window| {
            let result = editor.restart().await;
            let _ = workspace.update_in(window, |workspace, window, cx| match result {
                Ok(()) => {
                    workspace.state.update(cx, |state, cx| {
                        state.editor_error = None;
                        cx.notify();
                    });
                    view.update(cx, |view, cx| view.sync(window, cx));
                }
                Err(error) => view.update(cx, |view, cx| {
                    view.failed(format!("Neovim restart failed: {error:#}"), cx)
                }),
            });
        })
        .detach();
    }

    pub fn cancel_editor_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = &self.editor {
            let recovery = view.read(cx).recovery_text(cx);
            let key = view.read(cx).active_key().cloned();
            if !recovery.is_empty() {
                self.state.update(cx, |state, _| {
                    state.set_setting(
                        "recovered_composition",
                        serde_json::json!({ "key": key, "text": recovery }),
                    )
                });
            }
            if let Some(surface) = view.read(cx).view.clone() {
                surface.update(cx, |surface, cx| surface.cancel_composition(window, cx));
            }
        }
        let Some(editor) = self.state.read(cx).editor.clone() else {
            return;
        };
        cx.spawn_in(window, async move |workspace, window| {
            let result = editor.cancel_command().await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.state.update(cx, |state, cx| {
                    match result {
                        Ok(()) => state.editor_error = None,
                        Err(error) => state.notice(
                            NoticeLevel::Error,
                            format!("Neovim requires restart: {error:#}"),
                        ),
                    }
                    cx.notify();
                });
                workspace.sync_session(window, cx);
            });
        })
        .detach();
    }

    pub fn copy_draft_recovery(&self, cx: &mut App) {
        let state = self.state.read(cx);
        let text = serde_json::json!({
            "threads": state.snapshot.threads,
            "editor_buffers": state.drafts.values().map(|draft| serde_json::json!({ "key": format!("{:?}", draft.key), "text": draft.text })).collect::<Vec<_>>(),
            "saved_uncommitted_preedit": state.snapshot.settings.get("recovered_composition"),
            "current_uncommitted_preedit": self.editor.as_ref().map(|view| view.read(cx).recovery_text(cx)),
        });
        cx.write_to_clipboard(ClipboardItem::new_string(
            serde_json::to_string_pretty(&text).unwrap_or_else(|_| text.to_string()),
        ));
    }

    fn begin_orderly_quit(
        &mut self,
        allow_data_loss: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_layout(cx);
        if let Some(view) = &self.editor {
            let text = view.read(cx).recovery_text(cx);
            let key = view.read(cx).active_key().cloned();
            if !text.is_empty() {
                self.state.update(cx, |state, _| {
                    state.set_setting(
                        "recovered_composition",
                        serde_json::json!({ "key": key, "text": text }),
                    )
                });
            }
        }
        self.focus_region(Region::Thread, window, cx);
        let editor = self.state.read(cx).editor.clone();
        cx.spawn_in(window, async move |workspace, window| {
            let snapshots = if let Some(editor) = &editor {
                editor.persistence_barrier().await
            } else { Ok(Vec::new()) };
            if snapshots.is_err() && !allow_data_loss
                && let Some(editor) = &editor { let _ = editor.resume_input().await; }
            let _ = workspace.update_in(window, |workspace, _, cx| {
                workspace.quit_requested = false;
                workspace.state.update(cx, |state, cx| {
                    match snapshots {
                        Ok(drafts) => {
                            for draft in drafts { state.record_draft(draft); }
                            state.begin_shutdown(allow_data_loss, cx);
                        }
                        Err(_) if allow_data_loss => state.begin_shutdown(true, cx),
                        Err(error) => {
                            let error = format!("Quit cancelled: {error:#}. Complete or cancel composition, or copy draft recovery before quitting.");
                            state.shutdown_error = Some(error.clone());
                            state.notice(NoticeLevel::Error, error);
                            cx.notify();
                        }
                    }
                });
            });
        }).detach();
    }

    pub fn quit_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).shutting_down || self.quit_requested {
            return;
        }
        self.quit_requested = true;
        let state = self.state.read(cx);
        let allow_data_loss = state.storage_error.is_some() || state.shutdown_error.is_some();
        if state.active_threads.is_empty() && state.pending_factory.is_empty() && !allow_data_loss {
            self.begin_orderly_quit(false, window, cx);
            return;
        }
        let message = if allow_data_loss {
            "Some edits could not be saved. Copy Draft Recovery before quitting to retain them. Active interactive turns stop; Factory jobs already started continue."
        } else {
            "Active interactive turns will stop. Saved drafts and session references are retained. Factory jobs already started continue; a pending dispatch may require outcome review after reopening."
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            "Quit DROSTJE.?",
            Some(message),
            &["Cancel", "Quit"],
            cx,
        );
        cx.spawn_in(window, async move |workspace, window| {
            let confirmed = answer.await == Ok(1);
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.quit_requested = false;
                if confirmed {
                    workspace.begin_orderly_quit(allow_data_loss, window, cx);
                } else {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
