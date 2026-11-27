use super::*;

pub enum AppEvent {
    WriteFinished(Result<(), String>),
    RepositoryChecked(ProjectId, Result<platform::RepositoryInfo, String>),
    ProjectResolved(SpaceId, Result<platform::RepositoryInfo, String>),
    Diagnostics(BTreeMap<String, String>),
    TitleModels(Uuid, crate::titles::InstalledModels),
    PiChecked(Uuid, Option<PathBuf>),
    PiInstalled(Result<PathBuf, String>),
    StorageFailed(String),
    InteractiveActivity { thread_id: ThreadId, active: bool },
    FactorySnapshot(Box<crate::factory::FactorySnapshot>),
    HarnessOpened(ThreadId, Uuid, Result<ConversationHandle, String>),
    Harness(ThreadId, Uuid, ConversationEvent),
    Notice(WorkspaceNotice),
    HarnessDrained(async_channel::Sender<()>),
    WorktreeCreated(Uuid, Result<Box<Worktree>, String>),
    WorktreeRenamed(WorktreeId, Result<String, String>),
    WorktreeInspected(WorktreeId, Result<platform::WorktreeStatus, String>),
    WorktreeRemoved(Option<Uuid>, WorktreeId, Result<bool, RemovalFailure>),
    WorktreeHeadRead(WorktreeId, Result<Option<String>, String>),
    BranchSwitched(Uuid, WorkingTree, Result<String, String>),
    Committed(Uuid, WorkingTree, Result<(String, String), String>),
    Branches(branches::BranchListing),
}

