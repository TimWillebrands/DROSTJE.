use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::{
    random::random_bytes,
    seal::{LinkKey, OpenError},
    topic::Topic,
};

const SCHEME: &str = "drostje";
const VERSION: &str = "1";
const LINK_INFO: &[u8] = b"drostje phone link v1";
const CHECK_INFO: &[u8] = b"drostje phone check v1";
pub const MAX_PHONE_NAME: usize = 60;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PairingError {
    #[error("This QR code is not a DROSTJE pairing code")]
    NotAPairingLink,
    #[error("This pairing code is from a newer DROSTJE; update the app")]
    UnsupportedVersion,
    #[error("The pairing code is damaged: {0}")]
    Damaged(&'static str),
    #[error("The phone's reply was not made from this pairing code")]
    Forged,
    #[error(transparent)]
    Open(#[from] OpenError),
}

#[derive(Clone, PartialEq, Eq)]
pub struct PairingOffer {
    pub server: String,
    pub topic: Topic,
    pub desktop_key: [u8; 32],
    pub code: [u8; 32],
    pub token: Option<String>,
}

impl std::fmt::Debug for PairingOffer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingOffer")
            .field("server", &self.server)
            .field("topic", &self.topic)
            .finish_non_exhaustive()
    }
}

impl PairingOffer {
    pub fn to_link(&self) -> String {
        let mut url = url::Url::parse(&format!("{SCHEME}://pair")).expect("static link base");
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("v", VERSION)
                .append_pair("s", &self.server)
                .append_pair("t", self.topic.as_str())
                .append_pair("k", &URL_SAFE_NO_PAD.encode(self.desktop_key))
                .append_pair("c", &URL_SAFE_NO_PAD.encode(self.code));
            if let Some(token) = &self.token {
                query.append_pair("a", token);
            }
        }
        url.to_string()
    }

    pub fn parse(link: &str) -> Result<Self, PairingError> {
        let url = url::Url::parse(link.trim()).map_err(|_| PairingError::NotAPairingLink)?;
        if url.scheme() != SCHEME || url.host_str() != Some("pair") {
            return Err(PairingError::NotAPairingLink);
        }
        let field = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        if field("v").as_deref() != Some(VERSION) {
            return Err(PairingError::UnsupportedVersion);
        }
        let server = field("s").ok_or(PairingError::Damaged("server is missing"))?;
        let topic = field("t")
            .and_then(|topic| Topic::parse(&topic).ok())
            .ok_or(PairingError::Damaged("topic is missing or invalid"))?;
        let key = |name: &str, what: &'static str| -> Result<[u8; 32], PairingError> {
            let text = field(name).ok_or(PairingError::Damaged(what))?;
            let bytes = URL_SAFE_NO_PAD
                .decode(text)
                .map_err(|_| PairingError::Damaged(what))?;
            bytes.try_into().map_err(|_| PairingError::Damaged(what))
        };
        Ok(Self {
            server,
            topic,
            desktop_key: key("k", "desktop key is missing or invalid")?,
            code: key("c", "secret is missing or invalid")?,
            token: field("a").filter(|token| !token.is_empty()),
        })
    }

    fn hello_context(&self) -> String {
        format!("drostje-pair-hello:{}", self.topic)
    }

    fn reply_context(&self) -> String {
        format!("drostje-pair-reply:{}", self.topic)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CheckCode(u32);

impl std::fmt::Display for CheckCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:03} {:03}", self.0 / 1000, self.0 % 1000)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub phone: String,
    pub sealed: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HelloBody {
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairReply {
    Accepted {
        desktop: String,
        down: Topic,
        up: Topic,
    },
    Declined,
}

#[derive(Debug)]
pub struct Handshake {
    pub key: LinkKey,
    pub check: CheckCode,
}

fn derive(
    secret: &StaticSecret,
    peer: &PublicKey,
    desktop: &[u8; 32],
    phone: &[u8; 32],
    code: &[u8; 32],
) -> Result<Handshake, PairingError> {
    let shared = secret.diffie_hellman(peer);
    if !shared.was_contributory() {
        return Err(PairingError::Forged);
    }
    let mut input = Vec::with_capacity(96);
    input.extend_from_slice(shared.as_bytes());
    input.extend_from_slice(desktop);
    input.extend_from_slice(phone);
    let hkdf = Hkdf::<Sha256>::new(Some(code), &input);
    let mut key = [0u8; 32];
    hkdf.expand(LINK_INFO, &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    let mut check = [0u8; 4];
    hkdf.expand(CHECK_INFO, &mut check)
        .expect("4 bytes is a valid HKDF-SHA256 output length");
    Ok(Handshake {
        key: LinkKey::from_bytes(key),
        check: CheckCode(u32::from_be_bytes(check) % 1_000_000),
    })
}

pub fn clean_phone_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_PHONE_NAME)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "Phone".into()
    } else {
        trimmed.into()
    }
}

pub struct DesktopPairing {
    secret: StaticSecret,
    pub offer: PairingOffer,
}

#[derive(Debug)]
pub struct IncomingPhone {
    pub name: String,
    pub handshake: Handshake,
}

impl DesktopPairing {
    pub fn new(server: String, token: Option<String>) -> Self {
        let secret = StaticSecret::from(random_bytes::<32>());
        let desktop_key = PublicKey::from(&secret).to_bytes();
        Self {
            secret,
            offer: PairingOffer {
                server,
                topic: Topic::random(),
                desktop_key,
                code: random_bytes::<32>(),
                token,
            },
        }
    }

