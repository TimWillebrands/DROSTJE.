use super::commands::{
    AcceptCompletion, DismissCompletion, NextCompletion, PasteImage, PreviousCompletion,
};
use crate::model::{DraftKey, DraftRevision, DraftSnapshot};
use crate::neovim::grid::{Cell, GridSnapshot, Highlight, HighlightedChunk};
use crate::neovim::{ClipboardContent, NvimCommand, NvimEditor, NvimEvent, NvimTheme};
use ghostty_terminal::input_surface::{native_input_context, native_text_key};
use gpui_kit::{
    component::{ActiveTheme, Theme, alert::Alert, label::Label},
    prelude::*,
    *,
};
use std::{
    collections::{HashMap, VecDeque},
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub enum NvimViewEvent {
    Ready,
    DraftChanged(DraftSnapshot),
    DraftActivated(DraftSnapshot),
    Failed(String),
    InputRejected(String),
    CompositionChanged(bool),
    /// Neovim's mode changed; `NvimView::mode` has the new one.
    ModeChanged,
    /// A key the caret popup claimed instead of Neovim.
    CaretPopupKey(CaretPopupKey),
    /// Neovim asked to stop editing (`:q`, `:wq`, `ZZ`…); the owner moves focus away.
    Leave,
}

/// What the caret popup's Actions ask of its owner. Bindings live in `commands::bind_keys`
/// and follow Vim's insert-mode completion menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaretPopupKey {
    Next,
    Previous,
    Accept,
    Dismiss,
}

#[derive(Clone)]
enum Target {
    Buffer {
        key: DraftKey,
        revision: DraftRevision,
    },
    Cmdline {
        generation: uuid::Uuid,
        level: u64,
        expected: String,
    },
}

struct Composition {
    target: Target,
    range: Range<usize>,
    base: String,
    text: String,
    selection: Range<usize>,
    anchor: Point<Pixels>,
    recoverable: bool,
}

struct PaintedText {
    origin: Point<Pixels>,
    line: ShapedLine,
}

/// Each cell is shaped independently. Only compatible, undecorated layouts are
/// combined: glyph positions stay relative to Neovim columns, not text advances.
struct GridText {
    origin: Point<Pixels>,
    layout: LineLayout,
    decorations: Vec<DecorationRun>,
}

enum RowText {
    Batched(GridText),
    Decorated(usize),
}

#[derive(Default)]
struct RowShape {
    texts: Vec<RowText>,
    // Indexed in paint order: avoid a large common variant or a box per cell.
    decorated: Vec<PaintedText>,
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
}

struct RowCacheKey {
    font: Font,
    font_size: Pixels,
    cell: Size<Pixels>,
    viewport: Size<Pixels>,
    scale_factor: f32,
    cols: usize,
    theme: NvimTheme,
    defaults: [u32; 3],
    highlights: Arc<HashMap<u64, Highlight>>,
}

impl PartialEq for RowCacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.font == other.font
            && self.font_size == other.font_size
            && self.cell == other.cell
            && self.viewport == other.viewport
            && self.scale_factor == other.scale_factor
            && self.cols == other.cols
            && self.theme == other.theme
            && self.defaults == other.defaults
            && Arc::ptr_eq(&self.highlights, &other.highlights)
    }
}

struct CachedRow {
    // Retain the allocation so a recycled pointer cannot hit a stale shape.
    _source: Arc<Vec<Cell>>,
    shape: Arc<RowShape>,
    visible: bool,
}

#[derive(Default)]
struct RowCache {
    key: Option<RowCacheKey>,
    rows: HashMap<usize, CachedRow>,
}

impl RowCache {
    fn prepare(
        &mut self,
        key: RowCacheKey,
        rows: &[Arc<Vec<Cell>>],
        mut shape: impl FnMut(&[Cell]) -> RowShape,
    ) -> Vec<Arc<RowShape>> {
        if self.key.as_ref() != Some(&key) {
            self.rows = HashMap::new();
            self.key = Some(key);
        }
        for cached in self.rows.values_mut() {
            cached.visible = false;
        }
        let result = rows
            .iter()
            .map(|row| {
                let cached = self
                    .rows
                    .entry(Arc::as_ptr(row) as usize)
                    .or_insert_with(|| CachedRow {
                        _source: Arc::clone(row),
                        shape: Arc::new(shape(row)),
                        visible: false,
                    });
                cached.visible = true;
                Arc::clone(&cached.shape)
            })
            .collect();
        // Scrolled-out rows are not an unbounded history of shaped text.
        self.rows.retain(|_, cached| cached.visible);
        result
    }
}

struct Frame {
    overlays: Vec<PaintedText>,
    texts: Vec<PaintedText>,
    rows: Vec<Arc<RowShape>>,
    cmd_cursor: Option<Point<Pixels>>,
    cmd_layout: Option<(ShapedLine, Point<Pixels>, usize)>,
}

/// The sole consumer of an editor's event channel. The shadow only answers native
/// synchronous input queries; Neovim's flushed grid is the display authority.
pub struct NvimView {
    editor: NvimEditor,
    focus_handle: FocusHandle,
    _focus_subscriptions: Vec<Subscription>,
    _events: Task<()>,
    grid: Arc<GridSnapshot>,
    draft: Option<DraftSnapshot>,
    ready: bool,
    writable: bool,
    error: Option<String>,
    diagnostic: Option<String>,
    composition: Option<Composition>,
    pending_compositions: VecDeque<String>,
    recovery: String,
    bounds: Option<Bounds<Pixels>>,
    cell: Size<Pixels>,
    font_size: Pixels,
    font: Font,
    scale_factor: f32,
    theme_colors: Option<NvimTheme>,
    frame: Option<Frame>,
    row_cache: RowCache,
    requested_size: Option<(u32, u32)>,
    blink_epoch: Instant,
    blink_timer: Option<Task<()>>,
    bell: u64,
    dragging: Option<MouseButton>,
    scroll_remainder: Point<f32>,
    /// Shown below the caret while the owner has something to offer there; while shown the
    /// surface adds the `CaretPopup` key context, whose Actions become `CaretPopupKey`s.
    caret_popup: Option<AnyView>,
}

impl EventEmitter<NvimViewEvent> for NvimView {}

