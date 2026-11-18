use std::sync::Arc;

use drostje_phone::{
    PairingOffer,
    link::Connection,
    phone_side::{PairedDesktop, PhoneEvent, PhonePairingEvent, Snapshot},
    view::{PhoneView, SpaceView, ThemeView},
};
use uuid::Uuid;

pub const QUIET_AFTER_SECONDS: i64 = 5 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Home,
    Space(Uuid),
    Thread(Uuid),
    Link,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    Error(String),
    Info(String),
}

#[derive(Clone, Debug)]
pub enum Phase {
    Welcome {
        notice: Option<Notice>,
    },
    Pairing {
        server: String,
        check: Option<String>,
    },
    Linked(Box<Linked>),
}

#[derive(Clone, Debug)]
pub struct Linked {
    pub paired: PairedDesktop,
    pub view: Arc<PhoneView>,
    pub synced: bool,
    pub desktop_online: Option<bool>,
    pub heard_at: Option<i64>,
    pub connection: Connection,
    pub stack: Vec<Route>,
    pub confirm_unpair: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    StartPairing(PairingOffer),
    CancelPairing,
    StartLink(PairedDesktop, Option<PhoneView>),
    StopLink,
    Watch(Option<Uuid>),
    Unpair,
    SavePairing(Option<PairedDesktop>),
    SaveView(PhoneView),
    LeaveApp,
}

#[derive(Clone, Debug)]
pub enum Input {
    Link(String),
    Pairing(PhonePairingEvent),
    Phone(PhoneEvent),
    Open(Route),
    Back,
    CancelPairing,
    AskUnpair,
    CancelUnpair,
    ConfirmUnpair,
    DismissNotice,
}

pub struct PhoneApp {
    pub phase: Phase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopPresence {
    Connecting,
    Online,
    Offline,
    Quiet,
}

impl Linked {
    fn new(paired: PairedDesktop, cached: Option<PhoneView>) -> Self {
        Self {
            paired,
            view: Arc::new(cached.unwrap_or_default()),
            synced: false,
            desktop_online: None,
            heard_at: None,
            connection: Connection::Connecting,
            stack: vec![Route::Home],
            confirm_unpair: false,
        }
    }

    pub fn route(&self) -> &Route {
        self.stack.last().unwrap_or(&Route::Home)
    }

    pub fn desktop_name(&self) -> &str {
        if self.view.desktop.is_empty() {
            &self.paired.desktop
        } else {
            &self.view.desktop
        }
    }

    pub fn presence(&self, now: i64) -> DesktopPresence {
        match self.desktop_online {
            None => DesktopPresence::Connecting,
            Some(false) => DesktopPresence::Offline,
            Some(true)
                if self
                    .heard_at
                    .is_some_and(|heard| now - heard > QUIET_AFTER_SECONDS) =>
            {
                DesktopPresence::Quiet
            }
            Some(true) => DesktopPresence::Online,
        }
    }

    pub fn single_space(&self) -> Option<&SpaceView> {
        match self.view.spaces.as_slice() {
            [space] => Some(space),
            _ => None,
        }
    }

    fn watched(&self) -> Option<Uuid> {
        match self.route() {
            Route::Thread(id) => Some(*id),
            Route::Home | Route::Space(_) | Route::Link => None,
        }
    }

    fn prune_missing(&mut self) {
        let view = self.view.clone();
        self.stack.retain(|route| match route {
            Route::Home | Route::Link => true,
            Route::Space(id) => view.space(*id).is_some(),
            Route::Thread(id) => view.threads.contains_key(id),
        });
        if self.stack.is_empty() {
            self.stack.push(Route::Home);
        }
    }
}

impl PhoneApp {
    pub fn theme(&self) -> Option<&ThemeView> {
        match &self.phase {
            Phase::Linked(linked) => linked.view.theme.as_ref(),
            Phase::Welcome { .. } | Phase::Pairing { .. } => None,
        }
    }