    pub fn open_hello(&self, hello: &Hello) -> Result<IncomingPhone, PairingError> {
        let phone: [u8; 32] = URL_SAFE_NO_PAD
            .decode(&hello.phone)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(PairingError::Forged)?;
        let handshake = derive(
            &self.secret,
            &PublicKey::from(phone),
            &self.offer.desktop_key,
            &phone,
            &self.offer.code,
        )?;
        let sealed = URL_SAFE_NO_PAD
            .decode(&hello.sealed)
            .map_err(|_| PairingError::Forged)?;
        let body: HelloBody = handshake
            .key
            .open(&self.offer.hello_context(), &sealed)
            .map_err(|_| PairingError::Forged)?;
        Ok(IncomingPhone {
            name: clean_phone_name(&body.name),
            handshake,
        })
    }

    pub fn seal_reply(&self, key: &LinkKey, reply: &PairReply) -> String {
        URL_SAFE_NO_PAD.encode(key.seal(&self.offer.reply_context(), reply))
    }
}

pub fn phone_hello(offer: &PairingOffer, name: &str) -> Result<(Hello, Handshake), PairingError> {
    let secret = StaticSecret::from(random_bytes::<32>());
    let phone = PublicKey::from(&secret).to_bytes();
    let handshake = derive(
        &secret,
        &PublicKey::from(offer.desktop_key),
        &offer.desktop_key,
        &phone,
        &offer.code,
    )?;
    let body = HelloBody {
        name: clean_phone_name(name),
    };
    let hello = Hello {
        phone: URL_SAFE_NO_PAD.encode(phone),
        sealed: URL_SAFE_NO_PAD.encode(handshake.key.seal(&offer.hello_context(), &body)),
    };
    Ok((hello, handshake))
}

pub fn open_reply(
    offer: &PairingOffer,
    key: &LinkKey,
    text: &str,
) -> Result<PairReply, PairingError> {
    let sealed = URL_SAFE_NO_PAD
        .decode(text.trim())
        .map_err(|_| PairingError::Open(OpenError::Encoding))?;
    Ok(key.open(&offer.reply_context(), &sealed)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairing() -> DesktopPairing {
        DesktopPairing::new("https://seintje.example.org".into(), None)
    }

    #[test]
    fn the_link_in_the_qr_code_roundtrips() {
        let mut desktop = pairing();
        desktop.offer.token = Some("tk_abc".into());
        let link = desktop.offer.to_link();
        assert!(link.starts_with("drostje://pair?v=1&s=https%3A%2F%2Fseintje"));
        assert_eq!(PairingOffer::parse(&link).unwrap(), desktop.offer);
        assert!(!format!("{:?}", desktop.offer).contains("tk_abc"));
    }

    #[test]
    fn other_qr_codes_are_named_as_such() {
        assert_eq!(
            PairingOffer::parse("https://example.org"),
            Err(PairingError::NotAPairingLink)
        );
        assert_eq!(
            PairingOffer::parse("WIFI:S:home;;"),
            Err(PairingError::NotAPairingLink)
        );
        assert_eq!(
            PairingOffer::parse("drostje://pair?v=2"),
            Err(PairingError::UnsupportedVersion)
        );
        let link = pairing().offer.to_link();
        let damaged = link.replace("&c=", "&c=x");
        assert!(matches!(
            PairingOffer::parse(&damaged),
            Err(PairingError::Damaged(_))
        ));
    }

    #[test]
    fn both_sides_derive_the_same_key_and_check_code() {
        let desktop = pairing();
        let offer = PairingOffer::parse(&desktop.offer.to_link()).unwrap();
        let (hello, phone) = phone_hello(&offer, "Pixel 9\u{7}").unwrap();
        let incoming = desktop.open_hello(&hello).unwrap();
        assert_eq!(incoming.name, "Pixel 9");
        assert_eq!(incoming.handshake.key, phone.key);
        assert_eq!(incoming.handshake.check, phone.check);
        assert_eq!(phone.check.to_string().len(), 7);
        let reply = PairReply::Accepted {
            desktop: "Mac".into(),
            down: Topic::random(),
            up: Topic::random(),
        };
        let sealed = desktop.seal_reply(&incoming.handshake.key, &reply);
        assert_eq!(open_reply(&offer, &phone.key, &sealed).unwrap(), reply);
    }

    #[test]
    fn a_hello_without_the_secret_from_the_qr_code_is_refused() {
        let desktop = pairing();
        let mut guessed = desktop.offer.clone();
        guessed.code = [0; 32];
        let (hello, _) = phone_hello(&guessed, "Attacker").unwrap();
        assert_eq!(
            desktop.open_hello(&hello).unwrap_err(),
            PairingError::Forged
        );
        let other = pairing();
        let (hello, _) = phone_hello(&other.offer, "Other").unwrap();
        assert_eq!(
            desktop.open_hello(&hello).unwrap_err(),
            PairingError::Forged
        );
        let low_order = Hello {
            phone: URL_SAFE_NO_PAD.encode([0u8; 32]),
            sealed: String::new(),
        };
        assert_eq!(
            desktop.open_hello(&low_order).unwrap_err(),
            PairingError::Forged
        );
    }

    #[test]
    fn empty_or_long_phone_names_are_cleaned() {
        assert_eq!(clean_phone_name("  \n "), "Phone");
        assert_eq!(clean_phone_name(&"a".repeat(200)).len(), MAX_PHONE_NAME);
    }
}
