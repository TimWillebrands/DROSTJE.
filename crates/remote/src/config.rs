use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteHostConfig {
    pub id: String,
    pub name: String,
    pub relay_url: String,
    pub token_env: String,
    #[serde(default)]
    pub projects: BTreeMap<String, String>,
}

impl RemoteHostConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.id.trim().is_empty(), "Remote Host ID is required");
        ensure!(!self.name.trim().is_empty(), "Remote Host name is required");
        validate_relay_url(&self.relay_url)?;
        ensure!(
            valid_environment_name(&self.token_env),
            "Remote Host token_env must name an environment variable"
        );
        ensure!(
            self.projects.values().all(|key| !key.trim().is_empty()),
            "Remote project keys cannot be empty"
        );
        Ok(())
    }

    pub fn token(&self) -> Result<String> {
        self.validate()?;
        let token = std::env::var(&self.token_env).with_context(|| {
            format!(
                "Remote Host credential environment variable {} is unavailable",
                self.token_env
            )
        })?;
        ensure!(
            !token.is_empty() && !token.chars().any(char::is_control),
            "Remote Host credential is empty or invalid"
        );
        Ok(token)
    }
}

pub(crate) fn validate_relay_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).context("Invalid remote relay URL")?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Relay credentials must not appear in the URL"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "Relay URL must not contain query credentials or fragments"
    );
    let local = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    ensure!(
        url.scheme() == "wss" || (url.scheme() == "ws" && local),
        "Remote relay requires wss TLS (ws is allowed only on loopback)"
    );
    ensure!(url.host_str().is_some(), "Remote relay host is required");
    Ok(url)
}

fn valid_environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_references_only() {
        let config = RemoteHostConfig {
            id: "docker".into(),
            name: "Docker".into(),
            relay_url: "wss://relay.example/remote".into(),
            token_env: "DROSTJE_REMOTE_TOKEN".into(),
            projects: BTreeMap::new(),
        };
        config.validate().unwrap();
        let saved = serde_json::to_value(config).unwrap();
        assert_eq!(saved["token_env"], "DROSTJE_REMOTE_TOKEN");
        assert!(saved.get("token").is_none());
        assert!(!valid_environment_name("TOKEN=secret"));
    }

    #[test]
    fn relay_rejects_plaintext_and_url_secrets() {
        for url in [
            "ws://relay.example/remote",
            "wss://token@relay.example/remote",
            "wss://relay.example/remote?token=x",
            "https://relay.example",
        ] {
            assert!(validate_relay_url(url).is_err(), "{url}");
        }
        assert!(validate_relay_url("ws://127.0.0.1:9000/remote").is_ok());
        assert!(validate_relay_url("wss://relay.example/remote").is_ok());
    }
}
