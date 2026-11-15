use crate::ui::{
    commands::{Command, button},
    workspace::{Workspace, projects::relative_age},
};
use crate::{
    app::{AppState, tracker_error_text},
    model::{PullRequestState, ThreadId},
    tracker::{ReviewThread, ReviewThreads},
};
use gpui_kit::{
    component::{ActiveTheme, Sizable, alert::Alert, h_flex, link::Link, tag::Tag, v_flex},
    prelude::FluentBuilder,
    *,
};

const COMMENT_LINES: usize = 12;

pub(in crate::ui) fn state_tag(state: &PullRequestState) -> Tag {
    match state {
        PullRequestState::Active => Tag::secondary().child("Active"),
        PullRequestState::Completed => Tag::success().child("Completed"),
        PullRequestState::Abandoned => Tag::secondary().outline().child("Abandoned"),
        PullRequestState::Unknown(raw) => Tag::secondary().outline().child(raw.clone()),
    }
}

pub(in crate::ui) fn render(
    state: &AppState,
    id: ThreadId,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let Some(pr) = state
        .snapshot
        .threads
        .iter()
        .find(|thread| thread.id == id)
        .and_then(|thread| thread.pull_request.as_ref())
    else {
        return message("No pull request is linked to this conversation.", cx);
    };
    let review = state.review_state(id);
    let loading = review.is_some_and(|review| review.loading);
    let mono = cx.theme().mono_font_family.clone();
    let header = v_flex()
        .flex_shrink_0()
        .gap_1()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .min_w_0()
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(mono.clone())
                        .text_sm()
                        .child(format!("!{}", pr.id)),
                )
                .child(state_tag(&pr.state).xsmall())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .child(pr.title.clone()),
                )
                .child(
                    button("open-pull-request", Command::OpenPullRequest, owner, state)
                        .label("Open")
                        .debug_selector(|| "open-pull-request".into()),
                ),
        )
        .when(!pr.work_items.is_empty(), |header| {
            header.child(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Work items")
                    .child(
                        div().font_family(mono.clone()).child(
                            pr.work_items
                                .iter()
                                .map(|id| format!("#{id}"))
                                .collect::<Vec<_>>()
                                .join(" "),
                        ),
                    ),
            )
        });
    let content = match review.and_then(|review| review.result.as_ref()) {
        None if loading => message("Reading review threads…", cx),
        None => message("Refresh to read this pull request's review threads.", cx),
        Some(Err(error)) => v_flex()
            .flex_1()
            .p_3()
            .debug_selector(|| "review-error".into())
            .child(Alert::error("review-error", tracker_error_text(error)).small())
            .into_any_element(),
        Some(Ok(threads)) if threads.threads.is_empty() => message("No review comments", cx),
        Some(Ok(threads)) => thread_list(threads, &pr.url, cx),
    };
    v_flex()
        .id("review-sidebar")
        .debug_selector(|| "review-sidebar".into())
        .flex_1()
        .min_h_0()
        .child(header)
        .child(content)
        .into_any_element()
}

fn message(text: &'static str, cx: &App) -> AnyElement {
    div()
        .flex_1()
        .p_3()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

fn thread_list(threads: &ReviewThreads, url: &str, cx: &App) -> AnyElement {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let (open, resolved): (Vec<_>, Vec<_>) = threads
        .threads
        .iter()
        .partition(|thread| thread.status.open());
    let section = |label: String| {
        div()
            .px_2()
            .pt_2()
            .pb_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label)
    };
    v_flex()
        .id("review-threads")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .pb_2()
        .child(section(match open.len() {
            0 => "No open threads".into(),
            1 => "1 open thread".into(),
            count => format!("{count} open threads"),
        }))
        .children(open.iter().map(|thread| thread_card(thread, url, now, cx)))
        .when(!resolved.is_empty(), |list| {
            list.child(section(format!("Resolved ({})", resolved.len())))
                .children(
                    resolved
                        .iter()
                        .map(|thread| div().opacity(0.7).child(thread_card(thread, url, now, cx))),
                )
        })
        .when(threads.truncated, |list| {
            list.child(section(
                "Only the first threads were read; open the pull request for the rest".into(),
            ))
        })
        .into_any_element()
}

fn thread_card(thread: &ReviewThread, url: &str, now: i64, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let location = thread.file.as_deref().map(|file| match thread.line {
        Some(line) => format!("{}:{line}", file.trim_start_matches('/')),
        None => file.trim_start_matches('/').to_owned(),
    });
    let link = format!("{url}?discussionId={}", thread.id);
    let hidden = thread
        .total_comments
        .saturating_sub(thread.comments.len() as u64);
    let truncated = thread.comments.iter().any(|comment| comment.truncated);
    let selector = format!("review-thread-{}", thread.id);
    v_flex()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector)
        .mx_2()
        .mb_2()
        .p_2()
        .gap_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .min_w_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .when_some(location.clone(), |view, _| {
                            view.font_family(theme.mono_font_family.clone())
                        })
                        .text_ellipsis_start()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(location.unwrap_or_else(|| "General".into())),
                )
                .when(thread.outdated, |row| {
                    row.child(Tag::secondary().outline().xsmall().child("Outdated"))
                })
                .child(
                    if thread.status.open() {
                        Tag::warning()
                    } else {
                        Tag::secondary()
                    }
                    .xsmall()
                    .child(thread.status.label().to_owned()),
                ),
        )
        .children(thread.comments.iter().map(|comment| {
            v_flex()
                .gap_0p5()
                .pt_1()
                .child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(comment.author.clone()),
                        )
                        .when_some(relative_age(&comment.published, now), |row, age| {
                            row.child(div().text_color(theme.muted_foreground).child(age))
                        }),
                )
                .child(div().text_sm().line_clamp(COMMENT_LINES).child(
                    if comment.body.is_empty() {
                        "(no text)".to_owned()
                    } else {
                        comment.body.clone()
                    },
                ))
        }))
        .when(hidden > 0 || truncated, |card| {
            card.child(
                h_flex().text_xs().child(
                    Link::new(SharedString::from(format!(
                        "review-thread-open-{}",
                        thread.id
                    )))
                    .href(link)
                    .child(if hidden > 0 {
                        format!("{hidden} more in Azure DevOps")
                    } else {
                        "Full text in Azure DevOps".into()
                    }),
                ),
            )
        })
        .into_any_element()
}
