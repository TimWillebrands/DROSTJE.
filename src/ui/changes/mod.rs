pub(super) mod review;

use super::{
    commands::{self, Command, OpenChangeInNeovim, ToggleChangesLayout, button},
    settings::{self, DialogKind},
    theme,
    workspace::{Motion, PAGE_SHARE, Workspace},
};
use crate::{
    app::{AppState, ChangesLayout},
    diff::{FileChange, FileHeader, Location, LocationError, Patch, Row, RowKind, SplitRow},
    file_refs::FileReference,
    model::ThreadId,
};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable,
        alert::Alert,
        button::{Button, ButtonVariants},
        h_flex,
        scroll::ScrollableElement,
        tag::Tag,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
};

pub(super) type ChangesLists = Rc<RefCell<HashMap<ThreadId, ChangesList>>>;

pub(super) struct ChangesList {
    focus: FocusHandle,
    rows: RowList,
}

impl ChangesList {
    pub(super) fn contains_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }
}

const ROW_ESTIMATE: f32 = 1.5;

pub(super) struct RowList {
    list: ListState,
    rows: Arc<Patch>,
    layout: ChangesLayout,
    selected: Option<usize>,
    estimated_width: Rc<Cell<Option<Pixels>>>,
}

fn display_count(rows: &Patch, layout: ChangesLayout) -> usize {
    match layout {
        ChangesLayout::Inline => rows.rows.len(),
        ChangesLayout::SideBySide => rows.split.len(),
    }
}

fn convert(rows: &Patch, ix: usize, from: ChangesLayout, to: ChangesLayout) -> usize {
    match (from, to) {
        (ChangesLayout::Inline, ChangesLayout::SideBySide) => rows
            .split
            .iter()
            .position(|row| row.contains(ix))
            .unwrap_or(0),
        (ChangesLayout::SideBySide, ChangesLayout::Inline) => {
            rows.split.get(ix).map_or(0, |row| row.first())
        }
        _ => ix,
    }
}

impl RowList {
    fn new() -> Self {
        Self {
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            rows: Arc::default(),
            layout: ChangesLayout::Inline,
            selected: None,
            estimated_width: Rc::default(),
        }
    }

    fn sync(&mut self, rows: &Arc<Patch>, layout: ChangesLayout) {
        let same_rows = Arc::ptr_eq(&self.rows, rows);
        if same_rows && self.layout == layout {
            return;
        }
        let count = display_count(rows, layout);
        let top = self.list.logical_scroll_top();
        let (mut top_ix, mut offset) = (top.item_ix, top.offset_in_item);
        if self.layout != layout {
            top_ix = convert(&self.rows, top_ix, self.layout, layout);
            offset = px(0.);
            self.selected = self
                .selected
                .map(|ix| convert(&self.rows, ix, self.layout, layout));
        }
        if top_ix >= count {
            offset = px(0.);
        }
        let last = count.saturating_sub(1);
        self.list.reset(count);
        self.list.scroll_to(ListOffset {
            item_ix: top_ix.min(last),
            offset_in_item: offset,
        });
        self.selected = self.selected.filter(|_| count > 0).map(|ix| ix.min(last));
        self.estimated_width.set(None);
        self.rows = rows.clone();
        self.layout = layout;
    }

    fn select(&mut self, motion: Motion, page: usize) {
        let count = display_count(&self.rows, self.layout);
        if count == 0 {
            return;
        }
        let last = count - 1;
        let step = |delta: isize| match self.selected {
            None if delta < 0 => last,
            None => 0,
            Some(ix) => ix.saturating_add_signed(delta).min(last),
        };
        let next = match motion {
            Motion::First => 0,
            Motion::Last => last,
            Motion::Line(delta) => step(delta),
            Motion::Page(delta) => step(delta * page.max(1) as isize),
        };
        self.selected = Some(next);
        self.list.scroll_to_reveal_item(next);
    }

    fn location(&self) -> Option<Result<Location, LocationError>> {
        let ix = self.selected?;
        let row = match self.layout {
            ChangesLayout::Inline => ix,
            ChangesLayout::SideBySide => match *self.rows.split.get(ix)? {
                SplitRow::Full(row) => row,
                SplitRow::Pair { old, new } => new.or(old)?,
            },
        };
        Some(self.rows.location(row))
    }

