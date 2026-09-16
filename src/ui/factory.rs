use super::workspace::{Region, Workspace};
use crate::{
    app::{AppState, NoticeLevel},
    model::{FactoryLaunch, ThreadId},
};
use gpui_kit::{
    component::{
        ActiveTheme, Sizable,
        accordion::Accordion,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        v_flex,
    },
    *,
};
use std::path::{Path, PathBuf};

// Disclosure state belongs to the native element identity, not the selected thread.
#[derive(IntoElement)]
struct Disclosure {
    id: SharedString,
    title: SharedString,
    content: AnyElement,
}

impl RenderOnce for Disclosure {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| false);
        let open = *state.read(cx);
        Accordion::new(self.id)
            .small()
            .bordered(false)
            .h_auto()
            .item(|item| {
                let item = item.title(self.title).open(open);
                if open { item.child(self.content) } else { item }
            })
            .on_toggle_click(move |indices, window, cx| {
                state.update(cx, |open, _| *open = !indices.is_empty());
                window.refresh();
            })
    }
}

fn disclosure(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    content: impl IntoElement,
) -> Disclosure {
    Disclosure {
        id: id.into(),
        title: title.into(),
        content: content.into_any_element(),
    }
}

#[derive(IntoElement)]
struct JsonDetail(serde_json::Value);

impl RenderOnce for JsonDetail {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        plain_text(
            serde_json::to_string_pretty(&self.0).unwrap_or_else(|_| self.0.to_string()),
            cx,
        )
    }
}

fn plain_text(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(cx.theme().mono_font_size)
        .whitespace_normal()
        .child(text.into())
        .into_any_element()
}

// Only new log content can request following. Opening a disclosure or repainting
// must not pull a reader back down, and an offset above the bottom stays put.
#[derive(IntoElement)]
struct FollowingScroll {
    id: SharedString,
    revision: Option<(u64, usize)>,
    scroll: ScrollHandle,
    content: AnyElement,
}

impl RenderOnce for FollowingScroll {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let previous = window.use_keyed_state(self.id.clone(), cx, |_, _| None);
        let changed = previous.read(cx).as_ref() != Some(&self.revision);
        if changed {
            if previous.read(cx).is_none()
                || self.scroll.offset().y + self.scroll.max_offset().y <= Pixels::ZERO
            {
                self.scroll.scroll_to_bottom();
            }
            previous.update(cx, |previous, _| *previous = Some(self.revision));
        }
        div()
            .id(self.id)
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(self.content)
    }
}

