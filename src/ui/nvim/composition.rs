use super::{
    NvimView, NvimViewEvent,
    offsets::{
        buffer_offset, byte_floor, exact_utf16_range, utf16_ceil, utf16_floor, utf16_offset,
    },
    shaping::text_run,
};
use crate::model::{DraftKey, DraftRevision};
use crate::neovim::NvimCommand;
use gpui_kit::*;
use std::{ops::Range, time::Instant};

#[derive(Clone)]
pub(super) enum Target {
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

pub(super) struct Composition {
    pub(super) target: Target,
    pub(super) range: Range<usize>,
    pub(super) base: String,
    pub(super) text: String,
    pub(super) selection: Range<usize>,
    pub(super) anchor: Point<Pixels>,
    pub(super) recoverable: bool,
}

impl NvimView {
    pub(super) fn base_text(&self) -> (String, usize) {
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

    pub(super) fn native_text(&self) -> (String, Range<usize>) {
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

    pub(super) fn target(&self) -> Option<Target> {
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

    pub(super) fn target_current(&self, target: &Target) -> bool {
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

    pub(super) fn discard_composition(&mut self, cx: &mut Context<Self>) {
        self.composition = None;
        self.recovery.clear();
        self.send(NvimCommand::SetComposition(false), cx);
        cx.emit(NvimViewEvent::CompositionChanged(self.has_composition()));
        cx.notify();
    }

    pub(super) fn retain_native(
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

    pub(super) fn replace(
        &mut self,
        target: Target,
        range: Range<usize>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
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

pub(super) fn splice_composition(
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