    fn page(&self, line: Pixels) -> usize {
        (f32::from(self.list.viewport_bounds().size.height) * PAGE_SHARE / f32::from(line)) as usize
    }
}

fn row_list<'a>(
    lists: &'a mut HashMap<ThreadId, ChangesList>,
    id: ThreadId,
    cx: &App,
) -> &'a mut ChangesList {
    lists.entry(id).or_insert_with(|| ChangesList {
        focus: cx.focus_handle(),
        rows: RowList::new(),
    })
}

impl Workspace {
    pub(super) fn changes_focus(&self, id: ThreadId, cx: &App) -> FocusHandle {
        row_list(&mut self.changes_lists.borrow_mut(), id, cx)
            .focus
            .clone()
    }

    fn select_change(
        &mut self,
        id: ThreadId,
        motion: Motion,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let line = window.line_height();
        if let Some(list) = self.changes_lists.borrow_mut().get_mut(&id) {
            let page = list.rows.page(line);
            list.rows.select(motion, page);
        }
        cx.notify();
    }

    fn select_change_at(
        &mut self,
        id: ThreadId,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(list) = self.changes_lists.borrow_mut().get_mut(&id) {
            list.rows.selected = Some(ix);
            list.focus.focus(window, cx);
        }
        cx.notify();
    }

    pub(super) fn open_change_reason(&self, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let Some(thread) = state.conversation_thread() else {
            return Some("Select a conversation first.".into());
        };
        let lists = self.changes_lists.borrow();
        match lists.get(&thread.id).and_then(|list| list.rows.location()) {
            None => Some("Select a changed line in Changes first.".into()),
            Some(Err(error)) => Some(format!("Cannot open: {error}.")),
            Some(Ok(_)) => None,
        }
    }

    pub(super) fn open_selected_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| thread.id)
        else {
            return;
        };
        let location = self
            .changes_lists
            .borrow()
            .get(&thread)
            .and_then(|list| list.rows.location());
        match location {
            Some(Ok(location)) => self.open_file_reference(
                FileReference {
                    path: location.path,
                    line: Some(location.line),
                    column: None,
                },
                Some(thread),
                window,
                cx,
            ),
            Some(Err(error)) => self.state.update(cx, |state, cx| {
                state.notice(
                    crate::app::NoticeLevel::Warning,
                    format!("Could not open the change: {error}."),
                );
                cx.notify();
            }),
            None => {}
        }
    }
}

