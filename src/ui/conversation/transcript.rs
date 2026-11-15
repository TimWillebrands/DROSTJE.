use super::{
    images::thumbnail_cache,
    markdown::{Markdown, MarkdownStates},
    memo::Memo,
    rows::message,
    sticky_dock::DockCache,
    tool_runs::RunCache,
    tools::artifact_rows,
};
use crate::ui::{
    commands::SelectLast,
    workspace::{Motion, PAGE_SHARE, Workspace},
};
use crate::{
    app::{AppState, ConversationSession},
    model::ThreadId,
};
use gpui_kit::{
    component::{
        ActiveTheme, IconName, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        scroll::ScrollableElement,
    },
    prelude::FluentBuilder,
    *,
};
use std::{cell::Cell, rc::Rc};

/// A disclosure in one Conversation's transcript or composer band opened or closed: that
/// transcript's rows may change height, no other's.
pub(in crate::ui) struct TranscriptDisclosure {
    pub(super) thread: ThreadId,
}
impl EventEmitter<TranscriptDisclosure> for Workspace {}

/// Owns transcript layout independently of drafts, forms and the workspace shell.
///
/// A row reads its message and the session's tool activity (both in the session's change
/// journal), its disclosures (`TranscriptDisclosure`), whether the thread is working (a call
/// without a result runs or has none), where each artifact shows, and the theme. Only a
/// change of one of these renders or remeasures rows; other app state does not.
pub(in crate::ui) struct TranscriptView {
    id: ThreadId,
    state: Entity<AppState>,
    owner: WeakEntity<Workspace>,
    list: ListState,
    generation: Option<uuid::Uuid>,
    revision: u64,
    count: usize,
    invalidate_rows: bool,
    /// Whether the thread was working when the rows were last measured.
    active: Option<bool>,
    /// An artifact may have moved, resized, collapsed or closed: its rows remeasure.
    remeasure_artifacts: bool,
    /// List width at which unmeasured rows last got an estimated height; `None` once rows
    /// without one were added.
    estimated_width: Rc<Cell<Option<Pixels>>>,
    markdown: MarkdownStates,
    runs: RunCache,
    artifact_rows: Memo<(uuid::Uuid, u64), Vec<usize>>,
    pub(super) dock: DockCache,
    /// Thumbnails of this transcript and its draft; freed with it.
    pub(super) images: Entity<RetainAllImageCache>,
    _subscriptions: Vec<Subscription>,
}

impl TranscriptView {
    #[cfg(feature = "runtime-qa")]
    pub(in crate::ui) fn scroll_extent(&self) -> f32 {
        f32::from(self.list.max_offset_for_scrollbar().y)
    }
    pub(in crate::ui) fn new(
        id: ThreadId,
        state: Entity<AppState>,
        owner: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![
            cx.subscribe(
                &state,
                move |this, _, event: &crate::app::PresentationChange, cx| match event {
                    crate::app::PresentationChange::Transcript(thread) if *thread == id => {
                        cx.notify()
                    }
                    // Artifacts report placement and layout changes as thread changes.
                    crate::app::PresentationChange::Thread(thread) if *thread == id => {
                        if this.has_artifacts(cx) {
                            this.remeasure_artifacts = true;
                            cx.notify();
                        } else if this.stale(cx) {
                            cx.notify();
                        }
                    }
                    _ => {}
                },
            ),
            // Any other app change reaches rows only through the session or the activity.
            cx.observe(&state, |this, _, cx| {
                if this.stale(cx) {
                    cx.notify();
                }
            }),
            cx.observe_global::<gpui_kit::component::Theme>(|this, cx| {
                this.invalidate_rows = true;
                cx.notify();
            }),
        ];
        if let Some(workspace) = owner.upgrade() {
            subscriptions.push(cx.subscribe(
                &workspace,
                move |this, _, event: &TranscriptDisclosure, cx| {
                    if event.thread == id {
                        this.invalidate_rows = true;
                        cx.notify();
                    }
                },
            ));
        }
        let list = ListState::new(0, ListAlignment::Bottom, px(500.));
        list.set_follow_mode(FollowMode::Tail);
        // The jump button depends on scroll position; defer so the list's borrow is released.
        let view = cx.weak_entity();
        list.set_scroll_handler(move |_, _, cx| {
            let view = view.clone();
            cx.defer(move |cx| {
                let _ = view.update(cx, |_, cx| cx.notify());
            });
        });
        Self {
            id,
            state,
            owner,
            markdown: MarkdownStates::new(id, list.clone(), cx.entity_id()),
            list,
            generation: None,
            revision: 0,
            count: 0,
            invalidate_rows: false,
            active: None,
            remeasure_artifacts: false,
            estimated_width: Rc::default(),
            runs: RunCache::default(),
            artifact_rows: Memo::default(),
            dock: DockCache::default(),
            images: thumbnail_cache(cx),
            _subscriptions: subscriptions,
        }
    }

