use super::{
    PhoneRoot,
    widgets::{
        Tone, app_bar, banner, empty_state, icon, icon_button, loading, row, section, spinner,
        tone_color, wide_button,
    },
};
use crate::{
    state::{DesktopPresence, Input, Linked, Notice, Phase, Route},
    time,
};
use drostje_phone::{
    link::Connection,
    view::{
        ItemKind, PhoneView, SpaceView, ThreadKind, ThreadStatus, ThreadView, ToolState,
        Transcript, TranscriptItem,
    },
};
use gpui_kit::{
    assets::IconName,
    base::TestSupportExt,
    component::{ActiveTheme, text::TextView},
    prelude::FluentBuilder,
    *,
};

fn status_tone(status: ThreadStatus) -> Tone {
    match status {
        ThreadStatus::Waiting => Tone::Warn,
        ThreadStatus::Failed | ThreadStatus::Disconnected | ThreadStatus::Unresolved => Tone::Bad,
        ThreadStatus::Running | ThreadStatus::Starting | ThreadStatus::Stopping => Tone::Good,
        ThreadStatus::Idle => Tone::Neutral,
    }
}

fn thread_leading(thread: &ThreadView, cx: &App) -> AnyElement {
    let color = tone_color(status_tone(thread.status), cx);
    match (&thread.kind, thread.status) {
        (_, ThreadStatus::Running | ThreadStatus::Starting | ThreadStatus::Stopping) => {
            spinner(22., color).into_any_element()
        }
        (_, ThreadStatus::Waiting) => icon(IconName::CircleAlert, 24., color).into_any_element(),
        (_, ThreadStatus::Failed | ThreadStatus::Disconnected | ThreadStatus::Unresolved) => {
            icon(IconName::CircleX, 24., color).into_any_element()
        }
        (ThreadKind::Factory, ThreadStatus::Idle) => {
            icon(IconName::Wrench, 24., cx.theme().muted_foreground).into_any_element()
        }
        (ThreadKind::Conversation { .. }, ThreadStatus::Idle) => {
            icon(IconName::MessageSquare, 24., cx.theme().muted_foreground).into_any_element()
        }
    }
}

fn harness_label(thread: &ThreadView) -> &str {
    match &thread.kind {
        ThreadKind::Conversation { harness } => harness,
        ThreadKind::Factory => "Factory",
    }
}

fn thread_subtitle(thread: &ThreadView, now: i64) -> SharedString {
    let when = time::parse_utc(&thread.updated_at)
        .map(|then| time::ago(now, then))
        .unwrap_or_default();
    match thread.status {
        ThreadStatus::Idle if when.is_empty() => harness_label(thread).to_owned(),
        ThreadStatus::Idle => format!("{} · {when}", harness_label(thread)),
        status => format!("{} · {}", harness_label(thread), status.label()),
    }
    .into()
}

fn attention(view: &PhoneView, space: &SpaceView) -> usize {
    view.space_threads(space)
        .filter(|thread| thread.status.needs_attention() || thread.unseen)
        .count()
}