pub fn path_actions(label: &'static str, path: &Path, cx: &App) -> AnyElement {
    let reveal = path.to_path_buf();
    let copy = path.to_string_lossy().into_owned();
    let reveal_id = SharedString::from(format!("reveal-{label}-{copy}"));
    let copy_id = SharedString::from(format!("copy-{label}-{copy}"));
    v_flex()
        .gap_1()
        .child(plain_text(copy.clone(), cx))
        .child(
            h_flex()
                .flex_wrap()
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
                        .label("Copy path")
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
    .on_click(move |_, window, cx| {
        let _ = owner.update(cx, |workspace, cx| {
            workspace.state.update(cx, |state, cx| {
                state.load_factory_text(path.clone(), before, cx)
            });
            workspace.focus_region(Region::Output, window, cx);
        });
    })
}

fn selected_launch(state: &AppState) -> Option<&FactoryLaunch> {
    launch_for(state, state.factory_thread()?.id)
}
/// The launch recorded for a Factory thread, if it was ever started.
pub fn launch_for(state: &AppState, thread_id: ThreadId) -> Option<&FactoryLaunch> {
    state
        .snapshot
        .launches
        .iter()
        .find(|launch| launch.factory_thread_id == thread_id)
}

pub fn render(
    state: &AppState,
    thread_id: ThreadId,
    owner: &WeakEntity<Workspace>,
    scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    let Some(launch) = launch_for(state, thread_id) else {
        return div()
            .p_3()
            .text_color(cx.theme().muted_foreground)
            .child("No Factory run recorded for this thread.")
            .into_any_element();
    };
    let snapshot = state.factory_runs.get(&launch.id);
    let status = snapshot
        .map(|snapshot| snapshot.status_text())
        .unwrap_or_else(|| {
            launch
                .launch_error
                .as_ref()
                .map(|error| format!("Launch failed: {error}"))
                .unwrap_or_else(|| "Launch outcome unknown; replaying durable history".into())
        });
    let menu_owner = owner.clone();
    let mut header = v_flex()
        .px_3()
        .pt_3()
        .gap_2()
        .text_color(cx.theme().foreground)
        .child(
            h_flex()
                .gap_2()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .text_color(if launch.launch_error.is_some() {
                            cx.theme().danger
                        } else {
                            cx.theme().foreground
                        })
                        .child(status),
                )
                .child(
                    Button::new("factory-monitor-menu")
                        .small()
                        .ghost()
                        .label("Run actions")
                        .dropdown_menu(move |menu, _, _| {
                            let owner = menu_owner.clone();
                            menu.item(PopupMenuItem::new("Refresh run status").on_click(
                                move |_, _, cx| {
                                    let _ = owner.update(cx, |workspace, cx| {
                                        workspace.state.update(cx, |state, cx| {
                                            state.refresh_factory_runs();
                                            cx.notify();
                                        });
                                    });
                                },
                            ))
                        }),
                ),
        );
    if let Some(error) = launch
        .launch_error
        .as_ref()
        .filter(|_| snapshot.is_some_and(|snapshot| snapshot.reported_state.terminal()))
    {
        header = header.child(div().text_color(cx.theme().danger).child(error.clone()));
    }
    let mut body = v_flex().p_3().gap_3().text_color(cx.theme().foreground);
    if let Some(snapshot) = snapshot {
        for diagnostic in &snapshot.diagnostics {
            body = body.child(
                div()
                    .text_color(cx.theme().warning)
                    .child(diagnostic.clone()),
            );
        }
        for path in &snapshot.correlation_candidates {
            body = body.child(path_actions("correlation-candidate", path, cx));
        }
        let mut events = v_flex().gap_2();
        if snapshot.notes.is_empty() {
            events = events.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("No Factory events recorded yet."),
            );
        }
        for note in snapshot
            .notes
            .iter()
            .skip(snapshot.notes.len().saturating_sub(200))
        {
            let mut row = v_flex()
                .gap_1()
                .pb_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} · {}",
                            note.timestamp.as_deref().unwrap_or("Unknown time"),
                            note.event_type
                        )),
                );
            if let Some(text) = [
                "text",
                "message",
                "title",
                "reason",
                "diagnostic",
                "stopFailure",
            ]
            .into_iter()
            .find_map(|key| {
                note.payload
                    .get(key)
                    .and_then(|value| value.as_str())
                    .filter(|text| !text.is_empty())
            }) {
                row = row.child(div().text_sm().whitespace_normal().child(text.to_owned()));
            }
            events = events.child(row.child(disclosure(
                format!("factory-event-{}-{}", launch.id, note.offset),
                "Raw event",
                plain_text(note.raw.clone(), cx),
            )));
        }
        body = body.child(events);
    }
    if let Some(path) = &launch.event_path {
        let oldest = snapshot
            .and_then(|snapshot| snapshot.notes.first())
            .map(|note| note.offset);
        let mut controls = h_flex().flex_wrap().gap_1();
        if let Some(offset) = oldest.filter(|offset| *offset > 0) {
            controls = controls.child(read_button(
                "Older events…",
                path.clone(),
                Some(offset),
                owner,
            ));
        }
        body =
            body.child(controls.child(read_button("Read raw events", path.clone(), None, owner)));
    }
    if let Some(snapshot) = snapshot {
        if !snapshot.tasks.is_empty() {
            let mut tasks = v_flex().gap_2();
            for task in snapshot.tasks.values() {
                let mut row = v_flex()
                    .gap_1()
                    .pb_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
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
                    row = row.child(
                        div()
                            .text_color(cx.theme().warning)
                            .child(diagnostic.clone()),
                    );
                }
                tasks = tasks.child(row);
            }
            body = body.child(disclosure(
                format!("factory-tasks-{}", launch.id),
                "Tasks and dependencies",
                tasks,
            ));
        }
        if let Some(branch) = snapshot
            .details
            .get("feature-branch-ready")
            .and_then(|value| value.get("featureBranch"))
            .and_then(|value| value.as_str())
        {
            let copy = branch.to_owned();
            body = body.child(disclosure(
                format!("factory-ref-{}", launch.id),
                "Feature ref",
                v_flex()
                    .gap_1()
                    .child(plain_text(branch.to_owned(), cx))
                    .child(
                        Button::new("copy-feature-ref")
                            .small()
                            .ghost()
                            .label("Copy ref")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                            }),
                    ),
            ));
        }
        for (label, paths) in [
            ("Artifacts", &snapshot.artifacts),
            ("Transcripts", &snapshot.transcripts),
        ] {
            if !paths.is_empty() {
                let mut content = v_flex().gap_2();
                for path in paths {
                    content = content.child(path_actions(label, path, cx));
                    if label == "Transcripts" {
                        content = content.child(read_button(
                            "Read transcript",
                            path.clone(),
                            None,
                            owner,
                        ));
                    }
                }
                body = body.child(disclosure(
                    format!("factory-{label}-{}", launch.id),
                    label,
                    content,
                ));
            }
        }
        if snapshot
            .details
            .as_object()
            .is_some_and(|details| !details.is_empty())
        {
            body = body.child(disclosure(
                format!("factory-details-{}", launch.id),
                "Result details",
                JsonDetail(snapshot.details.clone()),
            ));
        }
    }
    body = body.child(disclosure(
        format!("factory-parameters-{}", launch.id),
        "Run parameters (read-only)",
        v_flex()
            .gap_2()
            .child(format!(
                "Launch {} · created {}",
                launch.id, launch.created_at
            ))
            .child(JsonDetail(launch.descriptor.clone())),
    ));
    v_flex()
        .size_full()
        .min_h_0()
        .child(header)
        .child(div().flex_1().min_h_0().child(FollowingScroll {
            id: format!("factory-monitor-{}", launch.id).into(),
            revision: snapshot.and_then(|snapshot| {
                snapshot
                    .notes
                    .last()
                    .map(|note| (note.offset, snapshot.notes.len()))
            }),
            scroll: scroll.clone(),
            content: body.into_any_element(),
        }))
        .into_any_element()
}

