//! Bounded loopback bridge between harness tools and the owning conversation's UI.
//! All sockets, file checks and waits run on dedicated workers, never the GPUI thread.
use crate::browser::BrowserCommand;
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const MAX_BODY: usize = 1024 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
const MAX_RESPONSE: usize = 16 * 1024 * 1024;
const IO_POLL: Duration = Duration::from_millis(100);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TOOL_SPECS: &str = include_str!("../assets/browser-tools/tools.json");
const INSTRUCTIONS: &str = "DROSTJE provides real embedded Servo browser tools. To present an HTML artifact, first write a complete HTML file under /tmp (for example /tmp/drostje-chart.html), then call show_artifact with its absolute path and optional title. Do not pass HTML source to the tool. Keep adjacent CSS, JavaScript and images beside that file so relative URLs work. Artifacts are live, unsandboxed pages: JavaScript and network access are enabled. Temporary files are not durable across cleanup or restarts. Use the browser tool to open websites in this thread's sidebar (placement sidebar) or a dock tab (placement tab), inspect state, navigate, evaluate JavaScript, click, type, send keys, scroll, save PNG screenshots, and close pages. Reuse the browser ID returned by open/show_artifact. Inspect loading/error state before relying on a page; tools return actual runtime errors and must not be represented as successful when they fail. These tools control DROSTJE's browser, not Chrome.";

#[derive(Clone, Debug)]
pub struct BrowserToolConfig {
    pub endpoint: String,
    pub thread: Uuid,
}

pub struct BrowserToolRequest {
    pub thread: Uuid,
    pub operation: BrowserToolOperation,
    pub reply: mpsc::Sender<std::result::Result<Value, String>>,
}

pub enum BrowserToolOperation {
    ShowArtifact { path: String, title: Option<String> },
    Open { url: String, placement: String },
    Control { id: Uuid, command: BrowserCommand },
    Inspect { id: Uuid },
    Screenshot { id: Uuid, path: String },
    Close { id: Uuid },
}

#[derive(Clone)]
pub struct BrowserToolServer(Arc<Server>);

struct Server {
    endpoint: String,
    stop: Arc<AtomicBool>,
    connections: async_channel::Sender<TcpStream>,
    requests: async_channel::Receiver<BrowserToolRequest>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.connections.close();
        self.requests.close();
        // Workers observe stop within IO_POLL. Do not join IO threads on the UI thread.
    }
}

impl BrowserToolServer {
    /// Call from bootstrap/background code: binding a socket is deliberately not lazy UI work.
    pub fn start() -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .context("Bind DROSTJE browser tool endpoint")?;
        listener.set_nonblocking(true)?;
        let route = format!("/{}", Uuid::new_v4());
        let endpoint = format!("http://{}{route}", listener.local_addr()?);
        let stop = Arc::new(AtomicBool::new(false));
        let (connections, pending) = async_channel::bounded::<TcpStream>(16);
        let (output, requests) = async_channel::bounded(32);
        let server = Self(Arc::new(Server {
            endpoint,
            stop: stop.clone(),
            connections: connections.clone(),
            requests,
        }));
        let specs: Arc<Value> = Arc::new(serde_json::from_str(TOOL_SPECS)?);
        for index in 0..4 {
            let pending = pending.clone();
            let output = output.clone();
            let stop = stop.clone();
            let route = route.clone();
            let specs = specs.clone();
            thread::Builder::new()
                .name(format!("drostje-browser-tools-{index}"))
                .spawn(move || {
                    while let Ok(mut stream) = pending.recv_blocking() {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        let _ = stream.set_read_timeout(Some(IO_POLL));
                        let _ = stream.set_write_timeout(Some(IO_POLL));
                        let response = match read_http(&mut stream, &stop) {
                            Ok(request) => handle_http(request, &route, &output, &stop, &specs),
                            Err(error) => {
                                HttpResponse::json(400, json!({"error": error.to_string()}))
                            }
                        };
                        let _ = write_http(&mut stream, response, &stop);
                    }
                })
                .context("Start browser tool worker")?;
        }
        thread::Builder::new()
            .name("drostje-browser-tools-listener".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // Dropping an excess connection is preferable to unbounded workers.
                            let _ = connections.try_send(stream);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => break,
                    }
                }
                connections.close();
                output.close();
            })
            .context("Start browser tool listener")?;
        Ok(server)
    }

    pub fn endpoint(&self) -> String {
        self.0.endpoint.clone()
    }

    pub fn requests(&self) -> async_channel::Receiver<BrowserToolRequest> {
        self.0.requests.clone()
    }
}

