mod appearance;
mod screens;
mod widgets;

use std::time::Duration;

use drostje_phone::{
    phone_side::{PairedDesktop, PhoneLink, PhonePairing},
    view::PhoneView,
};
use drostje_themes::Mode;
use gpui_kit::{base::TestSupportExt, component::ActiveTheme, *};

use crate::{
    device, links,
    look::Look,
    state::{Effect, Input, Notice, Phase, PhoneApp},
    storage::Storage,
    time,
};

const TICK: Duration = Duration::from_secs(20);

enum Persist {
    Pairing(Option<PairedDesktop>),
    View(PhoneView),
}

pub struct PhoneRoot {
    app: PhoneApp,
    persist: async_channel::Sender<Persist>,
    link: Option<PhoneLink>,
    link_generation: u64,
    pairing: Option<PhonePairing>,
    pairing_generation: u64,
    focus: FocusHandle,
    transcript: ScrollHandle,
    follow_transcript: bool,
    transcript_signature: (Option<uuid::Uuid>, usize, usize),
    now: i64,
    problem: Option<String>,
    system_dark: bool,
    look: Option<Look>,
    extras: appearance::Extras,
    _appearance: Subscription,
}

fn persist_loop(storage: Storage, jobs: async_channel::Receiver<Persist>) {
    while let Ok(job) = jobs.recv_blocking() {
        let result = match &job {
            Persist::Pairing(paired) => storage.save_pairing(paired.as_ref()),
            Persist::View(view) => storage.save_view(view),
        };
        if let Err(error) = result {
            log::error!("Saving phone state failed: {error:#}");
        }
    }
}

impl PhoneRoot {
    pub fn new(storage: Storage, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (persist, jobs) = async_channel::unbounded();
        let worker = storage.clone();
        std::thread::Builder::new()
            .name("phone-storage".into())
            .spawn(move || persist_loop(worker, jobs))
            .expect("spawning the phone storage thread");
        let (app, effects) = PhoneApp::start(storage.pairing(), storage.view());
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let appearance = cx.observe_window_appearance(window, |root, window, cx| {
            root.system_dark = appearance::is_dark(window.appearance());
            root.refresh_look(cx);
            cx.notify();
        });
        let mut root = Self {
            app,
            persist,
            link: None,
            link_generation: 0,
            pairing: None,
            pairing_generation: 0,
            focus,
            transcript: ScrollHandle::new(),
            follow_transcript: true,
            transcript_signature: (None, 0, 0),
            now: time::now(),
            problem: None,
            system_dark: appearance::is_dark(window.appearance()),
            look: None,
            extras: appearance::Extras::default(),
            _appearance: appearance,
        };
        root.refresh_look(cx);
        root.run(effects, cx);
        root.listen_for_links(cx);
        root.tick(cx);
        root
    }

