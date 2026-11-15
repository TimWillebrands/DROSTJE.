use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

pub type ProjectId = Uuid;
pub type ThreadId = Uuid;
pub type LaunchId = Uuid;
pub type SpaceId = Uuid;
pub type WorktreeId = Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub id: SpaceId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DraftKey {
    Thread(Uuid),
    Extension { thread: Uuid, request: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DraftRevision {
    pub generation: Uuid,
    pub changedtick: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftSnapshot {
    pub key: DraftKey,
    pub generation: Uuid,
    pub changedtick: u64,
    pub text: String,
    pub cursor: (usize, usize),
}
impl DraftSnapshot {
    pub fn revision(&self) -> DraftRevision {
        DraftRevision {
            generation: self.generation,
            changedtick: self.changedtick,
        }
    }
}

pub fn slash_command_insertion(draft: &str, name: &str) -> (usize, String) {
    let line = draft.split('\n').next().unwrap_or("");
    let end = line
        .strip_prefix('/')
        .map(|rest| rest.find(char::is_whitespace).unwrap_or(rest.len()))
        .filter(|&len| !line[1..1 + len].contains('/'))
        .map_or(0, |len| {
            let after = 1 + len;
            after + usize::from(line[after..].starts_with([' ', '\t']))
        });
    (end, format!("/{name} "))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneDirection {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelKey {
    pub provider: String,
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectDefaults {
    pub interactive_harness: String,
    pub interactive_model: Option<ModelKey>,
    pub factory_harness: String,
    pub factory_model: Option<String>,
    pub factory_thought_level: Option<String>,
    pub factory_target_branch: Option<String>,
}
impl Default for ProjectDefaults {
    fn default() -> Self {
        Self {
            interactive_harness: "omp".into(),
            interactive_model: None,
            factory_harness: "opencode".into(),
            factory_model: None,
            factory_thought_level: None,
            factory_target_branch: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub root: PathBuf,
    pub archived: bool,
    pub defaults: ProjectDefaults,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub id: Uuid,
    pub path: PathBuf,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub original: Option<PathBuf>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub id: Uuid,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<ImageAttachment>,
}
impl QueuedMessage {
    pub fn preview(&self) -> String {
        let line = self
            .text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default();
        let mut preview: String = line.chars().take(200).collect();
        if preview.is_empty() && !self.attachments.is_empty() {
            preview = match self.attachments.len() {
                1 => "1 image".into(),
                count => format!("{count} images"),
            };
        }
        preview
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadKind {
    Conversation,
    Factory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorySourceKind {
    WorkItem,
    Folder,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactoryThreadConfig {
    pub source_kind: FactorySourceKind,
    pub source: String,
    pub harness: String,
    pub model: String,
    pub thought_level: String,
    pub target_branch: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    pub id: WorktreeId,
    pub project_id: ProjectId,
    pub path: PathBuf,
    pub codename: String,
    pub branch: String,
    pub base: String,
    pub named: bool,
    pub created_by: ThreadId,
    pub created_at: String,
    pub removed_at: Option<String>,
}
impl Worktree {
    pub fn is_live(&self) -> bool {
        self.removed_at.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Active,
    Completed,
    Abandoned,
    Unknown(String),
}
impl PullRequestState {
    pub fn closed(&self) -> bool {
        match self {
            Self::Completed | Self::Abandoned => true,
            Self::Active | Self::Unknown(_) => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestLink {
    pub id: u64,
    pub url: String,
    pub title: String,
    pub state: PullRequestState,
    #[serde(default)]
    pub work_items: Vec<u64>,
    #[serde(default)]
    pub archived_on_close: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteLocation {
    pub host: String,
    pub project: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thread {
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub kind: ThreadKind,
    pub factory: Option<FactoryThreadConfig>,
    pub title: String,
    pub harness: String,
    pub session_ref: Option<String>,
    pub model: Option<ModelKey>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub worktree_id: Option<WorktreeId>,
    #[serde(default)]
    pub remote: Option<RemoteLocation>,
    #[serde(default)]
    pub execution_started: bool,
    #[serde(default)]
    pub pull_request: Option<PullRequestLink>,
    pub draft: String,
    pub attachments: Vec<ImageAttachment>,
    pub submission: Option<Value>,
    #[serde(default)]
    pub queued: Vec<QueuedMessage>,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactoryLaunch {
    pub id: LaunchId,
    pub project_id: ProjectId,
    pub factory_thread_id: ThreadId,
    pub descriptor: Value,
    pub event_path: Option<PathBuf>,
    pub work_item_id: Option<i32>,
    pub attempt_id: Option<String>,
    pub process_exit: Option<i32>,
    pub launch_error: Option<String>,
    pub created_at: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub projects: Vec<Project>,
    pub threads: Vec<Thread>,
    pub worktrees: Vec<Worktree>,
    pub launches: Vec<FactoryLaunch>,
    pub settings: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::{ImageAttachment, QueuedMessage, slash_command_insertion};
    use std::path::PathBuf;
    use uuid::Uuid;

    fn image() -> ImageAttachment {
        ImageAttachment {
            id: Uuid::new_v4(),
            path: PathBuf::from("/tmp/one.png"),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            original: None,
        }
    }

    #[test]
    fn insertion_replaces_the_typed_command_and_keeps_arguments() {
        assert_eq!(
            slash_command_insertion("", "compact"),
            (0, "/compact ".into())
        );
        assert_eq!(
            slash_command_insertion("/", "compact"),
            (1, "/compact ".into())
        );
        assert_eq!(
            slash_command_insertion("/comp", "compact"),
            (5, "/compact ".into())
        );
        assert_eq!(
            slash_command_insertion("/comp keep tests", "compact"),
            (6, "/compact ".into())
        );
        assert_eq!(
            slash_command_insertion("/comp\nsecond line", "compact"),
            (5, "/compact ".into())
        );
        assert_eq!(
            slash_command_insertion("fix the bug", "skill:tdd"),
            (0, "/skill:tdd ".into())
        );
        assert_eq!(
            slash_command_insertion("/usr/bin is missing", "review"),
            (0, "/review ".into())
        );
    }

    #[test]
    fn queued_preview_is_the_first_non_empty_line_or_the_image_count() {
        let message = QueuedMessage {
            id: Uuid::new_v4(),
            text: "\n  \n  Fix the tests  \nthen push".into(),
            attachments: vec![image()],
        };
        assert_eq!(message.preview(), "Fix the tests");
        let images_only = QueuedMessage {
            id: Uuid::new_v4(),
            text: " \n".into(),
            attachments: vec![image(), image()],
        };
        assert_eq!(images_only.preview(), "2 images");
    }

    #[test]
    fn legacy_conversation_location_defaults_to_local_and_remote_roundtrips() {
        let value = serde_json::json!({
            "id": Uuid::new_v4(), "project_id": Uuid::new_v4(),
            "kind": "conversation", "factory": null, "title": "Legacy",
            "harness": "omp", "session_ref": null, "model": null,
            "draft": "Unsent", "attachments": [], "submission": null,
            "archived": false, "created_at": "2026-01-01", "updated_at": "2026-01-01"
        });
        let mut thread: super::Thread = serde_json::from_value(value).unwrap();
        assert!(thread.remote.is_none());
        assert!(!thread.execution_started);
        thread.remote = Some(super::RemoteLocation {
            host: "host-a".into(),
            project: "mounted".into(),
        });
        thread.execution_started = true;
        let restored: super::Thread =
            serde_json::from_value(serde_json::to_value(&thread).unwrap()).unwrap();
        assert_eq!(restored.remote, thread.remote);
        assert!(restored.execution_started);
        assert_eq!(restored.draft, "Unsent");
    }
}
