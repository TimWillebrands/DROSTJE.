use super::{
    activity,
    commands::{
        ClearQueuedMessages, Command, FocusComposerArtifact, MoveArtifactIntoConversation,
        SelectLast, SwitchHarness, UnarchiveThread, button,
    },
    job_notice::{self, JobKind, JobNotice, JobOutput, JobResult, JobStart},
    theme,
    todo_list::{self, TodoList, TodoStatus},
    workspace::{Motion, PAGE_SHARE, Workspace},
};
use crate::{
    app::{AppState, ConversationSession, ThreadActivity},
    file_refs::{self, FileReference},
    harness::{
        ContentBlock, ContextOrigin, ContextUsage, LimitWindow, Message, PeerMessage, UsageLimit,
        UserAnswer, UserRequest, UserRequestKind,
    },
    model::{ImageAttachment, ModelKey, ThreadId},
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{
        ActiveTheme, Disableable, Icon, IconName, Sizable,
        alert::Alert,
        attachment::{
            Attachment, AttachmentActions, AttachmentContent, AttachmentDescription,
            AttachmentMedia, AttachmentTitle,
        },
        button::{Button, ButtonVariants},
        collapsible::Collapsible,
        h_flex,
        input::Input,
        label::Label,
        menu::{DropdownMenu, PopupMenuItem},
        progress::ProgressCircle,
        scroll::ScrollableElement,
        searchable_list::SearchableListItem,
        select::{Select, SelectEvent, SelectState},
        spinner::Spinner,
        tag::Tag,
        text::{
            InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
            TextView, TextViewState, TextViewStyle, markdown_ast,
        },
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime},
};

pub(super) struct TranscriptDisclosure;
impl EventEmitter<TranscriptDisclosure> for Workspace {}

/// Owns transcript layout independently of drafts, forms and the workspace shell.
pub(super) struct TranscriptView {
    id: ThreadId,
    state: Entity<AppState>,
    owner: WeakEntity<Workspace>,
    list: ListState,
    generation: Option<uuid::Uuid>,
    revision: u64,
    count: usize,
    invalidate_rows: bool,
    /// List width at which unmeasured rows last got an estimated height; `None` once rows
    /// without one were added.
    estimated_width: Rc<Cell<Option<Pixels>>>,
    markdown: MarkdownStates,
    _subscriptions: Vec<Subscription>,
}

impl TranscriptView {
    #[cfg(feature = "runtime-qa")]
    pub(super) fn scroll_extent(&self) -> f32 {
        f32::from(self.list.max_offset_for_scrollbar().y)
    }
    pub(super) fn new(
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
                    crate::app::PresentationChange::Thread(thread) if *thread == id => {
                        this.invalidate_rows = true;
                        cx.notify();
                    }
                    _ => {}
                },
            ),
            cx.observe(&state, |this, _, cx| {
                this.invalidate_rows = true;
                cx.notify();
            }),
            cx.observe_global::<gpui_kit::component::Theme>(|this, cx| {
                this.invalidate_rows = true;
                cx.notify();
            }),
        ];
        if let Some(workspace) = owner.upgrade() {
            subscriptions.push(cx.subscribe(
                &workspace,
                |this, _, _: &TranscriptDisclosure, cx| {
                    this.invalidate_rows = true;
                    cx.notify();
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
            estimated_width: Rc::default(),
            _subscriptions: subscriptions,
        }
    }

    fn sync(&mut self, cx: &App) {
        let Some(session) = self.state.read(cx).conversations.get(&self.id) else {
            if self.count != 0 {
                self.list.reset(0);
                self.markdown.clear();
                self.count = 0;
            }
            self.generation = None;
            return;
        };
        let count = session.messages.len();
        let changes = if self.generation != Some(session.generation) {
            RowChanges::Reset
        } else {
            match session.transcript_changes_since(self.revision) {
                None => RowChanges::Reset,
                Some(_) if self.invalidate_rows => RowChanges::All,
                Some(changes) => RowChanges::Rows(changes.collect()),
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
        let part = |index: usize| run_part(&session.messages[index], session);
        for anchor in anchors {
            let start = anchor
                .checked_sub(1)
                .and_then(|before| tool_run(part, count, before))
                .map_or(anchor, |run| run.head);
            let end = (anchor + 1 < count)
                .then(|| tool_run(part, count, anchor + 1))
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
    }

    pub(super) fn scroll(&mut self, motion: Motion, window: &Window, cx: &mut Context<Self>) {
        self.sync(cx);
        scroll_list(&self.list, motion, window.rem_size() * 2.25);
        cx.notify();
    }
}

/// What changed in a transcript since its list last rendered.
enum RowChanges {
    /// A new session, or a gap in the change journal: nothing known about the old rows holds.
    Reset,
    /// Any row may have changed height: thread state, a disclosure or the theme changed.
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
fn estimate_unmeasured_rows(
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
        let state = self.state.clone();
        let owner = self.owner.clone();
        let markdown = self.markdown.clone();
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
                        cx,
                    )
                })
                .unwrap_or_else(|| div().into_any_element())
        })
        .size_full();
        div()
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

#[derive(Clone, PartialEq)]
struct ModelOption {
    key: ModelKey,
    label: SharedString,
    name: String,
}

impl SearchableListItem for ModelOption {
    type Value = ModelKey;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &ModelKey {
        &self.key
    }
}

pub(super) struct ModelPicker {
    thread_id: ThreadId,
    options: Vec<ModelOption>,
    selected: Option<ModelKey>,
    select: Entity<SelectState<Vec<ModelOption>>>,
    _subscription: Subscription,
}

/// `None` is the harness default effort.
#[derive(Clone, PartialEq)]
struct EffortOption {
    value: Option<String>,
    label: SharedString,
}

impl SearchableListItem for EffortOption {
    type Value = Option<String>;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Option<String> {
        &self.value
    }
}

pub(super) struct EffortPicker {
    thread_id: ThreadId,
    options: Vec<EffortOption>,
    selected: Option<String>,
    select: Entity<SelectState<Vec<EffortOption>>>,
    _subscription: Subscription,
}

/// Permission modes are launch flags of the thread's harness, listed by `harness::permission_modes`;
/// `None` is the harness default and an empty list hides the picker.
pub(super) struct PermissionModePicker {
    thread_id: ThreadId,
    options: Vec<EffortOption>,
    selected: Option<String>,
    select: Entity<SelectState<Vec<EffortOption>>>,
    _subscription: Subscription,
}

/// Effort levels come from the chosen model, else the harness default entry, else the first
/// model that lists any; an empty list hides the picker.
fn effort_levels(state: &AppState, thread: &crate::model::Thread) -> Vec<String> {
    let Some(session) = state.conversations.get(&thread.id) else {
        return Vec::new();
    };
    let selected = session.model.as_ref().or(thread.model.as_ref());
    session
        .models
        .iter()
        .find(|model| Some(&model.key) == selected)
        .or_else(|| {
            session
                .models
                .iter()
                .find(|model| model.key.id == "default")
        })
        .or_else(|| {
            session
                .models
                .iter()
                .find(|model| !model.efforts.is_empty())
        })
        .map(|model| model.efforts.clone())
        .unwrap_or_default()
}

impl Workspace {
    pub(super) fn sync_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(thread) = state.conversation_thread() else {
            self.model_picker = None;
            return;
        };
        let thread_id = thread.id;
        let session = state.conversations.get(&thread_id);
        let selected = session
            .and_then(|session| session.model.as_ref())
            .or(thread.model.as_ref());
        if self.model_picker.as_ref().is_some_and(|picker| {
            picker.thread_id == thread_id
                && picker.selected.as_ref() == selected
                && picker.options.len() == session.map_or(0, |session| session.models.len())
                && session
                    .into_iter()
                    .flat_map(|session| &session.models)
                    .zip(&picker.options)
                    .all(|(model, option)| model.key == option.key && model.name == option.name)
        }) {
            return;
        }
        let options: Vec<_> = session
            .into_iter()
            .flat_map(|session| &session.models)
            .map(|model| ModelOption {
                key: model.key.clone(),
                name: model.name.clone(),
                label: format!("{} · {}/{}", model.name, model.key.provider, model.key.id).into(),
            })
            .collect();
        let selected = selected.cloned();
        if let Some(picker) = self
            .model_picker
            .as_mut()
            .filter(|picker| picker.thread_id == thread_id)
        {
            if picker.options != options || picker.selected != selected {
                picker.select.update(cx, |select, cx| {
                    if picker.options != options {
                        select.set_items(options.clone(), window, cx);
                    }
                    if let Some(selected) = &selected {
                        select.set_selected_value(selected, window, cx);
                    } else {
                        select.set_selected_index(None, window, cx);
                    }
                });
                picker.options = options;
                picker.selected = selected;
            }
            return;
        }
        let select = cx.new(|cx| {
            let mut select = SelectState::new(options.clone(), None, window, cx);
            if let Some(selected) = &selected {
                select.set_selected_value(selected, window, cx);
            }
            select
        });
        let subscription = cx.subscribe_in(
            &select,
            window,
            move |workspace, _, event: &SelectEvent<Vec<ModelOption>>, _, cx| {
                let SelectEvent::Confirm(Some(key)) = event else {
                    return;
                };
                workspace.state.update(cx, |state, cx| {
                    state.select_model(thread_id, key.clone());
                    cx.notify();
                });
            },
        );
        self.model_picker = Some(ModelPicker {
            thread_id,
            options,
            selected,
            select,
            _subscription: subscription,
        });
    }

    pub(super) fn sync_effort_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(thread) = state.conversation_thread() else {
            self.effort_picker = None;
            return;
        };
        let thread_id = thread.id;
        let levels = effort_levels(state, thread);
        if levels.is_empty() {
            self.effort_picker = None;
            return;
        }
        let options: Vec<_> = std::iter::once(EffortOption {
            value: None,
            label: "Default effort".into(),
        })
        .chain(levels.into_iter().map(|level| EffortOption {
            label: level.clone().into(),
            value: Some(level),
        }))
        .collect();
        let selected = thread.effort.clone();
        if let Some(picker) = self
            .effort_picker
            .as_mut()
            .filter(|picker| picker.thread_id == thread_id)
        {
            if picker.options != options || picker.selected != selected {
                picker.select.update(cx, |select, cx| {
                    if picker.options != options {
                        select.set_items(options.clone(), window, cx);
                    }
                    select.set_selected_value(&selected, window, cx);
                });
                picker.options = options;
                picker.selected = selected;
            }
            return;
        }
        let select = cx.new(|cx| {
            let mut select = SelectState::new(options.clone(), None, window, cx);
            select.set_selected_value(&selected, window, cx);
            select
        });
        let subscription = cx.subscribe_in(
            &select,
            window,
            move |workspace, _, event: &SelectEvent<Vec<EffortOption>>, _, cx| {
                let SelectEvent::Confirm(Some(effort)) = event else {
                    return;
                };
                workspace.state.update(cx, |state, cx| {
                    state.select_effort(thread_id, effort.clone());
                    cx.notify();
                });
            },
        );
        self.effort_picker = Some(EffortPicker {
            thread_id,
            options,
            selected,
            select,
            _subscription: subscription,
        });
    }

    pub(super) fn sync_permission_mode_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.read(cx);
        let Some(thread) = state.conversation_thread() else {
            self.permission_mode_picker = None;
            return;
        };
        let thread_id = thread.id;
        let modes = crate::harness::permission_modes(&thread.harness);
        if modes.is_empty() {
            self.permission_mode_picker = None;
            return;
        }
        let options: Vec<_> = std::iter::once(EffortOption {
            value: None,
            label: "Default permissions".into(),
        })
        .chain(modes.iter().map(|(value, label)| EffortOption {
            value: Some((*value).to_owned()),
            label: (*label).into(),
        }))
        .collect();
        let selected = thread.permission_mode.clone();
        if let Some(picker) = self
            .permission_mode_picker
            .as_mut()
            .filter(|picker| picker.thread_id == thread_id)
        {
            if picker.options != options || picker.selected != selected {
                picker.select.update(cx, |select, cx| {
                    if picker.options != options {
                        select.set_items(options.clone(), window, cx);
                    }
                    select.set_selected_value(&selected, window, cx);
                });
                picker.options = options;
                picker.selected = selected;
            }
            return;
        }
        let select = cx.new(|cx| {
            let mut select = SelectState::new(options.clone(), None, window, cx);
            select.set_selected_value(&selected, window, cx);
            select
        });
        let subscription = cx.subscribe_in(
            &select,
            window,
            move |workspace, _, event: &SelectEvent<Vec<EffortOption>>, _, cx| {
                let SelectEvent::Confirm(Some(mode)) = event else {
                    return;
                };
                workspace.state.update(cx, |state, cx| {
                    state.select_permission_mode(thread_id, mode.clone());
                    cx.notify();
                });
            },
        );
        self.permission_mode_picker = Some(PermissionModePicker {
            thread_id,
            options,
            selected,
            select,
            _subscription: subscription,
        });
    }

    pub(super) fn toggle_thread_details(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_details(id, String::new(), window, cx);
    }
}

// A full-width row preserves the transcript's edge scrollbar; only its content is centered.
fn content_column() -> Div {
    div().w_full().max_w(rems(60.)).px_3()
}

/// Agent prose in the theme's prose type, as wide as the tool cards beside it; code
/// inside keeps the mono font.
fn prose(content: AnyElement, cx: &App) -> AnyElement {
    let prose = theme::prose(cx);
    div()
        .debug_selector(|| "transcript-prose".into())
        .w_full()
        .min_w_0()
        .when_some(prose.family, |prose, family| prose.font_family(family))
        .when_some(prose.size, |prose, size| prose.text_size(size))
        .when_some(prose.line_height, |prose, height| {
            prose.line_height(relative(height))
        })
        .child(content)
        .into_any_element()
}

/// Utilisation at or above this share of a window or context reads as a warning.
const USAGE_WARNING_PERCENT: f32 = 80.;

/// Context and plan-limit meters for the composer toolbar; absent data renders nothing.
fn usage_meters(
    context: Option<ContextUsage>,
    limits: &[UsageLimit],
    now: SystemTime,
    cx: &App,
) -> Vec<AnyElement> {
    let tone = |percent: f32| {
        if percent >= USAGE_WARNING_PERCENT {
            cx.theme().warning
        } else {
            cx.theme().muted_foreground
        }
    };
    let mut meters = Vec::new();
    if let Some(usage) = context {
        let percent = context_percent(usage);
        let tooltip = format!(
            "Context: {} of {} tokens",
            compact_tokens(usage.used_tokens),
            compact_tokens(usage.max_tokens)
        );
        meters.push(
            h_flex()
                .id("context-usage")
                .flex_shrink_0()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(tone(percent))
                .child(
                    ProgressCircle::new("context-usage-ring")
                        .xsmall()
                        .color(tone(percent))
                        .value(percent)
                        .accessibility_label(tooltip.clone()),
                )
                .child(format!("{percent:.0}%"))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .into_any_element(),
        );
    }
    for limit in limits {
        let tooltip = limit_description(limit, now);
        meters.push(
            div()
                .id(window_label(limit.window))
                .flex_shrink_0()
                .text_xs()
                .text_color(tone(limit.percent_used))
                .child(format!(
                    "{} {:.0}%",
                    window_label(limit.window),
                    limit.percent_used
                ))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .into_any_element(),
        );
    }
    meters
}

fn context_percent(usage: ContextUsage) -> f32 {
    (usage.used_tokens as f64 * 100. / usage.max_tokens.max(1) as f64).clamp(0., 100.) as f32
}

fn window_label(window: LimitWindow) -> &'static str {
    match window {
        LimitWindow::FiveHour => "5h",
        LimitWindow::SevenDay => "7d",
    }
}

fn limit_description(limit: &UsageLimit, now: SystemTime) -> String {
    let window = match limit.window {
        LimitWindow::FiveHour => "5-hour limit",
        LimitWindow::SevenDay => "7-day limit",
    };
    let reset = match limit.resets_at.map(|at| at.duration_since(now)) {
        Some(Ok(remaining)) => format!(", resets in {}", compact_duration(remaining)),
        Some(Err(_)) => ", reset due".to_owned(),
        None => String::new(),
    };
    format!("{window}: {:.0}% used{reset}", limit.percent_used)
}

/// `84000` → `84k`, `1_000_000` → `1M`, keeping one decimal below ten.
fn compact_tokens(tokens: u64) -> String {
    let (value, unit) = match tokens {
        0..1_000 => return tokens.to_string(),
        1_000..1_000_000 => (tokens as f64 / 1_000., "k"),
        _ => (tokens as f64 / 1_000_000., "M"),
    };
    let text = if value < 10. {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    format!("{}{unit}", text.strip_suffix(".0").unwrap_or(&text))
}

/// Largest two units: `3d 4h`, `2h 14m`, `5m`.
fn compact_duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

fn inset(content: impl IntoElement) -> AnyElement {
    h_flex()
        .w_full()
        .justify_center()
        .child(content_column().child(content))
        .into_any_element()
}

/// Keep HTML and Markdown image references inert. Only decoded provider attachments
/// may load image resources; ordinary assistant prose cannot name a local file to load.
struct InertResources;
impl MarkdownPlugin for InertResources {
    fn name(&self) -> &str {
        "drostje-inert-resources"
    }
    fn is_block(&self) -> bool {
        true
    }
    fn parse(
        &self,
        node: &markdown_ast::Node,
        context: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        fn unsafe_resource(node: &markdown_ast::Node) -> bool {
            matches!(
                node,
                markdown_ast::Node::Html(_)
                    | markdown_ast::Node::Image(_)
                    | markdown_ast::Node::ImageReference(_)
            ) || node
                .children()
                .is_some_and(|children| children.iter().any(unsafe_resource))
        }
        if matches!(node, markdown_ast::Node::Root(_)) || !unsafe_resource(node) {
            return None;
        }
        let text = context.node_source(node).unwrap_or_default().to_owned();
        Some(
            MarkdownNode::new(self.name(), ())
                .text(text.clone())
                .markdown(text),
        )
    }
    fn render(&self, node: &MarkdownNode, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().whitespace_normal().child(node.as_text().to_owned())
    }
}
/// Inline code that names a file (`src/app.rs:42`) opens it in Neovim at that line.
/// Parsed from the Markdown tree, so code blocks and identifiers like `cx.notify` stay
/// plain code; whether the file exists is checked only when it is opened.
struct FileReferences {
    thread: Option<ThreadId>,
}
const FILE_REFERENCE: &str = "drostje-file-reference";
impl MarkdownPlugin for FileReferences {
    fn name(&self) -> &str {
        FILE_REFERENCE
    }
    fn parse(
        &self,
        node: &markdown_ast::Node,
        _: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let markdown_ast::Node::InlineCode(code) = node else {
            return None;
        };
        let reference = file_refs::parse(&code.value)?;
        Some(
            MarkdownNode::new(FILE_REFERENCE, reference)
                .text(code.value.clone())
                .markdown(format!("`{}`", code.value)),
        )
    }
    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let reference = node.data::<FileReference>()?.clone();
        let thread = self.thread;
        let offset = node.source_range().map_or(0, |range| range.start);
        let tooltip: SharedString = match reference.line {
            Some(line) => format!("Open in Neovim at line {line}").into(),
            None => "Open in Neovim".into(),
        };
        Some(InlineElement::new(
            Button::new(SharedString::from(format!("file-reference-{offset}")))
                .link()
                .tab_stop(false)
                .label(node.as_text().to_owned())
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(context.font_size())
                .h(context.line_height())
                .debug_selector(|| "conversation-file-reference".into())
                .tooltip(tooltip)
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    window.dispatch_action(
                        Box::new(super::commands::OpenFileReference {
                            reference: reference.clone(),
                            thread,
                        }),
                        cx,
                    );
                }),
        ))
    }
}