    pub fn start(paired: Option<PairedDesktop>, cached: Option<PhoneView>) -> (Self, Vec<Effect>) {
        match paired {
            Some(paired) => (
                Self {
                    phase: Phase::Linked(Box::new(Linked::new(paired.clone(), cached.clone()))),
                },
                vec![Effect::StartLink(paired, cached)],
            ),
            None => (
                Self {
                    phase: Phase::Welcome { notice: None },
                },
                Vec::new(),
            ),
        }
    }

    pub fn update(&mut self, input: Input) -> Vec<Effect> {
        match input {
            Input::Link(link) => self.scanned(&link),
            Input::Pairing(event) => self.pairing(event),
            Input::Phone(event) => self.phone(event),
            Input::Open(route) => self.open(route),
            Input::Back => self.back(),
            Input::CancelPairing => match self.phase {
                Phase::Pairing { .. } => {
                    self.phase = Phase::Welcome { notice: None };
                    vec![Effect::CancelPairing]
                }
                Phase::Welcome { .. } | Phase::Linked(_) => Vec::new(),
            },
            Input::AskUnpair => {
                if let Phase::Linked(linked) = &mut self.phase {
                    linked.confirm_unpair = true;
                }
                Vec::new()
            }
            Input::CancelUnpair => {
                if let Phase::Linked(linked) = &mut self.phase {
                    linked.confirm_unpair = false;
                }
                Vec::new()
            }
            Input::ConfirmUnpair => match &self.phase {
                Phase::Linked(linked) if linked.confirm_unpair => {
                    let desktop = linked.desktop_name().to_owned();
                    self.phase = Phase::Welcome {
                        notice: Some(Notice::Info(format!(
                            "This phone no longer follows {desktop}"
                        ))),
                    };
                    vec![Effect::Unpair, Effect::SavePairing(None)]
                }
                Phase::Linked(_) | Phase::Welcome { .. } | Phase::Pairing { .. } => Vec::new(),
            },
            Input::DismissNotice => {
                if let Phase::Welcome { notice } = &mut self.phase {
                    *notice = None;
                }
                Vec::new()
            }
        }
    }

    fn scanned(&mut self, link: &str) -> Vec<Effect> {
        let offer = match PairingOffer::parse(link) {
            Ok(offer) => offer,
            Err(error) => {
                if let Phase::Welcome { notice } = &mut self.phase {
                    *notice = Some(Notice::Error(error.to_string()));
                }
                return Vec::new();
            }
        };
        let mut effects = Vec::new();
        match &self.phase {
            Phase::Linked(_) => effects.push(Effect::StopLink),
            Phase::Pairing { .. } => effects.push(Effect::CancelPairing),
            Phase::Welcome { .. } => {}
        }
        self.phase = Phase::Pairing {
            server: offer.server.clone(),
            check: None,
        };
        effects.push(Effect::StartPairing(offer));
        effects
    }

    fn pairing(&mut self, event: PhonePairingEvent) -> Vec<Effect> {
        let Phase::Pairing { check, .. } = &mut self.phase else {
            return Vec::new();
        };
        match event {
            PhonePairingEvent::Sent { check: code } => {
                *check = Some(code.to_string());
                Vec::new()
            }
            PhonePairingEvent::Paired(paired) => {
                self.phase = Phase::Linked(Box::new(Linked::new(paired.clone(), None)));
                vec![
                    Effect::SavePairing(Some(paired.clone())),
                    Effect::StartLink(paired, None),
                ]
            }
            PhonePairingEvent::Declined => {
                self.phase = Phase::Welcome {
                    notice: Some(Notice::Error(
                        "The desktop did not allow this phone. Scan a new code to try again".into(),
                    )),
                };
                Vec::new()
            }
            PhonePairingEvent::Failed(reason) => {
                self.phase = Phase::Welcome {
                    notice: Some(Notice::Error(reason)),
                };
                Vec::new()
            }
        }
    }

