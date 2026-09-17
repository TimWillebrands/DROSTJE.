//! Conversation titles from a local Ollama server.
//!
//! Ollama is optional: an unreachable server or an empty model list means "no title", not an
//! error. Only this module knows Ollama's HTTP API. Calls block; run them off the GPUI thread.

use crate::model::{Thread, ThreadKind};
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::{
    io::{Read as _, Write as _},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    time::Duration,
};

/// Placeholder title of a Conversation that has not been named yet.
pub const DEFAULT_CONVERSATION_TITLE: &str = "New conversation";
/// Settings key holding the Ollama model name; blank or absent means the first installed model.
pub const MODEL_SETTING: &str = "title_model";

/// Settings key holding the selected title generator.
pub const PROVIDER_SETTING: &str = "title_provider";

/// The default attempts llama.cpp before falling back to Ollama.
pub const DEFAULT_PROVIDER: TitleProvider = TitleProvider::Automatic;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleProvider {
    /// Try llama.cpp first, then Ollama when its server is unavailable.
    Automatic,
    LlamaCpp,
    Ollama,
}

const DEFAULT_OLLAMA_HOST: &str = "127.0.0.1:11434";
const DEFAULT_LLAMA_CPP_HOST: &str = "127.0.0.1:8080";
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// Generation includes a cold model load.
const GENERATE_TIMEOUT: Duration = Duration::from_secs(120);
const RESPONSE_LIMIT: usize = 1024 * 1024;
const PROMPT_LIMIT: usize = 4000;
const TITLE_LIMIT: usize = 60;

const SYSTEM_PROMPT: &str = "You name coding conversations. Reply with one short, evocative \
title of two to five words that captures the user's requests. No quotes, no trailing punctuation, \
no explanation.";

/// Whether an explicitly requested title may replace this thread's title, whatever it is.
pub fn accepts_regenerated_title(thread: &Thread) -> bool {
    thread.kind == ThreadKind::Conversation && !thread.archived
}

/// Whether an automatic title may replace this thread's title: only the placeholder, so a
/// harness or user rename always wins.
pub fn accepts_generated_title(thread: &Thread) -> bool {
    accepts_regenerated_title(thread) && thread.title == DEFAULT_CONVERSATION_TITLE
}

/// Joins a Conversation's user prompts, oldest first, into one generation prompt.
pub fn conversation_prompt<'a>(prompts: impl IntoIterator<Item = &'a str>) -> String {
    prompts
        .into_iter()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Server address from `OLLAMA_HOST` (as the Ollama CLI reads it), else the default port.
pub fn ollama_host() -> String {
    std::env::var("OLLAMA_HOST")
        .ok()
        .map(|host| normalize_host(&host, 11434))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| DEFAULT_OLLAMA_HOST.into())
}

/// Server address from `LLAMA_CPP_HOST`, else llama.cpp's default server port.
pub fn llama_cpp_host() -> String {
    std::env::var("LLAMA_CPP_HOST")
        .ok()
        .map(|host| normalize_host(&host, 8080))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| DEFAULT_LLAMA_CPP_HOST.into())
}

fn normalize_host(raw: &str, default_port: u16) -> String {
    let host = raw.trim();
    let host = host
        .strip_prefix("http://")
        .or_else(|| host.strip_prefix("https://"))
        .unwrap_or(host)
        .trim_end_matches('/');
    if host.is_empty() {
        return String::new();
    }
    let host = if host.starts_with('[') {
        if host.contains("]:") {
            host.to_string()
        } else {
            format!("{host}:{default_port}")
        }
    } else if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:{default_port}")
    };
    // A server bound to every interface is reached through loopback.
    match host.strip_prefix("0.0.0.0:") {
        Some(port) => format!("127.0.0.1:{port}"),
        None => host,
    }
}

/// Generates a title for `prompt` with the selected local provider.
pub fn generate_title(
    provider: TitleProvider,
    model: Option<&str>,
    prompt: &str,
) -> Result<Option<String>> {
    generate_title_with_hosts(provider, &llama_cpp_host(), &ollama_host(), model, prompt)
}

/// Automatic mode prefers llama.cpp and falls back to Ollama only when the llama.cpp server is
/// unavailable.
fn generate_title_with_hosts(
    provider: TitleProvider,
    llama_cpp_host: &str,
    ollama_host: &str,
    model: Option<&str>,
    prompt: &str,
) -> Result<Option<String>> {
    match provider {
        TitleProvider::Automatic => generate_llama_cpp_title(llama_cpp_host, prompt)?.map_or_else(
            || generate_ollama_title(ollama_host, model, prompt),
            |title| Ok(Some(title)),
        ),
        TitleProvider::LlamaCpp => generate_llama_cpp_title(llama_cpp_host, prompt),
        TitleProvider::Ollama => generate_ollama_title(ollama_host, model, prompt),
    }
}

