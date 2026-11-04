use super::{
    nvim::{CaretPopupKey, NvimView, NvimViewEvent},
    slash_commands::{SlashCompletion, SlashCompletionEvent, completion_query},
    workspace::{Region, Workspace},
};
use crate::{
    app::{AppState, NoticeLevel},
    model::*,
    neovim::{NvimConfig, NvimEditor},
    platform,
};
use ghostty_terminal::input_surface::native_input_context;
use gpui_kit::component::{
    ActiveTheme, WindowExt, alert::Alert, dialog::DialogButtonProps, h_flex, label::Label,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;
use uuid::Uuid;

/// Work that follows a successful draft replacement, run on the app state.
pub type DraftContinuation = Box<dyn FnOnce(&mut AppState) + 'static>;

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
    /// Slash-command completion for the active Conversation draft, created on first use.
    completion: Option<Entity<SlashCompletion>>,
    /// The query the user closed the popup for; it stays closed until the query changes.
    dismissed_query: Option<String>,
    /// See `command_note`; kept so the header redraws only when the note changes.
    command_note: Option<(String, bool)>,
}

pub(super) struct PromptEditorChanged;
impl EventEmitter<PromptEditorChanged> for PromptEditor {}

impl PromptEditor {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self {
            state: state.clone(),
            view: None,
            desired: None,
            active: None,
            switching: false,
            opening: false,
            composition: false,
            window_inactive: false,
            focus: cx.focus_handle(),
            _subscriptions: vec![cx.subscribe(
                &state,
                |this, _, change: &crate::app::PresentationChange, cx| {
                    // Command lists arrive with conversation events.
                    if matches!(change, crate::app::PresentationChange::Thread(_)) {
                        this.sync_completion(cx);
                    }
                },
            )],
            switch_task: None,
            completion: None,
            dismissed_query: None,
            command_note: None,
        }
    }

    /// The note beside the draft label, for native QA.
    #[cfg(all(test, feature = "runtime-qa"))]
    pub fn command_note_text(&self) -> Option<&str> {
        self.command_note.as_ref().map(|(note, _)| note.as_str())
    }

    /// The command the popup would insert, for native QA.
    #[cfg(all(test, feature = "runtime-qa"))]
    pub fn completion_selection(&self, cx: &App) -> Option<String> {
        self.completion
            .as_ref()
            .and_then(|completion| completion.read(cx).selected())
            .map(|command| command.name.clone())
    }

    /// What the draft's leading slash command needs, shown beside the draft label: why
    /// DROSTJE. will refuse it, or its argument syntax until arguments are typed. The flag
    /// says whether the note is command syntax.
    fn command_note(&self, state: &AppState) -> Option<(String, bool)> {
        let Some(DraftKey::Thread(id)) = &self.active else {
            return None;
        };
        let draft = state.drafts.get(&DraftKey::Thread(*id))?;
        let catalog = &state.conversations.get(id)?.slash_commands;
        if let Some(refusal) = catalog.refusal(&draft.text) {
            return Some((refusal, false));
        }
        let command = catalog.invoked(&draft.text)?;
        let (_, arguments) = crate::harness::invocation(&draft.text)?;
        let hint = command
            .argument_hint
            .as_ref()
            .filter(|_| arguments.is_empty())?;
        Some((format!("/{} {hint}", command.name), true))
    }

    /// Shows the command popup while the active Conversation draft starts with a partly
    /// typed `/command` in insert mode, and hides it otherwise.
    fn sync_completion(&mut self, cx: &mut Context<Self>) {
        let note = self.command_note(self.state.read(cx));
        if note != self.command_note {
            self.command_note = note;
            cx.notify();
        }
        let Some(view) = self.view.clone() else {
            return;
        };
        let offer = match &self.active {
            Some(DraftKey::Thread(id)) if !self.switching && !self.composition => {
                let state = self.state.read(cx);
                (view.read(cx).mode() == "insert")
                    .then(|| state.drafts.get(&DraftKey::Thread(*id)))
                    .flatten()
                    .and_then(|draft| completion_query(&draft.text, draft.cursor))
                    .and_then(|query| {
                        let session = state.conversations.get(id)?;
                        Some((*id, query.to_owned(), session.slash_commands.clone()))
                    })
            }
            _ => None,
        };
        let Some((id, query, catalog)) = offer else {
            self.dismissed_query = None;
            view.update(cx, |view, cx| view.set_caret_popup(None, cx));
            return;
        };
        if self.dismissed_query.as_ref() == Some(&query) {
            view.update(cx, |view, cx| view.set_caret_popup(None, cx));
            return;
        }
        self.dismissed_query = None;
        let completion = match &self.completion {
            Some(completion) => completion.clone(),
            None => {
                let completion = cx.new(|_| SlashCompletion::new(id));
                self._subscriptions.push(cx.subscribe(
                    &completion,
                    |this, _, event: &SlashCompletionEvent, cx| match event {
                        SlashCompletionEvent::Accept(name) => {
                            this.accept_completion(name.clone(), cx)
                        }
                    },
                ));
                self.completion = Some(completion.clone());
                completion
            }
        };
        completion.update(cx, |completion, cx| {
            completion.set(id, &query, &catalog, cx)
        });
        view.update(cx, |view, cx| {
            view.set_caret_popup(Some(completion.clone().into()), cx)
        });
    }

    fn completion_key(&mut self, key: CaretPopupKey, cx: &mut Context<Self>) {
        let Some(completion) = self.completion.clone() else {
            return;
        };
        match key {
            CaretPopupKey::Next => completion.update(cx, |completion, cx| completion.select(1, cx)),
            CaretPopupKey::Previous => {
                completion.update(cx, |completion, cx| completion.select(-1, cx))
            }
            CaretPopupKey::Accept => {
                if let Some(name) = completion
                    .read(cx)
                    .selected()
                    .map(|command| command.name.clone())
                {
                    self.accept_completion(name, cx);
                }
            }
            CaretPopupKey::Dismiss => {
                self.dismissed_query = Some(completion.read(cx).query().to_owned());
                self.sync_completion(cx);
            }
        }
    }

    /// Replaces the typed `/partial` with the chosen command. The draft change that follows
    /// moves the cursor past the command, which closes the popup.
    fn accept_completion(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(DraftKey::Thread(id)) = self.active.clone() else {
            return;
        };
        if self
            .completion
            .as_ref()
            .is_none_or(|completion| completion.read(cx).thread() != id)
        {
            return;
        }
        let Some(editor) = self.state.read(cx).editor.clone() else {
            return;
        };
        let state = self.state.clone();
        cx.spawn(async move |_, cx| {
            if let Err(error) = editor
                .insert_slash_command(DraftKey::Thread(id), name)
                .await
            {
                state.update(cx, |state, cx| {
                    state.notice(
                        NoticeLevel::Warning,
                        format!("The command was not inserted: {error:#}"),
                    );
                    cx.notify();
                });
            }
        })
        .detach();
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
                        this._subscriptions.push(cx.subscribe_in(&view, window, |this, view, event: &NvimViewEvent, window, cx| {
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
                                    this.sync_completion(cx);
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
                                NvimViewEvent::ModeChanged => {
                                    this.sync_completion(cx);
                                    return;
                                }
                                NvimViewEvent::CaretPopupKey(key) => {
                                    this.completion_key(*key, cx);
                                    return;
                                }
                                // Same Action as Cmd-Escape; a late request after focus moved is moot.
                                NvimViewEvent::Leave => {
                                    if view.focus_handle(cx).is_focused(window) {
                                        window.dispatch_action(Box::new(super::commands::LeaveEditor), cx);
                                    }
                                    return;
                                }
                            }
                            cx.emit(PromptEditorChanged);
                            cx.notify();
                        }));
                        let focus = view.focus_handle(cx);
                        this._subscriptions.push(cx.on_focus(&focus, window, |_, _, cx| {
                            cx.notify();
                        }));
                        this._subscriptions.push(cx.on_blur(&focus, window, |this, _, cx| {
                            this.commit_on_focus_loss(cx);
                            cx.notify();
                        }));
                        this._subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
                            // An inactive window can retain the editor's focus handle.
                            this.window_inactive = !window.is_window_active();
                            if this.window_inactive {
                                this.commit_on_focus_loss(cx);
                            }
                            cx.notify();
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
        let focused = window.is_window_active() && self.focus_handle(cx).is_focused(window);
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
        let command_note = self.command_note.clone();
        if let Some(view) = &self.view {
            view.update(cx, |view, cx| view.set_writable(writable, cx));
        }
        div()
            .id("prompt-editor")
            .test_support()
            .key_context(native_input_context("PromptEditor"))
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            // Keep a steady focus cue even while Neovim's caret is blinking off.
            // Blurring never changes the buffer, Vim mode, or composition.
            .bg(cx.theme().background)
            .overflow_hidden()
            .border_1()
            .border_color(if focused {
                cx.theme().input
            } else {
                cx.theme().transparent
            })
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .px_2()
                    .py_0p5()
                    .text_xs()
                    .bg(if focused {
                        cx.theme().muted
                    } else {
                        cx.theme().transparent
                    })
                    .child(
                        Label::new(if focused {
                            "Draft · Editing"
                        } else {
                            "Draft · Not focused"
                        })
                        .flex_none()
                        .text_color(if focused {
                            cx.theme().foreground
                        } else {
                            cx.theme().muted_foreground
                        }),
                    )
                    .when_some(command_note, |header, (note, code)| {
                        header.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(cx.theme().muted_foreground)
                                .when(code, |note| {
                                    note.font_family(cx.theme().mono_font_family.clone())
                                })
                                .child(note),
                        )
                    }),
            )
            .when_some(self.view.clone(), |surface, view| {
                surface.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(view.cached(StyleRefinement::default().size_full())),
                )
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
    /// Opens the picker over the composer. An inactive draft is activated first so the
    /// chosen command can land in it; Root returns focus to the composer on dismiss.
    pub fn open_slash_command_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| thread.id)
        else {
            return;
        };
        self.dismiss(window, cx);
        if self.navigation_blocked(window, cx) {
            return;
        }
        if self.editor_selection != Some(DraftKey::Thread(id)) {
            self.focus_composer(window, cx);
        }
        self.palette = Some(super::commands::Palette::open_slash_commands(
            cx.entity().downgrade(),
            self.state.clone(),
            id,
            window,
            cx,
        ));
        cx.notify();
    }

    /// Writes `/name ` at the start of the Conversation's draft, replacing a partly typed
    /// command, and leaves the cursor after it in insert mode. Nothing is sent.
    pub fn insert_slash_command(
        &mut self,
        id: ThreadId,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = DraftKey::Thread(id);
        let warn = |workspace: &mut Self, text: String, cx: &mut Context<Self>| {
            workspace.state.update(cx, |state, cx| {
                state.notice(NoticeLevel::Warning, text);
                cx.notify();
            });
        };
        if self.editor_selection.as_ref() != Some(&key) || self.editor_override.is_some() {
            warn(
                self,
                "Wait for the conversation draft to activate, then insert the command again".into(),
                cx,
            );
            return;
        }
        let Some(editor) = self.state.read(cx).editor.clone() else {
            warn(
                self,
                "Neovim is not available; restart the editor".into(),
                cx,
            );
            return;
        };
        let name = name.to_owned();
        cx.spawn_in(window, async move |workspace, window| {
            let result = editor.insert_slash_command(key, name).await;
            let _ = workspace.update_in(window, |workspace, window, cx| match result {
                Ok(()) => workspace.focus_composer(window, cx),
                Err(error) => warn(
                    workspace,
                    format!("The command was not inserted: {error:#}"),
                    cx,
                ),
            });
        })
        .detach();
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
            // A running turn turns Submit into Queue; every other reason stays a reason.
            if self.state.read(cx).queue_reason(id).is_none() {
                self.queue_prompt(id, window, cx);
                return;
            }
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

    /// Moves the composer draft to the thread's queue; the editor stays focused and empty.
    pub fn queue_prompt(&mut self, id: ThreadId, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_selection != Some(DraftKey::Thread(id)) || self.editor_override.is_some() {
            return;
        }
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let snapshot = editor.update(cx, |editor, cx| {
            editor.snapshot(&DraftKey::Thread(id), window, cx)
        });
        cx.spawn_in(window, async move |workspace, window| {
            let result = snapshot.await;
            let _ = workspace.update_in(window, |workspace, _, cx| {
                workspace.state.update(cx, |state, cx| {
                    match result {
                        Ok(snapshot) => state.enqueue_snapshot(id, snapshot, cx),
                        Err(error) => state.notice(
                            NoticeLevel::Warning,
                            format!("Nothing was queued: {error:#}"),
                        ),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        editor.update(cx, |editor, cx| editor.focus(window, cx));
    }

    pub fn send_queued_now(&mut self, id: ThreadId, message_id: Uuid, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.send_queued_now(id, message_id, cx);
            cx.notify();
        });
    }

    /// Takes a queued message back into the composer. A non-empty draft is reviewed before
    /// it is replaced, exactly like a restored submission.
    pub fn edit_queued(
        &mut self,
        id: ThreadId,
        message_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit_conversation(id, cx) {
            return;
        }
        let Some(message) = self
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .and_then(|thread| thread.queued.iter().find(|m| m.id == message_id).cloned())
        else {
            return;
        };
        if self.editor_selection != Some(DraftKey::Thread(id)) || self.editor_override.is_some() {
            self.state.update(cx, |state, cx| {
                state.notice(
                    NoticeLevel::Warning,
                    "Focus this conversation's composer before editing a queued message",
                );
                cx.notify();
            });
            return;
        }
        self.replace_draft_reviewed_then(
            id,
            message.text,
            Some(message.attachments),
            false,
            Some(Box::new(move |state: &mut AppState| {
                state.remove_queued(id, message_id);
            })),
            window,
            cx,
        );
    }

    pub fn clear_queued_messages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, count)) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| (thread.id, thread.queued.len()))
        else {
            return;
        };
        if count == 0 || !self.can_edit_conversation(id, cx) {
            return;
        }
        let approval = window.prompt(
            PromptLevel::Warning,
            &match count {
                1 => "Remove the queued message?".to_owned(),
                count => format!("Remove all {count} queued messages?"),
            },
            Some("Their text is not kept anywhere else."),
            &["Remove", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |workspace, window| {
            if approval.await != Ok(0) {
                return;
            }
            let _ = workspace.update_in(window, |workspace, _, cx| {
                workspace.state.update(cx, |state, cx| {
                    state.clear_queued(id);
                    cx.notify();
                });
            });
        })
        .detach();
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
        self.replace_draft_reviewed_then(id, text, attachments, recovery, None, window, cx);
    }

    /// `then` runs on the app state once the editor text was replaced, before the view
    /// regains focus; a declined or failed replacement never runs it.
    #[expect(
        clippy::too_many_arguments,
        reason = "one replacement, one continuation"
    )]
    pub fn replace_draft_reviewed_then(
        &mut self,
        id: ThreadId,
        text: String,
        attachments: Option<Vec<ImageAttachment>>,
        recovery: bool,
        then: Option<DraftContinuation>,
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
                // An empty composer has nothing to lose, so it is replaced without a review.
                let blank = reviewed.text.trim().is_empty() && reviewed_attachments.is_empty();
                if !blank {
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
                            if let Some(then) = then {
                                then(state);
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
                // A rejected editor barrier cancels quit without killing terminal sessions.
                if snapshots.is_ok() || allow_data_loss {
                    workspace.close_all_terminals(cx);
                }
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
        if self.state.read(cx).shutting_down
            || self.quit_requested
            || self.terminal_confirmation
            || self
                .extension
                .as_ref()
                .is_some_and(|form| !form.inline_answer)
        {
            return;
        }
        let palette = self
            .palette
            .as_ref()
            .filter(|palette| palette.read(cx).hosted())
            .cloned();
        let form = self
            .dialog
            .as_ref()
            .filter(|dialog| dialog.read(cx).hosted())
            .cloned();
        let has_dialog = window.has_active_dialog(cx);
        let has_sheet = window.has_active_sheet(cx);
        let known_host = if palette.is_some() && form.is_none() {
            has_dialog && !has_sheet
        } else if let Some(form) = &form {
            match form.read(cx).kind {
                super::settings::DialogKind::Settings(_) => has_sheet && !has_dialog,
                _ => has_dialog && !has_sheet,
            }
        } else {
            false
        };
        // Only suspend our retained surfaces. Extension and terminal-risk prompts
        // must finish their own decision before Quit can replace their host.
        if (has_dialog || has_sheet) && !known_host {
            return;
        }
        self.quit_requested = true;
        let state = self.state.read(cx);
        let allow_data_loss = state.storage_error.is_some() || state.shutdown_error.is_some();
        if state.active_threads.is_empty()
            && state.pending_factory.is_empty()
            && !allow_data_loss
            && !self.has_running_terminals(cx)
        {
            self.dismiss(window, cx);
            self.begin_orderly_quit(false, window, cx);
            return;
        }
        let mut message = if allow_data_loss {
            "Some edits could not be saved. Copy Draft Recovery before quitting to retain them. Active interactive turns stop; Factory jobs already started continue.".to_owned()
        } else {
            "Saved drafts and session references are retained. Active interactive turns stop. Factory jobs already started continue; a pending dispatch may require outcome review after reopening.".to_owned()
        };
        if self.has_running_terminals(cx) {
            message.push_str(" All terminal shells and their foreground jobs will be stopped. Terminal tabs reopen with a new shell; running jobs and scrollback are not restored.");
        }
        let previous_focus = window.focused(cx);
        if let Some(palette) = &palette {
            palette.update(cx, |palette, cx| palette.suspend(window, cx));
        }
        if let Some(form) = &form {
            let kind = form.read(cx).kind.clone();
            form.update(cx, |form, cx| form.unhost(cx));
            super::settings::close_surface(&kind, window, cx);
        }
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, window, _| {
            let confirm = owner.clone();
            let close = owner.clone();
            let palette = palette.clone();
            let form = form.clone();
            let previous_focus = previous_focus.clone();
            alert
                .title("Quit DROSTJE.")
                .description(message.clone())
                .width((window.rem_size() * 32.).min(window.viewport_size().width))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Quit")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let confirm = confirm.clone();
                    window.defer(cx, move |window, cx| {
                        // Close the quit alert before dismissing the retained workspace
                        // surface; this avoids closing the same modal layer twice.
                        window.close_dialog(cx);
                        let _ = confirm.update(cx, |workspace, cx| {
                            workspace.quit_requested = true;
                            workspace.dismiss(window, cx);
                            workspace.begin_orderly_quit(allow_data_loss, window, cx);
                        });
                    });
                    false
                })
                .on_cancel(move |_, window, cx| {
                    let close = close.clone();
                    let palette = palette.clone();
                    let form = form.clone();
                    let previous_focus = previous_focus.clone();
                    // Close only the alert before rehosting the retained surface.
                    // An automatic/deferred close could dismiss the replacement
                    // or restore background focus after the Settings sheet opens.
                    window.defer(cx, move |window, cx| {
                        window.close_dialog(cx);
                        let _ = close.update(cx, |workspace, cx| {
                            workspace.quit_requested = false;
                            if let Some(palette) = palette
                                .filter(|palette| workspace.palette.as_ref() == Some(palette))
                            {
                                palette.update(cx, |palette, cx| palette.reopen(window, cx));
                            }
                            if let Some(form) =
                                form.filter(|form| workspace.dialog.as_ref() == Some(form))
                            {
                                super::settings::open_surface(close.clone(), window, cx);
                                // open_surface defers host creation and initial focus.
                                // Restore the exact field after those are scheduled.
                                window.defer(cx, move |window, _cx| {
                                    window.on_next_frame(move |window, cx| {
                                        if form.read(cx).hosted()
                                            && let Some(focus) = previous_focus
                                        {
                                            focus.focus(window, cx);
                                        }
                                    });
                                });
                            }
                            cx.notify();
                        });
                    });
                    false
                })
        });
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod quit_tests {
    use crate::{app::AppState, model::WorkspaceSnapshot, ui::workspace::Workspace};
    use gpui_kit::{
        AnyWindowHandle, AppContext, Entity, Focusable, TestAppContext,
        component::{Root, WindowExt},
    };
    use uuid::Uuid;

    fn workspace_window(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workspace>) {
        cx.dispatcher.allow_parking();
        let data_dir = std::env::temp_dir().join(format!("drostje-quit-qa-{}", Uuid::new_v4()));
        std::fs::create_dir(&data_dir).unwrap();
        let store = crate::store::Store::open(data_dir.clone()).unwrap();
        let bootstrap = crate::app::Bootstrap {
            data_dir: data_dir.clone(),
            store,
            snapshot: WorkspaceSnapshot::default(),
            diagnostics: Default::default(),
            repositories: Default::default(),
            browser_tools: None,
        };
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, Some(&data_dir), cx);
            crate::ui::commands::bind_keys(cx);
        });

        let mut workspace = None;
        let window = cx.add_window(|window, cx| {
            let state = cx.new(|cx| AppState::new(bootstrap, cx));
            let view = cx.new(|cx| Workspace::new(state.clone(), window, cx));
            workspace = Some(view.clone());
            let root = cx.new(|_| super::super::workspace::WorkspaceRoot { workspace: view });
            Root::new(root, window, cx)
        });
        (window.into(), workspace.unwrap())
    }

    fn draw_frames(cx: &mut TestAppContext, window: AnyWindowHandle) {
        cx.run_until_parked();
        for _ in 0..3 {
            cx.update_window(window, |_, window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            })
            .unwrap();
            cx.run_until_parked();
        }
    }

    #[gpui_kit::test]
    fn cancelling_quit_keeps_the_open_palette(cx: &mut TestAppContext) {
        let (window, workspace) = workspace_window(cx);

        let (palette, command) = cx
            .update_window(window, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.open_palette(window, cx);
                    let palette = workspace.palette.clone().unwrap();
                    let command = palette.read(cx).command.clone();
                    command.update(cx, |command, cx| {
                        command.set_query("settings", window, cx);
                    });
                    workspace.state.update(cx, |state, _| {
                        state.active_threads.insert(Uuid::new_v4());
                    });
                    // Also exercise the palette's still-pending initial focus callback.
                    workspace.quit_review(window, cx);
                    assert!(workspace.quit_requested);
                    assert!(!palette.read(cx).hosted());
                    (palette, command)
                })
            })
            .unwrap();
        draw_frames(cx, window);
        cx.update_window(window, |_, window, cx| {
            assert!(window.has_active_dialog(cx));
            assert!(!command.focus_handle(cx).is_focused(window));
        })
        .unwrap();

        cx.simulate_keystrokes(window, "escape");
        draw_frames(cx, window);
        cx.update_window(window, |_, window, cx| {
            let workspace = workspace.read(cx);
            assert!(!workspace.quit_requested);
            assert_eq!(workspace.palette.as_ref(), Some(&palette));
            assert_eq!(palette.read(cx).command, command);
            assert!(palette.read(cx).hosted());
            assert_eq!(command.read(cx).query(cx).as_ref(), "settings");
            assert!(command.focus_handle(cx).is_focused(window));
            assert!(window.has_active_dialog(cx));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn cancelling_quit_keeps_the_settings_draft(cx: &mut TestAppContext) {
        let (window, workspace) = workspace_window(cx);
        cx.update_window(window, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.execute(super::super::commands::Command::Settings, window, cx);
            });
        })
        .unwrap();
        draw_frames(cx, window);

        let (form, input) = cx
            .update_window(window, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let form = workspace.dialog.clone().unwrap();
                    let input = form.read(cx).fields[0].input.clone();
                    input.update(cx, |input, cx| {
                        input.set_value("retained settings draft", window, cx);
                    });
                    input.focus_handle(cx).focus(window, cx);
                    workspace.state.update(cx, |state, _| {
                        state.active_threads.insert(Uuid::new_v4());
                    });
                    workspace.quit_review(window, cx);
                    assert!(workspace.quit_requested);
                    assert!(!form.read(cx).hosted());
                    assert!(!window.has_active_sheet(cx));
                    (form, input)
                })
            })
            .unwrap();
        draw_frames(cx, window);
        cx.update_window(window, |_, window, cx| {
            assert!(window.has_active_dialog(cx));
            assert!(!input.focus_handle(cx).is_focused(window));
        })
        .unwrap();

        cx.simulate_keystrokes(window, "escape");
        draw_frames(cx, window);
        cx.update_window(window, |_, window, cx| {
            let workspace = workspace.read(cx);
            assert!(!workspace.quit_requested);
            assert_eq!(workspace.dialog.as_ref(), Some(&form));
            assert!(form.read(cx).hosted());
            assert_eq!(input.read(cx).value().as_ref(), "retained settings draft");
            assert!(input.focus_handle(cx).is_focused(window));
            assert!(window.has_active_sheet(cx));
            assert!(!window.has_active_dialog(cx));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn quit_does_not_interrupt_an_unknown_modal(cx: &mut TestAppContext) {
        let (window, workspace) = workspace_window(cx);
        cx.update_window(window, |_, window, cx| {
            window.open_alert_dialog(cx, |alert, _, _| alert.title("Terminal risk"));
            workspace.update(cx, |workspace, cx| {
                workspace.quit_review(window, cx);
                assert!(!workspace.quit_requested);
            });
            assert!(window.has_active_dialog(cx));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn quit_does_not_interrupt_terminal_confirmation_over_a_palette(cx: &mut TestAppContext) {
        let (window, workspace) = workspace_window(cx);
        cx.update_window(window, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_palette(window, cx);
                workspace.terminal_confirmation = true;
                window.open_alert_dialog(cx, |alert, _, _| alert.title("Terminal risk"));
                workspace.quit_review(window, cx);
                assert!(!workspace.quit_requested);
                assert!(workspace.palette.as_ref().unwrap().read(cx).hosted());
            });
            assert!(window.has_active_dialog(cx));
        })
        .unwrap();
    }
}