impl NvimView {
    pub fn new(editor: NvimEditor, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus_handle, window, |this, _, cx| {
                this.blink_epoch = Instant::now();
                this.send(NvimCommand::Focus(true), cx);
                cx.notify();
            }),
            cx.on_blur(&focus_handle, window, |this, _, cx| {
                this.send(NvimCommand::Focus(false), cx);
                cx.notify();
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                this.send(
                    NvimCommand::Focus(
                        window.is_window_active() && this.focus_handle.is_focused(window),
                    ),
                    cx,
                );
                this.blink_epoch = Instant::now();
                cx.notify();
            }),
        ];
        let receiver = editor.events.clone();
        let events = cx.spawn(async move |this, cx| {
            while let Ok(first) = receiver.recv().await {
                let mut exhausted = false;
                if this
                    .update(cx, |this, cx| {
                        let started = Instant::now();
                        let mut count = 1;
                        this.receive(first, cx);
                        loop {
                            if count >= 64 || started.elapsed() >= Duration::from_millis(2) {
                                exhausted = true;
                                break;
                            }
                            let Ok(event) = receiver.try_recv() else {
                                break;
                            };
                            // Never coalesce semantic events across a grid, draft activation,
                            // clipboard callback or composition acknowledgment.
                            this.receive(event, cx);
                            count += 1;
                        }
                    })
                    .is_err()
                {
                    return;
                }
                if exhausted {
                    // A ready channel never yields by itself. Return to the native
                    // event loop before consuming more, so key input cannot starve.
                    smol::Timer::after(Duration::from_millis(1)).await;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.fail(
                    "Neovim event stream closed. Restart Editor to recover the saved draft.".into(),
                    cx,
                );
            });
        });
        Self {
            editor,
            focus_handle,
            _focus_subscriptions: subscriptions,
            _events: events,
            grid: Arc::new(GridSnapshot::default()),
            draft: None,
            ready: false,
            writable: true,
            error: None,
            diagnostic: None,
            composition: None,
            pending_compositions: VecDeque::new(),
            recovery: String::new(),
            bounds: None,
            cell: size(cx.theme().mono_font_size, cx.theme().mono_font_size),
            font_size: cx.theme().mono_font_size,
            font: font(cx.theme().mono_font_family.clone()),
            scale_factor: window.scale_factor(),
            theme_colors: None,
            frame: None,
            row_cache: RowCache::default(),
            requested_size: None,
            blink_epoch: Instant::now(),
            bell: 0,
            dragging: None,
            blink_timer: None,
            scroll_remainder: point(0., 0.),
            caret_popup: None,
        }
    }

    /// Neovim's current mode name, such as `insert` or `normal`.
    pub fn mode(&self) -> &str {
        &self.grid.mode
    }

    pub fn set_caret_popup(&mut self, popup: Option<AnyView>, cx: &mut Context<Self>) {
        if self.caret_popup.as_ref().map(AnyView::entity_id)
            != popup.as_ref().map(AnyView::entity_id)
        {
            self.caret_popup = popup;
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
    }

    pub fn has_composition(&self) -> bool {
        self.composition.is_some()
            || !self.pending_compositions.is_empty()
            || !self.recovery.is_empty()
    }

    /// Separately persist this on quit if the semantic draft barrier refuses an
    /// unfinished composition. Never silently merge it into a committed draft.
    pub fn recovery_text(&self) -> String {
        let active = self
            .composition
            .as_ref()
            .map(|composition| composition.text.as_str())
            .unwrap_or("");
        let mut text = String::with_capacity(
            self.recovery.len()
                + active.len()
                + self
                    .pending_compositions
                    .iter()
                    .map(String::len)
                    .sum::<usize>(),
        );
        text.push_str(&self.recovery);
        for pending in &self.pending_compositions {
            text.push_str(pending);
        }
        text.push_str(active);
        text
    }

    pub fn cancel_composition(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.discard_composition(cx);
    }

    /// Deliberate read-only states drop new native input rather than retaining it
    /// as failed composition. Existing composition remains separately recoverable.
    pub fn set_writable(&mut self, writable: bool, cx: &mut Context<Self>) {
        if self.writable == writable {
            return;
        }
        self.writable = writable;
        self.dragging = None;
        self.scroll_remainder = point(0., 0.);
        if !writable && let Some(composition) = self.composition.as_mut() {
            composition.recoverable = true;
        }
        cx.notify();
    }

    fn enabled(&self) -> bool {
        self.writable
            && self.ready
            && self.error.is_none()
            && !self.grid.rows.is_empty()
            && self
                .draft
                .as_ref()
                .is_some_and(|draft| draft.generation == self.grid.generation)
    }

    fn send(&mut self, command: NvimCommand, cx: &mut Context<Self>) -> bool {
        match self.editor.try_command(command) {
            Ok(()) => true,
            Err(error) => {
                self.fail(format!("{error:#}"), cx);
                false
            }
        }
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.ready = false;
        self.dragging = None;
        for text in self.pending_compositions.drain(..) {
            self.recovery.push_str(&text);
        }
        if let Some(composition) = self.composition.as_mut() {
            composition.recoverable = true;
        }
        self.error = Some(error.clone());
        cx.emit(NvimViewEvent::Failed(error));
        cx.emit(NvimViewEvent::CompositionChanged(self.has_composition()));
        cx.notify();
    }

    fn reject(&mut self, diagnostic: String, cx: &mut Context<Self>) {
        self.diagnostic = Some(diagnostic.clone());
        cx.emit(NvimViewEvent::InputRejected(diagnostic));
        cx.notify();
    }

    fn receive(&mut self, event: NvimEvent, cx: &mut Context<Self>) {
        match event {
            NvimEvent::Grid(grid) => {
                if self
                    .draft
                    .as_ref()
                    .is_some_and(|draft| draft.generation != grid.generation)
                {
                    return;
                }
                if self.grid.cursor != grid.cursor
                    || self.grid.mode_index != grid.mode_index
                    || self
                        .grid
                        .cmdline
                        .last_key_value()
                        .map(|(level, cmd)| (*level, cmd.pos))
                        != grid
                            .cmdline
                            .last_key_value()
                            .map(|(level, cmd)| (*level, cmd.pos))
                {
                    self.blink_epoch = Instant::now();
                }
                let mode_changed = self.grid.mode != grid.mode;
                self.grid = grid;
                self.frame = None;
                if mode_changed {
                    cx.emit(NvimViewEvent::ModeChanged);
                }
            }
            NvimEvent::DraftChanged(draft) => {
                if self.draft.as_ref().is_some_and(|active| {
                    active.key == draft.key && active.generation == draft.generation
                }) {
                    self.draft = Some(draft.clone());
                }
                cx.emit(NvimViewEvent::DraftChanged(draft));
            }
            NvimEvent::DraftActivated(draft) => {
                if self.grid.generation != draft.generation {
                    self.frame = None;
                    self.requested_size = None;
                }
                self.draft = Some(draft.clone());
                self.blink_epoch = Instant::now();
                cx.emit(NvimViewEvent::DraftActivated(draft));
            }
            NvimEvent::ClipboardGet { reply } => {
                let content = cx
                    .read_from_clipboard()
                    .and_then(|item| clipboard_content(&item))
                    .ok_or_else(|| "The native clipboard contains no text".to_string());
                let _ = reply.try_send(content);
            }
            NvimEvent::ClipboardSet(content) => {
                // Native metadata is owned by the clipboard item, not a stale text cache.
                let metadata = serde_json::json!({"drostje_nvim_register": content.regtype, "lines": content.lines});
                cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(
                    content.text(),
                    metadata,
                ));
            }
            NvimEvent::CompositionCommitted => {
                self.pending_compositions.pop_front();
                cx.emit(NvimViewEvent::CompositionChanged(self.has_composition()));
            }
            NvimEvent::CompositionRejected(error) => {
                if let Some(text) = self.pending_compositions.pop_front() {
                    self.recovery.push_str(&text);
                }
                self.reject(error, cx);
                cx.emit(NvimViewEvent::CompositionChanged(self.has_composition()));
            }
            NvimEvent::InputRejected(error) => self.reject(error, cx),
            NvimEvent::Leave => cx.emit(NvimViewEvent::Leave),
            NvimEvent::Failed(error) => self.fail(error, cx),
            NvimEvent::Ready => {
                self.ready = true;
                self.error = None;
                self.diagnostic = None;
                self.requested_size = None;
                if let Some(composition) = self.composition.as_mut() {
                    // A restarted process cannot commit a previous generation's preedit.
                    composition.recoverable = true;
                }
                if self.has_composition() {
                    self.send(NvimCommand::SetComposition(true), cx);
                }
                cx.emit(NvimViewEvent::Ready);
            }
        }
        cx.notify();
    }

    fn base_text(&self) -> (String, usize) {
        if let Some((_, cmd)) = self.grid.cmdline.last_key_value() {
            let text: String = cmd
                .content
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect();
            let cursor = byte_floor(&text, cmd.pos);
            (text, cursor)
        } else {
            self.draft
                .as_ref()
                .map(|draft| (draft.text.clone(), buffer_offset(&draft.text, draft.cursor)))
                .unwrap_or_default()
        }
    }

    fn native_text(&self) -> (String, Range<usize>) {
        if let Some(composition) = self.composition.as_ref() {
            let mut text = composition.base.clone();
            if let Some(range) = exact_utf16_range(&text, &composition.range) {
                text.replace_range(range, &composition.text);
                return (
                    text,
                    composition.range.start + composition.selection.start
                        ..composition.range.start + composition.selection.end,
                );
            }
        }
        let (text, cursor) = self.base_text();
        let cursor = utf16_offset(&text, cursor);
        (text, cursor..cursor)
    }

    fn target(&self) -> Option<Target> {
        if let Some((level, cmd)) = self.grid.cmdline.last_key_value() {
            Some(Target::Cmdline {
                generation: self.grid.generation,
                level: *level,
                expected: cmd
                    .content
                    .iter()
                    .map(|chunk| chunk.text.as_str())
                    .collect(),
            })
        } else {
            self.draft.as_ref().map(|draft| Target::Buffer {
                key: draft.key.clone(),
                revision: draft.revision(),
            })
        }
    }

    fn target_current(&self, target: &Target) -> bool {
        match target {
            Target::Buffer { key, revision } => {
                self.grid.cmdline.is_empty()
                    && self.draft.as_ref().is_some_and(|draft| {
                        &draft.key == key
                            && draft.generation == revision.generation
                            && draft.changedtick == revision.changedtick
                    })
            }
            Target::Cmdline {
                generation,
                level,
                expected,
            } => {
                self.grid.generation == *generation
                    && self
                        .grid
                        .cmdline
                        .last_key_value()
                        .is_some_and(|(active, cmd)| {
                            active == level
                                && cmd
                                    .content
                                    .iter()
                                    .map(|chunk| chunk.text.as_str())
                                    .collect::<String>()
                                    == *expected
                        })
            }
        }
    }

    fn discard_composition(&mut self, cx: &mut Context<Self>) {
        self.composition = None;
        self.recovery.clear();
        self.send(NvimCommand::SetComposition(false), cx);
        cx.emit(NvimViewEvent::CompositionChanged(self.has_composition()));
        cx.notify();
    }

    fn retain_native(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(composition) = self.composition.as_mut() {
            if splice_composition(composition, range, text, selection).is_err() {
                self.recovery.push_str(text);
            }
            composition.recoverable = true;
        } else {
            self.recovery.push_str(text);
        }
        cx.emit(NvimViewEvent::CompositionChanged(self.has_composition()));
        cx.notify();
    }

    fn replace(&mut self, target: Target, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        if !self.target_current(&target) {
            self.recovery.push_str(text);
            self.reject("The draft changed during native composition. Composition is retained; copy it before cancelling or restarting.".into(), cx);
            cx.emit(NvimViewEvent::CompositionChanged(self.has_composition()));
            return;
        }
        let command = match target {
            Target::Buffer { key, revision } => NvimCommand::Replace {
                key,
                revision,
                range,
                text: text.into(),
            },
            Target::Cmdline { expected, .. } => NvimCommand::ReplaceCmdline {
                expected,
                range,
                text: text.into(),
            },
        };
        self.pending_compositions.push_back(text.to_owned());
        self.send(command, cx);
        cx.emit(NvimViewEvent::CompositionChanged(self.has_composition()));
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if let Some(chord) = clipboard_chord(key) {
            if self.enabled() && !self.has_composition() {
                match chord {
                    ClipboardChord::Copy => {
                        self.send(NvimCommand::Copy { cut: false }, cx);
                    }
                    ClipboardChord::Cut => {
                        self.send(NvimCommand::Copy { cut: true }, cx);
                    }
                    ClipboardChord::Paste => {
                        if let Some(item) = cx.read_from_clipboard() {
                            self.paste_item(item, window, cx);
                        }
                    }
                }
            }
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if key.modifiers.platform {
            // Workspace shortcuts (especially Cmd-Return and Cmd-Escape) bubble.
            return;
        }
        if !self.writable {
            return;
        }
        if key.key == "f23" || key.key == "f24" {
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if self.composition.is_some() {
            if key.key == "escape" {
                self.discard_composition(cx);
                window.prevent_default();
                cx.stop_propagation();
            }
            return;
        }
        if !self.enabled() {
            return;
        }
        if let Some(notation) = key_notation(key) {
            self.send(NvimCommand::Input(notation), cx);
            self.blink_epoch = Instant::now();
            window.prevent_default();
            cx.stop_propagation();
        }
        // Printable keys, including Option-generated native Unicode, are delivered
        // only by replace_text_in_range. Never guess Neovim's current mode here.
    }

    fn mouse(
        &mut self,
        button: MouseButton,
        action: &str,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        if !self.enabled() || !self.grid.mouse_enabled || self.has_composition() {
            return;
        }
        let Some(bounds) = self.bounds else {
            return;
        };
        let name = match button {
            MouseButton::Left => "left",
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
            _ => return,
        };
        let (row, col) = self.screen_cell(position, bounds);
        self.send(
            NvimCommand::Mouse {
                button: name.into(),
                action: action.into(),
                modifiers: mouse_modifiers(modifiers),
                row,
                col,
            },
            cx,
        );
    }

    fn screen_cell(&self, position: Point<Pixels>, bounds: Bounds<Pixels>) -> (u32, u32) {
        let col = ((position.x - bounds.left()) / self.cell.width)
            .floor()
            .max(0.) as usize;
        let row = ((position.y - bounds.top()) / self.cell.height)
            .floor()
            .max(0.) as usize;
        (
            row.min(self.grid.rows.len().saturating_sub(1)) as u32,
            col.min(self.grid.cols.saturating_sub(1)) as u32,
        )
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus(window, cx);
        self.dragging = Some(event.button);
        self.mouse(event.button, "press", event.position, event.modifiers, cx);
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.dragging.take().is_some() {
            self.mouse(event.button, "release", event.position, event.modifiers, cx);
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self.dragging {
            self.mouse(button, "drag", event.position, event.modifiers, cx);
        }
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.enabled() || !self.grid.mouse_enabled || self.has_composition() {
            return;
        }
        let Some(bounds) = self.bounds else {
            return;
        };
        let delta = match event.delta {
            ScrollDelta::Pixels(delta) => {
                point(delta.x / self.cell.width, delta.y / self.cell.height)
            }
            ScrollDelta::Lines(delta) => delta,
        };
        self.scroll_remainder.x += delta.x;
        self.scroll_remainder.y += delta.y;
        let x = self.scroll_remainder.x.trunc() as i32;
        let y = self.scroll_remainder.y.trunc() as i32;
        self.scroll_remainder.x -= x as f32;
        self.scroll_remainder.y -= y as f32;
        let (row, col) = self.screen_cell(event.position, bounds);
        for (count, negative, positive) in [(y, "down", "up"), (x, "right", "left")] {
            for _ in 0..count.unsigned_abs() {
                self.send(
                    NvimCommand::Mouse {
                        button: "wheel".into(),
                        action: if count < 0 { negative } else { positive }.into(),
                        modifiers: mouse_modifiers(event.modifiers),
                        row,
                        col,
                    },
                    cx,
                );
            }
        }
        cx.stop_propagation();
    }

    fn cursor_origin(&self) -> Point<Pixels> {
        if let Some(cursor) = self.frame.as_ref().and_then(|frame| frame.cmd_cursor) {
            return cursor;
        }
        point(
            self.cell.width * self.grid.cursor.1 as f32,
            self.cell.height * self.grid.cursor.0 as f32,
        )
    }

    fn prepare(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let theme = cx.theme();
        let colors = nvim_theme(theme);
        let font = font(theme.mono_font_family.clone());
        let font_changed = self.font != font;
        self.font = font;
        if self.ready
            && self.theme_colors != Some(colors)
            && self.send(NvimCommand::Theme(colors), cx)
        {
            self.theme_colors = Some(colors);
        }
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let cell_height = window.line_height().max(px(1.));
        let scale_factor = window.scale_factor();
        let metrics_changed = font_changed
            || font_size != self.font_size
            || cell_height != self.cell.height
            || scale_factor != self.scale_factor
            || self.bounds.is_none()
            || self
                .row_cache
                .key
                .as_ref()
                .is_none_or(|key| key.highlights.get(&0) != self.grid.highlights.get(&0));
        if metrics_changed {
            self.font_size = font_size;
            self.scale_factor = scale_factor;
            let run = text_run(&self.grid, &self.font, 0, 1);
            let width = window
                .text_system()
                .shape_line("M".into(), font_size, &[run], None)
                .width();
            self.cell = size(width.max(px(1.)), cell_height);
        }
        if self.frame.is_none()
            || metrics_changed
            || self
                .row_cache
                .key
                .as_ref()
                .is_none_or(|key| key.theme != colors)
            || self
                .bounds
                .is_none_or(|previous| previous.size != bounds.size)
        {
            let key = RowCacheKey {
                font: self.font.clone(),
                font_size,
                cell: self.cell,
                viewport: bounds.size,
                scale_factor,
                cols: self.grid.cols,
                theme: colors,
                defaults: [
                    self.grid.default_fg,
                    self.grid.default_bg,
                    self.grid.default_sp,
                ],
                highlights: Arc::clone(&self.grid.highlights),
            };
            // Include a partly visible bottom row, but never shape offscreen rows.
            let row_count = self
                .grid
                .rows
                .len()
                .min((bounds.size.height / self.cell.height).ceil().max(0.) as usize);
            let visible_cols = (bounds.size.width / self.cell.width).ceil().max(0.) as usize;
            let rows = self
                .row_cache
                .prepare(key, &self.grid.rows[..row_count], |row| {
                    shape_row(
                        &row[..row.len().min(visible_cols)],
                        &self.grid,
                        &self.font,
                        self.cell,
                        font_size,
                        window.text_system(),
                    )
                });
            self.frame = Some(make_frame(
                &self.grid,
                &self.font,
                self.cell,
                font_size,
                bounds.size,
                rows,
                window,
            ));
        }
        self.bounds = Some(bounds);
        let strip_rows = self.grid.cmdline.len() + self.grid.cmdline_block.len();
        let cols = (bounds.size.width / self.cell.width).floor().max(1.) as u32;
        let rows = ((bounds.size.height / self.cell.height).floor() as usize)
            .saturating_sub(strip_rows)
            .max(1) as u32;
        if self.ready && self.requested_size != Some((cols, rows)) {
            self.requested_size = Some((cols, rows));
            self.send(NvimCommand::Resize { cols, rows }, cx);
        }
        if self.bell != self.grid.bell {
            self.bell = self.grid.bell;
            window.play_system_bell();
        }
    }

    fn arm_blink(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.blink_timer = None;
        if !self.enabled()
            || !window.is_window_active()
            || !self.focus_handle.is_focused(window)
            || self.grid.busy
            || self.composition.is_some()
            || !self.grid.cursor_style_enabled
        {
            return;
        }
        let Some(style) = self
            .grid
            .mode_styles
            .get(self.grid.mode_index)
            .filter(|style| style.blinkwait > 0 && style.blinkon > 0 && style.blinkoff > 0)
        else {
            return;
        };
        let elapsed = self.blink_epoch.elapsed().as_millis() as u64;
        let delay = if elapsed < style.blinkwait {
            style.blinkwait - elapsed
        } else {
            let phase = (elapsed - style.blinkwait) % (style.blinkon + style.blinkoff);
            if phase < style.blinkoff {
                style.blinkoff - phase
            } else {
                style.blinkon + style.blinkoff - phase
            }
        };
        self.blink_timer = Some(cx.spawn(async move |this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(delay.max(1))).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
    }

    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let Some(frame) = self.frame.as_ref() else {
            return;
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let background = if self.grid.rows.is_empty() {
                cx.theme().background
            } else {
                rgb(self.grid.default_bg).into()
            };
            window.paint_quad(fill(bounds, background));
            // Backgrounds precede all glyphs, including overhangs from other rows.
            for (row_index, row) in frame.rows.iter().enumerate() {
                let origin = bounds.origin + point(px(0.), self.cell.height * row_index as f32);
                for (rect, color) in &row.backgrounds {
                    window.paint_quad(fill(Bounds::new(origin + rect.origin, rect.size), *color));
                }
            }
            for (row_index, row) in frame.rows.iter().enumerate() {
                let origin = bounds.origin + point(px(0.), self.cell.height * row_index as f32);
                for text in &row.texts {
                    match text {
                        RowText::Batched(text) => {
                            let _ = text.layout.paint(
                                origin + text.origin,
                                self.cell.height,
                                TextAlign::Left,
                                None,
                                &text.decorations,
                                window,
                                cx,
                            );
                        }
                        RowText::Decorated(index) => {
                            let text = &row.decorated[*index];
                            let _ = text.line.paint(
                                origin + text.origin,
                                self.cell.height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
                    }
                }
            }
            for text in &frame.texts {
                let _ = text.line.paint_background(
                    bounds.origin + text.origin,
                    self.cell.height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                let _ = text.line.paint(
                    bounds.origin + text.origin,
                    self.cell.height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            for text in &frame.overlays {
                let origin = bounds.origin + text.origin;
                window.paint_quad(fill(
                    Bounds::new(
                        origin,
                        size(text.line.width().max(self.cell.width), self.cell.height),
                    ),
                    rgb(self.grid.default_bg),
                ));
                let _ =
                    text.line
                        .paint(origin, self.cell.height, TextAlign::Left, None, window, cx);
            }
            if window.is_window_active()
                && self.focus_handle.is_focused(window)
                && self.enabled()
                && !self.grid.busy
                && self.composition.is_none()
            {
                self.paint_cursor(bounds, window, cx);
            }
            if let Some(composition) = self.composition.as_ref() {
                let cursor = utf16_byte(&composition.text, composition.selection.end)
                    .unwrap_or(composition.text.len());
                let mut offset = 0;
                for (row, text) in composition.text.split('\n').enumerate() {
                    let mut run = text_run(&self.grid, &self.font, 0, text.len());
                    run.underline = Some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(run.color),
                        wavy: composition.recoverable,
                    });
                    let line = window.text_system().shape_line(
                        text.to_owned().into(),
                        self.font_size,
                        &[run],
                        None,
                    );
                    let origin = bounds.origin
                        + composition.anchor
                        + point(px(0.), self.cell.height * row as f32);
                    window.paint_quad(fill(
                        Bounds::new(
                            origin,
                            size(line.width().max(self.cell.width), self.cell.height),
                        ),
                        rgb(self.grid.default_bg),
                    ));
                    let _ = line.paint(origin, self.cell.height, TextAlign::Left, None, window, cx);
                    if (offset..=offset + text.len()).contains(&cursor) {
                        window.paint_quad(fill(
                            Bounds::new(
                                origin + point(line.x_for_index(cursor - offset), px(0.)),
                                size(px(1.), self.cell.height),
                            ),
                            rgb(self.grid.default_fg),
                        ));
                    }
                    offset += text.len() + 1;
                }
            }
        });
    }

    fn paint_cursor(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let style = self
            .grid
            .cursor_style_enabled
            .then(|| self.grid.mode_styles.get(self.grid.mode_index))
            .flatten();
        if let Some(style) =
            style.filter(|style| style.blinkwait > 0 && style.blinkon > 0 && style.blinkoff > 0)
        {
            let elapsed = self.blink_epoch.elapsed().as_millis() as u64;
            if elapsed >= style.blinkwait
                && (elapsed - style.blinkwait) % (style.blinkon + style.blinkoff) < style.blinkoff
            {
                return;
            }
        }
        let origin = bounds.origin + self.cursor_origin();
        let fraction = style
            .map(|style| style.cell_percentage.clamp(1, 100) as f32 / 100.)
            .unwrap_or(1.);
        let shape = style.map(|style| style.shape.as_str()).unwrap_or("block");
        let (fg, bg, _) = colors(&self.grid, style.map(|style| style.attr_id).unwrap_or(0));
        let color = if style.is_some_and(|style| style.attr_id != 0) {
            bg
        } else {
            fg
        };
        let width = if self.grid.cmdline.is_empty() {
            self.grid
                .rows
                .get(self.grid.cursor.0)
                .and_then(|row| row.get(self.grid.cursor.1 + 1))
                .filter(|cell| cell.text.is_empty())
                .map(|_| self.cell.width * 2.)
                .unwrap_or(self.cell.width)
        } else {
            self.cell.width
        };
        let cursor = match shape {
            "vertical" => Bounds::new(
                origin,
                size((self.cell.width * fraction).max(px(1.)), self.cell.height),
            ),
            "horizontal" => Bounds::new(
                origin + point(px(0.), self.cell.height * (1. - fraction)),
                size(width, (self.cell.height * fraction).max(px(1.))),
            ),
            _ => Bounds::new(origin, size(width, self.cell.height)),
        };
        window.paint_quad(fill(cursor, color));
        if shape == "block"
            && self.grid.cmdline.is_empty()
            && let Some(cell) = self
                .grid
                .rows
                .get(self.grid.cursor.0)
                .and_then(|row| row.get(self.grid.cursor.1))
            && !cell.text.is_empty()
        {
            let mut run = text_run(&self.grid, &self.font, cell.hl_id, cell.text.len());
            run.color = if style.is_some_and(|style| style.attr_id != 0) {
                fg
            } else {
                bg
            };
            let line = window.text_system().shape_line(
                cell.text.clone().into(),
                self.font_size,
                &[run],
                None,
            );
            let _ = line.paint(origin, self.cell.height, TextAlign::Left, None, window, cx);
        }
        if shape == "block" {
            let layout = self.frame.as_ref().and_then(|frame| {
                frame
                    .overlays
                    .iter()
                    .find(|overlay| Some(overlay.origin) == frame.cmd_cursor)
                    .map(|overlay| (&overlay.line, overlay.origin))
                    .or_else(|| {
                        frame
                            .cmd_layout
                            .as_ref()
                            .map(|(line, origin, _)| (line, *origin))
                    })
            });
            if let Some((line, position)) = layout {
                let mut run = text_run(&self.grid, &self.font, 0, line.text.len());
                run.color = if style.is_some_and(|style| style.attr_id != 0) {
                    fg
                } else {
                    bg
                };
                let inverse = window.text_system().shape_line(
                    line.text.clone(),
                    self.font_size,
                    &[run],
                    None,
                );
                window.with_content_mask(Some(ContentMask { bounds: cursor }), |window| {
                    let _ = inverse.paint(
                        bounds.origin + position,
                        self.cell.height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                });
            }
        }
    }
}

impl EntityInputHandler for NvimView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let (text, _) = self.native_text();
        let start = utf16_floor(&text, range.start);
        let end = utf16_ceil(&text, range.end.max(range.start));
        *adjusted = Some(utf16_offset(&text, start)..utf16_offset(&text, end));
        Some(text[start..end].to_owned())
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if !ignore_disabled && !self.enabled() {
            return None;
        }
        Some(UTF16Selection {
            range: self.native_text().1,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.composition.as_ref().map(|composition| {
            composition.range.start
                ..composition.range.start + composition.text.encode_utf16().count()
        })
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if !self.writable {
            return;
        }
        if self
            .composition
            .as_ref()
            .is_some_and(|composition| !composition.recoverable)
        {
            self.discard_composition(cx);
        }
    }

    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.enabled() && !self.has_composition() {
            self.paste_item(item, window, cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.writable {
            return;
        }
        if !self.enabled()
            || self
                .composition
                .as_ref()
                .is_some_and(|composition| composition.recoverable)
        {
            self.retain_native(range, text, None, cx);
            return;
        }
        if text.is_empty() && self.composition.is_some() {
            self.discard_composition(cx);
            return;
        }
        self.blink_epoch = Instant::now();
        if let Some(mut composition) = self.composition.take() {
            if let Err(error) = splice_composition(&mut composition, range, text, None) {
                self.composition = Some(composition);
                self.recovery.push_str(text);
                self.reject(error, cx);
                return;
            }
            if !self.send(NvimCommand::SetComposition(false), cx) {
                self.recovery.push_str(&composition.text);
                return;
            }
            self.replace(composition.target, composition.range, &composition.text, cx);
        } else if let Some(range) = range.filter(|range| !range.is_empty()) {
            if let Some(target) = self.target() {
                self.replace(target, range, text, cx);
            }
        } else {
            self.send(NvimCommand::Input(text.replace('<', "<LT>")), cx);
            // The flushed grid, not queued input, changes the display. Invalidating
            // here forces GPUI to draw the old grid before dispatching the next key.
            return;
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.writable {
            return;
        }
        if !self.enabled()
            || self
                .composition
                .as_ref()
                .is_some_and(|composition| composition.recoverable)
        {
            self.retain_native(range, text, selection, cx);
            return;
        }
        if text.is_empty() {
            self.discard_composition(cx);
            return;
        }
        if let Some(composition) = self.composition.as_mut() {
            if let Err(error) = splice_composition(composition, range, text, selection) {
                self.recovery.push_str(text);
                self.reject(error, cx);
                return;
            }
        } else {
            let Some(target) = self.target() else {
                return;
            };
            let (base, cursor) = self.base_text();
            let cursor = utf16_offset(&base, cursor);
            let range = range.unwrap_or(cursor..cursor);
            if exact_utf16_range(&base, &range).is_none() {
                self.recovery.push_str(text);
                self.reject(
                    "Native composition supplied an invalid Unicode range; the text was retained."
                        .into(),
                    cx,
                );
                cx.emit(NvimViewEvent::CompositionChanged(true));
                return;
            }
            let length = text.encode_utf16().count();
            let selection = selection.unwrap_or(length..length);
            let start = utf16_offset(text, utf16_floor(text, selection.start));
            let end = utf16_offset(text, utf16_ceil(text, selection.end.max(selection.start)));
            self.composition = Some(Composition {
                target,
                range,
                base,
                text: text.into(),
                selection: start..end,
                anchor: self.cursor_origin(),
                recoverable: false,
            });
            if !self.send(NvimCommand::SetComposition(true), cx) {
                return;
            }
        }
        cx.emit(NvimViewEvent::CompositionChanged(true));
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if let Some(composition) = self.composition.as_ref() {
            let start = utf16_floor(
                &composition.text,
                range.start.saturating_sub(composition.range.start),
            );
            let end = utf16_ceil(
                &composition.text,
                range.end.saturating_sub(composition.range.start),
            );
            let line_start = composition.text[..start]
                .rfind('\n')
                .map_or(0, |newline| newline + 1);
            let line_end = composition.text[start..]
                .find('\n')
                .map_or(composition.text.len(), |newline| start + newline);
            let row = composition.text[..line_start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            let text = &composition.text[line_start..line_end];
            let line = window.text_system().shape_line(
                text.to_owned().into(),
                self.font_size,
                &[text_run(&self.grid, &self.font, 0, text.len())],
                None,
            );
            let start_x = line.x_for_index(start - line_start);
            let end_x = line.x_for_index(end.min(line_end).max(start) - line_start);
            return Some(Bounds::new(
                bounds.origin + composition.anchor + point(start_x, self.cell.height * row as f32),
                size((end_x - start_x).max(px(1.)), self.cell.height),
            ));
        }
        if let Some((line, origin, prefix)) = self
            .frame
            .as_ref()
            .and_then(|frame| frame.cmd_layout.as_ref())
        {
            let (text, _) = self.base_text();
            let start = utf16_floor(&text, range.start) + prefix;
            let end = utf16_ceil(&text, range.end) + prefix;
            return Some(Bounds::new(
                bounds.origin + *origin + point(line.x_for_index(start), px(0.)),
                size(
                    (line.x_for_index(end) - line.x_for_index(start)).max(px(1.)),
                    self.cell.height,
                ),
            ));
        }
        // Buffer rows may wrap and contain concealed text. Only Neovim can map those
        // rows; the reported screen cursor is the authoritative IME candidate anchor.
        let (text, cursor) = self.base_text();
        let cursor = utf16_offset(&text, cursor);
        (range.start == cursor)
            .then_some(Bounds::new(bounds.origin + self.cursor_origin(), self.cell))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.bounds?;
        if let Some(composition) = self.composition.as_ref() {
            let row = ((position.y - bounds.top() - composition.anchor.y) / self.cell.height)
                .floor()
                .max(0.) as usize;
            let start = buffer_offset(&composition.text, (row, 0));
            let end = composition.text[start..]
                .find('\n')
                .map_or(composition.text.len(), |newline| start + newline);
            let text = &composition.text[start..end];
            let line = window.text_system().shape_line(
                text.to_owned().into(),
                self.font_size,
                &[text_run(&self.grid, &self.font, 0, text.len())],
                None,
            );
            let byte = line.closest_index_for_x(position.x - bounds.left() - composition.anchor.x);
            return Some(composition.range.start + utf16_offset(&composition.text, start + byte));
        }
        if let Some((line, origin, prefix)) = self
            .frame
            .as_ref()
            .and_then(|frame| frame.cmd_layout.as_ref())
        {
            let (text, _) = self.base_text();
            let byte = line
                .closest_index_for_x(position.x - bounds.left() - origin.x)
                .saturating_sub(*prefix);
            return Some(utf16_offset(&text, byte_floor(&text, byte)));
        }
        let (text, cursor) = self.base_text();
        let origin = bounds.origin + self.cursor_origin();
        if position.x >= origin.x
            && position.x <= origin.x + self.cell.width
            && position.y >= origin.y
            && position.y <= origin.y + self.cell.height
        {
            Some(utf16_offset(&text, cursor))
        } else {
            None
        }
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.native_text().0.encode_utf16().count())
    }

    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.enabled() {
            return;
        }
        if let Some(composition) = self.composition.as_mut() {
            let start = range.start.saturating_sub(composition.range.start);
            let end = range.end.saturating_sub(composition.range.start);
            if range.start >= composition.range.start
                && exact_utf16_range(&composition.text, &(start..end)).is_some()
            {
                composition.selection = start..end;
                cx.notify();
            }
        }
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.enabled()
    }
}

impl Focusable for NvimView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NvimView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.arm_blink(window, cx);
        let prepare = cx.entity();
        let paint = prepare.clone();
        let popup_shown = self.caret_popup.is_some()
            && self.bounds.is_some()
            && self.enabled()
            && self.focus_handle.is_focused(window);
        let mut input_context = native_input_context("PromptEditor");
        if popup_shown {
            input_context.add("CaretPopup");
        }
        let mut surface = div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .key_context(input_context)
            .track_focus(&self.focus_handle)
            .when(popup_shown, |surface| {
                surface
                    .on_action(cx.listener(|_, _: &NextCompletion, _, cx| {
                        cx.emit(NvimViewEvent::CaretPopupKey(CaretPopupKey::Next))
                    }))
                    .on_action(cx.listener(|_, _: &PreviousCompletion, _, cx| {
                        cx.emit(NvimViewEvent::CaretPopupKey(CaretPopupKey::Previous))
                    }))
                    .on_action(cx.listener(|_, _: &AcceptCompletion, _, cx| {
                        cx.emit(NvimViewEvent::CaretPopupKey(CaretPopupKey::Accept))
                    }))
                    .on_action(cx.listener(|_, _: &DismissCompletion, _, cx| {
                        cx.emit(NvimViewEvent::CaretPopupKey(CaretPopupKey::Dismiss))
                    }))
            })
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .text_color(cx.theme().foreground)
            .bg(cx.theme().background)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        prepare.update(cx, |this, cx| this.prepare(bounds, window, cx))
                    },
                    move |bounds, (), window, cx| {
                        paint.update(cx, |this, cx| {
                            window.handle_input(
                                &this.focus_handle,
                                ElementInputHandler::new(bounds, cx.entity()),
                                cx,
                            );
                            this.paint(bounds, window, cx);
                        });
                    },
                )
                .w_full()
                .flex_1()
                .min_h_0(),
            );
        if let (Some(popup), Some(bounds)) = (self.caret_popup.clone(), self.bounds)
            && popup_shown
        {
            let caret = bounds.origin + self.cursor_origin();
            surface = surface.child(
                deferred(
                    anchored()
                        .position(point(caret.x, caret.y + self.cell.height))
                        .snap_to_window_with_margin(px(8.))
                        .child(popup),
                )
                .with_priority(1),
            );
        }
        if let Some(error) = self.error.as_ref() {
            surface = surface.child(Alert::error("nvim-error", error.clone()).banner());
        } else if !self.writable {
            surface = surface.child(
                Label::new("Editor is read-only")
                    .text_color(cx.theme().muted_foreground)
                    .p_2(),
            );
        } else if !self.enabled() {
            surface = surface.child(
                Label::new("Waiting for Neovim’s initial grid…")
                    .text_color(cx.theme().muted_foreground)
                    .p_2(),
            );
        }
        if let Some(diagnostic) = self.diagnostic.as_ref() {
            surface = surface.child(
                Alert::warning("nvim-diagnostic", diagnostic.clone())
                    .banner()
                    .on_close(cx.listener(|this, _, _, cx| {
                        this.diagnostic = None;
                        cx.notify();
                    })),
            );
        }
        if !self.recovery.is_empty()
            || self
                .composition
                .as_ref()
                .is_some_and(|composition| composition.recoverable)
        {
            surface = surface.child(
                Alert::warning(
                    "nvim-composition-recovery",
                    "Uncommitted composition retained. Copy it before cancelling or restarting.",
                )
                .banner(),
            );
        }
        surface
    }
}

fn splice_composition(
    composition: &mut Composition,
    range: Option<Range<usize>>,
    text: &str,
    selection: Option<Range<usize>>,
) -> Result<(), String> {
    let old_length = composition.text.encode_utf16().count();
    let marked = composition.range.start..composition.range.start + old_length;
    let requested = range.unwrap_or(marked.clone());
    let base_range = exact_utf16_range(&composition.base, &composition.range)
        .ok_or_else(|| "The native composition anchor is no longer valid".to_string())?;
    let mut virtual_text = composition.base.clone();
    virtual_text.replace_range(base_range, &composition.text);
    let requested_bytes = exact_utf16_range(&virtual_text, &requested).ok_or_else(|| {
        "Native composition supplied an invalid Unicode replacement range; text was retained"
            .to_string()
    })?;
    let start = requested.start.min(marked.start);
    let old_end = requested.end.max(marked.end);
    let base_end = composition.range.end + old_end - marked.end;
    let length = text.encode_utf16().count();
    let new_end = old_end - (requested.end - requested.start) + length;
    virtual_text.replace_range(requested_bytes, text);
    let new_range = exact_utf16_range(&virtual_text, &(start..new_end)).ok_or_else(|| {
        "Native composition could not preserve the replacement boundary".to_string()
    })?;
    let selection = selection.unwrap_or(length..length);
    let selected_start = utf16_offset(text, utf16_floor(text, selection.start));
    let selected_end = utf16_offset(text, utf16_ceil(text, selection.end.max(selection.start)));
    composition.range = start..base_end;
    composition.text = virtual_text[new_range].to_owned();
    composition.selection =
        requested.start - start + selected_start..requested.start - start + selected_end;
    Ok(())
}

fn byte_floor(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn utf16_byte(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += character.len_utf16();
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

fn utf16_floor(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units + character.len_utf16() > offset {
            return byte;
        }
        units += character.len_utf16();
    }
    text.len()
}

fn utf16_ceil(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units >= offset {
            return byte;
        }
        units += character.len_utf16();
    }
    text.len()
}

fn utf16_offset(text: &str, byte: usize) -> usize {
    text[..byte_floor(text, byte)].encode_utf16().count()
}

fn exact_utf16_range(text: &str, range: &Range<usize>) -> Option<Range<usize>> {
    if range.start > range.end {
        return None;
    }
    Some(utf16_byte(text, range.start)?..utf16_byte(text, range.end)?)
}

fn buffer_offset(text: &str, cursor: (usize, usize)) -> usize {
    let mut start = 0;
    for _ in 0..cursor.0 {
        let Some(newline) = text[start..].find('\n') else {
            return text.len();
        };
        start += newline + 1;
    }
    let length = text[start..].find('\n').unwrap_or(text.len() - start);
    start + byte_floor(&text[start..start + length], cursor.1)
}

/// The native clipboard chords the composer takes before Neovim sees the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardChord {
    Copy,
    Cut,
    Paste,
}

