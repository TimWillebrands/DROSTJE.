//! Projects list cursor and list or scroll motions inside the focused pane.

use super::*;

/// Where the keyboard cursor lands after filtering: the first thread, else the first row.
pub(super) fn first_thread_row(rows: &[Row]) -> Option<Row> {
    rows.iter()
        .copied()
        .find(|row| matches!(row, Row::Thread(_)))
        .or_else(|| rows.first().copied())
}

impl Workspace {
    /// Anchor Projects keyboard navigation to the current selection before focus moves in.
    /// Without a cursor the pane receives focus but draws no focus indication, so the
    /// first `j`/`k` starts from an unrelated row.
    pub(super) fn prepare_projects_cursor(&mut self, cx: &mut Context<Self>) {
        let query = self.thread_search.read(cx).value();
        let (rows, selected) = {
            let state = self.state.read(cx);
            let rows = projects::rows(state, &query);
            let selected = state
                .selected_thread
                .map(Row::Thread)
                .filter(|row| rows.contains(row))
                .or_else(|| {
                    state
                        .selected_project
                        .map(Row::Project)
                        .filter(|row| rows.contains(row))
                });
            (rows, selected)
        };
        let target = selected
            .or_else(|| self.cursor.filter(|row| rows.contains(row)))
            .or_else(|| first_thread_row(&rows));
        self.cursor = target;
        if let Some(index) = rows.iter().position(|row| Some(*row) == target) {
            self.projects.read(cx).scroll.scroll_to_item(index);
        }
        self.projects.update(cx, |_, cx| cx.notify());
    }

    pub(in crate::ui) fn set_sidebar_row_hovered(
        &mut self,
        row: Row,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let next = if hovered {
            Some(row)
        } else if self.hovered_sidebar_row == Some(row) {
            None
        } else {
            return;
        };
        if self.hovered_sidebar_row != next {
            self.hovered_sidebar_row = next;
            self.projects.update(cx, |_, cx| cx.notify());
        }
    }
    pub fn select_row(
        &mut self,
        row: Row,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let member = match row {
            Row::Project(id) => self.state.read(cx).project_in_active_space(id),
            Row::Thread(id) => self.state.read(cx).thread_in_active_space(id),
        };
        if !member || self.preparing_space_change {
            return;
        }
        self.cursor = Some(row);
        self.state.update(cx, |state, cx| {
            match row {
                Row::Project(id) => state.select_project(id),
                Row::Thread(id) if open => state.select_thread(id),
                _ => {}
            }
            cx.emit(PresentationChange::Selection);
        });
        if let Row::Thread(id) = row
            && open
        {
            // Reopening the selected thread shows its tab even when another tab
            // in its group is displayed.
            self.unrevealed_thread = Some(id);
            self.sync_selection_panels(window, cx);
        }
        self.focus_region(Region::Projects, window, cx);
    }
    pub(super) fn navigate(&mut self, motion: Motion, window: &mut Window, cx: &mut Context<Self>) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        let region = self.focused_region(window, cx);
        if region == Region::Thread
            && let Some(transcript) = self
                .focused_panel(window, cx)
                .and_then(|panel| panel.read(cx).transcript.clone())
        {
            transcript.update(cx, |transcript, cx| transcript.scroll(motion, window, cx));
            return;
        }
        if region != Region::Projects {
            let Some(panel) = self.focused_panel(window, cx) else {
                return;
            };
            let scroll = panel.read(cx).scroll.clone();
            let scroll_by = |distance: Pixels| {
                let offset = scroll.offset();
                scroll.set_offset(point(offset.x, (offset.y - distance).min(px(0.))));
            };
            match motion {
                Motion::First => scroll.set_offset(point(px(0.), px(0.))),
                Motion::Last => scroll.scroll_to_bottom(),
                Motion::Line(delta) => scroll_by(window.rem_size() * (delta as f32 * 2.25)),
                Motion::Page(delta) => {
                    scroll_by(scroll.bounds().size.height * (delta as f32 * PAGE_SHARE))
                }
            }
            panel.update(cx, |_, cx| cx.notify());
            return;
        }
        let query = self.thread_search.read(cx).value();
        let rows = projects::rows(self.state.read(cx), &query);
        if rows.is_empty() {
            return;
        }
        let index = rows
            .iter()
            .position(|row| Some(*row) == self.cursor)
            .unwrap_or(0);
        let last = rows.len() - 1;
        let step = |delta: isize| (index as isize + delta).clamp(0, last as isize) as usize;
        let next = match motion {
            Motion::First => 0,
            Motion::Last => last,
            Motion::Line(delta) => step(delta),
            Motion::Page(delta) => {
                let scroll = &self.projects.read(cx).scroll;
                // Rows share one height; an unmeasured list falls back to a short page.
                let page = scroll
                    .bounds_for_item(index)
                    .filter(|row| row.size.height > px(0.))
                    .map_or(10, |row| {
                        (scroll.bounds().size.height * PAGE_SHARE / row.size.height) as isize
                    });
                step(delta * page.max(1))
            }
        };
        self.cursor = rows.get(next).copied();
        self.projects.read(cx).scroll.scroll_to_item(next);
        self.projects.update(cx, |_, cx| cx.notify());
    }
    /// Return focus from the thread filter to the Projects list, optionally clearing
    /// the filter, with the cursor on the first matching thread.
    pub fn leave_thread_search(
        &mut self,
        clear: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if clear {
            self.thread_search
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        let query = self.thread_search.read(cx).value();
        let rows = projects::rows(self.state.read(cx), &query);
        if !clear || !self.cursor.is_some_and(|cursor| rows.contains(&cursor)) {
            self.cursor = first_thread_row(&rows);
        }
        if let Some(index) = rows.iter().position(|row| Some(*row) == self.cursor) {
            self.projects.read(cx).scroll.scroll_to_item(index);
        }
        let focus = self.projects.read(cx).focus.clone();
        focus.focus(window, cx);
        self.projects.update(cx, |_, cx| cx.notify());
    }
    pub(super) fn fold(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        let id = match self.cursor {
            Some(Row::Project(id)) => Some(id),
            Some(Row::Thread(id)) => self
                .state
                .read(cx)
                .snapshot
                .threads
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.project_id),
            None => self.state.read(cx).selected_project,
        };
        if let Some(id) = id {
            self.state.update(cx, |state, cx| {
                state.set_project_collapsed(id, collapsed);
                cx.emit(PresentationChange::Projects);
            });
            self.cursor = Some(Row::Project(id));
        }
        self.projects.update(cx, |_, cx| cx.notify());
    }
    pub(super) fn open_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.navigation_blocked(window, cx) {
            return;
        }
        if self.focused_region(window, cx) != Region::Projects {
            return;
        }
        if let Some(row) = self.cursor {
            self.select_row(row, true, window, cx);
            if matches!(row, Row::Thread(_)) {
                self.focus_region(Region::Thread, window, cx);
            }
        }
    }
}
