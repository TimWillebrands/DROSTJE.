//! Native conversation boundary. Provider wire objects stay in their harness module.
mod claude;
mod omp;

use crate::model::{ImageAttachment, ModelKey};
use serde_json::Value;
use std::{path::PathBuf, time::SystemTime};
pub const CONVERSATION_HARNESSES: [&str; 2] = ["omp", "claude-code"];

pub fn executable_name(harness: &str) -> anyhow::Result<&'static str> {
    match harness {
        "omp" => Ok("omp"),
        "claude-code" => Ok("claude"),
        harness => anyhow::bail!("Unsupported conversation harness: {harness}"),
    }
}

use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct ConversationConfig {
    pub harness: String,
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub session_dir: PathBuf,
    pub session_ref: Option<String>,
    pub model: Option<ModelKey>,
    /// Claude Code effort level (`--effort`); `None` means the harness default.
    pub effort: Option<String>,
    /// Only an unsent draft may replace an explicitly file-less provider session.
    pub allow_empty_session: bool,
}
#[derive(Clone)]
pub struct ConversationHandle {
    pub commands: async_channel::Sender<ConversationCommand>,
    pub events: async_channel::Receiver<ConversationEvent>,
}
#[derive(Debug)]
pub enum ConversationCommand {
    Submit {
        request_id: Uuid,
        text: String,
        images: Vec<ImageAttachment>,
    },
    Interrupt,
    SelectModel(ModelKey),
    LoadHistory {
        cursor: Option<String>,
    },
    Respond {
        request_id: String,
        answer: UserAnswer,
    },
    Close,
}
#[derive(Clone, Debug)]
pub enum UserAnswer {
    Value(String),
    Confirmed(bool),
    Cancelled { timed_out: bool },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationState {
    Starting,
    Ready,
    Running,
    WaitingForInput,
    Interrupting,
    Disconnected,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed,
    Unknown,
}
#[derive(Clone, Debug)]
pub struct ModelChoice {
    pub key: ModelKey,
    pub name: String,
    pub input: Vec<String>,
    /// Effort levels the harness accepts for this model; empty when effort is not configurable.
    pub efforts: Vec<String>,
}
/// Tokens in the model's context window, as last reported by the harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextUsage {
    pub used_tokens: u64,
    pub max_tokens: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitWindow {
    FiveHour,
    SevenDay,
}
/// Plan rate-limit utilisation for one window; only reported by harnesses whose
/// login has plan limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UsageLimit {
    pub window: LimitWindow,
    /// Percentage of the window used, 0–100.
    pub percent_used: f32,
    pub resets_at: Option<SystemTime>,
}
#[derive(Clone, Debug)]
pub enum ContentBlock {
    Text(String),
    Thinking(String),
    Image(ImageAttachment),
    ImageError(String),
    /// An assistant's request to run a tool. Its outcome arrives separately: live
    /// through `ToolActivity`, or in history as a message whose `tool_call_id` matches.
    ToolCall {
        id: String,
        name: String,
        /// Short provider-supplied purpose line, shown as the card caption.
        intent: Option<String>,
        input: Value,
    },
}
#[derive(Clone, Debug)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub raw: Option<Value>,
    /// Set on tool result messages; names the `ContentBlock::ToolCall` they answer.
    pub tool_call_id: Option<String>,
    pub error: bool,
    pub exit_code: Option<i64>,
}
#[derive(Clone, Debug)]
pub struct ToolActivity {
    pub id: String,
    pub name: String,
    pub intent: Option<String>,
    pub input: Value,
    pub result: Vec<ContentBlock>,
    pub error: bool,
    pub exit_code: Option<i64>,
    pub complete: bool,
    pub raw: Option<Value>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserRequestKind {
    Confirm,
    Select,
    Input,
    Editor,
}
#[derive(Clone, Debug)]
pub struct UserOption {
    pub value: String,
    pub description: Option<String>,
}
#[derive(Clone, Debug)]
pub struct UserRequest {
    pub id: String,
    pub kind: UserRequestKind,
    pub title: String,
    pub options: Vec<UserOption>,
    pub text: String,
    pub placeholder: Option<String>,
    pub deadline: Option<SystemTime>,
}
#[derive(Clone, Debug)]
pub enum ConversationEvent {
    Ready {
        session_ref: String,
        models: Vec<ModelChoice>,
    },
    StateChanged(ConversationState),
    HistoryReset,
    History {
        messages: Vec<Message>,
        next_cursor: Option<String>,
    },
    MessageUpdated(Message),
    ToolUpdated(ToolActivity),
    UserRequest(UserRequest),
    UserRequestDismissed {
        request_id: String,
    },
    SubmissionAccepted {
        request_id: Uuid,
    },
    TurnSettled {
        request_id: Uuid,
        outcome: TurnOutcome,
    },
    ModelSelected(ModelKey),
    TitleChanged(String),
    EditorTextRequested(String),
    StatusUpdated {
        key: String,
        text: Option<String>,
    },
    WidgetUpdated {
        key: String,
        lines: Vec<String>,
    },
    ContextUsageUpdated(ContextUsage),
    /// The complete set of known limit windows; empty when the login has no plan limits.
    LimitsUpdated(Vec<UsageLimit>),
    Notice {
        level: String,
        text: String,
    },
    LinkOffered {
        url: String,
        instructions: Option<String>,
    },
    Diagnostic {
        label: String,
        raw: Value,
    },
    Failed {
        request_id: Option<Uuid>,
        message: String,
    },
}
pub async fn open(config: ConversationConfig) -> anyhow::Result<ConversationHandle> {
    match config.harness.as_str() {
        "claude-code" => claude::open(config).await,
        "omp" => omp::open(config).await,
        harness => anyhow::bail!("Unsupported conversation harness: {harness}"),
    }
}

/// Largest physical JSONL request for a supported harness, without loading or
/// base64-encoding the owned image files.
pub fn submission_size(
    request_id: Uuid,
    text: &str,
    images: &[ImageAttachment],
) -> anyhow::Result<u64> {
    Ok(omp::submission_size(request_id, text, images)?
        .max(claude::submission_size(request_id, text, images)?))
}
/// Image preparation uses this before writing reviewed copies to disk.
pub fn submission_size_for_lengths(
    request_id: Uuid,
    text: &str,
    images: &[(&str, u64)],
) -> anyhow::Result<u64> {
    Ok(
        omp::submission_size_for_lengths(request_id, text, images)?.max(
            claude::submission_size_for_lengths(request_id, text, images)?,
        ),
    )
}
