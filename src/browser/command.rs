//! Typed worker requests and retained engine events; no process framing.
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const MAX_FRAME: usize = 64 * 1024 * 1024;
pub(super) const MAX_PENDING: usize = 256;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BrowserCommand {
    Navigate {
        url: String,
    },
    Back,
    Forward,
    Reload,
    /// Dimensions are physical pixels; scale is physical pixels per logical pixel.
    Resize {
        width: u32,
        height: u32,
        scale: f32,
    },
    /// Opt in to natural content-height snapshots for inline artifacts only.
    SetContentSizing {
        enabled: bool,
    },
    /// Positions are logical pixels. Buttons use DOM numbering (0 left, 1 middle, 2 right).
    Pointer {
        x: f32,
        y: f32,
        button: u8,
        pressed: bool,
    },
    Move {
        x: f32,
        y: f32,
    },
    /// Deltas are logical pixels, positive right/down.
    Scroll {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
    },
    /// DOM key/code names. Printable keydown inserts text; do not also send Text for it.
    Key {
        key: String,
        code: String,
        pressed: bool,
        shift: bool,
        control: bool,
        alt: bool,
        meta: bool,
    },
    /// Committed input-method text, not a synthetic DOM assignment.
    Text {
        text: String,
    },
    Focus {
        focused: bool,
    },
    Evaluate {
        request_id: String,
        script: String,
    },
    /// Settle an artifact page's pending `drostje.send` promise. Host-originated only:
    /// browser tools never construct it, so agents cannot forge delivery.
    Receipt {
        event: String,
        status: ReceiptStatus,
        reason: Option<String>,
    },
    /// Replace an artifact's theme tokens, for reloads and the current document.
    ArtifactTheme {
        css: String,
    },
}

/// What happened to one artifact channel event, as its page is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    /// The agent's runtime admitted the event as context.
    Delivered,
    /// Held until the runtime can receive it.
    Queued,
    /// Not delivered; the reason says why.
    Failed,
    /// Dispatched, but admission was not confirmed. Never resent.
    Unknown,
}

impl ReceiptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Queued => "queued",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// One `drostje.send` message from an artifact page, validated by the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelEvent {
    /// The page's event ID, echoed back in its receipt.
    pub id: String,
    pub kind: String,
    /// The complete `{v,id,type,data}` JSON object.
    pub body: String,
}

/// Largest encoded channel message accepted, in UTF-8 bytes. The kit bounds messages at
/// 32 Ki UTF-16 units, which is at most three times as many UTF-8 bytes.
pub const MAX_CHANNEL_MESSAGE: usize = 3 * 32 * 1024;
const CHANNEL_PREFIX: &str = "__DROSTJE_CHANNEL__";

impl ChannelEvent {
    /// `None` when `message` is not a channel message at all. Malformed channel
    /// messages are errors so the page is told, rather than dropped silently.
    pub fn parse(message: &str) -> Option<Result<Self, String>> {
        let encoded = message.strip_prefix(CHANNEL_PREFIX)?;
        Some((|| {
            if encoded.len() > MAX_CHANNEL_MESSAGE {
                return Err("Artifact event exceeds the 32 KiB event limit".to_owned());
            }
            let value: Value = serde_json::from_str(encoded)
                .map_err(|error| format!("Artifact event is not JSON: {error}"))?;
            if value["v"] != 1 {
                return Err("Artifact event has an unsupported version".into());
            }
            let id = value["id"]
                .as_str()
                .filter(|id| valid_id(id))
                .ok_or("Artifact event has no valid id")?;
            let kind = value["type"]
                .as_str()
                .filter(|kind| valid_kind(kind))
                .ok_or("Artifact event type must match [a-z][a-z0-9_.-]{0,63}")?;
            Ok(Self {
                id: id.to_owned(),
                kind: kind.to_owned(),
                body: encoded.to_owned(),
            })
        })())
    }

    /// The event ID of a malformed message, when one can be read, so its sender can be told.
    pub fn id_of(message: &str) -> Option<String> {
        let encoded = message.strip_prefix(CHANNEL_PREFIX)?;
        if encoded.len() > MAX_CHANNEL_MESSAGE {
            return None;
        }
        let value: Value = serde_json::from_str(encoded).ok()?;
        value["id"]
            .as_str()
            .filter(|id| valid_id(id))
            .map(str::to_owned)
    }
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_kind(kind: &str) -> bool {
    let mut bytes = kind.bytes();
    kind.len() <= 64
        && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        })
}

#[derive(Debug)]
pub(super) enum Request {
    Open {
        id: Uuid,
        url: String,
    },
    Command {
        id: Uuid,
        command: BrowserCommand,
    },
    Batch {
        id: Uuid,
        request_id: String,
        commands: Vec<BrowserCommand>,
        expected_document_revision: Option<u64>,
    },
}