pub(super) fn render(
    state: &AppState,
    id: ThreadId,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let remote = state
        .snapshot
        .threads
        .iter()
        .any(|thread| thread.id == id && thread.remote.is_some());
    let diff = state.conversation_diff(id);
    let loading = diff.is_none_or(|diff| diff.loading);
    let result = diff.and_then(|diff| diff.result.as_ref());
    let colors = theme::diff_colors(cx);
    let summary = match result {
        Some(Ok(diff)) if diff.changed_files > 0 => h_flex()
            .gap_2()
            .child(if diff.changed_files == 1 {
                "1 file".to_owned()
            } else {
                format!("{} files", diff.changed_files)
            })
            .child(
                h_flex()
                    .gap_1()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(
                        div()
                            .text_color(colors.added_sign)
                            .child(format!("+{}", diff.additions)),
                    )
                    .child(
                        div()
                            .text_color(colors.removed_sign)
                            .child(format!("−{}", diff.deletions)),
                    ),
            )
            .into_any_element(),
        Some(Ok(_)) => "Working tree clean".into_any_element(),
        Some(Err(_)) => "Changes unavailable".into_any_element(),
        None if loading => "Loading changes…".into_any_element(),
        None => "Changes not loaded".into_any_element(),
    };
    let commit_panel = owner.upgrade().and_then(|workspace| {
        let workspace = workspace.read(cx);
        if workspace.commit_panel_thread != Some(id) {
            return None;
        }
        let dialog = workspace.dialog.clone()?;
        let matches_thread =
            matches!(dialog.read(cx).kind, DialogKind::Commit(thread) if thread == id);
        matches_thread.then(|| settings::render_commit_panel(dialog, cx))
    });
    let content = match result {
        Some(Ok(diff)) if diff.rows.rows.is_empty() => div()
            .flex_1()
            .p_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No uncommitted changes")
            .into_any_element(),
        Some(Ok(diff)) => {
            let layout = state.changes_layout();
            owner
                .upgrade()
                .map(|workspace| {
                    let lists = workspace.read(cx).changes_lists.clone();
                    let mut lists = lists.borrow_mut();
                    let entry = row_list(&mut lists, id, cx);
                    let list = &mut entry.rows;
                    list.sync(&diff.rows, layout);
                    ChangesRows {
                        id,
                        owner: owner.clone(),
                        list: list.list.clone(),
                        rows: list.rows.clone(),
                        layout,
                        selected: list.selected,
                        focus: entry.focus.clone(),
                        estimated_width: list.estimated_width.clone(),
                    }
                    .into_any_element()
                })
                .unwrap_or_else(|| div().into_any_element())
        }
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
        .when(remote, |view| {
            view.child(
                gpui_kit::component::label::Label::new(state.thread_host_label(id))
                    .secondary("Host changes · read-only here; local Git actions are disabled"),
            )
        })
        .child(
            h_flex()
                .flex_shrink_0()
                .justify_between()
                .gap_2()
                .pl_2()
                .pr_1()
                .py_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(div().flex_1().min_w_0().child(summary))
                .child(
                    button(
                        "toggle-changes-layout",
                        Command::ToggleChangesLayout,
                        owner,
                        state,
                    )
                    .label(match state.changes_layout() {
                        ChangesLayout::Inline => "Side by side",
                        ChangesLayout::SideBySide => "Inline",
                    })
                    .when(
                        Command::ToggleChangesLayout
                            .disabled_reason(state)
                            .is_none(),
                        |button| {
                            button.tooltip_with_action(
                                Command::ToggleChangesLayout.label(),
                                &ToggleChangesLayout,
                                Some("ChangesList"),
                            )
                        },
                    )
                    .debug_selector(|| "toggle-changes-layout".into()),
                )
                .child(open_button(owner, cx))
                .when(!remote, |row| {
                    row.child(
                        button("commit-changes", Command::CommitChanges, owner, state)
                            .label("Commit…")
                            .debug_selector(|| "commit-changes".into()),
                    )
                }),
        )
        .when_some(commit_panel.filter(|_| !remote), |view, panel| {
            view.child(panel)
        })
        .child(content)
        .into_any_element()
}

fn open_button(owner: &WeakEntity<Workspace>, cx: &App) -> AnyElement {
    let reason = owner
        .upgrade()
        .and_then(|workspace| workspace.read(cx).open_change_reason(cx));
    Button::new("open-change-in-neovim")
        .label("Open")
        .small()
        .ghost()
        .disabled(reason.is_some())
        .map(|button| match reason {
            Some(reason) => button.tooltip(reason),
            None => button.tooltip_with_action(
                Command::OpenChangeInNeovim.label(),
                &OpenChangeInNeovim,
                Some("ChangesList"),
            ),
        })
        .debug_selector(|| "open-change-in-neovim".into())
        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenChangeInNeovim), cx))
        .into_any_element()
}

#[cfg(feature = "runtime-qa")]
impl Workspace {
    pub fn changes_cursor(&self, id: ThreadId, window: &Window, cx: &App) -> (Option<usize>, bool) {
        self.changes_lists
            .borrow()
            .get(&id)
            .map_or((None, false), |list| {
                (list.rows.selected, list.contains_focused(window, cx))
            })
    }
}

#[derive(IntoElement)]
struct ChangesRows {
    id: ThreadId,
    owner: WeakEntity<Workspace>,
    list: ListState,
    rows: Arc<Patch>,
    layout: ChangesLayout,
    selected: Option<usize>,
    focus: FocusHandle,
    estimated_width: Rc<Cell<Option<Pixels>>>,
}

