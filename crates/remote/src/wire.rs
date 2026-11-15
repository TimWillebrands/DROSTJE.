use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use drostje_harness::{ConversationEvent, PeerMessage, UserAnswer};
use drostje_model::{ImageAttachment, ModelKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, path::Path};
use uuid::Uuid;

pub const VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub artifacts: bool,
    #[serde(default)]
    pub artifact_completion: bool,
    #[serde(default)]
    pub image_reuse: bool,
}

impl Capabilities {
    pub const DESKTOP: Self = Self {
        artifacts: true,
        artifact_completion: true,
        image_reuse: true,
    };
}
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
    #[serde(default)]
    pub capabilities: Capabilities,
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
    feature = "browser",
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
    ArtifactRequest {
        thread: Uuid,
        generation: Uuid,
        call: Uuid,
        name: String,
        title: Option<String>,
        html: String,
        #[serde(default)]
        placement: drostje_browser_tools::ArtifactPlacement,
    },
    ArtifactComplete {
        thread: Uuid,
        generation: Uuid,
        call: Uuid,
        browser: Uuid,
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
    checked: bool,
}
fn decode_images<'a>(
    images: &'a [WireImage],
    directory: &Path,
    known: &HashMap<Uuid, ImageAttachment>,
) -> Result<Vec<DecodedImage<'a>>> {
    ensure!(images.len() <= 32, "Too many remote images");
    let mut total = 0usize;
    images.iter().map(|image| {
        ensure!(matches!(image.mime.as_str(), "image/png" | "image/jpeg" | "image/webp"), "Remote images must be PNG, JPEG or WebP; import/convert TIFF on the originating desktop first");
        ensure!(image.data.len() <= MAX_IMAGE_BYTES * 4 / 3 + 4, "Remote image exceeds transfer limit");
        let bytes = STANDARD.decode(&image.data)?;
        total += bytes.len();
        ensure!(total <= MAX_IMAGE_BYTES, "Remote images exceed transfer limit");
        let stored = known.get(&image.id).is_some_and(|stored| {
            stored.mime == image.mime
                && stored.width == image.width
                && stored.height == image.height
                && stored.path == directory.join(image.id.to_string())
        });
        if !stored {
            check_pixels(image, &bytes)?;
        }
        Ok(DecodedImage { wire: image, bytes, checked: !stored })
    }).collect()
}
fn check_pixels(image: &WireImage, bytes: &[u8]) -> Result<()> {
    let (decoded, mime, _) = drostje_harness::attachments::decode(bytes)?;
    ensure!(
        mime == image.mime,
        "Remote image MIME does not match transferred bytes"
    );
    ensure!(
        decoded.width() == image.width && decoded.height() == image.height,
        "Remote image dimensions do not match bytes"
    );
    Ok(())
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
    for DecodedImage {
        wire: image,
        bytes,
        checked,
    } in images
    {
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
                if !checked {
                    check_pixels(image, &bytes)?;
                }
                ensure!(
                    bytes.len() as u64 <= budget.saturating_sub(staged.added_bytes),
                    "Host attachment storage budget exceeded"
                );
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
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
    let decoded = decode_images(images, directory, &HashMap::new())?;
    let lengths: Vec<(&str, u64)> = decoded
        .iter()
        .map(|image| (image.wire.mime.as_str(), image.bytes.len() as u64))
        .collect();
    ensure!(
        drostje_harness::submission_size_for_lengths(request_id, text, &lengths)?
            <= drostje_harness::attachments::FRAME_LIMIT,
        "Submission exceeds native harness frame limit"
    );
    stage_decoded(decoded, directory, budget)
}
pub fn materialize_images(
    images: &[WireImage],
    directory: &Path,
    known: &HashMap<Uuid, ImageAttachment>,
) -> Result<Vec<ImageAttachment>> {
    let mut staged = stage_decoded(
        decode_images(images, directory, known)?,
        directory,
        u64::MAX,
    )?;
    staged.commit();
    Ok(staged.take_images())
}
pub fn event_images(event: &mut ConversationEvent) -> Result<Vec<WireImage>> {
    use drostje_harness::ContentBlock;
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
    use drostje_harness::{CommandSource, SlashCommand};

    #[test]
    fn desktops_and_hosts_of_either_version_still_understand_each_other() {
        let old = serde_json::json!({"id":Uuid::new_v4(),"version":1,"thread":Uuid::new_v4(),
            "epoch":Uuid::new_v4(),"operation":"Heartbeat"});
        let request: Request = serde_json::from_value(old).unwrap();
        assert_eq!(request.capabilities, Capabilities::default());
        assert!(!request.capabilities.artifacts);
        assert!(!request.capabilities.artifact_completion);
        assert!(!request.capabilities.image_reuse);
        let future: Capabilities =
            serde_json::from_value(serde_json::json!({"artifacts":true,"later":true})).unwrap();
        assert!(future.artifacts && !future.image_reuse);
        let new = serde_json::to_value(Request {
            id: Uuid::new_v4(),
            version: VERSION,
            capabilities: Capabilities::DESKTOP,
            thread: Uuid::new_v4(),
            epoch: Uuid::new_v4(),
            operation: Operation::Heartbeat,
        })
        .unwrap();
        assert_eq!(new["version"], 1);
        assert_eq!(new["capabilities"]["artifacts"], true);
        assert_eq!(new["capabilities"]["artifact_completion"], true);
        assert_eq!(new["capabilities"]["image_reuse"], true);
        let command: WireCommand = serde_json::from_value(serde_json::json!({"DeliverPeer":{
            "attempt":Uuid::new_v4(),"messages":[{"id":Uuid::new_v4(),"sender":Uuid::new_v4(),
            "sender_name":"Peer","space":Uuid::new_v4(),"body":"hello"}]}}))
        .unwrap();
        let WireCommand::DeliverPeer { messages, .. } = command else {
            panic!("expected peer context");
        };
        assert_eq!(messages[0].origin, drostje_harness::ContextOrigin::Peer);
    }

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
        let mut event = ConversationEvent::MessageUpdated(drostje_harness::Message {
            id: "message".into(),
            role: "assistant".into(),
            content: vec![
                drostje_harness::ContentBlock::Text("keep this text".into()),
                drostje_harness::ContentBlock::Image(ImageAttachment {
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
            matches!(&message.content[0], drostje_harness::ContentBlock::Text(text) if text == "keep this text")
        );
        assert!(matches!(
            &message.content[1],
            drostje_harness::ContentBlock::ImageError(_)
        ));
    }

    fn png() -> WireImage {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        WireImage {
            id: Uuid::new_v4(),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            data: STANDARD.encode(bytes.into_inner()),
        }
    }

    #[test]
    fn known_images_are_compared_with_their_file_instead_of_decoded_again() {
        let directory = std::env::temp_dir().join(format!("drostje-wire-{}", Uuid::new_v4()));
        let image = png();
        let stored = materialize_images(std::slice::from_ref(&image), &directory, &HashMap::new())
            .unwrap()
            .remove(0);
        assert_eq!(stored.path, directory.join(image.id.to_string()));
        let known = HashMap::from([(image.id, stored.clone())]);
        let opaque = b"not pixels".to_vec();
        std::fs::write(&stored.path, &opaque).unwrap();
        let mut resent = image.clone();
        resent.data = STANDARD.encode(&opaque);
        let reused = materialize_images(std::slice::from_ref(&resent), &directory, &known).unwrap();
        assert_eq!(reused[0].path, stored.path);
        assert!(
            materialize_images(std::slice::from_ref(&resent), &directory, &HashMap::new()).is_err()
        );
        resent.data = STANDARD.encode(b"other bytes");
        assert!(materialize_images(std::slice::from_ref(&resent), &directory, &known).is_err());
        std::fs::remove_file(&stored.path).unwrap();
        resent.data = STANDARD.encode(&opaque);
        assert!(materialize_images(std::slice::from_ref(&resent), &directory, &known).is_err());
        assert!(!stored.path.exists());
        assert!(materialize_images(std::slice::from_ref(&image), &directory, &known).is_ok());
        let _ = std::fs::remove_dir_all(directory);
    }
}