    fn phone(&mut self, event: PhoneEvent) -> Vec<Effect> {
        let Phase::Linked(linked) = &mut self.phase else {
            return Vec::new();
        };
        match event {
            PhoneEvent::Snapshot(Snapshot {
                view,
                synced,
                desktop_online,
                heard_at,
            }) => {
                let changed = synced && *linked.view != *view;
                linked.view = view;
                linked.synced = synced;
                linked.desktop_online = desktop_online;
                linked.heard_at = heard_at.or(linked.heard_at);
                linked.prune_missing();
                if changed {
                    vec![Effect::SaveView((*linked.view).clone())]
                } else {
                    Vec::new()
                }
            }
            PhoneEvent::Position(position) => {
                linked.paired.since = Some(position);
                vec![Effect::SavePairing(Some(linked.paired.clone()))]
            }
            PhoneEvent::Connection(connection) => {
                linked.connection = connection;
                Vec::new()
            }
            PhoneEvent::Revoked => {
                let desktop = linked.desktop_name().to_owned();
                self.phase = Phase::Welcome {
                    notice: Some(Notice::Error(format!(
                        "{desktop} removed this phone. Pair again to follow it"
                    ))),
                };
                vec![Effect::StopLink, Effect::SavePairing(None)]
            }
            PhoneEvent::Closed => Vec::new(),
        }
    }

    fn open(&mut self, route: Route) -> Vec<Effect> {
        let Phase::Linked(linked) = &mut self.phase else {
            return Vec::new();
        };
        if linked.route() == &route {
            return Vec::new();
        }
        let before = linked.watched();
        linked.stack.push(route);
        let after = linked.watched();
        if before == after {
            Vec::new()
        } else {
            vec![Effect::Watch(after)]
        }
    }

