use crate::app::AppState;
use crate::ui::{
    commands::{Command, button},
    theme,
    workspace::Workspace,
};
use gpui_kit::{
    component::{ActiveTheme, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};

/// The centre pane while no thread is open: name the next step and offer it.
const WORDMARK: &str = "DROSTJE.";

/// The brand in its display face, the full stop in the theme's signature color.
fn wordmark(cx: &App) -> impl IntoElement {
    let stop = WORDMARK.len() - 1;
    div()
        .debug_selector(|| "wordmark".into())
        .font_family(theme::DISPLAY_FONT)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(rems(2.25))
        .line_height(relative(1.))
        .child(StyledText::new(WORDMARK).with_highlights([(
            stop..WORDMARK.len(),
            HighlightStyle {
                color: Some(theme::signature(cx)),
                ..Default::default()
            },
        )]))
}

pub fn welcome(state: &AppState, owner: &WeakEntity<Workspace>, cx: &App) -> AnyElement {
    let has_project = state.project().is_some();
    let actions = h_flex()
        .gap_2()
        .flex_wrap()
        .justify_center()
        .when(has_project, |row| {
            row.child(
                button(
                    "empty-new-conversation",
                    Command::NewConversation,
                    owner,
                    state,
                )
                .outline()
                .debug_selector(|| "empty-new-conversation".into()),
            )
            .child(
                button("empty-new-factory", Command::NewFactoryThread, owner, state)
                    .outline()
                    .debug_selector(|| "empty-new-factory".into()),
            )
        })
        .when(!has_project, |row| {
            row.child(
                button("empty-add-project", Command::AddProject, owner, state)
                    .outline()
                    .debug_selector(|| "empty-add-project".into()),
            )
        });
    v_flex()
        .id("conversation-welcome")
        .size_full()
        .min_w_0()
        .min_h_0()
        .relative()
        .text_color(cx.theme().foreground)
        .children(crate::ui::theme::grain::layer(crate::ui::theme::grain(cx)))
        .items_center()
        .justify_center()
        .p_6()
        .child(
            v_flex()
                .w_full()
                .max_w(rems(30.))
                .items_center()
                .gap_3()
                .text_center()
                .child(wordmark(cx))
                .child(div().text_lg().child(if has_project {
                    "No thread selected"
                } else {
                    "Open a project"
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if has_project {
                            "Choose a thread from Projects, or start a new one."
                        } else {
                            "Add a Git working tree to start a Conversation or Factory thread."
                        }),
                )
                .child(actions),
        )
        .into_any_element()
}
