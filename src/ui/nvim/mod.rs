//! The Neovim prompt editor surface: the sole consumer of an editor's event channel, which
//! paints Neovim's flushed grid and forwards native keys, mouse and IME input to it.
mod clipboard;
mod composition;
mod cursor;
mod frame;
mod input;
mod offsets;
mod paint;
mod row_cache;
mod shaping;
#[cfg(all(test, feature = "runtime-qa"))]
mod tests;

use super::commands::{AcceptCompletion, DismissCompletion, NextCompletion, PreviousCompletion};
use crate::model::DraftSnapshot;
use crate::neovim::grid::GridSnapshot;
use crate::neovim::{NvimCommand, NvimEditor, NvimEvent, NvimTheme};
use clipboard::clipboard_content;
use composition::Composition;
use frame::Frame;
use ghostty_terminal::input_surface::native_input_context;
use gpui_kit::{
    component::{ActiveTheme, alert::Alert, label::Label},
    prelude::*,
    *,
};
use row_cache::RowCache;
use std::{
    collections::VecDeque,
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
