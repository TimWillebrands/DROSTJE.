//! The UI touches retained memory only. Servo and all rendering handles live on
//! a dedicated worker thread; commands and evaluation results are bounded.
use std::{
    collections::{HashMap, VecDeque},
    ffi::OsStr,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use serde_json::Value;

mod command;
mod diagnostics;
mod engine;
pub(crate) use command::frame_len;
pub use command::{BrowserCommand, ChannelEvent, ReceiptStatus};
use command::{Event, MAX_PENDING, PageState, Request};
use diagnostics::Diagnostics;

pub type BrowserId = uuid::Uuid;

static CONTENT_PROCESSES: AtomicBool = AtomicBool::new(false);

/// Must run first in `main`, before any other startup work. Servo re-executes
/// `current_exe` as `--content-process <token>` to host a site's script and
/// layout; such a process is served here and `true` means the caller must exit.
/// Otherwise engines started by this process run pages in content processes, so
/// a page that crashes its process shows Servo's crash page instead of taking
/// DROSTJE down. Binaries that never call this (libtest) keep pages in-process.
pub fn serve_content_process() -> bool {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new("--content-process")) {
        CONTENT_PROCESSES.store(true, Ordering::Relaxed);
        return false;
    }
    match args.next().and_then(|token| token.into_string().ok()) {
        Some(token) => engine::run_content_process(token),
        None => eprintln!("A Servo content process was started without a valid token"),
    }
    true
}

#[derive(Clone, Debug)]
pub struct BrowserSnapshot {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub error: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// Natural document height in CSS pixels, measured only for opted-in artifacts.
    pub content_height: Option<f32>,
    pub document_revision: u64,
    /// Physical surface dimensions; inspect exposes CSS dimensions.
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub scale: f32,
    pub frame: Option<Arc<BrowserFrame>>,
}

#[derive(Debug)]
pub struct BrowserFrame {
    pub revision: u64,
    pub document_revision: u64,
    pub scale: f32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

struct Session {
    wire_id: BrowserId,
    content_sizing: bool,
    /// Theme tokens for an artifact surface. Only artifacts get the kit and a channel;
    /// fixed when the surface opens, because the engine injects it at WebView creation.
    artifact: Option<String>,
    /// Channel events not yet taken by the presentation owner.
    channel: VecDeque<ChannelEvent>,
    snapshot: BrowserSnapshot,
    diagnostics: Diagnostics,
}

/// Unread channel events per artifact; the pane drains them every frame.
const MAX_CHANNEL_BACKLOG: usize = 64;

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
    stopped: bool,
}

#[derive(Default)]
struct Shared {
    memory: Mutex<Memory>,
    worker: Mutex<Option<thread::Thread>>,
}

impl Shared {
    fn wake(&self) {
        if let Some(worker) = self.worker.lock().as_ref() {
            worker.unpark();
        }
    }
}

struct Handle(Arc<Shared>);

impl Drop for Handle {
    fn drop(&mut self) {
        self.0.memory.lock().shutdown = true;
        self.0.wake();
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
        self.open_surface(id, url, None);
    }

    /// Open an artifact surface: the engine injects the artifact kit with these theme
    /// tokens and accepts `drostje.send` channel events from its file documents.
    pub fn open_artifact(&self, id: BrowserId, url: String, theme: String) {
        self.open_surface(id, url, Some(theme));
    }