fn generate_ollama_title(host: &str, model: Option<&str>, prompt: &str) -> Result<Option<String>> {
    let Some(address) = resolve(host) else {
        return Ok(None);
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) else {
        return Ok(None);
    };
    let model = match model.map(str::trim).filter(|model| !model.is_empty()) {
        Some(model) => model.to_string(),
        None => {
            let tags = request(&mut stream, host, "GET", "/api/tags", None, LIST_TIMEOUT)
                .context("Could not list Ollama models")?;
            let Some(model) = first_model(&tags) else {
                return Ok(None);
            };
            stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
                .context("Ollama closed before generating a title")?;
            model
        }
    };
    let body = json!({
        "model": model,
        "system": SYSTEM_PROMPT,
        "prompt": truncate(prompt.trim(), PROMPT_LIMIT),
        "stream": false,
        "options": {"temperature": 0.7},
    });
    let response = request(
        &mut stream,
        host,
        "POST",
        "/api/generate",
        Some(&body),
        GENERATE_TIMEOUT,
    )
    .with_context(|| format!("Ollama model {model} could not generate a title"))?;
    Ok(response
        .get("response")
        .and_then(Value::as_str)
        .and_then(clean_title))
}

fn resolve(host: &str) -> Option<SocketAddr> {
    host.to_socket_addrs().ok()?.next()
}

fn request(
    stream: &mut TcpStream,
    host: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> Result<Value> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let body = body.map(Value::to_string).unwrap_or_default();
    // HTTP/1.0 keeps the response unchunked and closes the connection after it.
    let head = format!(
        "{method} {path} HTTP/1.0\r\nHost: {host}\r\nAccept: application/json\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;
    let mut raw = Vec::new();
    stream
        .take(RESPONSE_LIMIT as u64 + 1)
        .read_to_end(&mut raw)?;
    anyhow::ensure!(
        raw.len() <= RESPONSE_LIMIT,
        "Title server response exceeds 1 MiB"
    );
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Result<Value> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("Title server sent an incomplete HTTP response")?;
    let head =
        std::str::from_utf8(&raw[..split]).context("Title server sent invalid HTTP headers")?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .context("Title server sent no HTTP status")?;
    let body: Value =
        serde_json::from_slice(&raw[split + 4..]).context("Title server sent a non-JSON body")?;
    if !(200..300).contains(&status) {
        let message = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("no error message");
        anyhow::bail!("HTTP {status}: {message}");
    }
    Ok(body)
}

fn first_model(tags: &Value) -> Option<String> {
    tags.get("models")?
        .as_array()?
        .iter()
        .find_map(|model| model.get("name").and_then(Value::as_str))
        .map(str::to_owned)
}

fn truncate(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((ix, _)) => &text[..ix],
        None => text,
    }
}

/// Reduces model output to a single-line title, or `None` when nothing usable remains.
fn clean_title(output: &str) -> Option<String> {
    let output = match output.rfind("</think>") {
        Some(ix) => &output[ix + "</think>".len()..],
        None => output,
    };
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let line = line
        .strip_prefix("Title:")
        .or_else(|| line.strip_prefix("title:"))
        .unwrap_or(line);
    let quotes: &[char] = &['"', '\'', '`', '*', '#', '“', '”', '‘', '’'];
    let line = line
        .trim()
        .trim_matches(quotes)
        .trim()
        .trim_end_matches(['.', ':', ';', ','])
        .trim();
    let words = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = truncate(&words, TITLE_LIMIT).trim_end().to_string();
    (!title.is_empty()).then_some(title)
}