/// Every file a Conversation displayed, in inline code or as a tool's file, most
/// recent first and each once: the rows of "Open file from conversation…".
pub(super) fn file_references(session: &ConversationSession) -> Vec<FileReference> {
    let mut seen = std::collections::HashSet::new();
    let mut references = Vec::new();
    let tools = session
        .tools
        .values()
        .filter_map(|tool| file_refs::tool_reference(&tool.name, &tool.input));
    let messages = session.messages.iter().rev().flat_map(|message| {
        let mut found = Vec::new();
        for block in &message.content {
            match block {
                ContentBlock::Text(text) => found.extend(file_refs::inline_code_references(text)),
                ContentBlock::PeerMessage(peer) => {
                    found.extend(file_refs::inline_code_references(&peer.body))
                }
                ContentBlock::ToolCall { name, input, .. } => {
                    found.extend(file_refs::tool_reference(name, input))
                }
                ContentBlock::Thinking(_)
                | ContentBlock::Image(_)
                | ContentBlock::ImageError(_) => {}
            }
        }
        found.into_iter().rev()
    });
    for reference in tools.chain(messages) {
        if seen.insert(reference.label()) {
            references.push(reference);
        }
    }
    references
}

/// Transcript prose stays close to body size: headings step up modestly and
/// paragraphs sit half a line apart, so a reply reads as a message, not a page.
/// `body` is the size the text inherits, so prose type scales its headings too.
fn transcript_text_style(body: Pixels) -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.5))
        .heading_font_size(move |level, _| {
            body * match level {
                1 => 1.3,
                2 => 1.2,
                3 => 1.1,
                _ => 1.,
            }
        })
}
/// Where a Markdown block keeps its parsed state.
#[derive(Clone, Copy)]
pub enum Markdown<'a> {
    /// With its element, for as long as the element renders in consecutive frames.
    Element,
    /// With the transcript, for the list row it renders in.
    Row(&'a MarkdownStates, usize),
}

/// `TextView` parses longer documents on a background executor.
const SYNC_PARSE_LIMIT: usize = 4 * 1024;

/// Parsed transcript Markdown, kept for as long as the transcript.
///
/// Element state lasts only while its element renders in consecutive frames, and a long
/// document lays out empty until its background parse lands. A row that left the list's
/// render window and came back would be measured at zero height, and scrolling across it
/// would skip it. Here each text parses once, and a landing parse remeasures its row.
#[derive(Clone)]
pub struct MarkdownStates {
    /// The Conversation whose working tree resolves file references in this text.
    thread: ThreadId,
    list: ListState,
    view: EntityId,
    entries: Rc<RefCell<HashMap<ElementId, MarkdownEntry>>>,
}

struct MarkdownEntry {
    state: Entity<TextViewState>,
    text: SharedString,
    row: Rc<Cell<usize>>,
    /// Set until a background parse lands and the row can be measured at its real height.
    parsing: Rc<Cell<bool>>,
    _parsed: Subscription,
}

impl MarkdownStates {
    fn new(thread: ThreadId, list: ListState, view: EntityId) -> Self {
        Self {
            thread,
            list,
            view,
            entries: Rc::default(),
        }
    }

    fn state(
        &self,
        id: ElementId,
        text: SharedString,
        row: usize,
        cx: &mut App,
    ) -> Entity<TextViewState> {
        let existing = self.entries.borrow_mut().get_mut(&id).map(|entry| {
            entry.row.set(row);
            let changed = entry.text != text;
            if changed {
                entry.parsing.set(text.len() > SYNC_PARSE_LIMIT);
                entry.text = text.clone();
            }
            (entry.state.clone(), changed)
        });
        match existing {
            Some((state, true)) => {
                state.update(cx, |state, cx| state.set_text(&text, cx));
                state
            }
            Some((state, false)) => state,
            None => self.insert(id, text, row, cx),
        }
    }

    fn insert(
        &self,
        id: ElementId,
        text: SharedString,
        row: usize,
        cx: &mut App,
    ) -> Entity<TextViewState> {
        let state = cx.new(|cx| TextViewState::markdown(&text, cx));
        let row = Rc::new(Cell::new(row));
        let parsing = Rc::new(Cell::new(text.len() > SYNC_PARSE_LIMIT));
        let parsed = cx.observe(&state, {
            let (list, view, row, parsing) =
                (self.list.clone(), self.view, row.clone(), parsing.clone());
            move |_, cx| {
                if parsing.replace(false) {
                    let row = row.get();
                    if row < list.item_count() {
                        list.remeasure_items(row..row + 1);
                    }
                }
                cx.notify(view);
            }
        });
        self.entries.borrow_mut().insert(
            id,
            MarkdownEntry {
                state: state.clone(),
                text,
                row,
                parsing,
                _parsed: parsed,
            },
        );
        state
    }

    fn clear(&self) {
        self.entries.borrow_mut().clear();
    }
}

pub fn markdown_text(
    id: impl Into<ElementId>,
    text: String,
    markdown: Markdown,
) -> impl IntoElement {
    TranscriptMarkdown {
        id: id.into(),
        text: text.into(),
        states: match markdown {
            Markdown::Element => None,
            Markdown::Row(states, row) => Some((states.clone(), row)),
        },
    }
}

#[derive(IntoElement)]
struct TranscriptMarkdown {
    id: ElementId,
    text: SharedString,
    states: Option<(MarkdownStates, usize)>,
}

impl RenderOnce for TranscriptMarkdown {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let body = window.text_style().font_size.to_pixels(window.rem_size());
        // Element-scoped text has no Conversation of its own: the selected one resolves it.
        let thread = self.states.as_ref().map(|(states, _)| states.thread);
        match self.states {
            Some((states, row)) => TextView::new(&states.state(self.id, self.text, row, cx)),
            None => TextView::markdown(self.id, self.text),
        }
        .style(transcript_text_style(body))
        .plugin(InertResources)
        .plugin(FileReferences { thread })
        .selectable(true)
        .on_link_click(move |url, event, window, cx| {
            if event.is_right_click() {
                return;
            }
            if url.starts_with("https://") || url.starts_with("http://") {
                window.dispatch_action(
                    Box::new(super::browser::OpenBrowserUrl {
                        url: url.to_string(),
                    }),
                    cx,
                );
            } else if let Some(reference) = file_refs::parse_link(url) {
                window.dispatch_action(
                    Box::new(super::commands::OpenFileReference { reference, thread }),
                    cx,
                );
            }
        })
    }
}
pub fn image_thumbnail(
    image: &ImageAttachment,
    owner: &WeakEntity<Workspace>,
    _cx: &App,
) -> AnyElement {
    let preview = image.clone();
    let owner = owner.clone();
    let open_preview = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
        cx.stop_propagation();
        let _ = owner.update(cx, |workspace, cx| {
            workspace.image_preview = Some(preview.clone());
            workspace.sync_detail_surface(window, cx);
            cx.notify();
        });
    };
    Attachment::new()
        .id(SharedString::from(format!("image-{}", image.id)))
        .media(
            AttachmentMedia::new().w(rems(8.)).h(rems(6.)).child(
                img(image.path.clone())
                    .w_full()
                    .h_full()
                    .object_fit(ObjectFit::Contain),
            ),
        )
        .content(
            AttachmentContent::new()
                .title(AttachmentTitle::new(format!(
                    "{} × {}",
                    image.width, image.height
                )))
                .description(AttachmentDescription::new(format!(
                    "{}{}",
                    image.mime,
                    if image.original.is_some() {
                        " · prepared"
                    } else {
                        ""
                    }
                ))),
        )
        .on_click(open_preview.clone())
        .actions(
            AttachmentActions::new().child(
                Button::new(SharedString::from(format!("preview-image-{}", image.id)))
                    .small()
                    .ghost()
                    .label("Preview")
                    .tooltip("Preview image")
                    .on_click(open_preview),
            ),
        )
        .into_any_element()
}
/// Flip a disclosure key in the workspace's `expanded` set.
fn toggle(
    owner: &WeakEntity<Workspace>,
    key: String,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + use<> {
    let owner = owner.clone();
    move |_, _, cx| {
        cx.stop_propagation();
        let _ = owner.update(cx, |workspace, cx| {
            if !workspace.expanded.remove(&key) {
                workspace.expanded.insert(key.clone());
            }
            cx.emit(TranscriptDisclosure);
        });
    }
}
fn is_expanded(owner: &WeakEntity<Workspace>, key: &str, cx: &App) -> bool {
    owner
        .upgrade()
        .is_some_and(|workspace| workspace.read(cx).expanded.contains(key))
}
/// The one chevron control every disclosure in the transcript uses.
fn disclosure(id: String, expanded: bool, owner: &WeakEntity<Workspace>, tooltip: &str) -> Button {
    Button::new(SharedString::from(id.clone()))
        .xsmall()
        .ghost()
        .cursor_default()
        .icon(if expanded {
            Lucide::ChevronDown
        } else {
            Lucide::ChevronRight
        })
        .tooltip(tooltip.to_owned())
        .on_click(toggle(owner, id))
}
fn mono(cx: &App) -> Div {
    div()
        .min_w_0()
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(cx.theme().mono_font_size)
        .whitespace_normal()
}

/// Match OMP's prose-first reasoning display: empty stream sentinels disappear and
/// fenced implementation detail becomes an ellipsis instead of dominating the timeline.
fn thinking_for_display(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some((marker, width)) = fence {
            let marker_width = trimmed.chars().take_while(|ch| *ch == marker).count();
            if marker_width >= width && trimmed.chars().all(|ch| ch == marker) {
                fence = None;
            }
            continue;
        }
        if matches!(trimmed, "<!-- -->" | "<!-->") {
            continue;
        }
        let marker = trimmed.chars().next();
        let marker_width = marker
            .filter(|marker| matches!(marker, '`' | '~'))
            .map_or(0, |marker| {
                trimmed.chars().take_while(|ch| *ch == marker).count()
            });
        if marker_width >= 3 {
            fence = marker.map(|marker| (marker, marker_width));
            if let Some(last) = lines.iter_mut().rev().find(|line| !line.trim().is_empty()) {
                let trimmed = last.trim_end();
                *last = if trimmed.ends_with("...") {
                    trimmed.to_owned()
                } else if let Some(prefix) = trimmed.strip_suffix('.') {
                    format!("{prefix}...")
                } else {
                    format!("{trimmed}...")
                };
            } else {
                lines.push("...".into());
            }
            continue;
        }
        lines.push(line.to_owned());
    }
    let display = lines.join("\n").trim().to_owned();
    if display
        .chars()
        .all(|ch| ch.is_whitespace() || matches!(ch, '.' | '…'))
    {
        String::new()
    } else {
        display
    }
}

/// The `**Title**` lines a model heads its reasoning steps with, including
/// those `thinking_for_display` closed with an ellipsis.
fn thinking_titles(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(['.', '…']);
            let title = line.strip_prefix("**")?.strip_suffix("**")?.trim();
            (!title.is_empty() && !title.contains("**")).then_some(title)
        })
        .collect()
}

/// Reasoning folds to a muted label and its step titles; the disclosure opens
/// the full text behind the same left rule.
fn thinking_block(
    identity: String,
    display: String,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    let key = format!("{identity}:thinking");
    let expanded = is_expanded(owner, &key, cx);
    let group: SharedString = format!("{key}:group").into();
    // Only the label row toggles, so selecting the reasoning text never folds it.
    let header = h_flex()
        .id(SharedString::from(format!("{key}:header")))
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .on_click(toggle(owner, key.clone()))
        .child(Icon::new(Lucide::Lightbulb).xsmall())
        .child(div().flex_1().child("Thinking"))
        .child(
            div()
                .when(!expanded, |chevron| {
                    chevron
                        .invisible()
                        .group_hover(group.clone(), |style| style.visible())
                })
                .child(disclosure(
                    key.clone(),
                    expanded,
                    owner,
                    if expanded {
                        "Hide reasoning"
                    } else {
                        "Show reasoning"
                    },
                )),
        );
    let detail: Option<AnyElement> = if expanded {
        let markdown_id = format!("{identity}:markdown");
        let rendered = display.clone();
        Some(guarded_content(
            identity,
            &display,
            owner,
            cx,
            "thinking",
            || markdown_text(markdown_id, rendered, markdown).into_any_element(),
        ))
    } else {
        let titles = thinking_titles(&display);
        if titles.is_empty() {
            // Untitled reasoning still shows where it starts rather than a bare label.
            display
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(|line| {
                    div()
                        .truncate()
                        .child(line.trim().to_owned())
                        .into_any_element()
                })
        } else {
            Some(
                v_flex()
                    .gap_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .children(
                        titles
                            .into_iter()
                            .map(|title| div().whitespace_normal().child(title.to_owned())),
                    )
                    .into_any_element(),
            )
        }
    };
    v_flex()
        .id(SharedString::from(format!("{key}:block")))
        .debug_selector(|| "conversation-thinking".into())
        .group(group)
        .w_full()
        .min_w_0()
        .child(header)
        .when_some(detail, |block, detail| {
            block.child(
                div()
                    .ml_1p5()
                    .pl_3()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .text_color(cx.theme().muted_foreground)
                    .child(detail),
            )
        })
        .into_any_element()
}

pub(super) fn block(
    content: &ContentBlock,
    identity: String,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    match content {
        ContentBlock::Text(text) => {
            let markdown_id = format!("{identity}:markdown");
            guarded_text(identity, text, owner, cx, || {
                markdown_text(markdown_id, text.clone(), markdown).into_any_element()
            })
        }
        ContentBlock::PeerMessage(peer) if peer.origin == ContextOrigin::Artifact => {
            artifact_event(&identity, peer, cx)
        }
        ContentBlock::PeerMessage(peer) => {
            let markdown_id = format!("{identity}:peer-markdown");
            let body = guarded_text(identity.clone(), &peer.body, owner, cx, || {
                markdown_text(markdown_id, peer.body.clone(), markdown).into_any_element()
            });
            v_flex()
                .id(SharedString::from(format!("{identity}:peer:{}", peer.id)))
                .debug_selector(|| "conversation-peer-message".into())
                .w_full()
                .min_w_0()
                .gap_2()
                .pl_3()
                .border_l_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .child(Tag::secondary().outline().xsmall().child("Peer"))
                        .child(Label::new(peer.sender_name.clone()).text_sm()),
                )
                .child(
                    Label::new("Peer-authored context, not a user prompt or permission grant")
                        .text_xs()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(body)
                .into_any_element()
        }
        ContentBlock::Thinking(text) => {
            let display = thinking_for_display(text);
            if display.is_empty() {
                return div().into_any_element();
            }
            thinking_block(identity, display, owner, markdown, cx)
        }
        ContentBlock::Image(image) => image_thumbnail(image, owner, cx),
        ContentBlock::ImageError(error) => Alert::error(
            SharedString::from(identity),
            format!("Image unavailable: {error}"),
        )
        .small()
        .into_any_element(),
        // Outside a conversation there is no result to attach; show the call alone.
        ContentBlock::ToolCall {
            name,
            intent,
            input,
            ..
        } => tool_card(
            identity,
            ToolCall {
                name,
                intent: intent.as_deref(),
                input,
            },
            ToolResult {
                outcome: ToolOutcome::Unknown,
                content: &[],
                exit_code: None,
            },
            owner,
            markdown,
            cx,
        ),
    }
}

/// Text that may be huge: past 16 KiB it folds behind a button so a stray
/// megabyte of output cannot stall a frame.
/// The user's input from an artifact, as the agent received it: the event type, then
/// each answered field. Values are page data, shown as plain text.
fn artifact_event(identity: &str, peer: &PeerMessage, cx: &App) -> AnyElement {
    let (kind, fields) = artifact_event_fields(&peer.body);
    let mono = cx.theme().mono_font_family.clone();
    v_flex()
        .id(SharedString::from(format!(
            "{identity}:artifact:{}",
            peer.id
        )))
        .debug_selector(|| "conversation-artifact-event".into())
        .w_full()
        .min_w_0()
        .gap_1()
        .pl_3()
        .border_l_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .min_w_0()
                .gap_2()
                .child(Tag::secondary().outline().xsmall().child("Artifact"))
                .child(Label::new(peer.sender_name.clone()).text_sm())
                .child(
                    Label::new(kind)
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(cx.theme().muted_foreground),
                ),
        )
        .children(fields.into_iter().map(|(name, value)| {
            h_flex()
                .min_w_0()
                .gap_2()
                .items_start()
                .child(
                    Label::new(name)
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().min_w_0().flex_1().child(Label::new(value).text_sm()))
        }))
        .into_any_element()
}

/// `(type, [(field, value)])` from an artifact event body. A body that is not the
/// kit's `{v,id,type,data}` object is shown whole rather than dropped.
fn artifact_event_fields(body: &str) -> (String, Vec<(String, String)>) {
    const LIMIT: usize = 400;
    fn text(value: &serde_json::Value) -> String {
        let text = match value {
            serde_json::Value::Null => "—".to_owned(),
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(items) => {
                items.iter().map(text).collect::<Vec<_>>().join(", ")
            }
            other => other.to_string(),
        };
        if text.chars().count() > LIMIT {
            let mut cut: String = text.chars().take(LIMIT).collect();
            cut.push('…');
            cut
        } else {
            text
        }
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return ("event".into(), vec![("body".into(), text(&body.into()))]);
    };
    let kind = value["type"].as_str().unwrap_or("event").to_owned();
    let fields = match &value["data"] {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(name, value)| (name.clone(), text(value)))
            .collect(),
        serde_json::Value::Null => Vec::new(),
        other => vec![("value".into(), text(other))],
    };
    (kind, fields)
}

