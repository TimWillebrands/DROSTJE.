use crate::ui::workspace::Workspace;
use crate::{
    harness::{UserAnswer, UserRequest, UserRequestKind},
    model::ThreadId,
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::Input,
        label::Label,
        v_flex,
    },
    *,
};

/// `harness` decides whether a permission request offers switching to its auto mode.
pub(super) fn request_card(
    id: ThreadId,
    harness: &str,
    request: &UserRequest,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let mut card = v_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .p_3()
        .border_1()
        .border_color(cx.theme().ring)
        .rounded(cx.theme().radius)
        .child(
            div()
                .id(SharedString::from(format!("request-{}-title", request.id)))
                .max_h(rems(4.))
                .overflow_y_scroll()
                .font_weight(FontWeight::SEMIBOLD)
                .child(request.title.clone()),
        );
    if !request.text.is_empty() {
        card = card.child(
            div()
                .id(SharedString::from(format!("request-{}-body", request.id)))
                .test_support()
                .max_h(rems(8.))
                .overflow_y_scroll()
                .text_sm()
                .child(request.text.clone()),
        );
    }
    let workspace = owner.upgrade();
    let form = workspace.as_ref().and_then(|workspace| {
        workspace
            .read(cx)
            .extension
            .as_ref()
            .filter(|form| form.thread == id && form.request.id == request.id)
    });
    let inline_answer = form.is_some_and(|form| form.inline_answer);
    let busy = form.is_some_and(|form| form.submitting);
    let mut choices = v_flex()
        .id(SharedString::from(format!(
            "request-{}-choices",
            request.id
        )))
        .test_support()
        .w_full()
        .min_h_0()
        .max_h(rems(14.))
        .overflow_y_scroll()
        .gap_1();
    let mut answers: Vec<(String, Option<String>, UserAnswer)> = Vec::new();
    if request.kind == UserRequestKind::Permission
        && crate::harness::auto_permission_mode(harness).is_some()
    {
        answers.push((
            "Yes and switch to auto".into(),
            Some("Approve all permission asks already open".into()),
            UserAnswer::PermissionAuto,
        ));
    }
    match request.kind {
        UserRequestKind::Confirm => {
            answers.push(("Confirm".into(), None, UserAnswer::Confirmed(true)));
            answers.push(("Decline".into(), None, UserAnswer::Confirmed(false)));
        }
        UserRequestKind::Select | UserRequestKind::Permission | UserRequestKind::Question => {
            for option in &request.options {
                answers.push((
                    option.value.clone(),
                    option.description.clone(),
                    UserAnswer::Value(option.value.clone()),
                ));
            }
        }
        UserRequestKind::Input | UserRequestKind::Editor => {
            let owner = owner.clone();
            let request = request.clone();
            choices = choices.child(
                Button::new(SharedString::from(format!("answer-{}", request.id)))
                    .small()
                    .outline()
                    .w_full()
                    .label("Answer…")
                    .disabled(inline_answer)
                    .on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.open_extension(id, request.clone(), window, cx)
                        });
                    }),
            );
        }
    }
    for (index, (label, description, answer)) in answers.into_iter().enumerate() {
        let owner = owner.clone();
        let request_id = request.id.clone();
        let mut label = Label::new(label).w_full().min_w_0().whitespace_normal();
        if let Some(description) = description {
            label = label.secondary(description);
        }
        choices = choices.child(
            Button::new(SharedString::from(format!(
                "request-{}-{index}",
                request.id
            )))
            .small()
            .outline()
            .w_full()
            .h_auto()
            .py_1p5()
            .child(label)
            .disabled(busy)
            .on_click(move |_, window, cx| {
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.back_from_custom_answer(id, &request_id, window, cx);
                    workspace.state.update(cx, |state, cx| {
                        state.respond(id, request_id.clone(), answer.clone());
                        cx.notify();
                    })
                });
            }),
        );
    }
    card = card.child(choices);
    if inline_answer
        && matches!(
            request.kind,
            UserRequestKind::Question | UserRequestKind::Input
        )
        && let Some(input) = owner
            .upgrade()
            .and_then(|workspace| workspace.read(cx).extension.as_ref()?.input.clone())
    {
        let submit = owner.clone();
        let back = owner.clone();
        let request_id = request.id.clone();
        card = card.child(
            v_flex()
                .id(SharedString::from(format!(
                    "request-{}-inline-editor",
                    request.id
                )))
                .key_context("DrostjeForm InlineRequestAnswer")
                .on_action(move |_: &crate::ui::commands::Dismiss, window, cx| {
                    cx.stop_propagation();
                    let _ = back.update(cx, |workspace, cx| {
                        workspace.back_from_custom_answer(id, &request_id, window, cx)
                    });
                })
                .on_action(move |_: &crate::ui::commands::Confirm, window, cx| {
                    cx.stop_propagation();
                    let _ =
                        submit.update(cx, |workspace, cx| workspace.confirm_extension(window, cx));
                })
                .gap_2()
                .child(
                    div()
                        .key_context("NativeInput")
                        .child(Input::new(&input).disabled(busy)),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new(SharedString::from(format!("request-{}-back", request.id)))
                                .small()
                                .ghost()
                                .label(if request.kind == UserRequestKind::Input {
                                    "Back to request"
                                } else {
                                    "Back to choices"
                                })
                                .disabled(busy)
                                .on_click({
                                    let owner = owner.clone();
                                    let request_id = request.id.clone();
                                    move |_, window, cx| {
                                        let _ = owner.update(cx, |workspace, cx| {
                                            workspace.back_from_custom_answer(
                                                id,
                                                &request_id,
                                                window,
                                                cx,
                                            )
                                        });
                                    }
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("request-{}-send", request.id)))
                                .small()
                                .primary()
                                .label("Send answer")
                                .disabled(busy)
                                .loading(busy)
                                .on_click({
                                    let owner = owner.clone();
                                    move |_, window, cx| {
                                        let _ = owner.update(cx, |workspace, cx| {
                                            workspace.confirm_extension(window, cx)
                                        });
                                    }
                                }),
                        ),
                ),
        );
    }
    if request.kind == UserRequestKind::Question {
        let custom = owner.clone();
        let request = request.clone();
        card = card.child(
            Button::new(SharedString::from(format!("request-{}-custom", request.id)))
                .small()
                .outline()
                .w_full()
                .label("Custom answer…")
                .disabled(inline_answer)
                .on_click(move |_, window, cx| {
                    let _ = custom.update(cx, |workspace, cx| {
                        workspace.open_custom_answer(id, request.clone(), window, cx)
                    });
                }),
        );
    }
    if let Some(error) = form.and_then(|form| form.error.as_ref()) {
        card = card.child(
            gpui_kit::component::alert::Alert::error(
                SharedString::from(format!("request-{}-error", request.id)),
                error.clone(),
            )
            .small(),
        );
    }
    let cancel = owner.clone();
    let request_id = request.id.clone();
    card.child(
        h_flex().justify_end().child(
            Button::new(SharedString::from(format!("request-{}-cancel", request.id)))
                .small()
                .ghost()
                .label("Cancel")
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    let _ = cancel.update(cx, |workspace, cx| {
                        if workspace.extension.as_ref().is_some_and(|form| {
                            form.inline_answer && form.thread == id && form.request.id == request_id
                        }) {
                            workspace.close_extension(true, window, cx);
                        } else {
                            workspace.state.update(cx, |state, cx| {
                                state.respond(
                                    id,
                                    request_id.clone(),
                                    UserAnswer::Cancelled { timed_out: false },
                                );
                                cx.notify();
                            })
                        }
                    });
                }),
        ),
    )
    .into_any_element()
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn request_cards_bound_large_content_and_offer_extra_actions(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use super::{UserRequest, UserRequestKind, Workspace, request_card, v_flex};
        use crate::harness::UserOption;
        use gpui_kit::{
            AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render, Styled,
            TestSupportExt, WeakEntity, Window, component::Root, px, test::TestWindowExt,
        };
        use serde_json::json;
        use uuid::Uuid;

        struct RequestHarness;
        impl Render for RequestHarness {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let owner = WeakEntity::<Workspace>::new_invalid();
                let request = UserRequest {
                    id: "kpi".into(),
                    kind: UserRequestKind::Question,
                    title: "KPI cards".into(),
                    options: vec![
                        UserOption {
                            value: "Possible".into(),
                            description: Some(
                                "Open services have status OPEN, filled services have status FILLED, \
                                 conflict or sick means BLOCKED plus CANCELLED, and proposals remain zero. "
                                    .repeat(40),
                            ),
                        },
                        UserOption {
                            value: "None".into(),
                            description: None,
                        },
                    ],
                    text: "Review this plan before choosing.\n".repeat(200),
                    placeholder: None,
                    deadline: None,
                };
                let permission = UserRequest {
                    id: "permission".into(),
                    kind: UserRequestKind::Permission,
                    title: "Allow Bash?".into(),
                    options: vec![UserOption {
                        value: "Allow".into(),
                        description: Some("This time".into()),
                    }],
                    text: "Bash: cargo check".into(),
                    placeholder: None,
                    deadline: None,
                };
                v_flex()
                    .id("request-harness")
                    .test_support()
                    .w(px(420.))
                    .child(request_card(
                        Uuid::from_u128(1),
                        "claude-code",
                        &request,
                        &owner,
                        cx,
                    ))
                    .child(request_card(
                        Uuid::from_u128(1),
                        "claude-code",
                        &permission,
                        &owner,
                        cx,
                    ))
                    .child(request_card(
                        Uuid::from_u128(1),
                        "opencode",
                        &UserRequest {
                            id: "permission-manual".into(),
                            ..permission.clone()
                        },
                        &owner,
                        cx,
                    ))
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let window = cx.add_window(|window, cx| {
            let content = cx.new(|_| RequestHarness);
            Root::new(content, window, cx)
        });
        cx.update_window(window.into(), |_, window, cx| {
            for mode in ["light", "dark"] {
                let settings = std::collections::BTreeMap::from([(
                    "theme_mode".to_owned(),
                    json!(mode),
                )]);
                crate::ui::theme::initialize(Some(&settings), None, cx);
                window.render_frame(cx);
                let card = window.find("request-harness").bounds();
                let long_choice = window.find("request-kpi-0").bounds();
                let short_choice = window.find("request-kpi-1").bounds();
                let custom = window.find("request-kpi-custom").bounds();
                let body = window.find("request-kpi-body").bounds();
                let choices = window.find("request-kpi-choices").bounds();
                let auto = window.find("request-permission-0").bounds();
                let rem = window.rem_size();
                assert!(
                    body.size.height <= rem * 8.,
                    "{mode} request context must scroll instead of growing beyond eight rem"
                );
                assert!(
                    choices.size.height <= rem * 14.,
                    "{mode} choices must scroll instead of filling the conversation"
                );
                assert!(
                    custom.top() >= card.top() && custom.bottom() <= card.bottom(),
                    "{mode} custom answer must remain inside the bounded request card"
                );
                assert!(
                    long_choice.size.height > short_choice.size.height,
                    "{mode} theme must wrap long option text instead of clipping it to the short button height"
                );
                assert!(
                    auto.size.height > px(0.),
                    "{mode} permission card must expose auto approval"
                );
                assert!(
                    window.try_find("request-permission-1").is_some()
                        && window.try_find("request-permission-manual-0").is_some()
                        && window.try_find("request-permission-manual-1").is_none(),
                    "{mode} a harness without an auto mode offers only its own options"
                );
                assert!(
                    long_choice.left() >= card.left() && long_choice.right() <= card.right(),
                    "{mode} choice must stay inside the request card: card={card:?}, choice={long_choice:?}"
                );
            }
        })
        .unwrap();
    }
}
