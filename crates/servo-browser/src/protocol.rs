//! Versioned, bounded framing shared by the app and its independently linked Servo helper.
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

const MAGIC: [u8; 4] = *b"DSV1";
const MAX_HEADER: usize = 1024 * 1024;
pub const MAX_FRAME: usize = 64 * 1024 * 1024;
pub const MAX_PENDING: usize = 256;

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

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Open { id: Uuid, url: String },
    Command { id: Uuid, command: BrowserCommand },
    Close { id: Uuid },
    Shutdown,
}

impl Request {
    pub fn id(&self) -> Option<Uuid> {
        match self {
            Self::Open { id, .. } | Self::Command { id, .. } | Self::Close { id } => Some(*id),
            Self::Shutdown => None,
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
            ),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PageState {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub error: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

pub fn write_packet<T: Serialize>(
    writer: &mut impl Write,
    header: &T,
    bytes: &[u8],
) -> io::Result<()> {
    let header = serde_json::to_vec(header).map_err(io::Error::other)?;
    if header.len() > MAX_HEADER || bytes.len() > MAX_FRAME {
        return Err(invalid("Browser message exceeds the protocol size limit"));
    }
    writer.write_all(&MAGIC)?;
    writer.write_all(&(header.len() as u32).to_le_bytes())?;
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&header)?;
    writer.write_all(bytes)?;
    writer.flush()
}

pub fn read_packet<T: DeserializeOwned>(
    reader: &mut impl Read,
) -> io::Result<Option<(T, Vec<u8>)>> {
    let mut prefix = [0; 12];
    loop {
        match reader.read(&mut prefix[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    reader.read_exact(&mut prefix[1..])?;
    if prefix[..4] != MAGIC {
        return Err(invalid(
            "Incompatible Servo helper protocol; rebuild both DROSTJE and drostje-servo",
        ));
    }
    let header_len = u32::from_le_bytes([prefix[4], prefix[5], prefix[6], prefix[7]]) as usize;
    let bytes_len = u32::from_le_bytes([prefix[8], prefix[9], prefix[10], prefix[11]]) as usize;
    if header_len > MAX_HEADER || bytes_len > MAX_FRAME {
        return Err(invalid("Browser message exceeds the protocol size limit"));
    }
    let mut header = vec![0; header_len];
    reader.read_exact(&mut header)?;
    let header = serde_json::from_slice(&header).map_err(io::Error::other)?;
    let mut bytes = vec![0; bytes_len];
    reader.read_exact(&mut bytes)?;
    Ok(Some((header, bytes)))
}

pub fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn binary_frames_round_trip_without_encoding_pixels() {
        let pixels = vec![0, 255, 13, 10, 42, 0, 255, 255];
        let event = Event::Frame {
            id: Uuid::nil(),
            revision: 42,
            width: 2,
            height: 1,
        };
        let mut stream = Vec::new();
        write_packet(&mut stream, &event, &pixels).unwrap();
        assert!(stream.ends_with(&pixels));
        let mut stream = Cursor::new(stream);
        let (event, actual) = read_packet::<Event>(&mut stream).unwrap().unwrap();
        assert!(matches!(
            event,
            Event::Frame {
                revision: 42,
                width: 2,
                height: 1,
                ..
            }
        ));
        assert_eq!(actual, pixels);
        assert!(read_packet::<Event>(&mut stream).unwrap().is_none());
    }

    #[test]
    fn truncated_and_oversized_packets_fail_before_allocation() {
        let mut packet = Vec::new();
        write_packet(&mut packet, &Request::Shutdown, &[]).unwrap();
        for end in 1..packet.len() {
            assert!(read_packet::<Request>(&mut Cursor::new(&packet[..end])).is_err());
        }
        packet[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_packet::<Request>(&mut Cursor::new(packet)).is_err());
        assert!(frame_len(u32::MAX, u32::MAX).is_err());
        assert!(frame_len(8192, 8192).is_err());
        assert_eq!(frame_len(100, 50).unwrap(), 20_000);
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
