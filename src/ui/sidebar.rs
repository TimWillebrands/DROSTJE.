use super::{
    commands::{Command, button},
    workspace::Workspace,
};
use crate::{
    app::AppState,
    model::{ProjectId, ThreadId},
};
use gpui_kit::component::Selectable;
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex, v_flex,
    },
    prelude::FluentBuilder,
    *,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Project(ProjectId),
    Thread(ThreadId),
}
pub fn rows(state: &AppState) -> Vec<Row> {
    let mut rows = Vec::new();
    for project in state
        .snapshot
        .projects
        .iter()
        .filter(|p| state.show_archived || !p.archived)
    {
        rows.push(Row::Project(project.id));
        if !state.collapsed.contains(&project.id) {
            rows.extend(
                state
                    .snapshot
                    .threads
                    .iter()
                    .filter(|t| t.project_id == project.id && (state.show_archived || !t.archived))
                    .map(|t| Row::Thread(t.id)),
            );
        }
    }
    rows
}
pub fn render(
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    cursor: Option<Row>,
    scroll: &ScrollHandle,
) -> AnyElement {
    let mut list = v_flex()
        .id("project-rows")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .gap_1();
    for row in rows(state) {
        let action_owner = owner.clone();
        let selected = cursor == Some(row);
        match row {
            Row::Project(id) => {
                let project = state
                    .snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == id)
                    .expect("project row");
                let collapsed = state.collapsed.contains(&id);
                let toggle_owner = owner.clone();
                list = list.child(
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .items_center()
                                .child(
                                    Button::new(SharedString::from(format!("collapse-{id}")))
                                        .ghost()
                                        .small()
                                        .label(if collapsed { ">" } else { "⌄" })
                                        .tooltip(if collapsed {
                                            "Expand project"
                                        } else {
                                            "Collapse project"
                                        })
                                        .on_click(move |_, _, cx| {
                                            let _ = toggle_owner.update(cx, |workspace, cx| {
                                                workspace.state.update(cx, |state, cx| {
                                                    if !state.collapsed.remove(&id) {
                                                        state.collapsed.insert(id);
                                                    }
                                                    cx.notify();
                                                });
                                            });
                                        }),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("project-{id}")))
                                        .ghost()
                                        .small()
                                        .flex_1()
                                        .label(format!(
                                            "{}{}",
                                            project.name,
                                            if project.archived { " · Archived" } else { "" }
                                        ))
                                        .selected(selected || state.selected_project == Some(id))
                                        .on_click(move |_, window, cx| {
                                            let _ = action_owner.update(cx, |workspace, cx| {
                                                workspace.select_row(row, false, window, cx)
                                            });
                                        }),
                                ),
                        )
                        .when_some(
                            state
                                .repositories
                                .get(&id)
                                .and_then(|r| r.as_ref().err())
                                .cloned(),
                            |view, error| {
                                view.child(
                                    div()
                                        .pl_3()
                                        .text_xs()
                                        .text_color(rgb(0xE6A0A0))
                                        .child(format!("Root unavailable: {error}")),
                                )
                            },
                        ),
                );
            }
            Row::Thread(id) => {
                let thread = state
                    .snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == id)
                    .expect("thread row");
                let badge = if thread.archived {
                    "Archived".into()
                } else if let Some(session) = state.conversations.get(&id) {
                    format!("{:?}", session.state)
                } else if thread.submission.is_some() {
                    "Outcome unresolved".into()
                } else if thread.session_ref.is_some() {
                    "Saved session".into()
                } else {
                    "Draft".into()
                };
                let model = thread
                    .model
                    .as_ref()
                    .map(|m| format!("{}/{}", m.provider, m.id))
                    .unwrap_or_else(|| "Harness default".into());
                list = list.child(
                    v_flex()
                        .pl_4()
                        .child(
                            Button::new(SharedString::from(format!("thread-{id}")))
                                .ghost()
                                .small()
                                .w_full()
                                .label(thread.title.clone())
                                .selected(selected || state.selected_thread == Some(id))
                                .on_click(move |_, window, cx| {
                                    let _ = action_owner.update(cx, |workspace, cx| {
                                        workspace.select_row(row, true, window, cx)
                                    });
                                }),
                        )
                        .child(
                            div()
                                .px_2()
                                .text_xs()
                                .text_color(rgb(0x8B95A5))
                                .child(format!("{model} · {badge}")),
                        ),
                );
            }
        }
    }
    if state.snapshot.projects.is_empty() {
        list = list.child(
            div()
                .p_3()
                .text_color(rgb(0x8B95A5))
                .child("No projects yet. Add an existing Git working tree to begin."),
        );
    }
    v_flex()
        .size_full()
        .gap_2()
        .p_2()
        .child(
            h_flex()
                .gap_1()
                .child(button("add-project", Command::AddProject, owner, state))
                .child(button(
                    "new-conversation",
                    Command::NewConversation,
                    owner,
                    state,
                )),
        )
        .child(
            button("history-toggle", Command::ShowArchived, owner, state)
                .label(if state.show_archived {
                    "Hide Archived History"
                } else {
                    "Show Archived History"
                })
                .selected(state.show_archived),
        )
        .when(state.registering, |view| {
            view.child("Inspecting working tree…")
        })
        .child(list)
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .child(button(
                    "rename-project",
                    Command::RenameProject,
                    owner,
                    state,
                ))
                .child(button(
                    "archive-project",
                    Command::ArchiveProject,
                    owner,
                    state,
                )),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(0x8B95A5))
                .child("j/k select · h/l fold · Return open"),
        )
        .into_any_element()
}
