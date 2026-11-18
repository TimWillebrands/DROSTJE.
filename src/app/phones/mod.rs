mod transcript;

use super::*;
use crate::phone::{
    Server,
    desktop_side::{DesktopLink, LinkEvent, PairingEvent, PairingSession},
    link::{Connection, LinkRecord, PAIRING_TTL},
    view::{
        PhoneView, ProjectView, SpaceView, ThemeView, ThreadKind as PhoneThreadKind, ThreadStatus,
        ThreadView, Transcript,
    },
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

pub use transcript::transcript_tail;

pub const PHONE_SERVER_SETTING: &str = "phone_server";
pub const PHONE_TOKEN_ENV_SETTING: &str = "phone_token_env";
const PHONES_SETTING: &str = "phones";
const PUBLISH_EVERY: Duration = Duration::from_millis(400);

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
            },
        );
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |state, cx| {
                        state.phone_link_event(id, generation, event);
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

    fn phone_link_event(&mut self, id: Uuid, generation: Uuid, event: LinkEvent) {
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
            }
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
                });
        };
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
        LinkRecord, PairedPhone, ThreadCondition, ThreadStatus, condition_status, http_origin,
        token_from,
    };
    use uuid::Uuid;

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