fn guarded_text(
    identity: String,
    text: &str,
    owner: &WeakEntity<Workspace>,
    cx: &App,
    render: impl FnOnce() -> AnyElement,
) -> AnyElement {
    guarded_content(identity, text, owner, cx, "result", render)
}

fn guarded_content(
    identity: String,
    text: &str,
    owner: &WeakEntity<Workspace>,
    cx: &App,
    kind: &str,
    render: impl FnOnce() -> AnyElement,
) -> AnyElement {
    if text.len() <= 16_384 {
        return render();
    }
    let expanded = is_expanded(owner, &identity, cx);
    let key = identity.clone();
    v_flex()
        .gap_2()
        .child(
            Button::new(SharedString::from(identity))
                .small()
                .ghost()
                .label(format!(
                    "{} large {kind} ({} bytes)",
                    if expanded { "Collapse" } else { "Expand" },
                    text.len()
                ))
                .on_click(toggle(owner, key)),
        )
        .when(expanded, |view| view.child(render()))
        .into_any_element()
}
/// Plain output (compiler messages, grep hits, file contents) rendered through
/// the kit's Markdown code block, which brings the code font, background,
/// highlighting for a known language and copy support.
fn code_block(
    identity: impl Into<ElementId>,
    text: &str,
    language: &str,
    markdown: Markdown,
) -> impl IntoElement {
    markdown_text(identity, code_fence(text, language), markdown)
}
/// Wrap `text` in a fence longer than any backtick run it contains, so the
/// block always closes where intended.
fn code_fence(text: &str, language: &str) -> String {
    let longest = text.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let text = text.strip_suffix('\n').unwrap_or(text);
    format!("{fence}{language}\n{text}\n{fence}\n")
}
/// Tools whose results are plain output rather than prose, with the language
/// the code block should highlight. `None` keeps the Markdown rendering.
fn result_language<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    match name.to_ascii_lowercase().as_str() {
        "bash" | "eval" | "grep" | "glob" => Some("text"),
        "read" | "write" | "edit" => Some(
            path_field(input)
                .and_then(|path| path.rsplit_once('.'))
                .map(|(_, extension)| extension)
                .filter(|extension| {
                    !extension.is_empty() && extension.chars().all(char::is_alphanumeric)
                })
                .unwrap_or("text"),
        ),
        _ => None,
    }
}
/// What a tool card says about its call, derived from the tool name and input only.
#[derive(Debug, PartialEq, Eq)]
pub struct ToolSummary {
    pub icon: Lucide,
    pub body: String,
}
const SUMMARY_LIMIT: usize = 200;
fn text_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input[key].as_str().filter(|value| !value.trim().is_empty())
}
/// OMP names the file `path`, Claude Code `file_path`.
fn path_field(input: &Value) -> Option<&str> {
    text_field(input, "path").or_else(|| text_field(input, "file_path"))
}
fn compact_json(input: &Value) -> String {
    let text = match input {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    if text.chars().count() > SUMMARY_LIMIT {
        let cut: String = text.chars().take(SUMMARY_LIMIT).collect();
        format!("{cut}…")
    } else {
        text
    }
}
/// Files named in an `edit` patch header line such as `[path/to/file.rs#69AE]`.
fn patch_paths(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .filter(|line| line.starts_with('[') && line.ends_with(']'))
        .map(|line| {
            let inner = &line[1..line.len() - 1];
            inner.rsplit_once('#').map_or(inner, |(path, _)| path)
        })
        .filter(|path| !path.is_empty())
        .collect()
}
/// Harnesses disagree on case (`read`, `Read`), so names match case-insensitively.
pub fn tool_summary(name: &str, input: &Value) -> ToolSummary {
    let (icon, body) = match name.to_ascii_lowercase().as_str() {
        "bash" => (
            Lucide::Terminal,
            text_field(input, "command").map(str::to_owned),
        ),
        "read" => (Lucide::FileText, path_field(input).map(str::to_owned)),
        "write" => (Lucide::FilePlus, path_field(input).map(str::to_owned)),
        "edit" => (
            Lucide::FilePen,
            text_field(input, "input")
                .map(patch_paths)
                .filter(|paths| !paths.is_empty())
                .map(|paths| paths.join("\n"))
                .or_else(|| path_field(input).map(str::to_owned)),
        ),
        "grep" => (
            Lucide::Search,
            text_field(input, "pattern").map(|pattern| match path_field(input) {
                Some(path) => format!("{pattern}  in {path}"),
                None => pattern.to_owned(),
            }),
        ),
        "glob" => (
            Lucide::FolderSearch,
            text_field(input, "pattern")
                .or_else(|| path_field(input))
                .map(str::to_owned),
        ),
        "todo" => (Lucide::ListTodo, todo_list::describe_change(input)),
        "task" => (
            Lucide::Workflow,
            input["tasks"]
                .as_array()
                .map(|tasks| match tasks.len() {
                    1 => "1 task".to_owned(),
                    n => format!("{n} tasks"),
                })
                .or_else(|| text_field(input, "description").map(str::to_owned)),
        ),
        "eval" => (
            Lucide::FlaskConical,
            text_field(input, "title")
                .or_else(|| text_field(input, "language"))
                .map(str::to_owned),
        ),
        "ask" => (
            Lucide::CircleQuestionMark,
            input["questions"][0]["question"]
                .as_str()
                .or_else(|| text_field(input, "question"))
                .map(str::to_owned),
        ),
        "web_search" | "websearch" => {
            (Lucide::Globe, text_field(input, "query").map(str::to_owned))
        }
        "webfetch" => (Lucide::Globe, text_field(input, "url").map(str::to_owned)),
        _ => (Lucide::Wrench, None),
    };
    ToolSummary {
        icon,
        body: body.unwrap_or_else(|| compact_json(input)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolOutcome {
    Running,
    Succeeded,
    Failed,
    /// No result was recorded and nothing is running; DROSTJE. does not guess.
    Unknown,
}

/// The call half of a tool card, taken from the assistant message.
#[derive(Clone, Copy)]
struct ToolCall<'a> {
    name: &'a str,
    intent: Option<&'a str>,
    input: &'a Value,
}
/// The result half, taken from live activity or the historical result message.
#[derive(Clone, Copy)]
struct ToolResult<'a> {
    outcome: ToolOutcome,
    content: &'a [ContentBlock],
    exit_code: Option<i64>,
}
impl ToolResult<'_> {
    fn recorded(error: bool, exit_code: Option<i64>) -> ToolOutcome {
        if error || exit_code.is_some_and(|code| code != 0) {
            ToolOutcome::Failed
        } else {
            ToolOutcome::Succeeded
        }
    }
}
/// What a collapsed tool row shows of the summary: its first line, marked when
/// more is folded away. The row itself truncates to the available width.
fn summary_line(body: &str) -> String {
    let mut lines = body.lines().filter(|line| !line.trim().is_empty());
    let first = lines.next().unwrap_or_default().trim();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

/// One row for a tool call, live or historical: icon, caption and the first line
/// of the call, about one line high. The disclosure opens the full call and its
/// result; a failed call opens automatically.
fn tool_card(
    key: String,
    call: ToolCall<'_>,
    result: ToolResult<'_>,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    let ToolCall {
        name,
        intent,
        input,
    } = call;
    let ToolResult {
        outcome,
        content: result,
        exit_code,
    } = result;
    if name == "todo"
        && outcome != ToolOutcome::Failed
        && let Some(list) = result.iter().find_map(|block| match block {
            ContentBlock::Text(text) => TodoList::parse(text),
            _ => None,
        })
    {
        return todo_card(key, call, &list, outcome, owner, cx);
    }
    let summary = tool_summary(name, input);
    let has_result = !result.is_empty() || exit_code.is_some();
    let has_detail = has_result || !summary.body.is_empty();
    let expanded = outcome == ToolOutcome::Failed || is_expanded(owner, &key, cx);
    let caption = intent.unwrap_or(name).to_owned();
    let mark: Option<AnyElement> = match outcome {
        ToolOutcome::Running => Some(Spinner::new().xsmall().into_any_element()),
        ToolOutcome::Failed => Some(
            Icon::new(Lucide::CircleX)
                .xsmall()
                .text_color(cx.theme().danger)
                .into_any_element(),
        ),
        ToolOutcome::Succeeded => None,
        ToolOutcome::Unknown => Some(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("No result")
                .into_any_element(),
        ),
    };
    let group: SharedString = format!("{key}:group").into();
    // Only the row toggles, so selecting text in the opened detail never folds it.
    let mut header = h_flex()
        .id(SharedString::from(format!("{key}:header")))
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(summary.icon).xsmall())
        .child(
            div()
                .flex_shrink_0()
                .max_w(relative(0.5))
                .truncate()
                .child(caption),
        )
        .child(
            mono(cx)
                .debug_selector(|| "conversation-tool-summary-line".into())
                .flex_1()
                .truncate()
                .when(!expanded, |line| line.child(summary_line(&summary.body))),
        );
    // The icon names the tool while folded; the opened row spells it out.
    if intent.is_some() && expanded {
        header = header.child(div().flex_shrink_0().child(name.to_owned()));
    }
    if let Some(reference) = file_refs::tool_reference(name, input) {
        let thread = match markdown {
            Markdown::Row(states, _) => Some(states.thread),
            Markdown::Element => None,
        };
        header = header.child(
            div()
                .invisible()
                .group_hover(group.clone(), |style| style.visible())
                .child(open_file_button(
                    format!("{key}:open-file"),
                    reference,
                    thread,
                )),
        );
    }
    header = header.when_some(mark, |row, mark| row.child(mark));
    if has_detail && outcome != ToolOutcome::Failed {
        header = header.on_click(toggle(owner, key.clone())).child(
            div()
                .when(!expanded, |chevron| {
                    chevron
                        .invisible()
                        .group_hover(group.clone(), |style| style.visible())
                })
                .child(disclosure(
                    key.clone(),
                    expanded,
                    owner,
                    if expanded {
                        "Hide details"
                    } else {
                        "Show details"
                    },
                )),
        );
    }
    let card_selector = format!("{key}:card");
    let mut card = v_flex()
        .id(SharedString::from(card_selector))
        .debug_selector(|| "conversation-tool-card".into())
        .test_support()
        .group(group)
        .w_full()
        .min_w_0()
        .child(header);
    let has_body = !summary.body.is_empty();
    let mut detail = v_flex()
        .w_full()
        .min_w_0()
        .mb_1()
        .px_3()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius)
        .when(expanded && has_body, |detail| {
            detail.child(mono(cx).py_2().child(summary.body))
        });
    if has_result && expanded {
        let mut output = v_flex()
            .debug_selector(|| "conversation-tool-full-result".into())
            .gap_2()
            .py_2()
            .when(has_body, |output| {
                output.border_t_1().border_color(cx.theme().border)
            });
        if let Some(code) = exit_code.filter(|code| *code != 0 || outcome == ToolOutcome::Failed) {
            output = output.child(mono(cx).child(format!("Exit code {code}")));
        }
        let language = result_language(name, input);
        for (index, block) in result.iter().enumerate() {
            let identity = format!("{key}:{index}");
            output = output.child(match (block, language) {
                (ContentBlock::Text(text), Some(language)) => {
                    let markdown_id = format!("{identity}:markdown");
                    guarded_text(identity, text, owner, cx, || {
                        code_block(markdown_id, text, language, markdown).into_any_element()
                    })
                }
                _ => self::block(block, identity, owner, markdown, cx),
            });
        }
        detail = detail.child(output);
    }
    if expanded && has_detail {
        card = card.child(detail);
    }
    card.into_any_element()
}

/// Opens the file a read, write or edit touched in Neovim. Shown on hover; the keyboard
/// path is "Open file from conversation…", which lists the same files.
fn open_file_button(id: String, reference: FileReference, thread: Option<ThreadId>) -> Button {
    Button::new(SharedString::from(id))
        .xsmall()
        .ghost()
        .cursor_default()
        .tab_stop(false)
        .icon(Lucide::FileCode)
        .tooltip("Open in Neovim")
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(
                Box::new(super::commands::OpenFileReference {
                    reference: reference.clone(),
                    thread,
                }),
                cx,
            );
        })
}

/// A `todo` call whose result parsed: what changed, OMP's tally and the
/// checklist. A new or blocked list opens by default; other changes fold the
/// list behind the disclosure so a run of `done` calls stays short.
fn todo_card(
    key: String,
    call: ToolCall<'_>,
    list: &TodoList,
    outcome: ToolOutcome,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let ToolCall {
        name,
        intent,
        input,
    } = call;
    let change = todo_list::describe_change(input).unwrap_or_else(|| name.to_owned());
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
        .on_click(toggle(owner, key.clone()))
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
fn todo_checklist(list: &TodoList, cx: &App) -> Div {
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

/// How a transcript message takes part in a run of consecutive tool calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunPart {
    /// Visible content that ends a run.
    Break,
    /// Renders nothing (a result shown on its call's card, a hidden harness note).
    Transparent,
    /// Visible content followed by this many tool calls, which start a run.
    Opens(usize),
    /// Only tool calls and reasoning, which continue a run.
    Calls(usize),
}

/// Consecutive tool calls across messages: `head` renders the run's header, rows up to
/// `end` belong to it. Only a run of two or more calls folds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ToolRun {
    head: usize,
    end: usize,
    calls: usize,
}

impl ToolRun {
    fn folds(self) -> bool {
        self.calls >= 2
    }
}

/// Where a message's trailing tool calls start, and how many there are. Reasoning and blank
/// text between them belong to the run; a `todo` call keeps its own card.
fn trailing_calls(content: &[ContentBlock]) -> Option<(usize, usize)> {
    let start = content
        .iter()
        .rposition(|block| match block {
            ContentBlock::ToolCall { name, .. } => name == "todo",
            ContentBlock::Thinking(_) => false,
            ContentBlock::Text(text) => !text.trim().is_empty(),
            ContentBlock::PeerMessage(_) | ContentBlock::Image(_) | ContentBlock::ImageError(_) => {
                true
            }
        })
        .map_or(0, |last| last + 1);
    let calls = content[start..]
        .iter()
        .filter(|block| matches!(block, ContentBlock::ToolCall { .. }))
        .count();
    (calls > 0).then_some((start, calls))
}

fn run_part(message: &Message, session: &ConversationSession) -> RunPart {
    if message.role == "toolResult" {
        return if message
            .tool_call_id
            .as_deref()
            .is_some_and(|call| session.has_tool_call(call))
        {
            RunPart::Transparent
        } else {
            RunPart::Break
        };
    }
    if harness_note_hidden(message) {
        return RunPart::Transparent;
    }
    if message.role != "assistant" {
        return RunPart::Break;
    }
    match trailing_calls(&message.content) {
        Some((0, calls)) => RunPart::Calls(calls),
        Some((_, calls)) => RunPart::Opens(calls),
        None => RunPart::Break,
    }
}

/// The run of tool calls the message at `index` belongs to, if any.
fn tool_run(part: impl Fn(usize) -> RunPart, len: usize, index: usize) -> Option<ToolRun> {
    let mut head = None;
    for ix in (0..=index).rev() {
        match part(ix) {
            RunPart::Break => break,
            RunPart::Opens(_) => {
                head = Some(ix);
                break;
            }
            RunPart::Calls(_) => head = Some(ix),
            RunPart::Transparent => {}
        }
    }
    let head = head?;
    let (RunPart::Opens(mut calls) | RunPart::Calls(mut calls)) = part(head) else {
        return None;
    };
    let mut end = head + 1;
    while end < len {
        match part(end) {
            RunPart::Calls(more) => calls += more,
            RunPart::Transparent => {}
            RunPart::Break | RunPart::Opens(_) => break,
        }
        end += 1;
    }
    Some(ToolRun { head, end, calls })
}

/// A call's outcome from live activity, else its recorded result; `active` is whether the
/// thread is still working on a call without either.
fn call_outcome(session: &ConversationSession, call_id: &str, active: bool) -> ToolOutcome {
    let pending = if active {
        ToolOutcome::Running
    } else {
        ToolOutcome::Unknown
    };
    if let Some(tool) = session.tools.get(call_id) {
        return if tool.error || tool.exit_code.is_some_and(|code| code != 0) {
            ToolOutcome::Failed
        } else if tool.complete {
            ToolOutcome::Succeeded
        } else {
            pending
        };
    }
    session.tool_result(call_id).map_or(pending, |result| {
        ToolResult::recorded(result.error, result.exit_code)
    })
}

/// The folded face of a run: how many calls, the latest one, and what is running, failed or
/// unknown, so nothing that needs attention hides behind the fold.
fn tool_run_header(
    key: String,
    run: ToolRun,
    expanded: bool,
    session: &ConversationSession,
    active: bool,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let mut latest = None;
    let (mut running, mut failed, mut unknown) = (0, 0, 0);
    for message in &session.messages[run.head..run.end] {
        let Some((start, _)) = trailing_calls(&message.content) else {
            continue;
        };
        for block in &message.content[start..] {
            let ContentBlock::ToolCall {
                id,
                name,
                intent,
                input,
            } = block
            else {
                continue;
            };
            match call_outcome(session, id, active) {
                ToolOutcome::Running => running += 1,
                ToolOutcome::Failed => failed += 1,
                ToolOutcome::Unknown => unknown += 1,
                ToolOutcome::Succeeded => {}
            }
            let intent = intent.as_deref().or_else(|| {
                session
                    .tools
                    .get(id)
                    .and_then(|tool| tool.intent.as_deref())
            });
            latest = Some((intent.unwrap_or(name).to_owned(), tool_summary(name, input)));
        }
    }
    let group: SharedString = format!("{key}:group").into();
    h_flex()
        .id(SharedString::from(format!("{key}:header")))
        .debug_selector(|| "conversation-tool-run".into())
        .test_support()
        .group(group.clone())
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .on_click(toggle(owner, key.clone()))
        .child(Icon::new(Lucide::Wrench).xsmall())
        .child(
            div()
                .flex_shrink_0()
                .text_color(cx.theme().foreground)
                .child(format!("{} tool calls", run.calls)),
        )
        .child(h_flex().flex_1().min_w_0().gap_2().when_some(
            latest.filter(|_| !expanded),
            |row, (caption, summary)| {
                row.child(Icon::new(summary.icon).xsmall())
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w(relative(0.5))
                            .truncate()
                            .child(caption),
                    )
                    .child(
                        mono(cx)
                            .flex_1()
                            .truncate()
                            .child(summary_line(&summary.body)),
                    )
            },
        ))
        .when(unknown > 0, |row| {
            row.child(div().flex_shrink_0().text_xs().child(if unknown == 1 {
                "1 without result".to_owned()
            } else {
                format!("{unknown} without result")
            }))
        })
        .when(failed > 0, |row| {
            row.child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(Icon::new(Lucide::CircleX).xsmall())
                    .child(format!("{failed} failed")),
            )
        })
        .when(running > 0, |row| row.child(Spinner::new().xsmall()))
        .child(
            div()
                .when(!expanded, |chevron| {
                    chevron
                        .invisible()
                        .group_hover(group, |style| style.visible())
                })
                .child(disclosure(
                    key,
                    expanded,
                    owner,
                    if expanded {
                        "Hide tool calls"
                    } else {
                        "Show tool calls"
                    },
                )),
        )
        .into_any_element()
}

