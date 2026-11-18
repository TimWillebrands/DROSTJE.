use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{LinkKey, OpenError, Topic, view::Patch};

pub const TEXT_LIMIT: usize = 3800;
pub const ATTACHMENT_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Down {
    Update {
        epoch: Uuid,
        seq: u64,
        reset: bool,
        patch: Box<Patch>,
    },
    Presence {
        epoch: Uuid,
        seq: u64,
        online: bool,
    },
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Up {
    Sync {
        watch: Option<Uuid>,
    },
    Watch {
        thread: Option<Uuid>,
    },
    Prompt {
        id: Uuid,
        thread: Uuid,
        text: String,
    },
    Unpaired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sealed {
    Text(String),
    Attachment(Vec<u8>),
}

fn context(direction: &str, topic: &Topic) -> String {
    format!("drostje-{direction}:{topic}")
}

fn seal<T: Serialize>(key: &LinkKey, context: &str, value: &T) -> Sealed {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let sealed = key.seal(context, value);
    let text = URL_SAFE_NO_PAD.encode(&sealed);
    if text.len() <= TEXT_LIMIT {
        Sealed::Text(text)
    } else {
        Sealed::Attachment(sealed)
    }
}

pub fn seal_down(key: &LinkKey, topic: &Topic, down: &Down) -> Sealed {
    seal(key, &context("down", topic), down)
}

pub fn seal_up(key: &LinkKey, topic: &Topic, up: &Up) -> Sealed {
    seal(key, &context("up", topic), up)
}

pub enum Received<'a> {
    Text(&'a str),
    Attachment(&'a [u8]),
}

fn open<T: serde::de::DeserializeOwned>(
    key: &LinkKey,
    context: &str,
    received: Received<'_>,
) -> Result<T, OpenError> {
    let bytes = match received {
        Received::Text(text) => key.open_text(context, text)?,
        Received::Attachment(bytes) => key.open_bytes(context, bytes)?,
    };
    serde_json::from_slice(&bytes).map_err(|error| OpenError::Shape(error.to_string()))
}

pub fn open_down(key: &LinkKey, topic: &Topic, received: Received<'_>) -> Result<Down, OpenError> {
    open(key, &context("down", topic), received)
}

pub fn open_up(key: &LinkKey, topic: &Topic, received: Received<'_>) -> Result<Up, OpenError> {
    open(key, &context("up", topic), received)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{Patch, ThreadKind, ThreadStatus, ThreadView};

    fn key() -> LinkKey {
        LinkKey::from_bytes([3; 32])
    }

    #[test]
    fn small_messages_travel_as_text_and_open_only_in_their_direction() {
        let topic = Topic::random();
        let up = Up::Watch {
            thread: Some(Uuid::from_u128(1)),
        };
        let Sealed::Text(text) = seal_up(&key(), &topic, &up) else {
            panic!("a watch request fits in a text message")
        };
        assert_eq!(open_up(&key(), &topic, Received::Text(&text)).unwrap(), up);
        assert!(open_down(&key(), &topic, Received::Text(&text)).is_err());
        assert!(open_up(&key(), &Topic::random(), Received::Text(&text)).is_err());
    }

    #[test]
    fn large_updates_become_attachments() {
        let topic = Topic::random();
        let threads = (0..200)
            .map(|n| ThreadView {
                id: Uuid::from_u128(n),
                title: format!("A fairly descriptive Thread title number {n}"),
                kind: ThreadKind::Conversation {
                    harness: "claude-code".into(),
                },
                status: ThreadStatus::Idle,
                unseen: false,
                updated_at: "2026-11-17T10:00:00Z".into(),
            })
            .collect();
        let down = Down::Update {
            epoch: Uuid::from_u128(9),
            seq: 1,
            reset: true,
            patch: Box::new(Patch {
                threads,
                ..Patch::default()
            }),
        };
        let Sealed::Attachment(bytes) = seal_down(&key(), &topic, &down) else {
            panic!("200 threads do not fit in a text message")
        };
        assert_eq!(
            open_down(&key(), &topic, Received::Attachment(&bytes)).unwrap(),
            down
        );
    }

    #[test]
    fn long_prompts_travel_up_as_attachments() {
        let topic = Topic::random();
        let up = Up::Prompt {
            id: Uuid::from_u128(4),
            thread: Uuid::from_u128(1),
            text: "Explain the redirect loop. ".repeat(400),
        };
        let Sealed::Attachment(bytes) = seal_up(&key(), &topic, &up) else {
            panic!("a 10 kB prompt does not fit in a text message")
        };
        assert_eq!(
            open_up(&key(), &topic, Received::Attachment(&bytes)).unwrap(),
            up
        );
    }

    #[test]
    fn text_messages_stay_under_the_ntfy_limit() {
        let topic = Topic::random();
        for size in [0usize, 1000, 2700, 2800, 2900, 5000] {
            let down = Down::Update {
                epoch: Uuid::nil(),
                seq: 1,
                reset: false,
                patch: Box::new(Patch {
                    desktop: Some("x".repeat(size)),
                    ..Patch::default()
                }),
            };
            if let Sealed::Text(text) = seal_down(&key(), &topic, &down) {
                assert!(text.len() <= TEXT_LIMIT, "{size}: {}", text.len());
            }
        }
    }
}