    /// Whether the session or the thread's activity moved on since the rows last rendered.
    fn stale(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        let session = state
            .conversations
            .get(&self.id)
            .map(|session| (session.generation, session.transcript_revision));
        let synced = self
            .generation
            .map(|generation| (generation, self.revision));
        session != synced
            || self
                .active
                .is_some_and(|active| active != state.active_threads.contains(&self.id))
    }

    fn has_artifacts(&self, cx: &App) -> bool {
        self.state
            .read(cx)
            .conversations
            .get(&self.id)
            .is_some_and(|session| !self.artifact_rows(session).is_empty())
    }

    /// Rows holding an artifact call, which show the artifact's pane or its docked stub.
    fn artifact_rows(&self, session: &ConversationSession) -> Rc<Vec<usize>> {
        self.artifact_rows
            .get((session.generation, session.transcript_revision), || {
                artifact_rows(session)
            })
    }

    fn sync(&mut self, cx: &App) {
        let state = self.state.read(cx);
        // Calls without a result read as running while the thread works, else as unknown.
        let active = state.active_threads.contains(&self.id);
        if self.active.is_some_and(|rendered| rendered != active) {
            self.invalidate_rows = true;
        }
        self.active = Some(active);
        let Some(session) = state.conversations.get(&self.id) else {
            if self.count != 0 {
                self.list.reset(0);
                self.markdown.clear();
                self.count = 0;
            }
            self.generation = None;
            self.invalidate_rows = false;
            self.remeasure_artifacts = false;
            return;
        };
        let count = session.messages.len();
        let changes = if self.generation != Some(session.generation) {
            RowChanges::Reset
        } else {
            match session.transcript_changes_since(self.revision) {
                None => RowChanges::Reset,
                Some(_) if self.invalidate_rows => RowChanges::All,
                Some(changes) => {
                    let mut rows: Vec<usize> = changes.collect();
                    if self.remeasure_artifacts {
                        rows.extend(self.artifact_rows(session).iter().copied());
                    }
                    RowChanges::Rows(rows)
                }
            }
        };
        if matches!(changes, RowChanges::Reset) {
            self.markdown.clear();
        }
        // A changed or new message can join or split a run of tool calls, which changes the
        // height of that run's other rows.
        let mut anchors = match &changes {
            RowChanges::Rows(rows) => rows.clone(),
            RowChanges::Reset | RowChanges::All => Vec::new(),
        };
        if count > self.count && !matches!(changes, RowChanges::Reset) {
            anchors.push(self.count);
        }
        update_rows(
            &self.list,
            &self.estimated_width,
            self.count,
            count,
            changes,
        );
        let kept = self.count.min(count);
        for anchor in anchors {
            let start = anchor
                .checked_sub(1)
                .and_then(|before| self.runs.run(session, before))
                .map_or(anchor, |run| run.head);
            let end = (anchor + 1 < count)
                .then(|| self.runs.run(session, anchor + 1))
                .flatten()
                .map_or(anchor + 1, |run| run.end);
            let rows = start.min(kept)..end.min(kept);
            if !rows.is_empty() {
                self.list.remeasure_items(rows);
            }
        }
        self.generation = Some(session.generation);
        self.revision = session.transcript_revision;
        self.count = count;
        self.invalidate_rows = false;
        self.remeasure_artifacts = false;
    }

