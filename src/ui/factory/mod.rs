pub mod form;

use super::{commands::Command, workspace::Workspace};
use crate::{
    app::{AppState, NoticeLevel},
    factory::TextPage,
    model::{FactoryLaunch, LaunchId, ThreadId},
};
use gpui_kit::{
    component::{
        ActiveTheme, Sizable,
        accordion::Accordion,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        scroll::ScrollableElement,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{
    cell::Cell,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

type DisclosureContent = Box<dyn FnOnce(&mut Window, &mut App) -> AnyElement>;

#[derive(IntoElement)]
struct Disclosure {
    id: SharedString,
    title: SharedString,
    content: DisclosureContent,
}

impl RenderOnce for Disclosure {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| false);
        let open = *state.read(cx);
        let content = open.then(|| (self.content)(window, cx));
        Accordion::new(self.id)
            .small()
            .bordered(false)
            .h_auto()
            .item(|item| {
                let item = item.title(self.title).open(open);
                match content {
                    Some(content) => item.child(content),
                    None => item,
                }
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
    let content = content.into_any_element();
    lazy_disclosure(id, title, move |_, _| content)
}

fn lazy_disclosure(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    content: impl FnOnce(&mut Window, &mut App) -> AnyElement + 'static,
) -> Disclosure {
    Disclosure {
        id: id.into(),
        title: title.into(),
        content: Box::new(content),
    }
}

fn raw_event(owner: &WeakEntity<Workspace>, launch: LaunchId, offset: u64, cx: &App) -> AnyElement {
    let raw = owner.upgrade().and_then(|workspace| {
        let state = workspace.read(cx).state.read(cx);
        state
            .factory_runs
            .get(&launch)?
            .notes
            .iter()
            .find(|note| note.offset == offset)
            .map(|note| note.raw.clone())
    });
    plain_text(raw.unwrap_or_default(), cx)
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
            workspace.execute(Command::FocusOutput, window, cx);
        });
    })
}

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
            let (owner, launch_id, offset) = (owner.clone(), launch.id, note.offset);
            events = events.child(row.child(lazy_disclosure(
                format!("factory-event-{}-{}", launch.id, note.offset),
                "Raw event",
                move |_, cx| raw_event(&owner, launch_id, offset, cx),
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
    thread_id: ThreadId,
    owner: &WeakEntity<Workspace>,
    scroll: &ScrollHandle,
    cx: &App,
) -> AnyElement {
    if state.factory_text_path.is_some()
        && let Some(Ok(page)) = &state.factory_text
    {
        return OutputPage {
            page: page.clone(),
            thread_id,
            owner: owner.clone(),
        }
        .into_any_element();
    }
    FollowingScroll {
        id: "output-detail".into(),
        revision: None,
        scroll: scroll.clone(),
        content: v_flex()
            .p_3()
            .gap_2()
            .text_color(cx.theme().foreground)
            .children(output_header(state, thread_id, owner, cx))
            .children(output_footer(state, thread_id, cx))
            .into_any_element(),
    }
    .into_any_element()
}

fn output_header(
    state: &AppState,
    thread_id: ThreadId,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> Vec<AnyElement> {
    let mut body = Vec::new();
    if let Some(launch) = launch_for(state, thread_id) {
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
        body.push(controls.into_any_element());
        body.push(
            Button::new("copy-launch-descriptor")
                .small()
                .ghost()
                .label("Copy launch descriptor")
                .on_click(move |_, _, cx| {
                    let text = serde_json::to_string_pretty(&descriptor)
                        .unwrap_or_else(|_| descriptor.to_string());
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                })
                .into_any_element(),
        );
        for key in ["log_dir", "target_repo", "data_root"] {
            if let Some(path) = launch.descriptor.get(key).and_then(|value| value.as_str()) {
                body.push(
                    disclosure(
                        format!("output-{key}-{}", launch.id),
                        key,
                        path_actions(key, Path::new(path), cx),
                    )
                    .into_any_element(),
                );
            }
        }
    }
    if let Some(path) = &state.factory_text_path {
        body.push(path_actions("selected-output", path, cx));
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
                let copy = page.clone();
                body.push(
                    controls
                        .child(
                            Button::new("copy-output-page")
                                .small()
                                .ghost()
                                .label("Copy displayed text")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copy.text.clone(),
                                    ))
                                }),
                        )
                        .into_any_element(),
                );
            }
            Some(Err(error)) => {
                body.push(
                    div()
                        .text_color(cx.theme().danger)
                        .child(format!("File unavailable: {error}"))
                        .into_any_element(),
                );
            }
            None => body.push("Loading file…".into_any_element()),
        }
    }
    body
}

