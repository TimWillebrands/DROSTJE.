use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use drostje_mobile::{
    links,
    state::{Composer, Notice, Phase, PromptStatus, Route},
    storage::Storage,
    ui::{self, PhoneRoot},
};
use drostje_ntfy::test_server::TestServer;
use drostje_phone::{
    PairingOffer, Server,
    desktop_side::{DesktopLink, LinkEvent, PairingEvent, PairingSession},
    view::{
        Appearance, ItemKind, PhoneView, ProjectView, PromptOutcome, PromptReceipt, Prompting,
        SpaceView, ThemeView, ThreadKind, ThreadStatus, ThreadView, ToolState, Transcript,
        TranscriptItem,
    },
};
use gpui_kit::{
    AnyWindowHandle, Bounds, Entity, HeadlessAppContext, WindowBounds, WindowOptions,
    component::{Theme, ThemeMode},
    px, size,
    test::TestWindowExt,
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(20);

struct Phone {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    root: Entity<PhoneRoot>,
    shots: PathBuf,
}

impl Phone {
    fn open(storage: Storage) -> Self {
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(gpui_kit::assets::AllAssets::new("")),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.allow_parking();
        cx.update(gpui_kit::init);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Default::default(),
                size: size(px(412.), px(892.)),
            })),
            focus: true,
            show: false,
            ..Default::default()
        };
        let (window, root) = cx
            .update(|cx| ui::open_window(storage, options, cx))
            .expect("the phone window opens headless");
        let shots = std::env::var_os("DROSTJE_PHONE_SHOTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("drostje-phone-shots"));
        std::fs::create_dir_all(&shots).expect("screenshot directory");
        let mut phone = Self {
            cx,
            window,
            root,
            shots,
        };
        phone.settle();
        phone
    }

    fn settle(&mut self) {
        self.cx.run_until_parked();
        self.cx
            .update_window(self.window, |_, window, cx| window.render_frame(cx))
            .expect("window renders");
        self.cx.run_until_parked();
    }

    fn phase<T>(&mut self, read: impl FnOnce(&Phase) -> T) -> T {
        let root = self.root.clone();
        self.cx.update(|cx| read(root.read(cx).phase()))
    }

    fn wait(&mut self, what: &str, mut done: impl FnMut(&Phase) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.settle();
            if self.phase(&mut done) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn click(&mut self, id: &str) {
        let id = id.to_owned();
        self.cx
            .update_window(self.window, |_, window, cx| window.click(id, cx))
            .expect("click reaches the window");
        self.settle();
    }

    fn press(&mut self, key: &str) {
        self.cx
            .update_window(self.window, |_, window, cx| window.press(key, cx))
            .expect("key reaches the window");
        self.settle();
    }

    fn type_text(&mut self, text: &str) {
        self.cx
            .update_window(self.window, |_, window, cx| window.input(text, cx))
            .expect("text reaches the window");
        self.settle();
    }

    fn disabled(&mut self, id: &str) -> Option<bool> {
        let id = id.to_owned();
        self.cx
            .update_window(self.window, |_, window, _| window.find(id).disabled())
            .expect("window can be queried")
    }

    fn composer_text(&mut self, thread: Uuid) -> Option<String> {
        let root = self.root.clone();
        self.cx.update(|cx| root.read(cx).composer_text(thread, cx))
    }

    fn visible(&mut self, id: &str) -> bool {
        let id = id.to_owned();
        self.cx
            .update_window(self.window, |_, window, _| {
                window.try_find(id).is_some_and(|element| element.visible())
            })
            .expect("window can be queried")
    }

    fn shot(&mut self, name: &str) {
        for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
            self.cx.update(|cx| Theme::change(mode, None, cx));
            self.settle();
            let image = self
                .cx
                .capture_screenshot(self.window)
                .expect("Metal renders the phone offscreen");
            image
                .save(self.shots.join(format!("{name}-{suffix}.png")))
                .expect("screenshot saves");
        }
    }

    fn look<T>(&mut self, read: impl FnOnce(&drostje_mobile::look::Look) -> T) -> T {
        let root = self.root.clone();
        self.cx.update(|cx| {
            read(
                root.read(cx)
                    .look()
                    .expect("the phone always wears a theme"),
            )
        })
    }

    fn linked<T>(&mut self, read: impl FnOnce(&drostje_mobile::state::Linked) -> T) -> T {
        self.phase(|phase| match phase {
            Phase::Linked(linked) => read(linked),
            other => panic!("expected a linked phone, found {other:?}"),
        })
    }
}