#[expect(
    clippy::too_many_arguments,
    reason = "a row helper takes the row context message() renders with"
)]
fn tool_call_row(
    thread: ThreadId,
    call_id: &str,
    call: ToolCall<'_>,
    session: &ConversationSession,
    active: bool,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    if call.name.ends_with("show_artifact") {
        let recorded = session.tool_result(call_id);
        let blocks = session
            .tools
            .get(call_id)
            .map(|tool| tool.result.as_slice())
            .or_else(|| recorded.map(|result| result.content.as_slice()))
            .unwrap_or(&[]);
        let browser = blocks.iter().find_map(|block| {
            let ContentBlock::Text(text) = block else {
                return None;
            };
            let value: Value = serde_json::from_str(text).ok()?;
            let id = value
                .get("id")
                .or_else(|| value.get("result")?.get("id"))?
                .as_str()?;
            uuid::Uuid::parse_str(id).ok()
        });
        if let Some((pane, placement)) =
            browser.and_then(|id| owner.upgrade()?.read(cx).artifact_browser(thread, id))
        {
            return match placement {
                crate::browser_tools::ArtifactPlacement::Conversation => pane.into_any_element(),
                // One retained surface, one place: the transcript keeps a pointer to it.
                crate::browser_tools::ArtifactPlacement::Composer => {
                    docked_artifact_stub(call_id, &pane, owner, cx)
                }
            };
        }
    }
    let key = format!("tool:{thread}:{call_id}");
    let outcome = call_outcome(session, call_id, active);
    if let Some(tool) = session.tools.get(call_id) {
        return tool_card(
            key,
            ToolCall {
                intent: call.intent.or(tool.intent.as_deref()),
                ..call
            },
            ToolResult {
                outcome,
                content: &tool.result,
                exit_code: tool.exit_code,
            },
            owner,
            markdown,
            cx,
        );
    }
    let result = match session.tool_result(call_id) {
        Some(result) => ToolResult {
            outcome,
            content: &result.content,
            exit_code: result.exit_code,
        },
        None => ToolResult {
            outcome,
            content: &[],
            exit_code: None,
        },
    };
    tool_card(key, call, result, owner, markdown, cx)
}

/// The transcript row of an artifact docked above the composer: its title and the
/// commands that reach it, never a second copy of the page.
fn docked_artifact_stub(
    call_id: &str,
    pane: &Entity<super::browser::BrowserPane>,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let title = pane
        .read(cx)
        .artifact_title()
        .unwrap_or("HTML artifact")
        .to_owned();
    let focus = owner.clone();
    let inline = owner.clone();
    h_flex()
        .id(SharedString::from(format!(
            "docked-artifact-stub-{call_id}"
        )))
        .debug_selector(|| "docked-artifact-stub".into())
        .test_support()
        .w_full()
        .min_w_0()
        .gap_2()
        .px_2()
        .py_1()
        .items_center()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius)
        .child(
            Tag::secondary()
                .outline()
                .xsmall()
                .child("Above the prompt"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Label::new(title).text_sm().truncate()),
        )
        .child(
            Button::new(SharedString::from(format!(
                "docked-artifact-focus-{call_id}"
            )))
            .ghost()
            .xsmall()
            .label("Focus")
            .tooltip_with_action(
                Command::FocusComposerArtifact.label(),
                &FocusComposerArtifact,
                Some("Workspace"),
            )
            .on_click(move |_, window, cx| {
                let _ = focus.update(cx, |workspace, cx| {
                    workspace.execute(Command::FocusComposerArtifact, window, cx)
                });
            }),
        )
        .child(
            Button::new(SharedString::from(format!(
                "docked-artifact-inline-{call_id}"
            )))
            .ghost()
            .xsmall()
            .label("Show here")
            .tooltip_with_action(
                Command::MoveArtifactIntoConversation.label(),
                &MoveArtifactIntoConversation,
                Some("Workspace"),
            )
            .on_click(move |_, window, cx| {
                let _ = inline.update(cx, |workspace, cx| {
                    workspace.execute(Command::MoveArtifactIntoConversation, window, cx)
                });
            }),
        )
        .into_any_element()
}

/// OMP injects `<system-reminder>` nudges, `<irc>` relays and `<critical>`
/// mode notes as `custom` messages for the model's benefit; the transcript
/// hides them. The role check keeps the same text visible when a user pastes it.
fn harness_note_hidden(message: &Message) -> bool {
    const HIDDEN: [(&str, &str); 3] = [
        ("<system-reminder>", "</system-reminder>"),
        ("<irc>", "</irc>"),
        ("<critical>", "</critical>"),
    ];
    message.role == "custom"
        && !message.content.is_empty()
        && message.content.iter().all(|block| match block {
            ContentBlock::Text(text) => {
                let text = text.trim();
                HIDDEN
                    .iter()
                    .any(|(open, close)| text.starts_with(open) && text.ends_with(close))
            }
            _ => false,
        })
}

#[expect(
    clippy::too_many_arguments,
    reason = "a transcript row takes its position as well as the row context"
)]
fn message(
    message: Arc<Message>,
    index: usize,
    id: ThreadId,
    session: &ConversationSession,
    active: bool,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    // A tool result answers a call rendered earlier; that card shows its content.
    if message.role == "toolResult" {
        if message
            .tool_call_id
            .as_deref()
            .is_some_and(|call| session.has_tool_call(call))
        {
            return div().into_any_element();
        }
        return inset(tool_card(
            format!("orphan-result:{id}:{}", message.id),
            ToolCall {
                name: "Tool result",
                intent: None,
                input: &Value::Null,
            },
            ToolResult {
                outcome: ToolResult::recorded(message.error, message.exit_code),
                content: &message.content,
                exit_code: message.exit_code,
            },
            owner,
            markdown,
            cx,
        ));
    }
    if harness_note_hidden(&message) {
        return div().into_any_element();
    }
    // In a folding run, the head row carries the header and every row its own calls.
    let run = tool_run(
        |ix: usize| run_part(&session.messages[ix], session),
        session.messages.len(),
        index,
    )
    .filter(|run| run.folds());
    let run_key = run.map(|run| format!("toolrun:{id}:{}", session.messages[run.head].id));
    let run_expanded = run_key
        .as_deref()
        .is_some_and(|key| is_expanded(owner, key, cx));
    let run_start = run
        .and_then(|_| trailing_calls(&message.content))
        .map(|(start, _)| start);
    let head = run.is_some_and(|run| run.head == index);
    if run.is_some() && !head && !run_expanded {
        return div().into_any_element();
    }
    let user_message = message.role == "user";
    let raw_key = format!("raw:{id}:{}", message.id);
    let raw_expanded = message.raw.is_some() && is_expanded(owner, &raw_key, cx);
    let group: SharedString = format!("message-group-{id}-{}", message.id).into();
    let copy_message = message.clone();
    // Actions stay out of the way until the pointer is over the message; each
    // one is an icon button with its own tooltip rather than a hidden menu. They
    // float over the top-right corner so a hidden row never pads the transcript.
    let mut actions = h_flex()
        .absolute()
        .top_0()
        .right_0()
        .gap_1()
        .rounded(cx.theme().radius)
        .bg(cx.theme().background)
        .invisible()
        .group_hover(group.clone(), |style| style.visible())
        .child(
            Button::new(SharedString::from(format!(
                "copy-message-{id}-{}",
                message.id
            )))
            .xsmall()
            .ghost()
            .cursor_default()
            .icon(IconName::Copy)
            .tooltip("Copy message")
            .on_click(move |_, _, cx| {
                let copy = copy_message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.as_str()),
                        ContentBlock::PeerMessage(peer) => Some(peer.body.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                cx.write_to_clipboard(ClipboardItem::new_string(copy));
            }),
        );
    if message.raw.is_some() {
        actions = actions.child(
            Button::new(SharedString::from(format!(
                "toggle-raw-{id}-{}",
                message.id
            )))
            .xsmall()
            .ghost()
            .cursor_default()
            .icon(Lucide::Braces)
            .tooltip(if raw_expanded {
                "Hide diagnostic payload"
            } else {
                "Show diagnostic payload"
            })
            .on_click(toggle(owner, raw_key)),
        );
    }
    let mut body = v_flex().w_full().min_w_0().gap_2();
    let mut images = h_flex().flex_wrap().gap_2();
    let mut has_images = false;
    // The run's calls sit indented under its header; folded, they are not rendered.
    let mut folded = v_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .pl_3()
        .border_l_1()
        .border_color(cx.theme().border);
    for (part, item) in message.content.iter().enumerate() {
        let identity = format!("{id}:{}:{part}", message.id);
        if let Some(start) = run_start.filter(|start| part >= *start) {
            if part == start
                && head
                && let (Some(run), Some(key)) = (run, run_key.clone())
            {
                body = body.child(tool_run_header(
                    key,
                    run,
                    run_expanded,
                    session,
                    active,
                    owner,
                    cx,
                ));
            }
            if run_expanded {
                folded = folded.child(match item {
                    ContentBlock::ToolCall {
                        id: call_id,
                        name,
                        intent,
                        input,
                    } => tool_call_row(
                        id,
                        call_id,
                        ToolCall {
                            name,
                            intent: intent.as_deref(),
                            input,
                        },
                        session,
                        active,
                        owner,
                        markdown,
                        cx,
                    ),
                    _ => block(item, identity, owner, markdown, cx),
                });
            }
            continue;
        }
        match item {
            ContentBlock::ToolCall {
                id: call_id,
                name,
                intent,
                input,
            } => {
                body = body.child(tool_call_row(
                    id,
                    call_id,
                    ToolCall {
                        name,
                        intent: intent.as_deref(),
                        input,
                    },
                    session,
                    active,
                    owner,
                    markdown,
                    cx,
                ));
            }
            ContentBlock::Image(_) if user_message => {
                has_images = true;
                images = images.child(block(item, identity, owner, markdown, cx));
            }
            ContentBlock::Text(text) if message.role == "custom" => match JobNotice::parse(text) {
                Some(notice) => {
                    for job in &notice.jobs {
                        body = body.child(job_card(
                            format!("job:{id}:{}:{}", message.id, job.id),
                            job,
                            owner,
                            markdown,
                            cx,
                        ));
                    }
                }
                None => {
                    body = body.child(match job_notice::system_notice_body(text) {
                        Some(text) => system_notice(identity, text, owner, markdown, cx),
                        None => block(item, identity, owner, markdown, cx),
                    });
                }
            },
            ContentBlock::Text(_) if message.role == "assistant" => {
                body = body.child(prose(block(item, identity, owner, markdown, cx), cx));
            }
            _ => body = body.child(block(item, identity, owner, markdown, cx)),
        }
    }
    if has_images {
        body = body.child(images);
    }
    if run_expanded {
        // Rows of one run touch, so the rule runs unbroken from the header to the last call.
        body = body.child(if head { folded.pb_1() } else { folded.py_1() });
    }
    if let Some(raw) = message.raw.as_ref().filter(|_| raw_expanded) {
        body = body.child(mono(cx).child(raw.to_string()));
    }
    let body = if user_message {
        body.px_3()
            .py_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .bg(cx.theme().background)
    } else if !matches!(message.role.as_str(), "assistant") {
        body.text_color(cx.theme().muted_foreground)
    } else {
        body
    };
    inset(
        v_flex()
            .w_full()
            .min_w_0()
            .id(SharedString::from(format!("message-{id}-{}", message.id)))
            .group(group)
            .relative()
            .map(|row| match (user_message, run_expanded, head) {
                (true, ..) => row.py_2(),
                (false, true, true) => row.pt_1(),
                (false, true, false) => row,
                (false, false, _) => row.py_1(),
            })
            .child(body)
            .child(actions),
    )
}

/// A completed tool result's content, live activity first, then the history message.
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
/// The list as of the latest `todo` call that answered without an error.
fn current_todo(session: &ConversationSession) -> Option<TodoList> {
    session
        .tool_call_order()
        .iter()
        .rev()
        .filter(|(name, _)| name == "todo")
        .find_map(|(_, id)| TodoList::parse(first_text(call_result(session, id)?)?))
}
/// Jobs started by `task` or async `bash` and not yet settled by a `wait`
/// result, historical `hub` snapshot or job notice, in start order.
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
            settled.extend(job_notice::settled_job_ids(name, text));
            continue;
        }
        if let Some(ContentBlock::ToolCall { input, .. }) = session.tool_call_block(id) {
            jobs.extend(job_notice::job_starts(name, input, text));
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
/// The band above the composer that keeps open tasks and running background
/// jobs in view while the transcript scrolls. Folded by default; the
/// `ToggleTaskList` and `ToggleBackgroundJobs` commands and the chevrons unfold.
fn sticky_dock(
    id: ThreadId,
    session: &ConversationSession,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> Option<AnyElement> {
    let todo = current_todo(session).filter(|list| {
        let (done, total) = list.progress();
        done < total
    });
    let jobs = running_jobs(session);
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
        let ids = jobs
            .iter()
            .map(|job| job.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
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
                    if open {
                        "Hide background jobs"
                    } else {
                        "Show background jobs"
                    },
                ))
                .child(Icon::new(Lucide::Workflow).xsmall())
                .child(div().child(summary))
                .child(div().flex_1().min_w_0().truncate().child(ids))
                .child(Spinner::new().xsmall()),
        );
        if open {
            for job in &jobs {
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
    if let Some(list) = todo {
        let key = format!("dock-todo:{id}");
        let open = is_expanded(owner, &key, cx);
        let (done, total) = list.progress();
        let current = list
            .phases
            .iter()
            .flat_map(|phase| phase.tasks.iter())
            .find(|task| task.status == TodoStatus::InProgress)
            .map(|task| task.content.clone())
            .unwrap_or_default();
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
                    if open {
                        "Hide task list"
                    } else {
                        "Show task list"
                    },
                ))
                .child(Icon::new(Lucide::ListTodo).xsmall())
                .child(div().child(format!("Tasks {done}/{total}")))
                .child(div().flex_1().min_w_0().truncate().child(current)),
        );
        if open {
            section = section.child(div().pl_6().child(todo_checklist(&list, cx)));
        }
        dock = dock.child(section);
    }
    Some(inset(dock))
}

/// Unrecognised notices remain inspectable, without exposing transport markup
/// or pretending that a new harness format is a completed background job.
fn system_notice(
    key: String,
    text: &str,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    let open = is_expanded(owner, &key, cx);
    let mut card = v_flex()
        .id(SharedString::from(format!("{key}:notice")))
        .test_support()
        .w_full()
        .min_w_0()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .py_1()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(Lucide::Info).xsmall())
                .child(div().flex_1().child("System notice"))
                .child(disclosure(
                    key.clone(),
                    open,
                    owner,
                    if open { "Hide notice" } else { "Show notice" },
                )),
        );
    if open {
        card = card.child(
            div()
                .id(SharedString::from(format!("{key}:notice-body")))
                .test_support()
                .min_w_0()
                .px_3()
                .py_2()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .child(guarded_text(key.clone(), text, owner, cx, || {
                    code_block(format!("{key}:text"), text, "text", markdown).into_any_element()
                })),
        );
    }
    card.into_any_element()
}