fn output_footer(state: &AppState, thread_id: ThreadId, cx: &App) -> Vec<AnyElement> {
    let mut body = Vec::new();
    if !state.notices.is_empty() {
        body.push(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Workspace diagnostics")
                .into_any_element(),
        );
    }
    for notice in &state.notices {
        let (level, color) = match notice.level {
            NoticeLevel::Info => ("Info", cx.theme().muted_foreground),
            NoticeLevel::Diagnostic => ("Diagnostic", cx.theme().muted_foreground),
            NoticeLevel::Warning => ("Warning", cx.theme().warning),
            NoticeLevel::Error => ("Error", cx.theme().danger),
        };
        body.push(
            div()
                .text_xs()
                .text_color(color)
                .child(format!("{level}: {}", notice.text))
                .into_any_element(),
        );
    }
    for project in &state.snapshot.projects {
        if let Some(Err(error)) = state.repositories.get(&project.id) {
            body.push(
                div()
                    .text_color(cx.theme().danger)
                    .child(format!("{}: {error}", project.name))
                    .into_any_element(),
            );
        }
    }
    let empty = launch_for(state, thread_id).is_none()
        && state.factory_text_path.is_none()
        && state.notices.is_empty()
        && !state.repositories.values().any(Result::is_err);
    if empty {
        body.push(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No output yet. Factory launch logs and diagnostics appear here.")
                .into_any_element(),
        );
    }
    body
}

fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut start = 0;
    let mut lines = Vec::new();
    for (index, _) in text.match_indices('\n') {
        lines.push(start..index);
        start = index + 1;
    }
    lines.push(start..text.len());
    lines
}

fn continued_tail(old: &TextPage, new: &TextPage) -> Option<usize> {
    if old.path != new.path || new.start_offset < old.start_offset {
        return None;
    }
    let skip = usize::try_from(new.start_offset - old.start_offset).ok()?;
    let rest = old.text.get(skip..)?;
    (old.text.as_bytes()[..skip]
        .last()
        .is_none_or(|byte| *byte == b'\n')
        && new.text.starts_with(rest))
    .then(|| {
        old.text.as_bytes()[..skip]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
    })
}

#[derive(IntoElement)]
struct OutputPage {
    page: Arc<TextPage>,
    thread_id: ThreadId,
    owner: WeakEntity<Workspace>,
}

struct PageRows {
    page: Option<Arc<TextPage>>,
    lines: Arc<[Range<usize>]>,
    list: ListState,
    estimated_width: Rc<Cell<Option<Pixels>>>,
}

impl PageRows {
    fn new() -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(400.));
        list.set_follow_mode(FollowMode::Tail);
        Self {
            page: None,
            lines: Arc::from([]),
            list,
            estimated_width: Rc::default(),
        }
    }

    fn show(&mut self, page: &Arc<TextPage>) {
        if self
            .page
            .as_ref()
            .is_some_and(|shown| Arc::ptr_eq(shown, page))
        {
            return;
        }
        let lines: Arc<[Range<usize>]> = line_ranges(&page.text).into();
        let continued = self.page.as_ref().and_then(|old| continued_tail(old, page));
        match continued {
            Some(dropped) => {
                let kept = self.lines.len() - dropped;
                self.list.splice(1..1 + dropped, 0);
                self.list.splice(kept..kept + 1, lines.len() + 1 - kept);
            }
            None => {
                let following = self.page.is_none() || self.list.is_following_tail();
                let top = self.list.logical_scroll_top();
                self.list.reset(lines.len() + 2);
                if following {
                    self.list.set_follow_mode(FollowMode::Tail);
                } else {
                    self.list.scroll_to(ListOffset {
                        item_ix: top.item_ix.min(lines.len() + 1),
                        offset_in_item: top.offset_in_item,
                    });
                }
            }
        }
        self.page = Some(page.clone());
        self.lines = lines;
    }
}

