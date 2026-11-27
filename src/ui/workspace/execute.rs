use super::*;

impl Workspace {
    pub(super) fn open_harness_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.palette = Some(Palette::open_harnesses(
            cx.entity().downgrade(),
            self.state.clone(),
            id,
            window,
            cx,
        ));
        cx.notify();
    }

    pub(super) fn open_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((thread, project)) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| (thread.id, thread.project_id))
        else {
            return;
        };
        self.dismiss(window, cx);
        if self.navigation_blocked(window, cx) {
            return;
        }
        self.state.update(cx, |state, cx| {
            state.refresh_branches(project, true);
            cx.notify();
        });
        self.palette = Some(Palette::open_branches(
            cx.entity().downgrade(),
            self.state.clone(),
            thread,
            window,
            cx,
        ));
        cx.notify();
    }

    pub(in crate::ui) fn open_conversation_in(
        &mut self,
        worktree: Option<uuid::Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let created = self.state.update(cx, |state, cx| {
            let result = state.new_conversation_in_workspace(worktree);
            if let Err(error) = &result {
                state.notice(crate::app::NoticeLevel::Error, format!("{error:#}"));
            }
            cx.notify();
            result.is_ok()
        });
        if created {
            self.focus_region(Region::Thread, window, cx);
        }
    }

    pub(in crate::ui) fn confirm_remove_worktree(
        &mut self,
        worktree: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_dialog(DialogKind::RemoveWorktree(worktree), window, cx);
    }

    pub(super) fn open_host_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self
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
        self.palette = Some(Palette::open_hosts(
            cx.entity().downgrade(),
            self.state.clone(),
            thread,
            window,
            cx,
        ));
        cx.notify();
    }

    pub fn execute(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).selected_thread.is_some() {
            self.active_terminal = None;
        }
        if self.state.read(cx).shutting_down {
            return;
        }
        if self.preparing_space_change
            && !matches!(
                command,
                Command::CancelEditorCommand | Command::CopyDraftRecovery
            )
        {
            self.space_error(
                anyhow::anyhow!("Wait for the editor draft to finish saving before navigating"),
                cx,
            );
            return;
        }
        if matches!(
            command,
            Command::NextThread
                | Command::PreviousThread
                | Command::CloseThread
                | Command::NewConversation
                | Command::NewOmpConversation
                | Command::NewPiConversation
                | Command::NewClaudeCodeConversation
                | Command::NewCodexConversation
                | Command::NewOpenCodeConversation
                | Command::NewConversationInWorktree
                | Command::NewConversationInWorkspace
                | Command::NewFactoryThread
                | Command::NewTerminal
                | Command::NewNeovim
                | Command::OpenConversationFile
                | Command::NewBrowser
                | Command::RestartTerminal
        ) && self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        if let Some(reason) = command.disabled_reason_in_workspace(self, cx) {
            self.state.update(cx, |state, cx| {
                state.notice(crate::app::NoticeLevel::Warning, reason);
                cx.notify();
            });
            return;
        }
        let kind = match command {
            Command::WidenSidebar | Command::NarrowSidebar => {
                if let Some(id) = self.state.read(cx).selected_thread {
                    let sizes = self.sidebar_sizes.borrow().get(&id).cloned();
                    if let Some(sizes) = sizes {
                        let width = sizes.read(cx).sizes().get(1).copied();
                        if let Some(width) = width {
                            let delta = if command == Command::WidenSidebar {
                                48.
                            } else {
                                -48.
                            };
                            sizes.update(cx, |sizes, cx| {
                                sizes.resize_panel(1, width + px(delta), window, cx)
                            });
                            if let Some(width) = sizes.read(cx).sizes().get(1).copied() {
                                self.state.update(cx, |state, _| {
                                    state.set_thread_sidebar_width(id, f32::from(width))
                                });
                            }
                        }
                    }
                }
                None
            }
            Command::NewBrowser => {
                self.open_browser("about:blank".into(), None, None, window, cx);
                None
            }
            Command::ToggleBrowser => {
                if let Some(id) = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .map(|thread| thread.id)
                {
                    if self.sidebar_browser(id).is_none() {
                        self.ensure_sidebar_browser(id, window, cx);
                    } else {
                        self.toggle_sidebar_tab(id, SidebarTab::Browser, window, cx);
                    }
                }
                None
            }
            Command::BrowserBack
            | Command::BrowserForward
            | Command::BrowserReload
            | Command::BrowserAddress
            | Command::ToggleArtifact => {
                if let Some(pane) = self.selected_browser(cx) {
                    pane.update(cx, |pane, cx| pane.command(command, window, cx));
                }
                None
            }
            Command::FocusComposerArtifact => {
                if let Some(pane) = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .and_then(|thread| self.docked_artifact(thread.id))
                {
                    pane.update(cx, |pane, cx| pane.focus(window, cx));
                }
                None
            }
            Command::MoveArtifactIntoConversation => {
                if let Some(thread) = self.state.read(cx).conversation_thread().map(|t| t.id) {
                    self.move_docked_artifact_into_conversation(thread, cx);
                }
                None
            }
            Command::NewTerminal => {
                self.new_terminal(window, cx);
                None
            }
            Command::ToggleTerminal => {
                if let Some(id) = self.state.read(cx).thread().map(|thread| thread.id) {
                    self.toggle_terminal_sidebar(id, window, cx);
                }
                None
            }
            Command::RestartTerminal => {
                self.restart_terminal(window, cx);
                None
            }
            Command::NewNeovim => {
                self.new_neovim(window, cx);
                None
            }
            Command::OpenConversationFile => {
                self.open_conversation_files(window, cx);
                None
            }
            Command::NewSpace => Some(DialogKind::NewSpace),
            Command::RenameSpace => {
                let id = self.state.read(cx).active_space;
                self.start_rename(RenameTarget::Space(id), RenameSurface::Header, window, cx);
                None
            }
            Command::DeleteSpace => Some(DialogKind::DeleteSpace(self.state.read(cx).active_space)),
            Command::SwitchSpace | Command::MoveProjectToSpace => {
                self.open_space_picker(command == Command::MoveProjectToSpace, window, cx);
                None
            }
            Command::NextSpace | Command::PreviousSpace => {
                let state = self.state.read(cx);
                let count = state.spaces.len();
                let current = state
                    .spaces
                    .iter()
                    .position(|space| space.id == state.active_space)
                    .unwrap_or(0);
                let index = if command == Command::NextSpace {
                    (current + 1) % count
                } else {
                    (current + count - 1) % count
                };
                let target = state.spaces[index].id;
                self.space_direction = Some(if command == Command::NextSpace {
                    -1.
                } else {
                    1.
                });
                if let Err(error) = self.switch_space(target, window, cx) {
                    self.space_error(error, cx);
                }
                None
            }
            Command::InstallPi => {
                self.state.update(cx, |state, cx| {
                    state.install_pi();
                    cx.notify();
                });
                None
            }
            Command::RestartEditor => {
                self.restart_editor(window, cx);
                None
            }
            Command::CancelEditorCommand => {
                self.cancel_editor_command(window, cx);
                None
            }
            Command::CopyDraftRecovery => {
                self.copy_draft_recovery(cx);
                None
            }
            Command::PasteImage => {
                self.paste_image(window, cx);
                None
            }
            Command::InsertSlashCommand => {
                self.open_slash_command_picker(window, cx);
                None
            }
            Command::ClearQueuedMessages => {
                self.clear_queued_messages(window, cx);
                None
            }
            Command::SendQueuedNow => {
                if let Some((id, message)) = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .and_then(|thread| thread.queued.first().map(|message| (thread.id, message.id)))
                {
                    self.send_queued_now(id, message, cx);
                }
                None
            }
            Command::EditQueuedMessage => {
                if let Some((id, message)) = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .and_then(|thread| thread.queued.last().map(|message| (thread.id, message.id)))
                {
                    self.edit_queued(id, message, window, cx);
                }
                None
            }
            Command::ToggleConversationDiffs => {
                if let Some(id) = self.state.read(cx).conversation_thread().map(|t| t.id) {
                    let tab = if self.state.read(cx).thread_sidebar_tab(id) == SidebarTab::Details {
                        SidebarTab::Details
                    } else {
                        SidebarTab::Changes
                    };
                    self.toggle_sidebar_tab(id, tab, window, cx);
                }
                None
            }
            Command::ToggleAgents => {
                if let Some(id) = self.state.read(cx).conversation_thread().map(|t| t.id) {
                    self.toggle_sidebar_tab(id, SidebarTab::Agents, window, cx);
                }
                None
            }
            Command::ShowNetworkMembers
            | Command::ShowNetworkMessages
            | Command::ShowThreadAgents => {
                let id = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .map(|thread| thread.id);
                if let Some(id) = id {
                    self.show_sidebar_tab(id, SidebarTab::Agents, window, cx);
                    if let Some(network) = self.network_sidebar(id, cx) {
                        let view = match command {
                            Command::ShowNetworkMembers => crate::ui::network::View::Members,
                            Command::ShowNetworkMessages => crate::ui::network::View::Messages,
                            _ => crate::ui::network::View::Thread,
                        };
                        network.update(cx, |network, cx| network.show_view(view, window, cx));
                    }
                }
                None
            }
            Command::JoinSpaceNetwork
            | Command::LeaveSpaceNetwork
            | Command::ResumeNetworkDelivery => {
                let thread = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .map(|thread| thread.id);
                if let Some(id) = thread {
                    self.state.update(cx, |state, cx| {
                        if command == Command::ResumeNetworkDelivery {
                            state.resume_network_delivery(id, cx);
                        } else {
                            state.set_network_joined(id, command == Command::JoinSpaceNetwork, cx);
                        }
                        state.show_thread_sidebar(id, SidebarTab::Agents);
                    });
                }
                None
            }
            Command::ToggleTaskList => {
                self.toggle_dock("dock-todo", cx);
                None
            }
            Command::ToggleBackgroundJobs => {
                self.toggle_dock("dock-jobs", cx);
                None
            }
            Command::GrowComposer | Command::ShrinkComposer => {
                let delta = if command == Command::GrowComposer {
                    48.
                } else {
                    -48.
                };
                self.resize_composer(delta, window, cx);
                None
            }
            Command::RegenerateTitle => {
                self.state
                    .update(cx, |state, cx| state.regenerate_title(cx));
                None
            }
            Command::ToggleChangesLayout => {
                self.state.update(cx, |state, cx| {
                    state.toggle_changes_layout();
                    cx.notify();
                });
                None
            }
            Command::OpenChangeInNeovim => {
                if self.open_change_reason(cx).is_none() {
                    self.open_selected_change(window, cx);
                }
                None
            }
            Command::RefreshConversationDiff => {
                self.state.update(cx, |state, cx| {
                    if let Some(id) = state.conversation_thread().map(|thread| thread.id) {
                        state.refresh_conversation_diff(id, cx);
                    }
                });
                None
            }
            Command::ToggleReview => {
                if let Some(id) = self.state.read(cx).conversation_thread().map(|t| t.id) {
                    self.toggle_sidebar_tab(id, SidebarTab::Review, window, cx);
                }
                None
            }
            Command::RefreshReview => {
                self.state.update(cx, |state, cx| {
                    if let Some(id) = state.conversation_thread().map(|thread| thread.id) {
                        state.refresh_reviews(id, cx);
                    }
                });
                None
            }
            Command::RefreshPullRequests => {
                self.state
                    .update(cx, |state, cx| state.poll_pull_requests(cx));
                None
            }
            Command::OpenPullRequest => {
                if let Some(url) = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .and_then(|thread| thread.pull_request.as_ref())
                    .map(|pr| pr.url.clone())
                {
                    cx.open_url(&url);
                }
                None
            }
            Command::AddProject => {
                self.add_project(window, cx);
                None
            }
            Command::RenameProject => {
                if let Some(id) = self.state.read(cx).selected_project {
                    self.start_rename(RenameTarget::Project(id), RenameSurface::Header, window, cx);
                }
                None
            }
            Command::ArchiveProject => {
                self.archive_project(window, cx);
                None
            }
            Command::RenameThread => {
                if let Some(id) = self.state.read(cx).selected_thread {
                    self.start_rename(RenameTarget::Thread(id), RenameSurface::Tab, window, cx);
                }
                None
            }
            Command::NewConversationInWorktree => self
                .state
                .read(cx)
                .selected_project
                .map(DialogKind::NewWorktreeConversation),
            Command::NewConversationInWorkspace => self
                .state
                .read(cx)
                .selected_project
                .map(DialogKind::NewWorkspaceConversation),
            Command::SwitchHarness => {
                self.open_harness_picker(window, cx);
                None
            }
            Command::SwitchExecutionHost => {
                self.open_host_picker(window, cx);
                None
            }
            Command::RetryConversation => {
                if let Some(thread) = self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .map(|thread| thread.id)
                {
                    self.state.update(cx, |state, cx| {
                        state.retry_conversation(thread);
                        cx.notify();
                    });
                }
                None
            }
            Command::RetryRemoteFile => {
                let file = self
                    .state
                    .read(cx)
                    .remote_file
                    .as_ref()
                    .map(|file| (file.thread, file.path.clone()));
                if let Some((thread, path)) = file {
                    self.state.update(cx, |state, cx| {
                        state.request_remote_file(thread, path, cx);
                        cx.notify();
                    });
                }
                None
            }
            Command::RemoveRemoteHost => {
                self.confirm_remove_remote_host(window, cx);
                None
            }
            Command::PairPhone => {
                self.open_pair_phone(window, cx);
                None
            }
            Command::RemovePhone => {
                self.confirm_remove_phone(window, cx);
                None
            }
            Command::RemoveWorktree => self
                .state
                .read(cx)
                .conversation_thread()
                .and_then(|thread| thread.worktree_id)
                .map(DialogKind::RemoveWorktree),
            Command::SwitchBranch => {
                self.open_branch_picker(window, cx);
                None
            }
            Command::CommitChanges => {
                self.open_commit_panel(window, cx);
                None
            }
            Command::ArchiveThread => {
                if self.extension.is_some()
                    || self
                        .editor
                        .as_ref()
                        .is_some_and(|editor| editor.read(cx).has_composition())
                {
                    return;
                }
                self.dismiss(window, cx);
                if self.detail_surface.is_some()
                    || self.has_open_form(cx)
                    || self.palette.is_some()
                    || self.theme_editor.is_some()
                {
                    return;
                }
                if let Some(id) = self.state.read(cx).thread().map(|thread| thread.id) {
                    self.archive_thread(id, window, cx);
                }
                None
            }
            Command::UnarchiveThread => {
                if let Some(id) = self.state.read(cx).thread().map(|thread| thread.id) {
                    self.unarchive_thread(id, cx);
                }
                None
            }
            Command::Settings => Some(DialogKind::Settings(self.state.read(cx).selected_project)),
            Command::CustomizeTheme => {
                self.open_theme_editor(window, cx);
                None
            }
            Command::NewConversation
                if self
                    .state
                    .read(cx)
                    .conversation_thread()
                    .is_some_and(|thread| {
                        let state = self.state.read(cx);
                        state.is_untouched(thread) && state.switch_harness_reason().is_none()
                    }) =>
            {
                self.open_harness_picker(window, cx);
                None
            }
            Command::NewConversation
            | Command::NewOmpConversation
            | Command::NewPiConversation
            | Command::NewClaudeCodeConversation
            | Command::NewCodexConversation
            | Command::NewOpenCodeConversation
            | Command::NewFactoryThread => {
                let created = self.state.update(cx, |state, cx| {
                    let result = if command == Command::NewConversation {
                        state.new_thread(ThreadKind::Conversation)
                    } else if command == Command::NewOmpConversation {
                        state.new_conversation("omp")
                    } else if command == Command::NewPiConversation {
                        state.new_conversation("pi")
                    } else if command == Command::NewClaudeCodeConversation {
                        state.new_conversation("claude-code")
                    } else if command == Command::NewCodexConversation {
                        state.new_conversation("codex")
                    } else if command == Command::NewOpenCodeConversation {
                        state.new_conversation("opencode")
                    } else {
                        state.new_thread(ThreadKind::Factory)
                    };
                    let created = result.is_ok();
                    if let Err(error) = result {
                        state.notice(crate::app::NoticeLevel::Error, format!("{error:#}"));
                    }
                    cx.notify();
                    created
                });
                if created {
                    self.focus_region(Region::Thread, window, cx);
                }
                None
            }
            Command::ShowArchived => {
                self.state.update(cx, |state, cx| {
                    state.set_show_archived(!state.show_archived);
                    cx.notify();
                });
                None
            }
            Command::SearchThreads => {
                self.focus_region(Region::Projects, window, cx);
                if self.focused_region(window, cx) == Region::Projects {
                    self.thread_search
                        .update(cx, |input, cx| input.focus(window, cx));
                }
                None
            }
            Command::ResetLayout => {
                self.reset_layout(window, cx);
                None
            }
            Command::RefreshFactoryRuns => {
                self.state.update(cx, |state, cx| {
                    state.refresh_factory_runs();
                    cx.notify();
                });
                None
            }
            Command::CloseThread => {
                self.close_active_panel(window, cx);
                None
            }
            Command::NextThread | Command::PreviousThread => {
                self.cycle_panel(command == Command::NextThread, window, cx);
                None
            }
            Command::FocusProjects | Command::FocusThread => {
                self.focus_region(
                    match command {
                        Command::FocusProjects => Region::Projects,
                        _ => Region::Thread,
                    },
                    window,
                    cx,
                );
                None
            }
            Command::FocusOutput => {
                if let Some(id) = self.state.read(cx).thread().map(|thread| thread.id) {
                    self.show_sidebar_tab(id, SidebarTab::Output, window, cx);
                }
                None
            }
            Command::ToggleProjects => {
                self.toggle_region(Region::Projects, window, cx);
                None
            }
            Command::ToggleOutput => {
                if let Some(id) = self.state.read(cx).thread().map(|thread| thread.id) {
                    self.toggle_sidebar_tab(id, SidebarTab::Output, window, cx);
                }
                None
            }
        };
        if let Some(kind) = kind {
            self.open_dialog(kind, window, cx);
        }
        cx.notify();
    }
    pub(super) fn add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if Command::AddProject
            .disabled_reason(self.state.read(cx))
            .is_some()
        {
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add Project".into()),
        });
        cx.spawn_in(window, async move |workspace, window| {
            let result = picked.await;
            let _ = workspace.update_in(window, |workspace, _window, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        workspace.state.update(cx, |state, cx| {
                            state.register_project(path);
                            cx.notify();
                        });
                    }
                }
                Ok(Ok(None)) => {}
                other => workspace.state.update(cx, |state, cx| {
                    state.notice(
                        NoticeLevel::Warning,
                        format!("Folder picker failed: {other:?}"),
                    );
                    cx.notify();
                }),
            });
        })
        .detach();
    }

    pub(super) fn archive_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(id) = state.selected_project else {
            return;
        };
        if let Some(reason) = state.archive_project_reason() {
            self.state.update(cx, |state, cx| {
                state.notice(NoticeLevel::Warning, reason);
                cx.notify();
            });
            return;
        }
        let Some(mut project) = state
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == id)
            .cloned()
        else {
            return;
        };
        let project_name = project.name.clone();
        let open_threads = state
            .open_threads
            .iter()
            .copied()
            .filter(|thread_id| {
                state
                    .snapshot
                    .threads
                    .iter()
                    .any(|thread| thread.id == *thread_id && thread.project_id == id)
            })
            .collect::<Vec<_>>();
        let selected_thread = state
            .selected_thread
            .filter(|thread_id| open_threads.contains(thread_id));
        let previous_project = self.state.read(cx).selected_project;
        self.state.update(cx, |state, cx| {
            state.close_project_threads(id);
            project.archived = true;
            state.save_project(project);
            cx.notify();
        });
        self.sync_thread_panels(window, cx);
        let owner = cx.entity().downgrade();
        window.push_notification(
            Notification::success(format!("Archived {project_name}")).action(move |_, _, cx| {
                let owner = owner.clone();
                let open_threads = open_threads.clone();
                let notification = cx.entity().downgrade();
                Button::new("undo-project-archive")
                    .small()
                    .label("Undo")
                    .on_click(move |_, window, cx| {
                        let restored = owner
                            .update(cx, |workspace, cx| {
                                workspace.undo_project_archive(
                                    id,
                                    open_threads.clone(),
                                    selected_thread,
                                    previous_project,
                                    window,
                                    cx,
                                )
                            })
                            .is_ok_and(|restored| restored);
                        if restored {
                            let _ = notification
                                .update(cx, |notification, cx| notification.dismiss(window, cx));
                        }
                    })
            }),
            cx,
        );
    }

    pub(super) fn undo_project_archive(
        &mut self,
        id: ProjectId,
        open_threads: Vec<ThreadId>,
        selected_thread: Option<ThreadId>,
        previous_project: Option<ProjectId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let restored = self.state.update(cx, |state, cx| {
            if let Err(error) = state.unarchive_project(id) {
                state.notice(NoticeLevel::Warning, format!("{error:#}"));
                return false;
            }
            for thread in &open_threads {
                state.select_thread(*thread);
            }
            if let Some(selected) = selected_thread {
                state.select_thread(selected);
            } else {
                state.select_terminal_project(previous_project.unwrap_or(id));
            }
            cx.notify();
            true
        });
        if restored {
            self.sync_thread_panels(window, cx);
        }
        restored
    }

    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).shutting_down
            || self
                .extension
                .as_ref()
                .is_some_and(|form| !form.inline_answer)
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        self.dismiss(window, cx);
        if self.detail_surface.is_some()
            || self.has_open_form(cx)
            || self.palette.is_some()
            || self.theme_editor.is_some()
        {
            return;
        }
        self.palette = Some(Palette::open(
            cx.entity().downgrade(),
            self.state.clone(),
            window,
            cx,
        ));
        cx.notify();
    }
    pub(super) fn open_theme_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.extension.is_some()
            || self.state.read(cx).shutting_down
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        self.dismiss(window, cx);
        if self.detail_surface.is_some()
            || self.has_open_form(cx)
            || self.palette.is_some()
            || self.theme_editor.is_some()
        {
            return;
        }
        self.theme_editor = Some(crate::ui::theme::editor::ThemeEditor::open(
            cx.entity().downgrade(),
            self.state.clone(),
            window,
            cx,
        ));
        cx.notify();
    }
    pub fn run_palette_target(
        &mut self,
        target: PaletteTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            PaletteTarget::Command(command) => self.execute(command, window, cx),
            PaletteTarget::Project(id) => self.select_row(Row::Project(id), true, window, cx),
            PaletteTarget::Thread(id) => {
                self.select_row(Row::Thread(id), true, window, cx);
                self.focus_region(Region::Thread, window, cx);
            }
        }
    }
}