/// One finished background job from a `<system-notice>`: a task agent's report
/// or a shell command's output. A structured report opens by default; raw
/// output folds behind the disclosure like any tool result.
fn job_card(
    key: String,
    job: &JobResult,
    owner: &WeakEntity<Workspace>,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    let structured = matches!(job.output, JobOutput::Structured(_));
    let has_output = !matches!(job.output, JobOutput::Empty);
    let open = has_output && (structured != is_expanded(owner, &key, cx));
    let status = job.status.as_deref().unwrap_or("Finished");
    let tag = if status.eq_ignore_ascii_case("failed") || status.eq_ignore_ascii_case("error") {
        Tag::danger()
    } else {
        Tag::secondary()
    }
    .outline()
    .small()
    .child(status.to_owned());
    let mut header = h_flex()
        .id(SharedString::from(format!("{key}:header")))
        .test_support()
        .w_full()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(
            Icon::new(if job.agent.is_some() {
                Lucide::Workflow
            } else {
                Lucide::Terminal
            })
            .xsmall(),
        )
        .child(
            div()
                .flex_shrink_0()
                .max_w(relative(0.4))
                .truncate()
                .child(format!("Background job {}", job.id)),
        )
        .child(
            mono(cx)
                .flex_1()
                .truncate()
                .when_some(job.label.as_ref(), |label, text| label.child(text.clone())),
        );
    for detail in [job.agent.as_deref(), job.duration.as_deref()]
        .into_iter()
        .flatten()
    {
        header = header.child(div().flex_shrink_0().child(detail.to_owned()));
    }
    if !has_output {
        header = header.child(div().flex_shrink_0().child("No output"));
    }
    header = header.child(tag);
    if has_output {
        header = header
            .on_click(toggle(owner, key.clone()))
            .child(disclosure(
                key.clone(),
                open,
                owner,
                if open { "Hide output" } else { "Show output" },
            ));
    }
    let mut card = v_flex()
        .id(SharedString::from(format!("{key}:card")))
        .test_support()
        .w_full()
        .min_w_0()
        .child(header);
    if open {
        let mut output = v_flex()
            .id(SharedString::from(format!("{key}:body")))
            .test_support()
            .w_full()
            .min_w_0()
            .gap_2()
            .mb_1()
            .px_3()
            .py_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius);
        if let Some(label) = &job.label {
            output = output.child(mono(cx).child(label.clone()));
        }
        if let Some(full) = &job.full_output {
            output = output.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Preview only; the full output is at {full}")),
            );
        }
        output = match &job.output {
            JobOutput::Structured(value) => output.child(structured_view(
                format!("{key}:structured"),
                value,
                0,
                markdown,
                cx,
            )),
            JobOutput::Text(text) => {
                let language = if text.trim_start().starts_with(['{', '[']) {
                    "json"
                } else {
                    "text"
                };
                let identity = format!("{key}:output");
                let markdown_id = format!("{identity}:markdown");
                output.child(guarded_text(identity, text, owner, cx, || {
                    code_block(markdown_id, text, language, markdown).into_any_element()
                }))
            }
            JobOutput::Empty => output,
        };
        card = card.child(output);
    }
    card.into_any_element()
}
/// Generic reading view for a job's JSON report: keys become captions, strings
/// render as Markdown, string lists as bullets, nested objects indent. Beyond a
/// few levels the raw JSON is shown so nothing is hidden.
fn structured_view(
    identity: String,
    value: &Value,
    depth: usize,
    markdown: Markdown,
    cx: &App,
) -> AnyElement {
    const MAX_DEPTH: usize = 3;
    match value {
        Value::Object(fields) if depth < MAX_DEPTH && !fields.is_empty() => {
            let mut column = v_flex().gap_2().min_w_0();
            for (key, value) in fields {
                let caption = humanise_key(key);
                column = column.child(
                    v_flex()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(caption),
                        )
                        .child(structured_view(
                            format!("{identity}:field:{key}"),
                            value,
                            depth + 1,
                            markdown,
                            cx,
                        )),
                );
            }
            column.into_any_element()
        }
        Value::Array(items)
            if depth < MAX_DEPTH
                && !items.is_empty()
                && items.iter().all(|item| item.is_string()) =>
        {
            let list = items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| format!("- {}", item.replace('\n', "\n  ")))
                .collect::<Vec<_>>()
                .join("\n");
            markdown_text(identity, list, markdown).into_any_element()
        }
        Value::Array(items)
            if depth < MAX_DEPTH
                && !items.is_empty()
                && items.iter().all(|item| item.is_object()) =>
        {
            let mut column = v_flex().gap_2().min_w_0();
            for (index, item) in items.iter().enumerate() {
                column = column.child(
                    div()
                        .pl_3()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .child(structured_view(
                            format!("{identity}:item:{index}"),
                            item,
                            depth + 1,
                            markdown,
                            cx,
                        )),
                );
            }
            column.into_any_element()
        }
        Value::String(text) => markdown_text(identity, text.clone(), markdown).into_any_element(),
        Value::Null => div()
            .text_color(cx.theme().muted_foreground)
            .child("None")
            .into_any_element(),
        Value::Bool(_) | Value::Number(_) => mono(cx).child(value.to_string()).into_any_element(),
        Value::Object(_) | Value::Array(_) => code_block(
            identity,
            &serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
            "json",
            markdown,
        )
        .into_any_element(),
    }
}
/// `tests_added` → `Tests added`, `main_required` → `Main required`.
fn humanise_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for (index, ch) in key.chars().enumerate() {
        let ch = if ch == '_' || ch == '-' { ' ' } else { ch };
        if index == 0 {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// `harness` decides whether a permission request offers switching to its auto mode.
fn request_card(
    id: ThreadId,
    harness: &str,
    request: &UserRequest,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let mut card = v_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .p_3()
        .border_1()
        .border_color(cx.theme().ring)
        .rounded(cx.theme().radius)
        .child(
            div()
                .id(SharedString::from(format!("request-{}-title", request.id)))
                .max_h(rems(4.))
                .overflow_y_scroll()
                .font_weight(FontWeight::SEMIBOLD)
                .child(request.title.clone()),
        );
    if !request.text.is_empty() {
        card = card.child(
            div()
                .id(SharedString::from(format!("request-{}-body", request.id)))
                .test_support()
                .max_h(rems(8.))
                .overflow_y_scroll()
                .text_sm()
                .child(request.text.clone()),
        );
    }
    let workspace = owner.upgrade();
    let form = workspace.as_ref().and_then(|workspace| {
        workspace
            .read(cx)
            .extension
            .as_ref()
            .filter(|form| form.thread == id && form.request.id == request.id)
    });
    let inline_answer = form.is_some_and(|form| form.inline_answer);
    let busy = form.is_some_and(|form| form.submitting);
    let mut choices = v_flex()
        .id(SharedString::from(format!(
            "request-{}-choices",
            request.id
        )))
        .test_support()
        .w_full()
        .min_h_0()
        .max_h(rems(14.))
        .overflow_y_scroll()
        .gap_1();
    let mut answers: Vec<(String, Option<String>, UserAnswer)> = Vec::new();
    if request.kind == UserRequestKind::Permission
        && crate::harness::auto_permission_mode(harness).is_some()
    {
        answers.push((
            "Yes and switch to auto".into(),
            Some("Approve all permission asks already open".into()),
            UserAnswer::PermissionAuto,
        ));
    }
    match request.kind {
        UserRequestKind::Confirm => {
            answers.push(("Confirm".into(), None, UserAnswer::Confirmed(true)));
            answers.push(("Decline".into(), None, UserAnswer::Confirmed(false)));
        }
        UserRequestKind::Select | UserRequestKind::Permission | UserRequestKind::Question => {
            for option in &request.options {
                answers.push((
                    option.value.clone(),
                    option.description.clone(),
                    UserAnswer::Value(option.value.clone()),
                ));
            }
        }
        UserRequestKind::Input | UserRequestKind::Editor => {
            let owner = owner.clone();
            let request = request.clone();
            choices = choices.child(
                Button::new(SharedString::from(format!("answer-{}", request.id)))
                    .small()
                    .outline()
                    .w_full()
                    .label("Answer…")
                    .disabled(inline_answer)
                    .on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.open_extension(id, request.clone(), window, cx)
                        });
                    }),
            );
        }
    }
    for (index, (label, description, answer)) in answers.into_iter().enumerate() {
        let owner = owner.clone();
        let request_id = request.id.clone();
        let mut label = Label::new(label).w_full().min_w_0().whitespace_normal();
        if let Some(description) = description {
            label = label.secondary(description);
        }
        choices = choices.child(
            Button::new(SharedString::from(format!(
                "request-{}-{index}",
                request.id
            )))
            .small()
            .outline()
            .w_full()
            .h_auto()
            .py_1p5()
            .child(label)
            .disabled(busy)
            .on_click(move |_, window, cx| {
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.back_from_custom_answer(id, &request_id, window, cx);
                    workspace.state.update(cx, |state, cx| {
                        state.respond(id, request_id.clone(), answer.clone());
                        cx.notify();
                    })
                });
            }),
        );
    }
    card = card.child(choices);
    if inline_answer
        && matches!(
            request.kind,
            UserRequestKind::Question | UserRequestKind::Input
        )
        && let Some(input) = owner
            .upgrade()
            .and_then(|workspace| workspace.read(cx).extension.as_ref()?.input.clone())
    {
        let submit = owner.clone();
        let back = owner.clone();
        let request_id = request.id.clone();
        card = card.child(
            v_flex()
                .id(SharedString::from(format!(
                    "request-{}-inline-editor",
                    request.id
                )))
                .key_context("DrostjeForm InlineRequestAnswer")
                .on_action(move |_: &super::commands::Dismiss, window, cx| {
                    cx.stop_propagation();
                    let _ = back.update(cx, |workspace, cx| {
                        workspace.back_from_custom_answer(id, &request_id, window, cx)
                    });
                })
                .on_action(move |_: &super::commands::Confirm, window, cx| {
                    cx.stop_propagation();
                    let _ =
                        submit.update(cx, |workspace, cx| workspace.confirm_extension(window, cx));
                })
                .gap_2()
                .child(
                    div()
                        .key_context("NativeInput")
                        .child(Input::new(&input).disabled(busy)),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new(SharedString::from(format!("request-{}-back", request.id)))
                                .small()
                                .ghost()
                                .label(if request.kind == UserRequestKind::Input {
                                    "Back to request"
                                } else {
                                    "Back to choices"
                                })
                                .disabled(busy)
                                .on_click({
                                    let owner = owner.clone();
                                    let request_id = request.id.clone();
                                    move |_, window, cx| {
                                        let _ = owner.update(cx, |workspace, cx| {
                                            workspace.back_from_custom_answer(
                                                id,
                                                &request_id,
                                                window,
                                                cx,
                                            )
                                        });
                                    }
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("request-{}-send", request.id)))
                                .small()
                                .primary()
                                .label("Send answer")
                                .disabled(busy)
                                .loading(busy)
                                .on_click({
                                    let owner = owner.clone();
                                    move |_, window, cx| {
                                        let _ = owner.update(cx, |workspace, cx| {
                                            workspace.confirm_extension(window, cx)
                                        });
                                    }
                                }),
                        ),
                ),
        );
    }
    if request.kind == UserRequestKind::Question {
        let custom = owner.clone();
        let request = request.clone();
        card = card.child(
            Button::new(SharedString::from(format!("request-{}-custom", request.id)))
                .small()
                .outline()
                .w_full()
                .label("Custom answer…")
                .disabled(inline_answer)
                .on_click(move |_, window, cx| {
                    let _ = custom.update(cx, |workspace, cx| {
                        workspace.open_custom_answer(id, request.clone(), window, cx)
                    });
                }),
        );
    }
    if let Some(error) = form.and_then(|form| form.error.as_ref()) {
        card = card.child(
            gpui_kit::component::alert::Alert::error(
                SharedString::from(format!("request-{}-error", request.id)),
                error.clone(),
            )
            .small(),
        );
    }
    let cancel = owner.clone();
    let request_id = request.id.clone();
    card.child(
        h_flex().justify_end().child(
            Button::new(SharedString::from(format!("request-{}-cancel", request.id)))
                .small()
                .ghost()
                .label("Cancel")
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    let _ = cancel.update(cx, |workspace, cx| {
                        if workspace.extension.as_ref().is_some_and(|form| {
                            form.inline_answer && form.thread == id && form.request.id == request_id
                        }) {
                            workspace.close_extension(true, window, cx);
                        } else {
                            workspace.state.update(cx, |state, cx| {
                                state.respond(
                                    id,
                                    request_id.clone(),
                                    UserAnswer::Cancelled { timed_out: false },
                                );
                                cx.notify();
                            })
                        }
                    });
                }),
        ),
    )
    .into_any_element()
}

/// The centre pane while no thread is open: name the next step and offer it.
const WORDMARK: &str = "DROSTJE.";

/// The brand in its display face, the full stop in the theme's signature color.
fn wordmark(cx: &App) -> impl IntoElement {
    let stop = WORDMARK.len() - 1;
    div()
        .debug_selector(|| "wordmark".into())
        .font_family(theme::DISPLAY_FONT)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(rems(2.25))
        .line_height(relative(1.))
        .child(StyledText::new(WORDMARK).with_highlights([(
            stop..WORDMARK.len(),
            HighlightStyle {
                color: Some(theme::signature(cx)),
                ..Default::default()
            },
        )]))
}

pub fn welcome(state: &AppState, owner: &WeakEntity<Workspace>, cx: &App) -> AnyElement {
    let has_project = state.project().is_some();
    let actions = h_flex()
        .gap_2()
        .flex_wrap()
        .justify_center()
        .when(has_project, |row| {
            row.child(
                button(
                    "empty-new-conversation",
                    Command::NewConversation,
                    owner,
                    state,
                )
                .outline()
                .debug_selector(|| "empty-new-conversation".into()),
            )
            .child(
                button("empty-new-factory", Command::NewFactoryThread, owner, state)
                    .outline()
                    .debug_selector(|| "empty-new-factory".into()),
            )
        })
        .when(!has_project, |row| {
            row.child(
                button("empty-add-project", Command::AddProject, owner, state)
                    .outline()
                    .debug_selector(|| "empty-add-project".into()),
            )
        });
    v_flex()
        .id("conversation-welcome")
        .size_full()
        .min_w_0()
        .min_h_0()
        .relative()
        .text_color(cx.theme().foreground)
        .children(super::grain::layer(super::theme::grain(cx)))
        .items_center()
        .justify_center()
        .p_6()
        .child(
            v_flex()
                .w_full()
                .max_w(rems(30.))
                .items_center()
                .gap_3()
                .text_center()
                .child(wordmark(cx))
                .child(div().text_lg().child(if has_project {
                    "No thread selected"
                } else {
                    "Open a project"
                }))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if has_project {
                            "Choose a thread from Projects, or start a new one."
                        } else {
                            "Add a Git working tree to start a Conversation or Factory thread."
                        }),
                )
                .child(actions),
        )
        .into_any_element()
}

pub(super) fn render(
    state: &AppState,
    id: ThreadId,
    owner: &WeakEntity<Workspace>,
    scroll: &ScrollHandle,
    transcript: &Entity<TranscriptView>,
    network: Option<&Entity<super::network::NetworkPanel>>,
    cx: &App,
) -> AnyElement {
    render_content(state, id, owner, scroll, transcript, network, cx)
}

