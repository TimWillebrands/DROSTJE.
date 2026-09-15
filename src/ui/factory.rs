use super::{
    commands::{Command, button},
    workspace::{Region, Workspace},
};
use crate::app::AppState;
use gpui_kit::{
    component::{
        Selectable, Sizable,
        button::{Button, ButtonVariants},
        h_flex, v_flex,
    },
    *,
};
use std::path::{Path, PathBuf};

pub fn path_actions(label: &'static str, path: &Path) -> AnyElement {
    let reveal = path.to_path_buf();
    let copy = path.to_string_lossy().into_owned();
    let reveal_id = SharedString::from(format!("reveal-{label}-{copy}"));
    let copy_id = SharedString::from(format!("copy-{label}-{copy}"));
    v_flex()
        .gap_1()
        .child(div().font_family("Menlo").text_xs().child(copy.clone()))
        .child(
            h_flex()
                .gap_1()
                .child(
                    Button::new(reveal_id)
                        .small()
                        .ghost()
                        .label("Reveal in Finder")
                        .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                )
                .child(
                    Button::new(copy_id)
                        .small()
                        .ghost()
                        .label("Copy Path")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        }),
                ),
        )
        .into_any_element()
}

fn read_button(
    label: &'static str,
    path: PathBuf,
    before: Option<u64>,
    owner: &WeakEntity<Workspace>,
) -> Button {
    let owner = owner.clone();
    Button::new(SharedString::from(format!(
        "read-{label}-{}-{before:?}",
        path.display()
    )))
    .small()
    .ghost()
    .label(label)
    .on_click(move |_, _, cx| {
        let _ = owner.update(cx, |workspace, cx| {
            workspace.state.update(cx, |state, cx| {
                state.load_factory_text(path.clone(), before, cx)
            })
        });
    })
}

pub fn render(
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    scroll: &ScrollHandle,
) -> AnyElement {
    let mut body = v_flex()
        .id("factory-history")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .gap_3();
    let launches: Vec<_> = state
        .snapshot
        .launches
        .iter()
        .filter(|launch| state.selected_project == Some(launch.project_id))
        .collect();
    if launches.is_empty() {
        body = body.child(div().text_color(rgb(0x8B95A5)).child(
            "No Factory launches recorded for this project. Nothing is launched automatically.",
        ));
    }
    for launch in launches {
        let owner = owner.clone();
        let id = launch.id;
        let source = launch
            .descriptor
            .get("source")
            .map(ToString::to_string)
            .unwrap_or_else(|| "Source not recorded".into());
        let projection = state.factory_runs.get(&id);
        let title = projection
            .and_then(|snapshot| snapshot.title.clone())
            .or_else(|| {
                launch
                    .descriptor
                    .get("packet")
                    .and_then(|packet| packet.get("title"))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| source.clone());
        let harness = launch
            .descriptor
            .get("harness")
            .and_then(|value| value.as_str())
            .unwrap_or("Not recorded");
        let model = launch
            .descriptor
            .get("model")
            .and_then(|value| value.as_str())
            .unwrap_or("Harness default");
        let status = projection
            .map(|snapshot| snapshot.status_text())
            .unwrap_or_else(|| {
                launch
                    .launch_error
                    .clone()
                    .unwrap_or_else(|| "Launch outcome unknown; replaying durable history".into())
            });
        body = body.child(
            v_flex()
                .gap_1()
                .pb_3()
                .border_b_1()
                .border_color(rgb(0x343A46))
                .child(
                    Button::new(SharedString::from(format!("factory-launch-{id}")))
                        .small()
                        .ghost()
                        .w_full()
                        .label(title)
                        .selected(state.selected_launch == Some(id))
                        .on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |workspace, cx| {
                                workspace
                                    .state
                                    .update(cx, |state, cx| state.select_factory_launch(id, cx));
                                workspace.focus_region(Region::Output, window, cx);
                            });
                        }),
                )
                .child(div().text_xs().child(source))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x8B95A5))
                        .child(format!("{harness} · {model}")),
                )
                .child(
                    div().text_xs().child(format!(
                        "Work item: {} · attempt: {}",
                        launch
                            .work_item_id
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "unknown".into()),
                        launch.attempt_id.as_deref().unwrap_or("not recorded")
                    )),
                )
                .child(div().text_xs().child(status)),
        );
    }
    v_flex()
        .size_full()
        .p_3()
        .gap_3()
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .child(button("factory-send", Command::SendToFactory, owner, state))
                .child(button(
                    "factory-refresh",
                    Command::RefreshFactoryRuns,
                    owner,
                    state,
                )),
        )
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .child(button(
                    "factory-folder",
                    Command::OpenFactoryFolder,
                    owner,
                    state,
                ))
                .child(button(
                    "factory-azure",
                    Command::DispatchAzureWorkItem,
                    owner,
                    state,
                )),
        )
        .child(body)
        .into_any_element()
}

