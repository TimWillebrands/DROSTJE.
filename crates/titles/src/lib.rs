//! Conversation titles and commit subjects from the local llama.cpp CLI or an optional Ollama
//! server.
//!
//! A missing llama executable, unreachable Ollama server, or empty Ollama model list means "no
//! text", not an error. Calls block; run them off the GPUI thread.
//!
//! [`installed_models`] lists what each provider can use without a download, for Settings.

use anyhow::{Context as _, Result};
use drostje_model::{Thread, ThreadKind};
use drostje_platform as platform;
use serde_json::{Value, json};
use std::{
    io::{Read as _, Write as _},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Placeholder title of a Conversation that has not been named yet.
pub const DEFAULT_CONVERSATION_TITLE: &str = "New conversation";
/// Settings key holding the Ollama model name; blank or absent means the first installed model.
pub const MODEL_SETTING: &str = "title_model";
/// Settings key holding the llama.cpp Hugging Face model reference.
pub const LLAMA_MODEL_SETTING: &str = "llama_title_model";
/// Installed model used when no llama.cpp title model is configured.
pub const DEFAULT_LLAMA_MODEL: &str = "ggml-org/gemma-4-26B-A4B-it-GGUF:Q4_0";

/// Settings key holding the selected title generator.
pub const PROVIDER_SETTING: &str = "title_provider";

/// The default attempts llama.cpp before falling back to Ollama.
pub const DEFAULT_PROVIDER: TitleProvider = TitleProvider::Automatic;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleProvider {
    /// Try the llama.cpp CLI first, then Ollama when the executable is unavailable.
    Automatic,
    LlamaCpp,
    Ollama,
}

/// Models each title provider has locally; each source fails on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledModels {
    /// Hugging Face references in the llama.cpp cache, as `-hf` accepts them.
    pub llama: Result<Vec<String>, String>,
    /// Model names the Ollama server lists.
    pub ollama: Result<Vec<String>, String>,
}

const DEFAULT_OLLAMA_HOST: &str = "127.0.0.1:11434";
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// Generation includes a cold model load.
const GENERATE_TIMEOUT: Duration = Duration::from_secs(120);
const RESPONSE_LIMIT: usize = 1024 * 1024;

/// One kind of short text the local model writes.
struct Task {
    system: &'static str,
    /// Leads the prompt for the llama.cpp CLI, which has no separate user turn label.
    lead: &'static str,
    prompt_limit: usize,
    output_limit: usize,
}

const TITLE: Task = Task {
    system: "You name coding conversations. Reply with one short, evocative title of two to \
five words that captures the user's requests. No quotes, no trailing punctuation, no explanation.",
    lead: "Name this coding conversation:",
    prompt_limit: 4000,
    output_limit: 60,
};

const COMMIT: Task = Task {
    system: "You write Git commit subjects. Reply with one line in the imperative mood, under \
72 characters, that says what the change does and why when it fits. No quotes, no prefix, no \
trailing period, no explanation.",
    lead: "Write the commit subject for this change:",
    prompt_limit: 12000,
    output_limit: 72,
};

/// The configured local text generator, read from Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generator {
    pub provider: TitleProvider,
    pub llama_executable: Option<String>,
    pub llama_model: Option<String>,
    pub ollama_model: Option<String>,
}

impl Generator {
    /// Names a Conversation from its user prompts.
    pub fn title(&self, prompt: &str) -> Result<Option<String>> {
        self.generate(&TITLE, &ollama_host(), prompt)
    }

    /// Writes a commit subject for `changes`, a diff summary and patch.
    pub fn commit_subject(&self, changes: &str) -> Result<Option<String>> {
        self.generate(&COMMIT, &ollama_host(), changes)
    }