impl AppState {
    pub(super) fn receive(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        match event {
            AppEvent::WriteFinished(result) => {
                self.pending_writes = self.pending_writes.saturating_sub(1);
                if let Err(error) = result {
                    self.storage_error = Some(error.clone());
                    self.pause_all_network_delivery();
                    self.sync_network();
                    self.notice(NoticeLevel::Error, error);
                    cx.notify();
                } else {
                    cx.emit(PresentationChange::Persistence);
                }
                return;
            }
            AppEvent::StorageFailed(error) => {
                self.storage_error = Some(error.clone());
                self.pause_all_network_delivery();
                self.sync_network();
                self.notice(NoticeLevel::Error, error);
            }
            AppEvent::RepositoryChecked(id, info) => {
                self.repositories.insert(id, info);
            }
            AppEvent::ProjectResolved(space, result) => {
                self.registering = false;
                match result {
                    Ok(info) => {
                        if let Some((id, archived)) = self
                            .snapshot
                            .projects
                            .iter()
                            .find(|p| p.root == info.root)
                            .map(|p| (p.id, p.archived))
                        {
                            self.repositories.insert(id, Ok(info));
                            if self.space_mutation_reason().is_none() {
                                let (destination, added) =
                                    self.register_project_membership(id, space);
                                let name =
                                    self.space_name(destination).unwrap_or("Default").to_owned();
                                if destination == self.active_space {
                                    self.select_project(id);
                                    if archived {
                                        self.set_show_archived(true);
                                    }
                                    self.notice(
                                        NoticeLevel::Info,
                                        if added {
                                            format!("Project added to space {name} and selected.")
                                        } else {
                                            "This working tree is already in this space; selected the existing project.".into()
                                        },
                                    );
                                } else {
                                    self.notice(
                                        NoticeLevel::Info,
                                        if added {
                                            format!("Project added to space {name}. Switch to that space to open it.")
                                        } else {
                                            format!("This working tree is already in space {name}. Switch to that space to open it.")
                                        },
                                    );
                                }
                            }
                        } else if self.space_mutation_reason().is_none() {
                            let project = Project {
                                id: Uuid::new_v4(),
                                name: info
                                    .root
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "Project".into()),
                                root: info.root.clone(),
                                archived: false,
                                defaults: ProjectDefaults::default(),
                            };
                            let id = project.id;
                            self.repositories.insert(project.id, Ok(info));
                            let destination = self.save_project_in_space(project, space);
                            if destination == self.active_space {
                                self.select_project(id);
                            } else {
                                let name =
                                    self.space_name(destination).unwrap_or("Default").to_owned();
                                self.notice(NoticeLevel::Info, format!("Project registered in space {name}. Switch to that space to open it."));
                            }
                        }
                    }
                    Err(error) => self.notice(NoticeLevel::Error, error),
                }
            }
            AppEvent::Diagnostics(diagnostics) => self.diagnostics = diagnostics,
            AppEvent::TitleModels(request, models) => {
                if self.title_models_request == Some(request) {
                    self.title_models_request = None;
                    self.title_models = Some(models);
                }
            }
            AppEvent::PiChecked(request, found) => self.pi_checked(request, found),
            AppEvent::PiInstalled(result) => {
                self.installing_pi = false;
                match result {
                    Ok(path) => {
                        let mut executables = self
                            .snapshot
                            .settings
                            .get("executables")
                            .and_then(Value::as_object)
                            .cloned()
                            .unwrap_or_default();
                        executables.insert(
                            "pi".into(),
                            Value::String(path.to_string_lossy().into_owned()),
                        );
                        self.set_setting("executables", Value::Object(executables));
                        self.notice(
                            NoticeLevel::Info,
                            format!("Pi installed at {}", path.display()),
                        );
                    }
                    Err(error) => self.notice(
                        NoticeLevel::Error,
                        format!("Pi installation failed: {error}"),
                    ),
                }
                self.refresh_diagnostics();
            }
            AppEvent::InteractiveActivity { thread_id, active } => {
                if active && self.is_conversation(thread_id) {
                    self.active_threads.insert(thread_id);
                } else {
                    self.active_threads.remove(&thread_id);
                    if self.settle_closed_pull_requests() {
                        cx.emit(PresentationChange::Projects);
                        cx.notify();
                    }
                }
                cx.emit(PresentationChange::Thread(thread_id));
                return;
            }
            AppEvent::FactorySnapshot(snapshot) => self.receive_factory(*snapshot),
            AppEvent::Notice(notice) => self.notice(notice.level, notice.text),
            AppEvent::HarnessOpened(id, generation, result) => {
                if self.shutting_down || !self.is_conversation(id) {
                    if let Ok(handle) = &result {
                        let _ = handle.commands.try_send(ConversationCommand::Close);
                    }
                    return;
                }
                match (
                    self.conversations
                        .get_mut(&id)
                        .filter(|s| s.generation == generation),
                    result,
                ) {
                    (Some(session), Ok(handle)) => {
                        session.commands = Some(handle.commands);
                    }
                    (Some(session), Err(error)) => {
                        session.state = ConversationState::Failed;
                        session.error = Some(error);
                    }
                    (None, Ok(handle)) => {
                        let _ = handle.commands.try_send(ConversationCommand::Close);
                    }
                    (None, Err(_)) => {}
                }
                self.sync_network();
                cx.emit(PresentationChange::Projects);
                cx.emit(PresentationChange::Thread(id));
                return;
            }
            AppEvent::Harness(id, generation, event) => {
                if self
                    .conversations
                    .get(&id)
                    .is_some_and(|s| s.generation == generation)
                {
                    let project_rows_changed = matches!(
                        &event,
                        ConversationEvent::Ready { .. }
                            | ConversationEvent::StateChanged(_)
                            | ConversationEvent::TitleChanged(_)
                            | ConversationEvent::UserRequest(_)
                            | ConversationEvent::UserRequestDismissed { .. }
                            | ConversationEvent::SubmissionAccepted { .. }
                            | ConversationEvent::TurnSettled { .. }
                            | ConversationEvent::Failed { .. }
                            | ConversationEvent::PeerDeliveryAvailable { .. }
                            | ConversationEvent::PeerDelivery { .. }
                            | ConversationEvent::PeerTurnStarted { .. }
                    );
                    let was_running = self.active_threads.contains(&id);
                    self.receive_conversation(id, event, cx);
                    if was_running && !self.active_threads.contains(&id) {
                        self.turn_ended(id);
                    }
                    if project_rows_changed {
                        cx.emit(PresentationChange::Projects);
                    }
                }
                return;
            }
            AppEvent::HarnessDrained(reply) => {
                let _ = reply.try_send(());
                return;
            }
            AppEvent::WorktreeCreated(request, result) => {
                self.worktree_created(request, result, cx)
            }
            AppEvent::WorktreeRenamed(id, result) => self.worktree_renamed(id, result, cx),
            AppEvent::WorktreeInspected(id, result) => self.worktree_inspected(id, result, cx),
            AppEvent::WorktreeRemoved(request, id, result) => {
                self.worktree_removed(request, id, result, cx)
            }
            AppEvent::WorktreeHeadRead(id, result) => {
                if let Ok(head) = result {
                    self.record_head(WorkingTree::Worktree(id), head);
                }
                cx.emit(PresentationChange::Projects);
                for thread in self.worktree_threads(id) {
                    cx.emit(PresentationChange::Thread(thread));
                }
            }
            AppEvent::BranchSwitched(request, tree, result) => {
                self.branch_switched(request, tree, result, cx)
            }
            AppEvent::Committed(request, tree, result) => self.committed(request, tree, result, cx),
            AppEvent::Branches(listing) => self.branches_received(listing, cx),
        }
        cx.notify();
    }
}
