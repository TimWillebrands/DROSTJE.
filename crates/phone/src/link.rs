use std::time::Duration;

use drostje_ntfy::{Message, Server};
use serde::{Deserialize, Serialize};

use crate::{LinkKey, Topic, messages::Received};

pub(crate) const TICK: Duration = Duration::from_millis(50);
pub const PAIRING_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRecord {
    pub server: String,
    pub down: Topic,
    pub up: Topic,
    pub key: LinkKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Connected,
    Problem(String),
}

pub(crate) enum Body {
    Text(String),
    Bytes(Vec<u8>),
}

impl Body {
    pub(crate) fn received(&self) -> Received<'_> {
        match self {
            Self::Text(text) => Received::Text(text),
            Self::Bytes(bytes) => Received::Attachment(bytes),
        }
    }
}

pub(crate) fn body(server: &Server, message: &Message) -> Result<Body, String> {
    match &message.attachment {
        Some(attachment) => server
            .download(attachment, crate::messages::ATTACHMENT_LIMIT)
            .map(Body::Bytes)
            .map_err(|error| error.to_string()),
        None => Ok(Body::Text(message.body.clone())),
    }
}

pub(crate) fn publish(
    server: &Server,
    topic: &Topic,
    sealed: crate::messages::Sealed,
) -> Result<drostje_ntfy::Published, drostje_ntfy::PublishError> {
    match sealed {
        crate::messages::Sealed::Text(text) => server.publish_text(topic.as_str(), &text),
        crate::messages::Sealed::Attachment(bytes) => {
            server.publish_attachment(topic.as_str(), &bytes)
        }
    }
}

pub(crate) struct ConnectionState {
    subscription: Option<String>,
    publish: Option<String>,
    connected: bool,
    reported: Option<Connection>,
}

impl ConnectionState {
    pub(crate) fn new() -> Self {
        Self {
            subscription: None,
            publish: None,
            connected: false,
            reported: None,
        }
    }

    pub(crate) fn subscribed(&mut self) {
        self.connected = true;
        self.subscription = None;
    }

    pub(crate) fn is_subscribed(&self) -> bool {
        self.connected
    }

    pub(crate) fn subscription_lost(&mut self, reason: String) {
        self.connected = false;
        self.subscription = Some(reason);
    }

    pub(crate) fn published(&mut self, result: Result<(), String>) {
        self.publish = result.err();
    }

    pub(crate) fn changed(&mut self) -> Option<Connection> {
        let current = match (&self.subscription, &self.publish) {
            (Some(problem), _) | (None, Some(problem)) => Connection::Problem(problem.clone()),
            (None, None) if self.connected => Connection::Connected,
            (None, None) => Connection::Connecting,
        };
        if self.reported.as_ref() == Some(&current) {
            return None;
        }
        self.reported = Some(current.clone());
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_reports_only_changes_and_names_the_first_problem() {
        let mut state = ConnectionState::new();
        assert_eq!(state.changed(), Some(Connection::Connecting));
        assert_eq!(state.changed(), None);
        state.subscribed();
        assert_eq!(state.changed(), Some(Connection::Connected));
        state.published(Err("Server rate limit reached".into()));
        assert_eq!(
            state.changed(),
            Some(Connection::Problem("Server rate limit reached".into()))
        );
        state.published(Ok(()));
        assert_eq!(state.changed(), Some(Connection::Connected));
        state.subscription_lost("Cannot reach server".into());
        assert_eq!(
            state.changed(),
            Some(Connection::Problem("Cannot reach server".into()))
        );
    }

    #[test]
    fn link_records_persist_their_key_as_text() {
        let record = LinkRecord {
            server: "https://ntfy.example".into(),
            down: Topic::random(),
            up: Topic::random(),
            key: LinkKey::from_bytes([5; 32]),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains(&record.key.to_text()));
        assert_eq!(serde_json::from_str::<LinkRecord>(&json).unwrap(), record);
    }
}