fn generate_llama_cpp_title(host: &str, prompt: &str) -> Result<Option<String>> {
    let Some(address) = resolve(host) else {
        return Ok(None);
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) else {
        return Ok(None);
    };
    let body = json!({
        "prompt": format!("{SYSTEM_PROMPT}\n\nUser requests:\n{}", truncate(prompt.trim(), PROMPT_LIMIT)),
        "n_predict": 24,
        "temperature": 0.7,
        "stream": false,
    });
    let response = request(
        &mut stream,
        host,
        "POST",
        "/completion",
        Some(&body),
        GENERATE_TIMEOUT,
    )
    .context("llama.cpp could not generate a title")?;
    Ok(response
        .get("content")
        .and_then(Value::as_str)
        .and_then(clean_title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serves `responses` in order, one per connection, and returns the received requests.
    fn serve(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            responses
                .into_iter()
                .map(|response| {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut received = Vec::new();
                    let mut buffer = [0; 4096];
                    loop {
                        let read = stream.read(&mut buffer).unwrap();
                        received.extend_from_slice(&buffer[..read]);
                        let text = String::from_utf8_lossy(&received);
                        if let Some(split) = text.find("\r\n\r\n") {
                            let length = text[..split]
                                .lines()
                                .find_map(|line| line.strip_prefix("Content-Length: "))
                                .and_then(|value| value.parse::<usize>().ok())
                                .unwrap_or(0);
                            if received.len() >= split + 4 + length {
                                break;
                            }
                        }
                    }
                    stream.write_all(response.as_bytes()).unwrap();
                    String::from_utf8(received).unwrap()
                })
                .collect()
        });
        (host, handle)
    }

    fn ok(body: Value) -> String {
        format!("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{body}")
    }

    #[test]
    fn first_listed_model_names_the_conversation() {
        let (host, server) = serve(vec![
            ok(json!({"models": [{"name": "lfm2:24b"}, {"name": "llama3:8b"}]})),
            ok(json!({"response": "\"Taming the Borrow Checker.\"\n"})),
        ]);
        let title = generate_ollama_title(&host, None, "Fix my lifetimes").unwrap();
        assert_eq!(title.as_deref(), Some("Taming the Borrow Checker"));
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /api/tags HTTP/1.0"));
        assert!(requests[1].starts_with("POST /api/generate HTTP/1.0"));
        let body: Value =
            serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["model"], "lfm2:24b");
        assert_eq!(body["prompt"], "Fix my lifetimes");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn configured_model_skips_listing() {
        let (host, server) = serve(vec![ok(json!({"response": "Sparks in the Parser"}))]);
        let title = generate_ollama_title(&host, Some("llama3:8b"), "Parse this").unwrap();
        assert_eq!(title.as_deref(), Some("Sparks in the Parser"));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("\"model\":\"llama3:8b\""));
    }

    #[test]
    fn no_models_means_no_title() {
        let (host, server) = serve(vec![ok(json!({"models": []}))]);
        assert_eq!(generate_ollama_title(&host, None, "Hello").unwrap(), None);
        server.join().unwrap();
    }

    #[test]
    fn unreachable_server_means_no_title() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let host = listener.local_addr().unwrap().to_string();
        drop(listener);
        assert_eq!(generate_ollama_title(&host, None, "Hello").unwrap(), None);
    }

    #[test]
    fn server_errors_are_reported() {
        let (host, server) = serve(vec![format!(
            "HTTP/1.0 404 Not Found\r\n\r\n{}",
            json!({"error": "model \"missing\" not found"})
        )]);
        let error = generate_ollama_title(&host, Some("missing"), "Hello").unwrap_err();
        assert!(format!("{error:#}").contains("model \"missing\" not found"));
        server.join().unwrap();
    }

    #[test]
    fn automatic_prefers_llama_cpp() {
        let (llama_cpp_host, llama_cpp) =
            serve(vec![ok(json!({"content": "Focused Parser Repair"}))]);
        let unused_ollama = TcpListener::bind("127.0.0.1:0").unwrap();
        let ollama_host = unused_ollama.local_addr().unwrap().to_string();
        drop(unused_ollama);

        let title = generate_title_with_hosts(
            TitleProvider::Automatic,
            &llama_cpp_host,
            &ollama_host,
            None,
            "Fix parser recovery",
        )
        .unwrap();

        assert_eq!(title.as_deref(), Some("Focused Parser Repair"));
        let requests = llama_cpp.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("POST /completion HTTP/1.0"));
    }

    #[test]
    fn titles_are_single_line_and_bounded() {
        assert_eq!(
            clean_title("<think>hmm</think>\n\nTitle: **Midnight   Migration**\nextra"),
            Some("Midnight Migration".into())
        );
        assert_eq!(clean_title("  \n\"\" \n"), None);
        assert_eq!(
            clean_title(&"word ".repeat(40)).unwrap().chars().count(),
            59
        );
    }

    /// Opt-in: needs a running local Ollama with at least one model.
    #[test]
    #[ignore]
    fn local_ollama_names_a_prompt() {
        let title = generate_title(
            TitleProvider::Ollama,
            None,
            "The sidebar flickers when I resize the window; find out why",
        )
        .unwrap();
        eprintln!("title: {title:?}");
        assert!(title.is_some());
    }

    #[test]
    fn only_placeholder_conversations_accept_titles() {
        let mut thread: Thread = serde_json::from_value(json!({
            "id": uuid::Uuid::nil(), "project_id": uuid::Uuid::nil(), "kind": "conversation",
            "factory": null, "title": DEFAULT_CONVERSATION_TITLE, "harness": "omp",
            "session_ref": null, "model": null, "draft": "", "attachments": [],
            "submission": null, "archived": false, "created_at": "", "updated_at": "",
        }))
        .unwrap();
        assert!(accepts_generated_title(&thread));
        thread.title = "Renamed by user".into();
        assert!(!accepts_generated_title(&thread));
        assert!(accepts_regenerated_title(&thread));
        thread.title = DEFAULT_CONVERSATION_TITLE.into();
        thread.archived = true;
        assert!(!accepts_generated_title(&thread));
        assert!(!accepts_regenerated_title(&thread));
        thread.archived = false;
        thread.kind = ThreadKind::Factory;
        assert!(!accepts_generated_title(&thread));
    }

    #[test]
    fn conversation_prompt_keeps_order_and_skips_blanks() {
        assert_eq!(
            conversation_prompt(["Fix the parser", "  ", " Then add tests "]),
            "Fix the parser\n\nThen add tests"
        );
    }

    #[test]
    fn hosts_follow_server_conventions() {
        assert_eq!(normalize_host("0.0.0.0", 11434), "127.0.0.1:11434");
        assert_eq!(
            normalize_host("http://example:8080/", 11434),
            "example:8080"
        );
        assert_eq!(normalize_host("[::1]", 8080), "[::1]:8080");
    }
}
