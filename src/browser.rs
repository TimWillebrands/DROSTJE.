//! The UI only touches retained memory here. Process discovery, pipes and shutdown
//! run on workers; Servo itself lives in a separately linked executable.
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use parking_lot::{Condvar, Mutex};
use serde_json::Value;

#[path = "../crates/servo-browser/src/protocol.rs"]
mod protocol;
pub use protocol::BrowserCommand;
use protocol::{Event, MAX_PENDING, PageState, Request};

pub type BrowserId = uuid::Uuid;

#[derive(Clone, Debug)]
pub struct BrowserSnapshot {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub error: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub frame: Option<Arc<BrowserFrame>>,
}

#[derive(Debug)]
pub struct BrowserFrame {
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

struct Session {
    wire_id: BrowserId,
    snapshot: BrowserSnapshot,
}

struct Evaluation {
    id: BrowserId,
    started: Instant,
    result: Option<Result<Value, String>>,
}

#[derive(Default)]
struct Memory {
    sessions: HashMap<BrowserId, Session>,
    wire_ids: HashMap<BrowserId, BrowserId>,
    evaluations: HashMap<String, Evaluation>,
    queue: VecDeque<Request>,
    started: bool,
    shutdown: bool,
    failure: Option<String>,
    stderr: VecDeque<u8>,
}

#[derive(Default)]
struct Shared {
    memory: Mutex<Memory>,
    wake: Condvar,
}

struct Handle(Arc<Shared>);

impl Drop for Handle {
    fn drop(&mut self) {
        self.0.memory.lock().shutdown = true;
        self.0.wake.notify_one();
    }
}

#[derive(Clone)]
pub struct BrowserRuntime(Arc<Handle>);

impl Default for BrowserRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserRuntime {
    pub fn new() -> Self {
        Self(Arc::new(Handle(Arc::new(Shared::default()))))
    }

    pub fn open(&self, id: BrowserId, url: String) {
        let shared = &self.0.0;
        let mut memory = shared.memory.lock();
        if memory.sessions.contains_key(&id) {
            drop(memory);
            self.command(id, BrowserCommand::Navigate { url });
            return;
        }
        let wire_id = BrowserId::new_v4();
        let error = if memory.shutdown {
            Some("Browser runtime has shut down; reopen DROSTJE".into())
        } else {
            memory.failure.clone()
        };
        memory.sessions.insert(
            id,
            Session {
                wire_id,
                snapshot: BrowserSnapshot {
                    url: url.clone(),
                    title: String::new(),
                    loading: error.is_none(),
                    error,
                    can_go_back: false,
                    can_go_forward: false,
                    frame: None,
                },
            },
        );
        memory.wire_ids.insert(wire_id, id);
        if !memory.shutdown && memory.failure.is_none() {
            enqueue(&mut memory, Request::Open { id: wire_id, url });
        }
        let start = !memory.started && !memory.shutdown && memory.failure.is_none();
        memory.started |= start;
        drop(memory);
        if start {
            let worker_shared = shared.clone();
            if let Err(error) = thread::Builder::new()
                .name("servo-supervisor".into())
                .spawn(move || supervise(worker_shared))
            {
                fail(shared, format!("Could not start browser worker: {error}"));
            }
        }
        shared.wake.notify_one();
    }

    pub fn command(&self, id: BrowserId, command: BrowserCommand) {
        let shared = &self.0.0;
        let mut memory = shared.memory.lock();
        let wire_id = memory.sessions.get(&id).map(|session| session.wire_id);
        if let BrowserCommand::Evaluate { request_id, .. } = &command {
            if memory.evaluations.contains_key(request_id) {
                if let Some(session) = memory.sessions.get_mut(&id) {
                    session.snapshot.error = Some(format!(
                        "JavaScript request ID {request_id:?} is already in use"
                    ));
                }
                return;
            }
            // Completed responses remain until consumed. Reject rather than silently
            // evict a result a caller is still waiting for.
            if memory.evaluations.len() >= MAX_PENDING {
                if let Some(session) = memory.sessions.get_mut(&id) {
                    session.snapshot.error = Some("Too many pending JavaScript results; consume previous results before evaluating again".into());
                }
                return;
            }
            let result = if wire_id.is_none() {
                Some(Err("Browser session is closed".into()))
            } else if memory.shutdown {
                Some(Err("Browser runtime has shut down".into()))
            } else {
                memory.failure.clone().map(Err)
            };
            memory.evaluations.insert(
                request_id.clone(),
                Evaluation {
                    id,
                    started: Instant::now(),
                    result,
                },
            );
        }
        let Some(wire_id) = wire_id else { return };
        if memory.shutdown || memory.failure.is_some() {
            return;
        }
        if matches!(
            command,
            BrowserCommand::Navigate { .. }
                | BrowserCommand::Reload
                | BrowserCommand::Back
                | BrowserCommand::Forward
        ) && let Some(session) = memory.sessions.get_mut(&id)
        {
            session.snapshot.error = None;
            session.snapshot.loading = true;
            if let BrowserCommand::Navigate { url } = &command {
                session.snapshot.url.clone_from(url);
            }
        }
        enqueue(
            &mut memory,
            Request::Command {
                id: wire_id,
                command,
            },
        );
        drop(memory);
        shared.wake.notify_one();
    }

