use std::sync::Arc;

use super::{SurfaceContent, fields::Choice};
use crate::app::{AppState, PhonePairingStatus};
use crate::phone::{link::Connection, qr::QrMatrix};
use crate::ui::commands::Command;
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable,
        alert::Alert,
        button::Button,
        h_flex,
        setting::{SettingField, SettingGroup, SettingItem},
        tag::Tag,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PairingView {
    pub status: PhonePairingStatus,
    pub problem: Option<String>,
    pub link: String,
    pub server: String,
}

impl PairingView {
    pub(super) fn read(state: &AppState) -> Option<Self> {
        state.phones.pairing.as_ref().map(|pairing| Self {
            status: pairing.status.clone(),
            problem: pairing.problem.clone(),
            link: pairing.link.clone(),
            server: pairing.server.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PhoneRow {
    id: Uuid,
    name: String,
    connection: Option<Connection>,
    following: Option<String>,
}

impl PhoneRow {
    pub(super) fn read(state: &AppState) -> Vec<Self> {
        state
            .paired_phones()
            .into_iter()
            .map(|phone| Self {
                id: phone.id,
                connection: state.phone_connection(phone.id).cloned(),
                following: state
                    .phone_watch(phone.id)
                    .map(|thread| thread.title.clone()),
                name: phone.name,
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PairingConfirm {
    pub label: &'static str,
    pub disabled: Option<&'static str>,
    pub loading: bool,
}

pub(super) fn pairing_confirm(status: Option<&PhonePairingStatus>) -> PairingConfirm {
    let waiting = PairingConfirm {
        label: "Allow phone",
        disabled: Some("Waiting for a phone to scan the code"),
        loading: false,
    };
    match status {
        Some(PhonePairingStatus::Incoming { .. }) => PairingConfirm {
            disabled: None,
            ..waiting
        },
        Some(PhonePairingStatus::Accepting { .. }) => PairingConfirm {
            disabled: Some("Pairing…"),
            loading: true,
            ..waiting
        },
        Some(PhonePairingStatus::Paired { .. }) => PairingConfirm {
            label: "Done",
            disabled: None,
            loading: false,
        },
        Some(PhonePairingStatus::Expired | PhonePairingStatus::Problem(_)) => PairingConfirm {
            label: "Show new code",
            disabled: None,
            loading: false,
        },
        Some(PhonePairingStatus::Connecting | PhonePairingStatus::Showing) | None => waiting,
    }
}

pub(super) fn connection_label(connection: Option<&Connection>) -> (&'static str, Option<String>) {
    match connection {
        Some(Connection::Connected) => ("Sharing", None),
        Some(Connection::Connecting) => ("Connecting…", None),
        Some(Connection::Problem(reason)) => ("Not sharing", Some(reason.clone())),
        None => (
            "Not sharing",
            Some("The phone server is not configured for this phone".into()),
        ),
    }
}

fn paint_qr(matrix: &QrMatrix, bounds: Bounds<Pixels>, window: &mut Window) {
    let modules = matrix.size();
    if modules == 0 {
        return;
    }
    let scale = window.scale_factor();
    let side = bounds.size.width.min(bounds.size.height);
    let device_cell = ((f32::from(side) * scale) / modules as f32).floor().max(1.);
    let cell = px(device_cell / scale);
    let drawn = cell * modules as f32;
    let origin = point(
        bounds.left() + (bounds.size.width - drawn) / 2.,
        bounds.top() + (bounds.size.height - drawn) / 2.,
    );
    for y in 0..modules {
        let mut x = 0;
        while x < modules {
            if !matrix.dark(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < modules && matrix.dark(x, y) {
                x += 1;
            }
            window.paint_quad(fill(
                Bounds::new(
                    point(origin.x + cell * start as f32, origin.y + cell * y as f32),
                    size(cell * (x - start) as f32, cell),
                ),
                black(),
            ));
        }
    }
}

fn qr_code(link: &str, cx: &App) -> AnyElement {
    let Some(matrix) = QrMatrix::encode(link) else {
        return Alert::error(
            "pair-phone-qr-error",
            "This server address makes the pairing code too long for a QR code. Use a shorter server address.",
        )
        .small()
        .into_any_element();
    };
    let matrix = Arc::new(matrix);
    div()
        .id("pair-phone-qr")
        .test_support()
        .flex_none()
        .size(rems(14.))
        .p(rems(0.9))
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(white())
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| paint_qr(&matrix, bounds, window),
            )
            .size_full(),
        )
        .into_any_element()
}

fn muted(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

impl SurfaceContent {
    pub(super) fn pair_phone_body(&self, body: Stateful<Div>, cx: &App) -> Stateful<Div> {
        let Some(pairing) = self.presentation.pairing.as_ref() else {
            return body.child(muted("Preparing a pairing code…", cx));
        };
        let body = match &pairing.status {
            PhonePairingStatus::Connecting | PhonePairingStatus::Showing => body.child(
                h_flex()
                    .gap_5()
                    .items_start()
                    .child(qr_code(&pairing.link, cx))
                    .child(
                        v_flex()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Scan this code with the DROSTJE. app"),
                            )
                            .child("1. Open DROSTJE. on your phone.")
                            .child("2. Tap Scan code.")
                            .child("3. Point the camera at this code.")
                            .child(
                                div()
                                    .id("pair-phone-status")
                                    .test_support()
                                    .child(muted(
                                        if matches!(pairing.status, PhonePairingStatus::Connecting) {
                                            format!("Connecting to {}…", pairing.server)
                                        } else {
                                            format!("Waiting for a phone · {}", pairing.server)
                                        },
                                        cx,
                                    )),
                            )
                            .child(muted(
                                "The code works for 5 minutes and pairs one phone.",
                                cx,
                            )),
                    ),
            ),
            PhonePairingStatus::Incoming { name, check }
            | PhonePairingStatus::Accepting { name, check } => body
                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("“{name}” wants to follow this desktop")))
                .child(
                    div()
                        .id("pair-phone-check")
                        .test_support()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_2xl()
                        .child(check.clone()),
                )
                .child("Allow it only when your phone shows the same code.")
                .child(muted(
                    "The phone sees your Spaces, Threads and the Conversation you open on it, and can send prompts to that Conversation. Questions from the agent are still answered here.",
                    cx,
                )),
            PhonePairingStatus::Paired { name } => body.child(
                div().id("pair-phone-paired").test_support().child(
                    Alert::success("pair-phone-paired-alert", format!("“{name}” is paired"))
                        .small()
                        .title("Phone paired"),
                ),
            )
            .child(muted("See and remove paired phones in Settings › Phones.", cx)),
            PhonePairingStatus::Expired => body
                .child(div().id("pair-phone-expired").test_support().child("The pairing code expired."))
                .child(muted("Show a new code to pair a phone.", cx)),
            PhonePairingStatus::Problem(reason) => body.child(
                div().id("pair-phone-unreachable").test_support().child(
                    Alert::error(
                        "pair-phone-unreachable-alert",
                        format!("Could not reach {}: {reason}", pairing.server),
                    )
                    .small(),
                ),
            )
            .child(muted("Check the server in Settings › Phones, then show a new code.", cx)),
        };
        body.when_some(pairing.problem.clone(), |body, problem| {
            body.child(Alert::warning("pair-phone-problem", problem).small())
        })
    }

    pub(super) fn phones_groups(
        &self,
        state: &AppState,
        readonly: bool,
        owner: &WeakEntity<SurfaceContent>,
    ) -> Vec<SettingGroup> {
        let server = self
            .group(
                "Phone server",
                &[crate::app::PHONE_SERVER_SETTING, crate::app::PHONE_TOKEN_ENV_SETTING],
                readonly,
                state,
                owner,
            )
            .description(
                "Phones follow this desktop through an ntfy server: SEINTJE, ntfy.sh or your own. Messages are end-to-end encrypted; the server sees random topic names and ciphertext only. The token is read from the named environment variable and travels to the phone inside the pairing code. Restart DROSTJE. after changing environment variables.",
            );
        let workspace = self.owner.clone();
        let pair_reason = Command::PairPhone.disabled_reason(state);
        let phones = state.paired_phones();
        let mut paired = SettingGroup::new()
            .title("Paired phones")
            .description(if phones.is_empty() {
                "No phones are paired. Choose Pair phone… and scan the code with the DROSTJE. app."
                    .to_owned()
            } else {
                "Each phone shows Spaces, Threads and the Conversation it opens, and can send prompts to that Conversation.".to_owned()
            })
            .item(
                SettingItem::new(
                    "Pair a phone",
                    SettingField::render(move |_, _, _| {
                        let workspace = workspace.clone();
                        Button::new("pair-phone")
                            .label(Command::PairPhone.label())
                            .disabled(pair_reason.is_some())
                            .when_some(pair_reason.clone(), |button, reason| button.tooltip(reason))
                            .on_click(move |_, window, cx| {
                                let _ = workspace.update(cx, |workspace, cx| {
                                    workspace.execute(Command::PairPhone, window, cx)
                                });
                            })
                    }),
                )
                .keywords(["phone", "pair", "android", "qr"]),
            );
        for phone in &phones {
            let (label, problem) = connection_label(state.phone_connection(phone.id));
            let following = state
                .phone_watch(phone.id)
                .map(|thread| format!(" · Following “{}”", thread.title))
                .unwrap_or_default();
            let paired_on = phone
                .paired_at
                .get(..10)
                .unwrap_or(&phone.paired_at)
                .to_owned();
            let id = phone.id;
            paired = paired.item(
                SettingItem::new(
                    phone.name.clone(),
                    SettingField::render(move |_, _, _| {
                        let tag = match label {
                            "Sharing" => Tag::success(),
                            "Connecting…" => Tag::secondary(),
                            _ => Tag::warning(),
                        };
                        div()
                            .id(SharedString::from(format!("phone-status-{id}")))
                            .test_support()
                            .child(tag.small().child(label))
                    }),
                )
                .description(match problem {
                    Some(problem) => format!("Paired {paired_on}{following} · {problem}"),
                    None => format!("Paired {paired_on}{following}"),
                })
                .keywords(["phone", "paired"]),
            );
        }
        if !phones.is_empty() {
            if let Some(select) = self.setting_item("phone", "Paired phones", readonly, owner) {
                paired = paired.item(select);
            }
            let remove_owner = self.owner.clone();
            paired = paired.item(
                SettingItem::new(
                    "Remove phone",
                    SettingField::render(move |_, _, cx| {
                        let owner = remove_owner.clone();
                        let reason = owner.upgrade().and_then(|workspace| {
                            Command::RemovePhone
                                .disabled_reason_in_workspace(workspace.read(cx), cx)
                        });
                        Button::new("remove-phone")
                            .label(Command::RemovePhone.label())
                            .disabled(reason.is_some())
                            .when_some(reason, |button, reason| button.tooltip(reason))
                            .on_click(move |_, window, cx| {
                                let _ = owner.update(cx, |workspace, cx| {
                                    workspace.execute(Command::RemovePhone, window, cx)
                                });
                            })
                    }),
                )
                .description("The phone stops receiving updates and shows that it was removed."),
            );
        }
        vec![server, paired]
    }

    pub(in crate::ui) fn phone_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(select) = self.select("phone") {
            let choices = phone_choices(self.state.read(cx));
            select.update(cx, |select, cx| {
                select.set_items(choices, window, cx);
                select.set_selected_value(&String::new(), window, cx);
            });
        }
        cx.notify();
    }
}

pub(super) fn phone_choices(state: &AppState) -> Vec<Choice> {
    std::iter::once(Choice::new("", "Choose a phone…"))
        .chain(
            state
                .paired_phones()
                .into_iter()
                .map(|phone| Choice::new(phone.id.to_string(), phone.name)),
        )
        .collect()
}

pub(in crate::ui) fn selected_phone(value: &str, state: &AppState) -> Option<Uuid> {
    let id = Uuid::parse_str(value).ok()?;
    state
        .paired_phones()
        .iter()
        .any(|phone| phone.id == id)
        .then_some(id)
}

#[cfg(all(test, feature = "runtime-qa"))]
#[path = "phones_ui_tests.rs"]
mod ui_tests;

#[cfg(test)]
mod tests {
    use super::{PairingConfirm, connection_label, pairing_confirm};
    use crate::app::PhonePairingStatus;
    use crate::phone::link::Connection;

    #[test]
    fn the_confirm_button_follows_the_pairing() {
        assert_eq!(
            pairing_confirm(Some(&PhonePairingStatus::Showing)).disabled,
            Some("Waiting for a phone to scan the code")
        );
        let incoming = PhonePairingStatus::Incoming {
            name: "Pixel".into(),
            check: "123 456".into(),
        };
        assert_eq!(
            pairing_confirm(Some(&incoming)),
            PairingConfirm {
                label: "Allow phone",
                disabled: None,
                loading: false
            }
        );
        let accepting = PhonePairingStatus::Accepting {
            name: "Pixel".into(),
            check: "123 456".into(),
        };
        assert!(pairing_confirm(Some(&accepting)).loading);
        assert_eq!(
            pairing_confirm(Some(&PhonePairingStatus::Expired)).label,
            "Show new code"
        );
        assert_eq!(
            pairing_confirm(Some(&PhonePairingStatus::Paired {
                name: "Pixel".into()
            }))
            .label,
            "Done"
        );
    }

    #[test]
    fn link_problems_explain_why_a_phone_is_not_sharing() {
        assert_eq!(
            connection_label(Some(&Connection::Problem("Cannot reach server x".into()))),
            ("Not sharing", Some("Cannot reach server x".into()))
        );
        assert_eq!(connection_label(Some(&Connection::Connected)).0, "Sharing");
    }
}