    fn back(&mut self) -> Vec<Effect> {
        match &mut self.phase {
            Phase::Linked(linked) => {
                if linked.confirm_unpair {
                    linked.confirm_unpair = false;
                    return Vec::new();
                }
                if linked.stack.len() <= 1 {
                    return vec![Effect::LeaveApp];
                }
                let before = linked.watched();
                linked.stack.pop();
                let after = linked.watched();
                if before == after {
                    Vec::new()
                } else {
                    vec![Effect::Watch(after)]
                }
            }
            Phase::Pairing { .. } => {
                self.phase = Phase::Welcome { notice: None };
                vec![Effect::CancelPairing]
            }
            Phase::Welcome { .. } => vec![Effect::LeaveApp],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drostje_phone::{
        DesktopPairing, LinkKey, Topic,
        link::LinkRecord,
        view::{ProjectView, ThreadKind, ThreadStatus, ThreadView},
    };

    fn paired() -> PairedDesktop {
        PairedDesktop {
            desktop: "Studio".into(),
            token: None,
            link: LinkRecord {
                server: "https://ntfy.example".into(),
                down: Topic::random(),
                up: Topic::random(),
                key: LinkKey::from_bytes([1; 32]),
            },
            since: None,
        }
    }

    fn thread(id: u128) -> ThreadView {
        ThreadView {
            id: Uuid::from_u128(id),
            title: format!("Thread {id}"),
            kind: ThreadKind::Conversation {
                harness: "Claude Code".into(),
            },
            status: ThreadStatus::Idle,
            unseen: false,
            updated_at: "2026-11-17T10:00:00Z".into(),
        }
    }

    fn view(spaces: usize) -> PhoneView {
        PhoneView {
            desktop: "Studio".into(),
            spaces: (0..spaces as u128)
                .map(|n| SpaceView {
                    id: Uuid::from_u128(100 + n),
                    name: format!("Space {n}"),
                    projects: vec![ProjectView {
                        id: Uuid::from_u128(200 + n),
                        name: "drost".into(),
                        threads: vec![Uuid::from_u128(n + 1)],
                    }],
                })
                .collect(),
            threads: (0..spaces as u128)
                .map(|n| (Uuid::from_u128(n + 1), thread(n + 1)))
                .collect(),
            transcript: None,
            theme: None,
        }
    }

    fn snapshot(view: PhoneView) -> Input {
        Input::Phone(PhoneEvent::Snapshot(Snapshot {
            view: Arc::new(view),
            synced: true,
            desktop_online: Some(true),
            heard_at: Some(1_000),
        }))
    }

    fn linked(app: &PhoneApp) -> &Linked {
        match &app.phase {
            Phase::Linked(linked) => linked,
            other => panic!("expected linked, got {other:?}"),
        }
    }

    #[test]
    fn a_fresh_install_welcomes_and_a_paired_one_reconnects_with_its_cache() {
        let (app, effects) = PhoneApp::start(None, None);
        assert!(matches!(app.phase, Phase::Welcome { notice: None }));
        assert!(effects.is_empty());
        let (app, effects) = PhoneApp::start(Some(paired()), Some(view(1)));
        assert_eq!(linked(&app).view.spaces.len(), 1);
        assert!(!linked(&app).synced);
        assert!(matches!(&effects[..], [Effect::StartLink(_, Some(_))]));
    }

    #[test]
    fn the_desktop_theme_applies_while_linked_and_ends_with_the_pairing() {
        let themed = PhoneView {
            theme: Some(ThemeView {
                appearance: drostje_phone::view::Appearance::Dark,
                light: serde_json::json!({"name": "Ink Light"}),
                dark: serde_json::json!({"name": "Ink Dark"}),
            }),
            ..view(1)
        };
        let (mut app, _) = PhoneApp::start(Some(paired()), Some(view(1)));
        assert_eq!(app.theme(), None, "a cache from before themes");
        app.update(snapshot(themed.clone()));
        assert_eq!(app.theme(), themed.theme.as_ref());
        app.update(Input::AskUnpair);
        app.update(Input::ConfirmUnpair);
        assert!(matches!(app.phase, Phase::Welcome { .. }));
        assert_eq!(app.theme(), None, "an unpaired phone wears Delft again");
    }

    #[test]
    fn scanning_a_code_pairs_and_saves_the_desktop() {
        let (mut app, _) = PhoneApp::start(None, None);
        let link = DesktopPairing::new("https://ntfy.example".into(), None)
            .offer
            .to_link();
        let effects = app.update(Input::Link(link));
        assert!(matches!(&effects[..], [Effect::StartPairing(_)]));
        assert!(matches!(app.phase, Phase::Pairing { check: None, .. }));
        let effects = app.update(Input::Pairing(PhonePairingEvent::Paired(paired())));
        assert!(matches!(
            &effects[..],
            [Effect::SavePairing(Some(_)), Effect::StartLink(_, None)]
        ));
        assert_eq!(linked(&app).route(), &Route::Home);
    }

    #[test]
    fn other_qr_codes_explain_themselves_on_the_welcome_screen() {
        let (mut app, _) = PhoneApp::start(None, None);
        assert!(
            app.update(Input::Link("https://example.org".into()))
                .is_empty()
        );
        let Phase::Welcome {
            notice: Some(Notice::Error(message)),
        } = &app.phase
        else {
            panic!("expected an error notice")
        };
        assert_eq!(message, "This QR code is not a DROSTJE pairing code");
    }

    #[test]
    fn declined_or_failed_pairing_returns_to_welcome_with_the_reason() {
        let (mut app, _) = PhoneApp::start(None, None);
        app.phase = Phase::Pairing {
            server: "https://x".into(),
            check: Some("123 456".into()),
        };
        app.update(Input::Pairing(PhonePairingEvent::Declined));
        assert!(matches!(
            app.phase,
            Phase::Welcome {
                notice: Some(Notice::Error(_))
            }
        ));
    }

    #[test]
    fn opening_a_thread_watches_it_and_back_stops_watching() {
        let (mut app, _) = PhoneApp::start(Some(paired()), None);
        app.update(snapshot(view(2)));
        let space = Uuid::from_u128(100);
        assert!(app.update(Input::Open(Route::Space(space))).is_empty());
        assert_eq!(
            app.update(Input::Open(Route::Thread(Uuid::from_u128(1)))),
            vec![Effect::Watch(Some(Uuid::from_u128(1)))]
        );
        assert_eq!(app.update(Input::Back), vec![Effect::Watch(None)]);
        assert!(app.update(Input::Back).is_empty());
        assert_eq!(app.update(Input::Back), vec![Effect::LeaveApp]);
    }

    #[test]
    fn a_thread_that_disappears_on_the_desktop_closes_on_the_phone() {
        let (mut app, _) = PhoneApp::start(Some(paired()), None);
        app.update(snapshot(view(2)));
        app.update(Input::Open(Route::Thread(Uuid::from_u128(2))));
        app.update(snapshot(view(1)));
        assert_eq!(linked(&app).route(), &Route::Home);
    }

    #[test]
    fn only_synced_changes_are_cached() {
        let (mut app, _) = PhoneApp::start(Some(paired()), None);
        assert!(matches!(
            &app.update(snapshot(view(1)))[..],
            [Effect::SaveView(_)]
        ));
        assert!(app.update(snapshot(view(1))).is_empty());
    }

    #[test]
    fn revocation_forgets_the_desktop_and_says_why() {
        let (mut app, _) = PhoneApp::start(Some(paired()), None);
        app.update(snapshot(view(1)));
        let effects = app.update(Input::Phone(PhoneEvent::Revoked));
        assert_eq!(effects, vec![Effect::StopLink, Effect::SavePairing(None)]);
        let Phase::Welcome {
            notice: Some(Notice::Error(message)),
        } = &app.phase
        else {
            panic!("expected a notice")
        };
        assert!(message.starts_with("Studio removed this phone"));
    }

    #[test]
    fn unpairing_needs_a_confirmation_and_back_cancels_it() {
        let (mut app, _) = PhoneApp::start(Some(paired()), None);
        assert!(app.update(Input::ConfirmUnpair).is_empty());
        app.update(Input::AskUnpair);
        assert!(app.update(Input::Back).is_empty());
        assert!(!linked(&app).confirm_unpair);
        app.update(Input::AskUnpair);
        assert_eq!(
            app.update(Input::ConfirmUnpair),
            vec![Effect::Unpair, Effect::SavePairing(None)]
        );
    }

    #[test]
    fn presence_turns_quiet_without_updates_and_offline_on_farewell() {
        let (mut app, _) = PhoneApp::start(Some(paired()), None);
        assert_eq!(linked(&app).presence(0), DesktopPresence::Connecting);
        app.update(snapshot(view(1)));
        assert_eq!(linked(&app).presence(1_010), DesktopPresence::Online);
        assert_eq!(
            linked(&app).presence(1_000 + QUIET_AFTER_SECONDS + 1),
            DesktopPresence::Quiet
        );
        app.update(Input::Phone(PhoneEvent::Snapshot(Snapshot {
            view: Arc::new(view(1)),
            synced: true,
            desktop_online: Some(false),
            heard_at: Some(2_000),
        })));
        assert_eq!(linked(&app).presence(2_001), DesktopPresence::Offline);
    }

    #[test]
    fn positions_are_saved_with_the_pairing() {
        let (mut app, _) = PhoneApp::start(Some(paired()), None);
        let effects = app.update(Input::Phone(PhoneEvent::Position("m7".into())));
        let [Effect::SavePairing(Some(saved))] = &effects[..] else {
            panic!("expected a save")
        };
        assert_eq!(saved.since.as_deref(), Some("m7"));
    }
}
