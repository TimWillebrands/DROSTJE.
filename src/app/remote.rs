use super::*;
use crate::remote::RemoteHostConfig;

const HOSTS_SETTING: &str = "remote_hosts";

pub struct RemoteFileView {
    pub thread: ThreadId,
    pub path: String,
    pub loading: bool,
    pub error: Option<String>,
    pub content: Option<String>,
    pub truncated: bool,
    generation: Uuid,
}

impl AppState {
    pub fn remote_hosts(&self) -> Vec<RemoteHostConfig> {
        self.snapshot
            .settings
            .get(HOSTS_SETTING)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    pub fn save_remote_host(&mut self, config: RemoteHostConfig) -> Result<()> {
        anyhow::ensure!(!self.shutting_down, "Application is shutting down");
        anyhow::ensure!(self.storage_error.is_none(), "Storage is not writable");
        config.validate()?;
        let mut hosts = self.remote_hosts();
        if let Some(old) = hosts.iter_mut().find(|host| host.id == config.id) {
            if old.relay_url != config.relay_url
                && self.snapshot.threads.iter().any(|thread| {
                    thread
                        .remote
                        .as_ref()
                        .is_some_and(|remote| remote.host == config.id)
                })
            {
                anyhow::bail!(
                    "This Host is used by a Conversation; add a new Host to change its relay identity"
                );
            }
            *old = config;
        } else {
            hosts.push(config);
        }
        self.set_setting(HOSTS_SETTING, serde_json::to_value(hosts)?);
        Ok(())
    }

    pub fn remove_remote_host_reason(&self, id: &str) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if self.storage_error.is_some() {
            return Some("Storage is not writable".into());
        }
        if self.snapshot.threads.iter().any(|thread| {
            thread
                .remote
                .as_ref()
                .is_some_and(|remote| remote.host == id)
        }) {
            return Some(
                "This Host is still referenced by a Conversation, including archived history"
                    .into(),
            );
        }
        None
    }

    pub fn remove_remote_host(&mut self, id: &str) -> Result<()> {
        if let Some(reason) = self.remove_remote_host_reason(id) {
            anyhow::bail!(reason);
        }
        let mut hosts = self.remote_hosts();
        hosts.retain(|host| host.id != id);
        self.set_setting(HOSTS_SETTING, serde_json::to_value(hosts)?);
        Ok(())
    }

