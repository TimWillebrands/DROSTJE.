mod events;
mod launch;

pub use events::{TextPage, read_text};
pub(crate) use launch::same_source;
pub use launch::{FactoryManager, check_readiness};

use crate::model::{FactoryLaunch, LaunchId, ProjectId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorySource {
    LocalFolder(PathBuf),
    AzureWorkItem(i32),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FactoryLaunchSpec {
    pub id: LaunchId,
    pub project_id: ProjectId,
    pub source: FactorySource,
    pub target_repo: PathBuf,
    pub target_branch: String,
    pub data_root: PathBuf,
    pub log_dir: PathBuf,
    pub harness: String,
    pub model: Option<String>,
    pub thought_level: Option<String>,
    pub executable: Option<PathBuf>,
    pub confirm_unknown: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportedState {
    Pending,
    Running,
    Completed,
    Failed,
    Paused,
}

impl ReportedState {
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Paused)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    OwnedRunning,
    Exited(i32),
    Unverified,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEntry {
    pub offset: u64,
    pub timestamp: Option<String>,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub raw: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub id: String,
    pub title: Option<String>,
    pub state: String,
    pub dependencies: Vec<String>,
    pub branch: Option<String>,
    pub step: Option<String>,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FactorySnapshot {
    pub launch: FactoryLaunch,
    pub reported_state: ReportedState,
    pub liveness: Liveness,
    pub title: Option<String>,
    pub tasks: BTreeMap<String, TaskSnapshot>,
    pub notes: Vec<EventEntry>,
    pub artifacts: Vec<PathBuf>,
    pub transcripts: Vec<PathBuf>,
    pub last_seen: Option<String>,
    pub diagnostics: Vec<String>,
    pub correlation_candidates: Vec<PathBuf>,
    pub details: serde_json::Value,
}

impl FactorySnapshot {
    pub fn status_text(&self) -> String {
        match self.reported_state {
            ReportedState::Completed => return "Completed".into(),
            ReportedState::Failed => return "Failed".into(),
            ReportedState::Paused => return "Paused".into(),
            _ => {}
        }
        if let Some(error) = &self.launch.launch_error {
            return format!("Launch failed: {error}");
        }
        match self.liveness {
            Liveness::Exited(code) => {
                format!("Interrupted / Unknown (process exited {code}; no terminal event)")
            }
            Liveness::OwnedRunning if self.reported_state == ReportedState::Pending => {
                "Process running; awaiting Factory events".into()
            }
            Liveness::OwnedRunning => "Running".into(),
            Liveness::Unverified if self.reported_state == ReportedState::Pending => {
                "Launch outcome unknown".into()
            }
            Liveness::Unverified => "Last reported: Running; process unverified".into(),
        }
    }
}
