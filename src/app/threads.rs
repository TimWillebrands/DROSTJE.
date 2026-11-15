use super::*;

impl AppState {
    pub fn thread(&self) -> Option<&Thread> {
        self.selected_thread
            .filter(|id| self.thread_in_active_space(*id))
            .and_then(|id| self.snapshot.threads.iter().find(|t| t.id == id))
    }
    pub fn conversation_thread(&self) -> Option<&Thread> {
        self.thread()
            .filter(|thread| thread.kind == ThreadKind::Conversation)
    }
    pub fn factory_thread(&self) -> Option<&Thread> {
        self.thread()
            .filter(|thread| thread.kind == ThreadKind::Factory)
    }

    /// Whether `thread` was never used: a placeholder made by New conversation or New
    /// Factory thread that Projects lists only while its tab is open, so an abandoned one
    /// disappears without being archived or deleted. Every user activity (drafting, sending,
    /// queueing, renaming, editing a Factory configuration, archiving) bumps `updated_at`,
    /// so an unused row still carries its creation timestamp and default title. Choosing a
    /// harness, model or effort is configuration, not activity. A Host-bound draft remains
    /// discoverable so its Host can be changed or released. Live state also counts as activity.
    pub fn is_untouched(&self, thread: &Thread) -> bool {
        let id = thread.id;
        let unused = !thread.archived
            && thread.remote.is_none()
            && thread.created_at == thread.updated_at
            && thread.draft.is_empty()
            && thread.attachments.is_empty()
            && thread.submission.is_none()
            && thread.queued.is_empty()
            && !self.active_threads.contains(&id);
        unused
            && match thread.kind {
                ThreadKind::Conversation => {
                    thread.title == crate::titles::DEFAULT_CONVERSATION_TITLE
                        && self.conversations.get(&id).is_none_or(|session| {
                            !session.preparing
                                && !session.messages.iter().any(|message| {
                                    matches!(message.role.as_str(), "user" | "assistant")
                                })
                        })
                }
                ThreadKind::Factory => {
                    thread.title == DEFAULT_FACTORY_TITLE
                        && !self.pending_factory.contains(&id)
                        && !self
                            .snapshot
                            .launches
                            .iter()
                            .any(|launch| launch.factory_thread_id == id)
                }
            }
    }

    /// Whether Projects and the palette list `thread` in `space`: an untouched thread only
    /// while its tab is open in that space.
    pub fn thread_listed_in(&self, thread: &Thread, space: SpaceId) -> bool {
        if !self.thread_in_space(thread.id, space) {
            return false;
        }
        self.thread_open_in(thread.id, space) || !self.is_untouched(thread)
    }

    /// Whether `id` has a tab in `space`, active or not.
    pub(crate) fn thread_open_in(&self, id: ThreadId, space: SpaceId) -> bool {
        if space == self.active_space {
            self.open_threads.contains(&id)
        } else {
            self.space_session(space)
                .is_some_and(|session| session.open_threads.contains(&id))
        }
    }

    pub fn is_conversation(&self, id: ThreadId) -> bool {
        self.snapshot
            .threads
            .iter()
            .any(|thread| thread.id == id && thread.kind == ThreadKind::Conversation)
    }

