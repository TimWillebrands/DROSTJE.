use super::{AppEvent, Write};
use crate::store::Store;
use async_channel::{Receiver, Sender};

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
