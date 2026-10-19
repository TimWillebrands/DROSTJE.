//! The UI touches retained memory only. Servo and all rendering handles live on
//! a dedicated worker thread; commands and evaluation results are bounded.
use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::Arc,
    thread,
    time::Instant,
};

use parking_lot::Mutex;
use serde_json::Value;

mod command;
mod engine;
pub use command::BrowserCommand;
use command::{Event, MAX_PENDING, PageState, Request};

pub type BrowserId = uuid::Uuid;

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
    content_sizing: bool,
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
                snapshot: BrowserSnapshot {
                    url: url.clone(),
                    title: String::new(),
                    loading: error.is_none(),
                    error,
                    can_go_back: false,
                    can_go_forward: false,
                    content_height: None,
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
            session.snapshot.loading = true;
            session.snapshot.content_height = None;
            if let BrowserCommand::Navigate { url } = &command {
                session.snapshot.url.clone_from(url);
            }
        }
        if let BrowserCommand::SetContentSizing { enabled } = &command
            && let Some(session) = memory.sessions.get_mut(&id)
        {
            session.content_sizing = *enabled;
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
                "Browser session closed before JavaScript evaluation completed".into(),
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
                },
        } => {
            if let Some(id) = memory.wire_ids.get(&id)
                && let Some(session) = memory.sessions.get_mut(id)
            {
                let snapshot = &mut session.snapshot;
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
                snapshot: BrowserSnapshot {
                    url: "about:blank".into(),
                    title: String::new(),
                    loading: false,
                    error: None,
                    can_go_back: false,
                    can_go_forward: false,
                    content_height: None,
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
