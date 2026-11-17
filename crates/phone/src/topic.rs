use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::random::random_bytes;

const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const PREFIX: &str = "drostje-";
const RANDOM_CHARS: usize = 32;
const MAX_LEN: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Topic(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("Topic names are 1 to 64 letters, digits, '-' or '_'")]
pub struct InvalidTopic;

impl Topic {
    pub fn parse(name: &str) -> Result<Self, InvalidTopic> {
        let valid = !name.is_empty()
            && name.len() <= MAX_LEN
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if valid {
            Ok(Self(name.to_owned()))
        } else {
            Err(InvalidTopic)
        }
    }

    pub fn random() -> Self {
        let mut name = String::with_capacity(PREFIX.len() + RANDOM_CHARS);
        name.push_str(PREFIX);
        while name.len() < PREFIX.len() + RANDOM_CHARS {
            for byte in random_bytes::<64>() {
                let limit = 256 - 256 % ALPHABET.len();
                if (byte as usize) < limit && name.len() < PREFIX.len() + RANDOM_CHARS {
                    name.push(ALPHABET[byte as usize % ALPHABET.len()] as char);
                }
            }
        }
        Self(name)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Topic {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Topic::parse(&name).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_topics_are_valid_unguessable_and_distinct() {
        let first = Topic::random();
        let second = Topic::random();
        assert_ne!(first, second);
        assert_eq!(first.as_str().len(), 40);
        assert!(first.as_str().starts_with("drostje-"));
        assert_eq!(Topic::parse(first.as_str()), Ok(first));
    }

    #[test]
    fn names_outside_the_ntfy_alphabet_are_refused() {
        for name in ["", "a/b", "a b", "ä", &"x".repeat(65), "a,b"] {
            assert_eq!(Topic::parse(name), Err(InvalidTopic), "{name}");
        }
        assert!(Topic::parse("Drostje_test-1").is_ok());
        assert!(serde_json::from_str::<Topic>("\"a,b\"").is_err());
    }
}