    pub fn close(&self, id: BrowserId) {
        let shared = &self.0.0;
        let mut memory = shared.memory.lock();
        if let Some(session) = memory.sessions.remove(&id) {
            memory.wire_ids.remove(&session.wire_id);
            memory
                .queue
                .retain(|request| request.id() != Some(session.wire_id));
            // Lifecycle messages have priority over replaceable input and cannot be lost.
            memory.queue.push_front(Request::Close {
                id: session.wire_id,
            });
        }
        for evaluation in memory
            .evaluations
            .values_mut()
            .filter(|evaluation| evaluation.id == id && evaluation.result.is_none())
        {
            evaluation.result = Some(Err(
                "Browser session closed before JavaScript evaluation completed".into(),
            ));
        }
        drop(memory);
        shared.wake.notify_one();
    }

    pub fn snapshot(&self, id: BrowserId) -> Option<BrowserSnapshot> {
        self.0
            .0
            .memory
            .lock()
            .sessions
            .get(&id)
            .map(|session| session.snapshot.clone())
    }

    pub fn evaluation(&self, request_id: &str) -> Option<Result<Value, String>> {
        let mut memory = self.0.0.memory.lock();
        memory.evaluations.get(request_id)?.result.as_ref()?;
        memory.evaluations.remove(request_id)?.result
    }

