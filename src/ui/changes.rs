//! The Changes tab of a Conversation's sidebar: its working tree's uncommitted diff as
//! coloured, virtualised rows, and the entry point to commit them.
//!
//! AppState owns the diff (`ProjectDiffState`); rows are parsed off-thread with it. This module
//! owns only the per-thread `ListState`, kept in the Workspace so scroll survives re-renders.

use super::{
    commands::{Command, button},
    settings::{self, DialogKind},
    theme,
    workspace::Workspace,
};
use crate::{
    app::AppState,
    diff::{FileChange, FileHeader, Patch, Row, RowKind},
    model::ThreadId,
};
use gpui_kit::{
    component::{ActiveTheme, Sizable, alert::Alert, h_flex, tag::Tag, v_flex},
    prelude::FluentBuilder,
    *,
};
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

/// Row lists by thread: two panes on one working tree scroll independently.
pub(super) type ChangesLists = Rc<RefCell<HashMap<ThreadId, RowList>>>;

pub(super) struct RowList {
    list: ListState,
    rows: Arc<Patch>,
}

/// The list for `id` showing `rows`. A new patch keeps the reader at the same row, clamped,
/// so a refresh while the agent works does not jump to the top.
fn row_list(lists: &ChangesLists, id: ThreadId, rows: &Arc<Patch>) -> ListState {
    let count = rows.rows.len();
    let mut lists = lists.borrow_mut();
    let entry = lists.entry(id).or_insert_with(|| RowList {
        list: ListState::new(count, ListAlignment::Top, px(400.)),
        rows: rows.clone(),
    });
    if !Arc::ptr_eq(&entry.rows, rows) {
        let top = entry.list.logical_scroll_top();
        entry.list.reset(count);
        entry.list.scroll_to(ListOffset {
            item_ix: top.item_ix.min(count.saturating_sub(1)),
            offset_in_item: if top.item_ix < count {
                top.offset_in_item
            } else {
                px(0.)
            },
        });
        entry.rows = rows.clone();
    }
    entry.list.clone()
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
            let lists = owner
                .upgrade()
                .map(|owner| owner.read(cx).changes_lists.clone())
                .unwrap_or_default();
            let rows = diff.rows.clone();
            let list_state = row_list(&lists, id, &rows);
            div()
                .id("conversation-diff-patch")
                .debug_selector(|| "conversation-diff-patch".into())
                .flex_1()
                .min_h_0()
                .pb_2()
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(cx.theme().mono_font_size)
                .child(
                    list(list_state, move |ix, _, cx| {
                        rows.rows
                            .get(ix)
                            .map(|row| render_row(row, ix, cx))
                            .unwrap_or_else(|| div().into_any_element())
                    })
                    .size_full(),
                )
                .into_any_element()
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
                .child(div().min_w_0().child(summary))
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

fn render_row(row: &Row, ix: usize, cx: &App) -> AnyElement {
    let colors = theme::diff_colors(cx);
    let muted = cx.theme().muted_foreground;
    match &row.kind {
        RowKind::File(header) => file_row(row, header, ix, cx),
        RowKind::Hunk => div()
            .px_2()
            .py_0p5()
            .bg(colors.hunk)
            .text_color(muted)
            .text_xs()
            .child(row.text.clone())
            .into_any_element(),
        RowKind::Note => div()
            .px_2()
            .py_0p5()
            .text_xs()
            .italic()
            .text_color(muted)
            .child(row.text.clone())
            .into_any_element(),
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
                .child(
                    div()
                        .flex_none()
                        .w(rems(2.75))
                        .pr_1()
                        .text_right()
                        .text_color(muted.opacity(0.7))
                        .children(number.map(|number| number.to_string())),
                )
                .child(
                    div()
                        .flex_none()
                        .w(rems(1.))
                        .text_center()
                        .text_color(sign_color)
                        .child(sign),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pr_2()
                        .whitespace_normal()
                        // An empty line still takes a line's height.
                        .child(if row.text.is_empty() {
                            SharedString::new_static(" ")
                        } else {
                            row.text.clone()
                        }),
                )
                .into_any_element()
        }
    }
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
    use super::{ChangesLists, row_list};
    use crate::diff;
    use gpui_kit::{ListOffset, px};
    use std::sync::Arc;
    use uuid::Uuid;

    fn patch(lines: usize) -> Arc<diff::Patch> {
        let mut text = String::from("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n");
        for ix in 0..lines {
            text.push_str(&format!("+line {ix}\n"));
        }
        Arc::new(diff::parse(&text))
    }

    #[test]
    fn a_new_patch_keeps_the_reader_on_a_clamped_row() {
        let lists = ChangesLists::default();
        let thread = Uuid::from_u128(1);
        let long = patch(20);
        let list = row_list(&lists, thread, &long);
        assert_eq!(list.item_count(), 22);
        list.scroll_to(ListOffset {
            item_ix: 15,
            offset_in_item: px(4.),
        });
        // The same patch leaves the list alone.
        assert_eq!(
            row_list(&lists, thread, &long).logical_scroll_top().item_ix,
            15
        );

        let longer = patch(30);
        let list = row_list(&lists, thread, &longer);
        assert_eq!(list.item_count(), 32);
        assert_eq!(list.logical_scroll_top().item_ix, 15);
        assert_eq!(list.logical_scroll_top().offset_in_item, px(4.));

        let short = patch(3);
        let list = row_list(&lists, thread, &short);
        assert_eq!(list.item_count(), 5);
        assert_eq!(list.logical_scroll_top().item_ix, 4);
        assert_eq!(list.logical_scroll_top().offset_in_item, px(0.));

        // Another pane on the same working tree scrolls on its own.
        let other = row_list(&lists, Uuid::from_u128(2), &short);
        assert_eq!(other.logical_scroll_top().item_ix, 0);
    }
}
