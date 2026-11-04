use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, ensure};
use async_channel::{Receiver, Sender};
use parking_lot::Mutex;
use serde_json::Value;
use uuid::Uuid;

use super::{
    RemoteHostConfig,
    transport::{self, Role, TransportEvent},
    wire::{
        self, Operation, ReceiptStatus, RemoteConversationConfig, Request, Response, WireCommand,
        WireImage,
    },
};
use crate::{
    browser_tools::{BrowserToolOperation, BrowserToolRequest, BrowserToolServer},
    harness::{
        ContentBlock, ConversationCommand, ConversationEvent, ConversationHandle,
        ConversationState, PeerDeliveryOutcome, TurnOutcome,
    },
    network::Network,
};

const CAPACITY: usize = 256;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
type Reply<T> = Sender<Result<T>>;

/// One multiplexed, authenticated connection per configured Host. No mutation is replayed.
#[derive(Clone, Default)]
pub struct Client {
    hosts: Arc<Mutex<HashMap<String, Host>>>,
}
struct Host {
    config: RemoteHostConfig,
    requests: Sender<ClientRequest>,
    status: Arc<Mutex<String>>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.requests.close();
    }
}

enum ClientRequest {
    Open {
        thread: Uuid,
        generation: Uuid,
        project: String,
        config: RemoteConversationConfig,
        network: Network,
        artifacts: Option<BrowserToolServer>,
        resume: bool,
        reply: Reply<ConversationHandle>,
    },
    Query {
        thread: Uuid,
        operation: Operation,
        reply: Reply<Value>,
    },
    NetworkReply {
        thread: Uuid,
        epoch: Uuid,
        generation: Uuid,
        call: Uuid,
        result: std::result::Result<Value, String>,
    },
}
struct Runtime {
    project: String,
    config: RemoteConversationConfig,
    network: Network,
    /// The desktop's tool server, which shows this runtime's artifacts locally.
    artifacts: Option<BrowserToolServer>,
    capability: Uuid,
    generation: Option<Uuid>,
    open_id: Uuid,
    open_sent: bool,
    open_resume: bool,
    resume: bool,
    cursor: u64,
    recovered: bool,
    recovering_since: Option<Instant>,
    recovery_state: Option<ConversationState>,
    recovery_peer: Option<(bool, Option<String>)>,
    commands: Receiver<ConversationCommand>,
    events: Sender<ConversationEvent>,
    network_calls: HashSet<Uuid>,
    network_pending: usize,
    network_lease: (Sender<()>, Receiver<()>),
    handle: Option<ConversationHandle>,
    opening: Option<(Uuid, Reply<ConversationHandle>, Instant)>,
    pending: HashMap<Uuid, Pending>,
    cancellations: HashMap<Uuid, Receiver<()>>,
    image_dir: PathBuf,
}
struct Pending {
    identity: Identity,
    since: Instant,
}
enum Identity {
    Submit(Uuid),
    Peer(Uuid),
    Other,
    Query(Reply<Value>),
}

impl Client {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self, host: &str) -> String {
        self.hosts
            .lock()
            .get(host)
            .map(|h| h.status.lock().clone())
            .unwrap_or_else(|| "Disconnected".into())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Keep the Host, Thread, runtime capability and project identities explicit at admission"
    )]
    pub async fn open(
        &self,
        host: RemoteHostConfig,
        thread: Uuid,
        generation: Uuid,
        project: String,
        config: RemoteConversationConfig,
        network: Network,
        artifacts: Option<BrowserToolServer>,
        resume: bool,
    ) -> Result<ConversationHandle> {
        ensure!(!project.trim().is_empty(), "Remote project key is required");
        let sender = self.connection(host, resume)?;
        let (reply, response) = async_channel::bounded(1);
        sender
            .send(ClientRequest::Open {
                thread,
                generation,
                project,
                config,
                network,
                artifacts,
                resume,
                reply,
            })
            .await
            .map_err(|_| anyhow!("Remote Host client stopped"))?;
        response
            .recv()
            .await
            .map_err(|_| anyhow!("Remote Host client stopped during open"))?
    }

    pub async fn read_diff(&self, host: &str, thread: Uuid, path: Option<String>) -> Result<Value> {
        self.query(host, thread, Operation::ReadDiff { path }).await
    }
    pub async fn read_file(&self, host: &str, thread: Uuid, path: String) -> Result<Value> {
        self.query(host, thread, Operation::ReadFile { path }).await
    }
    async fn query(&self, host: &str, thread: Uuid, operation: Operation) -> Result<Value> {
        let sender = self
            .hosts
            .lock()
            .get(host)
            .map(|h| h.requests.clone())
            .ok_or_else(|| anyhow!("Remote Host is not connected"))?;
        let (reply, response) = async_channel::bounded(1);
        sender
            .send(ClientRequest::Query {
                thread,
                operation,
                reply,
            })
            .await
            .map_err(|_| anyhow!("Remote Host client stopped"))?;
        response
            .recv()
            .await
            .map_err(|_| anyhow!("Remote Host client stopped during query"))?
    }
    fn connection(
        &self,
        config: RemoteHostConfig,
        explicit_reconnect: bool,
    ) -> Result<Sender<ClientRequest>> {
        config.validate()?;
        let mut hosts = self.hosts.lock();
        if let Some(host) = hosts.get_mut(&config.id) {
            let same_connection = host.config.id == config.id
                && host.config.relay_url == config.relay_url
                && host.config.token_env == config.token_env;
            if same_connection && !host.requests.is_closed() {
                // Names and Project mappings are desktop metadata, not transport identity.
                host.config = config;
                return Ok(host.requests.clone());
            }
            ensure!(
                same_connection || explicit_reconnect,
                "Remote Host connection settings changed; use Reconnect to attach with the updated settings"
            );
        }
        ensure!(
            hosts.contains_key(&config.id) || hosts.len() < 32,
            "Too many connected remote Hosts"
        );
        // Resolve credentials before revoking a working attachment. An invalid edit must
        // not strand its existing Conversations; explicit Reconnect never stops a turn.
        let token = config.token()?;
        hosts.remove(&config.id);
        let connection = transport::connect(&config.relay_url, &token, Role::Desktop)?;
        let (requests, receiver) = async_channel::bounded(CAPACITY);
        let status = Arc::new(Mutex::new("Connecting".into()));
        let worker_status = status.clone();
        let worker_sender = requests.clone();
        std::thread::Builder::new()
            .name("remote-conversations".into())
            .spawn(move || run(connection, receiver, worker_sender, worker_status))?;
        hosts.insert(
            config.id.clone(),
            Host {
                config,
                requests: requests.clone(),
                status,
            },
        );
        Ok(requests)
    }
}