impl RenderOnce for ChangesRows {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            owner,
            list: list_state,
            rows,
            layout,
            selected,
            focus,
            estimated_width,
        } = self;
        let cursor = Cursor {
            selected,
            focused: focus.is_focused(window),
        };
        let row_owner = owner.clone();
        let rows_view = list(list_state.clone(), move |ix, _, cx| {
            let row = match layout {
                ChangesLayout::Inline => rows.rows.get(ix).map(|row| render_row(row, ix, cx)),
                ChangesLayout::SideBySide => rows
                    .split
                    .get(ix)
                    .map(|split| render_split_row(&rows, *split, ix, cx)),
            };
            let Some(row) = row else {
                return div().into_any_element();
            };
            let owner = row_owner.clone();
            cursor
                .wrap(row, ix, cx)
                .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    let open = event.click_count >= 2;
                    let _ = owner.update(cx, |workspace, cx| {
                        workspace.select_change_at(id, ix, window, cx);
                        if open {
                            workspace.execute(Command::OpenChangeInNeovim, window, cx);
                        }
                    });
                })
                .into_any_element()
        })
        .size_full();
        let motion = |motion: Motion| {
            let owner = owner.clone();
            move |window: &mut Window, cx: &mut App| {
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.select_change(id, motion, window, cx)
                });
            }
        };
        let (next, previous, first, last, page_down, page_up) = (
            motion(Motion::Line(1)),
            motion(Motion::Line(-1)),
            motion(Motion::First),
            motion(Motion::Last),
            motion(Motion::Page(1)),
            motion(Motion::Page(-1)),
        );
        div()
            .id("conversation-diff-patch")
            .debug_selector(|| "conversation-diff-patch".into())
            .test_support()
            .track_focus(&focus)
            .key_context("List ChangesList")
            .relative()
            .flex_1()
            .min_h_0()
            .pb_2()
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .on_action(move |_: &commands::SelectNext, window, cx| next(window, cx))
            .on_action(move |_: &commands::SelectPrevious, window, cx| previous(window, cx))
            .on_action(move |_: &commands::SelectFirst, window, cx| first(window, cx))
            .on_action(move |_: &commands::SelectLast, window, cx| last(window, cx))
            .on_action(move |_: &commands::SelectPageDown, window, cx| page_down(window, cx))
            .on_action(move |_: &commands::SelectPageUp, window, cx| page_up(window, cx))
            .child(rows_view)
            .child(super::conversation::estimate_unmeasured_rows(
                &list_state,
                &estimated_width,
                window.line_height() * ROW_ESTIMATE,
            ))
            .vertical_scrollbar(&list_state)
    }
}

#[derive(Clone, Copy)]
struct Cursor {
    selected: Option<usize>,
    focused: bool,
}

impl Cursor {
    fn wrap(self, row: AnyElement, ix: usize, cx: &App) -> Stateful<Div> {
        let selected = self.selected == Some(ix);
        let theme = cx.theme();
        div()
            .id(("change-row", ix))
            .w_full()
            .border_l_2()
            .border_color(match (selected, self.focused) {
                (true, true) => theme.muted_foreground,
                (true, false) => theme.border,
                (false, _) => theme.transparent,
            })
            .when(selected && self.focused, |row| row.bg(theme.list_active))
            .child(row)
    }
}

fn render_row(row: &Row, ix: usize, cx: &App) -> AnyElement {
    let colors = theme::diff_colors(cx);
    let muted = cx.theme().muted_foreground;
    match &row.kind {
        RowKind::File(header) => file_row(row, header, ix, cx),
        RowKind::Hunk => hunk_row(row, cx),
        RowKind::Note => note_row(row, cx),
        RowKind::Added | RowKind::Removed | RowKind::Context => {
            let (wash, sign, sign_color, number) = match row.kind {
                RowKind::Added => (Some(colors.added), "+", colors.added_sign, row.new_line),
                RowKind::Removed => (Some(colors.removed), "-", colors.removed_sign, row.old_line),
                _ => (None, " ", muted, row.new_line),
            };
            h_flex()
                .w_full()
                .items_start()
                .when_some(wash, |line, wash| line.bg(wash))
                .child(line_number(number, cx))
                .child(
                    div()
                        .flex_none()
                        .w(rems(1.))
                        .text_center()
                        .text_color(sign_color)
                        .child(sign),
                )
                .child(line_text(row))
                .into_any_element()
        }
    }
}

