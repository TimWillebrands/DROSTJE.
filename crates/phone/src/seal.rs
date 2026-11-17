use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::random::random_bytes;

const NONCE: usize = 24;
const TAG: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub struct LinkKey([u8; 32]);

impl std::fmt::Debug for LinkKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LinkKey(..)")
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OpenError {
    #[error("Message is not encoded for this phone link")]
    Encoding,
    #[error("Message was not sealed with this phone link's key")]
    Forged,
    #[error("Sealed message has an unexpected shape: {0}")]
    Shape(String),
}

impl serde::Serialize for LinkKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_text())
    }
}

impl<'de> serde::Deserialize<'de> for LinkKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_text(&text).map_err(serde::de::Error::custom)
    }
}

impl LinkKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    pub fn to_text(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    pub fn from_text(text: &str) -> Result<Self, OpenError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| OpenError::Encoding)?;
        Ok(Self(bytes.try_into().map_err(|_| OpenError::Encoding)?))
    }

    pub fn seal_bytes(&self, context: &str, plaintext: &[u8]) -> Vec<u8> {
        let nonce = random_bytes::<NONCE>();
        let cipher = XChaCha20Poly1305::new(&self.0.into());
        let sealed = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext,
                    aad: context.as_bytes(),
                },
            )
            .expect("XChaCha20-Poly1305 encryption cannot fail for in-memory buffers");
        let mut out = Vec::with_capacity(NONCE + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        out
    }

    pub fn open_bytes(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, OpenError> {
        if sealed.len() < NONCE + TAG {
            return Err(OpenError::Encoding);
        }
        let (nonce, body) = sealed.split_at(NONCE);
        let nonce: [u8; NONCE] = nonce.try_into().map_err(|_| OpenError::Encoding)?;
        XChaCha20Poly1305::new(&self.0.into())
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: body,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| OpenError::Forged)
    }

    pub fn seal_text(&self, context: &str, plaintext: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(self.seal_bytes(context, plaintext))
    }

    pub fn open_text(&self, context: &str, sealed: &str) -> Result<Vec<u8>, OpenError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(sealed.trim())
            .map_err(|_| OpenError::Encoding)?;
        self.open_bytes(context, &bytes)
    }

    pub fn seal<T: Serialize>(&self, context: &str, value: &T) -> Vec<u8> {
        let json = serde_json::to_vec(value).expect("phone link messages always serialize");
        self.seal_bytes(context, &json)
    }

    pub fn open<T: DeserializeOwned>(&self, context: &str, sealed: &[u8]) -> Result<T, OpenError> {
        let json = self.open_bytes(context, sealed)?;
        serde_json::from_slice(&json).map_err(|error| OpenError::Shape(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> LinkKey {
        LinkKey::from_bytes([byte; 32])
    }

    #[test]
    fn sealed_text_opens_only_with_the_same_key_and_context() {
        let sealed = key(1).seal_text("down:t", b"hello");
        assert_eq!(key(1).open_text("down:t", &sealed).unwrap(), b"hello");
        assert_eq!(key(2).open_text("down:t", &sealed), Err(OpenError::Forged));
        assert_eq!(key(1).open_text("up:t", &sealed), Err(OpenError::Forged));
        assert_eq!(sealed.len(), (NONCE + TAG + 5).div_ceil(3) * 4);
    }

    #[test]
    fn tampered_or_truncated_messages_are_refused() {
        let mut sealed = key(1).seal_bytes("c", b"payload");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert_eq!(key(1).open_bytes("c", &sealed), Err(OpenError::Forged));
        assert_eq!(
            key(1).open_bytes("c", &sealed[..10]),
            Err(OpenError::Encoding)
        );
        assert_eq!(
            key(1).open_text("c", "not base64!"),
            Err(OpenError::Encoding)
        );
    }

    #[test]
    fn the_same_plaintext_seals_differently_each_time() {
        assert_ne!(key(1).seal_text("c", b"x"), key(1).seal_text("c", b"x"));
    }

    #[test]
    fn keys_roundtrip_as_text_without_showing_in_debug_output() {
        let original = key(7);
        assert_eq!(LinkKey::from_text(&original.to_text()).unwrap(), original);
        assert_eq!(format!("{original:?}"), "LinkKey(..)");
        assert!(LinkKey::from_text("short").is_err());
    }
}