fn run(
    connection: transport::Connection,
    requests: Receiver<ClientRequest>,
    sender: Sender<ClientRequest>,
    status: Arc<Mutex<String>>,
) {
    let mut runtimes = HashMap::<Uuid, Runtime>::new();
    let mut epoch = None;
    let mut heartbeat = Instant::now();
    let cache = std::env::temp_dir().join(format!("drostje-remote-{}", Uuid::new_v4()));
    loop {
        while let Ok(event) = connection.incoming.try_recv() {
            match event {
                TransportEvent::PeerConnected { epoch: current } => {
                    if epoch.is_some_and(|previous| previous != current) {
                        disconnect(&mut runtimes, "Remote relay attachment changed");
                    }
                    epoch = Some(current);
                    *status.lock() = "Recovering".into();
                    for (&thread, runtime) in &mut runtimes {
                        let initial = runtime.resume && !runtime.open_sent;
                        if let Err(error) =
                            send_open(&connection.outgoing, current, thread, runtime, initial)
                        {
                            fail_runtime(runtime, error.to_string());
                        }
                    }
                }
                TransportEvent::PeerDisconnected => {
                    epoch = None;
                    *status.lock() = "Host offline".into();
                    disconnect(
                        &mut runtimes,
                        "Remote Host disconnected; uncertain work was not replayed",
                    );
                }
                TransportEvent::Disconnected(reason) => {
                    epoch = None;
                    *status.lock() = format!("Disconnected: {reason}");
                    disconnect(&mut runtimes, &reason);
                }
                TransportEvent::Payload(value) => {
                    if let Some(current) = epoch {
                        match serde_json::from_value::<Response>(value) {
                            Ok(response) => process_response(
                                response,
                                current,
                                &connection.outgoing,
                                &sender,
                                &mut runtimes,
                            ),
                            Err(_) => {
                                disconnect(&mut runtimes, "Invalid remote Host protocol response");
                                epoch = None;
                                let _ = connection.reconnect.try_send(());
                            }
                        }
                    }
                }
            }
        }
        while let Ok(request) = requests.try_recv() {
            match request {
                ClientRequest::Open {
                    thread,
                    generation,
                    project,
                    config,
                    network,
                    artifacts,
                    resume,
                    reply,
                } => {
                    if let Some(mut old) = runtimes.remove(&thread) {
                        if let Some(current) = epoch
                            && send(
                                &connection.outgoing,
                                Request {
                                    version: wire::VERSION,
                                    capabilities: wire::Capabilities::DESKTOP,
                                    id: Uuid::new_v4(),
                                    thread,
                                    epoch: current,
                                    operation: Operation::Detach,
                                },
                            )
                            .is_err()
                        {
                            let _ = connection.reconnect.try_send(());
                        }
                        fail_runtime(
                            &mut old,
                            "Remote Conversation replaced by explicit Resume".into(),
                        );
                    }
                    if runtimes.len() >= CAPACITY {
                        let _ = reply
                            .try_send(Err(anyhow!("Remote Host Conversation capacity reached")));
                        continue;
                    }
                    let (commands, command_rx) = async_channel::bounded(64);
                    let (events, event_rx) = async_channel::bounded(CAPACITY);
                    let open_id = Uuid::new_v4();
                    let mut runtime = Runtime {
                        project,
                        config,
                        network,
                        artifacts,
                        capability: generation,
                        generation: None,
                        open_id,
                        open_sent: false,
                        open_resume: false,
                        resume,
                        cursor: 0,
                        recovered: false,
                        recovering_since: None,
                        recovery_state: None,
                        recovery_peer: None,
                        commands: command_rx,
                        events,
                        network_calls: HashSet::new(),
                        network_pending: 0,
                        network_lease: async_channel::bounded(1),
                        handle: Some(ConversationHandle {
                            commands,
                            events: event_rx,
                        }),
                        opening: Some((open_id, reply, Instant::now())),
                        pending: HashMap::new(),
                        cancellations: HashMap::new(),
                        image_dir: cache.join(thread.to_string()),
                    };
                    if let Some(current) = epoch
                        && let Err(error) =
                            send_open(&connection.outgoing, current, thread, &mut runtime, resume)
                    {
                        fail_runtime(&mut runtime, error.to_string());
                    }
                    runtimes.insert(thread, runtime);
                }
                ClientRequest::Query {
                    thread,
                    operation,
                    reply,
                } => {
                    if let (Some(current), Some(runtime)) = (epoch, runtimes.get_mut(&thread)) {
                        if runtime.recovered && runtime.pending.len() < CAPACITY {
                            let id = Uuid::new_v4();
                            match send(
                                &connection.outgoing,
                                Request {
                                    version: wire::VERSION,
                                    capabilities: wire::Capabilities::DESKTOP,
                                    id,
                                    thread,
                                    epoch: current,
                                    operation,
                                },
                            ) {
                                Ok(()) => {
                                    runtime.pending.insert(
                                        id,
                                        Pending {
                                            identity: Identity::Query(reply),
                                            since: Instant::now(),
                                        },
                                    );
                                }
                                Err(error) => {
                                    let _ = reply.try_send(Err(error));
                                }
                            }
                        } else {
                            let _ = reply.try_send(Err(anyhow!(
                                "Remote Conversation is recovering or busy"
                            )));
                        }
                    } else {
                        let _ = reply.try_send(Err(anyhow!("Remote Conversation is offline")));
                    }
                }
                ClientRequest::NetworkReply {
                    thread,
                    epoch: expected,
                    generation,
                    call,
                    result,
                } => {
                    if epoch == Some(expected)
                        && let Some(runtime) = runtimes
                            .get_mut(&thread)
                            .filter(|r| r.generation == Some(generation) && r.recovered)
                    {
                        runtime.network_pending = runtime.network_pending.saturating_sub(1);
                        let _ = send(
                            &connection.outgoing,
                            Request {
                                version: wire::VERSION,
                                capabilities: wire::Capabilities::DESKTOP,
                                id: Uuid::new_v4(),
                                thread,
                                epoch: expected,
                                operation: Operation::NetworkResponse { call, result },
                            },
                        );
                    }
                }
            }
        }
        let mut detach = Vec::new();
        for (&thread, runtime) in &mut runtimes {
            if runtime
                .opening
                .as_ref()
                .is_some_and(|(_, _, start)| start.elapsed() > REQUEST_TIMEOUT)
                || runtime
                    .recovering_since
                    .is_some_and(|start| start.elapsed() > REQUEST_TIMEOUT)
            {
                fail_runtime(
                    runtime,
                    "Remote Host did not recover the Conversation before timeout".into(),
                );
            }
            if runtime.events.is_closed() {
                detach.push(thread);
                continue;
            }
            while let Ok(command) = runtime.commands.try_recv() {
                if matches!(command, ConversationCommand::Close) {
                    detach.push(thread);
                    break;
                }
                if !runtime.recovered || epoch.is_none() {
                    reject_command(
                        runtime,
                        &command,
                        "Remote Conversation is offline or recovering; command was not sent",
                    );
                    continue;
                }
                let Some(current) = epoch else {
                    continue;
                };
                if runtime.pending.len() >= CAPACITY {
                    reject_command(
                        runtime,
                        &command,
                        "Remote request capacity reached; command was not sent",
                    );
                    continue;
                }
                match prepare_command(command, runtime) {
                    Ok((command, identity)) => {
                        let id = Uuid::new_v4();
                        match send(
                            &connection.outgoing,
                            Request {
                                version: wire::VERSION,
                                capabilities: wire::Capabilities::DESKTOP,
                                id,
                                thread,
                                epoch: current,
                                operation: Operation::Command(command),
                            },
                        ) {
                            Ok(()) => {
                                runtime.pending.insert(
                                    id,
                                    Pending {
                                        identity,
                                        since: Instant::now(),
                                    },
                                );
                            }
                            Err(error) => settle(runtime, identity, &error.to_string(), false),
                        }
                    }
                    Err((identity, error)) => settle(runtime, identity, &error.to_string(), false),
                }
            }
            if runtime.commands.is_closed() && runtime.handle.is_none() {
                detach.push(thread);
            }
            let expired: Vec<_> = runtime
                .pending
                .iter()
                .filter(|(_, pending)| pending.since.elapsed() > REQUEST_TIMEOUT)
                .map(|(id, _)| *id)
                .collect();
            for id in expired {
                if let Some(pending) = runtime.pending.remove(&id) {
                    settle(
                        runtime,
                        pending.identity,
                        "Remote receipt timed out; request was not replayed",
                        true,
                    );
                }
            }
            let cancelled: Vec<_> = runtime
                .cancellations
                .iter()
                .filter(|(_, cancel)| cancel.is_closed() || !cancel.is_empty())
                .map(|(id, _)| *id)
                .collect();
            for attempt in cancelled {
                runtime.cancellations.remove(&attempt);
                if let Some(current) = epoch
                    && send(
                        &connection.outgoing,
                        Request {
                            version: wire::VERSION,
                            capabilities: wire::Capabilities::DESKTOP,
                            id: Uuid::new_v4(),
                            thread,
                            epoch: current,
                            operation: Operation::CancelPeer { attempt },
                        },
                    )
                    .is_err()
                {
                    let _ = connection.reconnect.try_send(());
                }
            }
        }
        for thread in detach {
            if let Some(current) = epoch
                && send(
                    &connection.outgoing,
                    Request {
                        version: wire::VERSION,
                        capabilities: wire::Capabilities::DESKTOP,
                        id: Uuid::new_v4(),
                        thread,
                        epoch: current,
                        operation: Operation::Detach,
                    },
                )
                .is_err()
            {
                let _ = connection.reconnect.try_send(());
            }
            if let Some(mut runtime) = runtimes.remove(&thread) {
                fail_runtime(&mut runtime, "Remote Conversation detached".into());
            }
        }
        if let Some(current) = epoch {
            if heartbeat.elapsed() >= Duration::from_secs(5) {
                for &thread in runtimes.keys() {
                    let _ = send(
                        &connection.outgoing,
                        Request {
                            version: wire::VERSION,
                            capabilities: wire::Capabilities::DESKTOP,
                            id: Uuid::new_v4(),
                            thread,
                            epoch: current,
                            operation: Operation::Heartbeat,
                        },
                    );
                }
                heartbeat = Instant::now();
            }
            if runtimes.values().all(|r| r.recovered) {
                *status.lock() = "Connected".into();
            }
        }
        if requests.is_closed() || connection.incoming.is_closed() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    disconnect(&mut runtimes, "Remote client stopped");
}

fn send(outgoing: &Sender<Value>, request: Request) -> Result<()> {
    let value = serde_json::to_value(request)?;
    let mut size = FrameBudget(wire::MAX_FRAME_BYTES - 4096);
    serde_json::to_writer(&mut size, &value)
        .map_err(|_| anyhow!("Remote request exceeds frame limit"))?;
    outgoing
        .try_send(value)
        .map_err(|_| anyhow!("Remote transport is unavailable or its bounded queue is full"))
}

struct FrameBudget(usize);
impl std::io::Write for FrameBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("Remote frame limit exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn send_open(
    outgoing: &Sender<Value>,
    epoch: Uuid,
    thread: Uuid,
    runtime: &mut Runtime,
    initial: bool,
) -> Result<()> {
    let id = Uuid::new_v4();
    runtime.open_id = id;
    runtime.open_resume = initial;
    send(
        outgoing,
        Request {
            version: wire::VERSION,
            capabilities: wire::Capabilities::DESKTOP,
            id,
            thread,
            epoch,
            operation: Operation::Open {
                project: runtime.project.clone(),
                config: runtime.config.clone(),
                after: runtime.cursor,
                generation: runtime.generation,
                resume: initial,
            },
        },
    )?;
    runtime.open_sent = true;
    runtime.recovering_since = Some(Instant::now());
    Ok(())
}
fn emit(runtime: &mut Runtime, event: ConversationEvent) {
    if !deliver_event(&runtime.events, &runtime.network_lease.0, event) {
        runtime.recovered = false;
        runtime.commands.close();
        runtime.cancellations.clear();
        runtime.network_pending = 0;
        runtime.network_calls.clear();
    }
}

fn deliver_event(
    events: &Sender<ConversationEvent>,
    network_lease: &Sender<()>,
    event: ConversationEvent,
) -> bool {
    if events.try_send(event).is_ok() {
        return true;
    }
    // Backpressure is terminal, not permission to discard observations and keep acting.
    network_lease.close();
    events.close();
    false
}
fn fail_runtime(runtime: &mut Runtime, message: String) {
    if let Some((_, reply, _)) = runtime.opening.take() {
        let _ = reply.try_send(Err(anyhow!(message.clone())));
    }
    runtime.handle.take();
    emit(
        runtime,
        ConversationEvent::Failed {
            request_id: None,
            message,
        },
    );
    runtime.events.close();
    runtime.recovered = false;
    runtime.network_lease.0.close();
}
fn disconnect(runtimes: &mut HashMap<Uuid, Runtime>, reason: &str) {
    for runtime in runtimes.values_mut() {
        runtime.recovered = false;
        runtime.recovering_since = None;
        runtime.recovery_state = None;
        runtime.recovery_peer = None;
        runtime.network_lease.0.close();
        runtime.cancellations.clear();
        runtime.network_pending = 0;
        runtime.network_calls.clear();
        let pending = std::mem::take(&mut runtime.pending);
        for (_, pending) in pending {
            settle(runtime, pending.identity, reason, true);
        }
        while let Ok(command) = runtime.commands.try_recv() {
            if matches!(command, ConversationCommand::Close) {
                runtime.events.close();
            } else {
                reject_command(
                    runtime,
                    &command,
                    "Connection changed before command admission; command was not sent",
                );
            }
        }
        emit(
            runtime,
            ConversationEvent::StateChanged(ConversationState::Disconnected),
        );
        emit(
            runtime,
            ConversationEvent::PeerDeliveryAvailable {
                available: false,
                reason: Some(reason.into()),
            },
        );
        emit(
            runtime,
            ConversationEvent::Notice {
                level: "warning".into(),
                text: reason.into(),
            },
        );
    }
}
fn settle(runtime: &mut Runtime, identity: Identity, reason: &str, uncertain: bool) {
    match identity {
        Identity::Submit(request_id) => {
            emit(
                runtime,
                ConversationEvent::Failed {
                    request_id: Some(request_id),
                    message: reason.into(),
                },
            );
            emit(
                runtime,
                ConversationEvent::TurnSettled {
                    request_id,
                    outcome: if uncertain {
                        TurnOutcome::Unknown
                    } else {
                        TurnOutcome::Failed
                    },
                },
            );
        }
        Identity::Peer(attempt) => {
            runtime.cancellations.remove(&attempt);
            emit(
                runtime,
                ConversationEvent::PeerDelivery {
                    attempt,
                    outcome: if uncertain {
                        PeerDeliveryOutcome::Unknown(reason.into())
                    } else {
                        PeerDeliveryOutcome::Rejected(reason.into())
                    },
                },
            );
        }
        Identity::Other => emit(
            runtime,
            ConversationEvent::Notice {
                level: "error".into(),
                text: reason.into(),
            },
        ),
        Identity::Query(reply) => {
            let _ = reply.try_send(Err(anyhow!(reason.to_owned())));
        }
    }
}
fn reject_command(runtime: &mut Runtime, command: &ConversationCommand, reason: &str) {
    let identity = match command {
        ConversationCommand::Submit { request_id, .. } => Identity::Submit(*request_id),
        ConversationCommand::DeliverPeer(delivery) => Identity::Peer(delivery.attempt),
        _ => Identity::Other,
    };
    settle(runtime, identity, reason, false);
}
fn prepare_command(
    command: ConversationCommand,
    runtime: &mut Runtime,
) -> std::result::Result<(WireCommand, Identity), (Identity, anyhow::Error)> {
    Ok(match command {
        ConversationCommand::Submit {
            request_id,
            text,
            images,
        } => {
            if images.len() > 32 {
                return Err((
                    Identity::Submit(request_id),
                    anyhow!("Too many remote image attachments"),
                ));
            }
            let total = images.iter().try_fold(0u64, |total, image| -> Result<u64> {
                let size = std::fs::metadata(&image.path)?.len();
                let total = total
                    .checked_add(size)
                    .ok_or_else(|| anyhow!("Remote image size overflow"))?;
                ensure!(
                    total <= wire::MAX_IMAGE_BYTES as u64,
                    "Remote image attachments exceed transfer limit"
                );
                Ok(total)
            });
            total.map_err(|error| (Identity::Submit(request_id), error))?;
            let images = images
                .iter()
                .map(WireImage::read)
                .collect::<Result<Vec<_>>>()
                .map_err(|e| (Identity::Submit(request_id), e))?;
            (
                WireCommand::Submit {
                    request_id,
                    text,
                    images,
                },
                Identity::Submit(request_id),
            )
        }
        ConversationCommand::DeliverPeer(delivery) => {
            if delivery.cancelled.is_closed() || !delivery.cancelled.is_empty() {
                return Err((
                    Identity::Peer(delivery.attempt),
                    anyhow!("Peer delivery authorization was revoked before send"),
                ));
            }
            runtime
                .cancellations
                .insert(delivery.attempt, delivery.cancelled);
            (
                WireCommand::DeliverPeer {
                    attempt: delivery.attempt,
                    messages: delivery.messages,
                },
                Identity::Peer(delivery.attempt),
            )
        }
        ConversationCommand::Interrupt => (WireCommand::Interrupt, Identity::Other),
        ConversationCommand::SelectModel(model) => {
            (WireCommand::SelectModel(model), Identity::Other)
        }
        ConversationCommand::LoadHistory { cursor } => {
            (WireCommand::LoadHistory { cursor }, Identity::Other)
        }
        ConversationCommand::Respond { request_id, answer } => {
            (WireCommand::Respond { request_id, answer }, Identity::Other)
        }
        ConversationCommand::Close => {
            return Err((
                Identity::Other,
                anyhow!("Close detaches and is not a host command"),
            ));
        }
    })
}

fn process_response(
    response: Response,
    epoch: Uuid,
    outgoing: &Sender<Value>,
    sender: &Sender<ClientRequest>,
    runtimes: &mut HashMap<Uuid, Runtime>,
) {
    match response {
        Response::Opened {
            id,
            thread,
            generation,
            session_ref,
            replay_after,
            ..
        } => {
            if let Some(runtime) = runtimes.get_mut(&thread).filter(|r| r.open_id == id) {
                if runtime.generation.is_some_and(|old| old != generation) {
                    fail_runtime(
                        runtime,
                        "Remote runtime changed; explicit Resume is required".into(),
                    );
                    return;
                }
                if replay_after != runtime.cursor && !runtime.open_resume {
                    fail_runtime(
                        runtime,
                        "Remote event retention changed; explicit Resume is required".into(),
                    );
                    return;
                }
                runtime.cursor = replay_after;
                runtime.generation = Some(generation);
                if session_ref.is_some() {
                    runtime.config.session_ref = session_ref;
                }
                emit(
                    runtime,
                    ConversationEvent::StateChanged(ConversationState::Starting),
                );
                emit(
                    runtime,
                    ConversationEvent::PeerDeliveryAvailable {
                        available: false,
                        reason: Some("Recovering remote observations".into()),
                    },
                );
                if let Some((_, reply, _)) = runtime.opening.take()
                    && let Some(handle) = runtime.handle.take()
                    && reply.try_send(Ok(handle)).is_err()
                {
                    runtime.events.close();
                }
            }
        }
        Response::Event {
            thread,
            generation,
            sequence,
            mut event,
            images,
        } => {
            if let Some(runtime) = runtimes.get_mut(&thread) {
                if runtime.generation != Some(generation) || sequence <= runtime.cursor {
                    return;
                }
                if sequence != runtime.cursor + 1 {
                    fail_runtime(
                        runtime,
                        "Remote event recovery has a sequence gap; explicit Resume is required"
                            .into(),
                    );
                    return;
                }
                if let Err(error) = localize_images(&mut event, &images, &runtime.image_dir) {
                    fail_runtime(runtime, format!("Remote image transfer failed: {error}"));
                    return;
                }
                if let ConversationEvent::Ready { session_ref, .. } = &event {
                    runtime.config.session_ref = Some(session_ref.clone());
                }
                if !runtime.recovered {
                    runtime.recovering_since = Some(Instant::now());
                    match &mut event {
                        ConversationEvent::Ready { peer_delivery, .. } => {
                            runtime.recovery_peer = Some((*peer_delivery, None));
                            *peer_delivery = false;
                        }
                        ConversationEvent::StateChanged(state) => {
                            runtime.recovery_state = Some(state.clone());
                            *state = ConversationState::Starting;
                        }
                        ConversationEvent::PeerDeliveryAvailable { available, reason } => {
                            runtime.recovery_peer = Some((*available, reason.clone()));
                            *available = false;
                            *reason = Some("Recovering remote observations".into());
                        }
                        _ => {}
                    }
                }
                emit(runtime, event);
                if !runtime.events.is_closed() {
                    runtime.cursor = sequence;
                    let _ = send(
                        outgoing,
                        Request {
                            version: wire::VERSION,
                            capabilities: wire::Capabilities::DESKTOP,
                            id: Uuid::new_v4(),
                            thread,
                            epoch,
                            operation: Operation::Ack { through: sequence },
                        },
                    );
                }
            }
        }
        Response::Recovered {
            thread,
            generation,
            through,
        } => {
            if let Some(runtime) = runtimes.get_mut(&thread) {
                if runtime.generation.is_none() || runtime.events.is_closed() {
                    return;
                }
                if runtime.generation != Some(generation) || runtime.cursor != through {
                    fail_runtime(
                        runtime,
                        "Remote recovery cursor does not match host state".into(),
                    );
                    return;
                }
                while let Ok(command) = runtime.commands.try_recv() {
                    if matches!(command, ConversationCommand::Close) {
                        runtime.events.close();
                    } else {
                        reject_command(
                            runtime,
                            &command,
                            "Command queued during recovery was not replayed; submit again explicitly",
                        );
                    }
                }
                if runtime.events.is_closed() {
                    return;
                }
                runtime.recovered = true;
                runtime.network_lease = async_channel::bounded(1);
                runtime.recovering_since = None;
                if let Some(state) = runtime.recovery_state.take() {
                    emit(runtime, ConversationEvent::StateChanged(state));
                }
                if let Some((available, reason)) = runtime.recovery_peer.take() {
                    emit(
                        runtime,
                        ConversationEvent::PeerDeliveryAvailable { available, reason },
                    );
                }
            }
        }
        Response::Receipt { id, thread, status } => {
            if let Some(runtime) = runtimes.get_mut(&thread)
                && let Some(pending) = runtime.pending.remove(&id)
            {
                match status {
                    ReceiptStatus::Admitted => {}
                    ReceiptStatus::Rejected(reason) => {
                        settle(runtime, pending.identity, &reason, false)
                    }
                    ReceiptStatus::Unknown(reason) => {
                        settle(runtime, pending.identity, &reason, true)
                    }
                }
            }
        }
        Response::Rejected {
            id,
            thread,
            message,
        } => {
            if let Some(runtime) = runtimes.get_mut(&thread) {
                if let Some(pending) = runtime.pending.remove(&id) {
                    settle(runtime, pending.identity, &message, false);
                } else if runtime.open_id == id {
                    fail_runtime(runtime, message);
                }
            }
        }
        Response::Data { id, thread, result } => {
            if let Some(runtime) = runtimes.get_mut(&thread)
                && let Some(Pending {
                    identity: Identity::Query(reply),
                    ..
                }) = runtime.pending.remove(&id)
            {
                let _ = reply.try_send(result.map_err(anyhow::Error::msg));
            }
        }
        Response::NetworkRequest {
            thread,
            generation,
            call,
            action,
        } => {
            if let Some(runtime) = runtimes
                .get_mut(&thread)
                .filter(|r| r.recovered && r.generation == Some(generation))
            {
                if runtime.network_pending >= 64
                    || runtime.network_calls.len() >= 4096
                    || !runtime.network_calls.insert(call)
                {
                    let _ = send(outgoing, Request { version: wire::VERSION,
 capabilities: wire::Capabilities::DESKTOP, id: Uuid::new_v4(), thread, epoch, operation: Operation::NetworkResponse { call, result: Err("Duplicate remote Network request or admission capacity reached".into()) } });
                    return;
                }
                runtime.network_pending += 1;
                let network = runtime.network.clone();
                let capability = runtime.capability;
                let sender = sender.clone();
                let lease = runtime.network_lease.1.clone();
                smol::spawn(async move {
                    if lease.is_closed() {
                        return;
                    }
                    let result = smol::future::race(
                        async {
                            match serde_json::from_value(action) {
                                Ok(action) => network
                                    .call(capability, action)
                                    .await
                                    .map_err(|error| error.to_string()),
                                Err(_) => Err("Invalid remote Network action".into()),
                            }
                        },
                        async {
                            let _ = lease.recv().await;
                            Err("Remote Network attachment was revoked".into())
                        },
                    )
                    .await;
                    let _ = sender
                        .send(ClientRequest::NetworkReply {
                            thread,
                            epoch,
                            generation,
                            call,
                            result,
                        })
                        .await;
                })
                .detach();
            } else {
                let _ = send(
                    outgoing,
                    Request {
                        version: wire::VERSION,
                        capabilities: wire::Capabilities::DESKTOP,
                        id: Uuid::new_v4(),
                        thread,
                        epoch,
                        operation: Operation::NetworkResponse {
                            call,
                            result: Err("Remote Conversation is not attached and authorized".into()),
                        },
                    },
                );
            }
        }
        Response::ArtifactRequest {
            thread,
            generation,
            call,
            name,
            title,
            html,
            placement,
        } => {
            let Some(runtime) =
                admit_desktop_call(runtimes, outgoing, epoch, thread, generation, call)
            else {
                return;
            };
            let server = runtime.artifacts.clone();
            let directory = runtime.image_dir.join("artifacts");
            let sender = sender.clone();
            smol::spawn(async move {
                let result = show_remote_artifact(
                    server, directory, thread, call, name, title, html, placement,
                )
                .await;
                let _ = sender
                    .send(ClientRequest::NetworkReply {
                        thread,
                        epoch,
                        generation,
                        call,
                        result,
                    })
                    .await;
            })
            .detach();
        }
        Response::ArtifactComplete {
            thread,
            generation,
            call,
            browser,
        } => {
            let Some(runtime) =
                admit_desktop_call(runtimes, outgoing, epoch, thread, generation, call)
            else {
                return;
            };
            let server = runtime.artifacts.clone();
            let sender = sender.clone();
            smol::spawn(async move {
                let result = desktop_tool(
                    server,
                    thread,
                    BrowserToolOperation::CompleteArtifact { id: browser },
                )
                .await;
                let _ = sender
                    .send(ClientRequest::NetworkReply {
                        thread,
                        epoch,
                        generation,
                        call,
                        result,
                    })
                    .await;
            })
            .detach();
        }
    }
}

/// Admit one host-originated desktop call for the current runtime generation, or answer
/// it with the refusal right away. Bounded like Network calls; a call ID is never reused.
fn admit_desktop_call<'a>(
    runtimes: &'a mut HashMap<Uuid, Runtime>,
    outgoing: &Sender<Value>,
    epoch: Uuid,
    thread: Uuid,
    generation: Uuid,
    call: Uuid,
) -> Option<&'a mut Runtime> {
    let refuse = |reason: &str| {
        let _ = send(
            outgoing,
            Request {
                version: wire::VERSION,
                capabilities: wire::Capabilities::DESKTOP,
                id: Uuid::new_v4(),
                thread,
                epoch,
                operation: Operation::NetworkResponse {
                    call,
                    result: Err(reason.into()),
                },
            },
        );
    };
    let Some(runtime) = runtimes
        .get_mut(&thread)
        .filter(|r| r.recovered && r.generation == Some(generation))
    else {
        refuse("Remote Conversation is not attached and authorized");
        return None;
    };
    if runtime.network_pending >= 64
        || runtime.network_calls.len() >= 4096
        || !runtime.network_calls.insert(call)
    {
        refuse("Duplicate remote artifact request or admission capacity reached");
        return None;
    }
    runtime.network_pending += 1;
    Some(runtime)
}