impl RenderOnce for OutputPage {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let rows = window.use_keyed_state("output-detail-page", cx, |_, _| PageRows::new());
        rows.update(cx, |rows, _| rows.show(&self.page));
        let rows = rows.read(cx);
        let (list_state, lines, estimated_width) = (
            rows.list.clone(),
            rows.lines.clone(),
            rows.estimated_width.clone(),
        );
        let (page, owner, thread_id) = (self.page, self.owner, self.thread_id);
        let view = list(list_state.clone(), move |ix, _, cx| {
            if let Some(range) = ix.checked_sub(1).and_then(|line| lines.get(line)) {
                let line = &page.text[range.clone()];
                return div()
                    .px_3()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .whitespace_normal()
                    .child(SharedString::from(
                        if line.is_empty() { " " } else { line }.to_owned(),
                    ))
                    .into_any_element();
            }
            let workspace = owner.upgrade();
            let state = workspace
                .as_ref()
                .map(|workspace| workspace.read(cx).state.read(cx));
            if ix == 0 {
                let header = state
                    .map(|state| output_header(state, thread_id, &owner, cx))
                    .unwrap_or_default();
                v_flex()
                    .debug_selector(|| "output-detail-header".into())
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .gap_2()
                    .children(header)
                    .into_any_element()
            } else {
                let footer = state
                    .map(|state| output_footer(state, thread_id, cx))
                    .unwrap_or_default();
                v_flex()
                    .debug_selector(|| "output-detail-footer".into())
                    .px_3()
                    .pb_3()
                    .gap_2()
                    .when(!footer.is_empty(), |footer| footer.pt_2())
                    .children(footer)
                    .into_any_element()
            }
        })
        .size_full();
        div()
            .id("output-detail")
            .relative()
            .size_full()
            .min_h_0()
            .text_color(cx.theme().foreground)
            .child(view)
            .child(super::conversation::estimate_unmeasured_rows(
                &list_state,
                &estimated_width,
                window.line_height(),
            ))
            .vertical_scrollbar(&list_state)
    }
}

#[cfg(test)]
mod tests {
    use super::{PageRows, TextPage, continued_tail, line_ranges};
    use std::{path::PathBuf, sync::Arc};

    fn page(start: u64, text: &str) -> TextPage {
        TextPage {
            path: PathBuf::from("stdout.log"),
            text: text.into(),
            start_offset: start,
            end_offset: start + text.len() as u64,
            has_older: start > 0,
        }
    }

    #[test]
    fn page_lines_keep_the_trailing_empty_line() {
        let text = "one\n\nthree\n";
        let lines: Vec<&str> = line_ranges(text)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(lines, ["one", "", "three", ""]);
        assert_eq!(line_ranges("").len(), 1);
    }

    #[test]
    fn a_later_tail_of_the_same_file_continues_the_page() {
        let old = page(10, "a\nb\nc\n");
        assert_eq!(continued_tail(&old, &page(10, "a\nb\nc\nd\n")), Some(0));
        assert_eq!(continued_tail(&old, &page(12, "b\nc\nd\n")), Some(1));
        assert_eq!(continued_tail(&old, &page(13, "\nc\n")), None, "mid-line");
        assert_eq!(continued_tail(&old, &page(10, "x\nb\n")), None, "rewritten");
        assert_eq!(continued_tail(&old, &page(0, "z\na\n")), None, "older page");
        let mut other = page(10, "a\nb\nc\n");
        other.path = PathBuf::from("stderr.log");
        assert_eq!(continued_tail(&old, &other), None);
    }

    #[test]
    fn the_same_page_keeps_its_rows_and_a_continued_tail_splices_them() {
        let mut rows = PageRows::new();
        let first = Arc::new(page(10, "a\nb\nc\n"));
        rows.show(&first);
        assert_eq!(rows.list.item_count(), 6);
        let lines = rows.lines.clone();
        rows.show(&first);
        assert!(
            Arc::ptr_eq(&lines, &rows.lines),
            "an unchanged page is not split again"
        );
        rows.show(&Arc::new(page(12, "b\nc\nd\ne\n")));
        assert_eq!(rows.lines.len(), 5);
        assert_eq!(rows.list.item_count(), 7);
        rows.show(&Arc::new(page(0, "z\n")));
        assert_eq!(rows.list.item_count(), 4);
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod runtime_tests {
    use super::{OutputPage, TextPage};
    use gpui_kit::{
        AppContext, Context, IntoElement, ParentElement, Render, Styled, VisualTestContext,
        WeakEntity, Window, component::Root, div,
    };
    use std::{path::PathBuf, sync::Arc};

    struct Host(Arc<TextPage>);
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(OutputPage {
                page: self.0.clone(),
                thread_id: crate::model::ThreadId::new_v4(),
                owner: WeakEntity::new_invalid(),
            })
        }
    }

    #[gpui_kit::test]
    fn a_long_output_page_opens_at_its_end(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let text: String = (0..5_000).map(|line| format!("line {line}\n")).collect();
        let page = Arc::new(TextPage {
            path: PathBuf::from("stdout.log"),
            end_offset: text.len() as u64,
            text,
            start_offset: 0,
            has_older: false,
        });
        let window = cx.add_window(|window, cx| {
            let host = cx.new(|_| Host(page));
            Root::new(host, window, cx)
        });
        for _ in 0..2 {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
        }
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        assert!(
            visual.debug_bounds("output-detail-footer").is_some(),
            "the end of the page is shown"
        );
        assert!(
            visual.debug_bounds("output-detail-header").is_none(),
            "rows far above the viewport are not rendered"
        );
    }
}