/// macOS uses the Cmd key, where Ctrl-C/X/V stay Vim keys. Elsewhere Ctrl is
/// Vim's, so the terminal convention Ctrl-Shift-C/X/V is the native chord.
fn clipboard_chord(key: &Keystroke) -> Option<ClipboardChord> {
    let modifiers = key.modifiers;
    let native = if cfg!(target_os = "macos") {
        modifiers.platform && !modifiers.control && !modifiers.alt && !modifiers.shift
    } else {
        modifiers.control && modifiers.shift && !modifiers.platform && !modifiers.alt
    };
    if !native || modifiers.function {
        return None;
    }
    match key.key.as_str() {
        "c" => Some(ClipboardChord::Copy),
        "x" => Some(ClipboardChord::Cut),
        "v" => Some(ClipboardChord::Paste),
        _ => None,
    }
}

fn key_notation(key: &Keystroke) -> Option<String> {
    if native_text_key(key) {
        return None;
    }
    let name = match key.key.as_str() {
        "escape" => "Esc",
        "enter" | "return" => "CR",
        "backspace" => "BS",
        "delete" => "Del",
        "tab" => "Tab",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "space" if key.modifiers.control => "Space",
        name if name.strip_prefix('f').is_some_and(|number| {
            number
                .parse::<u8>()
                .is_ok_and(|number| (1..=22).contains(&number))
        }) =>
        {
            name
        }
        _ if key.modifiers.control || (key.modifiers.alt && key.key_char.is_none()) => {
            key.key.as_str()
        }
        _ => return None,
    };
    let mut modifiers = String::new();
    if key.modifiers.control {
        modifiers.push_str("C-");
    }
    if key.modifiers.alt {
        modifiers.push_str("M-");
    }
    if key.modifiers.shift {
        modifiers.push_str("S-");
    }
    Some(format!("<{modifiers}{name}>"))
}

