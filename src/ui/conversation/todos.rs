use super::{
    disclosure::{disclosure, is_expanded, toggle},
    spinner::Spinner,
    tools::ToolCall,
};
use crate::{
    model::ThreadId,
    transcript::{
        ToolOutcome, text_field,
        todo::{TodoList, TodoStatus, describe_change},
    },
    ui::workspace::Workspace,
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{ActiveTheme, Icon, Sizable, h_flex, v_flex},
    prelude::FluentBuilder,
    *,
};

pub(super) fn todo_card(
    key: String,
    call: ToolCall<'_>,
    list: &TodoList,
    outcome: ToolOutcome,
    owner: &WeakEntity<Workspace>,
    thread: Option<ThreadId>,
    cx: &App,
) -> AnyElement {
    let ToolCall {
        name,
        intent,
        input,
    } = call;
    let change = describe_change(input).unwrap_or_else(|| name.to_owned());
    let open_by_default = matches!(text_field(input, "op"), Some("init" | "block"));
    let open = open_by_default != is_expanded(owner, &key, cx);
    let (done, total) = list.progress();
    let tally = list
        .overall
        .clone()
        .unwrap_or_else(|| format!("{done}/{total} done"));
    let header = h_flex()
        .id(SharedString::from(format!("{key}:header")))
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .on_click(toggle(owner, thread, key.clone()))
        .child(Icon::new(Lucide::ListTodo).xsmall())
        .child(
            div()
                .flex_shrink_0()
                .max_w(relative(0.5))
                .truncate()
                .child(intent.unwrap_or(name).to_owned()),
        )
        .child(div().flex_1().min_w_0().truncate().child(change))
        .child(div().flex_shrink_0().child(tally))
        .when(outcome == ToolOutcome::Running, |row| {
            row.child(Spinner::new().xsmall())
        })
        .child(disclosure(
            key.clone(),
            open,
            owner,
            thread,
            if open { "Hide tasks" } else { "Show tasks" },
        ));
    v_flex()
        .id(SharedString::from(format!("{key}:card")))
        .w_full()
        .min_w_0()
        .child(header)
        .when(open, |card| {
            card.child(
                div()
                    .mb_1()
                    .px_3()
                    .py_2()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .child(todo_checklist(list, cx)),
            )
        })
        .into_any_element()
}
pub(super) fn todo_checklist(list: &TodoList, cx: &App) -> Div {
    let mut column = v_flex().gap_1();
    for phase in &list.phases {
        let active = list.active_phase.as_deref() == Some(phase.name.as_str());
        let (done, total) = phase.progress();
        column = column.child(
            h_flex()
                .gap_2()
                .items_center()
                .pt_1()
                .text_xs()
                .text_color(if active {
                    cx.theme().foreground
                } else {
                    cx.theme().muted_foreground
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(phase.name.clone()),
                )
                .child(div().child(format!("{done}/{total}"))),
        );
        for task in &phase.tasks {
            let (icon, icon_color, text_color, dropped) = match &task.status {
                TodoStatus::Completed => (
                    Lucide::SquareCheck,
                    cx.theme().muted_foreground,
                    cx.theme().muted_foreground,
                    false,
                ),
                TodoStatus::InProgress => (
                    Lucide::SquareDot,
                    cx.theme().primary,
                    cx.theme().foreground,
                    false,
                ),
                TodoStatus::Pending => (
                    Lucide::Square,
                    cx.theme().muted_foreground,
                    cx.theme().foreground,
                    false,
                ),
                TodoStatus::Dropped => (
                    Lucide::SquareMinus,
                    cx.theme().muted_foreground,
                    cx.theme().muted_foreground,
                    true,
                ),
                TodoStatus::Blocked(_) => (
                    Lucide::Ban,
                    cx.theme().warning,
                    cx.theme().foreground,
                    false,
                ),
            };
            let mut body = v_flex().flex_1().min_w_0().child(
                div()
                    .text_color(text_color)
                    .when(dropped, |text| text.line_through())
                    .child(task.content.clone()),
            );
            if let TodoStatus::Blocked(reason) = &task.status {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(reason.clone()),
                );
            }
            column = column.child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .text_sm()
                    .child(
                        div()
                            .flex_shrink_0()
                            .pt_0p5()
                            .child(Icon::new(icon).xsmall().text_color(icon_color)),
                    )
                    .child(body),
            );
        }
    }
    column
}