    pub(in crate::ui) fn scroll(
        &mut self,
        motion: Motion,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.sync(cx);
        scroll_list(&self.list, motion, window.rem_size() * 2.25);
        cx.notify();
    }
}

/// What changed in a transcript since its list last rendered.
enum RowChanges {
    /// A new session, or a gap in the change journal: nothing known about the old rows holds.
    Reset,
    /// Any row may have changed height: the thread started or stopped working, one of its
    /// disclosures or the theme changed.
    All,
    /// These rows changed in place.
    Rows(Vec<usize>),
}

/// Brings `list` from `old` rows to `count`. Changed rows are remeasured in place: a splice
/// would move a reader inside a replaced row to its top and forget the row's height, so the
/// reader would jump and the scrollbar would shrink to the rows in view.
fn update_rows(
    list: &ListState,
    estimated_width: &Cell<Option<Pixels>>,
    old: usize,
    count: usize,
    changes: RowChanges,
) {
    let kept = old.min(count);
    if count > old || matches!(changes, RowChanges::Reset) {
        // New rows have no height until `estimate_unmeasured_rows` runs again.
        estimated_width.set(None);
    }
    match changes {
        RowChanges::Reset => return list.reset(count),
        RowChanges::All => list.remeasure_items(0..kept),
        RowChanges::Rows(mut rows) => {
            rows.retain(|row| *row < kept);
            rows.sort_unstable();
            rows.dedup();
            for row in rows {
                list.remeasure_items(row..row + 1);
            }
        }
    }
    if count != old {
        list.splice(kept..old, count - kept);
    }
}

/// Rows the list has not laid out count as a short message until they are measured.
const ROW_ESTIMATE: f32 = 3.;

