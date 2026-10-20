//! Committing a Conversation's working tree, and the local model's subject for that commit.
//!
//! The commit is a worktree request, so it excludes branch switches and worktree changes while
//! it runs and reuses their dialog lifecycle. The subject is written by the same local
//! generator as Conversation titles. Git and the model run off-thread; results carry their
//! request id and stale ones are dropped.

use super::*;

/// A commit subject written for one working tree; the Commit dialog that asked observes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitSubjectRequest {
    pub id: Uuid,
    pub tree: WorkingTree,
    pub status: CommitSubjectStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitSubjectStatus {
    Pending,
    Written(String),
    /// No local generator answered: no llama.cpp executable and no Ollama server.
    Unavailable,
    Failed(String),
}

/// Diff text handed to the model: a file summary first, so a truncated patch still names
/// every change, then the patch itself.
pub fn commit_prompt(diff: &platform::RepositoryDiff) -> String {
    use crate::diff::{FileChange, RowKind};
    let mut prompt = format!(
        "{} files changed, +{} -{}\n",
        diff.changed_files, diff.additions, diff.deletions
    );
    for row in &diff.rows.rows {
        if let RowKind::File(header) = &row.kind {
            let change = match &header.change {
                FileChange::Added => "added",
                FileChange::Deleted => "deleted",
                FileChange::Modified => "modified",
                FileChange::Renamed { .. } => "renamed",
            };
            prompt.push_str(&format!(
                "{change} {} (+{} -{})\n",
                row.text, header.additions, header.deletions
            ));
        }
    }
    prompt.push('\n');
    prompt.push_str(&diff.patch);
    prompt
}

impl AppState {
    pub fn commit_reason(&self) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        let Some(thread) = self.conversation_thread() else {
            return Some("Select a conversation first.".into());
        };
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
        let clean = self
            .working_tree_diff(tree)
            .and_then(|diff| diff.result.as_ref())
            .is_some_and(|result| result.as_ref().is_ok_and(|diff| diff.changed_files == 0));
        if clean {
            return Some("No changes to commit.".into());
        }
        None
    }

    /// Stages every change in `thread`'s working tree and commits it with `message`. The
    /// dialog stays open until the request settles and shows Git's refusal, hooks included.
    pub fn commit_changes(&mut self, thread: ThreadId, message: &str) -> Result<Uuid> {
        if let Some(reason) = self.commit_reason() {
            anyhow::bail!(reason);
        }
        let (tree, root) = self
            .conversation_thread()
            .filter(|selected| selected.id == thread)
            .and_then(|thread| self.working_tree_of(thread))
            .context("Thread selection changed; open Commit changes again.")?;
        let message = message.trim().to_owned();
        anyhow::ensure!(!message.is_empty(), "Write a commit message.");
        let request = Uuid::new_v4();
        self.worktree_request = Some(WorktreeRequest {
            id: request,
            kind: WorktreeRequestKind::Commit { tree },
            status: WorktreeRequestStatus::Pending,
        });
        let sender = self.events.clone();
        smol::spawn(async move {
            let result = smol::unblock(move || {
                platform::commit_all(&root, &message)
                    .map(|hash| {
                        let subject = message.lines().next().unwrap_or_default().to_owned();
                        (hash, subject)
                    })
                    .map_err(|error| format!("{error:#}"))
            })
            .await;
            let _ = sender
                .send(AppEvent::Committed(request, tree, result))
                .await;
        })
        .detach();
        Ok(request)
    }

    pub(super) fn committed(
        &mut self,
        request: Uuid,
        tree: WorkingTree,
        result: Result<(String, String), String>,
        cx: &mut Context<Self>,
    ) {
        let status = match result {
            Ok((hash, subject)) => {
                let project = match tree {
                    WorkingTree::Worktree(id) => self.worktree(id).map(|w| w.project_id),
                    WorkingTree::Project(id) => Some(id),
                };
                if let Some(project) = project {
                    self.refresh_repository(project);
                }
                self.refresh_project_diff(tree, true, cx);
                self.notice(NoticeLevel::Info, format!("Committed {hash}: {subject}"));
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

    /// Asks the local generator for a subject describing `thread`'s uncommitted changes, read
    /// fresh from Git. A newer request replaces an older one, whose result is then dropped.
    pub fn write_commit_subject(
        &mut self,
        thread: ThreadId,
        cx: &mut Context<Self>,
    ) -> Result<Uuid> {
        anyhow::ensure!(!self.shutting_down, "Application is shutting down");
        let (tree, root) = self
            .snapshot
            .threads
            .iter()
            .find(|candidate| candidate.id == thread)
            .and_then(|thread| self.working_tree_of(thread))
            .context("Project root is unavailable.")?;
        let request = Uuid::new_v4();
        self.commit_subject = Some(CommitSubjectRequest {
            id: request,
            tree,
            status: CommitSubjectStatus::Pending,
        });
        cx.emit(PresentationChange::CommitSubject(request));
        let generator = self.text_generator();
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || {
                let diff = platform::working_tree_diff(&root)?;
                anyhow::ensure!(
                    diff.changed_files > 0,
                    "Nothing to commit; the working tree is clean"
                );
                generator.commit_subject(&commit_prompt(&diff))
            })
            .await;
            let _ = this.update(cx, |state, cx| {
                let Some(pending) = state
                    .commit_subject
                    .as_mut()
                    .filter(|pending| pending.id == request)
                else {
                    return;
                };
                pending.status = match result {
                    Ok(Some(subject)) => CommitSubjectStatus::Written(subject),
                    Ok(None) => CommitSubjectStatus::Unavailable,
                    Err(error) => CommitSubjectStatus::Failed(format!("{error:#}")),
                };
                cx.emit(PresentationChange::CommitSubject(request));
            });
        })
        .detach();
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::commit_prompt;
    use crate::{diff, platform::RepositoryDiff};
    use std::sync::Arc;

    #[test]
    fn commit_prompt_lists_files_before_the_patch() {
        let patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/n.txt b/n.txt\nnew file mode 100644\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,1 @@\n+hello";
        let diff = RepositoryDiff {
            patch: patch.into(),
            changed_files: 2,
            additions: 2,
            deletions: 1,
            rows: Arc::new(diff::parse(patch)),
            ..RepositoryDiff::default()
        };
        let prompt = commit_prompt(&diff);
        assert!(prompt.starts_with(
            "2 files changed, +2 -1\nmodified a.rs (+1 -1)\nadded n.txt (+1 -0)\n\ndiff --git"
        ));
        assert!(prompt.ends_with("+hello"));
    }
}