    fn generate(&self, task: &Task, ollama_host: &str, prompt: &str) -> Result<Option<String>> {
        let llama = || {
            generate_llama_cli(
                task,
                self.llama_executable.as_deref(),
                self.llama_model.as_deref(),
                prompt,
            )
        };
        let ollama = || generate_ollama(task, ollama_host, self.ollama_model.as_deref(), prompt);
        match self.provider {
            TitleProvider::Automatic => llama()?.map_or_else(ollama, |text| Ok(Some(text))),
            TitleProvider::LlamaCpp => llama(),
            TitleProvider::Ollama => ollama(),
        }
    }
}

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

/// Lists the downloaded llama.cpp models and installed Ollama models.
pub fn installed_models(llama_executable: Option<&str>) -> InstalledModels {
    InstalledModels {
        llama: llama_cached_models(llama_executable).map_err(|error| format!("{error:#}")),
        ollama: ollama_models(&ollama_host()).map_err(|error| format!("{error:#}")),
    }
}

fn ollama_models(host: &str) -> Result<Vec<String>> {
    let address = resolve(host).with_context(|| format!("Ollama host {host} does not resolve"))?;
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .with_context(|| format!("Ollama is not running at {host}"))?;
    let tags = request(&mut stream, host, "GET", "/api/tags", None, LIST_TIMEOUT)
        .context("Could not list Ollama models")?;
    Ok(model_names(&tags))
}