/// Gives rows the list has not laid out an estimated height, so the scrollbar spans the whole
/// transcript and a drag lands proportionally instead of skipping rows that count as zero.
/// The list forgets every row height when its width changes; this runs after the list's
/// prepaint and before the scrollbar's, so the thumb is right in the same frame.
pub(in crate::ui) fn estimate_unmeasured_rows(
    list: &ListState,
    estimated_width: &Rc<Cell<Option<Pixels>>>,
    row: Pixels,
) -> impl IntoElement {
    let list = list.clone();
    let estimated_width = estimated_width.clone();
    canvas(
        move |_, _, _| {
            let width = list.viewport_bounds().size.width;
            if estimated_width.replace(Some(width)) != Some(width) {
                // Measured rows keep their height; only unknown ones take the estimate.
                list.with_uniform_item_height(row);
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_0()
}

fn scroll_list(list: &ListState, motion: Motion, line: Pixels) {
    match motion {
        // `scroll_to` pauses tail following; revealing an item would snap back to the end.
        Motion::First => list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        }),
        Motion::Last => scroll_to_latest(list),
        Motion::Line(delta) => list.scroll_by(line * delta as f32),
        Motion::Page(delta) => {
            list.scroll_by(list.viewport_bounds().size.height * (delta as f32 * PAGE_SHARE))
        }
    }
}

/// Returns to the latest message and resumes following new output.
fn scroll_to_latest(list: &ListState) {
    list.set_follow_mode(FollowMode::Tail);
    list.scroll_to_end();
}

/// Unmeasured rows leave the end unknown; a list that stopped following is then away from it.
fn scrolled_up(list: &ListState) -> bool {
    list.max_offset_for_scrollbar().y > px(0.)
        && !list.is_following_tail()
        && !list.is_scrolled_to_end().unwrap_or(false)
}

impl Render for TranscriptView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(cx);
        self.markdown.begin_render(cx);
        let state = self.state.clone();
        let owner = self.owner.clone();
        let markdown = self.markdown.clone();
        let runs = self.runs.clone();
        let id = self.id;
        let scrolled_up = scrolled_up(&self.list);
        let rows = list(self.list.clone(), move |index, _, cx| {
            let state = state.read(cx);
            let Some(session) = state.conversations.get(&id) else {
                return div().into_any_element();
            };
            session
                .messages
                .get(index)
                .cloned()
                .map(|current| {
                    message(
                        current,
                        index,
                        id,
                        session,
                        state.active_threads.contains(&id),
                        &owner,
                        Markdown::Row(&markdown, index),
                        &runs,
                        cx,
                    )
                })
                .unwrap_or_else(|| div().into_any_element())
        })
        .size_full();
        div()
            .image_cache(self.images.clone())
            .id("transcript")
            .relative()
            .size_full()
            .child(rows)
            .child(estimate_unmeasured_rows(
                &self.list,
                &self.estimated_width,
                window.rem_size() * ROW_ESTIMATE,
            ))
            .vertical_scrollbar(&self.list)
            .when(scrolled_up, |this| {
                this.child(
                    h_flex()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_2()
                        .justify_center()
                        .child(
                            Button::new("transcript-to-latest")
                                .small()
                                .secondary()
                                .rounded(cx.theme().radius_full())
                                .icon(IconName::ArrowDown)
                                .tooltip_with_action(
                                    "Scroll to latest",
                                    &SelectLast,
                                    Some("Thread"),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    scroll_to_latest(&this.list);
                                    cx.notify();
                                })),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn only_this_threads_session_and_activity_render_the_transcript(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use super::TranscriptView;
        use crate::app::{AppState, Bootstrap, PresentationChange};
        use gpui_kit::{AppContext, WeakEntity, component::Root};
        use std::{cell::Cell, rc::Rc};
        use uuid::Uuid;

        cx.dispatcher.allow_parking();
        let data_dir =
            std::env::temp_dir().join(format!("drostje-transcript-qa-{}", Uuid::new_v4()));
        std::fs::create_dir(&data_dir).unwrap();
        let bootstrap = Bootstrap {
            data_dir: data_dir.clone(),
            store: crate::store::Store::open(data_dir.clone()).unwrap(),
            snapshot: Default::default(),
            diagnostics: Default::default(),
            repositories: Default::default(),
            browser_tools: None,
        };
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, Some(&data_dir), cx);
        });
        let state = cx.new(|cx| AppState::new(bootstrap, cx));
        let (id, other) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let transcript =
                cx.new(|cx| TranscriptView::new(id, state.clone(), WeakEntity::new_invalid(), cx));
            view = Some(transcript.clone());
            Root::new(transcript, window, cx)
        });
        let view = view.unwrap();
        let draw = |cx: &mut gpui_kit::TestAppContext| {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
            })
            .unwrap();
        };
        draw(cx);
        let renders = Rc::new(Cell::new(0));
        cx.update(|cx| {
            let renders = renders.clone();
            cx.observe(&view, move |_, _| renders.set(renders.get() + 1))
                .detach();
        });

        // App changes rows do not read: another thread, this thread's chrome, a bare notify.
        state.update(cx, |_, cx| {
            cx.emit(PresentationChange::Thread(other));
            cx.emit(PresentationChange::Transcript(other));
            cx.emit(PresentationChange::Thread(id));
            cx.notify();
        });
        draw(cx);
        assert_eq!(renders.get(), 0, "nothing the rows show changed");

        // The thread starts working: calls without a result now read as running.
        state.update(cx, |state, cx| {
            state.active_threads.insert(id);
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(renders.get(), 1);
        draw(cx);
        assert_eq!(cx.read(|cx| view.read(cx).active), Some(true));

        // The theme changes every row.
        cx.update(|cx| {
            let settings = std::collections::BTreeMap::from([(
                "theme_mode".to_owned(),
                serde_json::json!("dark"),
            )]);
            crate::ui::theme::initialize(Some(&settings), Some(&data_dir), cx);
        });
        cx.run_until_parked();
        assert!(renders.get() >= 2);
        draw(cx);
        drop(view);
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn transcript_leaves_and_rejoins_the_latest_message(cx: &mut gpui_kit::TestAppContext) {
        use super::{Motion, scroll_list, scrolled_up};
        use gpui_kit::{
            AppContext, Context, FollowMode, IntoElement, ListAlignment, ListState, Render, Styled,
            Window, component::Root, div, list, px,
        };

        struct Rows(ListState);
        impl Render for Rows {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                list(self.0.clone(), |_, _, _| {
                    div().h(px(40.)).w_full().into_any_element()
                })
                .size_full()
            }
        }

        cx.update(gpui_kit::init);
        let rows = ListState::new(200, ListAlignment::Bottom, px(500.));
        rows.set_follow_mode(FollowMode::Tail);
        let window = cx.add_window({
            let rows = rows.clone();
            |window, cx| Root::new(cx.new(|_| Rows(rows)), window, cx)
        });
        let draw = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
        };
        draw(cx);
        assert!(!scrolled_up(&rows));

        // `gg` must stay at the top instead of being pulled back by tail following.
        scroll_list(&rows, Motion::First, px(36.));
        draw(cx);
        assert!(scrolled_up(&rows));
        assert_eq!(rows.logical_scroll_top().item_ix, 0);

        // A page is most of the viewport, in whole 40px rows here.
        let page = (rows.viewport_bounds().size.height * super::PAGE_SHARE / px(40.)) as usize;
        assert!(page > 1);
        scroll_list(&rows, Motion::Page(1), px(36.));
        draw(cx);
        assert_eq!(rows.logical_scroll_top().item_ix, page);
        scroll_list(&rows, Motion::Page(-1), px(36.));
        draw(cx);
        assert_eq!(rows.logical_scroll_top().item_ix, 0);

        rows.splice(200..200, 1);
        draw(cx);
        assert!(scrolled_up(&rows), "new output must not move a reader");

        scroll_list(&rows, Motion::Last, px(36.));
        draw(cx);
        assert!(!scrolled_up(&rows));
        assert!(rows.is_following_tail());
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn transcript_updates_keep_the_reader_and_the_scrollbar(cx: &mut gpui_kit::TestAppContext) {
        use super::{RowChanges, estimate_unmeasured_rows, update_rows};
        use gpui_kit::{
            AppContext, Context, FollowMode, IntoElement, ListAlignment, ListOffset, ListState,
            ParentElement, Pixels, Render, Styled, Window, component::Root, div, list, px, size,
        };
        use std::{cell::Cell, rc::Rc};

        const ROW: Pixels = px(40.);
        struct Rows(ListState, Rc<Cell<Option<Pixels>>>);
        impl Render for Rows {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .relative()
                    .size_full()
                    .child(
                        list(self.0.clone(), |_, _, _| {
                            div().h(ROW).w_full().into_any_element()
                        })
                        .size_full(),
                    )
                    // The estimate matches every row, so the scrollbar must span all of them.
                    .child(estimate_unmeasured_rows(&self.0, &self.1, ROW))
            }
        }

        cx.update(gpui_kit::init);
        let rows = ListState::new(0, ListAlignment::Bottom, px(500.));
        rows.set_follow_mode(FollowMode::Tail);
        let estimated = Rc::new(Cell::new(None));
        update_rows(&rows, &estimated, 0, 200, RowChanges::Reset);
        let window = cx.add_window({
            let rows = rows.clone();
            let estimated = estimated.clone();
            |window, cx| Root::new(cx.new(|_| Rows(rows, estimated)), window, cx)
        });
        let draw = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
        };
        let spans_all_rows = |rows: &ListState| {
            rows.max_offset_for_scrollbar().y
                == ROW * rows.item_count() as f32 - rows.viewport_bounds().size.height
        };
        let at_reader = |rows: &ListState| {
            let top = rows.logical_scroll_top();
            (top.item_ix, top.offset_in_item) == (100, px(15.))
        };

        draw(cx);
        assert!(
            spans_all_rows(&rows),
            "rows never laid out still count toward the scrollbar"
        );

        rows.scroll_to(ListOffset {
            item_ix: 100,
            offset_in_item: px(15.),
        });
        draw(cx);
        assert!(at_reader(&rows));

        for changes in [RowChanges::All, RowChanges::Rows(vec![101, 100, 101])] {
            update_rows(&rows, &estimated, 200, 200, changes);
            draw(cx);
            assert!(at_reader(&rows), "a changed row must not move the reader");
            assert!(
                spans_all_rows(&rows),
                "a changed row must keep known heights"
            );
        }

        update_rows(&rows, &estimated, 200, 203, RowChanges::Rows(vec![199]));
        draw(cx);
        assert!(at_reader(&rows), "new output must not move the reader");
        assert!(spans_all_rows(&rows), "new rows count toward the scrollbar");

        // The list forgets every measured height when its width changes.
        cx.simulate_window_resize(window.into(), size(px(640.), px(480.)));
        draw(cx);
        assert!(at_reader(&rows), "a resize must not move the reader");
        assert!(
            spans_all_rows(&rows),
            "a resize must not shrink the scrollbar to the rows in view"
        );
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn streaming_output_follows_only_a_reader_at_the_latest_message(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use super::{RowChanges, update_rows};
        use gpui_kit::{
            AppContext, Context, FollowMode, IntoElement, ListAlignment, ListState, Pixels,
            PlatformInput, Render, ScrollDelta, ScrollWheelEvent, Styled, Window, component::Root,
            div, list, point, px,
        };
        use std::{cell::Cell, rc::Rc};

        const ROW: Pixels = px(40.);
        // The newest row streams: its height grows while the others stay put.
        struct Rows(ListState, Rc<Cell<(usize, Pixels)>>);
        impl Render for Rows {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let (count, latest) = self.1.get();
                list(self.0.clone(), move |index, _, _| {
                    let height = if index + 1 == count { latest } else { ROW };
                    div().h(height).w_full().into_any_element()
                })
                .size_full()
            }
        }

        cx.update(gpui_kit::init);
        let rows = ListState::new(0, ListAlignment::Bottom, px(500.));
        rows.set_follow_mode(FollowMode::Tail);
        let estimated = Rc::new(Cell::new(None));
        let latest = Rc::new(Cell::new((50, ROW)));
        update_rows(&rows, &estimated, 0, 50, RowChanges::Reset);
        let window = cx.add_window({
            let (rows, latest) = (rows.clone(), latest.clone());
            |window, cx| Root::new(cx.new(|_| Rows(rows, latest)), window, cx)
        });
        let draw = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
        };
        let wheel = |cx: &mut gpui_kit::TestAppContext, up: Pixels| {
            cx.update_window(window.into(), |_, window, cx| {
                let size = window.viewport_size();
                window.dispatch_event(
                    PlatformInput::ScrollWheel(ScrollWheelEvent {
                        position: point(size.width / 2., size.height / 2.),
                        delta: ScrollDelta::Pixels(point(px(0.), up)),
                        ..Default::default()
                    }),
                    cx,
                );
            })
            .unwrap();
        };
        // Output streams into the newest row, then a new row starts, as during a turn.
        let stream = |cx: &mut gpui_kit::TestAppContext, count: usize| {
            latest.set((count, latest.get().1 + ROW * 3.));
            update_rows(
                &rows,
                &estimated,
                count,
                count,
                RowChanges::Rows(vec![count - 1]),
            );
            draw(cx);
            latest.set((count + 1, ROW));
            update_rows(&rows, &estimated, count, count + 1, RowChanges::All);
            draw(cx);
            count + 1
        };
        draw(cx);
        let count = stream(cx, 50);
        assert!(
            rows.is_following_tail(),
            "output follows a reader at the end"
        );

        wheel(cx, px(60.));
        draw(cx);
        assert!(!rows.is_following_tail());
        let top = |rows: &ListState| {
            let top = rows.logical_scroll_top();
            (top.item_ix, top.offset_in_item)
        };
        let reader = top(&rows);
        let count = stream(cx, count);
        let count = stream(cx, count);
        assert_eq!(
            top(&rows),
            reader,
            "output must not move a reader who scrolled back"
        );
        assert!(!rows.is_following_tail());

        wheel(cx, px(-10_000.));
        draw(cx);
        assert!(
            rows.is_following_tail(),
            "scrolling to the end rejoins the output"
        );
        stream(cx, count);
        assert!(rows.is_following_tail());
    }
}