    pub fn shutdown(&self) {
        let shared = &self.0.0;
        let mut memory = shared.memory.lock();
        memory.shutdown = true;
        memory.queue.clear();
        for evaluation in memory
            .evaluations
            .values_mut()
            .filter(|evaluation| evaluation.result.is_none())
        {
            evaluation.result = Some(Err("Browser runtime shut down".into()));
        }
        drop(memory);
        shared.wake.notify_one();
    }
}

fn enqueue(memory: &mut Memory, request: Request) {
    if memory
        .queue
        .back()
        .is_some_and(|previous| request.supersedes(previous))
    {
        memory.queue.pop_back();
    }
    if memory.queue.len() >= MAX_PENDING {
        let error = "Browser input queue is full; wait for the page to respond before trying again";
        if let Some(id) = request.id().and_then(|id| memory.wire_ids.get(&id))
            && let Some(session) = memory.sessions.get_mut(id)
        {
            session.snapshot.error = Some(error.into());
            session.snapshot.loading = false;
        }
        if let Request::Command {
            command: BrowserCommand::Evaluate { request_id, .. },
            ..
        } = request
            && let Some(evaluation) = memory.evaluations.get_mut(&request_id)
        {
            evaluation.result = Some(Err(error.into()));
        }
        return;
    }
    memory.queue.push_back(request);
}

fn helper_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("DROSTJE_SERVO") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        let path = parent.join("drostje-servo");
        if path.is_file() {
            return Ok(path);
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for profile in ["debug", "release"] {
        let path = root
            .join("crates/servo-browser/target")
            .join(profile)
            .join("drostje-servo");
        if path.is_file() {
            return Ok(path);
        }
    }
    Err("Servo helper not found. Build it with `cargo build --manifest-path crates/servo-browser/Cargo.toml --locked`, reinstall with ./install.sh, or set DROSTJE_SERVO to the drostje-servo executable".into())
}

fn fail(shared: &Shared, error: String) {
    let mut memory = shared.memory.lock();
    memory.failure = Some(error.clone());
    for session in memory.sessions.values_mut() {
        session.snapshot.loading = false;
        session.snapshot.error = Some(error.clone());
    }
    for evaluation in memory
        .evaluations
        .values_mut()
        .filter(|evaluation| evaluation.result.is_none())
    {
        evaluation.result = Some(Err(error.clone()));
    }
    shared.wake.notify_one();
}

fn apply_event(memory: &mut Memory, event: Event, bytes: Vec<u8>) -> io::Result<()> {
    if !matches!(event, Event::Frame { .. }) && !bytes.is_empty() {
        return Err(protocol::invalid(
            "Unexpected binary data in Servo status message",
        ));
    }
    match event {
        Event::State {
            id,
            state:
                PageState {
                    url,
                    title,
                    loading,
                    error,
                    can_go_back,
                    can_go_forward,
                },
        } => {
            if let Some(id) = memory.wire_ids.get(&id)
                && let Some(session) = memory.sessions.get_mut(id)
            {
                let snapshot = &mut session.snapshot;
                snapshot.url = url;
                snapshot.title = title;
                snapshot.loading = loading;
                // Local transport/input failures survive unrelated title/frame updates.
                if error.is_some() {
                    snapshot.error = error;
                }
                snapshot.can_go_back = can_go_back;
                snapshot.can_go_forward = can_go_forward;
            }
        }
        Event::Frame {
            id,
            revision,
            width,
            height,
        } => {
            if bytes.len() != protocol::frame_len(width, height)? {
                return Err(protocol::invalid(
                    "Servo frame length does not match its dimensions",
                ));
            }
            if let Some(id) = memory.wire_ids.get(&id)
                && let Some(session) = memory.sessions.get_mut(id)
                && session
                    .snapshot
                    .frame
                    .as_ref()
                    .is_none_or(|frame| revision > frame.revision)
            {
                session.snapshot.frame = Some(Arc::new(BrowserFrame {
                    revision,
                    width,
                    height,
                    rgba: bytes,
                }));
            }
        }
        Event::Evaluation {
            id,
            request_id,
            result,
        } => {
            if let Some(evaluation) = memory.evaluations.get_mut(&request_id)
                && memory.wire_ids.get(&id) == Some(&evaluation.id)
                && evaluation.result.is_none()
            {
                evaluation.result = Some(result);
            }
        }
        Event::Fatal { error } => return Err(io::Error::other(error)),
    }
    Ok(())
}

fn supervise(shared: Arc<Shared>) {
    let result = run_helper(&shared);
    if let Err(error) = result {
        let stderr = {
            let memory = shared.memory.lock();
            String::from_utf8_lossy(&memory.stderr.iter().copied().collect::<Vec<_>>())
                .trim()
                .to_owned()
        };
        let detail = if stderr.is_empty() {
            String::new()
        } else {
            format!("\nServo stderr: {stderr}")
        };
        fail(
            &shared,
            format!(
                "{error}{detail}\nRebuild/reinstall drostje-servo and reopen DROSTJE to restart the browser"
            ),
        );
    }
}

fn run_helper(shared: &Arc<Shared>) -> Result<(), String> {
    let path = helper_path()?;
    let mut child = Command::new(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not launch Servo helper {}: {error}", path.display()))?;
    let result = communicate(shared, &mut child);
    // This supervisor owns process reaping even when startup, a pipe or a worker fails.
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn communicate(shared: &Arc<Shared>, child: &mut Child) -> Result<(), String> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or("Servo helper stdin was not piped")?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or("Servo helper stdout was not piped")?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or("Servo helper stderr was not piped")?;
    let (sender, receiver) = mpsc::sync_channel::<Request>(32);
    let writer_shared = shared.clone();
    thread::Builder::new()
        .name("servo-input".into())
        .spawn(move || {
            while let Ok(request) = receiver.recv() {
                if let Err(error) = protocol::write_packet(&mut stdin, &request, &[]) {
                    fail(
                        &writer_shared,
                        format!("Could not send browser input: {error}"),
                    );
                    break;
                }
            }
        })
        .map_err(|error| format!("Could not start Servo input worker: {error}"))?;
    let reader_shared = shared.clone();
    thread::Builder::new()
        .name("servo-output".into())
        .spawn(move || {
            let result = (|| -> io::Result<()> {
                while let Some((event, bytes)) = protocol::read_packet::<Event>(&mut stdout)? {
                    apply_event(&mut reader_shared.memory.lock(), event, bytes)?;
                }
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Servo helper closed its output",
                ))
            })();
            if let Err(error) = result
                && !reader_shared.memory.lock().shutdown
            {
                fail(&reader_shared, error.to_string());
            }
        })
        .map_err(|error| format!("Could not start Servo output worker: {error}"))?;
    let stderr_shared = shared.clone();
    let stderr_worker = thread::Builder::new()
        .name("servo-stderr".into())
        .spawn(move || {
            let mut buffer = [0; 2048];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(length) => {
                        let mut memory = stderr_shared.memory.lock();
                        memory.stderr.extend(&buffer[..length]);
                        let excess = memory.stderr.len().saturating_sub(8192);
                        memory.stderr.drain(..excess);
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })
        .map_err(|error| format!("Could not start Servo diagnostics worker: {error}"))?;
    let mut stopping = None;
    let result = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Could not inspect Servo helper: {error}"))?
        {
            break if stopping.is_some() {
                Ok(())
            } else {
                Err(format!("Servo helper exited ({status})"))
            };
        }
        let mut memory = shared.memory.lock();
        if memory.shutdown {
            let deadline = stopping.get_or_insert_with(|| {
                let _ = sender.try_send(Request::Shutdown);
                Instant::now() + Duration::from_secs(2)
            });
            if Instant::now() >= *deadline {
                break Ok(());
            }
        } else if let Some(error) = &memory.failure {
            break Err(error.clone());
        } else {
            for evaluation in memory
                .evaluations
                .values_mut()
                .filter(|evaluation| evaluation.result.is_none())
            {
                if evaluation.started.elapsed() > Duration::from_secs(30) {
                    evaluation.result = Some(Err("JavaScript evaluation did not finish within 30 seconds; the page may be busy or navigating".into()));
                }
            }
            while let Some(request) = memory.queue.pop_front() {
                match sender.try_send(request) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full(request)) => {
                        memory.queue.push_front(request);
                        break;
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        memory.failure = Some("Servo input worker stopped".into());
                        break;
                    }
                }
            }
        }
        shared.wake.wait_for(&mut memory, Duration::from_millis(25));
    };
    let _ = child.kill();
    let _ = child.wait();
    drop(sender);
    let _ = stderr_worker.join();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_with_session() -> (Memory, BrowserId, BrowserId) {
        let id = BrowserId::new_v4();
        let wire_id = BrowserId::new_v4();
        let mut memory = Memory::default();
        memory.wire_ids.insert(wire_id, id);
        memory.sessions.insert(
            id,
            Session {
                wire_id,
                snapshot: BrowserSnapshot {
                    url: "about:blank".into(),
                    title: String::new(),
                    loading: false,
                    error: None,
                    can_go_back: false,
                    can_go_forward: false,
                    frame: None,
                },
            },
        );
        (memory, id, wire_id)
    }

    #[test]
    fn frames_are_bounded_validated_and_stale_revisions_are_ignored() {
        let (mut memory, id, wire_id) = memory_with_session();
        let frame = |revision| Event::Frame {
            id: wire_id,
            revision,
            width: 1,
            height: 1,
        };
        assert!(apply_event(&mut memory, frame(1), vec![1, 2]).is_err());
        apply_event(&mut memory, frame(2), vec![1, 2, 3, 255]).unwrap();
        apply_event(&mut memory, frame(1), vec![9; 4]).unwrap();
        assert_eq!(
            memory.sessions[&id].snapshot.frame.as_ref().unwrap().rgba,
            [1, 2, 3, 255]
        );
        memory.wire_ids.remove(&wire_id);
        apply_event(&mut memory, frame(3), vec![9; 4]).unwrap();
        assert_eq!(
            memory.sessions[&id]
                .snapshot
                .frame
                .as_ref()
                .unwrap()
                .revision,
            2
        );
    }

    #[test]
    fn navigation_status_does_not_erase_transport_errors() {
        let (mut memory, id, wire_id) = memory_with_session();
        memory.sessions.get_mut(&id).unwrap().snapshot.error = Some("pipe failed".into());
        apply_event(
            &mut memory,
            Event::State {
                id: wire_id,
                state: PageState {
                    title: "Loaded".into(),
                    ..PageState::default()
                },
            },
            vec![],
        )
        .unwrap();
        assert_eq!(
            memory.sessions[&id].snapshot.error.as_deref(),
            Some("pipe failed")
        );
    }

    #[test]
    fn closing_completes_pending_evaluations_without_launching_a_helper() {
        let runtime = BrowserRuntime::new();
        let id = BrowserId::new_v4();
        runtime.command(
            id,
            BrowserCommand::Evaluate {
                request_id: "missing".into(),
                script: "1".into(),
            },
        );
        assert!(runtime.evaluation("missing").unwrap().is_err());
        assert!(runtime.evaluation("missing").is_none());
        assert!(!runtime.0.0.memory.lock().started);
    }
}
