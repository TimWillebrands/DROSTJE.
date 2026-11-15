use super::*;
use crate::store::Store;
use async_channel::{Receiver, Sender};

pub(super) enum Write {
    Project(Project),
    Thread(
        Box<Thread>,
        Option<async_channel::Sender<Result<(), String>>>,
    ),
    Setting(String, Value),
    Worktree(Worktree),
    Flush(async_channel::Sender<Result<(), String>>),
}

impl AppState {
    pub(super) fn enqueue(&mut self, request: Write) -> bool {
        if self.storage_error.is_some() {
            return false;
        }
        match self.writes.try_send(request) {
            Ok(()) => {
                self.pending_writes += 1;
                true
            }
            Err(error) => {
                self.storage_error = Some(error.to_string());
                self.pause_all_network_delivery();
                self.sync_network();
                false
            }
        }
    }

    pub fn set_setting(&mut self, key: &str, value: Value) {
        self.snapshot.settings.insert(key.into(), value.clone());
        self.enqueue(Write::Setting(key.into(), value));
    }

    pub fn save_project(&mut self, project: Project) {
        self.save_project_in_space(project, self.active_space);
    }

    pub(super) fn save_project_in_space(&mut self, project: Project, space: SpaceId) -> SpaceId {
        let id = project.id;
        let is_new = !self.snapshot.projects.iter().any(|old| old.id == id);
        if is_new && self.space_mutation_reason().is_some() {
            return space;
        }
        if let Some(old) = self
            .snapshot
            .projects
            .iter_mut()
            .find(|p| p.id == project.id)
        {
            *old = project.clone();
        } else {
            self.snapshot.projects.push(project.clone());
        }
        self.enqueue(Write::Project(project));
        self.sync_network();
        if is_new {
            self.register_project_membership(id, space).0
        } else {
            space
        }
    }

    /// Replaces the snapshot's copy of `thread`, or adds it when it is new. Returns whether
    /// anything the Space network reads from it changed (always for a new thread).
    fn put_thread(&mut self, thread: Thread) -> bool {
        if let Some(old) = self.snapshot.threads.iter_mut().find(|t| t.id == thread.id) {
            let changed = network::peer_context(old) != network::peer_context(&thread);
            *old = thread;
            changed
        } else {
            self.snapshot.threads.push(thread);
            true
        }
    }

    pub fn save_thread(&mut self, thread: Thread) {
        // A read-only store refuses every write, so the snapshot then keeps the only copy.
        let write = if self.storage_error.is_none() {
            self.edited_threads.remove(&thread.id);
            Some(Box::new(thread.clone()))
        } else {
            None
        };
        let network_changed = self.put_thread(thread);
        if let Some(write) = write {
            self.enqueue(Write::Thread(write, None));
        }
        if network_changed {
            self.sync_network();
        }
    }

    /// Typing changes memory only: no timer, write request, or save notification.
    pub fn edit_thread(&mut self, thread: Thread) {
        self.edited_threads.insert(thread.id);
        self.put_thread(thread);
    }

    pub fn commit_thread_edits(&mut self, id: ThreadId) -> bool {
        if self.storage_error.is_some() || !self.edited_threads.remove(&id) {
            return false;
        }
        let Some(thread) = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .cloned()
        else {
            return false;
        };
        let queued = self.enqueue(Write::Thread(Box::new(thread), None));
        if !queued {
            self.edited_threads.insert(id);
        }
        queued
    }

    pub(super) fn commit_all_thread_edits(&mut self) {
        let ids: Vec<_> = self.edited_threads.iter().copied().collect();
        for id in ids {
            self.commit_thread_edits(id);
        }
    }

    /// A persistence acknowledgement in the same queue as draft edits and layout changes.
    /// Callers must await success before sending any corresponding external side effect.
    pub fn persist_thread(
        &mut self,
        thread: Thread,
    ) -> async_channel::Receiver<Result<(), String>> {
        let (reply, result) = async_channel::bounded(1);
        if let Some(error) = &self.storage_error {
            let _ = reply.try_send(Err(error.clone()));
            self.put_thread(thread);
        } else {
            let id = thread.id;
            let write = Box::new(thread.clone());
            self.put_thread(thread);
            self.edited_threads.remove(&id);
            self.commit_all_thread_edits();
            self.enqueue(Write::Thread(write, Some(reply)));
        }
        result
    }
}

/// The queue contains committed edits only. Flush acknowledges all preceding
/// writes; typing never reaches this worker.
pub(super) async fn run(store: Store, requests: Receiver<Write>, completed: Sender<AppEvent>) {
    while let Ok(request) = requests.recv().await {
        let result = match request {
            Write::Project(project) => store.save_project(project).await,
            Write::Thread(thread, reply) => {
                let result = store
                    .save_thread(*thread)
                    .await
                    .map_err(|error| format!("{error:#}"));
                if let Some(reply) = reply {
                    let _ = reply.send(result.clone()).await;
                }
                result.map_err(anyhow::Error::msg)
            }
            Write::Setting(key, value) => store.set_setting(key, value).await,
            Write::Worktree(worktree) => store.save_worktree(worktree).await,
            Write::Flush(reply) => {
                let result = store.flush().await.map_err(|error| format!("{error:#}"));
                let _ = reply.send(result).await;
                continue;
            }
        }
        .map_err(|error| format!("{error:#}"));
        let _ = completed.send(AppEvent::WriteFinished(result)).await;
    }
}
