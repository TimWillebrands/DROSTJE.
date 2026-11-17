use std::{io::Read, net::IpAddr, time::Duration};

use serde::Deserialize;
use url::Url;

use crate::{Attachment, Subscription, subscribe};

const PUBLISH_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const ERROR_BODY_LIMIT: u64 = 8 * 1024;
const ATTACHMENT_NAME: &str = "sealed.bin";

#[derive(Clone)]
pub struct Server {
    base: Url,
    authorization: Option<String>,
    agent: ureq::Agent,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Server")
            .field("base", &self.base.as_str())
            .field("authorized", &self.authorization.is_some())
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InvalidServer {
    #[error("Server address is not a URL")]
    NotUrl,
    #[error(
        "Server address must start with https:// (http:// only on this computer or a local network)"
    )]
    Insecure,
    #[error("Server address must not contain a user name, password, query or fragment")]
    Credentials,
    #[error("Server token must not be empty or contain control characters")]
    Token,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PublishError {
    #[error("Server rate limit reached")]
    RateLimited { retry_after: Option<Duration> },
    #[error("Server refused the message ({status}): {message}")]
    Refused { status: u16, message: String },
    #[error("Message is larger than the server accepts")]
    TooLarge,
    #[error("Server is unreachable: {0}")]
    Network(String),
    #[error("Server answered with an unreadable response: {0}")]
    Response(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    pub id: String,
}

#[derive(Deserialize)]
struct PublishedBody {
    id: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
}

fn local_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost" || domain.ends_with(".local"),
        Some(url::Host::Ipv4(ip)) => {
            let ip = IpAddr::V4(ip);
            ip.is_loopback()
                || matches!(ip, IpAddr::V4(v4) if v4.is_private() || v4.is_link_local())
        }
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unique_local(),
        None => false,
    }
}

impl Server {
    pub fn new(address: &str, token: Option<&str>) -> Result<Self, InvalidServer> {
        let mut base = Url::parse(address.trim()).map_err(|_| InvalidServer::NotUrl)?;
        if base.host().is_none() || base.cannot_be_a_base() {
            return Err(InvalidServer::NotUrl);
        }
        match base.scheme() {
            "https" => {}
            "http" if local_host(&base) => {}
            "http" => return Err(InvalidServer::Insecure),
            _ => return Err(InvalidServer::NotUrl),
        }
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(InvalidServer::Credentials);
        }
        let path = base.path().trim_end_matches('/').to_owned();
        base.set_path(&path);
        let authorization = match token.map(str::trim) {
            None => None,
            Some(token) if token.is_empty() || token.chars().any(char::is_control) => {
                return Err(InvalidServer::Token);
            }
            Some(token) => Some(format!("Bearer {token}")),
        };
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .new_agent();
        Ok(Self {
            base,
            authorization,
            agent,
        })
    }

    pub fn address(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    pub(crate) fn authorization(&self) -> Option<&str> {
        self.authorization.as_deref()
    }

    pub(crate) fn topic_url(&self, topic: &str) -> Url {
        let mut url = self.base.clone();
        let path = format!("{}/{}", self.base.path().trim_end_matches('/'), topic);
        url.set_path(&path);
        url
    }

    pub(crate) fn same_origin(&self, url: &Url) -> bool {
        url.scheme() == self.base.scheme()
            && url.host() == self.base.host()
            && url.port_or_known_default() == self.base.port_or_known_default()
    }

    pub fn publish_text(&self, topic: &str, text: &str) -> Result<Published, PublishError> {
        let mut request = self
            .agent
            .post(self.topic_url(topic).as_str())
            .config()
            .timeout_global(Some(PUBLISH_TIMEOUT))
            .build()
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("X-Cache", "yes")
            .header("X-Firebase", "no");
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        published(request.send(text))
    }

    pub fn publish_attachment(&self, topic: &str, bytes: &[u8]) -> Result<Published, PublishError> {
        let mut request = self
            .agent
            .put(self.topic_url(topic).as_str())
            .config()
            .timeout_global(Some(PUBLISH_TIMEOUT))
            .build()
            .header("Content-Type", "application/octet-stream")
            .header("X-Filename", ATTACHMENT_NAME)
            .header("X-Cache", "yes")
            .header("X-Firebase", "no");
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        published(request.send(bytes))
    }

    pub fn download(&self, attachment: &Attachment, limit: u64) -> Result<Vec<u8>, PublishError> {
        let url = Url::parse(&attachment.url)
            .map_err(|_| PublishError::Response("attachment URL is invalid".into()))?;
        if !self.same_origin(&url) {
            return Err(PublishError::Response(
                "attachment is stored on another server".into(),
            ));
        }
        if attachment.size.is_some_and(|size| size > limit) {
            return Err(PublishError::TooLarge);
        }
        let mut request = self
            .agent
            .get(url.as_str())
            .config()
            .timeout_global(Some(DOWNLOAD_TIMEOUT))
            .build();
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        let mut response = request
            .call()
            .map_err(|error| PublishError::Network(error.to_string()))?;
        check_status(&mut response)?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| PublishError::Network(error.to_string()))?;
        if bytes.len() as u64 > limit {
            return Err(PublishError::TooLarge);
        }
        Ok(bytes)
    }

    pub fn subscribe(&self, topics: &[&str], since: Option<String>) -> Subscription {
        subscribe::start(self.clone(), topics, since)
    }
}

