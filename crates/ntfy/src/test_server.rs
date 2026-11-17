use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use serde_json::json;

#[derive(Clone, Debug)]
struct Stored {
    id: String,
    time: i64,
    topic: String,
    message: String,
    attachment: Option<(String, usize)>,
}

#[derive(Default)]
struct State {
    messages: Vec<Stored>,
    files: HashMap<String, Vec<u8>>,
    next: u64,
    rate_limited: bool,
    required_token: Option<String>,
    publishes: usize,
}

#[derive(Clone)]
pub struct TestServer {
    address: String,
    state: Arc<Mutex<State>>,
}

impl TestServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binding a loopback test port");
        let address = format!("http://{}", listener.local_addr().expect("bound address"));
        let state = Arc::new(Mutex::new(State::default()));
        let server = Self {
            address: address.clone(),
            state: state.clone(),
        };
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = state.clone();
                let address = address.clone();
                std::thread::spawn(move || handle(stream, &state, &address));
            }
        });
        server
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn require_token(&self, token: &str) {
        self.state.lock().required_token = Some(format!("Bearer {token}"));
    }

    pub fn set_rate_limited(&self, limited: bool) {
        self.state.lock().rate_limited = limited;
    }

    pub fn publishes(&self) -> usize {
        self.state.lock().publishes
    }

    pub fn messages(&self, topic: &str) -> Vec<String> {
        self.state
            .lock()
            .messages
            .iter()
            .filter(|message| message.topic == topic)
            .map(|message| message.message.clone())
            .collect()
    }

    pub fn has_attachment(&self, topic: &str) -> bool {
        self.state
            .lock()
            .messages
            .iter()
            .any(|message| message.topic == topic && message.attachment.is_some())
    }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

fn read_head(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let path = target
        .split_once('?')
        .map(|(path, _)| path.to_owned())
        .unwrap_or(target);
    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    Some(Request {
        method,
        path,
        headers,
    })
}

fn respond(stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

fn frame(message: &Stored, address: &str) -> String {
    let mut value = json!({
        "id": message.id,
        "time": message.time,
        "event": "message",
        "topic": message.topic,
        "message": message.message,
    });
    if let Some((id, size)) = &message.attachment {
        value["attachment"] = json!({
            "name": "sealed.bin",
            "size": size,
            "url": format!("{address}/file/{id}.bin"),
        });
    }
    value.to_string()
}

fn handle(stream: TcpStream, state: &Mutex<State>, address: &str) {
    let mut peek = [0u8; 2048];
    let length = stream.peek(&mut peek).unwrap_or(0);
    let head = String::from_utf8_lossy(&peek[..length]).to_ascii_lowercase();
    if head.starts_with("get ") && head.contains("upgrade: websocket") {
        return websocket(stream, state, address);
    }
    let mut reader = BufReader::new(stream.try_clone().expect("cloning a test stream"));
    let Some(request) = read_head(&mut reader) else {
        return;
    };
    let mut stream = stream;
    let required = state.lock().required_token.clone();
    if required.is_some() && request.headers.get("authorization") != required.as_ref() {
        return respond(
            &mut stream,
            "403 Forbidden",
            "",
            br#"{"error":"forbidden"}"#,
        );
    }
    if request.method == "GET" {
        let id = request
            .path
            .strip_prefix("/file/")
            .and_then(|name| name.strip_suffix(".bin"))
            .unwrap_or_default();
        let file = state.lock().files.get(id).cloned();
        return match file {
            Some(bytes) => respond(&mut stream, "200 OK", "", &bytes),
            None => respond(&mut stream, "404 Not Found", "", b"{}"),
        };
    }
    let length: usize = request
        .headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let mut state = state.lock();
    if state.rate_limited {
        return respond(
            &mut stream,
            "429 Too Many Requests",
            "Retry-After: 7\r\n",
            br#"{"error":"limit reached"}"#,
        );
    }
    state.publishes += 1;
    state.next += 1;
    let id = format!("m{:04}", state.next);
    let topic = request.path.trim_start_matches('/').to_owned();
    let attachment = request.headers.contains_key("x-filename");
    let message = if attachment {
        state.files.insert(id.clone(), body.clone());
        "You received a file: sealed.bin".to_owned()
    } else {
        String::from_utf8_lossy(&body).into_owned()
    };
    let stored = Stored {
        id: id.clone(),
        time: now(),
        topic,
        message,
        attachment: attachment.then(|| (id.clone(), body.len())),
    };
    state.messages.push(stored.clone());
    drop(state);
    respond(
        &mut stream,
        "200 OK",
        "",
        frame(&stored, address).as_bytes(),
    );
}

#[expect(
    clippy::result_large_err,
    reason = "tungstenite's handshake callback fixes its error type to ErrorResponse"
)]
fn websocket(stream: TcpStream, state: &Mutex<State>, address: &str) {
    let required = state.lock().required_token.clone();
    let mut target = String::new();
    let mut authorized = required.is_none();
    let callback = |request: &tungstenite::handshake::server::Request,
                    response: tungstenite::handshake::server::Response| {
        target = request.uri().to_string();
        if let Some(required) = &required {
            authorized = request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                == Some(required.as_str());
        }
        if authorized {
            Ok(response)
        } else {
            let mut refusal = tungstenite::handshake::server::ErrorResponse::new(None);
            *refusal.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
            Err(refusal)
        }
    };
    let Ok(mut socket) = tungstenite::accept_hdr(stream, callback) else {
        return;
    };
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let topics: Vec<String> = path
        .trim_start_matches('/')
        .trim_end_matches("/ws")
        .split(',')
        .map(str::to_owned)
        .collect();
    let since = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("since="))
        .map(str::to_owned);
    let open = json!({"id":"open","time":now(),"event":"open","topic":topics.join(",")});
    if socket
        .send(tungstenite::Message::text(open.to_string()))
        .is_err()
    {
        return;
    }
    let mut sent = {
        let state = state.lock();
        match since {
            Some(since) => state
                .messages
                .iter()
                .position(|message| message.id == since)
                .map(|position| position + 1)
                .unwrap_or(0),
            None => state.messages.len(),
        }
    };
    loop {
        let pending: Vec<Stored> = state.lock().messages[sent..].to_vec();
        sent += pending.len();
        for message in pending
            .iter()
            .filter(|message| topics.contains(&message.topic))
        {
            if socket
                .send(tungstenite::Message::text(frame(message, address)))
                .is_err()
            {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