fn next<T, E>(events: &async_channel::Receiver<E>, mut pick: impl FnMut(E) -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(event) => {
                if let Some(found) = pick(event) {
                    return found;
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("expected desktop event did not arrive")
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn thread(n: u128, title: &str, harness: &str, status: ThreadStatus, unseen: bool) -> ThreadView {
    ThreadView {
        id: id(n),
        title: title.into(),
        kind: if harness == "Factory" {
            ThreadKind::Factory
        } else {
            ThreadKind::Conversation {
                harness: harness.into(),
            }
        },
        status,
        unseen,
        updated_at: "2026-11-17T09:12:00Z".into(),
    }
}

fn desktop_view(transcript: Option<Transcript>) -> PhoneView {
    let threads = [
        thread(
            1,
            "Fix the login redirect loop",
            "Claude Code",
            ThreadStatus::Running,
            false,
        ),
        thread(
            2,
            "Upgrade the payroll export to the 2027 tax tables",
            "Codex",
            ThreadStatus::Waiting,
            false,
        ),
        thread(
            3,
            "Review timesheet approval flow",
            "OMP",
            ThreadStatus::Idle,
            true,
        ),
        thread(
            4,
            "Nightly Factory run",
            "Factory",
            ThreadStatus::Failed,
            false,
        ),
        thread(5, "Release notes for 0.9", "Pi", ThreadStatus::Idle, false),
    ];
    PhoneView {
        desktop: "Studio Mac".into(),
        spaces: vec![
            SpaceView {
                id: id(100),
                name: "Default".into(),
                projects: vec![
                    ProjectView {
                        id: id(200),
                        name: "drost".into(),
                        threads: vec![id(1), id(2), id(3)],
                    },
                    ProjectView {
                        id: id(201),
                        name: "cootje".into(),
                        threads: vec![id(4)],
                    },
                ],
            },
            SpaceView {
                id: id(101),
                name: "Releases".into(),
                projects: vec![ProjectView {
                    id: id(202),
                    name: "drost".into(),
                    threads: vec![id(5)],
                }],
            },
        ],
        threads: threads
            .into_iter()
            .map(|thread| (thread.id, thread))
            .collect(),
        transcript,
        theme: None,
        receipts: Vec::new(),
    }
}

fn bundled_theme(name: &str) -> serde_json::Value {
    drostje_themes::bundled()
        .into_iter()
        .find(|theme| theme["name"] == name)
        .unwrap_or_else(|| panic!("{name} is bundled"))
}

fn themed(view: PhoneView, light: &str, dark: &str) -> PhoneView {
    PhoneView {
        theme: Some(ThemeView {
            appearance: Appearance::System,
            light: bundled_theme(light),
            dark: bundled_theme(dark),
        }),
        ..view
    }
}

fn transcript() -> Transcript {
    let item = |id: &str, kind: ItemKind, text: &str| TranscriptItem {
        id: id.into(),
        kind,
        text: text.into(),
        clipped: false,
    };
    Transcript {
        thread: id(1),
        earlier: true,
        items: vec![
            item(
                "u1",
                ItemKind::User,
                "The login page keeps redirecting to itself after SSO. Can you find out why?",
            ),
            item(
                "t1",
                ItemKind::Thinking,
                "The redirect target is read from the session before the SSO callback stores it.",
            ),
            item(
                "a1",
                ItemKind::Assistant,
                "I found the loop: `returnUrl` is read **before** the callback stores it.\n\n- `auth/callback.ts` writes the session\n- `middleware.ts` reads it too early",
            ),
            item(
                "tool:c1",
                ItemKind::Tool {
                    name: "bash".into(),
                    state: ToolState::Done,
                },
                "Run the auth tests",
            ),
            item(
                "tool:c2",
                ItemKind::Tool {
                    name: "edit".into(),
                    state: ToolState::Running,
                },
                "Move the session read after the callback",
            ),
        ],
        prompting: Prompting::Closed("Claude Code is working. Wait for this Turn to finish".into()),
    }
}

fn paired_phone(ntfy: &TestServer, data: &std::path::Path) -> (Phone, DesktopLink) {
    let mut phone = Phone::open(Storage::new(data.to_path_buf()));
    let session = PairingSession::start(
        Server::new(ntfy.address(), None).unwrap(),
        None,
        Duration::from_secs(60),
    );
    next(&session.events, |event| {
        (event == PairingEvent::Listening).then_some(())
    });
    links::deliver(session.offer.to_link());
    phone.wait("the pairing request", |phase| {
        matches!(phase, Phase::Pairing { check: Some(_), .. })
    });
    next(&session.events, |event| {
        matches!(event, PairingEvent::Incoming { .. }).then_some(())
    });
    session.accept("Studio Mac".into());
    let link = next(&session.events, |event| match event {
        PairingEvent::Paired { link, .. } => Some(link),
        _ => None,
    });
    phone.wait("pairing", |phase| matches!(phase, Phase::Linked(_)));
    let desktop = DesktopLink::start(Server::new(ntfy.address(), None).unwrap(), link, None);
    (phone, desktop)
}

fn conversation(prompting: Prompting, receipts: Vec<PromptReceipt>, asked: bool) -> PhoneView {
    let mut transcript = transcript();
    transcript.prompting = prompting;
    transcript.items.truncate(3);
    if asked {
        let mut prompt = transcript.items[0].clone();
        prompt.id = "u2".into();
        prompt.text = "Also cover the logout redirect".into();
        transcript.items.push(prompt);
    }
    PhoneView {
        receipts,
        ..desktop_view(Some(transcript))
    }
}

fn the_desktop_prompt(desktop: &DesktopLink) -> (Uuid, String) {
    next(&desktop.events, |event| match event {
        LinkEvent::Prompt { id, text, .. } => Some((id, text)),
        _ => None,
    })
}

fn a_phone_sends_prompts_and_learns_what_the_desktop_did_with_them() {
    let ntfy = TestServer::start();
    let data = std::env::temp_dir().join(format!("drostje-phone-{}", Uuid::new_v4()));
    let (mut phone, desktop) = paired_phone(&ntfy, &data);
    desktop.show(desktop_view(None));
    phone.wait(
        "sync",
        |phase| matches!(phase, Phase::Linked(linked) if linked.synced),
    );
    phone.click(&format!("space-{}", id(100)));
    phone.click(&format!("thread-{}", id(1)));
    next(&desktop.events, |event| {
        (event == LinkEvent::Watch(Some(id(1)))).then_some(())
    });
    desktop.show(conversation(
        Prompting::Closed("Claude Code is working. Wait for this Turn to finish".into()),
        Vec::new(),
        false,
    ));
    phone.wait("a busy agent", |phase| {
        matches!(phase, Phase::Linked(linked) if matches!(linked.composer(id(1)), Composer::Closed(reason) if reason.contains("working")))
    });
    assert!(phone.visible("composer-status"));
    phone.click("prompt-input");
    phone.type_text("Also cover the logout redirect");
    assert_eq!(phone.disabled("send-prompt"), Some(true));
    phone.shot("17-composer-busy");

    desktop.show(conversation(Prompting::Open, Vec::new(), false));
    phone.wait(
        "an idle agent",
        |phase| matches!(phase, Phase::Linked(linked) if linked.composer(id(1)) == Composer::Open),
    );
    assert_ne!(phone.disabled("send-prompt"), Some(true));
    phone.shot("18-composer-ready");
    phone.press("enter");
    assert_eq!(
        phone.linked(|linked| linked.composer(id(1))),
        Composer::Sending
    );
    phone.shot("19-prompt-sending");
    let (first, text) = the_desktop_prompt(&desktop);
    assert_eq!(text, "Also cover the logout redirect");
    desktop.show(conversation(
        Prompting::Closed("Claude Code is working. Wait for this Turn to finish".into()),
        vec![PromptReceipt {
            id: first,
            outcome: PromptOutcome::Started,
        }],
        true,
    ));
    phone.wait(
        "the started Turn",
        |phase| matches!(phase, Phase::Linked(linked) if linked.prompt.is_none()),
    );
    assert_eq!(phone.composer_text(id(1)).as_deref(), Some(""));
    phone.shot("20-prompt-started");

    desktop.show(conversation(Prompting::Open, Vec::new(), true));
    phone.wait(
        "the agent to finish",
        |phase| matches!(phase, Phase::Linked(linked) if linked.composer(id(1)) == Composer::Open),
    );
    phone.click("prompt-input");
    phone.type_text("Ship");
    phone.press("shift-enter");
    phone.type_text("it");
    assert_eq!(phone.composer_text(id(1)).as_deref(), Some("Ship\nit"));
    phone.click("send-prompt");
    let (second, text) = the_desktop_prompt(&desktop);
    assert_eq!(text, "Ship\nit");
    assert_ne!(second, first);
    desktop.show(conversation(
        Prompting::Open,
        vec![PromptReceipt {
            id: second,
            outcome: PromptOutcome::Refused(
                "The last prompt needs a look on the computer before the next one".into(),
            ),
        }],
        true,
    ));
    phone.wait("the refusal", |phase| {
        matches!(phase, Phase::Linked(linked) if linked.prompt.as_ref().is_some_and(|prompt| matches!(prompt.status, PromptStatus::Refused(_))))
    });
    assert!(phone.visible("prompt-note"));
    assert_eq!(phone.composer_text(id(1)).as_deref(), Some("Ship\nit"));
    phone.shot("21-prompt-refused");
    phone.click("prompt-note");
    assert!(!phone.visible("prompt-note"));

    phone.click("prompt-input");
    phone.press("escape");
    assert_eq!(
        phone.linked(|linked| linked.route().clone()),
        Route::Space(id(100)),
        "Back leaves the Conversation even from the composer"
    );
    let _ = std::fs::remove_dir_all(data);
}

fn a_phone_pairs_follows_spaces_and_threads_and_unpairs() {
    let ntfy = TestServer::start();
    let data = std::env::temp_dir().join(format!("drostje-phone-{}", Uuid::new_v4()));
    let mut phone = Phone::open(Storage::new(data.clone()));
    assert!(phone.phase(|phase| matches!(phase, Phase::Welcome { notice: None })));
    phone.shot("01-welcome");

    phone.click("scan-code");
    phone.shot("02-welcome-camera-unavailable");

    let session = PairingSession::start(
        Server::new(ntfy.address(), None).unwrap(),
        None,
        Duration::from_secs(60),
    );
    next(&session.events, |event| {
        (event == PairingEvent::Listening).then_some(())
    });
    links::deliver(
        PairingOffer::parse(&session.offer.to_link())
            .unwrap()
            .to_link(),
    );
    phone.wait("the check code", |phase| {
        matches!(phase, Phase::Pairing { check: Some(_), .. })
    });
    let desktop_check = next(&session.events, |event| match event {
        PairingEvent::Incoming { check, .. } => Some(check.to_string()),
        _ => None,
    });
    let phone_check = phone.phase(|phase| match phase {
        Phase::Pairing { check, .. } => check.clone(),
        _ => None,
    });
    assert_eq!(phone_check.as_deref(), Some(desktop_check.as_str()));
    assert!(phone.visible("pairing-check"));
    phone.shot("03-pairing-confirm");

    session.accept("Studio Mac".into());
    let link = next(&session.events, |event| match event {
        PairingEvent::Paired { link, .. } => Some(link),
        _ => None,
    });
    phone.wait("the pairing to finish", |phase| {
        matches!(phase, Phase::Linked(_))
    });
    phone.shot("04-loading");

    let desktop = DesktopLink::start(Server::new(ntfy.address(), None).unwrap(), link, None);
    desktop.show(PhoneView {
        desktop: "Studio Mac".into(),
        ..PhoneView::default()
    });
    phone.wait(
        "an empty desktop",
        |phase| matches!(phase, Phase::Linked(linked) if linked.synced),
    );
    phone.shot("05-no-spaces");

    desktop.show(desktop_view(None));
    phone.wait(
        "two Spaces",
        |phase| matches!(phase, Phase::Linked(linked) if linked.view.spaces.len() == 2),
    );
    phone.shot("06-spaces");

    phone.click(&format!("space-{}", id(100)));
    assert_eq!(
        phone.linked(|linked| linked.route().clone()),
        Route::Space(id(100))
    );
    phone.shot("07-threads");

    phone.click(&format!("thread-{}", id(1)));
    let watched = next(&desktop.events, |event| match event {
        LinkEvent::Watch(Some(thread)) => Some(thread),
        _ => None,
    });
    assert_eq!(watched, id(1));
    phone.shot("08-thread-loading");
    desktop.show(desktop_view(Some(transcript())));
    phone.wait(
        "the transcript",
        |phase| matches!(phase, Phase::Linked(linked) if linked.view.transcript.is_some()),
    );
    phone.shot("09-thread");

    desktop.show(themed(
        desktop_view(Some(transcript())),
        "Ink Light",
        "Cootje Dark",
    ));
    phone.wait(
        "the desktop theme",
        |phase| matches!(phase, Phase::Linked(linked) if linked.view.theme.is_some()),
    );
    let faces = phone.look(|look| {
        (
            look.light["name"].as_str().map(str::to_owned),
            look.dark["name"].as_str().map(str::to_owned),
        )
    });
    assert_eq!(
        faces,
        (Some("Ink Light".into()), Some("Cootje Dark".into()))
    );
    phone.shot("09-thread-desktop-theme");
    desktop.show(themed(
        desktop_view(Some(transcript())),
        "Delft Light",
        "Delft Dark",
    ));
    phone.wait("Delft again", |phase| {
        matches!(phase, Phase::Linked(linked) if linked.view.theme.as_ref().is_some_and(|theme| theme.dark["name"] == "Delft Dark"))
    });

    phone.press("escape");
    assert_eq!(
        phone.linked(|linked| linked.route().clone()),
        Route::Space(id(100))
    );
    next(&desktop.events, |event| {
        (event == LinkEvent::Watch(None)).then_some(())
    });
    phone.click(&format!("thread-{}", id(2)));
    phone.shot("10-thread-waiting");
    phone.press("escape");
    phone.press("escape");
    assert_eq!(phone.linked(|linked| linked.route().clone()), Route::Home);

    phone.click("open-link");
    phone.shot("11-this-phone");
    phone.click("unpair");
    assert!(phone.linked(|linked| linked.confirm_unpair));
    phone.shot("12-unpair-confirm");
    phone.press("escape");
    assert!(!phone.linked(|linked| linked.confirm_unpair));

    drop(desktop);
    phone.wait(
        "the desktop to go offline",
        |phase| matches!(phase, Phase::Linked(linked) if linked.desktop_online == Some(false)),
    );
    phone.press("escape");
    phone.shot("13-desktop-offline");

    phone.click("open-link");
    phone.click("unpair");
    phone.click("confirm-unpair");
    assert!(phone.phase(|phase| matches!(
        phase,
        Phase::Welcome {
            notice: Some(Notice::Info(_))
        }
    )));
    phone.shot("14-unpaired");
    phone.wait("storage to forget the desktop", |_| {
        Storage::new(data.clone()).pairing().is_none()
    });
    let _ = std::fs::remove_dir_all(data);
}

fn a_desktop_that_removes_the_phone_is_explained() {
    let ntfy = TestServer::start();
    let data = std::env::temp_dir().join(format!("drostje-phone-{}", Uuid::new_v4()));
    let mut phone = Phone::open(Storage::new(data.clone()));
    let session = PairingSession::start(
        Server::new(ntfy.address(), None).unwrap(),
        None,
        Duration::from_secs(60),
    );
    next(&session.events, |event| {
        (event == PairingEvent::Listening).then_some(())
    });
    links::deliver(session.offer.to_link());
    phone.wait("the pairing request", |phase| {
        matches!(phase, Phase::Pairing { check: Some(_), .. })
    });
    next(&session.events, |event| {
        matches!(event, PairingEvent::Incoming { .. }).then_some(())
    });
    session.accept("Studio Mac".into());
    let link = next(&session.events, |event| match event {
        PairingEvent::Paired { link, .. } => Some(link),
        _ => None,
    });
    phone.wait("pairing", |phase| matches!(phase, Phase::Linked(_)));
    let desktop = DesktopLink::start(Server::new(ntfy.address(), None).unwrap(), link, None);
    desktop.show(desktop_view(None));
    phone.wait(
        "sync",
        |phase| matches!(phase, Phase::Linked(linked) if linked.synced),
    );
    desktop.revoke();
    phone.wait("the removal", |phase| {
        matches!(
            phase,
            Phase::Welcome {
                notice: Some(Notice::Error(_))
            }
        )
    });
    phone.shot("15-removed-by-desktop");
    let _ = std::fs::remove_dir_all(data);
}

fn a_declined_phone_returns_to_welcome() {
    let ntfy = TestServer::start();
    let data = std::env::temp_dir().join(format!("drostje-phone-{}", Uuid::new_v4()));
    let mut phone = Phone::open(Storage::new(data.clone()));
    let session = PairingSession::start(
        Server::new(ntfy.address(), None).unwrap(),
        None,
        Duration::from_secs(60),
    );
    next(&session.events, |event| {
        (event == PairingEvent::Listening).then_some(())
    });
    links::deliver(session.offer.to_link());
    phone.wait("the pairing request", |phase| {
        matches!(phase, Phase::Pairing { check: Some(_), .. })
    });
    next(&session.events, |event| {
        matches!(event, PairingEvent::Incoming { .. }).then_some(())
    });
    session.decline();
    phone.wait("the refusal", |phase| {
        matches!(
            phase,
            Phase::Welcome {
                notice: Some(Notice::Error(_))
            }
        )
    });
    phone.shot("16-declined");
    let _ = std::fs::remove_dir_all(data);
}

fn main() {
    if cfg!(not(target_os = "macos")) {
        println!("screens: skipped; the offscreen phone renderer runs on macOS (Metal)");
        return;
    }
    for (name, scenario) in [
        (
            "a_phone_pairs_follows_spaces_and_threads_and_unpairs",
            a_phone_pairs_follows_spaces_and_threads_and_unpairs as fn(),
        ),
        (
            "a_desktop_that_removes_the_phone_is_explained",
            a_desktop_that_removes_the_phone_is_explained,
        ),
        (
            "a_declined_phone_returns_to_welcome",
            a_declined_phone_returns_to_welcome,
        ),
        (
            "a_phone_sends_prompts_and_learns_what_the_desktop_did_with_them",
            a_phone_sends_prompts_and_learns_what_the_desktop_did_with_them,
        ),
    ] {
        scenario();
        println!("screens: {name} ... ok");
    }
}