fn retry_after(response: &ureq::http::Response<ureq::Body>) -> Option<Duration> {
    response
        .headers()
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn check_status(response: &mut ureq::http::Response<ureq::Body>) -> Result<(), PublishError> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    if status == 429 {
        return Err(PublishError::RateLimited {
            retry_after: retry_after(response),
        });
    }
    if status == 413 {
        return Err(PublishError::TooLarge);
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(ERROR_BODY_LIMIT)
        .read_to_string()
        .unwrap_or_default();
    let message = serde_json::from_str::<ErrorBody>(&text)
        .map(|body| body.error)
        .ok()
        .filter(|error| !error.is_empty())
        .unwrap_or_else(|| text.trim().chars().take(200).collect());
    Err(PublishError::Refused { status, message })
}

fn published(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<Published, PublishError> {
    let mut response = result.map_err(|error| PublishError::Network(error.to_string()))?;
    check_status(&mut response)?;
    let text = response
        .body_mut()
        .with_config()
        .limit(ERROR_BODY_LIMIT)
        .read_to_string()
        .map_err(|error| PublishError::Response(error.to_string()))?;
    let body: PublishedBody =
        serde_json::from_str(&text).map_err(|error| PublishError::Response(error.to_string()))?;
    Ok(Published { id: body.id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_servers_and_local_http_servers_are_accepted() {
        for address in [
            "https://ntfy.sh",
            "https://seintje.example.org/",
            "http://127.0.0.1:8080",
            "http://localhost:2586",
            "http://192.168.1.20",
            "http://10.0.2.2:2586",
            "http://desk.local",
        ] {
            assert!(Server::new(address, None).is_ok(), "{address}");
        }
    }

    #[test]
    fn insecure_or_credential_bearing_addresses_are_refused() {
        assert_eq!(
            Server::new("http://ntfy.sh", None).unwrap_err(),
            InvalidServer::Insecure
        );
        assert_eq!(
            Server::new("https://u:p@ntfy.sh", None).unwrap_err(),
            InvalidServer::Credentials
        );
        assert_eq!(
            Server::new("https://ntfy.sh?auth=x", None).unwrap_err(),
            InvalidServer::Credentials
        );
        assert_eq!(
            Server::new("ntfy.sh", None).unwrap_err(),
            InvalidServer::NotUrl
        );
        assert_eq!(
            Server::new("wss://ntfy.sh", None).unwrap_err(),
            InvalidServer::NotUrl
        );
        assert_eq!(
            Server::new("https://ntfy.sh", Some(" ")).unwrap_err(),
            InvalidServer::Token
        );
    }

    #[test]
    fn topics_live_under_the_server_path_and_attachments_must_share_its_origin() {
        let server = Server::new("https://example.org/ntfy/", Some("tk_1")).unwrap();
        assert_eq!(server.address(), "https://example.org/ntfy");
        assert_eq!(server.topic_url("t").as_str(), "https://example.org/ntfy/t");
        assert!(server.same_origin(&Url::parse("https://example.org/file/x.bin").unwrap()));
        assert!(!server.same_origin(&Url::parse("https://evil.example/file/x.bin").unwrap()));
        assert!(!server.same_origin(&Url::parse("http://example.org/file/x.bin").unwrap()));
        assert!(!format!("{server:?}").contains("tk_1"));
    }
}