pub fn output(
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    let mut body = v_flex().p_3().gap_2().text_color(cx.theme().foreground);
    if let Some(launch) = selected_launch(state) {
        let launch_dir = state
            .data_dir
            .join("factory-launches")
            .join(launch.id.to_string());
        let mut controls = h_flex()
            .flex_wrap()
            .gap_1()
            .child(read_button(
                "Read launcher stdout",
                launch_dir.join("stdout.log"),
                None,
                owner,
            ))
            .child(read_button(
                "Read launcher stderr",
                launch_dir.join("stderr.log"),
                None,
                owner,
            ));
        if let Some(path) = &launch.event_path {
            controls = controls.child(read_button("Read raw events", path.clone(), None, owner));
        }
        let descriptor = launch.descriptor.clone();
        body = body.child(controls).child(
            Button::new("copy-launch-descriptor")
                .small()
                .ghost()
                .label("Copy launch descriptor")
                .on_click(move |_, _, cx| {
                    let text = serde_json::to_string_pretty(&descriptor)
                        .unwrap_or_else(|_| descriptor.to_string());
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }),
        );
        for key in ["log_dir", "target_repo", "data_root"] {
            if let Some(path) = launch.descriptor.get(key).and_then(|value| value.as_str()) {
                body = body.child(disclosure(
                    format!("output-{key}-{}", launch.id),
                    key,
                    path_actions(key, Path::new(path), cx),
                ));
            }
        }
    }
    if let Some(path) = &state.factory_text_path {
        body = body.child(path_actions("selected-output", path, cx));
        match &state.factory_text {
            Some(Ok(page)) => {
                let mut controls = h_flex().flex_wrap().gap_1().child(read_button(
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
                    .child(plain_text(page.text.clone(), cx));
            }
            Some(Err(error)) => {
                body = body.child(
                    div()
                        .text_color(cx.theme().danger)
                        .child(format!("File unavailable: {error}")),
                );
            }
            None => body = body.child("Loading file…"),
        }
    }
    if !state.notices.is_empty() {
        body = body.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Workspace diagnostics"),
        );
    }
    for notice in &state.notices {
        let (level, color) = match notice.level {
            NoticeLevel::Info => ("Info", cx.theme().muted_foreground),
            NoticeLevel::Diagnostic => ("Diagnostic", cx.theme().muted_foreground),
            NoticeLevel::Warning => ("Warning", cx.theme().warning),
            NoticeLevel::Error => ("Error", cx.theme().danger),
        };
        body = body.child(
            div()
                .text_xs()
                .text_color(color)
                .child(format!("{level}: {}", notice.text)),
        );
    }
    for project in &state.snapshot.projects {
        if let Some(Err(error)) = state.repositories.get(&project.id) {
            body = body.child(
                div()
                    .text_color(cx.theme().danger)
                    .child(format!("{}: {error}", project.name)),
            );
        }
    }
    FollowingScroll {
        id: "output-detail".into(),
        revision: state
            .factory_text
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|page| (page.end_offset, page.text.len())),
        scroll: scroll.clone(),
        content: body.into_any_element(),
    }
    .into_any_element()
}
