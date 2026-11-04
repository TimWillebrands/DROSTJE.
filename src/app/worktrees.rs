//! Worktree lifecycle owned by the App layer: creation requests from the dialog, the one-time
//! branch rename after the first generated title, and removal (automatic when clean and
//! merged, explicit otherwise). Git runs off-thread through `platform`; results come back as
//! `AppEvent`s tagged with the request or worktree identity and stale ones are dropped.

use super::*;
use crate::worktrees::{self as naming, Removal};

/// Which checkout a Conversation's processes and Changes read: the project root or one of
/// its linked worktrees. `Copy` so it can ride in `PresentationChange`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkingTree {
    Project(ProjectId),
    Worktree(WorktreeId),
}

/// The single worktree operation in flight; the dialog that started it observes the status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeRequest {
    pub id: Uuid,
    pub kind: WorktreeRequestKind,
    pub status: WorktreeRequestStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeRequestKind {
    Create {
        project: ProjectId,
        base: String,
    },
    Remove {
        worktree: WorktreeId,
        force: bool,
    },
    Switch {
        tree: WorkingTree,
        branch: String,
        create: bool,
    },
    /// Stage everything in the working tree and commit it.
    Commit {
        tree: WorkingTree,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeRequestStatus {
    Pending,
    Failed(String),
    /// Removal stopped at uncommitted changes; the same request with `force` discards them.
    Refused(String),
    /// Finished; a creation carries the Conversation it opened.
    Done(Option<ThreadId>),
}

/// Why a removal did not happen; only a dirty tree can be overridden.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemovalFailure {
    Dirty(String),
    Other(String),
}

impl AppState {
    pub fn worktree(&self, id: WorktreeId) -> Option<&Worktree> {
        self.snapshot.worktrees.iter().find(|w| w.id == id)
    }

    pub fn thread_worktree(&self, thread: &Thread) -> Option<&Worktree> {
        thread.worktree_id.and_then(|id| self.worktree(id))
    }

    /// The checkout a thread's processes run in. A removed worktree still resolves to its
    /// old directory so reads fail honestly instead of silently falling back to the root.
    pub fn working_tree_of(&self, thread: &Thread) -> Option<(WorkingTree, PathBuf)> {
        if thread.remote.is_some() {
            return None;
        }
        if let Some(worktree) = self.thread_worktree(thread) {
            return Some((WorkingTree::Worktree(worktree.id), worktree.path.clone()));
        }
        self.snapshot
            .projects
            .iter()
            .find(|project| project.id == thread.project_id)
            .map(|project| (WorkingTree::Project(project.id), project.root.clone()))
    }

    pub fn save_worktree(&mut self, worktree: Worktree) {
        if let Some(old) = self
            .snapshot
            .worktrees
            .iter_mut()
            .find(|w| w.id == worktree.id)
        {
            *old = worktree.clone();
        } else {
            self.snapshot.worktrees.push(worktree.clone());
        }
        self.enqueue(Write::Worktree(worktree));
        self.sync_network();
    }

    /// A live thread is one that can still start work: neither it nor its project is archived.
    pub fn worktree_has_live_threads(&self, id: WorktreeId) -> bool {
        self.snapshot.threads.iter().any(|thread| {
            thread.worktree_id == Some(id)
                && !thread.archived
                && !self
                    .snapshot
                    .projects
                    .iter()
                    .any(|project| project.id == thread.project_id && project.archived)
        })
    }

    /// The branch to show for a worktree: Git's live HEAD when read, else the recorded one.
    pub fn worktree_branch_label(&self, worktree: &Worktree) -> String {
        match self.worktree_heads.get(&worktree.id) {
            Some(Some(branch)) => branch.clone(),
            Some(None) => "detached".into(),
            None => worktree.branch.clone(),
        }
    }

    /// Stores a HEAD observed by a Git read so labels follow switches made anywhere.
    pub(super) fn record_head(&mut self, tree: WorkingTree, head: Option<String>) {
        match tree {
            WorkingTree::Worktree(id) => {
                self.worktree_heads.insert(id, head);
            }
            WorkingTree::Project(id) => {
                if let Some(Ok(repository)) = self.repositories.get_mut(&id) {
                    repository.branch = head;
                }
            }
        }
    }

    fn refresh_worktree_head(&self, id: WorktreeId) {
        let Some(path) = self
            .worktree(id)
            .filter(|w| w.is_live())
            .map(|w| w.path.clone())
        else {
            return;
        };
        let sender = self.events.clone();
        smol::spawn(async move {
            let result = smol::unblock(move || {
                platform::worktree_head_branch(&path).map_err(|error| format!("{error:#}"))
            })
            .await;
            let _ = sender.send(AppEvent::WorktreeHeadRead(id, result)).await;
        })
        .detach();
    }

    pub(super) fn tree_threads(&self, tree: WorkingTree) -> Vec<ThreadId> {
        self.snapshot
            .threads
            .iter()
            .filter(|thread| self.working_tree_of(thread).map(|(key, _)| key) == Some(tree))
            .map(|thread| thread.id)
            .collect()
    }

    pub fn switch_branch_reason(&self) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is read-only: {error}"));
        }
        let Some(thread) = self.conversation_thread() else {
            return Some("Select a conversation first.".into());
        };
        if let Some(reason) = self.remote_action_reason(thread.id) {
            return Some(reason);
        }
        if thread.archived {
            return Some("Archived threads are read-only.".into());
        }
        if self.thread_worktree(thread).is_some_and(|w| !w.is_live()) {
            return Some("This conversation's worktree was removed.".into());
        }
        if !matches!(self.repositories.get(&thread.project_id), Some(Ok(_))) {
            return Some("Project root is unavailable.".into());
        }
        if self.worktree_request_pending() {
            return Some("A worktree operation is in progress.".into());
        }
        let Some((tree, _)) = self.working_tree_of(thread) else {
            return Some("Project root is unavailable.".into());
        };
        if self
            .tree_threads(tree)
            .iter()
            .any(|thread| self.active_threads.contains(thread))
        {
            return Some("Stop the active turns in this working tree first.".into());
        }
        None
    }

    /// Checks out `branch` (created from HEAD when `create`) in the selected Conversation's
    /// working tree. Git refuses dirty trees and branches checked out elsewhere; the dialog
    /// shows that refusal.
    pub fn switch_branch(&mut self, branch: &str, create: bool) -> Result<Uuid> {
        if let Some(reason) = self.switch_branch_reason() {
            anyhow::bail!(reason);
        }
        let (tree, root) = self
            .conversation_thread()
            .and_then(|thread| self.working_tree_of(thread))
            .context("Select a conversation first.")?;
        let branch = branch.trim().to_owned();
        anyhow::ensure!(!branch.is_empty(), "Choose a branch or name a new one.");
        let request = Uuid::new_v4();
        self.worktree_request = Some(WorktreeRequest {
            id: request,
            kind: WorktreeRequestKind::Switch {
                tree,
                branch: branch.clone(),
                create,
            },
            status: WorktreeRequestStatus::Pending,
        });
        let sender = self.events.clone();
        smol::spawn(async move {
            let result = smol::unblock(move || {
                platform::switch_branch(&root, &branch, create)
                    .map(|()| branch)
                    .map_err(|error| format!("{error:#}"))
            })
            .await;
            let _ = sender
                .send(AppEvent::BranchSwitched(request, tree, result))
                .await;
        })
        .detach();
        Ok(request)
    }

    pub(super) fn branch_switched(
        &mut self,
        request: Uuid,
        tree: WorkingTree,
        result: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let status = match result {
            Ok(branch) => {
                self.record_head(tree, Some(branch.clone()));
                let project = match tree {
                    WorkingTree::Worktree(id) => {
                        let worktree = self.worktree(id).cloned();
                        if let Some(mut worktree) = worktree
                            && self.storage_error.is_none()
                        {
                            // A chosen branch ends the one-time rename from the title.
                            worktree.branch = branch.clone();
                            worktree.named = true;
                            self.save_worktree(worktree);
                        }
                        self.worktree(id).map(|w| w.project_id)
                    }
                    WorkingTree::Project(id) => Some(id),
                };
                if let Some(project) = project {
                    self.refresh_repository(project);
                }
                self.refresh_project_diff(tree, true, cx);
                self.notice(NoticeLevel::Info, format!("Switched to branch {branch}"));
                WorktreeRequestStatus::Done(None)
            }
            Err(error) => WorktreeRequestStatus::Failed(error),
        };
        if let Some(pending) = self
            .worktree_request
            .as_mut()
            .filter(|pending| pending.id == request)
        {
            pending.status = status;
        }
        cx.emit(PresentationChange::WorktreeRequest(request));
        cx.emit(PresentationChange::Projects);
        for thread in self.tree_threads(tree) {
            cx.emit(PresentationChange::Thread(thread));
        }
    }

    pub(super) fn worktree_threads(&self, id: WorktreeId) -> Vec<ThreadId> {
        self.snapshot
            .threads
            .iter()
            .filter(|thread| thread.worktree_id == Some(id))
            .map(|thread| thread.id)
            .collect()
    }

    pub(super) fn worktree_request_pending(&self) -> bool {
        self.worktree_request
            .as_ref()
            .is_some_and(|request| request.status == WorktreeRequestStatus::Pending)
    }

    pub fn create_worktree_reason(&self) -> Option<String> {
        if let Some(reason) = self
            .selected_thread
            .and_then(|id| self.remote_action_reason(id))
        {
            return Some(reason);
        }
        if let Some(reason) = self.mutable_project_reason() {
            return Some(reason);
        }
        if self.worktree_request_pending() {
            return Some("A worktree operation is in progress.".into());
        }
        match self
            .project()
            .map(|project| self.repositories.get(&project.id))
        {
            Some(Some(Ok(_))) => None,
            Some(Some(Err(error))) => Some(format!("Project root is unavailable: {error}")),
            _ => Some("The project working tree has not been inspected.".into()),
        }
    }

    /// Starts a worktree of `project_id` from local branch `base` and, once Git has created
    /// it, a Conversation inside it. Nothing is persisted until the directory exists.
    pub fn create_worktree_conversation(
        &mut self,
        project_id: ProjectId,
        base: &str,
    ) -> Result<Uuid> {
        if let Some(reason) = self.create_worktree_reason() {
            anyhow::bail!(reason);
        }
        let project = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .cloned()
            .context("Project is unavailable")?;
        anyhow::ensure!(
            !project.archived,
            "This project is archived. History is read-only."
        );
        let branches: HashSet<String> = self
            .repositories
            .get(&project_id)
            .and_then(|repository| repository.as_ref().ok())
            .context("The project working tree has not been inspected.")?
            .branches
            .iter()
            .cloned()
            .collect();
        let base = base.trim().to_owned();
        anyhow::ensure!(
            branches.contains(&base),
            "Choose an existing local branch as the base."
        );
        let taken: HashSet<String> = self
            .snapshot
            .worktrees
            .iter()
            .filter(|worktree| worktree.project_id == project_id)
            .map(|worktree| worktree.codename.clone())
            .collect();
        let request = Uuid::new_v4();
        let id = Uuid::new_v4();
        let created_by = Uuid::new_v4();
        let data_dir = self.data_dir.clone();
        let root = project.root.clone();
        self.worktree_request = Some(WorktreeRequest {
            id: request,
            kind: WorktreeRequestKind::Create {
                project: project_id,
                base: base.clone(),
            },
            status: WorktreeRequestStatus::Pending,
        });
        let sender = self.events.clone();
        smol::spawn(async move {
            let result = smol::unblock(move || -> Result<Worktree> {
                let codename = naming::unique_codename(id, |name| {
                    taken.contains(name) || naming::worktree_dir(&data_dir, &root, name).exists()
                });
                let path = naming::worktree_dir(&data_dir, &root, &codename);
                let branch = naming::unique_branch(&naming::initial_branch(&codename), |name| {
                    branches.contains(name) || platform::branch_exists(&root, name).unwrap_or(true)
                })
                .context("Every branch name for this worktree is taken")?;
                let path = platform::create_worktree(&root, &path, &branch, &base)?;
                Ok(Worktree {
                    id,
                    project_id,
                    path,
                    codename,
                    branch,
                    base,
                    named: false,
                    created_by,
                    created_at: platform::now(),
                    removed_at: None,
                })
            })
            .await
            .map_err(|error| format!("{error:#}"));
            let _ = sender
                .send(AppEvent::WorktreeCreated(request, result.map(Box::new)))
                .await;
        })
        .detach();
        Ok(request)
    }

    pub(super) fn worktree_created(
        &mut self,
        request: Uuid,
        result: Result<Box<Worktree>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(WorktreeRequestKind::Create { project, .. }) = self
            .worktree_request
            .as_ref()
            .filter(|pending| pending.id == request)
            .map(|pending| pending.kind.clone())
        else {
            return;
        };
        let status = match result {
            Err(error) => WorktreeRequestStatus::Failed(error),
            Ok(worktree) => {
                let worktree = *worktree;
                let project = self
                    .snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == project)
                    .cloned();
                let blocked = if self.shutting_down {
                    Some("Application is shutting down".to_owned())
                } else if let Some(error) = &self.storage_error {
                    Some(format!("Storage is not writable: {error}"))
                } else if project.is_none() {
                    Some("Project is unavailable".to_owned())
                } else {
                    None
                };
                match (blocked, project) {
                    (Some(reason), project) => {
                        // The row was never written: take the directory back so nothing is
                        // left that the app does not know about.
                        if let Some(project) = project {
                            let root = project.root;
                            let path = worktree.path.clone();
                            let branch = worktree.branch.clone();
                            smol::unblock(move || {
                                let _ = platform::remove_worktree(&root, &path, false);
                                let _ = platform::delete_branch(&root, &branch);
                            })
                            .detach();
                        }
                        WorktreeRequestStatus::Failed(reason)
                    }
                    (None, Some(project)) => {
                        let thread_id = worktree.created_by;
                        let notice = format!(
                            "Worktree {} on branch {}",
                            worktree.codename, worktree.branch
                        );
                        self.save_worktree(worktree.clone());
                        self.worktree_heads
                            .insert(worktree.id, Some(worktree.branch.clone()));
                        let thread =
                            self.build_conversation(&project, None, thread_id, Some(worktree.id));
                        self.save_thread(thread);
                        match self.register_thread_membership(
                            thread_id,
                            project.id,
                            self.active_space,
                        ) {
                            Ok(_) => self.select_thread(thread_id),
                            Err(error) => self.notice(NoticeLevel::Error, format!("{error:#}")),
                        }
                        self.notice(NoticeLevel::Info, notice);
                        WorktreeRequestStatus::Done(Some(thread_id))
                    }
                    (None, None) => WorktreeRequestStatus::Failed("Project is unavailable".into()),
                }
            }
        };
        if let Some(pending) = self
            .worktree_request
            .as_mut()
            .filter(|pending| pending.id == request)
        {
            pending.status = status;
        }
        cx.emit(PresentationChange::WorktreeRequest(request));
        cx.emit(PresentationChange::Projects);
        cx.emit(PresentationChange::Selection);
    }

    /// The one rename a worktree gets: from its codename branch to the generated title, only
    /// while the creating Conversation's title is the first automatic one.
    pub(super) fn rename_worktree_branch(&mut self, thread_id: ThreadId, title: &str) {
        let Some(worktree) = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .and_then(|thread| self.thread_worktree(thread))
            .filter(|worktree| {
                worktree.is_live() && !worktree.named && worktree.created_by == thread_id
            })
            .cloned()
        else {
            return;
        };
        if self.shutting_down
            || self.storage_error.is_some()
            || !self.renaming_worktrees.insert(worktree.id)
        {
            return;
        }
        let candidate = naming::titled_branch(title).filter(|name| name != &worktree.branch);
        let Some(root) = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == worktree.project_id)
            .map(|project| project.root.clone())
        else {
            self.renaming_worktrees.remove(&worktree.id);
            return;
        };
        let Some(candidate) = candidate else {
            // Nothing usable in the title: the codename branch is the name.
            self.renaming_worktrees.remove(&worktree.id);
            let mut settled = worktree;
            settled.named = true;
            self.save_worktree(settled);
            return;
        };
        let sender = self.events.clone();
        let id = worktree.id;
        let old = worktree.branch.clone();
        let path = worktree.path.clone();
        smol::spawn(async move {
            let result = smol::unblock(move || -> Result<String> {
                let new = naming::unique_branch(&candidate, |name| {
                    platform::branch_exists(&root, name).unwrap_or(true)
                })
                .context("Every branch name for this title is taken")?;
                platform::rename_branch(&path, &old, &new)?;
                Ok(new)
            })
            .await
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(AppEvent::WorktreeRenamed(id, result)).await;
        })
        .detach();
    }

    pub(super) fn worktree_renamed(
        &mut self,
        id: WorktreeId,
        result: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        self.renaming_worktrees.remove(&id);
        let Some(mut worktree) = self.worktree(id).cloned() else {
            return;
        };
        match result {
            Ok(branch) => {
                self.worktree_heads.insert(id, Some(branch.clone()));
                worktree.branch = branch
            }
            Err(error) => self.notice(
                NoticeLevel::Warning,
                format!(
                    "Worktree {} keeps branch {}: {error}",
                    worktree.codename, worktree.branch
                ),
            ),
        }
        worktree.named = true;
        if self.storage_error.is_none() {
            self.save_worktree(worktree.clone());
        }
        self.refresh_repository(worktree.project_id);
        cx.emit(PresentationChange::Projects);
        for thread in self.worktree_threads(id) {
            cx.emit(PresentationChange::Thread(thread));
        }
    }

    /// Re-reads one project's branches and state, for after a branch rename or removal.
    pub(super) fn refresh_repository(&self, id: ProjectId) {
        let Some(root) = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.root.clone())
        else {
            return;
        };
        let sender = self.events.clone();
        smol::spawn(async move {
            let result = smol::unblock(move || {
                platform::inspect_repository(&root).map_err(|error| format!("{error:#}"))
            })
            .await;
            let _ = sender.send(AppEvent::RepositoryChecked(id, result)).await;
        })
        .detach();
    }

    /// Archives a thread; when it was the last live thread of a worktree, that worktree is
    /// inspected for automatic removal. Archiving says the thread no longer matters: its tab
    /// closes without the thread being selected first (a neighbouring tab takes over only when
    /// it was the selected one), and it leaves Projects unless archived threads are shown.
    pub fn archive_thread(&mut self, id: ThreadId) -> Result<()> {
        self.archive_thread_because(id, None)
    }

    /// `archive_thread`, with the `cause` the notice names when DROSTJE. archived on its own.
    pub(super) fn archive_thread_because(
        &mut self,
        id: ThreadId,
        cause: Option<&str>,
    ) -> Result<()> {
        if let Some(reason) = self.archive_thread_reason_for(id) {
            anyhow::bail!(reason);
        }
        // Closing first commits the draft the tab held.
        self.close_thread(id);
        let mut thread = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .cloned()
            .context("This thread is no longer available.")?;
        thread.archived = true;
        thread.updated_at = platform::now();
        let worktree = thread.worktree_id;
        let title = thread.title.clone();
        // An untouched thread was never more than a placeholder; it leaves silently.
        let untouched = self.is_untouched(&thread);
        self.save_thread(thread);
        if !untouched && !self.show_archived {
            let because = cause.map(|cause| format!(": {cause}")).unwrap_or_default();
            self.notice(
                NoticeLevel::Info,
                format!("Archived {title}{because}. Show archived threads to unarchive it."),
            );
        }
        if let Some(worktree) = worktree
            && !self.worktree_has_live_threads(worktree)
        {
            self.inspect_worktree_for_removal(worktree);
        }
        Ok(())
    }

    /// Every live worktree without live threads is a removal candidate; at startup this
    /// finishes what a previous session left behind.
    pub(super) fn reconcile_worktrees(&mut self) {
        let live: Vec<_> = self
            .snapshot
            .worktrees
            .iter()
            .filter(|worktree| worktree.is_live())
            .map(|worktree| worktree.id)
            .collect();
        for id in live {
            self.refresh_worktree_head(id);
        }
        let candidates: Vec<_> = self
            .snapshot
            .worktrees
            .iter()
            .filter(|worktree| worktree.is_live() && !self.worktree_has_live_threads(worktree.id))
            .map(|worktree| worktree.id)
            .collect();
        for id in candidates {
            self.inspect_worktree_for_removal(id);
        }
    }

    fn inspect_worktree_for_removal(&mut self, id: WorktreeId) {
        if self.shutting_down || self.storage_error.is_some() {
            return;
        }
        let Some(worktree) = self.worktree(id).filter(|w| w.is_live()).cloned() else {
            return;
        };
        let Some(root) = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == worktree.project_id)
            .map(|project| project.root.clone())
        else {
            return;
        };
        if !self.inspecting_worktrees.insert(id) {
            return;
        }
        let sender = self.events.clone();
        smol::spawn(async move {
            let result = smol::unblock(move || {
                platform::inspect_worktree(&root, &worktree.path, &worktree.branch, &worktree.base)
                    .map_err(|error| format!("{error:#}"))
            })
            .await;
            let _ = sender.send(AppEvent::WorktreeInspected(id, result)).await;
        })
        .detach();
    }

    pub(super) fn worktree_inspected(
        &mut self,
        id: WorktreeId,
        result: Result<platform::WorktreeStatus, String>,
        cx: &mut Context<Self>,
    ) {
        self.inspecting_worktrees.remove(&id);
        if self.shutting_down || self.storage_error.is_some() {
            return;
        }
        let Some(worktree) = self.worktree(id).filter(|w| w.is_live()).cloned() else {
            return;
        };
        // A thread may have been created on it while Git was reading.
        if self.worktree_has_live_threads(id) || self.worktree_request_pending() {
            return;
        }
        match result {
            Err(error) => self.notice(
                NoticeLevel::Diagnostic,
                format!("Worktree {} kept: {error}", worktree.codename),
            ),
            Ok(status) => match naming::removal_decision(&status, &worktree.branch) {
                Removal::Remove | Removal::Forget => {
                    self.remove_worktree_files(None, worktree, false)
                }
                Removal::Keep(reason) => self.notice(
                    NoticeLevel::Diagnostic,
                    format!("Worktree {} kept: {reason}", worktree.codename),
                ),
            },
        }
        cx.notify();
    }

    pub fn remove_worktree_reason(&self) -> Option<String> {
        let Some(thread) = self.conversation_thread() else {
            return Some("Select a conversation in a worktree first.".into());
        };
        match thread.worktree_id {
            Some(id) => self.remove_worktree_reason_for(id),
            None => Some("This conversation runs in the project root.".into()),
        }
    }

    pub fn remove_worktree_reason_for(&self, id: WorktreeId) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is read-only: {error}"));
        }
        let Some(worktree) = self.worktree(id) else {
            return Some("This worktree is no longer available.".into());
        };
        if !worktree.is_live() {
            return Some("This worktree was already removed.".into());
        }
        if self.worktree_request_pending() {
            return Some("A worktree operation is in progress.".into());
        }
        if self
            .worktree_threads(id)
            .iter()
            .any(|thread| self.active_threads.contains(thread))
        {
            return Some("Stop the active turns in this worktree first.".into());
        }
        None
    }

    /// Removes a worktree directory on request. Its Conversations' harness sessions are
    /// closed first; they stay in history and can no longer resume.
    pub fn remove_worktree(&mut self, id: WorktreeId, force: bool) -> Result<Uuid> {
        if let Some(reason) = self.remove_worktree_reason_for(id) {
            anyhow::bail!(reason);
        }
        let worktree = self
            .worktree(id)
            .cloned()
            .context("Worktree is unavailable")?;
        for thread in self.worktree_threads(id) {
            if let Some(session) = self.conversations.remove(&thread)
                && let Some(commands) = session.commands
            {
                let _ = commands.try_send(ConversationCommand::Close);
            }
            self.close_artifacts(thread);
        }
        let request = Uuid::new_v4();
        self.worktree_request = Some(WorktreeRequest {
            id: request,
            kind: WorktreeRequestKind::Remove {
                worktree: id,
                force,
            },
            status: WorktreeRequestStatus::Pending,
        });
        self.remove_worktree_files(Some(request), worktree, force);
        Ok(request)
    }

    /// Removes the directory (or prunes a registration whose directory is already gone) and
    /// deletes the branch only when it is merged into the base. `Ok(true)` means the branch
    /// went too.
    fn remove_worktree_files(&mut self, request: Option<Uuid>, worktree: Worktree, force: bool) {
        let Some(root) = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == worktree.project_id)
            .map(|project| project.root.clone())
        else {
            return;
        };
        let sender = self.events.clone();
        let id = worktree.id;
        smol::spawn(async move {
            let result = smol::unblock(move || -> Result<bool, RemovalFailure> {
                let other = |error: anyhow::Error| RemovalFailure::Other(format!("{error:#}"));
                let status = platform::inspect_worktree(
                    &root,
                    &worktree.path,
                    &worktree.branch,
                    &worktree.base,
                )
                .map_err(other)?;
                if status.exists {
                    if !status.clean && !force {
                        return Err(RemovalFailure::Dirty(format!(
                            "Worktree {} has uncommitted changes. Confirm again to discard them.",
                            worktree.codename
                        )));
                    }
                    platform::remove_worktree(&root, &worktree.path, force).map_err(other)?;
                } else {
                    platform::prune_worktrees(&root).map_err(other)?;
                }
                Ok(status.merged && platform::delete_branch(&root, &worktree.branch).is_ok())
            })
            .await;
            let _ = sender
                .send(AppEvent::WorktreeRemoved(request, id, result))
                .await;
        })
        .detach();
    }

    pub(super) fn worktree_removed(
        &mut self,
        request: Option<Uuid>,
        id: WorktreeId,
        result: Result<bool, RemovalFailure>,
        cx: &mut Context<Self>,
    ) {
        let Some(mut worktree) = self.worktree(id).cloned() else {
            return;
        };
        let status = match result {
            Ok(branch_deleted) => {
                worktree.removed_at = Some(platform::now());
                if self.storage_error.is_none() {
                    self.save_worktree(worktree.clone());
                }
                self.notice(
                    NoticeLevel::Info,
                    format!(
                        "Removed worktree {}; branch {} {}",
                        worktree.codename,
                        worktree.branch,
                        if branch_deleted {
                            "deleted"
                        } else {
                            "kept (not merged into its base)"
                        }
                    ),
                );
                self.refresh_repository(worktree.project_id);
                WorktreeRequestStatus::Done(None)
            }
            Err(failure) => {
                let (refused, error) = match failure {
                    RemovalFailure::Dirty(error) => (true, error),
                    RemovalFailure::Other(error) => (false, error),
                };
                if request.is_none() {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("Could not remove worktree {}: {error}", worktree.codename),
                    );
                }
                if refused {
                    WorktreeRequestStatus::Refused(error)
                } else {
                    WorktreeRequestStatus::Failed(error)
                }
            }
        };
        if let Some(request) = request {
            if let Some(pending) = self
                .worktree_request
                .as_mut()
                .filter(|pending| pending.id == request)
            {
                pending.status = status;
            }
            cx.emit(PresentationChange::WorktreeRequest(request));
        }
        cx.emit(PresentationChange::Projects);
        for thread in self.worktree_threads(id) {
            cx.emit(PresentationChange::Thread(thread));
        }
    }
}