fn render_split_row(patch: &Patch, split: SplitRow, ix: usize, cx: &App) -> AnyElement {
    let (old, new) = match split {
        SplitRow::Full(row) => {
            return patch
                .rows
                .get(row)
                .map(|row| render_row(row, ix, cx))
                .unwrap_or_else(|| div().into_any_element());
        }
        SplitRow::Pair { old, new } => (old, new),
    };
    let side = |row: Option<usize>, old_side: bool| {
        let colors = theme::diff_colors(cx);
        let row = row.and_then(|ix| patch.rows.get(ix));
        let wash = match row.map(|row| &row.kind) {
            Some(RowKind::Added) => Some(colors.added),
            Some(RowKind::Removed) => Some(colors.removed),
            Some(_) => None,
            None => Some(cx.theme().muted.opacity(0.4)),
        };
        h_flex()
            .flex_1()
            .min_w_0()
            .items_start()
            .when_some(wash, |side, wash| side.bg(wash))
            .when(old_side, |side| {
                side.border_r_1().border_color(cx.theme().border)
            })
            .when_some(row, |side, row| {
                let number = if old_side { row.old_line } else { row.new_line };
                side.child(line_number(number, cx)).child(line_text(row))
            })
    };
    h_flex()
        .w_full()
        .items_stretch()
        .child(side(old, true))
        .child(side(new, false))
        .into_any_element()
}

fn line_number(number: Option<u32>, cx: &App) -> AnyElement {
    div()
        .flex_none()
        .w(rems(2.75))
        .pr_1()
        .text_right()
        .text_color(cx.theme().muted_foreground.opacity(0.7))
        .children(number.map(|number| number.to_string()))
        .into_any_element()
}

fn line_text(row: &Row) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .pl_1()
        .pr_2()
        .whitespace_normal()
        .child(if row.text.is_empty() {
            SharedString::new_static(" ")
        } else {
            SharedString::from(&row.text)
        })
        .into_any_element()
}

fn hunk_row(row: &Row, cx: &App) -> AnyElement {
    div()
        .px_2()
        .py_0p5()
        .bg(theme::diff_colors(cx).hunk)
        .text_color(cx.theme().muted_foreground)
        .text_xs()
        .child(SharedString::from(&row.text))
        .into_any_element()
}

fn note_row(row: &Row, cx: &App) -> AnyElement {
    div()
        .px_2()
        .py_0p5()
        .text_xs()
        .italic()
        .text_color(cx.theme().muted_foreground)
        .child(SharedString::from(&row.text))
        .into_any_element()
}

