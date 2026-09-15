use super::workspace::{Region, Workspace};
use crate::{
    app::AppEvent,
    model::*,
    neovim::{DraftKey, NvimConfig, NvimEditor, NvimView, NvimViewEvent},
    platform,
};
use gpui_kit::*;
use serde_json::Value;

impl Workspace {
    pub fn start_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.opening_editor || self.editor.is_some() {
            return;
        }
        self.opening_editor = true;
        let explicit = self
            .state
            .read(cx)
            .snapshot
            .settings
            .get("executables")
            .and_then(|v| v.get("nvim"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        cx.spawn_in(window, async move |workspace, window| {
            let result = async {
                let executable = smol::unblock(move || platform::resolve_executable("nvim", explicit.as_deref())).await?;
                NvimEditor::open(NvimConfig::new(executable)).await
            }.await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.opening_editor = false;
                match result {
                    Ok(editor) => {
                        workspace.state.update(cx, |state, cx| { state.editor = Some(editor.clone()); state.editor_error = None; cx.notify(); });
                        let view = cx.new(|cx| NvimView::new(editor, window, cx));
                        let recovery_view = view.clone();
                        let recovery_state = workspace.state.clone();
                        App::on_app_quit(cx, move |cx| {
                            let text = recovery_view.read(cx).recovery_text();
                            let key = recovery_view.read(cx).active_key().cloned();
                            if !recovery_state.read(cx).shutdown_complete && !text.is_empty() { recovery_state.update(cx, |state, _| state.set_setting("recovered_composition", serde_json::json!({ "key": key, "text": text }))); }
                            async {}
                        }).detach();
                        workspace._subscriptions.push(cx.observe_in(&view, window, |_, _, _, cx| cx.notify()));
                        workspace._subscriptions.push(cx.subscribe_in(&view, window, |workspace, _, event: &NvimViewEvent, window, cx| {
                            match event {
                                NvimViewEvent::DraftChanged(draft) | NvimViewEvent::DraftActivated(draft) => workspace.state.update(cx, |state, cx| { state.record_draft(draft.clone()); cx.notify(); }),
                                NvimViewEvent::Ready => { workspace.state.update(cx, |state, cx| { state.editor_error = None; cx.notify(); }); workspace.editor_selection = None; workspace.sync_session(window, cx); }
                                NvimViewEvent::Failed(error) => workspace.state.update(cx, |state, cx| { state.editor_error = Some(error.clone()); state.notice(format!("Neovim: {error}")); cx.notify(); }),
                                NvimViewEvent::InputRejected(error) => workspace.state.update(cx, |state, cx| { state.notice(format!("Editor input: {error}")); cx.notify(); }),
                                NvimViewEvent::CompositionChanged(false) => workspace.sync_session(window, cx),
                                _ => {}
                            }
                            cx.notify();
                        }));
                        workspace.editor = Some(view);
                        workspace.sync_session(window, cx);
                    }
                    Err(error) => workspace.state.update(cx, |state, cx| { state.editor_error = Some(format!("{error:#}")); state.notice(format!("Neovim could not start: {error:#}. Configure its executable in Settings.")); cx.notify(); }),
                }
                cx.notify();
            });
        }).detach();
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
        let selected = self.state.read(cx).selected_thread;
        if let Some(id) = selected {
            self.state
                .update(cx, |state, _| state.ensure_conversation(id));
        }
        if self.editor_switching || self.state.read(cx).editor_error.is_some() {
            return;
        }
        let desired = self.editor_override.clone().or_else(|| {
            self.state
                .read(cx)
                .thread()
                .filter(|t| !t.archived)
                .map(|thread| (DraftKey::Thread(thread.id), thread.draft.clone()))
        });
        if desired.as_ref().map(|(key, _)| key) == self.editor_selection.as_ref() {
            self.apply_initial_factory_handoff(window, cx);
            return;
        }
        let Some(editor) = self.state.read(cx).editor.clone() else {
            return;
        };
        if self
            .editor
            .as_ref()
            .is_some_and(|view| view.read(cx).has_composition())
        {
            return;
        }
        let previous = self.editor_selection.clone();
        self.editor_switching = true;
        cx.spawn_in(window, async move |workspace, window| {
            let result = async {
                let snapshot = if let Some(key) = previous { Some(editor.snapshot(key).await?) } else { None };
                if let Some((key, text)) = &desired { editor.open_draft(key.clone(), text.clone()).await?; editor.focus_draft(key.clone()).await?; }
                Ok::<_, anyhow::Error>(snapshot)
            }.await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.editor_switching = false;
                match result {
                    Ok(snapshot) => {
                        if let Some(snapshot) = snapshot { workspace.state.update(cx, |state, cx| { state.record_draft(snapshot); cx.notify(); }); }
                        workspace.editor_selection = desired.map(|(key, _)| key);
                        workspace.sync_session(window, cx);
                    }
                    Err(error) => workspace.state.update(cx, |state, cx| { state.editor_error = Some(format!("{error:#}")); state.notice(format!("Draft switch cancelled: {error:#}. Cancel the editor command or restart Neovim.")); cx.notify(); }),
                }
                cx.notify();
            });
        }).detach();
    }

    pub fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_region(Region::Conversation, window, cx);
        self.sync_session(window, cx);
        if let Some(editor) = &self.editor {
            editor.update(cx, |view, cx| view.focus(window, cx));
        } else {
            self.start_editor(window, cx);
        }
    }
    pub fn restart_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.state.read(cx).editor.clone() else {
            self.start_editor(window, cx);
            return;
        };
        let events = self.state.read(cx).events.clone();
        smol::spawn(async move {
            if let Err(error) = editor.restart().await {
                let _ = events
                    .send(AppEvent::Notice(format!(
                        "Editor restart failed: {error:#}"
                    )))
                    .await;
            }
        })
        .detach();
    }
    pub fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.extension.is_some() {
            self.confirm_extension(window, cx);
            return;
        }
        if self.factory_form.is_some() {
            self.review_factory_task(window, cx);
            return;
        }
        let Some(id) = self.state.read(cx).selected_thread else {
            return;
        };
        if self.editor_switching
            || self.editor_selection != Some(DraftKey::Thread(id))
            || self.editor_override.is_some()
        {
            self.state.update(cx, |state, cx| {
                state.notice("Wait for the selected conversation draft to activate");
                cx.notify();
            });
            return;
        }
        if let Some(reason) = self.state.read(cx).submit_reason(id) {
            self.state.update(cx, |state, cx| {
                state.notice(reason);
                cx.notify();
            });
            return;
        }
        if self
            .editor
            .as_ref()
            .is_some_and(|view| view.read(cx).has_composition())
        {
            self.state.update(cx, |state, cx| {
                state.notice("Complete or cancel input composition before sending");
                cx.notify();
            });
            return;
        }
        let Some(editor) = self.state.read(cx).editor.clone() else {
            return;
        };
        self.state.update(cx, |state, cx| {
            if let Some(session) = state.conversations.get_mut(&id) {
                session.preparing = true;
                session.error = None;
            }
            state.active_threads.insert(id);
            cx.notify();
        });
        cx.spawn_in(window, async move |workspace, window| {
            let result = editor.snapshot(DraftKey::Thread(id)).await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.state.update(cx, |state, cx| match result {
                    Ok(snapshot) => state.submit_snapshot(id, snapshot, cx),
                    Err(error) => {
                        state.submission_failed(id, format!("Nothing was sent: {error:#}"));
                        cx.notify();
                    }
                });
                if let Some(view) = &workspace.editor {
                    view.update(cx, |view, cx| view.focus(window, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn restore_submitted(&mut self, id: ThreadId, window: &mut Window, cx: &mut Context<Self>) {
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
        let Some(editor) = self.state.read(cx).editor.clone() else {
            self.start_editor(window, cx);
            return;
        };
        let approval = window.prompt(PromptLevel::Warning, "Replace this conversation draft?", Some("This explicitly replaces its current draft. New typing after this review will not be overwritten."), &["Replace Draft", "Cancel"], cx);
        cx.spawn_in(window, async move |workspace, window| {
            if approval.await != Ok(0) {
                return;
            }
            let result = async {
                let snapshot = editor.snapshot(DraftKey::Thread(id)).await?;
                let replaced = editor
                    .replace_if_revision(snapshot.key.clone(), snapshot.revision(), text)
                    .await?;
                anyhow::ensure!(
                    replaced,
                    "Draft changed during replacement; the newer text was preserved"
                );
                Ok::<_, anyhow::Error>(())
            }
            .await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.state.update(cx, |state, cx| {
                    match result {
                        Ok(()) => {
                            if let Some(mut thread) =
                                state.snapshot.threads.iter().find(|t| t.id == id).cloned()
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
                        Err(error) => state.notice(format!("Draft was not replaced: {error:#}")),
                    }
                    cx.notify();
                });
                if let Some(view) = &workspace.editor {
                    view.update(cx, |view, cx| view.focus(window, cx));
                }
            });
        })
        .detach();
    }

    pub fn attach_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.state.read(cx).selected_thread else {
            return;
        };
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
                    state.notice(format!("Image selection failed: {other:?}"));
                    cx.notify();
                }),
            });
        })
        .detach();
    }
    pub fn import_images(
        &mut self,
        id: ThreadId,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).storage_error.is_some() {
            return;
        }
        if !self.state.read(cx).snapshot.threads.iter().any(|thread| {
            thread.id == id
                && !thread.archived
                && self
                    .state
                    .read(cx)
                    .snapshot
                    .projects
                    .iter()
                    .any(|project| project.id == thread.project_id && !project.archived)
        }) {
            return;
        }
        let data = self.state.read(cx).data_dir.clone();
        cx.spawn_in(window, async move |workspace, window| {
            let results = smol::unblock(move || {
                paths
                    .iter()
                    .map(|path| crate::attachments::import(path, &data))
                    .collect::<Vec<_>>()
            })
            .await;
            let _ = workspace.update_in(window, |workspace, _, cx| {
                workspace.state.update(cx, |state, cx| {
                    if let Some(mut thread) = state
                        .snapshot
                        .threads
                        .iter()
                        .find(|t| {
                            t.id == id
                                && !t.archived
                                && state
                                    .snapshot
                                    .projects
                                    .iter()
                                    .any(|p| p.id == t.project_id && !p.archived)
                        })
                        .cloned()
                    {
                        for result in results {
                            match result {
                                Ok(image) => thread.attachments.push(image),
                                Err(error) => {
                                    state.notice(format!("Attachment rejected: {error:#}"))
                                }
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
        let Some(thread) = self.state.read(cx).thread().cloned() else {
            return;
        };
        let Some(editor) = self.state.read(cx).editor.clone() else {
            return;
        };
        let data = self.state.read(cx).data_dir.clone();
        let original_ids: Vec<_> = thread.attachments.iter().map(|image| image.id).collect();
        cx.spawn_in(window, async move |workspace, window| {
            let result = async {
                let snapshot = editor.snapshot(DraftKey::Thread(thread.id)).await?;
                smol::unblock(move || crate::attachments::optimize(uuid::Uuid::new_v4(), &snapshot.text, &thread.attachments, &data)).await
            }.await;
            let _ = workspace.update_in(window, |workspace, _, cx| workspace.state.update(cx, |state, cx| {
                match result {
                    Ok(prepared) => if let Some(mut current) = state.snapshot.threads.iter().find(|t| t.id == thread.id).cloned() {
                        if current.attachments.iter().map(|image| image.id).collect::<Vec<_>>() == original_ids { current.attachments = prepared; state.save_thread(current); state.notice("Prepared JPEG copies are shown below. Review their actual dimensions and previews before Send; originals are retained."); }
                        else { state.notice("Attachments changed during optimization; current attachments were preserved"); }
                    },
                    Err(error) => state.notice(format!("Images were not prepared: {error:#}")),
                }
                cx.notify();
            }));
        }).detach();
    }
}

impl Workspace {
    pub fn cancel_editor_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = &self.editor {
            view.update(cx, |view, cx| view.cancel_composition(window, cx));
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
                        Err(error) => state.notice(format!("Editor requires restart: {error:#}")),
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
        let factory_drafts: std::collections::BTreeMap<_, _> = state
            .snapshot
            .settings
            .iter()
            .filter(|(key, _)| key.starts_with("factory_draft:"))
            .collect();
        let text = serde_json::json!({
            "threads": state.snapshot.threads,
            "factory_drafts": factory_drafts,
            "editor_buffers": state.drafts.values().map(|draft| serde_json::json!({ "key": format!("{:?}", draft.key), "text": draft.text })).collect::<Vec<_>>(),
            "saved_uncommitted_preedit": state.snapshot.settings.get("recovered_composition"),
            "current_uncommitted_preedit": self.editor.as_ref().map(|view| view.read(cx).recovery_text()),
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
        if let Some(view) = &self.editor {
            let text = view.read(cx).recovery_text();
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
        self.focus_region(Region::Conversation, window, cx);
        self.state
            .update(cx, |state, cx| state.begin_shutdown(allow_data_loss, cx));
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
            "Quit Drost?",
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