/// Runs in the harness's existing blocking preparation worker. No Node subprocess is
/// needed for Claude: its supported Streamable HTTP MCP transport talks to this server.
pub(crate) fn configure_harness(
    command: &mut Command,
    harness: &str,
    session_dir: &Path,
    config: Option<&BrowserToolConfig>,
) -> Option<String> {
    let config = config?;
    let install = || -> Result<Vec<std::ffi::OsString>> {
        let directory = session_dir.join("browser-tools");
        std::fs::create_dir_all(&directory).context("Create browser tool extension directory")?;
        if harness == "claude-code" {
            let mcp = directory.join("mcp.json");
            let contents = json!({"mcpServers": {"drostje": {
                "type": "http", "url": format!("{}/mcp/{}", config.endpoint, config.thread)
            }}});
            std::fs::write(&mcp, serde_json::to_vec(&contents)?)?;
            Ok(vec!["--mcp-config".into(), mcp.into_os_string(),
                "--append-system-prompt".into(),
                format!("{INSTRUCTIONS} In Claude Code the tools are named mcp__drostje__show_artifact and mcp__drostje__browser.").into()])
        } else {
            ensure!(
                matches!(harness, "omp" | "pi"),
                "Unsupported browser harness: {harness}"
            );
            for (name, contents) in [
                ("tools.json", TOOL_SPECS),
                (
                    "client.mjs",
                    include_str!("../assets/browser-tools/client.mjs"),
                ),
                ("omp.ts", include_str!("../assets/browser-tools/omp.ts")),
                ("pi.ts", include_str!("../assets/browser-tools/pi.ts")),
            ] {
                std::fs::write(directory.join(name), contents)
                    .with_context(|| format!("Install browser tool extension {name}"))?;
            }
            Ok(vec![
                "--extension".into(),
                directory.join(format!("{harness}.ts")).into_os_string(),
                "--append-system-prompt".into(),
                INSTRUCTIONS.into(),
            ])
        }
    };
    match install() {
        Ok(arguments) => {
            command
                .args(arguments)
                .env("DROSTJE_BROWSER_ENDPOINT", &config.endpoint)
                .env("DROSTJE_BROWSER_THREAD", config.thread.to_string());
            None
        }
        Err(error) => {
            let warning = format!(
                "DROSTJE browser tools unavailable: {error:#}. Conversation remains available without artifact/browser tools."
            );
            command.arg("--append-system-prompt").arg(&warning);
            Some(warning)
        }
    }
}

struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}
struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}
impl HttpResponse {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: body.to_string().into_bytes(),
        }
    }
    fn empty(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
        }
    }
}

fn read_http(stream: &mut TcpStream, stop: &AtomicBool) -> Result<HttpRequest> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut data = Vec::with_capacity(4096);
    let mut buffer = [0; 4096];
    let header_end = loop {
        if let Some(position) = data.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            ensure!(position <= MAX_HEADERS, "HTTP headers too large");
            break position + 4;
        }
        ensure!(data.len() <= MAX_HEADERS, "HTTP headers too large");
        read_more(stream, &mut data, &mut buffer, deadline, stop)?;
    };
    let header = std::str::from_utf8(&data[..header_end])?;
    let mut lines = header.split("\r\n");
    let mut first = lines
        .next()
        .context("Missing HTTP request line")?
        .split_whitespace();
    let method = first.next().context("Missing HTTP method")?.to_owned();
    let path = first.next().context("Missing HTTP path")?.to_owned();
    ensure!(
        first.next() == Some("HTTP/1.1") && first.next().is_none(),
        "Expected HTTP/1.1"
    );
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').context("Invalid HTTP header")?;
        if key.eq_ignore_ascii_case("content-length") {
            ensure!(length.is_none(), "Duplicate Content-Length");
            length = Some(value.trim().parse::<usize>()?);
        }
        ensure!(
            !key.eq_ignore_ascii_case("transfer-encoding"),
            "Chunked requests are unsupported"
        );
        // This is a local agent endpoint, not a cross-origin web API.
        ensure!(
            !key.eq_ignore_ascii_case("origin"),
            "Browser-origin requests are unsupported"
        );
    }
    let length = length.unwrap_or(0);
    ensure!(length <= MAX_BODY, "Tool request exceeds 1 MiB");
    while data.len() < header_end + length {
        read_more(stream, &mut data, &mut buffer, deadline, stop)?;
    }
    let body = data[header_end..header_end + length].to_vec();
    Ok(HttpRequest { method, path, body })
}

