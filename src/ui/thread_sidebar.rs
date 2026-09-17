//! A thread's sidebar: the Changes and Output tabs beside a Conversation, the Output
//! tab beside a Factory thread. Open state and tab live in `AppState`.

use super::{
    commands::{Command, button},
    factory,
    workspace::Workspace,
};
use crate::{
    app::{AppState, SidebarTab},
    model::ThreadId,
};
use gpui_kit::{
    component::{
        ActiveTheme, IconName, Sizable,
        alert::Alert,
        h_flex,
        spinner::Spinner,
        tab::{Tab, TabBar},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

/// Tabs in display order; a Factory thread has no working-tree Changes.
fn tabs(conversation: bool) -> &'static [SidebarTab] {
    if conversation {
        &[SidebarTab::Changes, SidebarTab::Output]
    } else {
        &[SidebarTab::Output]
    }
}

fn label(tab: SidebarTab) -> &'static str {
    match tab {
        SidebarTab::Changes => "Changes",
        SidebarTab::Output => "Output",
    }
}

/// The command that shows or hides `tab`; the collapse control dispatches it too.
fn toggle_command(tab: SidebarTab) -> Command {
    match tab {
        SidebarTab::Changes => Command::ToggleConversationDiffs,
        SidebarTab::Output => Command::ToggleOutput,
    }
}

pub(super) fn render(
    state: &AppState,
    id: ThreadId,
    conversation: bool,
    owner: &WeakEntity<Workspace>,
    output_scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    let open = state.thread_sidebar_open(id);
    let active = state.thread_sidebar_tab(id);
    let toggle = button(
        "toggle-thread-sidebar",
        toggle_command(active),
        owner,
        state,
    )
    .label("")
    .icon(if open {
        IconName::ChevronRight
    } else {
        IconName::ChevronLeft
    })
    .tooltip(if open { "Hide sidebar" } else { "Show sidebar" })
    .debug_selector(|| "toggle-thread-sidebar".into());
    if !open {
        return v_flex()
            .w(rems(2.5))
            .h_full()
            .flex_shrink_0()
            .items_center()
            .border_l_1()
            .border_color(cx.theme().border)
            .pt_2()
            .child(toggle)
            .into_any_element();
    }
    let tabs = tabs(conversation);
    let selected = tabs.iter().position(|tab| *tab == active).unwrap_or(0);
    let tab_owner = owner.clone();
    let loading = active == SidebarTab::Changes
        && state.conversation_diff(id).is_none_or(|diff| diff.loading);
    let header = TabBar::new("thread-sidebar-tabs")
        .selected_index(selected)
        .children(tabs.iter().map(|tab| Tab::new().label(label(*tab))))
        .on_click(move |ix, window, cx| {
            let Some(tab) = tabs.get(*ix).copied() else {
                return;
            };
            let _ = tab_owner.update(cx, |workspace, cx| {
                workspace.show_sidebar_tab(id, tab, window, cx)
            });
        })
        .suffix(
            h_flex()
                .h_full()
                .items_center()
                .gap_1()
                .px_1()
                .when(loading, |row| row.child(Spinner::new().xsmall()))
                .when(active == SidebarTab::Changes, |row| {
                    row.child(
                        button(
                            "refresh-conversation-diff",
                            Command::RefreshConversationDiff,
                            owner,
                            state,
                        )
                        .label("Refresh")
                        .debug_selector(|| "refresh-conversation-diff".into()),
                    )
                })
                .child(toggle),
        );
    let body = match active {
        SidebarTab::Changes => changes(state, id, cx),
        SidebarTab::Output => div()
            .flex_1()
            .min_h_0()
            .child(factory::output(state, id, owner, output_scroll, cx))
            .into_any_element(),
    };
    v_flex()
        .id("thread-sidebar")
        .debug_selector(|| "thread-sidebar".into())
        .w(rems(22.))
        .max_w(relative(0.4))
        .h_full()
        .min_w_0()
        .min_h_0()
        .flex_shrink_0()
        .border_l_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().sidebar)
        .child(header)
        .child(body)
        .into_any_element()
}

fn changes(state: &AppState, id: ThreadId, cx: &App) -> AnyElement {
    let diff = state.conversation_diff(id);
    let loading = diff.is_none_or(|diff| diff.loading);
    let result = diff.and_then(|diff| diff.result.as_ref());
    let summary = match result {
        Some(Ok(diff)) if diff.changed_files > 0 => format!(
            "{} files · +{} −{}",
            diff.changed_files, diff.additions, diff.deletions
        ),
        Some(Ok(_)) => "Working tree clean".to_owned(),
        Some(Err(_)) => "Changes unavailable".to_owned(),
        None if loading => "Loading changes…".to_owned(),
        None => "Changes not loaded".to_owned(),
    };
    let content = match result {
        Some(Ok(diff)) if diff.patch.is_empty() => div()
            .flex_1()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No uncommitted changes")
            .into_any_element(),
        Some(Ok(diff)) => div()
            .id("conversation-diff-patch")
            .debug_selector(|| "conversation-diff-patch".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .whitespace_normal()
            .child(diff.patch.clone())
            .into_any_element(),
        Some(Err(error)) => v_flex()
            .flex_1()
            .gap_2()
            .p_3()
            .text_sm()
            .child(
                Alert::error(
                    "conversation-diff-error",
                    format!("Changes could not be read: {error}"),
                )
                .small(),
            )
            .child("Retry after restoring access to the working tree.")
            .into_any_element(),
        None => div()
            .flex_1()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(if loading {
                "Reading working tree changes…"
            } else {
                "Open or refresh this conversation to read changes."
            })
            .into_any_element(),
    };
    v_flex()
        .id("conversation-diff-sidebar")
        .debug_selector(|| "conversation-diff-sidebar".into())
        .flex_1()
        .min_h_0()
        .child(
            div()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(summary),
        )
        .child(content)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{SidebarTab, tabs};

    #[test]
    fn factory_threads_only_offer_output() {
        assert_eq!(tabs(true), &[SidebarTab::Changes, SidebarTab::Output]);
        assert_eq!(tabs(false), &[SidebarTab::Output]);
    }
}