fn mouse_modifiers(modifiers: Modifiers) -> String {
    let mut result = String::new();
    if modifiers.control {
        result.push('C');
    }
    if modifiers.alt {
        result.push('A');
    }
    if modifiers.shift {
        result.push('S');
    }
    if modifiers.platform {
        result.push('D');
    }
    result
}

impl NvimView {
    /// Text goes to Neovim; an image clipboard becomes a draft attachment through
    /// the workspace's `PasteImage` action so composer and palette share one path.
    fn paste_item(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if clipboard_has_image(&item) {
            window.dispatch_action(Box::new(PasteImage), cx);
        } else if let Some(text) = item.text() {
            self.send(NvimCommand::Paste(text), cx);
        }
    }
}

fn clipboard_has_image(item: &ClipboardItem) -> bool {
    item.entries()
        .iter()
        .any(|entry| matches!(entry, ClipboardEntry::Image(_)))
}

fn clipboard_content(item: &ClipboardItem) -> Option<ClipboardContent> {
    let text = item.text()?;
    if let Some(metadata) = item
        .metadata()
        .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
        && let (Some(regtype), Some(lines)) = (
            metadata["drostje_nvim_register"].as_str(),
            metadata["lines"].as_array(),
        )
    {
        let lines: Option<Vec<String>> = lines
            .iter()
            .map(|line| line.as_str().map(str::to_owned))
            .collect();
        if let Some(lines) = lines {
            let content = ClipboardContent {
                lines,
                regtype: regtype.into(),
            };
            if content.text() == text
                && (regtype == "v" || regtype == "V" || regtype.starts_with('\u{16}'))
            {
                return Some(content);
            }
        }
    }
    let linewise = text.ends_with('\n');
    let body = if linewise {
        &text[..text.len() - 1]
    } else {
        &text
    };
    Some(ClipboardContent {
        lines: body.split('\n').map(str::to_owned).collect(),
        regtype: if linewise { "V" } else { "v" }.into(),
    })
}

