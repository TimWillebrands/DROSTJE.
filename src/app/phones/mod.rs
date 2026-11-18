mod transcript;

use super::*;
use crate::phone::{
    Server,
    desktop_side::{DesktopLink, LinkEvent, PairingEvent, PairingSession},
    link::{Connection, LinkRecord, PAIRING_TTL},
    view::{
        PhoneView, ProjectView, PromptOutcome, PromptReceipt, Prompting, SpaceView, ThemeView,
        ThreadKind as PhoneThreadKind, ThreadStatus, ThreadView, Transcript,
    },
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use transcript::transcript_tail;

pub const PHONE_SERVER_SETTING: &str = "phone_server";
pub const PHONE_TOKEN_ENV_SETTING: &str = "phone_token_env";
const PHONES_SETTING: &str = "phones";
const PUBLISH_EVERY: Duration = Duration::from_millis(400);
pub const PROMPT_TTL_SECONDS: i64 = 120;
const RECEIPTS_KEPT: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedPhone {
    pub id: Uuid,
    pub name: String,
    pub paired_at: String,
    pub link: LinkRecord,
    #[serde(default)]
    pub token_env: Option<String>,
    #[serde(default)]
    pub up_since: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhonePairingStatus {
    Connecting,
    Showing,
    Incoming { name: String, check: String },
    Accepting { name: String, check: String },
    Paired { name: String },
    Expired,
    Problem(String),
}

pub struct PhonePairing {
    session: Option<PairingSession>,
    pub link: String,
    pub server: String,
    pub status: PhonePairingStatus,
    pub problem: Option<String>,
    pub expires_at: Instant,
    generation: Uuid,
}

pub struct PhoneLinkState {
    link: DesktopLink,
    pub watch: Option<ThreadId>,
    pub connection: Connection,
    shown: Option<PhoneView>,
    generation: Uuid,
    prompts: PhonePrompts,
}

#[derive(Debug, Default)]
struct PhonePrompts {
    receipts: VecDeque<PromptReceipt>,
}

impl PhonePrompts {
    fn answered(&self, id: Uuid) -> bool {
        self.receipts.iter().any(|receipt| receipt.id == id)
    }

    fn record(&mut self, id: Uuid, outcome: PromptOutcome) {
        if self.answered(id) {
            return;
        }
        if self.receipts.len() == RECEIPTS_KEPT {
            self.receipts.pop_front();
        }
        self.receipts.push_back(PromptReceipt { id, outcome });
    }

    fn receipts(&self) -> Vec<PromptReceipt> {
        self.receipts.iter().cloned().collect()
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

fn late_prompt_reason(sent_at: i64, now: i64) -> Option<String> {
    (now - sent_at > PROMPT_TTL_SECONDS).then(|| {
        "The prompt reached the computer too late and was not sent. Send it again if you still want it".into()
    })
}

type TranscriptKey = (Uuid, u64, u64, usize);

#[derive(Default)]
pub struct Phones {
    pub pairing: Option<PhonePairing>,
    pub links: BTreeMap<Uuid, PhoneLinkState>,
    ticking: bool,
    transcripts: HashMap<ThreadId, (TranscriptKey, Transcript)>,
    theme: Option<ThemeView>,
}

fn http_origin(relay_url: &str) -> Option<String> {
    let url = url::Url::parse(relay_url).ok()?;
    let scheme = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        _ => return None,
    };
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

fn token_from(env: Option<&str>) -> Result<Option<String>> {
    let Some(name) = env.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(None);
    };
    let token = std::env::var(name).with_context(|| {
        format!("Environment variable {name} is not set; restart DROSTJE. after setting it")
    })?;
    anyhow::ensure!(
        !token.trim().is_empty(),
        "Environment variable {name} is empty"
    );
    Ok(Some(token.trim().to_owned()))
}

pub fn condition_status(condition: Option<ThreadCondition>) -> ThreadStatus {
    match condition {
        None => ThreadStatus::Idle,
        Some(ThreadCondition::Waiting | ThreadCondition::Paused) => ThreadStatus::Waiting,
        Some(ThreadCondition::Starting) => ThreadStatus::Starting,
        Some(ThreadCondition::Running) => ThreadStatus::Running,
        Some(ThreadCondition::Stopping) => ThreadStatus::Stopping,
        Some(ThreadCondition::Failed) => ThreadStatus::Failed,
        Some(ThreadCondition::Disconnected) => ThreadStatus::Disconnected,
        Some(ThreadCondition::Unresolved) => ThreadStatus::Unresolved,
    }
}

pub fn desktop_name() -> String {
    platform::host_name().unwrap_or_else(|| "DROSTJE.".into())
}

impl AppState {
    pub fn phone_server(&self) -> String {
        self.snapshot
            .settings
            .get(PHONE_SERVER_SETTING)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|server| !server.trim().is_empty())
            .or_else(|| {
                self.remote_hosts()
                    .iter()
                    .find_map(|host| http_origin(&host.relay_url))
            })
            .unwrap_or_default()
    }

    pub fn phone_token_env(&self) -> String {
        self.snapshot
            .settings
            .get(PHONE_TOKEN_ENV_SETTING)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    pub fn save_phone_server(&mut self, server: &str, token_env: &str) -> Result<()> {
        let server = server.trim();
        let token_env = token_env.trim();
        if !server.is_empty() {
            Server::new(server, None).map_err(|error| anyhow::anyhow!("{error}"))?;
        }
        anyhow::ensure!(
            token_env.is_empty()
                || token_env
                    .chars()
                    .all(|character| character == '_' || character.is_ascii_alphanumeric()),
            "The token variable must be an environment variable name such as DROSTJE_PHONE_TOKEN"
        );
        self.set_setting(PHONE_SERVER_SETTING, json!(server));
        self.set_setting(PHONE_TOKEN_ENV_SETTING, json!(token_env));
        Ok(())
    }

    pub fn paired_phones(&self) -> Vec<PairedPhone> {
        self.snapshot
            .settings
            .get(PHONES_SETTING)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    fn store_phones(&mut self, phones: &[PairedPhone]) {
        self.set_setting(
            PHONES_SETTING,
            serde_json::to_value(phones).expect("paired phones always serialize"),
        );
    }

    fn phone_server_config(&self) -> Result<(Server, Option<String>)> {
        let address = self.phone_server();
        anyhow::ensure!(
            !address.is_empty(),
            "Set a phone server in Settings › Phones first"
        );
        let env = self.phone_token_env();
        let token = token_from(Some(&env))?;
        let server = Server::new(&address, token.as_deref())
            .map_err(|error| anyhow::anyhow!("Phone server: {error}"))?;
        Ok((server, token))
    }

    pub fn pair_phone_reason(&self) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down.".into());
        }
        if self.storage_error.is_some() {
            return Some("Storage is not writable.".into());
        }
        self.phone_server_config()
            .err()
            .map(|error| format!("{error:#}."))
    }

    pub fn start_phone_pairing(&mut self, cx: &mut Context<Self>) -> Result<()> {
        if let Some(reason) = self.pair_phone_reason() {
            anyhow::bail!(reason);
        }
        self.cancel_phone_pairing();
        let (server, token) = self.phone_server_config()?;
        let address = server.address().to_owned();
        let session = PairingSession::start(server, token, PAIRING_TTL);
        let events = session.events.clone();
        let generation = Uuid::new_v4();
        self.phones.pairing = Some(PhonePairing {
            link: session.offer.to_link(),
            server: address,
            session: Some(session),
            status: PhonePairingStatus::Connecting,
            problem: None,
            expires_at: Instant::now() + PAIRING_TTL,
            generation,
        });
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |state, cx| {
                        state.phone_pairing_event(generation, event, cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let expiry = cx.background_executor().timer(PAIRING_TTL);
        cx.spawn(async move |this, cx| {
            expiry.await;
            let _ = this.update(cx, |state, cx| {
                if let Some(pairing) = state
                    .phones
                    .pairing
                    .as_mut()
                    .filter(|pairing| pairing.generation == generation)
                    && matches!(
                        pairing.status,
                        PhonePairingStatus::Connecting
                            | PhonePairingStatus::Showing
                            | PhonePairingStatus::Problem(_)
                    )
                {
                    pairing.session = None;
                    pairing.status = PhonePairingStatus::Expired;
                    cx.notify();
                }
            });
        })
        .detach();
        Ok(())
    }

    fn phone_pairing_event(
        &mut self,
        generation: Uuid,
        event: PairingEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(pairing) = self
            .phones
            .pairing
            .as_mut()
            .filter(|pairing| pairing.generation == generation)
        else {
            return;
        };
        match event {
            PairingEvent::Listening => {
                pairing.problem = None;
                if matches!(
                    pairing.status,
                    PhonePairingStatus::Connecting | PhonePairingStatus::Problem(_)
                ) {
                    pairing.status = PhonePairingStatus::Showing;
                }
            }
            PairingEvent::Incoming { name, check } => {
                pairing.problem = None;
                pairing.status = PhonePairingStatus::Incoming {
                    name,
                    check: check.to_string(),
                };
            }
            PairingEvent::Problem(reason) => match &pairing.status {
                PhonePairingStatus::Accepting { name, check } => {
                    pairing.status = PhonePairingStatus::Incoming {
                        name: name.clone(),
                        check: check.clone(),
                    };
                    pairing.problem = Some(reason);
                }
                PhonePairingStatus::Connecting => {
                    pairing.status = PhonePairingStatus::Problem(reason);
                }
                _ => pairing.problem = Some(reason),
            },
            PairingEvent::Expired => {
                pairing.session = None;
                pairing.status = PhonePairingStatus::Expired;
            }
            PairingEvent::Paired { name, link } => {
                pairing.session = None;
                pairing.status = PhonePairingStatus::Paired { name: name.clone() };
                let phone = PairedPhone {
                    id: Uuid::new_v4(),
                    name,
                    paired_at: platform::now(),
                    link,
                    token_env: Some(self.phone_token_env()).filter(|env| !env.is_empty()),
                    up_since: None,
                };
                let mut phones = self.paired_phones();
                phones.push(phone.clone());
                self.store_phones(&phones);
                self.start_phone_link(phone, cx);
            }
        }
    }

    pub fn accept_phone_reason(&self) -> Option<&'static str> {
        match self.phones.pairing.as_ref().map(|pairing| &pairing.status) {
            Some(PhonePairingStatus::Incoming { .. }) if self.storage_error.is_none() => None,
            Some(PhonePairingStatus::Incoming { .. }) => Some("Storage is not writable"),
            Some(PhonePairingStatus::Accepting { .. }) => Some("Pairing…"),
            _ => Some("Waiting for a phone to scan the code"),
        }
    }

    pub fn accept_phone_pairing(&mut self) -> Result<()> {
        if let Some(reason) = self.accept_phone_reason() {
            anyhow::bail!(reason);
        }
        let pairing = self
            .phones
            .pairing
            .as_mut()
            .expect("an incoming phone implies a pairing session");
        let PhonePairingStatus::Incoming { name, check } = &pairing.status else {
            unreachable!("accept_phone_reason allows only an incoming phone");
        };
        pairing.problem = None;
        pairing.status = PhonePairingStatus::Accepting {
            name: name.clone(),
            check: check.clone(),
        };
        if let Some(session) = &pairing.session {
            session.accept(desktop_name());
        }
        Ok(())
    }

    pub fn cancel_phone_pairing(&mut self) {
        if let Some(pairing) = self.phones.pairing.take()
            && let Some(session) = pairing.session
            && matches!(pairing.status, PhonePairingStatus::Incoming { .. })
        {
            session.decline();
        }
    }

    pub fn start_phone_links(&mut self, cx: &mut Context<Self>) {
        for phone in self.paired_phones() {
            self.start_phone_link(phone, cx);
        }
    }

    fn start_phone_link(&mut self, phone: PairedPhone, cx: &mut Context<Self>) {
        let server = token_from(phone.token_env.as_deref()).and_then(|token| {
            Server::new(&phone.link.server, token.as_deref())
                .map_err(|error| anyhow::anyhow!("{error}"))
        });
        let server = match server {
            Ok(server) => server,
            Err(error) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("Phone “{}” is not connected: {error:#}", phone.name),
                );
                return;
            }
        };
        let link = DesktopLink::start(server, phone.link.clone(), phone.up_since.clone());
        let events = link.events.clone();
        let generation = Uuid::new_v4();
        let id = phone.id;
        self.phones.links.insert(
            id,
            PhoneLinkState {
                link,
                watch: None,
                connection: Connection::Connecting,
                shown: None,
                generation,
                prompts: PhonePrompts::default(),
            },
        );
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |state, cx| {
                        state.phone_link_event(id, generation, event, cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        self.start_phone_ticker(cx);
        self.publish_phone_views();
    }

    fn phone_link_event(
        &mut self,
        id: Uuid,
        generation: Uuid,
        event: LinkEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(runtime) = self
            .phones
            .links
            .get_mut(&id)
            .filter(|runtime| runtime.generation == generation)
        else {
            return;
        };
        match event {
            LinkEvent::Watch(thread) => {
                runtime.watch = thread;
                runtime.shown = None;
                if let Some(thread) = thread {
                    self.ensure_conversation(thread);
                }
            }
            LinkEvent::Prompt {
                id: prompt,
                thread,
                text,
                sent_at,
            } => self.phone_prompt(id, prompt, thread, text, sent_at, cx),
            LinkEvent::Connection(connection) => runtime.connection = connection,
            LinkEvent::Position(position) => {
                let mut phones = self.paired_phones();
                if let Some(phone) = phones.iter_mut().find(|phone| phone.id == id) {
                    phone.up_since = Some(position);
                    self.store_phones(&phones);
                }
            }
            LinkEvent::Unpaired => {
                let name = self.forget_phone(id);
                self.notice(NoticeLevel::Info, format!("Phone “{name}” unpaired itself"));
            }
            LinkEvent::Revoked | LinkEvent::Closed => {
                self.phones.links.remove(&id);
            }
        }
    }

    fn phone_prompt(
        &mut self,
        phone: Uuid,
        prompt: Uuid,
        thread: ThreadId,
        text: String,
        sent_at: i64,
        cx: &mut Context<Self>,
    ) {
        if self
            .phones
            .links
            .get(&phone)
            .is_none_or(|runtime| runtime.prompts.answered(prompt))
        {
            return;
        }
        let refusal = late_prompt_reason(sent_at, unix_now())
            .or_else(|| self.phone_prompt_refusal(thread, &text));
        let outcome = match refusal {
            Some(reason) => PromptOutcome::Refused(reason),
            None => {
                let name = self
                    .paired_phones()
                    .into_iter()
                    .find(|paired| paired.id == phone)
                    .map(|paired| paired.name)
                    .unwrap_or_else(|| "A phone".into());
                let title = self
                    .snapshot
                    .threads
                    .iter()
                    .find(|known| known.id == thread)
                    .map(|known| known.title.clone())
                    .unwrap_or_default();
                self.submit_message(thread, text, Vec::new(), None, cx);
                self.notice(
                    NoticeLevel::Info,
                    format!("{name} sent a prompt to “{title}”"),
                );
                PromptOutcome::Started
            }
        };
        if let Some(runtime) = self.phones.links.get_mut(&phone) {
            runtime.prompts.record(prompt, outcome);
        }
        self.publish_phone_views();
    }

    fn phone_prompt_refusal(&self, thread: ThreadId, text: &str) -> Option<String> {
        if text.trim().is_empty() {
            return Some("Enter a prompt".into());
        }
        if let Prompting::Closed(reason) = self.phone_prompting(thread) {
            return Some(reason);
        }
        self.command_refusal(thread, text)
    }

    pub fn phone_prompting(&self, id: ThreadId) -> Prompting {
        let Some(thread) = self.snapshot.threads.iter().find(|thread| thread.id == id) else {
            return Prompting::Closed("This Thread is no longer on the computer".into());
        };
        if thread.kind != ThreadKind::Conversation {
            return Prompting::Closed("Factory threads do not accept prompts".into());
        }
        let harness = crate::harness::display_name(&thread.harness);
        if let Some(session) = self.conversations.get(&id) {
            let reason = match session.state {
                _ if session.preparing || self.active_threads.contains(&id) => Some(format!(
                    "{harness} is working. Wait for this Turn to finish"
                )),
                ConversationState::Starting => Some(format!("Starting {harness}…")),
                ConversationState::WaitingForInput => {
                    Some("Answer the agent's question on the computer first".into())
                }
                ConversationState::Running | ConversationState::Interrupting => Some(format!(
                    "{harness} is working. Wait for this Turn to finish"
                )),
                ConversationState::Disconnected | ConversationState::Failed => Some(format!(
                    "{harness} is not connected. Reconnect it on the computer"
                )),
                ConversationState::Ready if thread.submission.is_some() => {
                    Some("The last prompt needs a look on the computer before the next one".into())
                }
                ConversationState::Ready => None,
            };
            if let Some(reason) = reason {
                return Prompting::Closed(reason);
            }
        }
        match self.turn_reason(id, false) {
            Some(reason) => Prompting::Closed(reason),
            None => Prompting::Open,
        }
    }

    fn forget_phone(&mut self, id: Uuid) -> String {
        let mut phones = self.paired_phones();
        let name = phones
            .iter()
            .find(|phone| phone.id == id)
            .map(|phone| phone.name.clone())
            .unwrap_or_else(|| "Phone".into());
        phones.retain(|phone| phone.id != id);
        self.store_phones(&phones);
        self.phones.links.remove(&id);
        name
    }

    pub fn remove_phone_reason(&self, id: Uuid) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if self.storage_error.is_some() {
            return Some("Storage is not writable".into());
        }
        (!self.paired_phones().iter().any(|phone| phone.id == id))
            .then(|| "This phone is no longer paired".into())
    }

    pub fn remove_phone(&mut self, id: Uuid) -> Result<String> {
        if let Some(reason) = self.remove_phone_reason(id) {
            anyhow::bail!(reason);
        }
        if let Some(runtime) = self.phones.links.remove(&id) {
            runtime.link.revoke();
        }
        Ok(self.forget_phone(id))
    }

    pub fn phone_connection(&self, id: Uuid) -> Option<&Connection> {
        self.phones
            .links
            .get(&id)
            .map(|runtime| &runtime.connection)
    }

    pub fn phone_watch(&self, id: Uuid) -> Option<&Thread> {
        let watch = self.phones.links.get(&id)?.watch?;
        self.snapshot
            .threads
            .iter()
            .find(|thread| thread.id == watch)
    }

    fn start_phone_ticker(&mut self, cx: &mut Context<Self>) {
        if self.phones.ticking {
            return;
        }
        self.phones.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PUBLISH_EVERY).await;
                let running = this.update(cx, |state, _| {
                    if state.phones.links.is_empty() {
                        state.phones.ticking = false;
                        return false;
                    }
                    state.publish_phone_views();
                    true
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn phone_workspace(&self) -> PhoneView {
        let mut threads = BTreeMap::new();
        let spaces = self
            .spaces
            .iter()
            .map(|space| {
                let mut projects: Vec<&Project> = self
                    .snapshot
                    .projects
                    .iter()
                    .filter(|project| {
                        !project.archived && self.project_in_space(project.id, space.id)
                    })
                    .collect();
                projects.sort_by_key(|project| project.name.to_lowercase());
                SpaceView {
                    id: space.id,
                    name: space.name.clone(),
                    projects: projects
                        .into_iter()
                        .map(|project| {
                            let mut listed: Vec<&Thread> = self
                                .snapshot
                                .threads
                                .iter()
                                .filter(|thread| {
                                    thread.project_id == project.id
                                        && !thread.archived
                                        && self.thread_listed_in(thread, space.id)
                                })
                                .collect();
                            listed.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                            for thread in &listed {
                                threads.insert(thread.id, self.phone_thread(thread));
                            }
                            ProjectView {
                                id: project.id,
                                name: project.name.clone(),
                                threads: listed.iter().map(|thread| thread.id).collect(),
                            }
                        })
                        .collect(),
                }
            })
            .collect();
        PhoneView {
            desktop: desktop_name(),
            spaces,
            threads,
            transcript: None,
            theme: self.phones.theme.clone(),
            receipts: Vec::new(),
        }
    }

    pub fn set_phone_theme(&mut self, theme: ThemeView) {
        if self.phones.theme.as_ref() != Some(&theme) {
            self.phones.theme = Some(theme);
            self.publish_phone_views();
        }
    }

    fn phone_thread(&self, thread: &Thread) -> ThreadView {
        ThreadView {
            id: thread.id,
            title: if thread.title.trim().is_empty() {
                "Untitled".into()
            } else {
                thread.title.clone()
            },
            kind: match thread.kind {
                ThreadKind::Conversation => PhoneThreadKind::Conversation {
                    harness: crate::harness::display_name(&thread.harness).to_owned(),
                },
                ThreadKind::Factory => PhoneThreadKind::Factory,
            },
            status: condition_status(self.thread_condition(thread)),
            unseen: matches!(
                self.thread_activity(thread.id),
                ThreadActivity::Unseen { .. }
            ),
            updated_at: thread.updated_at.clone(),
        }
    }

    fn phone_transcript(&mut self, thread: ThreadId) -> Option<Transcript> {
        let mut transcript = self.phone_history(thread)?;
        transcript.prompting = self.phone_prompting(thread);
        Some(transcript)
    }

    fn phone_history(&mut self, thread: ThreadId) -> Option<Transcript> {
        let Some(session) = self.conversations.get(&thread) else {
            return self
                .snapshot
                .threads
                .iter()
                .any(|known| known.id == thread && known.kind == ThreadKind::Conversation)
                .then(|| Transcript {
                    thread,
                    earlier: false,
                    items: Vec::new(),
                    prompting: Prompting::default(),
                });
        };
        if session.state == ConversationState::Starting
            && session.messages.is_empty()
            && session.error.is_none()
        {
            return None;
        }
        let key = (
            session.generation,
            session.transcript_revision,
            session.tools_revision,
            session.requests.len() + usize::from(session.error.is_some()),
        );
        if let Some((cached, transcript)) = self.phones.transcripts.get(&thread)
            && *cached == key
        {
            return Some(transcript.clone());
        }
        let transcript = transcript_tail(
            thread,
            &session.messages,
            &session.tools,
            session.requests.values(),
            session.error.as_deref(),
        );
        self.phones
            .transcripts
            .insert(thread, (key, transcript.clone()));
        Some(transcript)
    }

    fn publish_phone_views(&mut self) {
        if self.phones.links.is_empty() {
            return;
        }
        let base = self.phone_workspace();
        let watched: Vec<(Uuid, Option<ThreadId>)> = self
            .phones
            .links
            .iter()
            .map(|(id, runtime)| (*id, runtime.watch))
            .collect();
        let mut used = std::collections::HashSet::new();
        for (id, watch) in watched {
            let mut view = base.clone();
            if let Some(thread) = watch.filter(|thread| base.threads.contains_key(thread)) {
                used.insert(thread);
                view.transcript = self.phone_transcript(thread);
            }
            if let Some(runtime) = self.phones.links.get(&id) {
                view.receipts = runtime.prompts.receipts();
            }
            if let Some(runtime) = self.phones.links.get_mut(&id)
                && runtime.shown.as_ref() != Some(&view)
            {
                runtime.link.show(view.clone());
                runtime.shown = Some(view);
            }
        }
        self.phones
            .transcripts
            .retain(|thread, _| used.contains(thread));
    }

    pub(super) fn close_phone_links(&mut self) -> Vec<async_channel::Receiver<()>> {
        self.cancel_phone_pairing();
        std::mem::take(&mut self.phones.links)
            .into_values()
            .map(|runtime| {
                runtime.link.close();
                runtime.link.finished()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LinkRecord, PROMPT_TTL_SECONDS, PairedPhone, PhonePrompts, PromptOutcome, RECEIPTS_KEPT,
        ThreadCondition, ThreadStatus, condition_status, http_origin, late_prompt_reason,
        token_from,
    };
    use uuid::Uuid;

    #[test]
    fn prompts_older_than_the_limit_are_refused() {
        let now = 1_000_000;
        assert_eq!(late_prompt_reason(now - PROMPT_TTL_SECONDS, now), None);
        assert_eq!(late_prompt_reason(now + 30, now), None);
        assert!(
            late_prompt_reason(now - PROMPT_TTL_SECONDS - 1, now)
                .unwrap()
                .contains("too late")
        );
    }

    #[test]
    fn each_prompt_is_answered_once_and_only_recent_receipts_are_kept() {
        let mut prompts = PhonePrompts::default();
        prompts.record(Uuid::from_u128(0), PromptOutcome::Started);
        prompts.record(
            Uuid::from_u128(0),
            PromptOutcome::Refused("a second answer".into()),
        );
        assert_eq!(prompts.receipts()[0].outcome, PromptOutcome::Started);
        for n in 1..=RECEIPTS_KEPT as u128 {
            prompts.record(Uuid::from_u128(n), PromptOutcome::Started);
        }
        assert_eq!(prompts.receipts().len(), RECEIPTS_KEPT);
        assert!(!prompts.answered(Uuid::from_u128(0)));
        assert!(prompts.answered(Uuid::from_u128(RECEIPTS_KEPT as u128)));
    }

    #[test]
    fn a_seintje_host_suggests_its_https_origin_as_phone_server() {
        assert_eq!(
            http_origin("wss://seintje.example.org/v1/relay/build-linux").as_deref(),
            Some("https://seintje.example.org")
        );
        assert_eq!(
            http_origin("ws://127.0.0.1:9000/remote").as_deref(),
            Some("http://127.0.0.1:9000")
        );
        assert_eq!(http_origin("https://x"), None);
    }

    #[test]
    fn thread_conditions_map_to_phone_statuses() {
        assert_eq!(condition_status(None), ThreadStatus::Idle);
        assert_eq!(
            condition_status(Some(ThreadCondition::Paused)),
            ThreadStatus::Waiting
        );
        assert_eq!(
            condition_status(Some(ThreadCondition::Unresolved)),
            ThreadStatus::Unresolved
        );
    }

    #[test]
    fn missing_token_variables_are_named() {
        let error = token_from(Some("DROSTJE_TEST_PHONE_TOKEN_THAT_IS_NOT_SET"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("DROSTJE_TEST_PHONE_TOKEN_THAT_IS_NOT_SET"));
        assert_eq!(token_from(Some("  ")).unwrap(), None);
        assert_eq!(token_from(None).unwrap(), None);
    }

    #[test]
    fn paired_phones_survive_a_settings_roundtrip() {
        let phone = PairedPhone {
            id: Uuid::nil(),
            name: "Pixel 9".into(),
            paired_at: "2026-11-17T10:00:00Z".into(),
            link: LinkRecord {
                server: "https://ntfy.example".into(),
                down: crate::phone::Topic::random(),
                up: crate::phone::Topic::random(),
                key: crate::phone::LinkKey::from_bytes([1; 32]),
            },
            token_env: None,
            up_since: Some("abc".into()),
        };
        let value = serde_json::to_value(vec![phone.clone()]).unwrap();
        let back: Vec<PairedPhone> = serde_json::from_value(value).unwrap();
        assert_eq!(back, vec![phone]);
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod app_tests {
    use super::{LinkRecord, PROMPT_TTL_SECONDS, PairedPhone, PromptOutcome, Prompting, unix_now};
    use crate::{
        app::{AppState, Bootstrap, ConversationSession},
        harness::{
            ContentBlock, ConversationCommand, ConversationEvent, ConversationState, Message,
        },
        model::{Project, ProjectDefaults, ThreadId},
        phone::{desktop_side::LinkEvent, view::ItemKind},
        platform,
        store::Store,
    };
    use gpui_kit::{AppContext as _, Entity, TestAppContext};
    use std::{collections::BTreeMap, path::PathBuf};
    use uuid::Uuid;

    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        _directory: Directory,
        state: Entity<AppState>,
        thread: ThreadId,
        phone: Uuid,
        generation: Uuid,
    }

    fn fixture(cx: &mut TestAppContext, git: bool) -> Fixture {
        cx.dispatcher.allow_parking();
        let directory =
            Directory(std::env::temp_dir().join(format!("drostje-phone-app-{}", Uuid::new_v4())));
        let root = directory.0.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let mut repositories = BTreeMap::new();
        let project = Project {
            id: Uuid::new_v4(),
            name: "drost".into(),
            root: root.clone(),
            archived: false,
            defaults: ProjectDefaults::default(),
        };
        if git {
            let status = std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()
                .unwrap();
            assert!(status.success(), "git init in the fixture");
            repositories.insert(
                project.id,
                platform::inspect_repository(&root).map_err(|error| format!("{error:#}")),
            );
        }
        let project_id = project.id;
        let store = Store::open(directory.0.join("data")).unwrap();
        smol::block_on(store.save_project(project)).unwrap();
        let snapshot = smol::block_on(store.snapshot()).unwrap();
        let state = cx.new(|cx| {
            AppState::new(
                Bootstrap {
                    data_dir: directory.0.join("data"),
                    store,
                    snapshot,
                    diagnostics: BTreeMap::new(),
                    repositories,
                    browser_tools: None,
                },
                cx,
            )
        });
        let phone = PairedPhone {
            id: Uuid::new_v4(),
            name: "Pixel 9".into(),
            paired_at: "2026-11-18T09:00:00Z".into(),
            link: LinkRecord {
                server: "http://127.0.0.1:9".into(),
                down: crate::phone::Topic::random(),
                up: crate::phone::Topic::random(),
                key: crate::phone::LinkKey::from_bytes([4; 32]),
            },
            token_env: None,
            up_since: None,
        };
        let phone_id = phone.id;
        let (thread, generation) = state.update(cx, |state, cx| {
            state.select_project(project_id);
            state.new_conversation("claude-code").unwrap();
            let thread = state.selected_thread.unwrap();
            let mut named = state
                .snapshot
                .threads
                .iter()
                .find(|known| known.id == thread)
                .cloned()
                .unwrap();
            named.title = "Fix the login redirect".into();
            state.save_thread(named);
            state.store_phones(std::slice::from_ref(&phone));
            state.start_phone_link(phone, cx);
            (thread, state.phones.links[&phone_id].generation)
        });
        Fixture {
            _directory: directory,
            state,
            thread,
            phone: phone_id,
            generation,
        }
    }

    fn send(f: &Fixture, event: LinkEvent, cx: &mut TestAppContext) {
        f.state.update(cx, |state, cx| {
            state.phone_link_event(f.phone, f.generation, event, cx)
        });
        cx.run_until_parked();
    }

    fn prompt(f: &Fixture, id: u128, text: &str, sent_at: i64, cx: &mut TestAppContext) {
        let event = LinkEvent::Prompt {
            id: Uuid::from_u128(id),
            thread: f.thread,
            text: text.into(),
            sent_at,
        };
        send(f, event, cx);
    }

    fn receipts(f: &Fixture, cx: &mut TestAppContext) -> Vec<(u128, PromptOutcome)> {
        f.state.read_with(cx, |state, _| {
            state.phones.links[&f.phone]
                .shown
                .as_ref()
                .expect("the phone was shown a view")
                .receipts
                .iter()
                .map(|receipt| (receipt.id.as_u128(), receipt.outcome.clone()))
                .collect()
        })
    }

    #[gpui_kit::test]
    fn a_phone_prompt_starts_one_turn_and_further_prompts_wait_for_it(cx: &mut TestAppContext) {
        let f = fixture(cx, true);
        let (commands, native) = async_channel::unbounded();
        f.state.update(cx, |state, _| {
            let mut session = ConversationSession::new(None);
            session.commands = Some(commands);
            session.state = ConversationState::Ready;
            session.persisted = true;
            state.conversations.insert(f.thread, session);
            assert_eq!(state.phone_prompting(f.thread), Prompting::Open);
        });

        prompt(&f, 1, "Also cover the logout redirect", unix_now(), cx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while native.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.run_until_parked();
        }
        match native.try_recv() {
            Ok(ConversationCommand::Submit { text, images, .. }) => {
                assert_eq!(text, "Also cover the logout redirect");
                assert!(images.is_empty());
            }
            other => panic!("expected the prompt to start a Turn, got {other:?}"),
        }
        assert_eq!(receipts(&f, cx), vec![(1, PromptOutcome::Started)]);
        f.state.read_with(cx, |state, _| {
            assert_eq!(
                state.phone_prompting(f.thread),
                Prompting::Closed("Claude Code is working. Wait for this Turn to finish".into())
            );
            assert!(state.active_threads.contains(&f.thread));
        });

        prompt(&f, 1, "Also cover the logout redirect", unix_now(), cx);
        prompt(&f, 2, "And the signup page", unix_now(), cx);
        prompt(&f, 3, "Old news", unix_now() - PROMPT_TTL_SECONDS - 60, cx);
        assert!(
            native.try_recv().is_err(),
            "a replay, a busy agent and a late prompt start nothing"
        );
        let answered = receipts(&f, cx);
        assert_eq!(answered.len(), 3, "{answered:?}");
        assert_eq!(answered[0], (1, PromptOutcome::Started));
        assert_eq!(
            answered[1],
            (
                2,
                PromptOutcome::Refused(
                    "Claude Code is working. Wait for this Turn to finish".into()
                )
            )
        );
        assert!(
            matches!(&answered[2], (3, PromptOutcome::Refused(reason)) if reason.contains("too late"))
        );
    }

    #[gpui_kit::test]
    fn prompts_for_threads_that_cannot_take_them_are_refused_with_the_reason(
        cx: &mut TestAppContext,
    ) {
        let f = fixture(cx, true);
        prompt(&f, 1, "   ", unix_now(), cx);
        prompt(&f, 2, "Hello", unix_now(), cx);
        let answered = receipts(&f, cx);
        assert_eq!(
            answered[0],
            (1, PromptOutcome::Refused("Enter a prompt".into()))
        );
        assert_eq!(
            answered[1],
            (
                2,
                PromptOutcome::Refused("Open the conversation harness first".into())
            )
        );
        let factory = f.state.update(cx, |state, _| {
            state.new_thread(crate::model::ThreadKind::Factory).unwrap();
            state.selected_thread.unwrap()
        });
        f.state.read_with(cx, |state, _| {
            assert_eq!(
                state.phone_prompting(factory),
                Prompting::Closed("Factory threads do not accept prompts".into())
            );
            assert_eq!(
                state.phone_prompting(Uuid::new_v4()),
                Prompting::Closed("This Thread is no longer on the computer".into())
            );
        });
    }

    #[gpui_kit::test]
    fn watching_a_thread_opens_its_conversation_so_the_phone_gets_its_history(
        cx: &mut TestAppContext,
    ) {
        let f = fixture(cx, false);
        f.state.read_with(cx, |state, _| {
            assert!(!state.conversations.contains_key(&f.thread));
        });
        f.state.update(cx, |state, cx| {
            state.phone_link_event(f.phone, f.generation, LinkEvent::Watch(Some(f.thread)), cx);
            assert!(
                state.conversations.contains_key(&f.thread),
                "the harness opens for the phone"
            );
            assert_eq!(
                state.phone_transcript(f.thread),
                None,
                "the phone shows loading while the harness starts"
            );
            let message = |id: &str, role: &str, text: &str| Message {
                id: id.into(),
                role: role.into(),
                content: vec![ContentBlock::Text(text.into())],
                raw: None,
                tool_call_id: None,
                error: false,
                exit_code: None,
            };
            state.receive_conversation(
                f.thread,
                ConversationEvent::History {
                    messages: vec![
                        message("u1", "user", "Why does login loop?"),
                        message("a1", "assistant", "The return URL is read too early."),
                    ],
                    next_cursor: None,
                },
                cx,
            );
            let transcript = state
                .phone_transcript(f.thread)
                .expect("history makes a transcript");
            let kinds: Vec<_> = transcript.items.iter().map(|item| &item.kind).collect();
            assert_eq!(kinds, vec![&ItemKind::User, &ItemKind::Assistant]);
            assert_eq!(
                transcript.prompting,
                Prompting::Closed("Starting Claude Code…".into())
            );
        });
    }
}