fn file_row(row: &Row, header: &FileHeader, ix: usize, cx: &App) -> AnyElement {
    let colors = theme::diff_colors(cx);
    let path = row.text.as_ref();
    let (directory, name) = match path.rfind('/') {
        Some(split) => (&path[..=split], &path[split + 1..]),
        None => ("", path),
    };
    let tag = match &header.change {
        FileChange::Added => Some(Tag::success().child("Added")),
        FileChange::Deleted => Some(Tag::danger().child("Deleted")),
        FileChange::Renamed { .. } => Some(Tag::info().child("Renamed")),
        FileChange::Modified => None,
    };
    let from = match &header.change {
        FileChange::Renamed { from } => Some(format!("from {from}")),
        _ => None,
    };
    v_flex()
        .when(ix > 0, |row| row.mt_3())
        .px_2()
        .py_1()
        .gap_0p5()
        .bg(colors.file)
        .border_y_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex_shrink(1.)
                                .min_w_0()
                                .text_ellipsis()
                                .text_color(cx.theme().muted_foreground)
                                .child(directory.to_owned()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child(name.to_owned()),
                        ),
                )
                .children(tag.map(|tag| tag.xsmall()))
                .when(header.binary, |line| {
                    line.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Binary"),
                    )
                })
                .when(header.additions > 0, |line| {
                    line.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(colors.added_sign)
                            .child(format!("+{}", header.additions)),
                    )
                })
                .when(header.deletions > 0, |line| {
                    line.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(colors.removed_sign)
                            .child(format!("−{}", header.deletions)),
                    )
                }),
        )
        .children(from.map(|from| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .text_ellipsis()
                .child(from)
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Motion, RowList};
    use crate::{app::ChangesLayout, diff};
    use gpui_kit::{ListOffset, px};
    use std::sync::Arc;

    fn patch(lines: usize) -> Arc<diff::Patch> {
        let mut text = String::from("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n");
        for ix in 0..lines {
            text.push_str(&format!("+line {ix}\n"));
        }
        Arc::new(diff::parse(&text))
    }

    #[test]
    fn a_new_patch_keeps_the_reader_on_a_clamped_row() {
        let mut list = RowList::new();
        let long = patch(20);
        list.sync(&long, ChangesLayout::Inline);
        assert_eq!(list.list.item_count(), 22);
        list.list.scroll_to(ListOffset {
            item_ix: 15,
            offset_in_item: px(4.),
        });
        list.selected = Some(16);
        list.sync(&long, ChangesLayout::Inline);
        assert_eq!(list.list.logical_scroll_top().item_ix, 15);

        list.sync(&patch(30), ChangesLayout::Inline);
        assert_eq!(list.list.item_count(), 32);
        assert_eq!(list.list.logical_scroll_top().item_ix, 15);
        assert_eq!(list.list.logical_scroll_top().offset_in_item, px(4.));
        assert_eq!(list.selected, Some(16));

        list.sync(&patch(3), ChangesLayout::Inline);
        assert_eq!(list.list.item_count(), 5);
        assert_eq!(list.list.logical_scroll_top().item_ix, 4);
        assert_eq!(list.list.logical_scroll_top().offset_in_item, px(0.));
        assert_eq!(list.selected, Some(4), "the cursor clamps to the last row");

        list.sync(&Arc::new(diff::parse("")), ChangesLayout::Inline);
        assert_eq!(list.selected, None, "no rows leave no cursor");
    }

    #[test]
    fn switching_layout_keeps_the_cursor_on_its_line() {
        let rows = Arc::new(diff::parse(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,3 +1,3 @@\n keep\n-old 1\n-old 2\n+new 1\n+new 2\n tail\n",
        ));
        let mut list = RowList::new();
        list.sync(&rows, ChangesLayout::Inline);
        assert_eq!(list.list.item_count(), 8);
        list.selected = Some(6);
        list.sync(&rows, ChangesLayout::SideBySide);
        assert_eq!(list.list.item_count(), 6);
        assert_eq!(list.selected, Some(4));
        assert_eq!(
            list.location().unwrap().map(|location| location.line),
            Ok(3),
            "a pair opens on its new side"
        );
        list.sync(&rows, ChangesLayout::Inline);
        assert_eq!(list.selected, Some(4), "back inline, the pair's first row");
        assert_eq!(
            list.location().unwrap().map(|location| location.line),
            Ok(2)
        );
    }

    #[test]
    fn motions_move_the_cursor_within_the_rows() {
        let mut list = RowList::new();
        list.select(Motion::Line(1), 10);
        assert_eq!(list.selected, None, "an empty list has no cursor");
        assert!(list.location().is_none());

        list.sync(&patch(20), ChangesLayout::Inline);
        list.select(Motion::Line(-1), 10);
        assert_eq!(
            list.selected,
            Some(21),
            "up without a cursor starts at the end"
        );
        list.select(Motion::First, 10);
        assert_eq!(list.selected, Some(0));
        list.select(Motion::Line(-1), 10);
        assert_eq!(list.selected, Some(0), "never wraps");
        list.select(Motion::Page(1), 10);
        assert_eq!(list.selected, Some(10));
        list.select(Motion::Last, 10);
        list.select(Motion::Line(1), 10);
        assert_eq!(list.selected, Some(21));
        assert_eq!(
            list.location()
                .unwrap()
                .map(|location| (location.path, location.line)),
            Ok(("a".to_owned(), 20))
        );
    }
}