/// Every role Neovim's highlight groups draw from, flattened onto the editor background
/// so translucent kit colors read the same inside Neovim as around it.
fn nvim_theme(theme: &Theme) -> NvimTheme {
    let background = theme.background.to_rgb();
    let packed = |color: Hsla| u32::from(background.blend(color.to_rgb())) >> 8;
    NvimTheme {
        dark: theme.is_dark(),
        foreground: packed(theme.foreground),
        background: u32::from(background) >> 8,
        muted: packed(theme.muted_foreground),
        surface: packed(theme.muted),
        border: packed(theme.border),
        selection: packed(theme.selection),
        caret: packed(theme.caret),
        accent: packed(theme.primary),
        popover: packed(theme.popover),
        popover_foreground: packed(theme.popover_foreground),
        success: packed(theme.success),
        warning: packed(theme.warning),
        danger: packed(theme.danger),
        info: packed(theme.info),
    }
}

fn colors(grid: &GridSnapshot, id: u64) -> (Hsla, Hsla, Hsla) {
    let default = Highlight::default();
    let highlight = grid.highlights.get(&id).unwrap_or(&default);
    let mut foreground = highlight.foreground.unwrap_or(grid.default_fg);
    let mut background = highlight.background.unwrap_or(grid.default_bg);
    if highlight.reverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    let blend = highlight.blend.min(100) as u32;
    let mut blended = 0;
    for shift in [0, 8, 16] {
        let channel = (((background >> shift) & 255) * (100 - blend)
            + ((grid.default_bg >> shift) & 255) * blend)
            / 100;
        blended |= channel << shift;
    }
    (
        rgb(foreground).into(),
        rgb(blended).into(),
        rgb(highlight.special.unwrap_or(grid.default_sp)).into(),
    )
}

