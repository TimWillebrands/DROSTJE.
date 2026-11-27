mod appearance;
mod screens;
mod transcript_scroll;
mod widgets;

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use drostje_phone::{
    phone_side::{PairedDesktop, PhoneEvent, PhoneLink, PhonePairing, PromptPosted},
    view::PhoneView,
};
use drostje_themes::Mode;
use gpui_kit::{
    base::TestSupportExt,
    component::{
        ActiveTheme,
        input::{InputEvent, TextareaState},
    },
    *,
};
use uuid::Uuid;

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

struct PromptComposer {
    input: Entity<TextareaState>,
    empty: bool,
}

pub struct PhoneRoot {
    app: PhoneApp,
    persist: async_channel::Sender<Persist>,
    link: Option<PhoneLink>,
    link_generation: u64,
    pairing: Option<PhonePairing>,
    pairing_generation: u64,
    focus: FocusHandle,
    transcript: transcript_scroll::TranscriptScroll,
    follow_transcript: bool,
    transcript_signature: (Option<uuid::Uuid>, usize, usize),
    now: i64,
    problem: Option<String>,
    system_dark: bool,
    look: Option<Look>,
    extras: appearance::Extras,
    composers: HashMap<Uuid, PromptComposer>,
    expanded_runs: HashSet<String>,
    sent: Vec<(Uuid, String)>,
    _composer_events: Vec<Subscription>,
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
            transcript: transcript_scroll::TranscriptScroll::default(),
            follow_transcript: true,
            transcript_signature: (None, 0, 0),
            now: time::now(),
            problem: None,
            system_dark: appearance::is_dark(window.appearance()),
            look: None,
            extras: appearance::Extras::default(),
            composers: HashMap::new(),
            expanded_runs: HashSet::new(),
            sent: Vec::new(),
            _composer_events: Vec::new(),
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
                    self.transcript.reset_gesture();
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
                Effect::Prompt { id, thread, text } => {
                    let queued = match &self.link {
                        Some(link) => link.prompt(id, thread, text),
                        None => Err("This phone is not connected to the server".into()),
                    };
                    if let Err(reason) = queued {
                        let posted = PromptPosted::NotSent(reason);
                        let follow = self
                            .app
                            .update(Input::Phone(PhoneEvent::Prompt { id, posted }));
                        self.run(follow, cx);
                    }
                }
                Effect::ClearComposer { thread, text } => self.sent.push((thread, text)),
                Effect::LeaveApp => device::leave(),
            }
        }
    }

    fn composer(
        &mut self,
        thread: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TextareaState>, bool) {
        if let Some(composer) = self.composers.get_mut(&thread) {
            composer.empty = composer_is_empty(composer.input.read(cx));
            return (composer.input.clone(), composer.empty);
        }
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .autocorrect(true)
                .placeholder("Send a prompt…")
        });
        let events = cx.subscribe_in(
            &composer,
            window,
            move |root, composer, event: &InputEvent, _, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => root.send_prompt(thread, cx),
                InputEvent::Change => {
                    let empty = composer_is_empty(composer.read(cx));
                    if let Some(composer) = root.composers.get_mut(&thread)
                        && composer.empty != empty
                    {
                        composer.empty = empty;
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { shift: true, .. }
                | InputEvent::Focus
                | InputEvent::Blur => {}
            },
        );
        self._composer_events.push(events);
        self.composers.insert(
            thread,
            PromptComposer {
                input: composer.clone(),
                empty: true,
            },
        );
        (composer, true)
    }

    fn send_prompt(&mut self, thread: Uuid, cx: &mut Context<Self>) {
        let Some(composer) = self.composers.get(&thread) else {
            return;
        };
        let text = composer.input.read(cx).value().to_string();
        self.input(
            Input::Prompt {
                id: Uuid::new_v4(),
                thread,
                text,
                now: time::now(),
            },
            cx,
        );
    }

    fn clear_sent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (thread, text) in std::mem::take(&mut self.sent) {
            if let Some(composer) = self.composers.get(&thread)
                && composer.input.read(cx).value().trim() == text
            {
                composer
                    .input
                    .update(cx, |input, cx| input.set_value("", window, cx));
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

fn composer_is_empty(composer: &TextareaState) -> bool {
    composer.text().chars().all(char::is_whitespace)
}

impl Render for PhoneRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let insets = device::insets();
        self.extras = appearance::Extras::of(self.look.as_ref(), cx);
        self.clear_sent(window, cx);
        if !self.focus.contains_focused(window, cx) {
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
    let drawn = storage.clone();
    match open_window(storage, options, cx) {
        Ok((handle, _)) => {
            let watched = handle.update(cx, |_, window, _| {
                window.on_next_frame(move |window, _| {
                    window.on_next_frame(move |_, _| device::frame_drawn(&drawn));
                });
            });
            if let Err(error) = watched {
                log::warn!("Watching the first frame failed: {error:#}");
            }
        }
        Err(error) => log::error!("Opening the DROSTJE. window failed: {error:#}"),
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

    pub fn composer_text(&self, thread: Uuid, cx: &App) -> Option<String> {
        self.composers
            .get(&thread)
            .map(|composer| composer.input.read(cx).value().to_string())
    }
}
