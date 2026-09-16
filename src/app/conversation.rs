use super::*;
use crate::titles;

impl AppState {
    pub fn ensure_conversation(&mut self, id: ThreadId) {
        if self.shutting_down
            || self.conversations.contains_key(&id)
            || self.storage_error.is_some()
        {
            return;
        }
        let Some(thread) = self.snapshot.threads.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        if thread.kind != ThreadKind::Conversation {
            return;
        }
        let Some(project) = self
            .snapshot
            .projects
            .iter()
            .find(|p| p.id == thread.project_id)
            .cloned()
        else {
            return;
        };
        if (thread.archived || project.archived) && thread.session_ref.is_none() {
            return;
        }
        let mut session = ConversationSession::new(thread.model.clone());
        let (finished, drain) = async_channel::bounded(1);
        session.finished = Some(drain);
        let generation = session.generation;
        self.conversations.insert(id, session);
        let saved = self.persist_thread(thread.clone());
        let sender = self.events.clone();
        let executables = self.snapshot.settings.get("executables").cloned();
        let session_dir = self
            .data_dir
            .join(format!("{}-sessions", thread.harness))
            .join(id.to_string());
        smol::spawn(async move {
            let result = async {
                saved
                    .recv()
                    .await
                    .context("Persistence owner closed")?
                    .map_err(anyhow::Error::msg)?;
                let config = smol::unblock(move || -> Result<_> {
                    let repository = platform::inspect_repository(&project.root)?;
                    let executable_name = harness::executable_name(&thread.harness)?;
                    let explicit = executables
                        .as_ref()
                        .and_then(|value| value.get(executable_name))
                        .and_then(Value::as_str);
                    Ok(harness::ConversationConfig {
                        allow_empty_session: thread.submission.is_none(),
                        harness: thread.harness,
                        executable: platform::resolve_executable(executable_name, explicit)?,
                        cwd: repository.root,
                        session_dir,
                        session_ref: thread.session_ref,
                        model: thread.model,
                        effort: thread.effort,
                    })
                })
                .await?;
                harness::open(config).await
            }
            .await;
            match result {
                Ok(handle) => {
                    let events = handle.events.clone();
                    let commands = handle.commands.clone();
                    if sender
                        .send(AppEvent::HarnessOpened(id, generation, Ok(handle)))
                        .await
                        .is_err()
                    {
                        let _ = commands.send(ConversationCommand::Close).await;
                        return;
                    }
                    while let Ok(event) = events.recv().await {
                        if sender
                            .send(AppEvent::Harness(id, generation, event))
                            .await
                            .is_err()
                        {
                            let _ = commands.send(ConversationCommand::Close).await;
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = sender
                        .send(AppEvent::HarnessOpened(
                            id,
                            generation,
                            Err(format!("{error:#}")),
                        ))
                        .await;
                }
            }
            let _ = sender.send(AppEvent::HarnessDrained(finished)).await;
        })
        .detach();
    }

    pub fn retry_conversation(&mut self, id: ThreadId) {
        if !self.is_conversation(id) {
            return;
        }
        if self.active_threads.contains(&id) {
            self.notice(
                NoticeLevel::Warning,
                "Stop the active turn before reconnecting.",
            );
            return;
        }
        if let Some(old) = self.conversations.remove(&id) {
            if let Some(commands) = old.commands {
                let _ = commands.try_send(ConversationCommand::Close);
            }
        }
        self.ensure_conversation(id);
    }

    pub fn submit_reason(&self, id: ThreadId) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is not writable: {error}"));
        }
        let Some(thread) = self.snapshot.threads.iter().find(|t| t.id == id) else {
            return Some("Select a Conversation first".into());
        };
        if thread.kind != ThreadKind::Conversation {
            return Some("Factory threads do not accept prompts".into());
        }
        if thread.archived
            || self
                .snapshot
                .projects
                .iter()
                .any(|p| p.id == thread.project_id && p.archived)
        {
            return Some("Archived history is read-only".into());
        }
        if !matches!(self.repositories.get(&thread.project_id), Some(Ok(_))) {
            return Some("Project root is unavailable".into());
        }
        if self.editor.is_none() || self.editor_error.is_some() {
            return Some("Connect or restart Neovim before sending".into());
        }
        let Some(session) = self.conversations.get(&id) else {
            return Some("Open the conversation harness first".into());
        };
        if session.preparing
            || self.active_threads.contains(&id)
            || session.state != ConversationState::Ready
        {
            return Some(format!(
                "Conversation is {:?}; only one foreground turn is allowed",
                session.state
            ));
        }
        if !session.persisted {
            return Some("Saving the provider session reference".into());
        }
        if thread.submission.is_some() {
            return Some("Review the previous submitted draft before starting another turn".into());
        }
        // An empty model list means the harness has not reported models yet; its Submit then
        // owns image validation.
        if !thread.attachments.is_empty()
            && !session.models.is_empty()
            && !session.models.iter().any(|m| {
                Some(&m.key) == session.model.as_ref()
                    && m.input.iter().any(|input| input == "image")
            })
        {
            return Some("The selected model does not advertise image input".into());
        }
        None
    }