fn text_run(grid: &GridSnapshot, font: &Font, id: u64, len: usize) -> TextRun {
    let default = Highlight::default();
    let highlight = grid.highlights.get(&id).unwrap_or(&default);
    let (color, _, special) = colors(grid, id);
    let mut font = font.clone();
    if highlight.bold {
        font = font.bold();
    }
    if highlight.italic {
        font = font.italic();
    }
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: (highlight.underline || highlight.undercurl).then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(special),
            wavy: highlight.undercurl,
        }),
        strikethrough: highlight.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(color),
        }),
    }
}

fn shape_chunks(
    grid: &GridSnapshot,
    font: &Font,
    chunks: &[HighlightedChunk],
    prefix: &str,
    font_size: Pixels,
    window: &Window,
) -> ShapedLine {
    let mut text = prefix.to_owned();
    let mut runs = Vec::with_capacity(chunks.len() + 1);
    if !prefix.is_empty() {
        runs.push(text_run(grid, font, 0, prefix.len()));
    }
    for chunk in chunks {
        if chunk.text.is_empty() {
            continue;
        }
        text.push_str(&chunk.text);
        let mut run = text_run(grid, font, chunk.hl_id, chunk.text.len());
        run.background_color = Some(colors(grid, chunk.hl_id).1);
        runs.push(run);
    }
    window
        .text_system()
        .shape_line(text.into(), font_size, &runs, None)
}