pub fn output(
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    scroll: &ScrollHandle,
) -> AnyElement {
    let mut body = v_flex()
        .id("output-detail")
        .size_full()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .p_3()
        .gap_2();
    if let Some(launch) = state.selected_launch.and_then(|id| {
        state
            .snapshot
            .launches
            .iter()
            .find(|launch| launch.id == id)
    }) {
        body = body.child(format!(
            "Factory launch {} · created {}",
            launch.id, launch.created_at
        ));
        if let Some(error) = &launch.launch_error {
            body = body.child(div().text_color(rgb(0xE6A0A0)).child(error.clone()));
        }
        if let Some(snapshot) = state.factory_runs.get(&launch.id) {
            body = body
                .child(div().text_lg().child(snapshot.status_text()))
                .child(format!(
                    "Last event: {}",
                    snapshot.last_seen.as_deref().unwrap_or("none")
                ));
            for diagnostic in &snapshot.diagnostics {
                body = body.child(div().text_color(rgb(0xE5C07B)).child(diagnostic.clone()));
            }
            for path in &snapshot.correlation_candidates {
                body = body.child(path_actions("correlation-candidate", path));
            }
            if let Some(branch) = snapshot
                .details
                .get("feature-branch-ready")
                .and_then(|value| value.get("featureBranch"))
                .and_then(|value| value.as_str())
            {
                let branch = branch.to_owned();
                let copy = branch.clone();
                body = body.child(
                    h_flex()
                        .gap_2()
                        .child(format!("Feature ref: {branch}"))
                        .child(
                            Button::new("copy-feature-ref")
                                .small()
                                .ghost()
                                .label("Copy ref")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                                }),
                        ),
                );
            }
            if !snapshot.tasks.is_empty() {
                body = body.child(div().text_lg().child("Tasks and dependency links"));
            }
            for task in snapshot.tasks.values() {
                let mut row = v_flex()
                    .gap_1()
                    .p_2()
                    .border_b_1()
                    .border_color(rgb(0x343A46))
                    .child(format!(
                        "{} · {} · {}",
                        task.id,
                        task.title.as_deref().unwrap_or("Task"),
                        task.state
                    ));
                if !task.dependencies.is_empty() {
                    row = row.child(format!("Depends on: {}", task.dependencies.join(", ")));
                }
                if let Some(branch) = &task.branch {
                    row = row.child(format!("Branch: {branch}"));
                }
                if let Some(step) = &task.step {
                    row = row.child(format!("Step: {step}"));
                }
                if let Some(diagnostic) = &task.diagnostic {
                    row = row.child(div().text_color(rgb(0xE5C07B)).child(diagnostic.clone()));
                }
                body = body.child(row);
            }
            for path in &snapshot.transcripts {
                body = body.child(
                    v_flex()
                        .gap_1()
                        .child("Sandbox transcript (plain text)")
                        .child(path_actions("transcript", path))
                        .child(read_button("Tail transcript", path.clone(), None, owner)),
                );
            }
            for path in &snapshot.artifacts {
                body = body.child(path_actions("artifact", path));
            }
            if !snapshot.details.is_null() {
                body = body.child(
                    div()
                        .font_family("Menlo")
                        .text_xs()
                        .whitespace_normal()
                        .child(
                            serde_json::to_string_pretty(&snapshot.details)
                                .unwrap_or_else(|_| snapshot.details.to_string()),
                        ),
                );
            }
            body = body.child(
                div()
                    .text_lg()
                    .child("Recent events and advisory notes (latest 200)"),
            );
            for note in &snapshot.notes {
                body = body.child(
                    v_flex()
                        .gap_1()
                        .pb_2()
                        .border_b_1()
                        .border_color(rgb(0x343A46))
                        .child(format!(
                            "{} · {}",
                            note.timestamp.as_deref().unwrap_or("unknown time"),
                            note.event_type
                        ))
                        .child(
                            div()
                                .font_family("Menlo")
                                .text_xs()
                                .whitespace_normal()
                                .child(note.payload.to_string()),
                        ),
                );
            }
        } else {
            body = body.child("Launch outcome unknown; durable event files are being replayed. No process ownership is inferred.");
        }
        if let Some(path) = &launch.event_path {
            body = body.child(path_actions("events", path)).child(read_button(
                "Read raw events",
                path.clone(),
                None,
                owner,
            ));
        }
        let launch_dir = state
            .data_dir
            .join("factory-launches")
            .join(launch.id.to_string());
        body = body.child(
            h_flex()
                .gap_2()
                .child(read_button(
                    "Tail launcher stdout",
                    launch_dir.join("stdout.log"),
                    None,
                    owner,
                ))
                .child(read_button(
                    "Tail launcher stderr",
                    launch_dir.join("stderr.log"),
                    None,
                    owner,
                )),
        );
        for key in ["log_dir", "target_repo", "data_root"] {
            if let Some(path) = launch.descriptor.get(key).and_then(|value| value.as_str()) {
                body = body
                    .child(div().text_xs().child(key))
                    .child(path_actions(key, &PathBuf::from(path)));
            }
        }
        let descriptor = serde_json::to_string_pretty(&launch.descriptor)
            .unwrap_or_else(|_| launch.descriptor.to_string());
        body = body.child(
            Button::new("copy-launch-descriptor")
                .small()
                .ghost()
                .label("Copy launch descriptor")
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(descriptor.clone()))
                }),
        );
    }
    if let Some(path) = &state.factory_text_path {
        body = body
            .child(div().text_lg().child("File-backed output"))
            .child(path_actions("selected-output", path));
        match &state.factory_text {
            Some(Ok(page)) => {
                let mut controls = h_flex().gap_2().child(read_button(
                    "Latest / resume tail",
                    path.clone(),
                    None,
                    owner,
                ));
                if page.has_older {
                    controls = controls.child(read_button(
                        "Load older text",
                        path.clone(),
                        Some(page.start_offset),
                        owner,
                    ));
                }
                let copy = page.text.clone();
                body = body
                    .child(
                        controls.child(
                            Button::new("copy-output-page")
                                .small()
                                .ghost()
                                .label("Copy displayed text")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                                }),
                        ),
                    )
                    .child(
                        div()
                            .font_family("Menlo")
                            .text_xs()
                            .whitespace_normal()
                            .child(page.text.clone()),
                    );
            }
            Some(Err(error)) => {
                body = body.child(
                    div()
                        .text_color(rgb(0xE5C07B))
                        .child(format!("File unavailable: {error}")),
                )
            }
            None => body = body.child("Loading file…"),
        }
    }
    body = body.child(div().text_sm().child("Workspace diagnostics"));
    if state.notices.is_empty() {
        body = body.child(
            div()
                .text_color(rgb(0x8B95A5))
                .child("No workspace errors reported."),
        );
    }
    for notice in &state.notices {
        body = body.child(div().font_family("Menlo").text_xs().child(notice.clone()));
    }
    for project in &state.snapshot.projects {
        if let Some(Err(error)) = state.repositories.get(&project.id) {
            body = body.child(
                div()
                    .text_color(rgb(0xE6A0A0))
                    .child(format!("{}: {error}", project.name)),
            );
        }
    }
    body.into_any_element()
}
