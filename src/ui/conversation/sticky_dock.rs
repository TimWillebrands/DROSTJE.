use super::{
    disclosure::{disclosure, is_expanded},
    inset,
    memo::Memo,
    spinner::Spinner,
    todos::todo_checklist,
};
use crate::ui::workspace::Workspace;
use crate::{
    app::ConversationSession,
    harness::ContentBlock,
    model::ThreadId,
    transcript::{
        jobs::{JobKind, JobNotice, JobStart, job_starts, settled_job_ids},
        todo::{TodoList, TodoStatus},
    },
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{ActiveTheme, Icon, Sizable, h_flex, v_flex},
    *,
};
use std::rc::Rc;
use uuid::Uuid;

fn call_result<'a>(session: &'a ConversationSession, call_id: &str) -> Option<&'a [ContentBlock]> {
    if let Some(tool) = session.tools.get(call_id)
        && tool.complete
    {
        return (!tool.error).then_some(&tool.result[..]);
    }
    let result = session.tool_result(call_id)?;
    (!result.error).then_some(&result.content[..])
}
fn first_text(blocks: &[ContentBlock]) -> Option<&str> {
    blocks.iter().find_map(|block| match block {
        ContentBlock::Text(text) => Some(text.as_str()),
        _ => None,
    })
}
fn current_todo(session: &ConversationSession) -> Option<TodoList> {
    session
        .tool_call_order()
        .iter()
        .rev()
        .filter(|(name, _)| name == "todo")
        .find_map(|(_, id)| TodoList::parse(first_text(call_result(session, id)?)?))
}
fn running_jobs(session: &ConversationSession) -> Vec<JobStart> {
    let mut jobs = Vec::new();
    let mut settled = std::collections::HashSet::new();
    for (name, id) in session.tool_call_order() {
        if !matches!(name.as_str(), "task" | "bash" | "wait" | "hub") {
            continue;
        }
        let Some(text) = call_result(session, id).and_then(first_text) else {
            continue;
        };
        if matches!(name.as_str(), "wait" | "hub") {
            settled.extend(settled_job_ids(name, text));
            continue;
        }
        if let Some(ContentBlock::ToolCall { input, .. }) = session.tool_call_block(id) {
            jobs.extend(job_starts(name, input, text));
        }
    }
    for notice in session.notices() {
        if let Some(notice) = first_text(&notice.content).and_then(JobNotice::parse) {
            settled.extend(notice.jobs.into_iter().map(|job| job.id));
        }
    }
    jobs.retain(|job| !settled.contains(&job.id));
    jobs
}
pub(super) struct DockContents {
    todo: Option<OpenTodo>,
    jobs: Vec<JobStart>,
    job_ids: SharedString,
}

struct OpenTodo {
    list: TodoList,
    tally: SharedString,
    current: SharedString,
}

impl DockContents {
    fn derive(session: &ConversationSession) -> Self {
        let todo = current_todo(session)
            .filter(|list| {
                let (done, total) = list.progress();
                done < total
            })
            .map(|list| {
                let (done, total) = list.progress();
                let current = list
                    .phases
                    .iter()
                    .flat_map(|phase| phase.tasks.iter())
                    .find(|task| task.status == TodoStatus::InProgress)
                    .map(|task| task.content.clone())
                    .unwrap_or_default();
                OpenTodo {
                    list,
                    tally: format!("Tasks {done}/{total}").into(),
                    current: current.into(),
                }
            });
        let jobs = running_jobs(session);
        let job_ids = jobs
            .iter()
            .map(|job| job.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
            .into();
        Self {
            todo,
            jobs,
            job_ids,
        }
    }
}

pub(super) type DockCache = Memo<(Uuid, u64), DockContents>;

pub(super) fn dock_contents(cache: &DockCache, session: &ConversationSession) -> Rc<DockContents> {
    cache.get((session.generation, session.transcript_revision), || {
        DockContents::derive(session)
    })
}

pub(super) fn sticky_dock(
    id: ThreadId,
    contents: &DockContents,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> Option<AnyElement> {
    let DockContents {
        todo,
        jobs,
        job_ids,
    } = contents;
    if todo.is_none() && jobs.is_empty() {
        return None;
    }
    let mut dock = v_flex()
        .w_full()
        .min_w_0()
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(cx.theme().border);
    if !jobs.is_empty() {
        let key = format!("dock-jobs:{id}");
        let open = is_expanded(owner, &key, cx);
        let summary = match jobs.len() {
            1 => "1 background job running".to_owned(),
            n => format!("{n} background jobs running"),
        };
        let mut section = v_flex().gap_1().child(
            h_flex()
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(disclosure(
                    key,
                    open,
                    owner,
                    Some(id),
                    if open {
                        "Hide background jobs"
                    } else {
                        "Show background jobs"
                    },
                ))
                .child(Icon::new(Lucide::Workflow).xsmall())
                .child(div().child(summary))
                .child(div().flex_1().min_w_0().truncate().child(job_ids.clone()))
                .child(Spinner::new().xsmall()),
        );
        if open {
            for job in jobs {
                let (icon, detail) = match &job.kind {
                    JobKind::Agent(agent) => (Lucide::Bot, format!("{agent} agent")),
                    JobKind::Shell => (Lucide::Terminal, job.label.clone()),
                };
                section = section.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .pl_6()
                        .text_sm()
                        .child(
                            Icon::new(icon)
                                .xsmall()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(div().child(job.id.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(cx.theme().muted_foreground)
                                .child(detail),
                        ),
                );
            }
        }
        dock = dock.child(section);
    }
    if let Some(OpenTodo {
        list,
        tally,
        current,
    }) = todo
    {
        let key = format!("dock-todo:{id}");
        let open = is_expanded(owner, &key, cx);
        let mut section = v_flex().gap_1().child(
            h_flex()
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(disclosure(
                    key,
                    open,
                    owner,
                    Some(id),
                    if open {
                        "Hide task list"
                    } else {
                        "Show task list"
                    },
                ))
                .child(Icon::new(Lucide::ListTodo).xsmall())
                .child(div().child(tally.clone()))
                .child(div().flex_1().min_w_0().truncate().child(current.clone())),
        );
        if open {
            section = section.child(div().pl_6().child(todo_checklist(list, cx)));
        }
        dock = dock.child(section);
    }
    Some(inset(dock))
}
