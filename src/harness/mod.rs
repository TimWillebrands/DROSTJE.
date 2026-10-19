//! Native conversation boundary. Provider wire objects stay in their harness module.
mod claude;
mod commands;
mod omp;
mod pi;
use crate::model::{ImageAttachment, ModelKey};
pub use commands::{CommandCatalog, CommandSource, SlashCommand, invocation};
use serde_json::Value;
use std::{path::PathBuf, time::SystemTime};
pub const CONVERSATION_HARNESSES: [&str; 3] = ["omp", "pi", "claude-code"];

pub fn executable_name(harness: &str) -> anyhow::Result<&'static str> {
    match harness {
        "omp" => Ok("omp"),
        "pi" => Ok("pi"),
        "claude-code" => Ok("claude"),
        harness => anyhow::bail!("Unsupported conversation harness: {harness}"),
    }
}

pub fn display_name(harness: &str) -> &str {
    match harness {
        "omp" => "OMP",
        "pi" => "Pi",
        "claude-code" => "Claude Code",
        harness => harness,
    }
}

use uuid::Uuid;

/// Permission modes a harness accepts as a launch flag, as `(value, label)`. Empty when the
/// harness has none.
pub fn permission_modes(harness: &str) -> &'static [(&'static str, &'static str)] {
    match harness {
        "claude-code" => &[
            ("acceptEdits", "Accept edits"),
            ("plan", "Plan"),
            ("auto", "Auto"),
            ("dontAsk", "Don't ask"),
            ("bypassPermissions", "Bypass permissions"),
        ],
        _ => &[],
    }
}

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
    /// Claude Code permission mode (`--permission-mode`); `None` means the harness default.
    pub permission_mode: Option<String>,
    /// Optional live artifact/browser tools, scoped to this application's conversation.
    pub browser_tools: Option<crate::browser_tools::BrowserToolConfig>,
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
    /// Approve this Claude Code permission, switch the session to auto, and approve other
    /// permission requests that are already pending.
    PermissionAuto,
    Cancelled {
        timed_out: bool,
    },
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
    /// A Claude Code permission request. Kept distinct so only permissions expose auto approval.
    Permission,
    /// A Claude Code AskUserQuestion selection which also accepts a free-form answer.
    Question,
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
    /// The complete command list; replaces any earlier one.
    CommandsUpdated(Vec<SlashCommand>),
    /// The harness could not list its commands. Prompts still send typed commands.
    CommandsUnavailable(String),
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
        "omp" | "pi" => omp::open(config).await,
        harness => anyhow::bail!("Unsupported conversation harness: {harness}"),
    }
}

pub async fn install_pi(data_dir: PathBuf) -> anyhow::Result<PathBuf> {
    pi::install(data_dir).await
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

/// A bounded exact-byte cache avoids rewriting the same image on every canonical
/// message snapshot. Files outlive cache eviction because messages own references.
#[derive(Default)]
struct ImageCache {
    entries: std::collections::VecDeque<(String, String, ImageAttachment)>,
    bytes: usize,
}
impl ImageCache {
    /// Stores a provider's base64 image under `directory/native-images`.
    fn import(
        &mut self,
        mime: &str,
        encoded: &str,
        directory: &std::path::Path,
    ) -> anyhow::Result<ImageAttachment> {
        use anyhow::{Context, ensure};
        use base64::{Engine, engine::general_purpose::STANDARD};
        ensure!(
            matches!(mime, "image/png" | "image/jpeg" | "image/webp"),
            "Unsupported provider image MIME: {mime}"
        );
        ensure!(
            encoded.len() as u64 <= crate::attachments::FILE_LIMIT.div_ceil(3) * 4,
            "Provider image exceeds 20 MiB"
        );
        if let Some((_, _, image)) = self
            .entries
            .iter()
            .find(|(data, kind, _)| data == encoded && kind == mime)
        {
            return Ok(image.clone());
        }
        let bytes = STANDARD
            .decode(encoded)
            .context("Corrupt provider image base64")?;
        let image =
            crate::attachments::import_bytes(&bytes, mime, &directory.join("native-images"))?;
        const CACHE_BYTES: usize = 32 * 1024 * 1024;
        while self.entries.len() >= 16 || self.bytes + encoded.len() > CACHE_BYTES {
            if let Some((data, _, _)) = self.entries.pop_front() {
                self.bytes -= data.len();
            } else {
                break;
            }
        }
        self.bytes += encoded.len();
        self.entries
            .push_back((encoded.into(), mime.into(), image.clone()));
        Ok(image)
    }
}

#[cfg(test)]
mod tests {
    use super::permission_modes;

    #[test]
    fn claude_code_offers_all_supported_permission_modes() {
        assert_eq!(
            permission_modes("claude-code"),
            [
                ("acceptEdits", "Accept edits"),
                ("plan", "Plan"),
                ("auto", "Auto"),
                ("dontAsk", "Don't ask"),
                ("bypassPermissions", "Bypass permissions"),
            ]
        );
    }
}