fn read_more(
    stream: &mut TcpStream,
    data: &mut Vec<u8>,
    buffer: &mut [u8],
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<()> {
    loop {
        ensure!(!stop.load(Ordering::Acquire), "Browser tool server stopped");
        ensure!(Instant::now() < deadline, "Timed out reading tool request");
        match stream.read(buffer) {
            Ok(0) => bail!("Tool client disconnected"),
            Ok(count) => {
                data.extend_from_slice(&buffer[..count]);
                return Ok(());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn write_http(stream: &mut TcpStream, response: HttpResponse, stop: &AtomicBool) -> Result<()> {
    let response = if response.body.len() > MAX_RESPONSE {
        HttpResponse::json(
            500,
            json!({"error": "Browser tool response exceeds 16 MiB"}),
        )
    } else {
        response
    };
    let reason = match response.status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        response.status,
        reason,
        response.body.len()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    for mut bytes in [header.as_bytes(), response.body.as_slice()] {
        while !bytes.is_empty() {
            ensure!(!stop.load(Ordering::Acquire), "Browser tool server stopped");
            ensure!(Instant::now() < deadline, "Timed out writing tool response");
            match stream.write(bytes) {
                Ok(0) => bail!("Tool client disconnected"),
                Ok(count) => bytes = &bytes[count..],
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct ToolCall {
    thread: Uuid,
    name: String,
    arguments: Value,
    cwd: Option<PathBuf>,
}

fn handle_http(
    request: HttpRequest,
    route: &str,
    output: &async_channel::Sender<BrowserToolRequest>,
    stop: &AtomicBool,
    specs: &Value,
) -> HttpResponse {
    let mcp_prefix = format!("{route}/mcp/");
    let mcp_thread = request
        .path
        .strip_prefix(&mcp_prefix)
        .and_then(|id| Uuid::parse_str(id).ok());
    if request.path != route && mcp_thread.is_none() {
        return HttpResponse::json(404, json!({"error": "Unknown browser tool endpoint"}));
    }
    if request.method != "POST" {
        // A stateless MCP server has no SSE stream or session to delete.
        return HttpResponse::empty(if request.method == "DELETE" && mcp_thread.is_some() {
            200
        } else {
            405
        });
    }
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(body) => body,
        Err(error) => return HttpResponse::json(400, json!({"error": error.to_string()})),
    };
    if let Some(thread) = mcp_thread {
        return handle_mcp(body, thread, output, stop, specs);
    }
    let result = serde_json::from_value::<ToolCall>(body)
        .map_err(anyhow::Error::from)
        .and_then(|call| execute_tool(call, output, stop));
    match result {
        Ok(result) => HttpResponse::json(200, json!({"result": result})),
        Err(error) => HttpResponse::json(400, json!({"error": format!("{error:#}")})),
    }
}

fn handle_mcp(
    body: Value,
    thread: Uuid,
    output: &async_channel::Sender<BrowserToolRequest>,
    stop: &AtomicBool,
    specs: &Value,
) -> HttpResponse {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    if body["jsonrpc"] != "2.0" || !body["method"].is_string() {
        return HttpResponse::json(
            200,
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32600,"message":"Invalid Request"}}),
        );
    }
    if body.get("id").is_none() {
        return HttpResponse::empty(202);
    }
    let result = match body["method"].as_str().unwrap_or_default() {
        "initialize" => {
            let version = body["params"]["protocolVersion"]
                .as_str()
                .filter(|version| {
                    matches!(
                        *version,
                        "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25"
                    )
                })
                .unwrap_or("2025-03-26");
            json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"drostje-browser","version":env!("CARGO_PKG_VERSION")},"instructions":INSTRUCTIONS})
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools":specs}),
        "tools/call" => {
            let result = body["params"]["name"]
                .as_str()
                .context("Missing tool name")
                .and_then(|name| {
                    execute_tool(
                        ToolCall {
                            thread,
                            name: name.into(),
                            arguments: body["params"]
                                .get("arguments")
                                .cloned()
                                .unwrap_or_else(|| json!({})),
                            cwd: None,
                        },
                        output,
                        stop,
                    )
                });
            match result {
                Ok(value) => {
                    json!({"content":[{"type":"text","text":value.to_string()}],"isError":false})
                }
                Err(error) => {
                    json!({"content":[{"type":"text","text":format!("{error:#}")}],"isError":true})
                }
            }
        }
        _ => {
            return HttpResponse::json(
                200,
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
            );
        }
    };
    HttpResponse::json(200, json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn execute_tool(
    call: ToolCall,
    output: &async_channel::Sender<BrowserToolRequest>,
    stop: &AtomicBool,
) -> Result<Value> {
    let mut artifact = None;
    let operations = match call.name.as_str() {
        "show_artifact" => {
            let original = string(&call.arguments, "path")?;
            let path = Path::new(&original);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                call.cwd
                    .as_deref()
                    .context("show_artifact requires an absolute path")?
                    .join(path)
            };
            let path = std::fs::canonicalize(&path)
                .with_context(|| format!("Artifact file is unavailable: {}", path.display()))?;
            ensure!(
                path.is_file(),
                "Artifact path is not a regular file: {}",
                path.display()
            );
            std::fs::File::open(&path)
                .with_context(|| format!("Artifact file is unreadable: {}", path.display()))?;
            let file_url = url::Url::from_file_path(&path)
                .map_err(|_| anyhow::anyhow!("Cannot convert artifact path to file URL"))?
                .to_string();
            let path = path
                .to_str()
                .context("Artifact path is not valid UTF-8")?
                .to_owned();
            artifact = Some((original, path.clone(), file_url));
            vec![BrowserToolOperation::ShowArtifact {
                path,
                title: optional_string(&call.arguments, "title")?,
            }]
        }
        "browser" => browser_operations(&call.arguments)?,
        other => bail!("Unknown DROSTJE browser tool: {other}"),
    };
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut result = Value::Null;
    for operation in operations {
        ensure!(!stop.load(Ordering::Acquire), "Browser tool server stopped");
        let (reply, response) = mpsc::channel();
        output
            .try_send(BrowserToolRequest {
                thread: call.thread,
                operation,
                reply,
            })
            .map_err(|error| anyhow::anyhow!("Browser UI request queue is unavailable: {error}"))?;
        result = loop {
            ensure!(!stop.load(Ordering::Acquire), "Browser tool server stopped");
            ensure!(
                Instant::now() < deadline,
                "Timed out waiting for the browser runtime; operation outcome is unknown"
            );
            match response.recv_timeout(IO_POLL) {
                Ok(Ok(value)) => break value,
                Ok(Err(error)) => bail!(error),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("Browser UI dropped the request without a response")
                }
            }
        };
    }
    if let Some((original, path, file_url)) = artifact {
        let object = result
            .as_object_mut()
            .context("Browser UI returned a non-object artifact result")?;
        object.insert("path".into(), Value::String(path));
        object.insert("original_path".into(), Value::String(original));
        object.insert("file_url".into(), Value::String(file_url));
    }
    Ok(result)
}

fn string(value: &Value, field: &str) -> Result<String> {
    value[field]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("Missing or invalid {field}"))
}
fn optional_string(value: &Value, field: &str) -> Result<Option<String>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => string(value, field).map(Some),
    }
}
fn number(value: &Value, field: &str) -> Result<f32> {
    let number = value[field]
        .as_f64()
        .with_context(|| format!("Missing or invalid {field}"))? as f32;
    ensure!(number.is_finite(), "{field} must be a finite number");
    Ok(number)
}
fn boolean(value: &Value, field: &str) -> Result<bool> {
    match value.get(field) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        _ => bail!("Invalid {field}: expected boolean"),
    }
}

