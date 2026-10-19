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
}

#[derive(Debug)]
pub(super) enum Request {
    Open { id: Uuid, url: String },
    Command { id: Uuid, command: BrowserCommand },
}

impl Request {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Open { id, .. } | Self::Command { id, .. } => *id,
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageState {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub error: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub content_height: Option<f32>,
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
        width: u32,
        height: u32,
    },
    Evaluation {
        id: Uuid,
        request_id: String,
        result: Result<Value, String>,
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