/// Rem height the queue bar adds to the composer: a header plus up to four rows; longer
/// queues scroll inside the bar.
fn queued_messages_height(
    thread: &crate::model::Thread,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> f32 {
    if thread.queued.is_empty() {
        return 0.;
    }
    if is_expanded(owner, &format!("queue-collapsed:{}", thread.id), cx) {
        return QUEUE_HEADER_HEIGHT;
    }
    QUEUE_HEADER_HEIGHT + QUEUE_ROW_HEIGHT * thread.queued.len().min(QUEUE_VISIBLE_ROWS) as f32
}
const QUEUE_HEADER_HEIGHT: f32 = 2.25;
const QUEUE_ROW_HEIGHT: f32 = 2.;
const QUEUE_VISIBLE_ROWS: usize = 4;

/// Zed-style queue above the composer: a collapsible header with the count and Clear all,
/// then one row per waiting prompt with Remove, Edit and Send now. The front row is the
/// one the queue sends next. `dispatch_reason` disables Send now while the conversation is
/// idle but unable to start a turn; during a turn, Send now stops that turn first.
fn queued_messages(
    thread: &crate::model::Thread,
    active: bool,
    dispatch_reason: Option<String>,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let id = thread.id;
    let collapsed_key = format!("queue-collapsed:{id}");
    let collapsed = is_expanded(owner, &collapsed_key, cx);
    let count = thread.queued.len();
    let clear = owner.clone();
    let header = h_flex()
        .items_center()
        .gap_1()
        .px_2()
        .h(rems(QUEUE_HEADER_HEIGHT))
        .child(disclosure(
            collapsed_key,
            !collapsed,
            owner,
            if collapsed {
                "Show queued messages"
            } else {
                "Hide queued messages"
            },
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(match count {
                    1 => "1 queued message".to_owned(),
                    count => format!("{count} queued messages"),
                }),
        )
        .child(
            Button::new("clear-queued")
                .xsmall()
                .ghost()
                .label("Clear all")
                .tooltip_with_action(
                    "Remove every queued message…",
                    &ClearQueuedMessages,
                    Some("Thread"),
                )
                .on_click(move |_, window, cx| {
                    let _ = clear.update(cx, |workspace, cx| {
                        workspace.clear_queued_messages(window, cx)
                    });
                }),
        );
    let mut bar = v_flex()
        .id(SharedString::from(format!("queued-messages-{id}")))
        .debug_selector(|| "queued-messages".into())
        .test_support()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(header);
    if collapsed {
        return bar.into_any_element();
    }
    let mut rows = v_flex()
        .id(SharedString::from(format!("queued-rows-{id}")))
        .max_h(rems(QUEUE_ROW_HEIGHT * QUEUE_VISIBLE_ROWS as f32))
        .overflow_y_scroll()
        .w_full()
        .min_w_0();
    for (index, message) in thread.queued.iter().enumerate() {
        let message_id = message.id;
        let remove = owner.clone();
        let edit = owner.clone();
        let send = owner.clone();
        let next = index == 0;
        let send_reason = if active {
            None
        } else {
            dispatch_reason.clone()
        };
        let attachments = message.attachments.len();
        rows = rows.child(
            h_flex()
                .id(SharedString::from(format!("queued-{message_id}")))
                .debug_selector(move || {
                    if next {
                        "queued-row-next".into()
                    } else {
                        "queued-row".into()
                    }
                })
                .items_center()
                .gap_2()
                .px_2()
                .h(rems(QUEUE_ROW_HEIGHT))
                .child(Icon::new(Lucide::CircleDot).xsmall().text_color(if next {
                    cx.theme().primary
                } else {
                    cx.theme().muted_foreground
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(message.preview()),
                )
                .when(attachments > 0, |row| {
                    row.child(Tag::secondary().xsmall().child(match attachments {
                        1 => "1 image".to_owned(),
                        count => format!("{count} images"),
                    }))
                })
                .child(
                    Button::new(SharedString::from(format!("remove-queued-{message_id}")))
                        .xsmall()
                        .ghost()
                        .icon(Lucide::Trash)
                        .tooltip("Remove")
                        .on_click(move |_, _, cx| {
                            let _ = remove.update(cx, |workspace, cx| {
                                workspace.state.update(cx, |state, cx| {
                                    state.remove_queued(id, message_id);
                                    cx.notify();
                                })
                            });
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("edit-queued-{message_id}")))
                        .xsmall()
                        .ghost()
                        .icon(Lucide::Pencil)
                        .tooltip("Edit in composer")
                        .on_click(move |_, window, cx| {
                            let _ = edit.update(cx, |workspace, cx| {
                                workspace.edit_queued(id, message_id, window, cx)
                            });
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("send-queued-{message_id}")))
                        .xsmall()
                        .outline()
                        .label("Send now")
                        .disabled(send_reason.is_some())
                        .tooltip(send_reason.unwrap_or_else(|| {
                            if active {
                                "Stop the running turn and send this next".to_owned()
                            } else {
                                "Send this now".to_owned()
                            }
                        }))
                        .on_click(move |_, _, cx| {
                            let _ = send.update(cx, |workspace, cx| {
                                workspace.send_queued_now(id, message_id, cx)
                            });
                        }),
                ),
        );
    }
    bar = bar.child(rows);
    bar.into_any_element()
}

fn frame(
    layout: super::thread_sidebar::SidebarLayout,
    root: Stateful<Div>,
    history: Div,
    sidebar: AnyElement,
    composer: Option<super::composer::ComposerSplit>,
) -> AnyElement {
    // The composer shares the history column so both centre over the same width;
    // the sidebar spans the full height beside them.
    let content = match composer {
        Some(mut split) => {
            split.history = history.into_any_element();
            split.into_any_element()
        }
        None => history.into_any_element(),
    };
    root.child(layout.wrap(content, sidebar)).into_any_element()
}

fn render_content(
    state: &AppState,
    id: ThreadId,
    owner: &WeakEntity<Workspace>,
    output_scroll: &ScrollHandle,
    transcript: &Entity<TranscriptView>,
    network: Option<&Entity<super::network::NetworkPanel>>,
    cx: &App,
) -> AnyElement {
    let drop_owner = owner.clone();
    let sidebar = super::thread_sidebar::render(state, id, true, owner, output_scroll, network, cx);
    let layout = super::thread_sidebar::SidebarLayout::new(state, id, owner, cx);
    let root = v_flex()
        .id(SharedString::from(format!("conversation-view-{id}")))
        .size_full()
        .min_w_0()
        .min_h_0()
        .text_color(cx.theme().foreground)
        .on_drop(move |paths: &ExternalPaths, window, cx| {
            let _ = drop_owner.update(cx, |workspace, cx| {
                if workspace.editor_override.is_none()
                    && workspace.state.read(cx).selected_thread == Some(id)
                {
                    workspace.import_images(id, paths.paths().to_vec(), window, cx);
                }
            });
        });
    let mut history = v_flex().flex_1().min_w_0().min_h_0().gap_2().pb_2();
    let Some(thread) = state.snapshot.threads.iter().find(|thread| thread.id == id) else {
        return frame(
            layout,
            root,
            history.child(div().p_3().child("This thread is no longer available.")),
            sidebar,
            None,
        );
    };
    let Some(project) = state
        .snapshot
        .projects
        .iter()
        .find(|project| project.id == thread.project_id)
    else {
        return frame(
            layout,
            root,
            history.child(
                div()
                    .p_3()
                    .child("This thread's project is no longer available."),
            ),
            sidebar,
            None,
        );
    };
    let active = state.active_threads.contains(&id);
    let host_reason = state.thread_host_selection_reason(id).or_else(|| {
        (state.selected_thread != Some(id)).then(|| "Select this Conversation first.".to_owned())
    });
    let host_owner = owner.clone();
    history = history.child(
        h_flex()
            .id("conversation-execution-location")
            .test_support()
            .items_center()
            .gap_2()
            .px_3()
            .flex_wrap()
            .child(
                Button::new("switch-execution-host")
                    .small()
                    .ghost()
                    .label(state.thread_host_label(id))
                    .disabled(host_reason.is_some())
                    .tooltip(
                        host_reason
                            .unwrap_or_else(|| Command::SwitchExecutionHost.label().to_owned()),
                    )
                    .on_click(move |_, window, cx| {
                        let _ = host_owner.update(cx, |workspace, cx| {
                            workspace.execute(Command::SwitchExecutionHost, window, cx)
                        });
                    }),
            )
            .child(gpui_kit::component::label::Label::new(
                state.thread_host_status(id),
            )),
    );
    if thread.remote.is_none()
        && let Some(Err(error)) = state.repositories.get(&project.id)
    {
        history = history.child(
            Alert::error(
                "conversation-root-error",
                format!("Root unavailable: {error}"),
            )
            .small(),
        );
    }
    let mut statuses: Vec<String> = Vec::new();
    if let Some(session) = state.conversations.get(&id) {
        if let Some(error) = &session.error {
            history = history.child(inset(
                Alert::error("conversation-session-error", error.clone()).small(),
            ));
        }
        statuses = session.statuses.values().cloned().collect();
        history = history.child(
            div().relative().flex_1().min_h(rems(4.)).min_w_0().child(
                transcript
                    .clone()
                    .cached(StyleRefinement::default().absolute().size_full()),
            ),
        );
        // A fresh Conversation offers its harness up front: the first thing to decide.
        if state.is_untouched(thread) && state.switch_harness_reason_for(id).is_none() {
            let switch = owner.clone();
            history = history.child(inset(
                h_flex()
                    .id("untouched-harness-hint")
                    .test_support()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Starts on {}. New conversation again picks another harness.",
                        crate::harness::display_name(&thread.harness)
                    ))
                    .child(
                        Button::new("untouched-switch-harness")
                            .small()
                            .ghost()
                            .label(Command::SwitchHarness.label())
                            .tooltip_with_action(
                                Command::SwitchHarness.label(),
                                &SwitchHarness,
                                Some("Workspace"),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = switch.update(cx, |workspace, cx| {
                                    workspace.execute(Command::SwitchHarness, window, cx)
                                });
                            }),
                    ),
            ));
        }
        for request in session.requests.values() {
            history = history.child(inset(request_card(id, &thread.harness, request, owner, cx)));
        }
        for (index, (key, lines)) in session.widgets.iter().enumerate() {
            if key.starts_with("Proposed editor replacement") {
                let owner = owner.clone();
                let text = lines.join("\n");
                history = history.child(inset(
                    Button::new(("apply-editor-proposal", index))
                        .small()
                        .ghost()
                        .label("Review replacement…")
                        .on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |workspace, cx| {
                                workspace.replace_draft_reviewed(
                                    id,
                                    text.clone(),
                                    None,
                                    false,
                                    window,
                                    cx,
                                )
                            });
                        }),
                ));
                continue;
            }
            // Widgets are OMP's live panels (plans, progress); open unless folded away.
            let folded_key = format!("widget-folded:{id}:{key}");
            let folded = is_expanded(owner, &folded_key, cx);
            history = history.child(inset(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(disclosure(
                                folded_key,
                                !folded,
                                owner,
                                if folded { "Show widget" } else { "Hide widget" },
                            ))
                            .child(key.clone()),
                    )
                    .child(
                        Collapsible::new()
                            .open(!folded)
                            .content(mono(cx).pl_6().child(lines.join("\n"))),
                    ),
            ));
        }
        if let Some(dock) = sticky_dock(id, session, owner, cx) {
            history = history.child(dock);
        }
        // Input the agent waits for sits directly above the composer.
        if let Some(pane) = owner
            .upgrade()
            .and_then(|workspace| workspace.read(cx).docked_artifact(id))
        {
            history = history.child(inset(
                div()
                    .id("composer-artifact")
                    .debug_selector(|| "composer-artifact".into())
                    .test_support()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .child(pane),
            ));
        }
    } else {
        history = history.child(div().flex_1().p_4().child(if thread.archived {
            "Archived conversation has no provider history.".into()
        } else {
            format!(
                "Opening {} session…",
                crate::harness::display_name(&thread.harness)
            )
        }));
    }
    // A submission that is simply in flight is already visible through the
    // disabled Send control and the sidebar state; only an unresolved one after
    // a failure or restart needs attention here.
    let unresolved_submission = thread.submission.as_ref().filter(|submission| {
        let submission_state = submission
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        !active || !matches!(submission_state, "pending" | "accepted")
    });
    if let Some(submission) = unresolved_submission {
        let restore = owner.clone();
        let dismiss = owner.clone();
        let submission_state = submission
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let detail = submission
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map(|error| format!(" {error}."))
            .unwrap_or_default();
        history = history.child(inset(
            v_flex()
                .gap_1()
                .child(
                    Alert::warning(
                        "unresolved-submission",
                        format!("Submission {submission_state}.{detail} The draft was not resent."),
                    )
                    .small(),
                )
                .when(!active, |view| {
                    view.child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("restore-submitted")
                                    .small()
                                    .ghost()
                                    .label("Restore submitted draft…")
                                    .on_click(move |_, window, cx| {
                                        let _ = restore.update(cx, |workspace, cx| {
                                            workspace.restore_submitted(id, window, cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("dismiss-submitted")
                                    .small()
                                    .ghost()
                                    .label("Keep current draft")
                                    .on_click(move |_, _, cx| {
                                        let _ = dismiss.update(cx, |workspace, cx| {
                                            workspace.state.update(cx, |state, cx| {
                                                state.acknowledge_recovery(id);
                                                cx.notify();
                                            })
                                        });
                                    }),
                            ),
                    )
                }),
        ));
    }
    if thread.archived || project.archived {
        // An archived thread in a live project can come back; an archived project cannot here.
        let unarchive = (thread.archived && !project.archived).then(|| {
            let reason = state.unarchive_thread_reason_for(id);
            let owner = owner.clone();
            Button::new("unarchive-thread")
                .small()
                .ghost()
                .label(Command::UnarchiveThread.label())
                .disabled(reason.is_some())
                .map(|button| match reason {
                    Some(reason) => button.tooltip(reason),
                    None => button.tooltip_with_action(
                        Command::UnarchiveThread.label(),
                        &UnarchiveThread,
                        Some("Workspace"),
                    ),
                })
                .on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |workspace, cx| workspace.unarchive_thread(id, cx));
                })
        });
        return frame(
            layout,
            root,
            history.child(inset(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Archived history · read-only"),
                    )
                    .children(unarchive),
            )),
            sidebar,
            None,
        );
    }
    // One region under a single divider: attachments, the editor and its
    // toolbar stack without a box of their own.
    let mut composer = v_flex()
        .id("composer-draft-content")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll();
    if !thread.queued.is_empty() {
        composer = composer.child(queued_messages(
            thread,
            active,
            state.queued_dispatch_reason(id),
            owner,
            cx,
        ));
    }
    let mut previews = h_flex()
        .id("draft-attachments")
        .overflow_x_scroll()
        .gap_3()
        .px_2()
        .pt_2();
    for image in &thread.attachments {
        let remove = owner.clone();
        let attachment_id = image.id;
        previews = previews.child(
            v_flex()
                .gap_1()
                .child(image_thumbnail(image, owner, cx))
                .child(
                    Button::new(SharedString::from(format!("remove-{attachment_id}")))
                        .small()
                        .ghost()
                        .label("Remove")
                        .on_click(move |_, _, cx| {
                            let _ = remove.update(cx, |workspace, cx| {
                                workspace.state.update(cx, |state, cx| {
                                    if let Some(mut thread) =
                                        state.snapshot.threads.iter().find(|t| t.id == id).cloned()
                                    {
                                        thread.attachments.retain(|a| a.id != attachment_id);
                                        state.save_thread(thread);
                                    }
                                    cx.notify();
                                })
                            });
                        }),
                ),
        );
    }
    if !thread.attachments.is_empty() {
        composer = composer.child(previews);
    }
    // One Neovim instance: the selected conversation hosts it, every other visible
    // conversation shows its saved draft and how to take the editor over.
    let hosts_editor = owner.upgrade().is_some_and(|workspace| {
        let workspace = workspace.read(cx);
        workspace.editor.is_some()
            && workspace.editor_override.is_none()
            && workspace.editor_selection == Some(crate::model::DraftKey::Thread(id))
    });
    let editor = owner
        .upgrade()
        .and_then(|workspace| workspace.read(cx).editor.clone())
        .filter(|_| hosts_editor);
    composer = composer.child(match editor {
        Some(editor) => div()
            .flex_1()
            .min_h(rems(2.))
            .child(editor)
            .into_any_element(),
        None => div()
            .id(SharedString::from(format!("draft-preview-{id}")))
            .flex_1()
            .min_h(rems(2.))
            .overflow_y_scroll()
            .px_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .whitespace_normal()
            .child(if thread.draft.trim().is_empty() {
                format!(
                    "Message {} — press i to write",
                    crate::harness::display_name(&thread.harness)
                )
            } else {
                format!("{}\n\nPress i to edit", thread.draft)
            })
            .into_any_element(),
    });
    // Zed-style toolbar: context actions left, meters, model settings and send right.
    let mut leading = h_flex().flex_1().min_w_0().items_center().gap_1();
    let mut trailing = h_flex().flex_shrink_0().items_center().gap_1();
    let attach = owner.clone();
    let menu_owner = owner.clone();
    let no_images = thread.attachments.is_empty();
    leading = leading
        .child(
            Button::new("attach-images")
                .small()
                .ghost()
                .icon(Lucide::Plus)
                .tooltip("Attach images…")
                .disabled(state.storage_error.is_some())
                .on_click(move |_, window, cx| {
                    let _ = attach.update(cx, |workspace, cx| workspace.attach_images(window, cx));
                }),
        )
        .child(
            Button::new("composer-actions")
                .small()
                .ghost()
                .icon(IconName::Ellipsis)
                .tooltip("Conversation actions")
                .dropdown_menu(move |menu, _, _| {
                    let details = menu_owner.clone();
                    let optimize = menu_owner.clone();
                    let recovery = menu_owner.clone();
                    menu.item(PopupMenuItem::new("Thread details").on_click(
                        move |_, window, cx| {
                            let _ = details.update(cx, |workspace, cx| {
                                workspace.toggle_thread_details(id, window, cx)
                            });
                        },
                    ))
                    .item(
                        PopupMenuItem::new("Optimize images")
                            .disabled(no_images)
                            .on_click(move |_, window, cx| {
                                let _ = optimize.update(cx, |workspace, cx| {
                                    workspace.optimize_images(window, cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Copy recovery").on_click(move |_, _, cx| {
                            let _ = recovery
                                .update(cx, |workspace, cx| workspace.copy_draft_recovery(cx));
                        }),
                    )
                }),
        );
    // The harness name doubles as its switcher; once a turn exists it only names it.
    let switch_reason = state.switch_harness_reason_for(id).or_else(|| {
        (state.selected_thread != Some(id)).then(|| "Select this conversation first.".to_owned())
    });
    let switch = owner.clone();
    leading = leading.child(
        Button::new("switch-harness")
            .small()
            .ghost()
            .label(crate::harness::display_name(&thread.harness).to_owned())
            .disabled(switch_reason.is_some())
            .map(|button| match switch_reason {
                Some(reason) => button.tooltip(reason),
                None => button.tooltip_with_action(
                    Command::SwitchHarness.label(),
                    &SwitchHarness,
                    Some("Workspace"),
                ),
            })
            .on_click(move |_, window, cx| {
                let _ = switch.update(cx, |workspace, cx| {
                    workspace.execute(Command::SwitchHarness, window, cx)
                });
            }),
    );
    // The harness status line trails the context actions, muted and truncated.
    leading = leading.child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(statuses.join(" · ")),
    );
    if let Some(session) = state.conversations.get(&id) {
        trailing = trailing.children(usage_meters(
            session.context_usage,
            &session.limits,
            SystemTime::now(),
            cx,
        ));
    }
    // The wordmark's full stop, breathing beside the composer while a turn runs.
    if active {
        trailing = trailing.children(activity::mark(
            "turn-running".into(),
            ThreadActivity::Running,
            cx,
        ));
    }
    if let Some(workspace) = owner.upgrade() {
        let launch_settings_reason = state.remote_launch_settings_reason(id);
        if let Some(picker) = workspace
            .read(cx)
            .model_picker
            .as_ref()
            .filter(|picker| picker.thread_id == id)
        {
            if !picker.options.is_empty() {
                trailing = trailing.child(
                    div()
                        .min_w_0()
                        .max_w(rems(20.))
                        .key_context("NativeInput")
                        .child(
                            Select::new(&picker.select)
                                .small()
                                .appearance(false)
                                .disabled(active)
                                .placeholder("Select model"),
                        ),
                );
            }
        }
        if let Some(picker) = workspace
            .read(cx)
            .effort_picker
            .as_ref()
            .filter(|picker| picker.thread_id == id)
        {
            trailing = trailing.child(
                div()
                    .id("conversation-effort")
                    .when_some(launch_settings_reason.clone(), |view, reason| {
                        view.tooltip(move |window, cx| {
                            Tooltip::new(reason.clone()).build(window, cx)
                        })
                    })
                    .min_w_0()
                    .max_w(rems(10.))
                    .key_context("NativeInput")
                    .child(
                        Select::new(&picker.select)
                            .small()
                            .appearance(false)
                            .disabled(active || launch_settings_reason.is_some())
                            .placeholder("Effort"),
                    ),
            );
        }
        if let Some(picker) = workspace
            .read(cx)
            .permission_mode_picker
            .as_ref()
            .filter(|picker| picker.thread_id == id)
        {
            trailing = trailing.child(
                div()
                    .id("conversation-permissions")
                    .when_some(launch_settings_reason.clone(), |view, reason| {
                        view.tooltip(move |window, cx| {
                            Tooltip::new(reason.clone()).build(window, cx)
                        })
                    })
                    .min_w_0()
                    .max_w(rems(12.))
                    .key_context("NativeInput")
                    .child(
                        Select::new(&picker.select)
                            .small()
                            .appearance(false)
                            .disabled(active || launch_settings_reason.is_some())
                            .placeholder("Permissions"),
                    ),
            );
        }
    }
    // Claude Code reports its model only after the first prompt, so an empty list is not a failure.
    let unavailable = state
        .conversations
        .get(&id)
        .is_none_or(|session| session.error.is_some());
    if unavailable {
        trailing = trailing.child(
            button(
                "retry-model-discovery",
                Command::RetryConversation,
                owner,
                state,
            )
            .label(if thread.remote.is_some() {
                "Reconnect Host"
            } else {
                "Models unavailable · Retry"
            })
            .disabled(
                state.selected_thread != Some(id)
                    || Command::RetryConversation.disabled_reason(state).is_some(),
            ),
        );
    }
    if active {
        let queue = owner.clone();
        let queue_reason = state.queue_reason(id).or_else(|| {
            (!hosts_editor).then(|| "Focus this conversation to edit and queue".to_owned())
        });
        trailing = trailing.child(
            Button::new("queue-prompt")
                .small()
                .ghost()
                .icon(Lucide::ListPlus)
                .label("Queue")
                .disabled(queue_reason.is_some())
                .tooltip(queue_reason.unwrap_or_else(|| "Queue for after this turn".to_owned()))
                .on_click(move |_, window, cx| {
                    let _ =
                        queue.update(cx, |workspace, cx| workspace.queue_prompt(id, window, cx));
                }),
        );
        let stop = owner.clone();
        trailing = trailing.child(
            Button::new("interrupt-prompt")
                .small()
                .danger()
                .outline()
                .icon(Lucide::Square)
                .label("Stop")
                .tooltip("Interrupt turn")
                .on_click(move |_, _, cx| {
                    let _ = stop.update(cx, |workspace, cx| {
                        workspace.state.update(cx, |state, cx| {
                            state.interrupt(id);
                            cx.notify();
                        });
                    });
                }),
        );
    } else {
        let send = owner.clone();
        let reason = state.submit_reason(id).or_else(|| {
            (!hosts_editor).then(|| "Focus this conversation to edit and send".to_owned())
        });
        trailing = trailing.child(
            Button::new("send-prompt")
                .small()
                .primary()
                .icon(Lucide::ArrowUp)
                .disabled(reason.is_some())
                .tooltip(reason.unwrap_or_else(|| "Send".to_owned()))
                .on_click(move |_, window, cx| {
                    let _ = send.update(cx, |workspace, cx| workspace.submit_prompt(window, cx));
                }),
        );
    }
    let controls = h_flex()
        .flex_shrink_0()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .child(leading)
        .child(trailing);
    // A definite bounded height propagates through the inset wrapper so the editor shrinks,
    // rather than overflowing the actions or introducing a second transcript scroller.
    let preferred_height =
        13. + if thread.attachments.is_empty() {
            0.
        } else {
            9.
        } + queued_messages_height(thread, owner, cx);
    frame(
        layout,
        root,
        history,
        sidebar,
        Some(super::composer::ComposerSplit {
            id,
            owner: owner.clone(),
            height: state.thread_composer_height(id),
            preferred_rems: preferred_height,
            history: div().into_any_element(),
            composer: h_flex()
                .id("composer-row")
                .debug_selector(|| "composer-row".into())
                .test_support()
                .w_full()
                .flex_shrink_0()
                .justify_center()
                .h_full()
                .min_h_0()
                .relative()
                .border_t_1()
                .border_color(cx.theme().border)
                .pt_2()
                .pb_2()
                // Sweeps along the top border while a turn runs.
                .when(active, |row| {
                    row.child(
                        div()
                            .debug_selector(|| "composer-sweep".into())
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(rems(-0.0625))
                            .child(activity::sweep(cx)),
                    )
                })
                .child(
                    content_column().h_full().min_h_0().child(
                        v_flex()
                            .id("composer-content")
                            .debug_selector(|| "composer-content".into())
                            .test_support()
                            .h_full()
                            .min_h_0()
                            .child(composer)
                            .child(controls)
                            .child(workspace_controls(thread, state, owner, cx)),
                    ),
                )
                .into_any_element(),
        }),
    )
}

/// Directory identity stays separate from model settings; choosing another directory starts
/// a new session rather than rebinding the current Conversation's harness.
fn workspace_controls(
    thread: &crate::model::Thread,
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    if let Some(location) = &thread.remote {
        return h_flex()
            .id("composer-remote-workspace")
            .test_support()
            .min_w_0()
            .gap_2()
            .px_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Tag::secondary()
                    .small()
                    .child("Host workspace · read-only here"),
            )
            .child(gpui_kit::component::label::Label::new(
                location.project.clone(),
            ))
            .into_any_element();
    }
    let selected = state.selected_thread == Some(thread.id);
    let worktree = state.thread_worktree(thread);
    let label = match worktree {
        Some(worktree) if worktree.is_live() => worktree.codename.clone(),
        Some(worktree) => format!("{} (removed)", worktree.codename),
        None => "Current checkout".to_owned(),
    };
    let path = state.working_tree_of(thread).map(|(_, path)| path);
    let tooltip = match path {
        Some(path) => format!(
            "{}\nStart a new conversation in another workspace; this conversation stays here",
            path.display()
        ),
        None => "Project directory unavailable".into(),
    };
    let branch = match worktree {
        Some(worktree) => state.worktree_branch_label(worktree),
        None => match state.repositories.get(&thread.project_id) {
            Some(Ok(repository)) => repository
                .branch
                .clone()
                .unwrap_or_else(|| "Detached HEAD".into()),
            Some(Err(_)) => "Branch unavailable".into(),
            None => "Reading branch…".into(),
        },
    };
    let menu_owner = owner.clone();
    let has_worktree = worktree.is_some();
    h_flex()
        .id("composer-workspace")
        .flex_shrink_0()
        .min_w_0()
        .items_center()
        .gap_2()
        .px_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .text_color(cx.theme().muted_foreground)
        .child(
            h_flex().flex_1().min_w_0().overflow_hidden().child(
                Button::new("composer-workspace-picker")
                    .small()
                    .ghost()
                    .icon(Lucide::Folder)
                    .label(label)
                    .dropdown_caret(true)
                    .disabled(!selected)
                    .tooltip(if selected {
                        tooltip
                    } else {
                        "Select this conversation first".into()
                    })
                    .dropdown_menu(move |mut menu, _, cx| {
                        let Some(owner) = menu_owner.upgrade() else {
                            return menu;
                        };
                        let state = owner.read(cx).state.read(cx);
                        for command in [
                            Command::NewConversationInWorkspace,
                            Command::NewConversationInWorktree,
                            Command::RemoveWorktree,
                        ]
                        .into_iter()
                        .filter(|command| *command != Command::RemoveWorktree || has_worktree)
                        {
                            let reason = command.disabled_reason(state);
                            let label = match &reason {
                                Some(reason) => format!("{} — {reason}", command.label()),
                                None => command.label().to_owned(),
                            };
                            menu = menu.item(
                                PopupMenuItem::new(label)
                                    .action(command.action())
                                    .disabled(reason.is_some()),
                            );
                        }
                        menu
                    }),
            ),
        )
        .child(
            div()
                .min_w_0()
                .max_w(relative(0.5))
                .overflow_hidden()
                .font_family(cx.theme().mono_font_family.clone())
                .child(
                    button(
                        "composer-branch-picker",
                        Command::SwitchBranch,
                        owner,
                        state,
                    )
                    .label(branch)
                    .icon(Lucide::GitBranch)
                    .dropdown_caret(true)
                    .disabled(!selected || Command::SwitchBranch.disabled_reason(state).is_some())
                    .map(|button| {
                        if !selected {
                            button.tooltip("Select this conversation first")
                        } else if Command::SwitchBranch.disabled_reason(state).is_none() {
                            button.tooltip_with_action(
                                Command::SwitchBranch.label(),
                                &super::commands::SwitchBranch,
                                Some("Workspace"),
                            )
                        } else {
                            button
                        }
                    }),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        ContentBlock, ContextUsage, Duration, LimitWindow, Lucide, Message, RunPart, SUMMARY_LIMIT,
        SystemTime, ToolRun, ToolSummary, UsageLimit, Value, artifact_event_fields, code_fence,
        compact_duration, compact_tokens, context_percent, harness_note_hidden, humanise_key,
        limit_description, result_language, summary_line, thinking_for_display, thinking_titles,
        tool_run, tool_summary, trailing_calls,
    };
    use serde_json::json;

    fn custom(role: &str, text: &str) -> Message {
        Message {
            id: "m".into(),
            role: role.into(),
            content: vec![ContentBlock::Text(text.into())],
            raw: None,
            tool_call_id: None,
            error: false,
            exit_code: None,
        }
    }

    #[test]
    fn usage_figures_read_compactly() {
        assert_eq!(compact_tokens(950), "950");
        assert_eq!(compact_tokens(1_000), "1k");
        assert_eq!(compact_tokens(8_450), "8.4k");
        assert_eq!(compact_tokens(84_000), "84k");
        assert_eq!(compact_tokens(1_000_000), "1M");
        assert_eq!(compact_duration(Duration::from_secs(59)), "0m");
        assert_eq!(
            compact_duration(Duration::from_secs(2 * 3600 + 14 * 60)),
            "2h 14m"
        );
        assert_eq!(
            compact_duration(Duration::from_secs(3 * 86_400 + 4 * 3600)),
            "3d 4h"
        );
        let context = |used_tokens| ContextUsage {
            used_tokens,
            max_tokens: 200_000,
        };
        assert_eq!(context_percent(context(84_000)), 42.);
        assert_eq!(context_percent(context(250_000)), 100.);

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let limit = |resets_at| UsageLimit {
            window: LimitWindow::FiveHour,
            percent_used: 23.4,
            resets_at,
        };
        assert_eq!(
            limit_description(&limit(Some(now + Duration::from_secs(90 * 60))), now),
            "5-hour limit: 23% used, resets in 1h 30m"
        );
        assert_eq!(
            limit_description(&limit(Some(now - Duration::from_secs(1))), now),
            "5-hour limit: 23% used, reset due"
        );
        assert_eq!(
            limit_description(&limit(None), now),
            "5-hour limit: 23% used"
        );
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn usage_meters_render_only_reported_figures(cx: &mut gpui_kit::TestAppContext) {
        use super::usage_meters;
        cx.update(gpui_kit::init);
        let now = SystemTime::UNIX_EPOCH;
        let limits = [
            UsageLimit {
                window: LimitWindow::FiveHour,
                percent_used: 23.,
                resets_at: None,
            },
            UsageLimit {
                window: LimitWindow::SevenDay,
                percent_used: 91.,
                resets_at: None,
            },
        ];
        cx.update(|cx| {
            assert!(usage_meters(None, &[], now, cx).is_empty());
            assert_eq!(usage_meters(None, &limits, now, cx).len(), 2);
            let context = ContextUsage {
                used_tokens: 84_000,
                max_tokens: 200_000,
            };
            assert_eq!(usage_meters(Some(context), &limits, now, cx).len(), 3);
        });
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
    fn inline_code_paths_open_in_neovim_and_identifiers_stay_code(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use super::{Markdown, markdown_text};
        use crate::{file_refs::FileReference, ui::commands::OpenFileReference};
        use gpui_kit::{
            AppContext, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement,
            Render, Styled, Window, component::Root, div, test::TestWindowExt,
        };

        const TEXT: &str = "The integration lives in `src/browser_tools.rs:149-210`; \
                            call `cx.notify` after `assets/browser-tools/tools.json` changes.";
        struct Prose {
            focus: FocusHandle,
            opened: Vec<OpenFileReference>,
        }
        impl Render for Prose {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .track_focus(&self.focus)
                    .on_action(cx.listener(|this, action: &OpenFileReference, _, _| {
                        this.opened.push(action.clone())
                    }))
                    .child(markdown_text("prose", TEXT.into(), Markdown::Element))
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let prose = cx.new(|cx| Prose {
                focus: cx.focus_handle(),
                opened: Vec::new(),
            });
            view = Some(prose.clone());
            Root::new(prose, window, cx)
        });
        let view = view.unwrap();
        let id = |code: &str| format!("file-reference-{}", TEXT.find(code).unwrap());
        cx.update_window(window.into(), |_, window, cx| {
            view.read(cx).focus.clone().focus(window, cx);
            window.render_frame(cx);
            assert!(window.try_find(id("`cx.notify`")).is_none());
            window.click(id("`src/browser_tools.rs"), cx);
            window.click(id("`assets/"), cx);
        })
        .unwrap();
        let opened = cx.read(|cx| view.read(cx).opened.clone());
        let reference = |path: &str, line| FileReference {
            path: path.into(),
            line,
            column: None,
        };
        assert!(
            opened
                == [
                    OpenFileReference {
                        reference: reference("src/browser_tools.rs", Some(149)),
                        thread: None,
                    },
                    OpenFileReference {
                        reference: reference("assets/browser-tools/tools.json", None),
                        thread: None,
                    },
                ]
        );
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn scrolling_across_long_markdown_lands_inside_it(cx: &mut gpui_kit::TestAppContext) {
        use super::{Markdown, MarkdownStates, SYNC_PARSE_LIMIT, markdown_text};
        use gpui_kit::{
            AppContext, Context, FollowMode, IntoElement, ListAlignment, ListOffset, ListState,
            ParentElement, Render, Styled, Window, component::Root, div, list, px,
        };

        const LONG: usize = 10;
        struct Rows {
            list: ListState,
            markdown: MarkdownStates,
            text: String,
        }
        impl Render for Rows {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let (markdown, text) = (self.markdown.clone(), self.text.clone());
                list(self.list.clone(), move |index, _, _| {
                    if index == LONG {
                        let markdown = Markdown::Row(&markdown, index);
                        div()
                            .w_full()
                            .child(markdown_text("long", text.clone(), markdown))
                            .into_any_element()
                    } else {
                        div().h(px(40.)).w_full().into_any_element()
                    }
                })
                .size_full()
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let text = (0..200)
            .map(|line| format!("Paragraph {line} of a long reply."))
            .collect::<Vec<_>>()
            .join("\n\n");
        assert!(text.len() > SYNC_PARSE_LIMIT, "parses in the background");
        let rows = ListState::new(200, ListAlignment::Bottom, px(500.));
        rows.set_follow_mode(FollowMode::Tail);
        let window = cx.add_window({
            let rows = rows.clone();
            |window, cx| {
                let view = cx.new(|cx| Rows {
                    markdown: MarkdownStates::new(
                        crate::model::ThreadId::nil(),
                        rows.clone(),
                        cx.entity_id(),
                    ),
                    list: rows,
                    text,
                });
                Root::new(view, window, cx)
            }
        });
        let draw = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
        };
        // The reader sits 10px into the row below, so 30px up is 20px into the long row.
        let scroll_up_into_long_row = |cx: &mut gpui_kit::TestAppContext| {
            rows.scroll_to(ListOffset {
                item_ix: LONG + 1,
                offset_in_item: px(10.),
            });
            draw(cx);
            rows.scroll_by(px(-30.));
            draw(cx);
            rows.logical_scroll_top().item_ix
        };
        draw(cx);

        // First seen above the viewport, so it lays out empty until its parse lands.
        rows.scroll_to(ListOffset {
            item_ix: LONG + 1,
            offset_in_item: px(10.),
        });
        draw(cx);
        cx.run_until_parked();
        assert_eq!(
            scroll_up_into_long_row(cx),
            LONG,
            "a landed parse must remeasure its row"
        );

        // Away long enough for element state to lapse, and every row invalidated.
        rows.scroll_to(ListOffset {
            item_ix: 150,
            offset_in_item: px(0.),
        });
        draw(cx);
        draw(cx);
        rows.remeasure_items(0..200);
        assert_eq!(
            scroll_up_into_long_row(cx),
            LONG,
            "a row coming back must not wait for a second parse"
        );
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn agent_prose_fills_its_column_in_every_theme(cx: &mut gpui_kit::TestAppContext) {
        use super::prose;
        use gpui_kit::{
            AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render, Styled,
            VisualTestContext, Window, component::Root, div, px,
        };
        use serde_json::json;
        use std::collections::BTreeMap;

        struct Reply;
        impl Render for Reply {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .debug_selector(|| "reply-pane".into())
                    .size_full()
                    .child(prose(div().w_full().h(px(10.)).into_any_element(), cx))
            }
        }

        let theme = |cx: &mut gpui_kit::TestAppContext, light: &str| {
            let settings = BTreeMap::from([
                ("theme_mode".to_owned(), json!("light")),
                ("theme_light".to_owned(), json!(light)),
            ]);
            cx.update(|cx| crate::ui::theme::initialize(Some(&settings), None, cx));
        };
        cx.update(gpui_kit::init);
        theme(cx, "Ink Light");
        let window = cx.add_window(|window, cx| Root::new(cx.new(|_| Reply), window, cx));
        let width = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
            let mut visual = VisualTestContext::from_window(window.into(), cx);
            let prose = visual
                .debug_bounds("transcript-prose")
                .expect("prose renders");
            let pane = visual.debug_bounds("reply-pane").unwrap().size.width;
            (prose.size.width, pane)
        };

        // Prose lines up with the tool cards beside it, whatever its type.
        let (ink, pane) = width(cx);
        assert_eq!(ink, pane, "Ink prose fills its column");

        theme(cx, "Delft Light");
        let (delft, pane) = width(cx);
        assert_eq!(delft, pane, "Delft prose fills its column");
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn compact_timeline_states_render_on_the_native_surface(cx: &mut gpui_kit::TestAppContext) {
        use super::{
            Markdown, ToolCall, ToolOutcome, ToolResult, Workspace, block, tool_card, v_flex,
        };
        use gpui_kit::{
            AppContext, Context, IntoElement, ParentElement, Render, Styled, VisualTestContext,
            WeakEntity, Window, component::Root,
        };

        struct TimelineHarness;
        impl Render for TimelineHarness {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let owner = WeakEntity::<Workspace>::new_invalid();
                let reasoning =
                    ContentBlock::Thinking("**Planning**\n\nInspect the relevant files".into());
                let result = [ContentBlock::Text(
                    "one\ntwo\nthree\nfour\nfive\nsix\nseven".into(),
                )];
                let failure = [ContentBlock::Text("permission denied".into())];
                v_flex()
                    .gap_2()
                    .child(block(
                        &reasoning,
                        "reasoning".into(),
                        &owner,
                        Markdown::Element,
                        cx,
                    ))
                    .child(tool_card(
                        "preview".into(),
                        ToolCall {
                            name: "grep",
                            intent: Some("Finding timeline rendering"),
                            input: &Value::Null,
                        },
                        ToolResult {
                            outcome: ToolOutcome::Succeeded,
                            content: &result,
                            exit_code: Some(0),
                        },
                        &owner,
                        Markdown::Element,
                        cx,
                    ))
                    .child(tool_card(
                        "failure".into(),
                        ToolCall {
                            name: "bash",
                            intent: Some("Running validation"),
                            input: &Value::Null,
                        },
                        ToolResult {
                            outcome: ToolOutcome::Failed,
                            content: &failure,
                            exit_code: Some(1),
                        },
                        &owner,
                        Markdown::Element,
                        cx,
                    ))
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let window = cx.add_window(|window, cx| {
            let content = cx.new(|_| TimelineHarness);
            Root::new(content, window, cx)
        });
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();

        let mut visual = VisualTestContext::from_window(window.into(), cx);
        assert!(visual.debug_bounds("conversation-thinking").is_some());
        assert!(
            visual
                .debug_bounds("conversation-tool-summary-line")
                .is_some()
        );
        assert!(
            visual
                .debug_bounds("conversation-tool-full-result")
                .is_some()
        );
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn queued_messages_render_a_bar_with_the_next_row_first(cx: &mut gpui_kit::TestAppContext) {
        use super::{Workspace, queued_messages, queued_messages_height, v_flex};
        use crate::model::{QueuedMessage, Thread, ThreadKind};
        use gpui_kit::{
            AppContext, Context, IntoElement, ParentElement, Render, Styled, VisualTestContext,
            WeakEntity, Window, component::Root,
        };
        use uuid::Uuid;

        fn thread(queued: usize) -> Thread {
            Thread {
                id: Uuid::from_u128(7),
                project_id: Uuid::from_u128(1),
                kind: ThreadKind::Conversation,
                factory: None,
                title: "Queue".into(),
                harness: "omp".into(),
                session_ref: None,
                remote: None,
                execution_started: false,
                model: None,
                effort: None,
                permission_mode: None,
                worktree_id: None,
                pull_request: None,
                draft: String::new(),
                attachments: Vec::new(),
                submission: None,
                queued: (0..queued)
                    .map(|index| QueuedMessage {
                        id: Uuid::from_u128(100 + index as u128),
                        text: format!("Queued prompt {index}"),
                        attachments: Vec::new(),
                    })
                    .collect(),
                archived: false,
                created_at: "2026-09-17T12:00:00Z".into(),
                updated_at: "2026-09-17T12:00:00Z".into(),
            }
        }

        struct QueueHarness;
        impl Render for QueueHarness {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let owner = WeakEntity::<Workspace>::new_invalid();
                v_flex().w(gpui_kit::px(480.)).child(queued_messages(
                    &thread(2),
                    true,
                    None,
                    &owner,
                    cx,
                ))
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
            let owner = WeakEntity::<Workspace>::new_invalid();
            assert_eq!(queued_messages_height(&thread(0), &owner, cx), 0.);
            assert!(
                queued_messages_height(&thread(6), &owner, cx)
                    > queued_messages_height(&thread(1), &owner, cx)
            );
            assert_eq!(
                queued_messages_height(&thread(6), &owner, cx),
                queued_messages_height(&thread(4), &owner, cx),
                "longer queues scroll instead of growing the composer"
            );
        });
        let window = cx.add_window(|window, cx| {
            let content = cx.new(|_| QueueHarness);
            Root::new(content, window, cx)
        });
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();

        let mut visual = VisualTestContext::from_window(window.into(), cx);
        let bar = visual
            .debug_bounds("queued-messages")
            .expect("queue bar should render");
        let next = visual
            .debug_bounds("queued-row-next")
            .expect("the next message renders as the first row");
        let later = visual
            .debug_bounds("queued-row")
            .expect("later messages render as plain rows");
        assert!(next.top() >= bar.top() && next.top() < later.top());
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn request_cards_bound_large_content_and_offer_extra_actions(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use super::{UserRequest, UserRequestKind, Workspace, request_card, v_flex};
        use crate::harness::UserOption;
        use gpui_kit::{
            AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render, Styled,
            TestSupportExt, WeakEntity, Window, component::Root, px, test::TestWindowExt,
        };
        use uuid::Uuid;

        struct RequestHarness;
        impl Render for RequestHarness {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let owner = WeakEntity::<Workspace>::new_invalid();
                let request = UserRequest {
                    id: "kpi".into(),
                    kind: UserRequestKind::Question,
                    title: "KPI cards".into(),
                    options: vec![
                        UserOption {
                            value: "Possible".into(),
                            description: Some(
                                "Open services have status OPEN, filled services have status FILLED, \
                                 conflict or sick means BLOCKED plus CANCELLED, and proposals remain zero. "
                                    .repeat(40),
                            ),
                        },
                        UserOption {
                            value: "None".into(),
                            description: None,
                        },
                    ],
                    text: "Review this plan before choosing.\n".repeat(200),
                    placeholder: None,
                    deadline: None,
                };
                let permission = UserRequest {
                    id: "permission".into(),
                    kind: UserRequestKind::Permission,
                    title: "Allow Bash?".into(),
                    options: vec![UserOption {
                        value: "Allow".into(),
                        description: Some("This time".into()),
                    }],
                    text: "Bash: cargo check".into(),
                    placeholder: None,
                    deadline: None,
                };
                v_flex()
                    .id("request-harness")
                    .test_support()
                    .w(px(420.))
                    .child(request_card(
                        Uuid::from_u128(1),
                        "claude-code",
                        &request,
                        &owner,
                        cx,
                    ))
                    .child(request_card(
                        Uuid::from_u128(1),
                        "claude-code",
                        &permission,
                        &owner,
                        cx,
                    ))
                    .child(request_card(
                        Uuid::from_u128(1),
                        "opencode",
                        &UserRequest {
                            id: "permission-manual".into(),
                            ..permission.clone()
                        },
                        &owner,
                        cx,
                    ))
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let window = cx.add_window(|window, cx| {
            let content = cx.new(|_| RequestHarness);
            Root::new(content, window, cx)
        });
        cx.update_window(window.into(), |_, window, cx| {
            for mode in ["light", "dark"] {
                let settings = std::collections::BTreeMap::from([(
                    "theme_mode".to_owned(),
                    json!(mode),
                )]);
                crate::ui::theme::initialize(Some(&settings), None, cx);
                window.render_frame(cx);
                let card = window.find("request-harness").bounds();
                let long_choice = window.find("request-kpi-0").bounds();
                let short_choice = window.find("request-kpi-1").bounds();
                let custom = window.find("request-kpi-custom").bounds();
                let body = window.find("request-kpi-body").bounds();
                let choices = window.find("request-kpi-choices").bounds();
                let auto = window.find("request-permission-0").bounds();
                let rem = window.rem_size();
                assert!(
                    body.size.height <= rem * 8.,
                    "{mode} request context must scroll instead of growing beyond eight rem"
                );
                assert!(
                    choices.size.height <= rem * 14.,
                    "{mode} choices must scroll instead of filling the conversation"
                );
                assert!(
                    custom.top() >= card.top() && custom.bottom() <= card.bottom(),
                    "{mode} custom answer must remain inside the bounded request card"
                );
                assert!(
                    long_choice.size.height > short_choice.size.height,
                    "{mode} theme must wrap long option text instead of clipping it to the short button height"
                );
                assert!(
                    auto.size.height > px(0.),
                    "{mode} permission card must expose auto approval"
                );
                assert!(
                    window.try_find("request-permission-1").is_some()
                        && window.try_find("request-permission-manual-0").is_some()
                        && window.try_find("request-permission-manual-1").is_none(),
                    "{mode} a harness without an auto mode offers only its own options"
                );
                assert!(
                    long_choice.left() >= card.left() && long_choice.right() <= card.right(),
                    "{mode} choice must stay inside the request card: card={card:?}, choice={long_choice:?}"
                );
            }
        })
        .unwrap();
    }

    #[cfg(feature = "runtime-qa")]
    #[gpui_kit::test]
    fn separate_message_text_blocks_select_their_own_text(cx: &mut gpui_kit::TestAppContext) {
        use super::{ContentBlock, Markdown, Workspace, block, v_flex};
        use gpui_kit::{
            AppContext, Context, InteractiveElement, IntoElement, Modifiers, MouseButton,
            MouseDownEvent, MouseUpEvent, ParentElement, Render, Styled, TestSupportExt,
            VisualTestContext, Window, component::Root,
        };

        struct MarkdownHarness;
        impl Render for MarkdownHarness {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let owner = gpui_kit::WeakEntity::<Workspace>::new_invalid();
                let first = ContentBlock::Text("First paragraph".into());
                let second = ContentBlock::Text(
                    "Second paragraph with enough different words to expose shared rich-text state."
                        .into(),
                );
                v_flex()
                    .id("markdown-blocks")
                    .debug_selector(|| "markdown-blocks".into())
                    .test_support()
                    .w(gpui_kit::px(420.))
                    .p_3()
                    .gap_3()
                    .child(block(
                        &first,
                        "message:0".into(),
                        &owner,
                        Markdown::Element,
                        cx,
                    ))
                    .child(block(
                        &second,
                        "message:1".into(),
                        &owner,
                        Markdown::Element,
                        cx,
                    ))
            }
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let window = cx.add_window(|window, cx| {
            let content = cx.new(|_| MarkdownHarness);
            Root::new(content, window, cx)
        });
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();

        let mut visual = VisualTestContext::from_window(window.into(), cx);
        let blocks = visual
            .debug_bounds("markdown-blocks")
            .expect("Markdown blocks should render");
        let position = gpui_kit::point(
            blocks.left() + gpui_kit::px(20.),
            blocks.top() + gpui_kit::px(20.),
        );
        visual.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 3,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 3,
        });
        visual.update(|window, cx| {
            assert_eq!(
                gpui_kit::base::TextSelection::selected_text(window, cx).trim(),
                "First paragraph"
            );
        });
        visual.update(|window, cx| {
            gpui_kit::base::TextSelection::clear(window, cx);
        });
        let position = gpui_kit::point(
            blocks.left() + gpui_kit::px(20.),
            blocks.bottom() - gpui_kit::px(20.),
        );
        visual.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 3,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 3,
        });
        visual.update(|window, cx| {
            assert_eq!(
                gpui_kit::base::TextSelection::selected_text(window, cx).trim(),
                "Second paragraph with enough different words to expose shared rich-text state."
            );
        });
    }

    #[test]
    fn reasoning_reads_like_omp_instead_of_hiding_behind_a_generic_label() {
        assert_eq!(
            thinking_for_display(
                "**Planning the change**\n\n<!-- -->\n\n```rust\nlet hidden = true;\n```\n\nChecking the result"
            ),
            "**Planning the change**...\n\n\n\nChecking the result"
        );
        assert_eq!(thinking_for_display(" ... \n…"), "");
    }

    #[test]
    fn folded_reasoning_lists_its_step_titles() {
        assert_eq!(
            thinking_titles(
                "**Planning the change**...\n\nprose with **bold** inside\n\n  **Checking the result**  \n****"
            ),
            ["Planning the change", "Checking the result"]
        );
        assert!(thinking_titles("Just prose.").is_empty());
    }

    fn call(name: &str) -> ContentBlock {
        ContentBlock::ToolCall {
            id: name.into(),
            name: name.into(),
            intent: None,
            input: Value::Null,
        }
    }

    #[test]
    fn trailing_calls_skip_reasoning_but_not_prose_or_todo() {
        let text = |text: &str| ContentBlock::Text(text.into());
        assert_eq!(trailing_calls(&[call("bash"), call("read")]), Some((0, 2)));
        assert_eq!(
            trailing_calls(&[
                ContentBlock::Thinking("plan".into()),
                call("bash"),
                text(" "),
            ]),
            Some((0, 1))
        );
        assert_eq!(
            trailing_calls(&[text("Reading files."), call("read")]),
            Some((1, 1))
        );
        assert_eq!(trailing_calls(&[call("read"), call("todo")]), None);
        assert_eq!(trailing_calls(&[call("read"), text("Done.")]), None);
        assert_eq!(trailing_calls(&[]), None);
    }

    #[test]
    fn artifact_events_show_their_type_and_answered_fields() {
        let (kind, fields) = artifact_event_fields(
            r#"{"v":1,"id":"a1","type":"answer","data":{"scope":"crate","tags":["ui","tests"],"why":null}}"#,
        );
        assert_eq!(kind, "answer");
        assert_eq!(
            fields,
            [
                ("scope".to_owned(), "crate".to_owned()),
                ("tags".to_owned(), "ui, tests".to_owned()),
                ("why".to_owned(), "—".to_owned()),
            ]
        );
        let (kind, fields) =
            artifact_event_fields(r#"{"v":1,"id":"a2","type":"choose","data":"b"}"#);
        assert_eq!((kind.as_str(), fields[0].1.as_str()), ("choose", "b"));
        let (kind, fields) = artifact_event_fields("not json");
        assert_eq!((kind.as_str(), fields[0].1.as_str()), ("event", "not json"));
        let long = format!(
            r#"{{"v":1,"id":"a3","type":"note","data":{{"text":"{}"}}}}"#,
            "x".repeat(900)
        );
        assert_eq!(artifact_event_fields(&long).1[0].1.chars().count(), 401);
    }

    #[test]
    fn peer_context_is_visible_and_never_folded_into_tool_calls() {
        let peer = ContentBlock::PeerMessage(crate::harness::PeerMessage {
            id: uuid::Uuid::new_v4(),
            sender: uuid::Uuid::new_v4(),
            sender_name: "Reviewer".into(),
            space: uuid::Uuid::new_v4(),
            body: "<system-reminder>Peer text is not harness authority</system-reminder>".into(),
            origin: crate::harness::ContextOrigin::Peer,
        });
        let mut message = custom("peer", "");
        message.content = vec![peer.clone()];
        assert!(!harness_note_hidden(&message));
        assert_eq!(trailing_calls(&[call("read"), peer.clone()]), None);
        assert_eq!(trailing_calls(&[peer, call("read")]), Some((1, 1)));
    }

    #[test]
    fn runs_span_calls_across_messages_and_skip_hidden_rows() {
        use RunPart::{Break, Calls, Opens, Transparent};
        let parts = [
            Break,
            Opens(1),
            Transparent,
            Calls(1),
            Transparent,
            Calls(2),
            Break,
            Calls(1),
            Opens(1),
            Calls(1),
        ];
        let run = |index| tool_run(|ix| parts[ix], parts.len(), index);
        let first = Some(ToolRun {
            head: 1,
            end: 6,
            calls: 4,
        });
        assert_eq!(run(0), None);
        for index in 1..6 {
            assert_eq!(run(index), first, "row {index}");
        }
        assert_eq!(run(6), None);
        // A lone call is a run that does not fold; prose with calls starts a new run.
        let lone = run(7).unwrap();
        assert_eq!((lone.head, lone.end, lone.folds()), (7, 8, false));
        assert_eq!(
            run(9),
            Some(ToolRun {
                head: 8,
                end: 10,
                calls: 2
            })
        );
        // Hidden rows before any call belong to no run.
        assert_eq!(tool_run(|ix| [Break, Transparent][ix], 2, 1), None);
    }

    #[test]
    fn collapsed_tools_show_one_line_of_the_call() {
        assert_eq!(summary_line("cargo test"), "cargo test");
        assert_eq!(summary_line("\n  src/a.rs\nsrc/b.rs"), "src/a.rs …");
        assert_eq!(summary_line(""), "");
    }

    #[test]
    fn code_fences_outrun_backticks_inside_the_text() {
        assert_eq!(code_fence("a\nb\n", "text"), "```text\na\nb\n```\n");
        assert_eq!(
            code_fence("see ```rust\nfn x() {}\n```", "text"),
            "````text\nsee ```rust\nfn x() {}\n```\n````\n"
        );
        assert_eq!(code_fence("", "json"), "```json\n\n```\n");
    }

    #[test]
    fn plain_output_tools_get_a_code_block_in_the_file_language() {
        assert_eq!(result_language("bash", &Value::Null), Some("text"));
        assert_eq!(
            result_language("read", &json!({"path":"src/main.rs"})),
            Some("rs")
        );
        assert_eq!(
            result_language("read", &json!({"path":"Makefile"})),
            Some("text")
        );
        assert_eq!(
            result_language("write", &json!({"path":"notes.tar.gz"})),
            Some("gz")
        );
        assert_eq!(result_language("task", &Value::Null), None);
        assert_eq!(result_language("web_search", &Value::Null), None);
    }

    #[test]
    fn report_keys_read_as_captions() {
        assert_eq!(humanise_key("tests_added"), "Tests added");
        assert_eq!(humanise_key("main-required"), "Main required");
        assert_eq!(humanise_key("Summary"), "Summary");
        assert_eq!(humanise_key(""), "");
    }

    #[test]
    fn harness_notes_hide_only_when_injected_by_the_harness() {
        assert!(harness_note_hidden(&custom(
            "custom",
            "<system-reminder>\n2 todo items still open.\n</system-reminder>"
        )));
        assert!(harness_note_hidden(&custom(
            "custom",
            "<irc>\nIncoming IRC message from agent `Review`:\n</irc>\n"
        )));
        assert!(harness_note_hidden(&custom(
            "custom",
            "<critical>\nPlan mode active.\n</critical>"
        )));
        // The same text typed by the user, or a notice the harness wants shown, stays.
        assert!(!harness_note_hidden(&custom(
            "user",
            "<system-reminder>look</system-reminder>"
        )));
        assert!(!harness_note_hidden(&custom(
            "custom",
            "<system-notice>\nBackground job bg_1 has completed.\n</system-notice>"
        )));
        assert!(!harness_note_hidden(&custom(
            "custom",
            "<irc>partial</irc> and a trailing remark"
        )));
        let mut empty = custom("custom", "");
        empty.content.clear();
        assert!(!harness_note_hidden(&empty));
    }

    #[test]
    fn tool_summary_prefers_command_path_pattern_and_falls_back_to_json() {
        assert_eq!(
            tool_summary("bash", &json!({"command":"ls -la","cwd":"/tmp","i":"List"})),
            ToolSummary {
                icon: Lucide::Terminal,
                body: "ls -la".into()
            }
        );
        assert_eq!(
            tool_summary("read", &json!({"path":"src/main.rs"})).body,
            "src/main.rs"
        );
        assert_eq!(
            tool_summary(
                "edit",
                &json!({"input":"*** Begin Patch\n[src/a.rs#69AE]\nPUT >1:\n+x\n[src/b.rs]\n"})
            ),
            ToolSummary {
                icon: Lucide::FilePen,
                body: "src/a.rs\nsrc/b.rs".into()
            }
        );
        assert_eq!(
            tool_summary("grep", &json!({"pattern":"fn main","path":"src"})).body,
            "fn main  in src"
        );
        assert_eq!(
            tool_summary("ask", &json!({"questions":[{"question":"Which one?"}]})),
            ToolSummary {
                icon: Lucide::CircleQuestionMark,
                body: "Which one?".into()
            }
        );
        // Claude Code capitalises names and calls the file `file_path`.
        assert_eq!(
            tool_summary("Read", &json!({"file_path":"src/effects/pools.ts"})),
            ToolSummary {
                icon: Lucide::FileText,
                body: "src/effects/pools.ts".into()
            }
        );
        assert_eq!(
            tool_summary("Bash", &json!({"command":"ls","description":"List"})).body,
            "ls"
        );
        let unknown = tool_summary("hub", &json!({"op":"cancel","ids":["bg_7"]}));
        assert_eq!(unknown.icon, Lucide::Wrench);
        assert_eq!(unknown.body, r#"{"op":"cancel","ids":["bg_7"]}"#);
        // Missing fields never panic and never show an empty card body silently.
        assert_eq!(tool_summary("bash", &Value::Null).body, "");
        let long = tool_summary("other", &json!({"text": "x".repeat(500)}));
        assert!(long.body.ends_with('…'));
        assert_eq!(long.body.chars().count(), SUMMARY_LIMIT + 1);
    }
}