impl GridText {
    fn append(&mut self, origin: Point<Pixels>, line: &LineLayout, color: Hsla) {
        let offset = origin.x - self.origin.x;
        let byte_offset = self.layout.len;
        for run in &line.runs {
            if self
                .layout
                .runs
                .last()
                .is_none_or(|last| last.font_id != run.font_id)
            {
                self.layout.runs.push(ShapedRun {
                    font_id: run.font_id,
                    glyphs: Vec::with_capacity(run.glyphs.len()),
                });
            }
            // The run was just inserted if absent.
            if let Some(destination) = self.layout.runs.last_mut() {
                destination.glyphs.extend(run.glyphs.iter().map(|glyph| {
                    let mut glyph = glyph.clone();
                    glyph.position.x += offset;
                    glyph.index += byte_offset;
                    glyph
                }));
            }
        }
        self.layout.width = self.layout.width.max(offset + line.width);
        self.layout.len += line.len;
        if let Some(decoration) = self.decorations.last_mut()
            && decoration.color == color
        {
            decoration.len += line.len as u32;
        } else {
            self.decorations.push(DecorationRun {
                len: line.len as u32,
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    }
}

impl RowShape {
    fn push_text(&mut self, origin: Point<Pixels>, line: ShapedLine, run: TextRun) {
        if run.underline.is_some() || run.strikethrough.is_some() {
            // Retain exact per-cell decoration extent, including overhang and
            // fallback metrics. A concatenated underline would cross grid gaps.
            self.texts.push(RowText::Decorated(self.decorated.len()));
            self.decorated.push(PaintedText { origin, line });
            return;
        }
        if let Some(RowText::Batched(batch)) = self.texts.last_mut()
            && batch.layout.ascent == line.ascent
            && batch.layout.descent == line.descent
        {
            batch.append(origin, &line, run.color);
            return;
        }
        let mut batch = GridText {
            origin,
            layout: LineLayout {
                font_size: line.font_size,
                ascent: line.ascent,
                descent: line.descent,
                ..LineLayout::default()
            },
            decorations: Vec::new(),
        };
        batch.append(origin, &line, run.color);
        self.texts.push(RowText::Batched(batch));
    }
}

fn shape_row(
    row: &[Cell],
    grid: &GridSnapshot,
    font: &Font,
    cell: Size<Pixels>,
    font_size: Pixels,
    text_system: &WindowTextSystem,
) -> RowShape {
    let mut shape = RowShape::default();
    let default_background = rgb(grid.default_bg).into();
    for (column, value) in row.iter().enumerate() {
        let origin = point(cell.width * column as f32, px(0.));
        let background = colors(grid, value.hl_id).1;
        if background != default_background {
            if let Some((rect, previous)) = shape.backgrounds.last_mut()
                && *previous == background
                && rect.right() == origin.x
            {
                rect.size.width += cell.width;
            } else {
                shape
                    .backgrounds
                    .push((Bounds::new(origin, cell), background));
            }
        }
        let decorated = grid.highlights.get(&value.hl_id).is_some_and(|highlight| {
            highlight.underline || highlight.undercurl || highlight.strikethrough
        });
        if value.text.is_empty() || (!decorated && value.text == " ") {
            continue;
        }
        // Shape the complete grapheme Neovim put in this cell, never a
        // concatenated row. Wide continuation cells contribute no glyphs;
        // combining marks, fallback fonts and emoji retain their shaped offsets.
        let run = text_run(grid, font, value.hl_id, value.text.len());
        let line = text_system.shape_line(
            value.text.clone().into(),
            font_size,
            std::slice::from_ref(&run),
            None,
        );
        shape.push_text(origin, line, run);
    }
    shape
}

fn make_frame(
    grid: &GridSnapshot,
    font: &Font,
    cell: Size<Pixels>,
    font_size: Pixels,
    viewport: Size<Pixels>,
    rows: Vec<Arc<RowShape>>,
    window: &Window,
) -> Frame {
    let visible_rows = (viewport.height / cell.height).floor().max(1.) as usize;
    let mut frame = Frame {
        texts: Vec::new(),
        rows,
        overlays: Vec::new(),
        cmd_cursor: None,
        cmd_layout: None,
    };
    let strip_length = grid.cmdline_block.len() + grid.cmdline.len();
    let skip = strip_length.saturating_sub(visible_rows.saturating_sub(1));
    let mut row = grid.rows.len();
    for chunks in grid.cmdline_block.iter().skip(skip) {
        frame.texts.push(PaintedText {
            origin: point(px(0.), cell.height * row as f32),
            line: shape_chunks(grid, font, chunks, "", font_size, window),
        });
        row += 1;
    }
    for (level, cmd) in grid
        .cmdline
        .iter()
        .skip(skip.saturating_sub(grid.cmdline_block.len()))
    {
        let prefix = format!("{}{}{}", cmd.firstc, cmd.prompt, " ".repeat(cmd.indent));
        let line = shape_chunks(grid, font, &cmd.content, &prefix, font_size, window);
        let text: String = cmd
            .content
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect();
        let byte = byte_floor(&text, cmd.pos);
        let before = chunk_range(&cmd.content, 0..byte);
        let prefix_line = shape_chunks(grid, font, &before, &prefix, font_size, window);
        let horizontal_scroll = (prefix_line.width() + cell.width - viewport.width).max(px(0.));
        let origin = point(-horizontal_scroll, cell.height * row as f32);
        let cursor = origin + point(prefix_line.width(), px(0.));
        let active = grid
            .cmdline
            .last_key_value()
            .is_some_and(|(active, _)| active == level);
        if active {
            frame.cmd_cursor = Some(cursor);
            frame.cmd_layout = Some((line.clone(), origin, prefix.len()));
        }
        if let Some((special, shift)) = cmd.special.as_ref() {
            let special_line = window.text_system().shape_line(
                special.clone().into(),
                font_size,
                &[text_run(grid, font, 0, special.len())],
                None,
            );
            if *shift {
                let suffix = chunk_range(&cmd.content, byte..text.len());
                frame.texts.push(PaintedText {
                    origin,
                    line: prefix_line,
                });
                frame.texts.push(PaintedText {
                    origin: cursor + point(special_line.width(), px(0.)),
                    line: shape_chunks(grid, font, &suffix, "", font_size, window),
                });
            } else {
                frame.texts.push(PaintedText { origin, line });
            }
            frame.overlays.push(PaintedText {
                origin: cursor,
                line: special_line,
            });
        } else {
            frame.texts.push(PaintedText { origin, line });
        }
        row += 1;
    }
    frame
}

fn chunk_range(chunks: &[HighlightedChunk], range: Range<usize>) -> Vec<HighlightedChunk> {
    let mut offset = 0;
    let mut result = Vec::new();
    for chunk in chunks {
        let start = range.start.saturating_sub(offset).min(chunk.text.len());
        let end = range.end.saturating_sub(offset).min(chunk.text.len());
        if start < end {
            result.push(HighlightedChunk {
                hl_id: chunk.hl_id,
                text: chunk.text[start..end].to_owned(),
            });
        }
        offset += chunk.text.len();
        if offset >= range.end {
            break;
        }
    }
    result
}

#[cfg(test)]
mod row_cache_tests {
    use super::{Cell, GridSnapshot, Highlight, NvimTheme, RowCache, RowCacheKey, RowShape};
    use gpui_kit::{font, px, size};
    use std::cell::Cell as Counter;
    use std::sync::Arc;

    fn key(grid: &GridSnapshot) -> RowCacheKey {
        RowCacheKey {
            font: font("monospace"),
            font_size: px(14.),
            cell: size(px(8.), px(18.)),
            viewport: size(px(800.), px(400.)),
            scale_factor: 2.,
            cols: grid.cols,
            theme: NvimTheme::default(),
            defaults: [grid.default_fg, grid.default_bg, grid.default_sp],
            highlights: Arc::clone(&grid.highlights),
        }
    }

    #[test]
    fn unchanged_and_scrolled_rows_reuse_shapes_but_evicted_rows_do_not() {
        let a = Arc::new(vec![Cell {
            text: "a".into(),
            hl_id: 0,
        }]);
        let b = Arc::new(vec![Cell {
            text: "b".into(),
            hl_id: 0,
        }]);
        let mut grid = GridSnapshot {
            cols: 1,
            rows: vec![a.clone(), b.clone(), a.clone()],
            ..GridSnapshot::default()
        };
        let mut cache = RowCache::default();
        let calls = Counter::new(0);
        let shape = |_: &[Cell]| {
            calls.set(calls.get() + 1);
            RowShape::default()
        };
        let first = cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(calls.get(), 2);
        assert!(Arc::ptr_eq(&first[0], &first[2]));
        grid.cursor = (1, 0);
        let cursor = cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(calls.get(), 2);
        assert!(Arc::ptr_eq(&first[0], &cursor[0]));
        grid.rows.swap(0, 1);
        let scrolled = cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(calls.get(), 2);
        assert!(Arc::ptr_eq(&first[1], &scrolled[0]));
        Arc::make_mut(&mut grid.rows[1])[0].text = "changed".into();
        cache.prepare(key(&grid), &grid.rows, shape);
        assert_eq!(calls.get(), 3);
        // Only the visible slice is retained. Returning to an evicted row must
        // shape it again rather than keeping a history of every visited row.
        cache.prepare(key(&grid), &grid.rows[..1], shape);
        cache.prepare(key(&grid), &[a], shape);
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn geometry_font_theme_and_highlights_invalidate_retained_shapes() {
        let grid = GridSnapshot {
            cols: 1,
            rows: vec![Arc::new(vec![Cell::default()])],
            ..GridSnapshot::default()
        };
        let mut cache = RowCache::default();
        let calls = Counter::new(0);
        let shape = |_: &[Cell]| {
            calls.set(calls.get() + 1);
            RowShape::default()
        };
        let changes: &[fn(&mut RowCacheKey)] = &[
            |key| key.font = font("another font"),
            |key| key.font_size = px(15.),
            |key| key.cell.width = px(9.),
            |key| key.cell.height = px(20.),
            |key| key.viewport.width = px(400.),
            |key| key.scale_factor = 1.,
            |key| key.cols = 2,
            |key| key.theme.foreground = 1,
            |key| key.defaults[0] = 1,
            |key| {
                Arc::make_mut(&mut key.highlights).insert(
                    0,
                    Highlight {
                        bold: true,
                        ..Highlight::default()
                    },
                );
            },
        ];
        for change in changes {
            cache.prepare(key(&grid), &grid.rows, shape);
            let before = calls.get();
            let mut changed = key(&grid);
            change(&mut changed);
            cache.prepare(changed, &grid.rows, shape);
            assert_eq!(calls.get(), before + 1);
        }
    }
}

#[cfg(test)]
mod clipboard_chord_tests {
    #[test]
    fn native_clipboard_chord_uses_the_platform_paste_key_and_leaves_vim_keys_alone() {
        use super::{ClipboardChord, clipboard_chord};
        use gpui_kit::Keystroke;
        let chord = |source: &str| clipboard_chord(&Keystroke::parse(source).unwrap());
        let (native, vim) = if cfg!(target_os = "macos") {
            ("cmd", "ctrl-shift")
        } else {
            ("ctrl-shift", "cmd")
        };
        assert_eq!(chord(&format!("{native}-v")), Some(ClipboardChord::Paste));
        assert_eq!(chord(&format!("{native}-c")), Some(ClipboardChord::Copy));
        assert_eq!(chord(&format!("{native}-x")), Some(ClipboardChord::Cut));
        assert_eq!(chord(&format!("{native}-alt-v")), None);
        assert_eq!(chord(&format!("{native}-enter")), None);
        assert_eq!(chord(&format!("{vim}-v")), None);
        // Vim's own Ctrl-V (blockwise visual, literal insert) and Ctrl-C stay Neovim's.
        assert_eq!(chord("ctrl-v"), None);
        assert_eq!(chord("ctrl-c"), None);
        assert_eq!(chord("v"), None);
    }
}

#[cfg(all(test, feature = "runtime-qa"))]
mod tests {
    use super::{DraftKey, Instant, NvimCommand, NvimEditor, NvimView};
    use crate::neovim::NvimConfig;
    use gpui_kit::component::Root;
    use gpui_kit::{AppContext, Entity, EntityInputHandler, TestAppContext};
    use std::time::Duration;

    #[gpui_kit::test]
    fn batched_rows_preserve_independent_unicode_cell_geometry(cx: &mut TestAppContext) {
        use super::{Cell, GridSnapshot, Highlight, RowText, shape_row, text_run};
        use gpui_kit::{
            FontId, GlyphId, Hsla, LineLayout, Pixels, Point, WindowTextSystem, font, point, px,
            size,
        };

        fn glyphs(
            layout: &LineLayout,
            origin: Point<Pixels>,
            color: impl Fn(usize) -> Hsla,
        ) -> Vec<(FontId, GlyphId, Point<Pixels>, bool, Hsla)> {
            let baseline = (px(22.) - layout.ascent - layout.descent) / 2. + layout.ascent;
            layout
                .runs
                .iter()
                .flat_map(|run| {
                    let color = &color;
                    run.glyphs.iter().map(move |glyph| {
                        (
                            run.font_id,
                            glyph.id,
                            origin + glyph.position + point(px(0.), baseline),
                            glyph.is_emoji,
                            color(glyph.index),
                        )
                    })
                })
                .collect()
        }

        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let text_system = WindowTextSystem::new(cx.text_system().clone());
            let font = font("Menlo");
            let cell = size(px(9.), px(22.));
            let mut grid = GridSnapshot::default();
            let highlights = std::sync::Arc::make_mut(&mut grid.highlights);
            highlights.insert(
                1,
                Highlight {
                    foreground: Some(0xff0000),
                    italic: true,
                    ..Highlight::default()
                },
            );
            highlights.insert(
                2,
                Highlight {
                    underline: true,
                    undercurl: true,
                    strikethrough: true,
                    ..Highlight::default()
                },
            );
            // Deliberately non-natural cell width, ligature candidates, spaces,
            // wide continuations, combining marks, emoji, RTL and fallback fonts.
            let row: Vec<Cell> = [
                ("f", 0),
                ("i", 0),
                (" ", 0),
                ("界", 1),
                ("", 1),
                ("e\u{301}", 0),
                ("🙂", 0),
                ("", 0),
                ("x", 2),
                ("ג", 0),
                ("A", 1),
                (" ", 2),
                ("_", 2),
            ]
            .into_iter()
            .map(|(text, hl_id)| Cell {
                text: text.into(),
                hl_id,
            })
            .collect();
            let mut expected = Vec::new();
            let mut expected_decorations = Vec::new();
            for (column, value) in row.iter().enumerate() {
                let run = text_run(&grid, &font, value.hl_id, value.text.len());
                let decorated = run.underline.is_some() || run.strikethrough.is_some();
                if value.text.is_empty() || (!decorated && value.text == " ") {
                    continue;
                }
                let line = text_system.shape_line(
                    value.text.clone().into(),
                    px(14.),
                    std::slice::from_ref(&run),
                    None,
                );
                let origin = point(cell.width * column as f32, px(0.));
                expected.extend(glyphs(&line, origin, |_| run.color));
                if decorated {
                    expected_decorations.push((origin, line.width()));
                }
            }
            let shape = shape_row(&row, &grid, &font, cell, px(14.), &text_system);
            let mut actual = Vec::new();
            let mut actual_decorations = Vec::new();
            for text in &shape.texts {
                match text {
                    RowText::Batched(batch) => {
                        actual.extend(glyphs(&batch.layout, batch.origin, |index| {
                            let mut end = 0;
                            batch
                                .decorations
                                .iter()
                                .find_map(|decoration| {
                                    end += decoration.len as usize;
                                    (index < end).then_some(decoration.color)
                                })
                                .expect("every positioned glyph has a color")
                        }))
                    }
                    RowText::Decorated(index) => {
                        let text = &shape.decorated[*index];
                        let column = (text.origin.x / cell.width).round() as usize;
                        let run = text_run(&grid, &font, row[column].hl_id, text.line.len());
                        actual.extend(glyphs(&text.line, text.origin, |_| run.color));
                        actual_decorations.push((text.origin, text.line.width()));
                    }
                }
            }
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(&expected) {
                assert_eq!(
                    (actual.0, actual.1, actual.3, actual.4),
                    (expected.0, expected.1, expected.3, expected.4)
                );
                assert!((actual.2.x - expected.2.x).abs() < px(0.001));
                assert!((actual.2.y - expected.2.y).abs() < px(0.001));
            }
            assert_eq!(actual_decorations, expected_decorations);
            assert!(
                shape.texts.len()
                    < row
                        .iter()
                        .filter(|cell| !cell.text.is_empty() && cell.text != " ")
                        .count()
            );
        });
    }

    async fn until(
        cx: &mut TestAppContext,
        view: &Entity<NvimView>,
        predicate: impl Fn(&NvimView) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| predicate(view.read(cx))) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Neovim view did not reach expected state: {:?}",
                cx.read(|cx| {
                    let view = view.read(cx);
                    (
                        view.ready,
                        view.writable,
                        view.error.clone(),
                        view.draft.clone(),
                        view.grid.generation,
                        view.grid.rows.len(),
                    )
                })
            );
            smol::Timer::after(Duration::from_millis(10)).await;
        }
    }
    async fn service<T>(cx: &mut TestAppContext, future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            if let Some(result) = smol::future::or(async { Some(future.as_mut().await) }, async {
                smol::Timer::after(Duration::from_millis(10)).await;
                None
            })
            .await
            {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "Native Neovim operation timed out"
            );
        }
    }

    #[gpui_kit::test]
    #[ignore = "requires installed Neovim; isolated native typing and flushed grids"]
    async fn native_typing_bursts_reach_the_flushed_grid(cx: &mut TestAppContext) {
        cx.dispatcher.allow_parking();
        let path = crate::platform::resolve_executable("nvim", None).unwrap();
        let editor = NvimEditor::open(NvimConfig::new(path)).await.unwrap();
        let key = DraftKey::Thread(uuid::Uuid::new_v4());
        editor.open_draft(key.clone(), String::new()).await.unwrap();
        editor.focus_draft(key).await.unwrap();
        editor
            .try_command(NvimCommand::Input("<Esc>i".into()))
            .unwrap();
        cx.update(gpui_kit::init);
        cx.update(crate::ui::commands::bind_keys);
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let entity = cx.new(|cx| NvimView::new(editor.clone(), window, cx));
            entity.update(cx, |view, cx| view.focus(window, cx));
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        until(cx, &view, |view| view.enabled()).await;
        let mut expected = String::new();
        for burst in ["gghjkl", "burst", "visible", ":?{}~", "é"] {
            // No waits, snapshots or extra key are needed to make the final
            // character visible. Dispatch reaches the native input handler.
            for character in burst.chars() {
                cx.update_window(window.into(), |_, window, cx| {
                    assert!(window.dispatch_keystroke(
                        gpui_kit::Keystroke::parse(&character.to_string()).unwrap(),
                        cx,
                    ));
                    assert!(!window.has_pending_keystrokes());
                })
                .unwrap();
            }
            expected.push_str(burst);
            until(cx, &view, |view| {
                view.draft
                    .as_ref()
                    .is_some_and(|draft| draft.text == expected)
                    && view.grid.rows.iter().any(|row| {
                        row.iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<String>()
                            .contains(&expected)
                    })
            })
            .await;
            cx.read(|cx| {
                let view = view.read(cx);
                assert!(view.error.is_none());
                assert!(!view.has_composition());
                assert!(view.recovery_text().is_empty());
            });
        }
        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_keystroke(gpui_kit::Keystroke::parse("escape").unwrap(), cx);
        })
        .unwrap();
        until(cx, &view, |view| view.mode() == "normal").await;
        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_keystroke(gpui_kit::Keystroke::parse(":").unwrap(), cx);
        })
        .unwrap();
        until(cx, &view, |view| !view.grid.cmdline.is_empty()).await;
        for (key, text, modifiers) in [
            (
                "g",
                "G",
                gpui_kit::Modifiers {
                    shift: true,
                    ..Default::default()
                },
            ),
            (
                "e",
                "é",
                gpui_kit::Modifiers {
                    alt: true,
                    ..Default::default()
                },
            ),
        ] {
            cx.update_window(window.into(), |_, window, cx| {
                window.dispatch_keystroke(
                    gpui_kit::Keystroke {
                        key: key.into(),
                        key_char: Some(text.into()),
                        modifiers,
                    },
                    cx,
                );
                assert!(!window.has_pending_keystrokes());
            })
            .unwrap();
        }
        until(cx, &view, |view| {
            view.grid
                .cmdline
                .last_key_value()
                .is_some_and(|(_, command)| {
                    command
                        .content
                        .iter()
                        .map(|chunk| chunk.text.as_str())
                        .collect::<String>()
                        == "Gé"
                })
        })
        .await;
        cx.read(|cx| {
            assert_eq!(view.read(cx).draft.as_ref().unwrap().text, expected);
            assert!(!view.read(cx).has_composition());
        });
        editor.shutdown().await.unwrap();
    }

    #[gpui_kit::test]
    #[ignore = "requires installed Neovim; isolated native input and real editing"]
    async fn native_composition_is_recovery_only_until_committed(cx: &mut TestAppContext) {
        cx.dispatcher.allow_parking();
        let path = crate::platform::resolve_executable("nvim", None).unwrap();
        let editor = NvimEditor::open(NvimConfig::new(path)).await.unwrap();
        let key = DraftKey::Thread(uuid::Uuid::new_v4());
        editor
            .open_draft(key.clone(), "before".into())
            .await
            .unwrap();
        editor.focus_draft(key.clone()).await.unwrap();
        let initial = editor.snapshot(key.clone()).await.unwrap();
        editor
            .try_command(NvimCommand::Input("<Esc>A".into()))
            .unwrap();
        cx.update(gpui_kit::init);
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let entity = cx.new(|cx| NvimView::new(editor.clone(), window, cx));
            entity.update(cx, |view, cx| view.focus(window, cx));
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        until(cx, &view, |view| {
            view.enabled() && view.draft.as_ref().is_some_and(|draft| draft.cursor.1 == 6)
        })
        .await;
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(Some(6..6), "仮😀", Some(3..3), window, cx);
                assert_eq!(view.recovery_text(), "仮😀");
                assert!(view.has_composition());
            })
        })
        .unwrap();
        assert!(editor.snapshot(key.clone()).await.is_err());
        assert!(
            editor
                .replace_if_revision(key.clone(), initial.revision(), "unsafe".into())
                .await
                .is_err()
        );
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.replace_text_in_range(None, "確定😀", window, cx);
            })
        })
        .unwrap();
        let committed = service(cx, editor.snapshot(key.clone())).await.unwrap();
        assert_eq!(committed.text, "before確定😀");
        until(cx, &view, |view| !view.has_composition()).await;
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.set_writable(false, cx);
                view.replace_text_in_range(None, "discard", window, cx);
                view.replace_and_mark_text_in_range(None, "discard", None, window, cx);
                assert!(view.recovery_text().is_empty());
                view.set_writable(true, cx);
            })
        })
        .unwrap();
        assert_eq!(
            editor.snapshot(key.clone()).await.unwrap().text,
            committed.text
        );
        until(cx, &view, |view| {
            view.draft
                .as_ref()
                .is_some_and(|draft| draft.text == committed.text)
        })
        .await;
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(None, "retained", None, window, cx);
            })
        })
        .unwrap();
        assert_eq!(cx.read(|cx| view.read(cx).recovery_text()), "retained");
        editor.restart().await.unwrap();
        until(cx, &view, |view| view.ready && view.error.is_none()).await;
        assert_eq!(cx.read(|cx| view.read(cx).recovery_text()), "retained");
        assert!(editor.snapshot(key.clone()).await.is_err());
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.cancel_composition(window, cx);
            })
        })
        .unwrap();
        assert_eq!(editor.snapshot(key).await.unwrap().text, committed.text);
        editor.persistence_barrier().await.unwrap();
        editor.shutdown().await.unwrap();
    }
}