    pub fn send_command(&mut self, id: ThreadId, command: ConversationCommand) {
        if !self.is_conversation(id) {
            return;
        }
        let Some(commands) = self
            .conversations
            .get(&id)
            .and_then(|s| s.commands.as_ref())
        else {
            self.notice(
                NoticeLevel::Warning,
                "Conversation harness is not connected",
            );
            return;
        };
        if let Err(error) = commands.try_send(command) {
            self.notice(
                NoticeLevel::Error,
                format!("Harness command could not be queued: {error}"),
            );
        }
    }

    pub fn interrupt(&mut self, id: ThreadId) {
        if self.is_conversation(id) && self.active_threads.contains(&id) {
            self.send_command(id, ConversationCommand::Interrupt);
        }
    }
    pub fn select_model(&mut self, id: ThreadId, model: ModelKey) {
        if !self.is_conversation(id) {
            return;
        }
        if self.active_threads.contains(&id) {
            self.notice(
                NoticeLevel::Warning,
                "Model changes are disabled during a turn",
            );
            return;
        }
        self.send_command(id, ConversationCommand::SelectModel(model));
    }
    /// Effort is a launch flag for the harness, so a change restarts its process; the
    /// conversation resumes from the provider session.
    pub fn select_effort(&mut self, id: ThreadId, effort: Option<String>) {
        if !self.is_conversation(id) {
            return;
        }
        if self.active_threads.contains(&id) {
            self.notice(
                NoticeLevel::Warning,
                "Effort changes are disabled during a turn",
            );
            return;
        }
        let Some(mut thread) = self.snapshot.threads.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        if thread.effort == effort {
            return;
        }
        thread.effort = effort;
        self.save_thread(thread);
        self.retry_conversation(id);
    }
    pub fn respond(&mut self, id: ThreadId, request_id: String, answer: UserAnswer) {
        self.send_command(id, ConversationCommand::Respond { request_id, answer });
    }

    pub fn record_draft(&mut self, draft: DraftSnapshot) {
        let id = match &draft.key {
            DraftKey::Thread(id) | DraftKey::Extension { thread: id, .. } => *id,
        };
        if !self.is_conversation(id) {
            return;
        }
        if self.drafts.get(&draft.key).is_some_and(|old| {
            old.generation == draft.generation && old.changedtick > draft.changedtick
        }) {
            return;
        }
        let key = draft.key.clone();
        self.drafts.insert(key.clone(), draft.clone());
        match key {
            DraftKey::Thread(id) => {
                if let Some(mut thread) = self.snapshot.threads.iter().find(|t| t.id == id).cloned()
                {
                    if thread.draft != draft.text {
                        thread.draft = draft.text;
                        thread.updated_at = platform::now();
                        self.edit_thread(thread);
                    }
                }
            }
            DraftKey::Extension { .. } => {}
        }
    }