impl Request {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Open { id, .. } | Self::Command { id, .. } | Self::Batch { id, .. } => *id,
        }
    }

    pub fn cost(&self) -> usize {
        match self {
            Self::Batch { commands, .. } => commands.len(),
            _ => 1,
        }
    }

    /// Only adjacent replaceable input may be coalesced: never move a resize/motion
    /// across a click, navigation or evaluation which observes its effects.
    pub fn supersedes(&self, previous: &Self) -> bool {
        match (self, previous) {
            (
                Self::Command { id, command },
                Self::Command {
                    id: old_id,
                    command: old,
                },
            ) if id == old_id => matches!(
                (command, old),
                (BrowserCommand::Move { .. }, BrowserCommand::Move { .. })
                    | (BrowserCommand::Resize { .. }, BrowserCommand::Resize { .. })
                    | (
                        BrowserCommand::SetContentSizing { .. },
                        BrowserCommand::SetContentSizing { .. }
                    )
            ),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageState {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub error: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub content_height: Option<f32>,
    pub document_revision: u64,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub scale: f32,
}

impl Default for PageState {
    fn default() -> Self {
        Self {
            url: String::new(),
            title: String::new(),
            loading: false,
            error: None,
            can_go_back: false,
            can_go_forward: false,
            content_height: None,
            document_revision: 0,
            viewport_width: 800,
            viewport_height: 600,
            scale: 1.0,
        }
    }
}

#[derive(Debug)]
pub enum Event {
    State {
        id: Uuid,
        state: PageState,
    },
    Frame {
        id: Uuid,
        revision: u64,
        document_revision: u64,
        scale: f32,
        width: u32,
        height: u32,
    },
    Evaluation {
        id: Uuid,
        request_id: String,
        result: Result<Value, String>,
    },
    Diagnostic {
        id: Uuid,
        kind: &'static str,
        level: String,
        message: String,
    },
    /// A validated channel message from an artifact document.
    Channel {
        id: Uuid,
        event: ChannelEvent,
    },
    Fatal {
        error: String,
    },
}

pub fn frame_len(width: u32, height: u32) -> io::Result<usize> {
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(invalid(
            "Browser viewport must be between 1 and 8192 pixels per side",
        ));
    }
    let length = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|length| *length <= MAX_FRAME)
        .ok_or_else(|| invalid("Browser viewport exceeds the 64 MiB RGBA frame limit"))?;
    Ok(length)
}

pub fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_messages_are_validated_and_malformed_ones_keep_their_id_for_a_receipt() {
        assert!(ChannelEvent::parse("ordinary console text").is_none());
        let event = ChannelEvent::parse(
            r#"__DROSTJE_CHANNEL__{"v":1,"id":"5f0c-ab","type":"scope.pick","data":{"a":1}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(event.id, "5f0c-ab");
        assert_eq!(event.kind, "scope.pick");
        assert!(event.body.starts_with(r#"{"v":1"#));
        for (message, reason) in [
            (r#"{"v":2,"id":"a","type":"x"}"#, "version"),
            (r#"{"v":1,"id":"a b","type":"x"}"#, "id"),
            (r#"{"v":1,"id":"a","type":"Upper"}"#, "type"),
            (r#"{"v":1,"id":"a","type":"1x"}"#, "type"),
            ("not json", "JSON"),
        ] {
            let error = ChannelEvent::parse(&format!("__DROSTJE_CHANNEL__{message}"))
                .unwrap()
                .unwrap_err();
            assert!(error.contains(reason), "{message}: {error}");
        }
        let oversized = format!(
            r#"__DROSTJE_CHANNEL__{{"v":1,"id":"big","type":"x","data":"{}"}}"#,
            "x".repeat(MAX_CHANNEL_MESSAGE)
        );
        assert!(ChannelEvent::parse(&oversized).unwrap().is_err());
        assert_eq!(
            ChannelEvent::id_of(&oversized),
            None,
            "never parse past the bound"
        );
        assert_eq!(
            ChannelEvent::id_of(r#"__DROSTJE_CHANNEL__{"v":1,"id":"kept","type":"BAD"}"#)
                .as_deref(),
            Some("kept")
        );
    }

    #[test]
    fn viewport_dimensions_are_bounded_before_frame_allocation() {
        assert_eq!(frame_len(1, 1).unwrap(), 4);
        assert_eq!(frame_len(4096, 4096).unwrap(), MAX_FRAME);
        for (width, height) in [
            (0, 1),
            (1, 0),
            (8193, 1),
            (1, 8193),
            (8192, 8192),
            (u32::MAX, u32::MAX),
        ] {
            assert!(frame_len(width, height).is_err());
        }
    }

    #[test]
    fn coalescing_preserves_input_barriers_and_browser_identity() {
        let id = Uuid::new_v4();
        let motion = |id, x| Request::Command {
            id,
            command: BrowserCommand::Move { x, y: 1.0 },
        };
        assert!(motion(id, 2.0).supersedes(&motion(id, 1.0)));
        assert!(!motion(Uuid::new_v4(), 2.0).supersedes(&motion(id, 1.0)));
        assert!(!motion(id, 2.0).supersedes(&Request::Command {
            id,
            command: BrowserCommand::Pointer {
                x: 1.0,
                y: 1.0,
                button: 0,
                pressed: true
            },
        }));
    }
}
