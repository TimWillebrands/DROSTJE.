use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub time: i64,
    pub topic: String,
    pub body: String,
    pub attachment: Option<Attachment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub expires: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    Open,
    Keepalive,
    Message(Message),
    Other,
}

#[derive(Deserialize)]
struct RawFrame {
    #[serde(default)]
    id: String,
    #[serde(default)]
    time: i64,
    event: String,
    #[serde(default)]
    topic: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    attachment: Option<Attachment>,
}

pub(crate) fn parse_frame(text: &str) -> Result<Frame, String> {
    let raw: RawFrame =
        serde_json::from_str(text).map_err(|error| format!("Unreadable ntfy event: {error}"))?;
    Ok(match raw.event.as_str() {
        "open" => Frame::Open,
        "keepalive" => Frame::Keepalive,
        "message" => {
            if raw.id.is_empty() {
                return Err("ntfy message event without an id".into());
            }
            Frame::Message(Message {
                id: raw.id,
                time: raw.time,
                topic: raw.topic,
                body: raw.message.unwrap_or_default(),
                attachment: raw.attachment,
            })
        }
        _ => Frame::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_events_keep_id_topic_body_and_attachment() {
        let frame = parse_frame(
            r#"{"id":"hwQ2YpKdmg","time":1700000000,"expires":1700043200,"event":"message","topic":"t","message":"You received a file: x.bin","attachment":{"name":"x.bin","type":"application/octet-stream","size":120,"expires":1700010800,"url":"https://ntfy.example/file/hwQ2YpKdmg.bin"}}"#,
        )
        .unwrap();
        let Frame::Message(message) = frame else {
            panic!("expected a message")
        };
        assert_eq!(message.id, "hwQ2YpKdmg");
        assert_eq!(message.topic, "t");
        let attachment = message.attachment.unwrap();
        assert_eq!(attachment.size, Some(120));
        assert_eq!(attachment.url, "https://ntfy.example/file/hwQ2YpKdmg.bin");
    }

    #[test]
    fn control_events_are_told_apart_and_garbage_is_reported() {
        assert_eq!(
            parse_frame(r#"{"id":"a","time":1,"event":"open","topic":"t"}"#),
            Ok(Frame::Open)
        );
        assert_eq!(
            parse_frame(r#"{"id":"b","time":1,"event":"keepalive","topic":"t"}"#),
            Ok(Frame::Keepalive)
        );
        assert_eq!(
            parse_frame(r#"{"id":"c","time":1,"event":"poll_request","topic":"t"}"#),
            Ok(Frame::Other)
        );
        assert!(parse_frame("not json").is_err());
        assert!(parse_frame(r#"{"event":"message","message":"x"}"#).is_err());
    }
}
