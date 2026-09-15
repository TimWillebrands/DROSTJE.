use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

pub type ProjectId = Uuid;
pub type ThreadId = Uuid;
pub type LaunchId = Uuid;

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
    pub interactive_model: Option<ModelKey>,
    pub factory_harness: String,
    pub factory_model: Option<String>,
    pub factory_thought_level: Option<String>,
    pub factory_target_branch: Option<String>,
}
impl Default for ProjectDefaults {
    fn default() -> Self {
        Self {
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub id: Uuid,
    pub path: PathBuf,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub original: Option<PathBuf>,
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
    pub draft: String,
    pub attachments: Vec<ImageAttachment>,
    pub submission: Option<Value>,
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
    pub launches: Vec<FactoryLaunch>,
    pub settings: BTreeMap<String, Value>,
}
