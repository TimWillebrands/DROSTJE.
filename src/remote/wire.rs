//! Versioned, path-free Conversation protocol inside the authenticated relay envelope.
use crate::harness::{ConversationEvent, PeerMessage, UserAnswer};
use crate::model::{ImageAttachment, ModelKey};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;

pub const VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteConversationConfig {
    pub harness: String,
    pub session_ref: Option<String>,
    pub model: Option<ModelKey>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    pub allow_empty_session: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: Uuid,
    pub version: u32,
    pub thread: Uuid,
    pub epoch: Uuid,
    pub operation: Operation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Operation {
    Open {
        project: String,
        config: RemoteConversationConfig,
        after: u64,
        generation: Option<Uuid>,
        resume: bool,
    },
    Command(WireCommand),
    CancelPeer {
        attempt: Uuid,
    },
    Ack {
        through: u64,
    },
    Detach,
    Heartbeat,
    NetworkResponse {
        call: Uuid,
        result: Result<Value, String>,
    },
    ReadDiff {
        path: Option<String>,
    },
    ReadFile {
        path: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WireCommand {
    Submit {
        request_id: Uuid,
        text: String,
        images: Vec<WireImage>,
    },
    DeliverPeer {
        attempt: Uuid,
        messages: Vec<PeerMessage>,
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
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ReceiptStatus {
    Admitted,
    Rejected(String),
    Unknown(String),
}
#[cfg_attr(
    feature = "desktop",
    expect(
        clippy::large_enum_variant,
        reason = "Desktop events are serialized immediately; keep them inline to avoid an allocation per event"
    )
)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Opened {
        id: Uuid,
        thread: Uuid,
        generation: Uuid,
        session_ref: Option<String>,
        latest_sequence: u64,
        replay_after: u64,
    },
    Event {
        thread: Uuid,
        generation: Uuid,
        sequence: u64,
        event: ConversationEvent,
        images: Vec<WireImage>,
    },
    Recovered {
        thread: Uuid,
        generation: Uuid,
        through: u64,
    },
    Receipt {
        id: Uuid,
        thread: Uuid,
        status: ReceiptStatus,
    },
    Rejected {
        id: Uuid,
        thread: Uuid,
        message: String,
    },
    NetworkRequest {
        thread: Uuid,
        generation: Uuid,
        call: Uuid,
        action: Value,
    },
    Data {
        id: Uuid,
        thread: Uuid,
        result: Result<Value, String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireImage {
    pub id: Uuid,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub data: String,
}
impl WireImage {
    pub fn read(image: &ImageAttachment) -> Result<Self> {
        use std::io::Read;
        let file = std::fs::File::open(&image.path)?;
        ensure!(
            file.metadata()?.len() <= MAX_IMAGE_BYTES as u64,
            "Remote image exceeds transfer limit"
        );
        let mut bytes = Vec::new();
        file.take(MAX_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_IMAGE_BYTES,
            "Remote image exceeds transfer limit"
        );
        Ok(Self {
            id: image.id,
            mime: image.mime.clone(),
            width: image.width,
            height: image.height,
            data: STANDARD.encode(bytes),
        })
    }
}
/// Newly created image files remain rollback-owned until native queue admission.
pub(crate) struct StagedImages {
    images: Vec<ImageAttachment>,
    created: Vec<std::path::PathBuf>,
    added_bytes: u64,
    committed: bool,
}
impl StagedImages {
    pub(crate) fn take_images(&mut self) -> Vec<ImageAttachment> {
        std::mem::take(&mut self.images)
    }
    pub(crate) fn commit(&mut self) -> u64 {
        self.committed = true;
        self.added_bytes
    }
}
impl Drop for StagedImages {
    fn drop(&mut self) {
        if !self.committed {
            for path in &self.created {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}
struct DecodedImage<'a> {
    wire: &'a WireImage,
    bytes: Vec<u8>,
}
fn decode_images(images: &[WireImage]) -> Result<Vec<DecodedImage<'_>>> {
    ensure!(images.len() <= 32, "Too many remote images");
    let mut total = 0usize;
    images.iter().map(|image| {
        ensure!(matches!(image.mime.as_str(), "image/png" | "image/jpeg" | "image/webp"), "Remote images must be PNG, JPEG or WebP; import/convert TIFF on the originating desktop first");
        ensure!(image.data.len() <= MAX_IMAGE_BYTES * 4 / 3 + 4, "Remote image exceeds transfer limit");
        let bytes = STANDARD.decode(&image.data)?;
        total += bytes.len();
        ensure!(total <= MAX_IMAGE_BYTES, "Remote images exceed transfer limit");
        let (decoded, mime, _) = crate::attachments::decode(&bytes)?;
        ensure!(mime == image.mime, "Remote image MIME does not match transferred bytes");
        ensure!(decoded.width() == image.width && decoded.height() == image.height, "Remote image dimensions do not match bytes");
        Ok(DecodedImage { wire: image, bytes })
    }).collect()
}
fn stage_decoded(
    images: Vec<DecodedImage<'_>>,
    directory: &Path,
    budget: u64,
) -> Result<StagedImages> {
    use std::io::{Read, Write};
    let mut staged = StagedImages {
        images: Vec::with_capacity(images.len()),
        created: Vec::new(),
        added_bytes: 0,
        committed: false,
    };
    if !images.is_empty() {
        std::fs::create_dir_all(directory)?;
    }
    for DecodedImage { wire: image, bytes } in images {
        let path = directory.join(image.id.to_string());
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.file_type().is_file() && metadata.len() == bytes.len() as u64,
                    "Remote image ID already has different bytes or is not an owned file"
                );
                let mut existing = Vec::new();
                std::fs::File::open(&path)?
                    .take(bytes.len() as u64 + 1)
                    .read_to_end(&mut existing)?;
                ensure!(
                    existing == bytes,
                    "Remote image ID already has different bytes"
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure!(
                    bytes.len() as u64 <= budget.saturating_sub(staged.added_bytes),
                    "Host attachment storage budget exceeded"
                );
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                // Track ownership before any fallible write so partial files roll back too.
                staged.created.push(path.clone());
                file.write_all(&bytes)?;
                file.sync_all()?;
                staged.added_bytes += bytes.len() as u64;
            }
            Err(error) => return Err(error.into()),
        }
        staged.images.push(ImageAttachment {
            id: image.id,
            path,
            mime: image.mime.clone(),
            width: image.width,
            height: image.height,
            original: None,
        });
    }
    Ok(staged)
}
pub(crate) fn stage_submission_images(
    request_id: Uuid,
    text: &str,
    images: &[WireImage],
    directory: &Path,
    budget: u64,
) -> Result<StagedImages> {
    let decoded = decode_images(images)?;
    let lengths: Vec<(&str, u64)> = decoded
        .iter()
        .map(|image| (image.wire.mime.as_str(), image.bytes.len() as u64))
        .collect();
    ensure!(
        crate::harness::submission_size_for_lengths(request_id, text, &lengths)?
            <= crate::attachments::FRAME_LIMIT,
        "Submission exceeds native harness frame limit"
    );
    stage_decoded(decoded, directory, budget)
}
/// Materialize received observation bytes under an owned directory, never a sender path.
pub fn materialize_images(images: &[WireImage], directory: &Path) -> Result<Vec<ImageAttachment>> {
    let mut staged = stage_decoded(decode_images(images)?, directory, u64::MAX)?;
    staged.commit();
    Ok(staged.take_images())
}
/// Move image bytes across the boundary and remove every host-local pathname.
pub fn event_images(event: &mut ConversationEvent) -> Result<Vec<WireImage>> {
    use crate::harness::ContentBlock;
    let mut images: Vec<WireImage> = Vec::new();
    let mut transferred = 0usize;
    let mut visit = |blocks: &mut Vec<ContentBlock>| -> Result<()> {
        for block in blocks {
            if let ContentBlock::Image(image) = block {
                if !images.iter().any(|existing| existing.id == image.id) {
                    match WireImage::read(image) {
                        Ok(wire)
                            if images.len() < 32
                                && transferred + wire.data.len() <= MAX_IMAGE_BYTES * 4 / 3 =>
                        {
                            transferred += wire.data.len();
                            images.push(wire);
                        }
                        Ok(_) => {
                            *block = ContentBlock::ImageError("Remote image transfer budget exceeded; request a smaller history page".into());
                            continue;
                        }
                        Err(error) => {
                            *block = ContentBlock::ImageError(format!(
                                "Image is unavailable on the remote host: {error:#}"
                            ));
                            continue;
                        }
                    }
                }
                image.path.clear();
                image.original = None;
            }
        }
        Ok(())
    };
    match event {
        ConversationEvent::History { messages, .. } => {
            for message in messages {
                visit(&mut message.content)?;
            }
        }
        ConversationEvent::MessageUpdated(message) => visit(&mut message.content)?,
        ConversationEvent::ToolUpdated(tool) => visit(&mut tool.result)?,
        _ => {}
    }
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{CommandSource, SlashCommand};

    #[test]
    fn unknown_command_refusal_survives_wire_roundtrip() {
        let event = ConversationEvent::CommandsUpdated(vec![SlashCommand {
            name: "provider-command".into(),
            aliases: Vec::new(),
            description: String::new(),
            argument_hint: None,
            source: CommandSource::Other("provider-extension".into()),
            blocked: Some("An exact, previously unknown provider reason".into()),
        }]);
        let bytes = serde_json::to_vec(&event).unwrap();
        let ConversationEvent::CommandsUpdated(commands) = serde_json::from_slice(&bytes).unwrap()
        else {
            panic!("wrong event")
        };
        assert_eq!(
            commands[0].blocked.as_deref(),
            Some("An exact, previously unknown provider reason")
        );
    }

    #[test]
    fn failed_image_transfer_preserves_adjacent_message_text() {
        let mut event = ConversationEvent::MessageUpdated(crate::harness::Message {
            id: "message".into(),
            role: "assistant".into(),
            content: vec![
                crate::harness::ContentBlock::Text("keep this text".into()),
                crate::harness::ContentBlock::Image(ImageAttachment {
                    id: Uuid::new_v4(),
                    path: Path::new("/nonexistent-drostje-host-image").to_path_buf(),
                    mime: "image/png".into(),
                    width: 1,
                    height: 1,
                    original: None,
                }),
            ],
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        });
        assert!(event_images(&mut event).unwrap().is_empty());
        let ConversationEvent::MessageUpdated(message) = event else {
            panic!("wrong event")
        };
        assert!(
            matches!(&message.content[0], crate::harness::ContentBlock::Text(text) if text == "keep this text")
        );
        assert!(matches!(
            &message.content[1],
            crate::harness::ContentBlock::ImageError(_)
        ));
    }
}