    fn listen_for_links(&mut self, cx: &mut Context<Self>) {
        let signal = links::signal();
        cx.spawn(async move |this, cx| {
            loop {
                let delivered = this.update(cx, |root, cx| {
                    for link in links::take() {
                        root.input(Input::Link(link), cx);
                    }
                });
                if delivered.is_err() || signal.recv().await.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this
                    .update(cx, |root, cx| {
                        root.now = time::now();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn input(&mut self, input: Input, cx: &mut Context<Self>) {
        let effects = self.app.update(input);
        self.run(effects, cx);
        self.refresh_look(cx);
        cx.notify();
    }

    fn refresh_look(&mut self, cx: &mut Context<Self>) {
        let look = Look::resolve(self.app.theme(), self.system_dark);
        if self.look.as_ref() == Some(&look) {
            return;
        }
        appearance::apply(&look, cx);
        device::system_bars(look.mode == Mode::Dark);
        self.look = Some(look);
    }

    fn run(&mut self, effects: Vec<Effect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                Effect::StartPairing(offer) => {
                    self.pairing_generation += 1;
                    let generation = self.pairing_generation;
                    let pairing = PhonePairing::start(offer, device::device_name());
                    let events = pairing.events.clone();
                    self.pairing = Some(pairing);
                    cx.spawn(async move |this, cx| {
                        while let Ok(event) = events.recv().await {
                            let delivered = this.update(cx, |root, cx| {
                                if root.pairing_generation == generation {
                                    root.input(Input::Pairing(event), cx);
                                }
                            });
                            if delivered.is_err() {
                                break;
                            }
                        }
                    })
                    .detach();
                }
                Effect::CancelPairing => {
                    self.pairing_generation += 1;
                    self.pairing = None;
                }
                Effect::StartLink(paired, cached) => {
                    self.pairing = None;
                    self.link_generation += 1;
                    let generation = self.link_generation;
                    match PhoneLink::start(paired, cached) {
                        Ok(link) => {
                            let events = link.events.clone();
                            self.link = Some(link);
                            cx.spawn(async move |this, cx| {
                                while let Ok(event) = events.recv().await {
                                    let delivered = this.update(cx, |root, cx| {
                                        if root.link_generation == generation {
                                            root.input(Input::Phone(event), cx);
                                        }
                                    });
                                    if delivered.is_err() {
                                        break;
                                    }
                                }
                            })
                            .detach();
                        }
                        Err(error) => {
                            self.app.phase = Phase::Welcome {
                                notice: Some(Notice::Error(format!(
                                    "The saved pairing cannot be used: {error}. Pair again"
                                ))),
                            };
                            let _ = self.persist.try_send(Persist::Pairing(None));
                        }
                    }
                }
                Effect::StopLink => {
                    self.link_generation += 1;
                    self.link = None;
                }
                Effect::Watch(thread) => {
                    self.follow_transcript = true;
                    if let Some(link) = &self.link {
                        link.watch(thread);
                    }
                }
                Effect::Unpair => {
                    self.link_generation += 1;
                    if let Some(link) = self.link.take() {
                        link.unpair();
                    }
                }
                Effect::SavePairing(paired) => {
                    let _ = self.persist.try_send(Persist::Pairing(paired));
                }
                Effect::SaveView(view) => {
                    let _ = self.persist.try_send(Persist::View(view));
                }
                Effect::LeaveApp => device::leave(),
            }
        }
    }

    fn scan(&mut self, cx: &mut Context<Self>) {
        self.problem = device::start_scanner().err();
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.input(Input::Back, cx);
        }
    }
}

impl Render for PhoneRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let insets = device::insets();
        self.extras = appearance::Extras::of(self.look.as_ref(), cx);
        if !self.focus.is_focused(window) {
            self.focus.focus(window, cx);
        }
        let body = match &self.app.phase {
            Phase::Welcome { notice } => self.welcome(notice.clone(), cx).into_any_element(),
            Phase::Pairing { server, check } => self
                .pairing_screen(server.clone(), check.clone(), cx)
                .into_any_element(),
            Phase::Linked(_) => self.linked(window, cx),
        };
        div()
            .id("phone-root")
            .key_context("PhoneRoot")
            .test_support()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .pt(px(insets.top))
            .pb(px(insets.bottom))
            .pl(px(insets.left))
            .pr(px(insets.right))
            .children(appearance::grain_layer(self.extras.grain))
            .child(body)
    }
}

pub fn open_window(
    storage: Storage,
    options: WindowOptions,
    cx: &mut App,
) -> anyhow::Result<(AnyWindowHandle, Entity<PhoneRoot>)> {
    appearance::load_fonts(cx);
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| PhoneRoot::new(storage, window, cx))
    })
}

pub fn open(storage: Storage, options: WindowOptions, cx: &mut App) {
    gpui_kit::init(cx);
    if let Err(error) = open_window(storage, options, cx) {
        log::error!("Opening the DROSTJE. window failed: {error:#}");
    }
    cx.activate(true);
}

impl PhoneRoot {
    pub fn phase(&self) -> &Phase {
        &self.app.phase
    }

    pub fn look(&self) -> Option<&Look> {
        self.look.as_ref()
    }
}