    pub fn rename_thread_reason(&self) -> Option<String> {
        let Some(thread) = self.thread() else {
            return Some("Select a thread first.".into());
        };
        self.rename_thread_reason_for(thread.id)
    }
    /// Why `id` cannot be renamed right now, independent of the current selection.
    pub fn rename_thread_reason_for(&self, id: ThreadId) -> Option<String> {
        if !self.thread_in_active_space(id) {
            return Some("This thread belongs to another space.".into());
        }
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is read-only: {error}"));
        }
        let Some(thread) = self.snapshot.threads.iter().find(|thread| thread.id == id) else {
            return Some("This thread is no longer available.".into());
        };
        let Some(project) = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == thread.project_id)
        else {
            return Some("This thread's project is no longer available.".into());
        };
        if project.archived {
            return Some("This project is archived. History is read-only.".into());
        }
        thread
            .archived
            .then(|| "Archived threads are read-only.".into())
    }
    pub fn archive_thread_reason(&self) -> Option<String> {
        if let Some(reason) = self.mutable_project_reason() {
            return Some(reason);
        }
        let Some(thread) = self.thread() else {
            return Some("Select a thread first.".into());
        };
        self.archive_thread_reason_for(thread.id)
    }
    /// Why `id` cannot be archived right now, independent of the current selection.
    pub fn archive_thread_reason_for(&self, id: ThreadId) -> Option<String> {
        if !self.thread_in_active_space(id) {
            return Some("This thread belongs to another space.".into());
        }
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is read-only: {error}"));
        }
        let Some(thread) = self.snapshot.threads.iter().find(|t| t.id == id) else {
            return Some("This thread is no longer available.".into());
        };
        if self
            .snapshot
            .projects
            .iter()
            .any(|p| p.id == thread.project_id && p.archived)
        {
            return Some("This project is archived. History is read-only.".into());
        }
        if thread.archived {
            return Some("This thread is already archived.".into());
        }
        if self.active_threads.contains(&thread.id)
            || self.pending_factory.contains(&thread.id)
            || self
                .snapshot
                .launches
                .iter()
                .any(|l| l.factory_thread_id == thread.id && self.owned_launches.contains(&l.id))
        {
            Some(
                "This thread has an active turn, pending Factory start, or owned Factory process."
                    .into(),
            )
        } else {
            None
        }
    }

    /// Why `id` cannot return from the archive right now.
    pub fn unarchive_thread_reason_for(&self, id: ThreadId) -> Option<String> {
        if !self.thread_in_active_space(id) {
            return Some("This thread belongs to another space.".into());
        }
        if let Some(reason) = self.space_mutation_reason() {
            return Some(reason);
        }
        let Some(thread) = self.snapshot.threads.iter().find(|t| t.id == id) else {
            return Some("This thread is no longer available.".into());
        };
        if !thread.archived {
            return Some("This thread is not archived.".into());
        }
        if self
            .snapshot
            .projects
            .iter()
            .any(|p| p.id == thread.project_id && p.archived)
        {
            return Some("This project is archived. History is read-only.".into());
        }
        if self.thread_worktree(thread).is_some_and(|w| !w.is_live()) {
            return Some("This thread's worktree was removed.".into());
        }
        None
    }
    pub fn unarchive_thread_reason(&self) -> Option<String> {
        match self.thread() {
            Some(thread) => self.unarchive_thread_reason_for(thread.id),
            None => Some("Select a thread first.".into()),
        }
    }
    /// Returns an archived thread to Projects with its history and draft.
    pub fn unarchive_thread(&mut self, id: ThreadId) -> Result<()> {
        if let Some(reason) = self.unarchive_thread_reason_for(id) {
            anyhow::bail!(reason);
        }
        let mut thread = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .cloned()
            .context("This thread is no longer available.")?;
        thread.archived = false;
        thread.updated_at = platform::now();
        self.save_thread(thread);
        Ok(())
    }

    pub fn select_thread(&mut self, id: ThreadId) {
        if !self.thread_in_active_space(id) {
            return;
        }
        let Some(thread) = self.snapshot.threads.iter().find(|thread| thread.id == id) else {
            return;
        };
        let launch_id = (thread.kind == ThreadKind::Factory)
            .then(|| {
                self.snapshot
                    .launches
                    .iter()
                    .find(|launch| launch.factory_thread_id == id)
                    .map(|launch| launch.id)
            })
            .flatten();
        self.selected_project = Some(thread.project_id);
        self.unseen_turns.seen(id);
        if self.selected_thread != Some(id)
            && let Some(previous) = self.selected_thread
        {
            self.commit_thread_edits(previous);
        }
        self.selected_thread = Some(id);
        if !self.open_threads.contains(&id) {
            self.open_threads.push(id);
        }
        self.select_launch_output(launch_id);
        self.persist_space_session();
    }
    pub fn close_thread(&mut self, id: ThreadId) {
        if !self.thread_in_active_space(id) {
            return;
        }
        self.pause_network_delivery(id);
        self.commit_thread_edits(id);
        let closed_ix = self.open_threads.iter().position(|open| *open == id);
        if let Some(ix) = closed_ix {
            self.open_threads.remove(ix);
        }
        if self.is_conversation(id)
            && !self.active_threads.contains(&id)
            && let Some(session) = self.conversations.remove(&id)
        {
            self.close_artifacts(id);
            if let Some(commands) = session.commands {
                let _ = commands.try_send(ConversationCommand::Close);
            }
        }
        if self.selected_thread == Some(id) {
            if let Some(next) = closed_ix
                .and_then(|ix| {
                    self.open_threads
                        .get(ix.min(self.open_threads.len().saturating_sub(1)))
                })
                .copied()
            {
                self.select_thread(next);
            } else {
                self.selected_thread = None;
                self.select_launch_output(None);
            }
        }
        self.persist_space_session();
        self.sync_network();
    }

    pub fn new_thread(&mut self, kind: ThreadKind) -> Result<()> {
        self.new_thread_with_harness(kind, None)
    }
    pub fn new_conversation(&mut self, harness: &str) -> Result<()> {
        anyhow::ensure!(
            harness::CONVERSATION_HARNESSES.contains(&harness),
            "Unsupported conversation harness: {harness}"
        );
        self.new_thread_with_harness(ThreadKind::Conversation, Some(harness))
    }
    /// Starts a fresh Conversation in an explicit checkout; `None` means the project root,
    /// regardless of the selected Conversation's worktree.
    pub fn new_conversation_in_workspace(&mut self, worktree_id: Option<WorktreeId>) -> Result<()> {
        if let Some(reason) = self.mutable_project_reason() {
            anyhow::bail!(reason);
        }
        anyhow::ensure!(
            !self.worktree_request_pending(),
            "A worktree operation is in progress."
        );
        let project = self.project().context("Select a project")?.clone();
        if let Some(id) = worktree_id {
            let worktree = self.worktree(id).context("Worktree not found")?;
            anyhow::ensure!(
                worktree.project_id == project.id,
                "This worktree belongs to another project."
            );
            anyhow::ensure!(worktree.is_live(), "This worktree was removed.");
        }
        let id = Uuid::new_v4();
        let thread = self.build_conversation(&project, None, id, worktree_id);
        self.save_thread(thread);
        self.register_thread_membership(id, project.id, self.active_space)?;
        self.select_thread(id);
        Ok(())
    }
    pub(super) fn new_thread_with_harness(
        &mut self,
        kind: ThreadKind,
        conversation_harness: Option<&str>,
    ) -> Result<()> {
        if let Some(reason) = self.mutable_project_reason() {
            anyhow::bail!(reason);
        }
        let project = self.project().context("Select a project")?.clone();
        // A new thread follows the selected Conversation's checkout: its live worktree or the
        // project root. Factory threads only borrow the branch as their default target.
        let selected_worktree = self
            .thread()
            .filter(|thread| {
                thread.kind == ThreadKind::Conversation && thread.project_id == project.id
            })
            .and_then(|thread| self.thread_worktree(thread))
            .filter(|worktree| worktree.is_live())
            .cloned();
        let id = Uuid::new_v4();
        let thread = match kind {
            ThreadKind::Conversation => self.build_conversation(
                &project,
                conversation_harness,
                id,
                selected_worktree.map(|worktree| worktree.id),
            ),
            ThreadKind::Factory => {
                let timestamp = platform::now();
                Thread {
                    id,
                    project_id: project.id,
                    kind,
                    remote: None,
                    execution_started: false,
                    factory: Some(FactoryThreadConfig {
                        source_kind: FactorySourceKind::WorkItem,
                        source: String::new(),
                        harness: if project.defaults.factory_harness.trim().is_empty() {
                            "opencode".into()
                        } else {
                            project.defaults.factory_harness.clone()
                        },
                        model: String::new(),
                        thought_level: String::new(),
                        target_branch: project
                            .defaults
                            .factory_target_branch
                            .clone()
                            .or_else(|| {
                                selected_worktree
                                    .as_ref()
                                    .map(|worktree| worktree.branch.clone())
                            })
                            .unwrap_or_else(|| {
                                self.repositories
                                    .get(&project.id)
                                    .and_then(|repository| repository.as_ref().ok())
                                    .and_then(|repository| repository.branch.clone())
                                    .unwrap_or_default()
                            }),
                    }),
                    title: DEFAULT_FACTORY_TITLE.into(),
                    harness: String::new(),
                    session_ref: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    worktree_id: None,
                    pull_request: None,
                    draft: String::new(),
                    attachments: Vec::new(),
                    submission: None,
                    queued: Vec::new(),
                    archived: false,
                    created_at: timestamp.clone(),
                    updated_at: timestamp,
                }
            }
        };
        let project_id = thread.project_id;
        self.save_thread(thread);
        self.register_thread_membership(id, project_id, self.active_space)?;
        self.select_thread(id);
        Ok(())
    }
    /// A fresh Conversation row in `project`, on `worktree_id` or the project root. It copies
    /// the selected Conversation's harness settings (see `conversation_defaults`).
    pub(crate) fn build_conversation(
        &self,
        project: &Project,
        harness: Option<&str>,
        id: ThreadId,
        worktree_id: Option<WorktreeId>,
    ) -> Thread {
        let timestamp = platform::now();
        let defaults = conversation::conversation_defaults(
            &self.snapshot.threads,
            project,
            self.conversation_thread(),
            harness,
        );
        Thread {
            id,
            project_id: project.id,
            kind: ThreadKind::Conversation,
            factory: None,
            title: crate::titles::DEFAULT_CONVERSATION_TITLE.into(),
            harness: defaults.harness,
            session_ref: None,
            model: defaults.model,
            effort: defaults.effort,
            permission_mode: defaults.permission_mode,
            worktree_id,
            remote: None,
            execution_started: false,
            pull_request: None,
            draft: String::new(),
            attachments: Vec::new(),
            submission: None,
            queued: Vec::new(),
            archived: false,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        }
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod tests {
    use crate::app::{
        AppState, Bootstrap, ConversationSession, WorktreeRequest, WorktreeRequestKind,
        WorktreeRequestStatus,
    };
    use crate::model::{ImageAttachment, ModelKey, Project, ProjectDefaults, ThreadKind, Worktree};
    use crate::store::Store;
    use gpui_kit::{AppContext as _, Entity, TestAppContext};
    use std::{collections::BTreeMap, fs, path::PathBuf};
    use uuid::Uuid;

    struct TempDirectory(PathBuf);
    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn open(cx: &mut TestAppContext) -> (TempDirectory, Entity<AppState>, Uuid) {
        // Persistence acknowledges writes from real background threads, as in production.
        cx.dispatcher.allow_parking();
        let dir = std::env::temp_dir().join(format!("drostje-app-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let dir = TempDirectory(dir);
        let store = Store::open(dir.0.join("data")).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Project".into(),
            root: dir.0.join("root"),
            archived: false,
            defaults: ProjectDefaults::default(),
        };
        let id = project.id;
        smol::block_on(store.save_project(project)).unwrap();
        let snapshot = smol::block_on(store.snapshot()).unwrap();
        let bootstrap = Bootstrap {
            data_dir: dir.0.join("data"),
            store,
            snapshot,
            diagnostics: BTreeMap::new(),
            repositories: BTreeMap::new(),
            browser_tools: None,
        };
        let state = cx.new(|cx| AppState::new(bootstrap, cx));
        state.update(cx, |state, _| state.select_project(id));
        (dir, state, id)
    }

    #[gpui_kit::test]
    fn terminal_sidebar_persists_for_conversation_and_factory(cx: &mut TestAppContext) {
        let (_dir, state, _) = open(cx);
        state.update(cx, |state, _| {
            for kind in [ThreadKind::Conversation, ThreadKind::Factory] {
                state.new_thread(kind).unwrap();
                let id = state.selected_thread.unwrap();
                state.show_thread_sidebar(id, crate::app::SidebarTab::Terminal);
                assert_eq!(
                    state.thread_sidebar_tab(id),
                    crate::app::SidebarTab::Terminal
                );
                state.toggle_thread_sidebar(id, crate::app::SidebarTab::Terminal);
                assert!(!state.thread_sidebar_open(id));
                assert_eq!(
                    state.thread_sidebar_tab(id),
                    crate::app::SidebarTab::Terminal
                );
                assert_eq!(
                    state.snapshot.settings["thread_sidebar_tab"][id.to_string()],
                    "terminal"
                );
                state.show_thread_sidebar(id, crate::app::SidebarTab::Output);
                state.show_thread_sidebar(id, crate::app::SidebarTab::Terminal);
                assert!(state.thread_sidebar_open(id));
                assert_eq!(
                    state.thread_sidebar_tab(id),
                    crate::app::SidebarTab::Terminal
                );
            }
        });
    }

    fn new_conversation(state: &Entity<AppState>, cx: &mut TestAppContext) -> Uuid {
        state.update(cx, |state, _| {
            state.new_thread(ThreadKind::Conversation).unwrap();
            state.selected_thread.unwrap()
        })
    }

    fn worktree(project_id: Uuid) -> Worktree {
        let id = Uuid::new_v4();
        Worktree {
            id,
            project_id,
            path: std::env::temp_dir().join(id.to_string()),
            codename: "quiet-forest".into(),
            branch: "worktree-branch".into(),
            base: "main".into(),
            named: true,
            created_by: Uuid::new_v4(),
            created_at: "2026-01-01T00:00:00Z".into(),
            removed_at: None,
        }
    }

    #[gpui_kit::test]
    fn read_only_storage_keeps_saved_threads_in_memory_only(cx: &mut TestAppContext) {
        let (_dir, state, _) = open(cx);
        let id = new_conversation(&state, cx);
        state.update(cx, |state, _| {
            let mut thread = state.thread().unwrap().clone();
            thread.draft = "typed".into();
            state.edit_thread(thread.clone());
            assert!(state.edited_threads.contains(&id));

            state.storage_error = Some("read-only test store".into());
            let pending = state.pending_writes;
            thread.title = "Renamed".into();
            state.save_thread(thread.clone());
            assert_eq!(state.thread().unwrap().title, "Renamed");
            assert_eq!(state.pending_writes, pending, "nothing is queued");
            assert!(
                state.edited_threads.contains(&id),
                "the edit still awaits a writable store"
            );

            thread.title = "Persisted".into();
            let reply = state.persist_thread(thread.clone());
            assert_eq!(state.thread().unwrap().title, "Persisted");
            assert_eq!(
                reply.try_recv().unwrap(),
                Err("read-only test store".into())
            );
            assert_eq!(state.pending_writes, pending);

            state.storage_error = None;
            thread.title = "Saved".into();
            state.save_thread(thread);
            assert_eq!(state.thread().unwrap().title, "Saved");
            assert_eq!(state.pending_writes, pending + 1);
            assert!(!state.edited_threads.contains(&id));
            assert_eq!(
                state.snapshot.threads.iter().filter(|t| t.id == id).count(),
                1
            );
        });
    }

    /// Gives `id` the activity a sent prompt leaves behind.
    fn use_thread(state: &Entity<AppState>, id: Uuid, cx: &mut TestAppContext) {
        state.update(cx, |state, _| {
            let mut thread = state
                .snapshot
                .threads
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .unwrap();
            thread.title = "Used".into();
            thread.updated_at = "2999-01-01T00:00:00Z".into();
            state.save_thread(thread);
        });
    }

    #[gpui_kit::test]
    fn untouched_threads_leave_projects_with_their_tab(cx: &mut TestAppContext) {
        let (_dir, state, _) = open(cx);
        let draft = new_conversation(&state, cx);
        let used = new_conversation(&state, cx);
        use_thread(&state, used, cx);
        state.update(cx, |state, _| {
            let listed = |state: &AppState, id| {
                let thread = state.snapshot.threads.iter().find(|t| t.id == id).unwrap();
                state.thread_listed_in(thread, state.active_space)
            };
            let thread = state
                .snapshot
                .threads
                .iter()
                .find(|t| t.id == draft)
                .unwrap();
            assert!(state.is_untouched(thread));
            assert!(listed(state, draft), "An open placeholder is listed");
            state.close_thread(draft);
            state.close_thread(used);
            assert!(
                !listed(state, draft),
                "A closed placeholder leaves Projects"
            );
            assert!(
                listed(state, used),
                "A used thread stays after its tab closes"
            );
            assert!(
                state.snapshot.threads.iter().any(|t| t.id == draft),
                "Nothing is deleted"
            );
        });
    }

    #[gpui_kit::test]
    fn host_bound_empty_conversation_stays_discoverable_after_close(cx: &mut TestAppContext) {
        cx.dispatcher.allow_parking();
        let (_dir, state, project) = open(cx);
        let id = new_conversation(&state, cx);
        state.update(cx, |state, _| {
            state
                .save_remote_host(crate::remote::RemoteHostConfig {
                    id: "empty-host".into(),
                    name: "Empty Host".into(),
                    relay_url: "wss://127.0.0.1:1/remote".into(),
                    token_env: format!("DROSTJE_TEST_UNUSED_{}", Uuid::new_v4().simple()),
                    projects: BTreeMap::from([(project.to_string(), "mounted".into())]),
                })
                .unwrap();
            state.set_thread_host(id, Some("empty-host")).unwrap();
            state.close_thread(id);
            let thread = state
                .snapshot
                .threads
                .iter()
                .find(|thread| thread.id == id)
                .unwrap();
            assert!(!state.is_untouched(thread));
            assert!(state.thread_listed_in(thread, state.active_space));
            assert!(!state.open_threads.contains(&id));
            assert!(state.remove_remote_host_reason("empty-host").is_some());
            state.select_thread(id);
            assert_eq!(state.selected_thread, Some(id));
            assert!(state.open_threads.contains(&id));
            state.set_thread_host(id, None).unwrap();
            assert!(state.remove_remote_host_reason("empty-host").is_none());
            state.remove_remote_host("empty-host").unwrap();
        });
    }

    #[gpui_kit::test]
    fn archiving_closes_the_tab_without_selecting_the_thread(cx: &mut TestAppContext) {
        let (_dir, state, _) = open(cx);
        let archived = new_conversation(&state, cx);
        use_thread(&state, archived, cx);
        let current = new_conversation(&state, cx);
        use_thread(&state, current, cx);
        state.update(cx, |state, _| {
            state.archive_thread(archived).unwrap();
            assert_eq!(state.selected_thread, Some(current), "Selection stays put");
            assert_eq!(state.open_threads, vec![current], "The archived tab closed");
            assert!(!state.show_archived, "Archived threads stay hidden");
            assert!(state.unarchive_thread_reason_for(archived).is_none());
            state.unarchive_thread(archived).unwrap();
            assert!(
                !state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == archived)
                    .unwrap()
                    .archived
            );
            assert_eq!(
                state.open_threads,
                vec![current],
                "Unarchiving opens nothing"
            );

            state.archive_thread(current).unwrap();
            assert_eq!(state.selected_thread, None);
            assert!(state.open_threads.is_empty());
        });
    }
    #[gpui_kit::test]
    fn undoing_project_archive_restores_its_read_only_state(cx: &mut TestAppContext) {
        cx.dispatcher.allow_parking();
        let (_dir, state, project) = open(cx);
        state.update(cx, |state, _| {
            let mut archived = state.project().unwrap().clone();
            archived.archived = true;
            state.save_project(archived);
            assert!(state.project().unwrap().archived);
            state.unarchive_project(project).unwrap();
            assert!(!state.project().unwrap().archived);
            assert!(state.unarchive_project(project).is_err());
        });
    }

    #[gpui_kit::test]
    fn a_new_conversation_copies_the_selected_one(cx: &mut TestAppContext) {
        let (_dir, state, _) = open(cx);
        let source = new_conversation(&state, cx);
        state.update(cx, |state, _| {
            let mut thread = state
                .snapshot
                .threads
                .iter()
                .find(|t| t.id == source)
                .cloned()
                .unwrap();
            thread.harness = "claude-code".into();
            thread.effort = Some("high".into());
            thread.permission_mode = Some("plan".into());
            state.save_thread(thread);
        });
        let copy = new_conversation(&state, cx);
        state.update(cx, |state, _| {
            let thread = state
                .snapshot
                .threads
                .iter()
                .find(|t| t.id == copy)
                .unwrap();
            assert_eq!(thread.harness, "claude-code");
            assert_eq!(thread.effort.as_deref(), Some("high"));
            assert_eq!(thread.permission_mode.as_deref(), Some("plan"));
        });
    }

    #[gpui_kit::test]
    fn switching_to_auto_persists_only_a_harness_auto_mode(cx: &mut TestAppContext) {
        use crate::harness::{UserAnswer, UserRequest, UserRequestKind};
        cx.dispatcher.allow_parking();
        let (_dir, state, _) = open(cx);
        for (harness, expected) in [
            ("codex", Some("auto")),
            ("claude-code", Some("auto")),
            ("opencode", Some("plan")),
        ] {
            let id = new_conversation(&state, cx);
            state.update(cx, |state, _| {
                let mut thread = state.thread().unwrap().clone();
                thread.harness = harness.into();
                thread.permission_mode = Some("plan".into());
                state.save_thread(thread);
                let mut session = ConversationSession::new(None);
                session.requests.insert(
                    "ask".into(),
                    UserRequest {
                        id: "ask".into(),
                        kind: UserRequestKind::Permission,
                        title: "Allow?".into(),
                        options: Vec::new(),
                        text: String::new(),
                        placeholder: None,
                        deadline: None,
                    },
                );
                state.conversations.insert(id, session);
                state.respond(id, "ask".into(), UserAnswer::PermissionAuto);
                let thread = state.snapshot.threads.iter().find(|t| t.id == id).unwrap();
                assert_eq!(thread.permission_mode.as_deref(), expected, "{harness}");
            });
        }
    }

    #[gpui_kit::test]
    fn explicit_project_root_does_not_inherit_the_selected_worktree(cx: &mut TestAppContext) {
        let (_dir, state, project) = open(cx);
        let source = new_conversation(&state, cx);
        state.update(cx, |state, _| {
            let workspace = worktree(project);
            let workspace_id = workspace.id;
            state.save_worktree(workspace);
            let mut thread = state.thread().unwrap().clone();
            thread.worktree_id = Some(workspace_id);
            state.save_thread(thread);

            state.new_thread(ThreadKind::Conversation).unwrap();
            assert_eq!(state.thread().unwrap().worktree_id, Some(workspace_id));
            state.select_thread(source);
            state.new_conversation_in_workspace(None).unwrap();

            let thread = state.thread().unwrap();
            assert_ne!(thread.id, source);
            assert_eq!(thread.project_id, project);
            assert_eq!(thread.worktree_id, None);
            assert!(state.thread_in_space(thread.id, state.active_space));
            assert!(state.open_threads.contains(&thread.id));
            assert_eq!(
                state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == source)
                    .unwrap()
                    .worktree_id,
                Some(workspace_id)
            );
        });
    }

    #[gpui_kit::test]
    fn choosing_a_workspace_preserves_source_draft_session_and_defaults(cx: &mut TestAppContext) {
        let (_dir, state, project) = open(cx);
        let source = new_conversation(&state, cx);
        state.update(cx, |state, _| {
            let source_workspace = worktree(project);
            let source_workspace_id = source_workspace.id;
            state.save_worktree(source_workspace);
            let target = worktree(project);
            let target_id = target.id;
            state.save_worktree(target);
            let mut thread = state.thread().unwrap().clone();
            thread.worktree_id = Some(source_workspace_id);
            thread.harness = "claude-code".into();
            thread.model = Some(ModelKey {
                provider: "anthropic".into(),
                id: "claude-opus".into(),
            });
            thread.effort = Some("high".into());
            thread.permission_mode = Some("plan".into());
            thread.session_ref = Some("existing-provider-session".into());
            thread.draft = "Keep this unsent prompt".into();
            thread.attachments.push(ImageAttachment {
                id: Uuid::new_v4(),
                path: PathBuf::from("/tmp/draft.png"),
                mime: "image/png".into(),
                width: 10,
                height: 20,
                original: None,
            });
            let original = serde_json::to_value(&thread).unwrap();
            let model = thread.model.clone();
            state.edit_thread(thread);
            let session = ConversationSession::new(model.clone());
            let generation = session.generation;
            state.conversations.insert(source, session);

            state
                .new_conversation_in_workspace(Some(target_id))
                .unwrap();

            let thread = state.thread().unwrap();
            assert_ne!(thread.id, source);
            assert_eq!(thread.project_id, project);
            assert_eq!(thread.worktree_id, Some(target_id));
            assert_eq!(thread.kind, ThreadKind::Conversation);
            assert_eq!(thread.harness, "claude-code");
            assert_eq!(thread.model, model);
            assert_eq!(thread.effort.as_deref(), Some("high"));
            assert_eq!(thread.permission_mode.as_deref(), Some("plan"));
            assert_eq!(thread.title, crate::titles::DEFAULT_CONVERSATION_TITLE);
            assert!(thread.session_ref.is_none());
            assert!(thread.draft.is_empty());
            assert!(thread.attachments.is_empty());
            assert!(thread.submission.is_none());
            assert!(thread.queued.is_empty());
            assert!(!thread.archived);
            assert!(thread.factory.is_none());
            assert_eq!(thread.created_at, thread.updated_at);
            assert!(!state.conversations.contains_key(&thread.id));
            assert_eq!(
                state.conversations.get(&source).unwrap().generation,
                generation
            );
            assert_eq!(
                serde_json::to_value(
                    state
                        .snapshot
                        .threads
                        .iter()
                        .find(|t| t.id == source)
                        .unwrap()
                )
                .unwrap(),
                original
            );
        });
    }

    #[gpui_kit::test]
    fn workspace_creation_without_a_source_uses_project_defaults(cx: &mut TestAppContext) {
        let (_dir, state, project) = open(cx);
        state.update(cx, |state, _| {
            let defaults = &mut state
                .snapshot
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .defaults;
            defaults.interactive_harness = "claude-code".into();
            defaults.interactive_model = Some(ModelKey {
                provider: "anthropic".into(),
                id: "claude-opus".into(),
            });
            let model = defaults.interactive_model.clone();
            state.new_conversation_in_workspace(None).unwrap();
            let thread = state.thread().unwrap();
            assert_eq!(thread.project_id, project);
            assert_eq!(thread.worktree_id, None);
            assert_eq!(thread.harness, "claude-code");
            assert_eq!(thread.model, model);
            assert_eq!(thread.effort, None);
            assert_eq!(thread.permission_mode, None);
        });
    }

    #[gpui_kit::test]
    fn invalid_workspaces_do_not_create_or_select_a_thread(cx: &mut TestAppContext) {
        let (_dir, state, project) = open(cx);
        let source = new_conversation(&state, cx);
        state.update(cx, |state, _| {
            let mut removed = worktree(project);
            removed.removed_at = Some("2026-02-01T00:00:00Z".into());
            let removed_id = removed.id;
            state.save_worktree(removed);
            let foreign = worktree(Uuid::new_v4());
            let foreign_id = foreign.id;
            state.save_worktree(foreign);
            let threads = serde_json::to_value(&state.snapshot.threads).unwrap();
            let open = state.open_threads.clone();

            for id in [Uuid::new_v4(), removed_id, foreign_id] {
                assert!(state.new_conversation_in_workspace(Some(id)).is_err());
                assert_eq!(state.selected_thread, Some(source));
                assert_eq!(state.open_threads, open);
                assert_eq!(
                    serde_json::to_value(&state.snapshot.threads).unwrap(),
                    threads
                );
            }
            state.worktree_request = Some(WorktreeRequest {
                id: Uuid::new_v4(),
                kind: WorktreeRequestKind::Remove {
                    worktree: removed_id,
                    force: false,
                },
                status: WorktreeRequestStatus::Pending,
            });
            assert!(state.new_conversation_in_workspace(None).is_err());
            assert_eq!(state.selected_thread, Some(source));
            assert_eq!(state.open_threads, open);
            assert_eq!(
                serde_json::to_value(&state.snapshot.threads).unwrap(),
                threads
            );
        });
    }

    #[gpui_kit::test]
    fn workspace_creation_rejects_immutable_or_missing_projects(cx: &mut TestAppContext) {
        let (_dir, state, project) = open(cx);
        state.update(cx, |state, _| {
            state
                .snapshot
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .archived = true;
            assert!(state.new_conversation_in_workspace(None).is_err());
            state
                .snapshot
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .archived = false;
            state.storage_error = Some("read-only test store".into());
            assert!(state.new_conversation_in_workspace(None).is_err());
            state.storage_error = None;
            state.shutting_down = true;
            assert!(state.new_conversation_in_workspace(None).is_err());
            state.shutting_down = false;
            state.selected_project = None;
            assert!(state.new_conversation_in_workspace(None).is_err());
            assert!(state.snapshot.threads.is_empty());
            assert!(state.open_threads.is_empty());
            assert_eq!(state.selected_thread, None);
        });
    }

    #[gpui_kit::test]
    fn closed_pull_requests_archive_their_conversation_once_it_is_idle(cx: &mut TestAppContext) {
        use crate::app::{AppEvent, LinkStatus};
        use crate::model::{PullRequestLink, PullRequestState};
        let (_dir, state, project) = open(cx);
        let id = new_conversation(&state, cx);
        use_thread(&state, id, cx);
        state.update(cx, |state, cx| {
            let tree = worktree(project);
            let tree_id = tree.id;
            state.save_worktree(tree);
            let mut thread = state.thread().unwrap().clone();
            thread.worktree_id = Some(tree_id);
            thread.pull_request = Some(PullRequestLink {
                id: 3170,
                url: "https://dev.azure.com/o/p/_git/r/pullrequest/3170".into(),
                title: "Fix upload".into(),
                state: PullRequestState::Completed,
                work_items: vec![2596],
                archived_on_close: false,
            });
            state.save_thread(thread);
            let find = |state: &AppState| {
                state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == id)
                    .unwrap()
                    .clone()
            };

            // A running turn holds the archive back and says why.
            state.active_threads.insert(id);
            state.settle_closed_pull_requests();
            assert!(!find(state).archived);
            assert!(matches!(
                state.pull_request_status(id),
                Some(LinkStatus::ArchivePending(_))
            ));

            // The turn ends: the Conversation archives, naming the pull request.
            state.receive(
                AppEvent::InteractiveActivity {
                    thread_id: id,
                    active: false,
                },
                cx,
            );
            let archived = find(state);
            assert!(archived.archived);
            assert!(archived.pull_request.unwrap().archived_on_close);
            assert!(state.pull_request_status(id).is_none());
            assert!(
                state
                    .notices
                    .iter()
                    .any(|notice| notice.text.contains("pull request 3170 was completed"))
            );

            // Unarchived by the user, it stays.
            state.unarchive_thread(id).unwrap();
            state.settle_closed_pull_requests();
            assert!(!find(state).archived);
        });
    }
}