    fn open_surface(&self, id: BrowserId, url: String, artifact: Option<String>) {
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
                content_sizing: false,
                artifact,
                channel: VecDeque::new(),
                diagnostics: Diagnostics::default(),
                snapshot: BrowserSnapshot {
                    url: url.clone(),
                    title: String::new(),
                    loading: error.is_none(),
                    error,
                    can_go_back: false,
                    can_go_forward: false,
                    content_height: None,
                    document_revision: 0,
                    viewport_width: 800,
                    viewport_height: 600,
                    scale: 1.0,
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
                .name("servo-engine".into())
                .stack_size(8 * 1024 * 1024)
                .spawn(move || run_worker(worker_shared))
            {
                fail(shared, format!("Could not start browser worker: {error}"));
            }
        }
        shared.wake();
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
            let navigates = match &command {
                BrowserCommand::Back => session.snapshot.can_go_back,
                BrowserCommand::Forward => session.snapshot.can_go_forward,
                _ => true,
            };
            if navigates {
                session.snapshot.loading = true;
                session.snapshot.content_height = None;
                session.snapshot.frame = None;
            }
            if let BrowserCommand::Navigate { url } = &command {
                session.snapshot.url.clone_from(url);
            }
        }
        if let BrowserCommand::SetContentSizing { enabled } = &command
            && let Some(session) = memory.sessions.get_mut(&id)
        {
            session.content_sizing = *enabled;
        }
        if let BrowserCommand::ArtifactTheme { css } = &command
            && let Some(session) = memory.sessions.get_mut(&id)
        {
            if session.artifact.is_none() {
                return;
            }
            session.artifact = Some(css.clone());
        }
        enqueue(
            &mut memory,
            Request::Command {
                id: wire_id,
                command,
            },
        );
        drop(memory);
        shared.wake();
    }

    /// Enqueue one indivisible input batch. Completion acknowledges delivery to
    /// Servo, not page-script handling, navigation completion or rendering.
    pub fn dispatch(&self, id: BrowserId, commands: Vec<BrowserCommand>) -> Result<String, String> {
        self.dispatch_batch(id, commands, None)
    }

    pub fn dispatch_in_document(
        &self,
        id: BrowserId,
        commands: Vec<BrowserCommand>,
        expected_document_revision: u64,
    ) -> Result<String, String> {
        self.dispatch_batch(id, commands, Some(expected_document_revision))
    }

    fn dispatch_batch(
        &self,
        id: BrowserId,
        commands: Vec<BrowserCommand>,
        expected_document_revision: Option<u64>,
    ) -> Result<String, String> {
        let shared = &self.0.0;
        let mut memory = shared.memory.lock();
        if memory.shutdown {
            return Err("Browser runtime has shut down".into());
        }
        if let Some(error) = &memory.failure {
            return Err(error.clone());
        }
        let wire_id = memory
            .sessions
            .get(&id)
            .ok_or("Browser session is closed")?
            .wire_id;
        if commands.is_empty() || commands.len() > MAX_PENDING {
            return Err(format!(
                "Browser dispatch requires 1–{MAX_PENDING} commands"
            ));
        }
        if commands
            .iter()
            .any(|command| matches!(command, BrowserCommand::Evaluate { .. }))
        {
            return Err(
                "Evaluate is asynchronous; submit it separately with its own request ID".into(),
            );
        }
        if memory.evaluations.len() >= MAX_PENDING {
            return Err(
                "Too many pending browser results; consume or discard previous results".into(),
            );
        }
        if memory.queue.iter().map(Request::cost).sum::<usize>() + commands.len() > MAX_PENDING {
            return Err(
                "Browser input queue is full; wait for pending commands before retrying".into(),
            );
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        memory.evaluations.insert(
            request_id.clone(),
            Evaluation {
                id,
                started: Instant::now(),
                result: None,
            },
        );
        memory.queue.push_back(Request::Batch {
            id: wire_id,
            request_id: request_id.clone(),
            commands,
            expected_document_revision,
        });
        drop(memory);
        shared.wake();
        Ok(request_id)
    }

    /// Asynchronous JavaScript evaluation with explicit admission failure.
    pub fn evaluate(&self, id: BrowserId, script: String) -> Result<String, String> {
        let shared = &self.0.0;
        let mut memory = shared.memory.lock();
        if memory.shutdown {
            return Err("Browser runtime has shut down".into());
        }
        if let Some(error) = &memory.failure {
            return Err(error.clone());
        }
        let wire_id = memory
            .sessions
            .get(&id)
            .ok_or("Browser session is closed")?
            .wire_id;
        if memory.evaluations.len() >= MAX_PENDING {
            return Err(
                "Too many pending browser results; consume or discard previous results".into(),
            );
        }
        if memory.queue.iter().map(Request::cost).sum::<usize>() >= MAX_PENDING {
            return Err(
                "Browser input queue is full; wait for pending commands before retrying".into(),
            );
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        memory.evaluations.insert(
            request_id.clone(),
            Evaluation {
                id,
                started: Instant::now(),
                result: None,
            },
        );
        memory.queue.push_back(Request::Command {
            id: wire_id,
            command: BrowserCommand::Evaluate {
                request_id: request_id.clone(),
                script,
            },
        });
        drop(memory);
        shared.wake();
        Ok(request_id)
    }

    pub fn discard_evaluation(&self, request_id: &str) {
        self.0.0.memory.lock().evaluations.remove(request_id);
    }

    pub fn inspect(&self, id: BrowserId) -> Option<Value> {
        let snapshot = self.snapshot(id)?;
        Some(serde_json::json!({
            "url": snapshot.url, "title": snapshot.title, "loading": snapshot.loading,
            "error": snapshot.error, "can_go_back": snapshot.can_go_back,
            "can_go_forward": snapshot.can_go_forward, "content_height": snapshot.content_height,
            "document_revision": snapshot.document_revision,
            "viewport": { "width": snapshot.viewport_width as f32 / snapshot.scale,
                "height": snapshot.viewport_height as f32 / snapshot.scale, "scale": snapshot.scale },
            "frame": snapshot.frame.as_ref().map(|frame| serde_json::json!({
                "revision": frame.revision, "document_revision": frame.document_revision,
                "width": frame.width, "height": frame.height, "scale": frame.scale,
            })),
        }))
    }

    /// Channel events an artifact sent since the last call, oldest first.
    pub fn take_channel(&self, id: BrowserId) -> Vec<ChannelEvent> {
        let mut memory = self.0.0.memory.lock();
        memory
            .sessions
            .get_mut(&id)
            .map(|session| session.channel.drain(..).collect())
            .unwrap_or_default()
    }

    /// Tell an artifact page what happened to one of its events.
    pub fn receipt(
        &self,
        id: BrowserId,
        event: String,
        status: ReceiptStatus,
        reason: Option<String>,
    ) {
        self.command(
            id,
            BrowserCommand::Receipt {
                event,
                status,
                reason,
            },
        );
    }

    pub fn is_artifact(&self, id: BrowserId) -> bool {
        self.0
            .0
            .memory
            .lock()
            .sessions
            .get(&id)
            .is_some_and(|session| session.artifact.is_some())
    }

    pub fn diagnostics(&self, id: BrowserId, after: u64) -> Option<Value> {
        let memory = self.0.0.memory.lock();
        Some(memory.sessions.get(&id)?.diagnostics.read(after))
    }

    pub fn close(&self, id: BrowserId) {
        let shared = &self.0.0;
        let mut memory = shared.memory.lock();
        if let Some(session) = memory.sessions.remove(&id) {
            memory.wire_ids.remove(&session.wire_id);
            memory
                .queue
                .retain(|request| request.id() != session.wire_id);
            // The worker reconciles live identities before each bounded batch.
            // No Close message can be dropped or grow beyond the input bound.
        }
        for evaluation in memory
            .evaluations
            .values_mut()
            .filter(|evaluation| evaluation.id == id && evaluation.result.is_none())
        {
            evaluation.result = Some(Err(
                "Browser session closed before the request completed".into()
            ));
        }
        drop(memory);
        shared.wake();
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
        shared.wake();
    }

    /// Waits for the worker to finish Servo's cooperative shutdown after
    /// `shutdown`; exiting the process during that teardown crashes in Servo's
    /// threads. Returns `false` when the timeout elapsed first.
    pub fn wait_for_shutdown(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let memory = self.0.0.memory.lock();
                if memory.stopped || !memory.started {
                    return true;
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
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
    if memory.queue.iter().map(Request::cost).sum::<usize>() + request.cost() > MAX_PENDING {
        let error = "Browser input queue is full; wait for the page to respond before trying again";
        if let Some(id) = memory.wire_ids.get(&request.id())
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
    shared.wake();
}

fn apply_event(memory: &mut Memory, event: Event, bytes: Vec<u8>) -> io::Result<()> {
    if !matches!(event, Event::Frame { .. }) && !bytes.is_empty() {
        return Err(command::invalid(
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
                    content_height,
                    document_revision,
                    viewport_width,
                    viewport_height,
                    scale,
                },
        } => {
            if let Some(id) = memory.wire_ids.get(&id)
                && let Some(session) = memory.sessions.get_mut(id)
            {
                let snapshot = &mut session.snapshot;
                if loading && !snapshot.loading {
                    snapshot.error = None;
                }
                if snapshot.document_revision != document_revision
                    || loading
                    || snapshot.viewport_width != viewport_width
                    || snapshot.viewport_height != viewport_height
                    || snapshot.scale != scale
                {
                    snapshot.frame = None;
                }
                snapshot.document_revision = document_revision;
                snapshot.viewport_width = viewport_width;
                snapshot.viewport_height = viewport_height;
                snapshot.scale = scale;
                snapshot.url = url;
                snapshot.title = title;
                snapshot.loading = loading;
                // Local queue/input failures survive unrelated title/frame updates.
                if error.is_some() {
                    snapshot.error = error;
                }
                snapshot.can_go_back = can_go_back;
                snapshot.can_go_forward = can_go_forward;
                snapshot.content_height = content_height;
            }
        }
        Event::Frame {
            id,
            revision,
            document_revision,
            scale,
            width,
            height,
        } => {
            if bytes.len() != command::frame_len(width, height)? {
                return Err(command::invalid(
                    "Servo frame length does not match its dimensions",
                ));
            }
            if let Some(id) = memory.wire_ids.get(&id)
                && let Some(session) = memory.sessions.get_mut(id)
                && !session.snapshot.loading
                && document_revision == session.snapshot.document_revision
                && scale == session.snapshot.scale
                && width == session.snapshot.viewport_width
                && height == session.snapshot.viewport_height
                && session
                    .snapshot
                    .frame
                    .as_ref()
                    .is_none_or(|frame| revision > frame.revision)
            {
                session.snapshot.frame = Some(Arc::new(BrowserFrame {
                    revision,
                    document_revision,
                    scale,
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
        Event::Diagnostic {
            id,
            kind,
            level,
            message,
        } => {
            if let Some(id) = memory.wire_ids.get(&id)
                && let Some(session) = memory.sessions.get_mut(id)
            {
                session.diagnostics.push(kind, level, message);
            }
        }
        Event::Channel { id, event } => {
            let Some(id) = memory.wire_ids.get(&id).copied() else {
                return Ok(());
            };
            let Some(session) = memory.sessions.get_mut(&id) else {
                return Ok(());
            };
            if session.artifact.is_none() {
                return Ok(());
            }
            if session.channel.len() < MAX_CHANNEL_BACKLOG {
                session.channel.push_back(event);
            } else {
                let wire_id = session.wire_id;
                enqueue(
                    memory,
                    Request::Command {
                        id: wire_id,
                        command: BrowserCommand::Receipt {
                            event: event.id,
                            status: ReceiptStatus::Failed,
                            reason: Some(
                                "Too many unsent events; wait for earlier ones to settle".into(),
                            ),
                        },
                    },
                );
            }
        }
        Event::Fatal { error } => return Err(io::Error::other(error)),
    }
    Ok(())
}

fn run_worker(shared: Arc<Shared>) {
    *shared.worker.lock() = Some(thread::current());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine::run(&shared)));
    let error = match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(payload) => Some(format!(
            "Servo worker panicked: {}",
            payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic")
        )),
    };
    if let Some(error) = error {
        fail(
            &shared,
            format!("{error}\nReopen DROSTJE to restart the browser"),
        );
    }
    shared.memory.lock().stopped = true;
    *shared.worker.lock() = None;
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
                content_sizing: false,
                artifact: None,
                channel: VecDeque::new(),
                diagnostics: Diagnostics::default(),
                snapshot: BrowserSnapshot {
                    url: "about:blank".into(),
                    title: String::new(),
                    loading: false,
                    error: None,
                    can_go_back: false,
                    can_go_forward: false,
                    content_height: None,
                    document_revision: 0,
                    viewport_width: 1,
                    viewport_height: 1,
                    scale: 1.0,
                    frame: None,
                },
            },
        );
        (memory, id, wire_id)
    }

    #[test]
    fn dispatch_rejects_full_batches_without_partial_queue_acceptance() {
        let (mut memory, id, _) = memory_with_session();
        memory.started = true;
        let runtime = BrowserRuntime::new();
        *runtime.0.0.memory.lock() = memory;
        let commands = || vec![BrowserCommand::Move { x: 1.0, y: 2.0 }; MAX_PENDING];
        let request_id = runtime.dispatch(id, commands()).unwrap();
        assert!(
            runtime.evaluation(&request_id).is_none(),
            "queued is not delivered"
        );
        assert!(runtime.dispatch(id, vec![BrowserCommand::Reload]).is_err());
        let memory = runtime.0.0.memory.lock();
        assert_eq!(memory.queue.len(), 1);
        assert_eq!(memory.queue.front().unwrap().cost(), MAX_PENDING);
        assert_eq!(memory.evaluations.len(), 1);
        drop(memory);
        assert!(
            runtime
                .evaluate(id, "1".into())
                .unwrap_err()
                .contains("queue is full")
        );
        runtime.close(id);
        assert!(
            runtime
                .evaluation(&request_id)
                .unwrap()
                .unwrap_err()
                .contains("closed")
        );
    }

    #[test]
    fn dispatch_shutdown_and_discard_complete_without_an_engine() {
        let (memory, id, _) = memory_with_session();
        let runtime = BrowserRuntime::new();
        *runtime.0.0.memory.lock() = memory;
        let first = runtime.dispatch(id, vec![BrowserCommand::Reload]).unwrap();
        let second = runtime.dispatch(id, vec![BrowserCommand::Reload]).unwrap();
        runtime.discard_evaluation(&first);
        assert!(runtime.evaluation(&first).is_none());
        runtime.shutdown();
        assert!(
            runtime
                .evaluation(&second)
                .unwrap()
                .unwrap_err()
                .contains("shut down")
        );
        assert!(runtime.dispatch(id, vec![BrowserCommand::Reload]).is_err());
        assert!(runtime.0.0.memory.lock().queue.is_empty());
        assert!(
            runtime
                .evaluate(id, "1".into())
                .unwrap_err()
                .contains("shut down")
        );
    }

    #[test]
    fn explicit_evaluation_rejects_retained_result_overflow_and_missing_sessions() {
        let (memory, id, _) = memory_with_session();
        let runtime = BrowserRuntime::new();
        *runtime.0.0.memory.lock() = memory;
        assert!(runtime.evaluate(BrowserId::new_v4(), "1".into()).is_err());
        for _ in 0..MAX_PENDING {
            runtime.evaluate(id, "1".into()).unwrap();
        }
        runtime.0.0.memory.lock().queue.clear();
        assert!(
            runtime
                .evaluate(id, "1".into())
                .unwrap_err()
                .contains("Too many pending")
        );
        assert_eq!(runtime.0.0.memory.lock().evaluations.len(), MAX_PENDING);
    }

    #[test]
    fn navigation_and_resize_reject_old_document_pixels() {
        let (mut memory, id, wire_id) = memory_with_session();
        let frame = |document_revision, scale| Event::Frame {
            id: wire_id,
            revision: 1,
            document_revision,
            scale,
            width: 1,
            height: 1,
        };
        apply_event(&mut memory, frame(0, 1.0), vec![0; 4]).unwrap();
        assert!(memory.sessions[&id].snapshot.frame.is_some());
        apply_event(
            &mut memory,
            Event::State {
                id: wire_id,
                state: PageState {
                    loading: true,
                    document_revision: 1,
                    viewport_width: 1,
                    viewport_height: 1,
                    scale: 1.0,
                    ..PageState::default()
                },
            },
            vec![],
        )
        .unwrap();
        assert!(memory.sessions[&id].snapshot.frame.is_none());
        apply_event(&mut memory, frame(1, 1.0), vec![0; 4]).unwrap();
        assert!(
            memory.sessions[&id].snapshot.frame.is_none(),
            "loading cannot publish old pixels"
        );
        memory.sessions.get_mut(&id).unwrap().snapshot.loading = false;
        apply_event(&mut memory, frame(0, 1.0), vec![0; 4]).unwrap();
        apply_event(&mut memory, frame(1, 2.0), vec![0; 4]).unwrap();
        assert!(memory.sessions[&id].snapshot.frame.is_none());
        apply_event(&mut memory, frame(1, 1.0), vec![0; 4]).unwrap();
        assert_eq!(
            memory.sessions[&id]
                .snapshot
                .frame
                .as_ref()
                .unwrap()
                .document_revision,
            1
        );
    }

    #[test]
    fn frames_are_bounded_validated_and_stale_revisions_are_ignored() {
        let (mut memory, id, wire_id) = memory_with_session();
        let frame = |revision| Event::Frame {
            id: wire_id,
            revision,
            document_revision: 0,
            scale: 1.0,
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
    fn navigation_status_does_not_erase_input_errors() {
        let (mut memory, id, wire_id) = memory_with_session();
        memory.sessions.get_mut(&id).unwrap().snapshot.error = Some("queue full".into());
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
            Some("queue full")
        );
    }

    #[test]
    fn missing_session_evaluation_finishes_without_starting_the_engine() {
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

    #[test]
    fn only_artifacts_hold_channel_events_and_a_full_backlog_tells_the_page() {
        let (mut memory, id, wire_id) = memory_with_session();
        let event = |n: usize| ChannelEvent {
            id: format!("e{n}"),
            kind: "answer".into(),
            body: "{}".into(),
        };
        apply_event(
            &mut memory,
            Event::Channel {
                id: wire_id,
                event: event(0),
            },
            vec![],
        )
        .unwrap();
        assert!(
            memory.sessions[&id].channel.is_empty(),
            "ordinary pages have no channel"
        );
        memory.sessions.get_mut(&id).unwrap().artifact = Some(":root{}".into());
        for n in 0..=MAX_CHANNEL_BACKLOG {
            apply_event(
                &mut memory,
                Event::Channel {
                    id: wire_id,
                    event: event(n),
                },
                vec![],
            )
            .unwrap();
        }
        assert_eq!(memory.sessions[&id].channel.len(), MAX_CHANNEL_BACKLOG);
        assert!(matches!(
            memory.queue.back(),
            Some(Request::Command {
                command: BrowserCommand::Receipt { event, status: ReceiptStatus::Failed, .. },
                ..
            }) if event == &format!("e{MAX_CHANNEL_BACKLOG}")
        ));
    }

    #[test]
    fn closing_cancels_queued_open_and_evaluations_without_a_lifecycle_backlog() {
        let runtime = BrowserRuntime::new();
        // Hold startup so this test observes the queue before the worker consumes it.
        runtime.0.0.memory.lock().started = true;
        for _ in 0..MAX_PENDING * 2 {
            let id = BrowserId::new_v4();
            runtime.open(id, "about:blank".into());
            runtime.command(
                id,
                BrowserCommand::Evaluate {
                    request_id: "closing".into(),
                    script: "1".into(),
                },
            );
            runtime.close(id);
            assert!(runtime.snapshot(id).is_none());
            assert!(runtime.evaluation("closing").unwrap().is_err());
        }
        let memory = runtime.0.0.memory.lock();
        assert!(memory.queue.is_empty());
        assert!(memory.wire_ids.is_empty());
    }

    #[test]
    fn full_input_queue_completes_rejected_evaluation_and_close_still_works() {
        let (mut memory, id, wire_id) = memory_with_session();
        memory.started = true;
        let runtime = BrowserRuntime::new();
        *runtime.0.0.memory.lock() = memory;
        for _ in 0..MAX_PENDING {
            runtime.command(id, BrowserCommand::Reload);
        }
        runtime.command(
            id,
            BrowserCommand::Evaluate {
                request_id: "full".into(),
                script: "1".into(),
            },
        );
        assert!(runtime.evaluation("full").unwrap().is_err());
        assert_eq!(runtime.0.0.memory.lock().queue.len(), MAX_PENDING);
        runtime.close(id);
        assert!(runtime.0.0.memory.lock().queue.is_empty());
        assert!(!runtime.0.0.memory.lock().wire_ids.contains_key(&wire_id));
    }

    #[test]
    fn completed_evaluations_are_retained_and_request_ids_cannot_overwrite_them() {
        let runtime = BrowserRuntime::new();
        let id = BrowserId::new_v4();
        for ix in 0..MAX_PENDING {
            runtime.command(
                id,
                BrowserCommand::Evaluate {
                    request_id: ix.to_string(),
                    script: "1".into(),
                },
            );
        }
        runtime
            .0
            .0
            .memory
            .lock()
            .evaluations
            .get_mut("0")
            .unwrap()
            .result = Some(Ok(serde_json::json!({"retained": true})));
        runtime.command(
            id,
            BrowserCommand::Evaluate {
                request_id: "overflow".into(),
                script: "2".into(),
            },
        );
        assert!(runtime.evaluation("overflow").is_none());
        runtime.command(
            id,
            BrowserCommand::Evaluate {
                request_id: "0".into(),
                script: "3".into(),
            },
        );
        assert_eq!(runtime.0.0.memory.lock().evaluations.len(), MAX_PENDING);
        assert_eq!(
            runtime.evaluation("0").unwrap().unwrap(),
            serde_json::json!({"retained": true})
        );
        for ix in 1..MAX_PENDING {
            assert!(runtime.evaluation(&ix.to_string()).unwrap().is_err());
        }
    }
}

#[cfg(test)]
mod smoke;