    pub fn submit_snapshot(&mut self, id: ThreadId, draft: DraftSnapshot, cx: &mut Context<Self>) {
        let Some(mut thread) = self.snapshot.threads.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        if thread.kind != ThreadKind::Conversation {
            return;
        }
        if self.storage_error.is_some() || self.editor.is_none() || self.editor_error.is_some() {
            self.submission_failed(id, "Storage or editor is unavailable".into());
            return;
        }
        if draft.key != DraftKey::Thread(id) {
            self.submission_failed(id, "Editor snapshot belongs to another draft".into());
            return;
        }
        if draft.text.trim().is_empty() && thread.attachments.is_empty() {
            self.submission_failed(id, "Enter a prompt or attach an image".into());
            return;
        }
        let Some(session) = self.conversations.get_mut(&id) else {
            return;
        };
        let Some(commands) = session.commands.clone() else {
            self.submission_failed(id, "Harness disconnected before submission".into());
            return;
        };
        let request_id = Uuid::new_v4();
        let generation = session.generation;
        session.submitted.insert(request_id, draft.clone());
        thread.submission = Some(
            json!({"request_id":request_id,"text":draft.text,"attachments":thread.attachments,"state":"pending"}),
        );
        thread.updated_at = platform::now();
        let images = thread.attachments.clone();
        let wants_title = titles::accepts_generated_title(&thread) && !draft.text.trim().is_empty();
        let saved = self.persist_thread(thread);
        let sender = self.events.clone();
        cx.spawn(async move |this, cx| {
            let text = draft.text.clone();
            let result = async {
                let checked = images.clone(); let prompt = text.clone();
                let size = smol::unblock(move || harness::submission_size(request_id, &prompt, &checked)).await?;
                anyhow::ensure!(size <= crate::attachments::FRAME_LIMIT, "Prompt exceeds 1 MiB. Use Optimize images for sending, remove images, or reduce text.");
                saved.recv().await.context("Persistence owner closed")?.map_err(anyhow::Error::msg)?;
                commands.send(ConversationCommand::Submit { request_id, text, images }).await.context("Harness disconnected before submission")?;
                Ok::<_, anyhow::Error>(())
            }.await;
            if result.is_ok() && wants_title {
                let _ = this.update(cx, |this, cx| this.generate_title(id, draft.text.clone(), false, cx));
            }
            if let Err(error) = result {
                let _ = sender.send(AppEvent::Harness(id, generation, ConversationEvent::Failed { request_id: Some(request_id), message: format!("{error:#}") })).await;
            }
            let _ = this.update(cx, |this, cx| {
                if let Some(session) = this.conversations.get_mut(&id)
                    && session.generation == generation
                    && session.preparing
                {
                    session.preparing = false;
                    cx.emit(PresentationChange::Thread(id));
                }
            });
        }).detach();
    }
    pub fn regenerate_title_reason(&self) -> Option<String> {
        let Some(thread) = self.conversation_thread() else {
            return Some("Select a conversation first.".into());
        };
        if !titles::accepts_regenerated_title(thread) {
            return Some("Archived threads are read-only.".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is not writable: {error}"));
        }
        if self.titling.contains(&thread.id) {
            return Some("A title is being generated.".into());
        }
        let Some(session) = self.conversations.get(&thread.id) else {
            return Some("Open the conversation first.".into());
        };
        if !session
            .messages
            .iter()
            .any(|message| message.role == "user")
        {
            return Some("Send a prompt first.".into());
        }
        None
    }

    /// Renames the selected Conversation from all of its user prompts, replacing any title.
    pub fn regenerate_title(&mut self, cx: &mut Context<Self>) {
        if let Some(reason) = self.regenerate_title_reason() {
            self.notice(NoticeLevel::Warning, reason);
            return;
        }
        let Some(id) = self.conversation_thread().map(|thread| thread.id) else {
            return;
        };
        let prompt = self
            .conversations
            .get(&id)
            .map(|session| {
                titles::conversation_prompt(
                    session
                        .messages
                        .iter()
                        .filter(|message| message.role == "user")
                        .flat_map(|message| &message.content)
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.as_str()),
                            _ => None,
                        }),
                )
            })
            .unwrap_or_default();
        if prompt.is_empty() {
            self.notice(
                NoticeLevel::Warning,
                "This conversation has no text prompts to name it from.",
            );
            return;
        }
        self.generate_title(id, prompt, true, cx);
    }

    /// Names a Conversation from `prompt` with a local Ollama model. Automatic requests
    /// (`replace == false`) keep a title set meanwhile and stay silent without Ollama.
    fn generate_title(
        &mut self,
        id: ThreadId,
        prompt: String,
        replace: bool,
        cx: &mut Context<Self>,
    ) {
        if self.shutting_down || !self.titling.insert(id) {
            return;
        }
        let model = self
            .snapshot
            .settings
            .get(titles::MODEL_SETTING)
            .and_then(Value::as_str)
            .map(str::to_owned);
        let host = titles::server_host();
        cx.emit(PresentationChange::Thread(id));
        cx.spawn(async move |this, cx| {
            let request_host = host.clone();
            let result = smol::unblock(move || {
                titles::generate_title(&request_host, model.as_deref(), &prompt)
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                this.titling.remove(&id);
                match result {
                    Ok(Some(title)) => this.apply_generated_title(id, title, replace, cx),
                    Ok(None) if replace => this.notice(
                        NoticeLevel::Warning,
                        format!("No title generated: Ollama is not reachable at {host} or has no installed models."),
                    ),
                    Ok(None) => {}
                    Err(error) => this.notice(
                        NoticeLevel::Warning,
                        format!("Could not generate a conversation title: {error:#}. Check the title model in Settings."),
                    ),
                }
                cx.emit(PresentationChange::Thread(id));
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_generated_title(
        &mut self,
        id: ThreadId,
        title: String,
        replace: bool,
        cx: &mut Context<Self>,
    ) {
        let accepts = if replace {
            titles::accepts_regenerated_title
        } else {
            titles::accepts_generated_title
        };
        let Some(mut thread) = self
            .snapshot
            .threads
            .iter()
            .find(|t| t.id == id && accepts(t))
            .cloned()
        else {
            return;
        };
        if self.storage_error.is_some() || thread.title == title {
            return;
        }
        thread.title = title;
        self.save_thread(thread);
        cx.emit(PresentationChange::Thread(id));
    }

    pub fn submission_failed(&mut self, id: ThreadId, error: String) {
        if !self.is_conversation(id) {
            return;
        }
        if let Some(session) = self.conversations.get_mut(&id) {
            session.preparing = false;
            session.error = Some(error.clone());
        }
        self.active_threads.remove(&id);
        self.notice(NoticeLevel::Error, error);
    }
    pub fn acknowledge_recovery(&mut self, id: ThreadId) {
        if !self.is_conversation(id) || self.active_threads.contains(&id) {
            return;
        }
        if let Some(mut thread) = self.snapshot.threads.iter().find(|t| t.id == id).cloned() {
            thread.submission = None;
            self.save_thread(thread);
        }
    }

    pub(super) fn receive_conversation(
        &mut self,
        id: ThreadId,
        event: ConversationEvent,
        cx: &mut Context<Self>,
    ) {
        if !self.is_conversation(id) {
            return;
        }
        let Some(mut session) = self.conversations.remove(&id) else {
            return;
        };
        let refresh_diff = matches!(
            &event,
            ConversationEvent::Ready { .. }
                | ConversationEvent::StateChanged(_)
                | ConversationEvent::ToolUpdated(_)
                | ConversationEvent::TurnSettled { .. }
                | ConversationEvent::Failed { .. }
        );
        let presentation = match &event {
            ConversationEvent::HistoryReset
            | ConversationEvent::History { .. }
            | ConversationEvent::MessageUpdated(_)
            | ConversationEvent::ToolUpdated(_) => PresentationChange::Transcript(id),
            ConversationEvent::Ready { .. }
            | ConversationEvent::StateChanged(_)
            | ConversationEvent::UserRequest(_)
            | ConversationEvent::UserRequestDismissed { .. }
            | ConversationEvent::SubmissionAccepted { .. }
            | ConversationEvent::TurnSettled { .. }
            | ConversationEvent::ModelSelected(_)
            | ConversationEvent::TitleChanged(_)
            | ConversationEvent::EditorTextRequested(_)
            | ConversationEvent::StatusUpdated { .. }
            | ConversationEvent::WidgetUpdated { .. }
            | ConversationEvent::ContextUsageUpdated(_)
            | ConversationEvent::LimitsUpdated(_)
            | ConversationEvent::Notice { .. }
            | ConversationEvent::LinkOffered { .. }
            | ConversationEvent::Diagnostic { .. }
            | ConversationEvent::Failed { .. } => PresentationChange::Thread(id),
        };
        let mut thread = if matches!(
            &event,
            ConversationEvent::Ready { .. }
                | ConversationEvent::SubmissionAccepted { .. }
                | ConversationEvent::TurnSettled { .. }
                | ConversationEvent::ModelSelected(_)
                | ConversationEvent::TitleChanged(_)
                | ConversationEvent::Failed { .. }
        ) {
            self.snapshot
                .threads
                .iter()
                .find(|thread| thread.id == id)
                .cloned()
        } else {
            None
        };
        match event {
            ConversationEvent::Ready {
                session_ref,
                models,
            } => {
                session.models = models;
                session.persisted = false;
                if let Some(mut thread) = thread.take() {
                    thread.session_ref = Some(session_ref);
                    let saved = self.persist_thread(thread);
                    let generation = session.generation;
                    cx.spawn(async move |this, cx| {
                        let result = saved.recv().await;
                        let _ = this.update(cx, |this, cx| {
                            if let Some(session) = this
                                .conversations
                                .get_mut(&id)
                                .filter(|s| s.generation == generation)
                            {
                                match result {
                                    Ok(Ok(())) => session.persisted = true,
                                    Ok(Err(error)) => session.error = Some(error),
                                    Err(error) => session.error = Some(error.to_string()),
                                }
                                cx.emit(PresentationChange::Thread(id));
                            }
                        });
                    })
                    .detach();
                }
            }
            ConversationEvent::StateChanged(state) => {
                if matches!(
                    state,
                    ConversationState::Running
                        | ConversationState::WaitingForInput
                        | ConversationState::Interrupting
                ) {
                    self.active_threads.insert(id);
                } else if !session.preparing {
                    self.active_threads.remove(&id);
                }
                if matches!(
                    state,
                    ConversationState::Disconnected | ConversationState::Failed
                ) {
                    session.requests.clear();
                }
                session.state = state;
            }
            ConversationEvent::HistoryReset => session.reset_history(),
            ConversationEvent::History {
                messages,
                next_cursor,
            } => {
                for message in messages {
                    session.message(message);
                }
                if let (Some(cursor), Some(commands)) = (next_cursor, &session.commands) {
                    let _ = commands.try_send(ConversationCommand::LoadHistory {
                        cursor: Some(cursor),
                    });
                }
            }
            ConversationEvent::MessageUpdated(message) => session.message(message),
            ConversationEvent::ToolUpdated(tool) => {
                session.mark_tool_changed(&tool.id);
                session.tools.insert(tool.id.clone(), Arc::new(tool));
            }
            ConversationEvent::UserRequest(request) => {
                session.requests.insert(request.id.clone(), request);
            }
            ConversationEvent::UserRequestDismissed { request_id } => {
                session.requests.remove(&request_id);
            }
            ConversationEvent::SubmissionAccepted { request_id } => {
                if let Some(mut thread) = thread.take() {
                    let matches = thread
                        .submission
                        .as_ref()
                        .and_then(|v| v.get("request_id"))
                        .and_then(Value::as_str)
                        .is_some_and(|id| id == request_id.to_string());
                    if matches {
                        if let Some(submission) = &mut thread.submission {
                            submission["state"] = json!("accepted");
                        }
                        let sent_images: Vec<ImageAttachment> = thread
                            .submission
                            .as_ref()
                            .and_then(|v| v.get("attachments"))
                            .cloned()
                            .and_then(|v| serde_json::from_value(v).ok())
                            .unwrap_or_default();
                        let sent_ids: Vec<_> = sent_images.iter().map(|image| image.id).collect();
                        let saved = self.persist_thread(thread);
                        if let (Some(editor), Some(submitted)) = (
                            self.editor.clone(),
                            session.submitted.get(&request_id).cloned(),
                        ) {
                            cx.spawn(async move |this, cx| {
                                if matches!(saved.recv().await, Ok(Ok(()))) {
                                    let result = editor
                                        .replace_if_revision(
                                            submitted.key.clone(),
                                            submitted.revision(),
                                            String::new(),
                                        )
                                        .await;
                                    let _ = this.update(cx, |this, cx| {
                                        match result {
                                            Ok(true) => {
                                                if let Some(mut thread) = this
                                                    .snapshot
                                                    .threads
                                                    .iter()
                                                    .find(|thread| thread.id == id)
                                                    .cloned()
                                                    && thread
                                                        .attachments
                                                        .iter()
                                                        .map(|image| image.id)
                                                        .eq(sent_ids.iter().copied())
                                                {
                                                    thread.attachments.clear();
                                                    this.save_thread(thread);
                                                }
                                            }
                                            Ok(false) => {}
                                            Err(error) => this.notice(
                                                NoticeLevel::Warning,
                                                format!("Submitted text retained: {error:#}"),
                                            ),
                                        }
                                        cx.emit(PresentationChange::Thread(id));
                                    });
                                }
                            })
                            .detach();
                        }
                    }
                }
            }
            ConversationEvent::TurnSettled {
                request_id,
                outcome,
            } => {
                self.active_threads.remove(&id);
                session.preparing = false;
                session.outcome = Some(outcome.clone());
                session.requests.clear();
                session.submitted.remove(&request_id);
                if let Some(mut thread) = thread.take() {
                    if thread
                        .submission
                        .as_ref()
                        .and_then(|v| v.get("request_id"))
                        .and_then(Value::as_str)
                        .is_some_and(|id| id == request_id.to_string())
                    {
                        if matches!(outcome, TurnOutcome::Completed | TurnOutcome::Interrupted) {
                            thread.submission = None;
                        } else if let Some(submission) = &mut thread.submission {
                            submission["state"] = json!(format!("{outcome:?}").to_lowercase());
                        }
                        self.save_thread(thread);
                    }
                }
            }
            ConversationEvent::ModelSelected(model) => {
                session.model = Some(model.clone());
                if let Some(mut thread) = thread.take() {
                    thread.model = Some(model);
                    self.save_thread(thread);
                }
            }
            ConversationEvent::TitleChanged(title) => {
                if let Some(mut thread) = thread.take() {
                    thread.title = title;
                    self.save_thread(thread);
                }
            }
            ConversationEvent::EditorTextRequested(text) => {
                let replacement = self.editor.clone().zip(
                    self.drafts
                        .get(&DraftKey::Thread(id))
                        .filter(|draft| draft.text.is_empty())
                        .cloned(),
                );
                if let Some((editor, draft)) = replacement {
                    let generation = session.generation;
                    cx.spawn(async move |this, cx| {
                        let result = editor
                            .replace_if_revision(draft.key.clone(), draft.revision(), text.clone())
                            .await;
                        if !matches!(result, Ok(true)) {
                            let _ = this.update(cx, |state, cx| {
                                if let Some(session) = state
                                    .conversations
                                    .get_mut(&id)
                                    .filter(|session| session.generation == generation)
                                {
                                    session.widgets.insert(
                                        "Proposed editor replacement — review before replacing your draft"
                                            .into(),
                                        vec![text],
                                    );
                                }
                                if let Err(error) = result {
                                    state.notice(
                                        NoticeLevel::Warning,
                                        format!("Could not set editor text: {error:#}"),
                                    );
                                }
                                cx.emit(PresentationChange::Thread(id));
                            });
                        }
                    })
                    .detach();
                } else {
                    session.widgets.insert(
                        "Proposed editor replacement — review before replacing your draft".into(),
                        vec![text],
                    );
                }
            }
            ConversationEvent::StatusUpdated { key, text } => {
                if let Some(text) = text {
                    session.statuses.insert(key, text);
                } else {
                    session.statuses.remove(&key);
                }
            }
            ConversationEvent::WidgetUpdated { key, lines } => {
                if lines.is_empty() {
                    session.widgets.remove(&key);
                } else {
                    session.widgets.insert(key, lines);
                }
            }
            ConversationEvent::ContextUsageUpdated(usage) => {
                session.context_usage = Some(usage);
            }
            ConversationEvent::LimitsUpdated(limits) => {
                session.limits = limits;
            }
            ConversationEvent::Notice { level, text } => {
                let level = match level.as_str() {
                    "info" => NoticeLevel::Info,
                    "warning" => NoticeLevel::Warning,
                    "error" => NoticeLevel::Error,
                    _ => NoticeLevel::Diagnostic,
                };
                self.notice(level, text);
            }
            ConversationEvent::LinkOffered { url, instructions } => {
                if session.links.len() == 200 {
                    session.links.remove(0);
                }
                session.links.push((url, instructions));
            }
            ConversationEvent::Diagnostic { label, raw } => {
                if session.diagnostics.len() == 200 {
                    session.diagnostics.pop_front();
                }
                session.diagnostics.push_back((label, raw));
            }
            ConversationEvent::Failed {
                request_id,
                message,
            } => {
                session.error = Some(message.clone());
                session.preparing = false;
                if request_id.is_some() {
                    self.active_threads.remove(&id);
                    session.outcome = Some(TurnOutcome::Failed);
                }
                if let Some(mut thread) = thread.take() {
                    if let Some(submission) = &mut thread.submission {
                        if request_id.is_some_and(|id| {
                            submission
                                .get("request_id")
                                .and_then(Value::as_str)
                                .is_some_and(|value| value == id.to_string())
                        }) {
                            submission["state"] = json!("failed");
                            submission["error"] = json!(message);
                            self.save_thread(thread);
                        }
                    }
                }
            }
        }
        self.conversations.insert(id, session);
        if refresh_diff {
            self.queue_conversation_diff(id, cx);
        }
        cx.emit(presentation);
    }
}