/// Run one operation on the desktop tool server for a remote runtime's Conversation.
async fn desktop_tool(
    server: Option<BrowserToolServer>,
    thread: Uuid,
    operation: BrowserToolOperation,
) -> std::result::Result<Value, String> {
    let server =
        server.ok_or("Artifacts are unavailable: the desktop browser tools did not start")?;
    smol::unblock(move || {
        let (reply, response) = std::sync::mpsc::channel();
        server.submit(BrowserToolRequest {
            thread,
            operation,
            reply,
        })?;
        response
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| "The desktop did not answer in time; the outcome is unknown".to_owned())?
    })
    .await
}

/// Write a remote artifact's HTML beside this runtime's other remote media and show
/// the local copy through the desktop tool server, exactly as a local artifact.
#[expect(
    clippy::too_many_arguments,
    reason = "Each artifact identity and presentation field stays explicit across the relay"
)]
async fn show_remote_artifact(
    server: Option<BrowserToolServer>,
    directory: PathBuf,
    thread: Uuid,
    call: Uuid,
    name: String,
    title: Option<String>,
    html: String,
    placement: crate::browser_tools::ArtifactPlacement,
) -> std::result::Result<Value, String> {
    if server.is_none() {
        return Err("Artifacts are unavailable: the desktop browser tools did not start".into());
    }
    if html.len() > crate::browser_tools::MAX_REMOTE_ARTIFACT {
        return Err("Remote artifact exceeds 2 MiB".into());
    }
    let path = smol::unblock(move || {
        let mut stem: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            .take(64)
            .collect();
        if !(stem.ends_with(".html") || stem.ends_with(".htm")) {
            stem.push_str(".html");
        }
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("Could not store the remote artifact: {error}"))?;
        let path = directory.join(format!("{call}-{stem}"));
        std::fs::write(&path, html)
            .map_err(|error| format!("Could not store the remote artifact: {error}"))?;
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| "Remote artifact path is not valid UTF-8".to_owned())
    })
    .await?;
    desktop_tool(
        server,
        thread,
        BrowserToolOperation::ShowArtifact {
            path,
            title,
            placement,
        },
    )
    .await
}