impl PhoneRoot {
    pub(super) fn welcome(&mut self, notice: Option<Notice>, cx: &mut Context<Self>) -> Div {
        let step = |number: &'static str, text: &'static str, cx: &App| {
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .flex_none()
                        .size(px(32.))
                        .rounded_full()
                        .bg(cx.theme().secondary)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(number),
                )
                .child(div().flex_1().min_w_0().pt_1().text_base().child(text))
        };
        let problem = self.problem.clone();
        div()
            .flex_1()
            .flex()
            .flex_col()
            .px_6()
            .pb_6()
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_8()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(icon(IconName::Smartphone, 40., cx.theme().foreground))
                            .child(
                                div()
                                    .text_3xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("DROSTJE."),
                            )
                            .child(
                                div()
                                    .text_base()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Follow the Spaces, Threads and Conversations of your computer from this phone."),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_5()
                            .child(step("1", "On your computer, open DROSTJE. and choose Pair phone…", cx))
                            .child(step("2", "Tap Scan code and point the camera at the code.", cx))
                            .child(step("3", "Check that both screens show the same number, then allow the phone on your computer.", cx)),
                    ),
            )
            .when_some(notice, |screen, notice| {
                let (name, text, tone) = match notice {
                    Notice::Error(text) => (IconName::CircleAlert, text, Tone::Bad),
                    Notice::Info(text) => (IconName::Info, text, Tone::Neutral),
                };
                screen.child(
                    div()
                        .id("welcome-notice")
                        .mb_4()
                        .on_click(cx.listener(|root, _, _, cx| root.input(Input::DismissNotice, cx)))
                        .child(banner(name, text, tone, cx).mx_0()),
                )
            })
            .when_some(problem, |screen, problem| {
                screen.child(div().mb_4().child(banner(IconName::CircleAlert, problem, Tone::Bad, cx).mx_0()))
            })
            .child(
                wide_button("scan-code", "Scan code", Some(IconName::ScanQrCode), true, false, cx)
                    .on_click(cx.listener(|root, _, _, cx| root.scan(cx)))
                    .test_support(),
            )
    }

    pub(super) fn pairing_screen(
        &mut self,
        server: String,
        check: Option<String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let back = icon_button("pairing-back", IconName::ChevronLeft, cx)
            .on_click(cx.listener(|root, _, _, cx| root.input(Input::CancelPairing, cx)));
        let body = match check {
            None => loading(format!("Contacting your computer through {server}…"), cx),
            Some(check) => div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .px_8()
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_center()
                        .child("Confirm on your computer"),
                )
                .child(
                    div()
                        .id("pairing-check")
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(rems(2.75))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(check.clone())
                        .test_support(),
                )
                .child(
                    div()
                        .text_base()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Choose Allow phone in DROSTJE. when it shows {check}."
                        )),
                )
                .child(
                    div()
                        .pt_4()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(spinner(16., cx.theme().muted_foreground))
                        .child("Waiting for your computer"),
                ),
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .child(app_bar(
                Some(back),
                "Pair with your computer",
                None,
                None,
                cx,
            ))
            .child(body)
            .child(
                div().px_6().pb_6().child(
                    wide_button("pairing-cancel", "Cancel", None, false, false, cx)
                        .on_click(
                            cx.listener(|root, _, _, cx| root.input(Input::CancelPairing, cx)),
                        )
                        .test_support(),
                ),
            )
    }

    pub(super) fn linked(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Phase::Linked(linked) = &self.app.phase else {
            return div().into_any_element();
        };
        let linked = (**linked).clone();
        let route = linked.route().clone();
        let screen = match &route {
            Route::Home => match linked.single_space() {
                Some(space) if linked.synced || !linked.view.spaces.is_empty() => {
                    let space = space.clone();
                    self.space_screen(&linked, &space, true, cx)
                }
                _ => self.home_screen(&linked, cx),
            },
            Route::Space(id) => match linked.view.space(*id).cloned() {
                Some(space) => self.space_screen(&linked, &space, false, cx),
                None => self.home_screen(&linked, cx),
            },
            Route::Thread(id) => match linked.view.threads.get(id).cloned() {
                Some(thread) => self.thread_screen(&linked, &thread, window, cx),
                None => self.home_screen(&linked, cx),
            },
            Route::Link => self.link_screen(&linked, cx),
        };
        div()
            .relative()
            .flex_1()
            .flex()
            .flex_col()
            .min_h_0()
            .child(screen)
            .when(linked.confirm_unpair, |root| {
                root.child(self.unpair_sheet(&linked, cx))
            })
            .into_any_element()
    }

    fn presence_line(&self, linked: &Linked) -> (SharedString, Tone) {
        let heard = linked
            .heard_at
            .map(|heard| time::ago(self.now, heard))
            .unwrap_or_else(|| "never".into());
        match (&linked.connection, linked.presence(self.now)) {
            (Connection::Problem(_), _) => ("Can't reach the server".into(), Tone::Bad),
            (_, DesktopPresence::Connecting) => ("Connecting…".into(), Tone::Neutral),
            (_, DesktopPresence::Online) => ("Online".into(), Tone::Good),
            (_, DesktopPresence::Offline) => {
                (format!("Offline · updated {heard}").into(), Tone::Warn)
            }
            (_, DesktopPresence::Quiet) => {
                (format!("No news · updated {heard}").into(), Tone::Warn)
            }
        }
    }

    fn status_banner(&self, linked: &Linked, cx: &App) -> Option<Div> {
        let desktop = linked.desktop_name().to_owned();
        match (&linked.connection, linked.presence(self.now)) {
            (Connection::Problem(reason), _) => Some(banner(
                IconName::WifiOff,
                format!("Can't reach the server. Retrying… ({reason})"),
                Tone::Bad,
                cx,
            )),
            (_, DesktopPresence::Offline) => Some(banner(
                IconName::Monitor,
                format!("{desktop} is not running DROSTJE. You see the last known state."),
                Tone::Warn,
                cx,
            )),
            (_, DesktopPresence::Quiet) => Some(banner(
                IconName::Monitor,
                format!("No updates from {desktop} for a while. It may be asleep."),
                Tone::Warn,
                cx,
            )),
            (_, DesktopPresence::Connecting | DesktopPresence::Online) => None,
        }
    }

    fn settings_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        icon_button("open-link", IconName::Settings, cx)
            .on_click(cx.listener(|root, _, _, cx| root.input(Input::Open(Route::Link), cx)))
    }

    fn back_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        icon_button("back", IconName::ChevronLeft, cx)
            .on_click(cx.listener(|root, _, _, cx| root.input(Input::Back, cx)))
    }

    fn home_screen(&mut self, linked: &Linked, cx: &mut Context<Self>) -> Div {
        let (subtitle, tone) = self.presence_line(linked);
        let view = linked.view.clone();
        let body = if view.spaces.is_empty() && !linked.synced {
            loading(format!("Loading {}…", linked.desktop_name()), cx).into_any_element()
        } else if view.spaces.is_empty() {
            empty_state(
                IconName::Inbox,
                "No Spaces yet",
                "Spaces you create in DROSTJE. on your computer appear here.",
                cx,
            )
            .into_any_element()
        } else {
            let mut list = div()
                .id("spaces")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pb_4();
            for space in &view.spaces {
                let count = view.space_threads(space).count();
                let needs = attention(&view, space);
                let id = space.id;
                let subtitle = match (count, needs) {
                    (0, _) => "No Threads".to_owned(),
                    (1, 0) => "1 Thread".to_owned(),
                    (count, 0) => format!("{count} Threads"),
                    (count, needs) => format!("{count} Threads · {needs} need you"),
                };
                let trailing = (needs > 0).then(|| {
                    div()
                        .size(px(10.))
                        .rounded_full()
                        .bg(cx.theme().warning)
                        .into_any_element()
                });
                list = list.child(
                    row(
                        SharedString::from(format!("space-{id}")),
                        icon(IconName::LayoutDashboard, 24., cx.theme().muted_foreground)
                            .into_any_element(),
                        space.name.clone(),
                        Some(subtitle.into()),
                        trailing,
                        cx,
                    )
                    .on_click(cx.listener(move |root, _, _, cx| {
                        root.input(Input::Open(Route::Space(id)), cx)
                    }))
                    .test_support(),
                );
            }
            list.into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(app_bar(
                None,
                linked.desktop_name().to_owned(),
                Some((subtitle, tone)),
                Some(self.settings_button(cx)),
                cx,
            ))
            .when_some(self.status_banner(linked, cx), |screen, banner| {
                screen.child(banner)
            })
            .child(body)
    }

    fn space_screen(
        &mut self,
        linked: &Linked,
        space: &SpaceView,
        root: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let view = linked.view.clone();
        let (subtitle, tone) = if root {
            self.presence_line(linked)
        } else {
            let (presence, tone) = self.presence_line(linked);
            (
                format!("{} · {presence}", linked.desktop_name()).into(),
                tone,
            )
        };
        let title = if root {
            linked.desktop_name().to_owned()
        } else {
            space.name.clone()
        };
        let threads = view.space_threads(space).count();
        let body = if threads == 0 {
            empty_state(
                IconName::MessageSquare,
                "No Threads here",
                "Start a Conversation in this Space on your computer to follow it here.",
                cx,
            )
            .into_any_element()
        } else {
            let mut list = div()
                .id("threads")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pb_4();
            for project in &space.projects {
                let listed: Vec<&ThreadView> = project
                    .threads
                    .iter()
                    .filter_map(|id| view.threads.get(id))
                    .collect();
                if listed.is_empty() {
                    continue;
                }
                list = list.child(section(project.name.clone(), cx));
                for thread in listed {
                    let id = thread.id;
                    let trailing = thread.unseen.then(|| {
                        div()
                            .size(px(10.))
                            .rounded_full()
                            .bg(cx.theme().primary)
                            .into_any_element()
                    });
                    list = list.child(
                        row(
                            SharedString::from(format!("thread-{id}")),
                            thread_leading(thread, cx),
                            thread.title.clone(),
                            Some(thread_subtitle(thread, self.now)),
                            trailing,
                            cx,
                        )
                        .on_click(cx.listener(move |root, _, _, cx| {
                            root.input(Input::Open(Route::Thread(id)), cx)
                        }))
                        .test_support(),
                    );
                }
            }
            list.into_any_element()
        };
        let leading = (!root).then(|| self.back_button(cx));
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(app_bar(
                leading,
                title,
                Some((subtitle, tone)),
                Some(self.settings_button(cx)),
                cx,
            ))
            .when_some(self.status_banner(linked, cx), |screen, banner| {
                screen.child(banner)
            })
            .child(body)
    }

    fn thread_screen(
        &mut self,
        linked: &Linked,
        thread: &ThreadView,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let project = linked
            .view
            .thread_space(thread.id)
            .and_then(|space| {
                space
                    .projects
                    .iter()
                    .find(|project| project.threads.contains(&thread.id))
            })
            .map(|project| project.name.clone())
            .unwrap_or_default();
        let subtitle: SharedString = if project.is_empty() {
            harness_label(thread).to_owned().into()
        } else {
            format!("{project} · {}", harness_label(thread)).into()
        };
        let transcript = linked
            .view
            .transcript
            .as_ref()
            .filter(|transcript| transcript.thread == thread.id)
            .cloned();
        let strip = match thread.status {
            ThreadStatus::Waiting => Some(banner(
                IconName::CircleAlert,
                "Waiting for your answer on the computer",
                Tone::Warn,
                cx,
            )),
            ThreadStatus::Failed | ThreadStatus::Disconnected | ThreadStatus::Unresolved => {
                Some(banner(
                    IconName::CircleX,
                    format!("{} — check it on the computer", thread.status.label()),
                    Tone::Bad,
                    cx,
                ))
            }
            ThreadStatus::Idle
            | ThreadStatus::Running
            | ThreadStatus::Starting
            | ThreadStatus::Stopping => None,
        };
        let body = match (&thread.kind, transcript) {
            (ThreadKind::Factory, _) => empty_state(
                IconName::Wrench,
                "Factory thread",
                format!(
                    "Status: {}. Run details stay on your computer.",
                    thread.status.label()
                ),
                cx,
            )
            .into_any_element(),
            (_, None) => loading("Loading the Conversation…", cx).into_any_element(),
            (_, Some(transcript)) if transcript.items.is_empty() => empty_state(
                IconName::MessageSquare,
                "Nothing to show yet",
                "Open this Conversation on your computer to follow it here.",
                cx,
            )
            .into_any_element(),
            (_, Some(transcript)) => self
                .transcript_list(&transcript, thread, cx)
                .into_any_element(),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(app_bar(
                Some(self.back_button(cx)),
                thread.title.clone(),
                Some((subtitle, Tone::Neutral)),
                None,
                cx,
            ))
            .when_some(self.status_banner(linked, cx), |screen, banner| {
                screen.child(banner)
            })
            .when_some(strip, |screen, strip| screen.child(strip))
            .child(body)
    }

    fn transcript_list(
        &mut self,
        transcript: &Transcript,
        thread: &ThreadView,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let signature = (
            Some(transcript.thread),
            transcript.items.len(),
            transcript
                .items
                .last()
                .map(|item| item.text.len())
                .unwrap_or_default(),
        );
        let max = self.transcript.max_offset().y;
        let offset = -self.transcript.offset().y;
        if signature.0 != self.transcript_signature.0 {
            self.follow_transcript = true;
        } else if max > px(0.) {
            self.follow_transcript = offset >= max - px(48.);
        }
        if signature != self.transcript_signature && self.follow_transcript {
            self.transcript.scroll_to_bottom();
        }
        self.transcript_signature = signature;
        let mut list = div()
            .id(SharedString::from(format!(
                "transcript-{}",
                transcript.thread
            )))
            .track_scroll(&self.transcript)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .px_4()
            .py_3();
        if transcript.earlier {
            list = list.child(
                div()
                    .text_xs()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child("Earlier messages are on your computer"),
            );
        }
        for item in &transcript.items {
            list = list.child(transcript_item(item, cx));
        }
        if thread.status.busy() {
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(spinner(16., cx.theme().muted_foreground))
                    .child(format!("{} is working…", harness_label(thread))),
            );
        }
        list
    }

    fn link_screen(&mut self, linked: &Linked, cx: &mut Context<Self>) -> Div {
        let (presence, tone) = self.presence_line(linked);
        let muted = cx.theme().muted_foreground;
        let field = move |label: &'static str, value: SharedString, color: Hsla| {
            div()
                .px_4()
                .py_3()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_sm().text_color(muted).child(label))
                .child(div().text_base().text_color(color).child(value))
        };
        let threads = linked.view.threads.len();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(app_bar(Some(self.back_button(cx)), "This phone", None, None, cx))
            .child(
                div()
                    .id("link-details")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(field("Computer", linked.desktop_name().to_owned().into(), cx.theme().foreground))
                    .child(field("Status", presence, tone_color(tone, cx)))
                    .child(field("Server", linked.paired.link.server.clone().into(), cx.theme().foreground))
                    .child(field("Following", format!("{} Spaces · {threads} Threads", linked.view.spaces.len()).into(), cx.theme().foreground))
                    .child(
                        div()
                            .px_4()
                            .py_3()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Messages are end-to-end encrypted between this phone and your computer. The server only passes them on. This phone can read along; it cannot send prompts or answer questions."),
                    ),
            )
            .child(
                div().px_6().pb_6().child(
                    wide_button("unpair", "Unpair this phone", Some(IconName::Unplug), false, true, cx)
                        .on_click(cx.listener(|root, _, _, cx| root.input(Input::AskUnpair, cx)))
                        .test_support(),
                ),
            )
    }

    fn unpair_sheet(&mut self, linked: &Linked, cx: &mut Context<Self>) -> Div {
        let desktop = linked.desktop_name().to_owned();
        div()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .justify_end()
            .child(
                div()
                    .id("unpair-scrim")
                    .absolute()
                    .inset_0()
                    .bg(cx.theme().overlay)
                    .on_click(cx.listener(|root, _, _, cx| root.input(Input::CancelUnpair, cx))),
            )
            .child(
                div()
                    .id("unpair-sheet")
                    .relative()
                    .bg(cx.theme().popover)
                    .rounded_t(px(28.))
                    .px_6()
                    .pt_6()
                    .pb_6()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(format!("Unpair from {desktop}?")))
                    .child(
                        div()
                            .text_base()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("This phone stops following {desktop}. To follow it again, pair with a new code.")),
                    )
                    .child(
                        wide_button("confirm-unpair", "Unpair", None, false, true, cx)
                            .on_click(cx.listener(|root, _, _, cx| root.input(Input::ConfirmUnpair, cx)))
                            .test_support(),
                    )
                    .child(
                        wide_button("cancel-unpair", "Cancel", None, false, false, cx)
                            .on_click(cx.listener(|root, _, _, cx| root.input(Input::CancelUnpair, cx)))
                            .test_support(),
                    ),
            )
    }
}