fn generate_ollama(
    task: &Task,
    host: &str,
    model: Option<&str>,
    prompt: &str,
) -> Result<Option<String>> {
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
            let Some(model) = model_names(&tags).into_iter().next() else {
                return Ok(None);
            };
            stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
                .context("Ollama closed before generating")?;
            model
        }
    };
    let body = json!({
        "model": model,
        "system": task.system,
        "prompt": truncate(prompt.trim(), task.prompt_limit),
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
    .with_context(|| format!("Ollama model {model} could not generate text"))?;
    Ok(response
        .get("response")
        .and_then(Value::as_str)
        .and_then(|output| clean_line(output, task.output_limit)))
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

fn model_names(tags: &Value) -> Vec<String> {
    tags.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| model.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn truncate(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((ix, _)) => &text[..ix],
        None => text,
    }
}

/// Reduces model output to one line of at most `limit` characters, or `None` when nothing
/// usable remains.
fn clean_line(output: &str, limit: usize) -> Option<String> {
    let output = match output.rfind("</think>") {
        Some(ix) => &output[ix + "</think>".len()..],
        None => output,
    };
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let line = [
        "Title:",
        "title:",
        "Subject:",
        "subject:",
        "Commit message:",
    ]
    .iter()
    .find_map(|prefix| line.strip_prefix(prefix))
    .unwrap_or(line);
    let quotes: &[char] = &['"', '\'', '`', '*', '#', '“', '”', '‘', '’'];
    let line = line
        .trim()
        .trim_matches(quotes)
        .trim()
        .trim_end_matches(['.', ':', ';', ','])
        .trim();
    let words = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = truncate(&words, limit).trim_end().to_string();
    (!title.is_empty()).then_some(title)
}

fn generate_llama_cli(
    task: &Task,
    executable: Option<&str>,
    model: Option<&str>,
    prompt: &str,
) -> Result<Option<String>> {
    let Ok(executable) = platform::resolve_executable("llama", executable) else {
        return Ok(None);
    };
    let model = model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(DEFAULT_LLAMA_MODEL);
    let prompt = format!(
        "{}\n\n{}",
        task.lead,
        truncate(prompt.trim(), task.prompt_limit)
    );
    let child = Command::new(&executable)
        .args([
            "cli",
            "-hf",
            model,
            "-st",
            "--simple-io",
            "--reasoning",
            "off",
            "--log-disable",
            "--no-show-timings",
            "--color",
            "off",
            "--log-colors",
            "off",
            "-n",
            "64",
            "-sys",
            task.system,
            "-p",
            &prompt,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Start llama.cpp at {}", executable.display()))?;
    let stdout = finish(child, GENERATE_TIMEOUT, "generation")?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.starts_with("Exiting"))
        .and_then(|line| clean_line(line, task.output_limit)))
}

fn llama_cached_models(executable: Option<&str>) -> Result<Vec<String>> {
    let executable = platform::resolve_executable("llama", executable)?;
    let child = Command::new(&executable)
        .args(["cli", "--cache-list"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Start llama.cpp at {}", executable.display()))?;
    Ok(parse_cache_list(&finish(
        child,
        LIST_TIMEOUT,
        "model listing",
    )?))
}

/// Model references from `llama cli --cache-list`, which numbers one per line.
fn parse_cache_list(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let (number, reference) = line.trim().split_once(". ")?;
            number.parse::<usize>().ok()?;
            let reference = reference.trim();
            (!reference.is_empty()).then(|| reference.to_owned())
        })
        .collect()
}

/// Waits for a llama.cpp child that writes little output and returns its stdout.
fn finish(mut child: std::process::Child, timeout: Duration, task: &str) -> Result<String> {
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("Wait for llama.cpp {task}"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "llama.cpp {task} timed out after {} seconds",
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(20));
    };
    anyhow::ensure!(status.success(), "llama.cpp exited with {status}");
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .context("llama.cpp stdout was not captured")?
        .read_to_string(&mut stdout)
        .with_context(|| format!("Read llama.cpp {task}"))?;
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, net::TcpListener, os::unix::fs::PermissionsExt, path::PathBuf};

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("drostje-llama-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

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
        let title = generate_ollama(&TITLE, &host, None, "Fix my lifetimes").unwrap();
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
        let title = generate_ollama(&TITLE, &host, Some("llama3:8b"), "Parse this").unwrap();
        assert_eq!(title.as_deref(), Some("Sparks in the Parser"));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("\"model\":\"llama3:8b\""));
    }

    #[test]
    fn commit_subjects_use_their_own_instructions_and_limit() {
        let (host, server) = serve(vec![ok(json!({
            "response": format!("Subject: `Colour the changes diff{}`.\n\nBody", " a".repeat(40))
        }))]);
        let subject = generate_ollama(&COMMIT, &host, Some("m"), "diff --git a/x b/x")
            .unwrap()
            .unwrap();
        assert!(subject.starts_with("Colour the changes diff a a"));
        assert!(subject.chars().count() <= 72);
        let requests = server.join().unwrap();
        let body: Value =
            serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["system"], COMMIT.system);
        assert_eq!(body["prompt"], "diff --git a/x b/x");
    }

    #[test]
    fn no_models_means_no_title() {
        let (host, server) = serve(vec![ok(json!({"models": []}))]);
        assert_eq!(generate_ollama(&TITLE, &host, None, "Hello").unwrap(), None);
        server.join().unwrap();
    }

    #[test]
    fn unreachable_server_means_no_title() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let host = listener.local_addr().unwrap().to_string();
        drop(listener);
        assert_eq!(generate_ollama(&TITLE, &host, None, "Hello").unwrap(), None);
    }

    #[test]
    fn server_errors_are_reported() {
        let (host, server) = serve(vec![format!(
            "HTTP/1.0 404 Not Found\r\n\r\n{}",
            json!({"error": "model \"missing\" not found"})
        )]);
        let error = generate_ollama(&TITLE, &host, Some("missing"), "Hello").unwrap_err();
        assert!(format!("{error:#}").contains("model \"missing\" not found"));
        server.join().unwrap();
    }

    #[test]
    fn llama_cli_uses_simple_text_contract() {
        let temp = TempDirectory::new();
        let executable = temp.0.join("llama");
        let arguments = temp.0.join("arguments");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nprintf '\\nFocused Parser Repair\\nExiting...\\n'\n",
                arguments.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();

        let title = generate_llama_cli(
            &TITLE,
            Some(executable.to_str().unwrap()),
            Some("ggml-org/gemma-test-GGUF:Q4_0"),
            "Fix parser recovery",
        )
        .unwrap();

        assert_eq!(title.as_deref(), Some("Focused Parser Repair"));
        let arguments = fs::read_to_string(arguments).unwrap();
        assert!(arguments.contains("cli\n-hf\nggml-org/gemma-test-GGUF:Q4_0\n"));
        assert!(arguments.contains("--simple-io\n"));
        assert!(arguments.contains("--reasoning\noff\n"));
        assert!(arguments.contains("-sys\n"));
        assert!(arguments.contains("-p\nName this coding conversation:\n\nFix parser recovery\n"));
    }

    #[test]
    fn llama_cache_list_names_downloaded_models() {
        let temp = TempDirectory::new();
        let executable = temp.0.join("llama");
        let arguments = temp.0.join("arguments");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nprintf 'number of models in cache: 2\\n   1. ggml-org/gemma-4-12B-it-GGUF:Q8_0\\n   2. ggml-org/gemma-4-26B-A4B-it-GGUF:Q4_0\\n'\n",
                arguments.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();

        let models = llama_cached_models(Some(executable.to_str().unwrap())).unwrap();

        assert_eq!(
            models,
            [
                "ggml-org/gemma-4-12B-it-GGUF:Q8_0",
                "ggml-org/gemma-4-26B-A4B-it-GGUF:Q4_0"
            ]
        );
        assert_eq!(
            fs::read_to_string(arguments).unwrap(),
            "cli\n--cache-list\n"
        );
    }

    #[test]
    fn missing_llama_cli_cannot_list_models() {
        let error = llama_cached_models(Some("/definitely/missing/llama")).unwrap_err();
        assert!(format!("{error:#}").contains("missing or not executable"));
    }

    #[test]
    fn cache_list_ignores_headers_and_blank_lines() {
        assert_eq!(
            parse_cache_list("number of models in cache: 0\n\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_cache_list("  12. owner/repo:Q4_K_M  \n"),
            ["owner/repo:Q4_K_M"]
        );
    }

    #[test]
    fn ollama_lists_every_installed_model() {
        let (host, server) = serve(vec![ok(
            json!({"models": [{"name": "lfm2:24b"}, {"model": "nameless"}, {"name": "llama3:8b"}]}),
        )]);
        assert_eq!(ollama_models(&host).unwrap(), ["lfm2:24b", "llama3:8b"]);
        server.join().unwrap();
    }

    #[test]
    fn unreachable_ollama_cannot_list_models() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let host = listener.local_addr().unwrap().to_string();
        drop(listener);
        let error = ollama_models(&host).unwrap_err();
        assert!(format!("{error:#}").starts_with(&format!("Ollama is not running at {host}")));
    }

    #[test]
    fn automatic_falls_back_when_llama_cli_is_unavailable() {
        let (ollama_host, ollama) = serve(vec![ok(json!({
            "response": "Reliable Local Titles"
        }))]);

        let generator = Generator {
            provider: TitleProvider::Automatic,
            llama_executable: Some("/definitely/missing/llama".into()),
            llama_model: None,
            ollama_model: Some("local-model".into()),
        };
        let title = generator
            .generate(&TITLE, &ollama_host, "Repair title generation")
            .unwrap();

        assert_eq!(title.as_deref(), Some("Reliable Local Titles"));
        ollama.join().unwrap();
    }

    #[test]
    fn titles_are_single_line_and_bounded() {
        assert_eq!(
            clean_line(
                "<think>hmm</think>\n\nTitle: **Midnight   Migration**\nextra",
                TITLE.output_limit
            ),
            Some("Midnight Migration".into())
        );
        assert_eq!(clean_line("  \n\"\" \n", TITLE.output_limit), None);
        assert_eq!(
            clean_line(&"word ".repeat(40), TITLE.output_limit)
                .unwrap()
                .chars()
                .count(),
            59
        );
    }

    /// Opt-in: needs a running local Ollama with at least one model.
    #[test]
    #[ignore]
    fn local_ollama_names_a_prompt() {
        let title = Generator {
            provider: TitleProvider::Ollama,
            llama_executable: None,
            llama_model: None,
            ollama_model: None,
        }
        .title("The sidebar flickers when I resize the window; find out why")
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