    pub fn thread_host_selection_reason(&self, id: ThreadId) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is not writable: {error}"));
        }
        let Some(thread) = self.snapshot.threads.iter().find(|thread| thread.id == id) else {
            return Some("Select a Conversation first".into());
        };
        if thread.kind != ThreadKind::Conversation {
            return Some("Factory execution is always local".into());
        }
        if thread.archived
            || self
                .snapshot
                .projects
                .iter()
                .any(|project| project.id == thread.project_id && project.archived)
        {
            return Some("Archived history is read-only".into());
        }
        if thread.worktree_id.is_some() {
            return Some("Desktop worktree Conversations remain local".into());
        }
        if thread.execution_started
            || thread.submission.is_some()
            || !thread.queued.is_empty()
            || self.active_threads.contains(&id)
            || self.conversations.get(&id).is_some_and(|session| {
                session.preparing
                    || session
                        .messages
                        .iter()
                        .any(|message| matches!(message.role.as_str(), "user" | "assistant"))
            })
        {
            return Some("The Host is fixed once a Conversation has started; start a new Conversation instead".into());
        }
        None
    }

    pub fn set_thread_host(&mut self, id: ThreadId, host: Option<&str>) -> Result<()> {
        if let Some(reason) = self.thread_host_selection_reason(id) {
            anyhow::bail!(reason);
        }
        let mut thread = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .cloned()
            .context("Conversation no longer exists")?;
        let location = match host {
            None => None,
            Some(id) => {
                let config = self
                    .remote_hosts()
                    .into_iter()
                    .find(|host| host.id == id)
                    .context("Host is not configured")?;
                config.validate()?;
                let project = config
                    .projects
                    .get(&thread.project_id.to_string())
                    .context("Map this Project to a mounted Host project key in Settings first")?
                    .clone();
                Some(RemoteLocation {
                    host: config.id,
                    project,
                })
            }
        };
        if thread.remote == location {
            return Ok(());
        }
        self.pause_network_delivery(id);
        self.invalidate_remote_diff(id);
        if self
            .remote_file
            .as_ref()
            .is_some_and(|view| view.thread == id)
        {
            self.remote_file = None;
        }
        if let Some(old) = self.conversations.remove(&id)
            && let Some(commands) = old.commands
        {
            let _ = commands.try_send(ConversationCommand::Close);
        }
        self.close_artifacts(id);
        thread.remote = location;
        thread.session_ref = None;
        self.save_thread(thread);
        self.ensure_conversation(id);
        Ok(())
    }

    pub fn thread_host_label(&self, id: ThreadId) -> String {
        let Some(location) = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .and_then(|thread| thread.remote.as_ref())
        else {
            return "Local".into();
        };
        self.remote_hosts()
            .into_iter()
            .find(|host| host.id == location.host)
            .map(|host| host.name)
            .unwrap_or_else(|| location.host.clone())
    }

    pub fn thread_host_status(&self, id: ThreadId) -> String {
        let Some(location) = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .and_then(|thread| thread.remote.as_ref())
        else {
            return "Local execution".into();
        };
        let status = self
            .conversations
            .get(&id)
            .and_then(|session| session.error.clone())
            .unwrap_or_else(|| self.remote_client.status(&location.host));
        format!(
            "{} · {} · {status}",
            self.thread_host_label(id),
            location.project
        )
    }

    pub fn remote_action_reason(&self, id: ThreadId) -> Option<String> {
        self.snapshot.threads.iter().find(|thread| thread.id == id).and_then(|thread| thread.remote.as_ref()).map(|_| format!("This Conversation runs on {}; desktop filesystem and Git mutations are unavailable", self.thread_host_label(id)))
    }

    pub fn remote_launch_settings_reason(&self, id: ThreadId) -> Option<String> {
        self.snapshot.threads.iter().find(|thread| thread.id == id)
            .filter(|thread| thread.remote.is_some() && thread.execution_started)
            .map(|_| "Remote launch settings are fixed after the first turn; start a new Conversation to change them".into())
    }

    pub fn request_remote_file(&mut self, thread: ThreadId, path: String, cx: &mut Context<Self>) {
        let Some(location) = self
            .snapshot
            .threads
            .iter()
            .find(|candidate| candidate.id == thread)
            .and_then(|thread| thread.remote.clone())
        else {
            return;
        };
        let generation = Uuid::new_v4();
        self.remote_file = Some(RemoteFileView {
            thread,
            path: path.clone(),
            loading: true,
            error: None,
            content: None,
            truncated: false,
            generation,
        });
        let client = self.remote_client.clone();
        cx.spawn(async move |this, cx| {
            let result = client
                .read_file(&location.host, thread, path)
                .await
                .and_then(|value| {
                    let content = value
                        .get("content")
                        .and_then(Value::as_str)
                        .context("Host did not return UTF-8 file content")?
                        .to_owned();
                    Ok((
                        content,
                        value
                            .get("truncated")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    ))
                });
            let _ = this.update(cx, |state, cx| {
                if let Some(view) = state
                    .remote_file
                    .as_mut()
                    .filter(|view| view.generation == generation)
                {
                    view.loading = false;
                    match result {
                        Ok((content, truncated)) => {
                            view.content = Some(content);
                            view.truncated = truncated;
                        }
                        Err(error) => {
                            view.error = Some(format!("Remote file unavailable: {error:#}"))
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn close_remote_file(&mut self) {
        self.remote_file = None;
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod tests {
    use crate::app::{AppState, Bootstrap};
    use crate::model::{Project, ProjectDefaults, RemoteLocation};
    use crate::store::Store;
    use gpui_kit::AppContext as _;
    use std::{collections::BTreeMap, path::PathBuf};
    use uuid::Uuid;

    #[gpui_kit::test]
    fn remote_threads_never_resolve_the_desktop_checkout(cx: &mut gpui_kit::TestAppContext) {
        cx.dispatcher.allow_parking();
        struct Directory(PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory =
            Directory(std::env::temp_dir().join(format!("drost-remote-app-{}", Uuid::new_v4())));
        let store = Store::open(directory.0.join("data")).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Remote project".into(),
            root: directory.0.join("must-not-be-created"),
            archived: false,
            defaults: ProjectDefaults::default(),
        };
        smol::block_on(store.save_project(project.clone())).unwrap();
        let snapshot = smol::block_on(store.snapshot()).unwrap();
        let state = cx.new(|cx| {
            AppState::new(
                Bootstrap {
                    data_dir: directory.0.clone(),
                    store,
                    snapshot,
                    repositories: BTreeMap::new(),
                    diagnostics: BTreeMap::new(),
                    browser_tools: None,
                },
                cx,
            )
        });
        state.update(cx, |state, _| {
            let id = Uuid::new_v4();
            let mut thread = state.build_conversation(&project, Some("omp"), id, None);
            assert_eq!(state.working_tree_of(&thread).unwrap().1, project.root);
            thread.remote = Some(RemoteLocation {
                host: "remote".into(),
                project: "mounted".into(),
            });
            assert!(state.working_tree_of(&thread).is_none());
            state.snapshot.threads.push(thread);
            state.selected_project = Some(project.id);
            state.selected_thread = Some(id);
            assert!(state.thread_host_selection_reason(id).is_none());
            assert!(state.commit_reason().is_some());
            assert!(state.switch_branch_reason().is_some());
            assert!(state.create_worktree_reason().is_some());
            assert!(state.commit_changes(id, "Must not commit locally").is_err());
            assert!(state.switch_branch("must-not-exist", true).is_err());
            assert!(
                state
                    .create_worktree_conversation(project.id, "main")
                    .is_err()
            );
            assert!(state.worktree_request.is_none());
            state.snapshot.threads.last_mut().unwrap().execution_started = true;
            assert!(state.thread_host_selection_reason(id).is_some());
            assert!(state.set_thread_host(id, None).is_err());
            assert_eq!(
                state
                    .snapshot
                    .threads
                    .last()
                    .unwrap()
                    .remote
                    .as_ref()
                    .unwrap()
                    .host,
                "remote"
            );
            assert!(!project.root.exists());
        });
    }
}