fn transcript_item(item: &TranscriptItem, cx: &App) -> AnyElement {
    let clipped = |element: Div| {
        element.when(item.clipped, |element| {
            element.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Shortened on the phone"),
            )
        })
    };
    match &item.kind {
        ItemKind::User => div()
            .flex()
            .flex_row()
            .justify_end()
            .child(clipped(
                div()
                    .max_w(relative(0.85))
                    .px_4()
                    .py_2()
                    .rounded(px(20.))
                    .bg(cx.theme().secondary)
                    .text_color(cx.theme().secondary_foreground)
                    .text_base()
                    .child(item.text.clone()),
            ))
            .into_any_element(),
        ItemKind::Assistant => clipped(
            div().w_full().text_base().child(
                TextView::markdown(
                    SharedString::from(format!("item-{}", item.id)),
                    item.text.clone(),
                )
                .selectable(true),
            ),
        )
        .into_any_element(),
        ItemKind::Thinking => div()
            .pl_3()
            .border_l_2()
            .border_color(cx.theme().border)
            .text_sm()
            .italic()
            .text_color(cx.theme().muted_foreground)
            .line_clamp(4)
            .child(item.text.clone())
            .into_any_element(),
        ItemKind::Tool { name, state } => {
            let (status, color) = match state {
                ToolState::Running => (
                    spinner(16., cx.theme().muted_foreground).into_any_element(),
                    cx.theme().muted_foreground,
                ),
                ToolState::Done => (
                    icon(IconName::CircleCheck, 16., cx.theme().success).into_any_element(),
                    cx.theme().success,
                ),
                ToolState::Failed => (
                    icon(IconName::CircleX, 16., cx.theme().danger).into_any_element(),
                    cx.theme().danger,
                ),
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .child(icon(IconName::Wrench, 16., color))
                .child(
                    div()
                        .flex_none()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(item.text.clone()),
                )
                .child(status)
                .into_any_element()
        }
        ItemKind::Notice => div()
            .w_full()
            .text_xs()
            .text_center()
            .text_color(cx.theme().muted_foreground)
            .child(item.text.clone())
            .into_any_element(),
        ItemKind::Error => banner(IconName::CircleX, item.text.clone(), Tone::Bad, cx)
            .mx_0()
            .into_any_element(),
    }
}