fn browser_operations(args: &Value) -> Result<Vec<BrowserToolOperation>> {
    let action = string(args, "action")?;
    if action == "open" {
        let placement = optional_string(args, "placement")?.unwrap_or_else(|| "sidebar".into());
        ensure!(
            matches!(placement.as_str(), "sidebar" | "tab"),
            "placement must be sidebar or tab"
        );
        return Ok(vec![BrowserToolOperation::Open {
            url: string(args, "url")?,
            placement,
        }]);
    }
    let id = Uuid::parse_str(&string(args, "id")?).context("Invalid browser ID")?;
    let control = |command| BrowserToolOperation::Control { id, command };
    let operation = match action.as_str() {
        "inspect" => BrowserToolOperation::Inspect { id },
        "close" => BrowserToolOperation::Close { id },
        "screenshot" => {
            let path = string(args, "path")?;
            ensure!(
                Path::new(&path).is_absolute(),
                "Screenshot path must be absolute"
            );
            BrowserToolOperation::Screenshot { id, path }
        }
        "navigate" => control(BrowserCommand::Navigate {
            url: string(args, "url")?,
        }),
        "back" => control(BrowserCommand::Back),
        "forward" => control(BrowserCommand::Forward),
        "reload" => control(BrowserCommand::Reload),
        "evaluate" => control(BrowserCommand::Evaluate {
            request_id: Uuid::new_v4().to_string(),
            script: string(args, "script")?,
        }),
        "type" => control(BrowserCommand::Text {
            text: string(args, "text")?,
        }),
        "scroll" => control(BrowserCommand::Scroll {
            x: number(args, "x")?,
            y: number(args, "y")?,
            dx: number(args, "dx")?,
            dy: number(args, "dy")?,
        }),
        "click" => {
            let x = number(args, "x")?;
            let y = number(args, "y")?;
            let button = match args.get("button") {
                None => 0,
                Some(value) => value
                    .as_u64()
                    .filter(|button| *button <= 2)
                    .context("button must be 0, 1 or 2")? as u8,
            };
            return Ok(vec![
                control(BrowserCommand::Pointer {
                    x,
                    y,
                    button,
                    pressed: true,
                }),
                control(BrowserCommand::Pointer {
                    x,
                    y,
                    button,
                    pressed: false,
                }),
            ]);
        }
        "key" => {
            let key = string(args, "key")?;
            let code = optional_string(args, "code")?.unwrap_or_default();
            let shift = boolean(args, "shift")?;
            let control_key = boolean(args, "control")?;
            let alt = boolean(args, "alt")?;
            let meta = boolean(args, "meta")?;
            return Ok(vec![
                control(BrowserCommand::Key {
                    key: key.clone(),
                    code: code.clone(),
                    pressed: true,
                    shift,
                    control: control_key,
                    alt,
                    meta,
                }),
                control(BrowserCommand::Key {
                    key,
                    code,
                    pressed: false,
                    shift,
                    control: control_key,
                    alt,
                    meta,
                }),
            ]);
        }
        other => bail!("Unknown browser action: {other}"),
    };
    Ok(vec![operation])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(server: &BrowserToolServer, path: &str, body: Value) -> Value {
        let endpoint = url::Url::parse(&server.endpoint()).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", endpoint.port().unwrap())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let body = body.to_string();
        write!(
            stream,
            "POST {}{} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            endpoint.path(),
            path,
            body.len(),
            body
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    }

    #[test]
    fn missing_artifact_fails_without_dispatching_ui() {
        let server = BrowserToolServer::start().unwrap();
        let result = post(
            &server,
            "",
            json!({"thread":Uuid::new_v4(),"name":"show_artifact","arguments":{"path":format!("/tmp/{}.html", Uuid::new_v4())}}),
        );
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("Artifact file is unavailable")
        );
        assert!(server.requests().is_empty());
    }

    #[test]
    fn artifact_keeps_relative_assets_and_canonical_path() {
        let root = std::env::temp_dir().join(format!("drostje-artifact-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.html"), "<img src='nearby.png'>").unwrap();
        let expected = std::fs::canonicalize(root.join("index.html")).unwrap();
        let server = BrowserToolServer::start().unwrap();
        let requests = server.requests();
        let worker = thread::spawn(move || {
            let request = requests.recv_blocking().unwrap();
            assert!(
                matches!(request.operation, BrowserToolOperation::ShowArtifact { ref path, .. } if Path::new(path) == expected)
            );
            request.reply.send(Ok(json!({"id":"artifact"}))).unwrap();
        });
        let result = post(
            &server,
            "",
            json!({"thread":Uuid::new_v4(),"name":"show_artifact","cwd":root,"arguments":{"path":"index.html"}}),
        );
        assert_eq!(result["result"]["original_path"], "index.html");
        assert!(
            result["result"]["file_url"]
                .as_str()
                .unwrap()
                .starts_with("file:///")
        );
        worker.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mcp_discovers_real_tools_and_preserves_runtime_errors() {
        let server = BrowserToolServer::start().unwrap();
        let thread = Uuid::new_v4();
        let path = format!("/mcp/{thread}");
        let initialized = post(
            &server,
            &path,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
        );
        assert_eq!(initialized["result"]["protocolVersion"], "2025-03-26");
        let tools = post(
            &server,
            &path,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        );
        assert_eq!(tools["result"]["tools"][0]["name"], "show_artifact");
        let requests = server.requests();
        let worker = thread::spawn(move || {
            let request = requests.recv_blocking().unwrap();
            assert_eq!(request.thread, thread);
            request
                .reply
                .send(Err("Servo helper is missing".into()))
                .unwrap();
        });
        let result = post(
            &server,
            &path,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"browser","arguments":{"action":"inspect","id":Uuid::new_v4()}}}),
        );
        assert_eq!(result["result"]["isError"], true);
        assert!(
            result["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Servo helper is missing")
        );
        worker.join().unwrap();
    }

    #[test]
    fn clicks_and_keys_include_release_and_evaluations_have_unique_ids() {
        let id = Uuid::new_v4();
        let click = browser_operations(&json!({"action":"click","id":id,"x":10,"y":20})).unwrap();
        assert!(matches!(
            click[0],
            BrowserToolOperation::Control {
                command: BrowserCommand::Pointer { pressed: true, .. },
                ..
            }
        ));
        assert!(matches!(
            click[1],
            BrowserToolOperation::Control {
                command: BrowserCommand::Pointer { pressed: false, .. },
                ..
            }
        ));
        assert!(
            browser_operations(&json!({"action":"click","id":id,"x":10,"y":20,"button":3}))
                .is_err()
        );
        let key = browser_operations(&json!({"action":"key","id":id,"key":"Enter"})).unwrap();
        assert!(matches!(
            key[1],
            BrowserToolOperation::Control {
                command: BrowserCommand::Key { pressed: false, .. },
                ..
            }
        ));
        let first =
            browser_operations(&json!({"action":"evaluate","id":id,"script":"1+1"})).unwrap();
        let second =
            browser_operations(&json!({"action":"evaluate","id":id,"script":"1+1"})).unwrap();
        let BrowserToolOperation::Control {
            command: BrowserCommand::Evaluate { request_id: a, .. },
            ..
        } = &first[0]
        else {
            panic!("Expected evaluation");
        };
        let BrowserToolOperation::Control {
            command: BrowserCommand::Evaluate { request_id: b, .. },
            ..
        } = &second[0]
        else {
            panic!("Expected evaluation");
        };
        assert_ne!(a, b);
    }

    #[test]
    fn oversized_requests_are_rejected_before_reading_the_body() {
        let server = BrowserToolServer::start().unwrap();
        let endpoint = url::Url::parse(&server.endpoint()).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", endpoint.port().unwrap())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
            endpoint.path(),
            MAX_BODY + 1
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("Tool request exceeds 1 MiB"));
        assert!(server.requests().is_empty());
    }

    #[test]
    fn dropping_last_server_handle_closes_request_channel() {
        let server = BrowserToolServer::start().unwrap();
        let requests = server.requests();
        let other = server.clone();
        drop(server);
        assert!(!requests.is_closed());
        drop(other);
        assert!(requests.is_closed());
    }
}