fn localize_images(
    event: &mut ConversationEvent,
    images: &[WireImage],
    directory: &std::path::Path,
) -> Result<()> {
    let local = if images.is_empty() {
        HashMap::new()
    } else {
        wire::materialize_images(images, directory)?
            .into_iter()
            .map(|image| (image.id, image))
            .collect::<HashMap<_, _>>()
    };
    let visit = |blocks: &mut Vec<ContentBlock>| -> Result<()> {
        for block in blocks {
            if let ContentBlock::Image(image) = block {
                *image = local.get(&image.id).cloned().ok_or_else(|| {
                    anyhow!("Remote image bytes missing; host paths are never read locally")
                })?;
            }
        }
        Ok(())
    };
    match event {
        ConversationEvent::History { messages, .. } => {
            for message in messages {
                visit(&mut message.content)?;
            }
        }
        ConversationEvent::MessageUpdated(message) => visit(&mut message.content)?,
        ConversationEvent::ToolUpdated(tool) => visit(&mut tool.result)?,
        _ => ensure!(
            images.is_empty(),
            "Unexpected image payload on remote event"
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{harness::Message, model::ImageAttachment};
    #[test]
    fn remote_image_paths_are_never_trusted() {
        let image = ImageAttachment {
            id: Uuid::new_v4(),
            path: "/etc/passwd".into(),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            original: Some("/private/secret".into()),
        };
        let mut event = ConversationEvent::MessageUpdated(Message {
            id: "m".into(),
            role: "assistant".into(),
            content: vec![ContentBlock::Image(image)],
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        });
        let directory = std::env::temp_dir().join(Uuid::new_v4().to_string());
        assert!(localize_images(&mut event, &[], &directory).is_err());
        let _ = std::fs::remove_dir(directory);
    }
    #[test]
    fn bounded_transport_rejects_instead_of_waiting_or_replaying() {
        let (sender, receiver) = async_channel::bounded(1);
        let request = || Request {
            version: wire::VERSION,
            capabilities: wire::Capabilities::DESKTOP,
            id: Uuid::new_v4(),
            thread: Uuid::new_v4(),
            epoch: Uuid::new_v4(),
            operation: Operation::Detach,
        };
        send(&sender, request()).unwrap();
        assert!(send(&sender, request()).is_err());
        assert_eq!(receiver.len(), 1);
    }
    #[test]
    fn last_client_drop_revokes_host_actor() {
        let (requests, receiver) = async_channel::bounded(1);
        let client = Client::new();
        client.hosts.lock().insert(
            "host".into(),
            Host {
                config: RemoteHostConfig {
                    id: "host".into(),
                    name: "Host".into(),
                    relay_url: "wss://relay.example/v1/relay/host".into(),
                    token_env: "TOKEN".into(),
                    projects: Default::default(),
                },
                requests,
                status: Arc::new(Mutex::new("Connected".into())),
            },
        );
        let clone = client.clone();
        drop(client);
        assert!(!receiver.is_closed());
        drop(clone);
        assert!(receiver.is_closed());
    }

    #[test]
    fn serialized_frame_budget_counts_escaped_bytes() {
        let mut budget = FrameBudget(4);
        assert!(serde_json::to_writer(&mut budget, &"\n\n").is_err());
        let mut budget = FrameBudget(6);
        serde_json::to_writer(&mut budget, &"\n\n").unwrap();
        assert_eq!(budget.0, 0);
    }

    #[test]
    fn event_overflow_closes_consumer_and_revokes_network_admission() {
        let (events, consumer) = async_channel::bounded(1);
        let (lease, network) = async_channel::bounded(1);
        assert!(deliver_event(
            &events,
            &lease,
            ConversationEvent::StateChanged(ConversationState::Ready)
        ));
        assert!(!deliver_event(
            &events,
            &lease,
            ConversationEvent::StateChanged(ConversationState::Running)
        ));
        assert!(network.is_closed());
        assert!(matches!(
            consumer.try_recv(),
            Ok(ConversationEvent::StateChanged(ConversationState::Ready))
        ));
        assert!(matches!(
            consumer.try_recv(),
            Err(async_channel::TryRecvError::Closed)
        ));
    }

    #[test]
    fn opened_streams_backlog_larger_than_capacity_before_recovery_admission() {
        let directory =
            std::env::temp_dir().join(format!("drostje-replay-test-{}", Uuid::new_v4()));
        let store = crate::store::Store::open(directory.clone()).unwrap();
        let (network, _updates) = Network::start(store, None).unwrap();
        let thread = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        let open_id = Uuid::new_v4();
        let (reply, opened) = async_channel::bounded(1);
        let (commands, command_rx) = async_channel::bounded(64);
        let (events, event_rx) = async_channel::bounded(CAPACITY);
        let mut runtimes = HashMap::from([(
            thread,
            Runtime {
                project: "project".into(),
                config: RemoteConversationConfig {
                    harness: "omp".into(),
                    session_ref: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    allow_empty_session: true,
                },
                network,
                artifacts: None,
                capability: Uuid::new_v4(),
                generation: None,
                open_id,
                open_sent: true,
                open_resume: false,
                resume: false,
                cursor: 0,
                recovered: false,
                recovering_since: Some(Instant::now()),
                recovery_state: None,
                recovery_peer: None,
                commands: command_rx,
                events,
                network_calls: HashSet::new(),
                network_pending: 0,
                network_lease: async_channel::bounded(1),
                handle: Some(ConversationHandle {
                    commands,
                    events: event_rx,
                }),
                opening: Some((open_id, reply, Instant::now())),
                pending: HashMap::new(),
                cancellations: HashMap::new(),
                image_dir: directory.join("images"),
            },
        )]);
        let (outgoing, sent) = async_channel::bounded(CAPACITY);
        let (sender, _requests) = async_channel::bounded(1);
        let latest = (CAPACITY * 3 + 2) as u64;
        process_response(
            Response::Opened {
                id: open_id,
                thread,
                generation,
                session_ref: Some("host-session".into()),
                latest_sequence: latest,
                replay_after: 0,
            },
            epoch,
            &outgoing,
            &sender,
            &mut runtimes,
        );
        // The application can start its event pump before any replay or Recovered frame.
        let handle = opened.try_recv().unwrap().unwrap();
        assert!(!runtimes[&thread].recovered);
        while handle.events.try_recv().is_ok() {}
        for sequence in 1..=latest {
            let event = match sequence {
                1 => ConversationEvent::Ready {
                    peer_delivery: true,
                    session_ref: "host-session".into(),
                    models: vec![],
                },
                2 => ConversationEvent::StateChanged(ConversationState::Ready),
                _ => ConversationEvent::Notice {
                    level: "info".into(),
                    text: sequence.to_string(),
                },
            };
            process_response(
                Response::Event {
                    thread,
                    generation,
                    sequence,
                    event,
                    images: vec![],
                },
                epoch,
                &outgoing,
                &sender,
                &mut runtimes,
            );
            match (sequence, handle.events.try_recv().unwrap()) {
                (1, ConversationEvent::Ready { peer_delivery, .. }) => assert!(!peer_delivery),
                (2, ConversationEvent::StateChanged(state)) => {
                    assert_eq!(state, ConversationState::Starting)
                }
                (_, ConversationEvent::Notice { text, .. }) => {
                    assert_eq!(text, sequence.to_string())
                }
                _ => panic!("Unexpected replay event"),
            }
            assert!(!runtimes[&thread].recovered);
            assert!(!handle.events.is_closed());
            while sent.try_recv().is_ok() {}
        }
        let submission = Uuid::new_v4();
        handle
            .commands
            .try_send(ConversationCommand::Submit {
                request_id: submission,
                text: "queued during recovery".into(),
                images: vec![],
            })
            .unwrap();
        process_response(
            Response::Recovered {
                thread,
                generation,
                through: latest,
            },
            epoch,
            &outgoing,
            &sender,
            &mut runtimes,
        );
        assert!(runtimes[&thread].recovered);
        assert!(runtimes[&thread].commands.is_empty());
        assert!(runtimes[&thread].pending.is_empty());
        assert!(
            matches!(handle.events.try_recv().unwrap(), ConversationEvent::Failed { request_id: Some(id), .. } if id == submission)
        );
        assert!(
            matches!(handle.events.try_recv().unwrap(), ConversationEvent::TurnSettled { request_id, outcome: TurnOutcome::Failed } if request_id == submission)
        );
        assert!(matches!(
            handle.events.try_recv().unwrap(),
            ConversationEvent::StateChanged(ConversationState::Ready)
        ));
        assert!(matches!(
            handle.events.try_recv().unwrap(),
            ConversationEvent::PeerDeliveryAvailable {
                available: true,
                ..
            }
        ));
        drop(runtimes);
        drop(handle);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn metadata_edits_reuse_connection_without_reloading_credentials() {
        let client = Client::new();
        let (requests, receiver) = async_channel::bounded(1);
        let config = RemoteHostConfig {
            id: "host".into(),
            name: "Original".into(),
            relay_url: "wss://relay.example/v1/relay/host".into(),
            token_env: format!("DROSTJE_TEST_UNSET_{}", Uuid::new_v4().simple()),
            projects: Default::default(),
        };
        client.hosts.lock().insert(
            config.id.clone(),
            Host {
                config: config.clone(),
                requests: requests.clone(),
                status: Arc::new(Mutex::new("Connected".into())),
            },
        );
        let mut updated = config;
        updated.name = "Renamed".into();
        updated
            .projects
            .insert(Uuid::new_v4().to_string(), "second-project".into());
        let reused = client.connection(updated.clone(), false).unwrap();
        assert!(reused.same_channel(&requests));
        assert!(!receiver.is_closed());
        assert_eq!(client.hosts.lock()["host"].config, updated);
        updated.token_env.push_str("_ROTATED");
        let error = client.connection(updated.clone(), false).unwrap_err();
        assert!(error.to_string().contains("Reconnect"));
        assert!(!receiver.is_closed());
        // Explicit retry reaches credential resolution, not an application-restart guard.
        let error = client.connection(updated, true).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("credential environment variable")
        );
        assert!(!receiver.is_closed());
    }
}
