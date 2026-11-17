use std::{
    io::ErrorKind,
    net::{TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use tungstenite::{
    Message as Frame, WebSocket, client::IntoClientRequest, protocol::WebSocketConfig,
    stream::MaybeTlsStream,
};

use crate::{
    Message, Server,
    message::{Frame as Parsed, parse_frame},
};

const QUEUE: usize = 256;
const TICK: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const SILENCE: Duration = Duration::from_secs(120);
const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(60);
const MAX_FRAME: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Connected,
    Message(Message),
    Disconnected(String),
}

pub struct Subscription {
    pub events: Receiver<Event>,
    stop: Arc<AtomicBool>,
}

impl Subscription {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.events.close();
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.stop();
    }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

pub(crate) fn start(server: Server, topics: &[&str], since: Option<String>) -> Subscription {
    let (sender, events) = async_channel::bounded(QUEUE);
    let stop = Arc::new(AtomicBool::new(false));
    let topics = topics.join(",");
    let worker_stop = stop.clone();
    std::thread::Builder::new()
        .name("ntfy-subscription".into())
        .spawn(move || run(server, topics, since, sender, worker_stop))
        .expect("spawning the ntfy subscription thread");
    Subscription { events, stop }
}

fn stopped(stop: &AtomicBool, sender: &Sender<Event>) -> bool {
    stop.load(Ordering::Relaxed) || sender.is_closed()
}

fn run(
    server: Server,
    topics: String,
    mut since: Option<String>,
    sender: Sender<Event>,
    stop: Arc<AtomicBool>,
) {
    let mut retry = FIRST_RETRY;
    let mut reported: Option<String> = None;
    while !stopped(&stop, &sender) {
        let result = connect(&server, &topics, since.as_deref()).and_then(|mut socket| {
            retry = FIRST_RETRY;
            reported = None;
            if sender.send_blocking(Event::Connected).is_err() {
                return Ok(());
            }
            read(&mut socket, &mut since, &sender, &stop)
        });
        if stopped(&stop, &sender) {
            break;
        }
        let reason = match result {
            Ok(()) => "Server closed the subscription".to_owned(),
            Err(reason) => reason,
        };
        if reported.as_deref() != Some(reason.as_str()) {
            if sender
                .send_blocking(Event::Disconnected(reason.clone()))
                .is_err()
            {
                break;
            }
            reported = Some(reason);
        }
        let deadline = Instant::now() + retry;
        retry = (retry * 2).min(MAX_RETRY);
        while Instant::now() < deadline && !stopped(&stop, &sender) {
            std::thread::sleep(TICK);
        }
    }
    sender.close();
}

fn tcp(socket: &Socket) -> Option<&TcpStream> {
    match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => Some(stream),
        MaybeTlsStream::Rustls(stream) => Some(&stream.sock),
        _ => None,
    }
}

fn connect(server: &Server, topics: &str, since: Option<&str>) -> Result<Socket, String> {
    let mut url = server.topic_url(&format!("{topics}/ws"));
    url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
        .map_err(|_| "Server address cannot carry a WebSocket".to_owned())?;
    if let Some(since) = since {
        url.query_pairs_mut().append_pair("since", since);
    }
    let host = url
        .host_str()
        .ok_or("Server address has no host")?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = url
        .port_or_known_default()
        .ok_or("Server address has no port")?;
    let addresses = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|_| format!("Cannot find server {host}"))?;
    let stream = addresses
        .take(8)
        .find_map(|address| TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).ok())
        .ok_or_else(|| format!("Cannot reach server {host}"))?;
    stream
        .set_nodelay(true)
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|error| error.to_string())?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| "Invalid subscription request".to_owned())?;
    if let Some(authorization) = server.authorization() {
        let mut value = tungstenite::http::HeaderValue::from_str(authorization)
            .map_err(|_| "Invalid server token".to_owned())?;
        value.set_sensitive(true);
        request
            .headers_mut()
            .insert(tungstenite::http::header::AUTHORIZATION, value);
    }
    let config = WebSocketConfig::default()
        .max_frame_size(Some(MAX_FRAME))
        .max_message_size(Some(MAX_FRAME));
    let (socket, _) = tungstenite::client_tls_with_config(request, stream, Some(config), None)
        .map_err(|error| match error {
            tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response)) => {
                match response.status().as_u16() {
                    401 | 403 => "Server refused access to the topic; check the token".to_owned(),
                    429 => "Server rate limit reached".to_owned(),
                    status => format!("Server refused the subscription ({status})"),
                }
            }
            error => format!("Subscription handshake failed: {error}"),
        })?;
    let tcp = tcp(&socket).ok_or("Unsupported TLS transport")?;
    tcp.set_read_timeout(Some(TICK))
        .map_err(|error| error.to_string())?;
    Ok(socket)
}

fn read(
    socket: &mut Socket,
    since: &mut Option<String>,
    sender: &Sender<Event>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut heard = Instant::now();
    loop {
        if stopped(stop, sender) {
            let _ = socket.close(None);
            return Ok(());
        }
        if heard.elapsed() > SILENCE {
            return Err("Server stopped sending keepalives".into());
        }
        match socket.read() {
            Ok(Frame::Text(text)) => {
                heard = Instant::now();
                match parse_frame(text.as_str()) {
                    Ok(Parsed::Message(message)) => {
                        *since = Some(message.id.clone());
                        if sender.send_blocking(Event::Message(message)).is_err() {
                            return Ok(());
                        }
                    }
                    Ok(Parsed::Open | Parsed::Keepalive | Parsed::Other) => {}
                    Err(reason) => return Err(reason),
                }
            }
            Ok(Frame::Close(_)) => return Err("Server closed the subscription".into()),
            Ok(_) => heard = Instant::now(),
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(error) => return Err(format!("Subscription lost: {error}")),
        }
    }
}
