use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use rmpv::Value;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// An empty string is a wide glyph's continuation, not a blank cell.
    pub text: CellText,
    pub hl_id: u64,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: CellText::BLANK,
            hl_id: 0,
        }
    }
}

const INLINE_CAPACITY: usize = 22;

/// The grapheme Neovim put in one cell. Text up to `INLINE_CAPACITY` bytes
/// (every single code point, most combining sequences) is stored inline, so
/// building, cloning and copy-on-write of a row never touch the heap for it;
/// longer clusters share one `Arc<str>`. The representation is canonical:
/// text that fits is always inline.
#[derive(Clone)]
pub struct CellText(CellTextRepr);

#[derive(Clone)]
enum CellTextRepr {
    /// `bytes[..len]` is valid UTF-8 copied from a `&str`; the rest is zero.
    Inline {
        len: u8,
        bytes: [u8; INLINE_CAPACITY],
    },
    Heap(Arc<str>),
}

impl CellText {
    /// A blank cell, as Neovim clears it.
    pub const BLANK: Self = Self::inline(b" ");

    const fn inline(text: &[u8]) -> Self {
        let mut bytes = [0; INLINE_CAPACITY];
        let mut index = 0;
        while index < text.len() {
            bytes[index] = text[index];
            index += 1;
        }
        Self(CellTextRepr::Inline {
            len: text.len() as u8,
            bytes,
        })
    }

    pub fn new(text: &str) -> Self {
        if text.len() <= INLINE_CAPACITY {
            Self::inline(text.as_bytes())
        } else {
            Self(CellTextRepr::Heap(Arc::from(text)))
        }
    }

    pub fn as_str(&self) -> &str {
        match &self.0 {
            // Inline bytes were copied whole from a `&str`, so this never
            // falls back; the check only keeps the module free of `unsafe`.
            CellTextRepr::Inline { len, bytes } => {
                std::str::from_utf8(&bytes[..usize::from(*len)]).unwrap_or_default()
            }
            CellTextRepr::Heap(text) => text,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match &self.0 {
            CellTextRepr::Inline { len, bytes } => &bytes[..usize::from(*len)],
            CellTextRepr::Heap(text) => text.as_bytes(),
        }
    }

    pub fn len(&self) -> usize {
        self.as_bytes().len()
    }

    pub fn is_empty(&self) -> bool {
        self.as_bytes().is_empty()
    }

    #[cfg(test)]
    fn is_heap(&self) -> bool {
        matches!(self.0, CellTextRepr::Heap(_))
    }
}

impl std::ops::Deref for CellText {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for CellText {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for CellText {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for CellText {
    fn from(text: String) -> Self {
        Self::new(&text)
    }
}

impl PartialEq for CellText {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for CellText {}

impl PartialEq<str> for CellText {
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<&str> for CellText {
    fn eq(&self, other: &&str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl std::fmt::Debug for CellText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), formatter)
    }
}

impl std::fmt::Display for CellText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Highlight {
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub special: Option<u32>,
    pub reverse: bool,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub undercurl: bool,
    pub strikethrough: bool,
    pub blend: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorStyle {
    pub shape: String,
    pub cell_percentage: u8,
    pub blinkwait: u64,
    pub blinkon: u64,
    pub blinkoff: u64,
    pub attr_id: u64,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            shape: "block".into(),
            cell_percentage: 100,
            blinkwait: 0,
            blinkon: 0,
            blinkoff: 0,
            attr_id: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightedChunk {
    pub hl_id: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmdline {
    pub content: Vec<HighlightedChunk>,
    /// UTF-8 byte offset into content, excluding firstc, prompt and indent.
    pub pos: usize,
    pub firstc: String,
    pub prompt: String,
    pub indent: usize,
    pub special: Option<(String, bool)>,
}

/// Every collection is shared with earlier snapshots until a redraw changes
/// it, so publishing a flush copies pointers, not grid or editor state.
#[derive(Clone, Debug)]
pub struct GridSnapshot {
    pub cols: usize,
    pub rows: Vec<Arc<Vec<Cell>>>,
    pub highlights: Arc<HashMap<u64, Highlight>>,
    pub default_fg: u32,
    pub default_bg: u32,
    pub default_sp: u32,
    pub cursor: (usize, usize),
    pub mode: Arc<str>,
    pub mode_index: usize,
    pub mode_styles: Arc<Vec<CursorStyle>>,
    pub cursor_style_enabled: bool,
    pub busy: bool,
    pub mouse_enabled: bool,
    pub bell: u64,
    pub options: Arc<HashMap<String, Value>>,
    pub cmdline: Arc<BTreeMap<u64, Cmdline>>,
    pub cmdline_block: Arc<Vec<Vec<HighlightedChunk>>>,
    pub generation: Uuid,
    pub serial: u64,
}

impl Default for GridSnapshot {
    fn default() -> Self {
        Self {
            cols: 0,
            rows: Vec::new(),
            highlights: Arc::new(HashMap::new()),
            default_fg: 0xffffff,
            default_bg: 0x000000,
            default_sp: 0xffffff,
            cursor: (0, 0),
            mode: "normal".into(),
            mode_index: 0,
            mode_styles: Arc::default(),
            cursor_style_enabled: false,
            busy: false,
            mouse_enabled: false,
            bell: 0,
            options: Arc::default(),
            cmdline: Arc::default(),
            cmdline_block: Arc::default(),
            generation: Uuid::nil(),
            serial: 0,
        }
    }
}

/// Only flush publishes staging. Published rows and highlights stay immutable
/// through copy-on-write, including across separate redraw notifications.
pub struct GridReducer {
    staging: GridSnapshot,
    committed: Arc<GridSnapshot>,
}

impl GridReducer {
    pub fn new(generation: Uuid) -> Self {
        let staging = GridSnapshot {
            generation,
            ..GridSnapshot::default()
        };
        let committed = Arc::new(staging.clone());
        Self { staging, committed }
    }

    pub fn snapshot(&self) -> Arc<GridSnapshot> {
        Arc::clone(&self.committed)
    }

    /// Accepts the redraw argument array, not the surrounding RPC envelope.
    /// A diagnostic is terminal for the session; staging after an error must
    /// not be reused. Earlier flushes remain valid committed snapshots.
    pub fn apply(&mut self, events: &[Value]) -> Result<Vec<Arc<GridSnapshot>>> {
        let mut publications = Vec::new();
        for (event_index, event) in events.iter().enumerate() {
            let event =
                array(event, "event").with_context(|| format!("redraw event {event_index}"))?;
            let name = string(required(event, 0, "event name")?, "event name")?;
            let minimum = match name {
                "grid_resize" | "grid_cursor_goto" => 3,
                "grid_line" | "default_colors_set" => 5,
                "grid_clear"
                | "grid_destroy"
                | "cmdline_hide"
                | "cmdline_block_show"
                | "cmdline_block_append" => 1,
                "grid_scroll" => 7,
                "hl_attr_define" => 4,
                "mode_info_set" | "mode_change" | "option_set" | "cmdline_pos" => 2,
                "cmdline_special_char" => 3,
                "cmdline_show" => 6,
                "busy_start" | "busy_stop" | "mouse_on" | "mouse_off" | "bell" | "visual_bell"
                | "flush" | "cmdline_block_hide" => 0,
                _ => continue,
            };
            ensure!(event.len() > 1, "redraw {name}: missing parameter tuple");
            for (tuple_index, tuple) in event[1..].iter().enumerate() {
                let context = || format!("redraw {name} tuple {tuple_index}");
                let tuple = array(tuple, "parameter tuple").with_context(context)?;
                ensure!(
                    tuple.len() >= minimum,
                    "redraw {name} tuple {tuple_index}: expected at least {minimum} parameters, got {}",
                    tuple.len()
                );
                if name == "flush" {
                    self.staging.serial = self
                        .staging
                        .serial
                        .checked_add(1)
                        .context("redraw flush: snapshot serial exhausted")?;
                    self.committed = Arc::new(self.staging.clone());
                    publications.push(Arc::clone(&self.committed));
                } else {
                    self.apply_tuple(name, tuple).with_context(context)?;
                }
            }
        }
        Ok(publications)
    }

    fn apply_tuple(&mut self, name: &str, args: &[Value]) -> Result<()> {
        match name {
            "grid_resize" => {
                single_grid(&args[0])?;
                let cols = index(&args[1], "width")?;
                let rows = index(&args[2], "height")?;
                ensure!(cols > 0 && rows > 0, "grid dimensions must be positive");
                let cells = cols.checked_mul(rows).context("grid dimensions overflow")?;
                ensure!(
                    cells <= isize::MAX as usize / std::mem::size_of::<Cell>(),
                    "grid dimensions exceed addressable storage"
                );
                self.resize(cols, rows)?;
            }
            "grid_line" => self.line(args)?,
            "grid_clear" => {
                single_grid(&args[0])?;
                if !self.staging.rows.is_empty() {
                    let blank = blank_row(self.staging.cols)?;
                    self.staging.rows.fill(blank);
                }
            }
            "grid_destroy" => {
                let grid = unsigned(&args[0], "grid")?;
                ensure!(grid > 0, "grid id must be positive");
                // A defensive destroy for an untracked grid must not destroy grid 1.
                if grid == 1 {
                    self.staging.rows.clear();
                    self.staging.cols = 0;
                    self.staging.cursor = (0, 0);
                }
            }
            "grid_scroll" => self.scroll(args)?,
            "grid_cursor_goto" => {
                single_grid(&args[0])?;
                let row = index(&args[1], "cursor row")?;
                let col = index(&args[2], "cursor column")?;
                ensure!(
                    row < self.staging.rows.len() && col < self.staging.cols,
                    "cursor ({row}, {col}) is outside the grid"
                );
                self.staging.cursor = (row, col);
            }
            "default_colors_set" => {
                let fg = color(&args[0], "default foreground")?;
                let bg = color(&args[1], "default background")?;
                let sp = color(&args[2], "default special")?;
                signed(&args[3], "terminal foreground")?;
                signed(&args[4], "terminal background")?;
                // -1 is the protocol's unset sentinel. RGB attach normally sends
                // concrete defaults, but retaining the last color is safe here.
                if let Some(fg) = fg {
                    self.staging.default_fg = fg;
                }
                if let Some(bg) = bg {
                    self.staging.default_bg = bg;
                }
                if let Some(sp) = sp {
                    self.staging.default_sp = sp;
                }
            }
            "hl_attr_define" => {
                let id = unsigned(&args[0], "highlight id")?;
                let highlight = parse_highlight(&args[1])?;
                map(&args[2], "terminal attributes")?;
                array(&args[3], "highlight info")?;
                if self.staging.highlights.get(&id) != Some(&highlight) {
                    Arc::make_mut(&mut self.staging.highlights).insert(id, highlight);
                }
            }
            "mode_info_set" => {
                let enabled = boolean(&args[0], "cursor style enabled")?;
                let styles = array(&args[1], "mode info")?
                    .iter()
                    .map(parse_cursor_style)
                    .collect::<Result<Vec<_>>>()?;
                self.staging.cursor_style_enabled = enabled;
                if *self.staging.mode_styles != styles {
                    self.staging.mode_styles = Arc::new(styles);
                }
            }
            "mode_change" => {
                let mode = string(&args[0], "mode")?;
                let mode_index = index(&args[1], "mode index")?;
                if *self.staging.mode != *mode {
                    self.staging.mode = mode.into();
                }
                self.staging.mode_index = mode_index;
            }
            "option_set" => {
                let option = string(&args[0], "option name")?;
                if self.staging.options.get(option) != Some(&args[1]) {
                    Arc::make_mut(&mut self.staging.options)
                        .insert(option.to_owned(), args[1].clone());
                }
            }
            "busy_start" => self.staging.busy = true,
            "busy_stop" => self.staging.busy = false,
            "mouse_on" => self.staging.mouse_enabled = true,
            "mouse_off" => self.staging.mouse_enabled = false,
            "bell" | "visual_bell" => {
                self.staging.bell = self
                    .staging
                    .bell
                    .checked_add(1)
                    .context("bell counter exhausted")?;
            }
            "cmdline_show" => {
                let content = parse_chunks(&args[0])?;
                let pos = index(&args[1], "command-line byte position")?;
                validate_cmdline_pos(&content, pos)?;
                let firstc = string(&args[2], "command-line first character")?.to_owned();
                let prompt = if args[3] == Value::Boolean(false) {
                    String::new()
                } else {
                    string(&args[3], "command-line prompt")?.to_owned()
                };
                let indent = index(&args[4], "command-line indent")?;
                let level = unsigned(&args[5], "command-line level")?;
                Arc::make_mut(&mut self.staging.cmdline).insert(
                    level,
                    Cmdline {
                        content,
                        pos,
                        firstc,
                        prompt,
                        indent,
                        special: None,
                    },
                );
            }
            "cmdline_pos" => {
                let pos = index(&args[0], "command-line byte position")?;
                let level = unsigned(&args[1], "command-line level")?;
                let cmdline = Arc::make_mut(&mut self.staging.cmdline)
                    .get_mut(&level)
                    .with_context(|| format!("command-line level {level} is not visible"))?;
                validate_cmdline_pos(&cmdline.content, pos)?;
                cmdline.pos = pos;
            }
            "cmdline_special_char" => {
                let text = string(&args[0], "command-line special character")?.to_owned();
                let shift = boolean(&args[1], "command-line special shift")?;
                let level = unsigned(&args[2], "command-line level")?;
                Arc::make_mut(&mut self.staging.cmdline)
                    .get_mut(&level)
                    .with_context(|| format!("command-line level {level} is not visible"))?
                    .special = Some((text, shift));
            }
            "cmdline_hide" => {
                let level = unsigned(&args[0], "command-line level")?;
                if self.staging.cmdline.contains_key(&level) {
                    Arc::make_mut(&mut self.staging.cmdline).remove(&level);
                }
            }
            "cmdline_block_show" => {
                self.staging.cmdline_block = Arc::new(
                    array(&args[0], "command-line block")?
                        .iter()
                        .map(parse_chunks)
                        .collect::<Result<Vec<_>>>()?,
                );
            }
            "cmdline_block_append" => {
                let line = parse_chunks(&args[0])?;
                Arc::make_mut(&mut self.staging.cmdline_block).push(line);
            }
            "cmdline_block_hide" if !self.staging.cmdline_block.is_empty() => {
                self.staging.cmdline_block = Arc::default();
            }
            _ => {}
        }
        Ok(())
    }

    fn resize(&mut self, cols: usize, rows: usize) -> Result<()> {
        self.staging.rows.truncate(rows);
        if cols != self.staging.cols {
            for row in &mut self.staging.rows {
                let row = Arc::make_mut(row);
                if cols > row.len() {
                    row.try_reserve_exact(cols - row.len())
                        .context("allocating resized grid row")?;
                }
                row.resize_with(cols, Cell::default);
            }
        }
        if rows > self.staging.rows.len() {
            let blank = blank_row(cols)?;
            self.staging
                .rows
                .try_reserve_exact(rows - self.staging.rows.len())
                .context("allocating grid rows")?;
            self.staging.rows.resize(rows, blank);
        }
        self.staging.cols = cols;
        self.staging.cursor.0 = self.staging.cursor.0.min(rows - 1);
        self.staging.cursor.1 = self.staging.cursor.1.min(cols - 1);
        Ok(())
    }

    fn line(&mut self, args: &[Value]) -> Result<()> {
        single_grid(&args[0])?;
        let row = index(&args[1], "row")?;
        let mut col = index(&args[2], "start column")?;
        let cells = array(&args[3], "cells")?;
        boolean(&args[4], "wrap")?;
        ensure!(
            row < self.staging.rows.len(),
            "row {row} is outside the grid"
        );
        ensure!(
            col <= self.staging.cols,
            "start column {col} is outside the grid"
        );
        if cells.is_empty() {
            return Ok(());
        }
        let row = &mut self.staging.rows[row];
        let mut hl_id = None;
        for (cell_index, value) in cells.iter().enumerate() {
            let cell = array(value, "cell")?;
            let text = string(required(cell, 0, "cell text")?, "cell text")?;
            if let Some(id) = cell.get(1) {
                hl_id = Some(unsigned(id, "cell highlight id")?);
            }
            let hl_id = hl_id
                .with_context(|| format!("cell {cell_index} omits the initial highlight id"))?;
            let repeat = cell
                .get(2)
                .map(|value| index(value, "cell repeat"))
                .transpose()?
                .unwrap_or(1);
            let end = col
                .checked_add(repeat)
                .context("cell repeat overflows the column")?;
            ensure!(
                end <= self.staging.cols,
                "cell {cell_index} ends at column {end}, beyond width {}",
                self.staging.cols
            );
            // Repeated redraws of identical cells retain the published row's
            // identity, so downstream shape caches do not invalidate on a no-op.
            if row[col..end]
                .iter()
                .any(|destination| destination.text != text || destination.hl_id != hl_id)
            {
                let text = CellText::new(text);
                for destination in &mut Arc::make_mut(row)[col..end] {
                    destination.text.clone_from(&text);
                    destination.hl_id = hl_id;
                }
            }
            col = end;
        }
        Ok(())
    }

    fn scroll(&mut self, args: &[Value]) -> Result<()> {
        single_grid(&args[0])?;
        let top = index(&args[1], "scroll top")?;
        let bottom = index(&args[2], "scroll bottom")?;
        let left = index(&args[3], "scroll left")?;
        let right = index(&args[4], "scroll right")?;
        let rows = signed(&args[5], "scroll rows")?;
        let cols = signed(&args[6], "scroll columns")?;
        ensure!(
            !self.staging.rows.is_empty(),
            "scroll targets an absent grid"
        );
        ensure!(
            top <= bottom && bottom <= self.staging.rows.len(),
            "invalid scroll row rectangle"
        );
        ensure!(
            left <= right && right <= self.staging.cols,
            "invalid scroll column rectangle"
        );
        if rows == 0 && cols == 0 {
            return Ok(());
        }
        let Some((dst_row, src_row, height)) = scroll_overlap(top, bottom, rows) else {
            return Ok(());
        };
        let Some((dst_col, src_col, width)) = scroll_overlap(left, right, cols) else {
            return Ok(());
        };
        // Traverse away from source rows, so writes cannot clobber a later read.
        // Exposed cells are left intact: Neovim fills them with grid_line events.
        for step in 0..height {
            let offset = if rows < 0 { height - 1 - step } else { step };
            let destination = dst_row + offset;
            let source = src_row + offset;
            if left == 0 && right == self.staging.cols && cols == 0 {
                self.staging.rows[destination] = Arc::clone(&self.staging.rows[source]);
            } else if destination != source {
                let (destination, source) = if destination < source {
                    let (before, after) = self.staging.rows.split_at_mut(source);
                    (&mut before[destination], &after[0])
                } else {
                    let (before, after) = self.staging.rows.split_at_mut(destination);
                    (&mut after[0], &before[source])
                };
                Arc::make_mut(destination)[dst_col..dst_col + width]
                    .clone_from_slice(&source[src_col..src_col + width]);
            } else {
                let row = Arc::make_mut(&mut self.staging.rows[destination]);
                for step in 0..width {
                    let offset = if cols < 0 { width - 1 - step } else { step };
                    let destination = dst_col + offset;
                    let source = src_col + offset;
                    if destination < source {
                        let (before, after) = row.split_at_mut(source);
                        before[destination].clone_from(&after[0]);
                    } else {
                        let (before, after) = row.split_at_mut(destination);
                        after[0].clone_from(&before[source]);
                    }
                }
            }
        }
        Ok(())
    }
}

fn blank_row(cols: usize) -> Result<Arc<Vec<Cell>>> {
    let mut cells = Vec::new();
    cells
        .try_reserve_exact(cols)
        .context("allocating blank grid row")?;
    cells.resize_with(cols, Cell::default);
    Ok(Arc::new(cells))
}

/// Returns destination start, source start and overlapping length.
fn scroll_overlap(start: usize, end: usize, displacement: i64) -> Option<(usize, usize, usize)> {
    let distance = usize::try_from(displacement.unsigned_abs()).ok()?;
    let length = end - start;
    if distance >= length {
        return None;
    }
    if displacement >= 0 {
        Some((start, start + distance, length - distance))
    } else {
        Some((start + distance, start, length - distance))
    }
}

fn parse_highlight(value: &Value) -> Result<Highlight> {
    let mut highlight = Highlight::default();
    for (key, value) in map(value, "RGB attributes")? {
        match string(key, "highlight attribute name")? {
            "foreground" => highlight.foreground = color(value, "foreground")?,
            "background" => highlight.background = color(value, "background")?,
            "special" => highlight.special = color(value, "special")?,
            "reverse" => highlight.reverse = boolean(value, "reverse")?,
            "bold" => highlight.bold = boolean(value, "bold")?,
            "italic" => highlight.italic = boolean(value, "italic")?,
            "underline" => highlight.underline = boolean(value, "underline")?,
            "undercurl" => highlight.undercurl = boolean(value, "undercurl")?,
            "strikethrough" => highlight.strikethrough = boolean(value, "strikethrough")?,
            "blend" => highlight.blend = percentage(value, "blend")?,
            _ => {}
        }
    }
    Ok(highlight)
}

fn parse_cursor_style(value: &Value) -> Result<CursorStyle> {
    let mut style = CursorStyle::default();
    for (key, value) in map(value, "cursor style")? {
        match string(key, "cursor style property")? {
            "cursor_shape" => style.shape = string(value, "cursor shape")?.to_owned(),
            "cell_percentage" => style.cell_percentage = percentage(value, "cell percentage")?,
            "blinkwait" => style.blinkwait = unsigned(value, "blink wait")?,
            "blinkon" => style.blinkon = unsigned(value, "blink on")?,
            "blinkoff" => style.blinkoff = unsigned(value, "blink off")?,
            "attr_id" => style.attr_id = unsigned(value, "cursor attribute id")?,
            _ => {}
        }
    }
    Ok(style)
}

fn parse_chunks(value: &Value) -> Result<Vec<HighlightedChunk>> {
    array(value, "highlighted chunks")?
        .iter()
        .map(|value| {
            let chunk = array(value, "highlighted chunk")?;
            let hl_id = unsigned(
                required(chunk, 0, "chunk highlight id")?,
                "chunk highlight id",
            )?;
            let text = string(required(chunk, 1, "chunk text")?, "chunk text")?.to_owned();
            Ok(HighlightedChunk { hl_id, text })
        })
        .collect()
}

fn validate_cmdline_pos(content: &[HighlightedChunk], pos: usize) -> Result<()> {
    let mut remaining = pos;
    for chunk in content {
        if remaining <= chunk.text.len() {
            ensure!(
                chunk.text.is_char_boundary(remaining),
                "command-line position {pos} splits a UTF-8 character"
            );
            return Ok(());
        }
        remaining -= chunk.text.len();
    }
    ensure!(
        remaining == 0,
        "command-line position {pos} exceeds content length"
    );
    Ok(())
}

fn required<'a>(values: &'a [Value], index: usize, field: &str) -> Result<&'a Value> {
    values
        .get(index)
        .with_context(|| format!("missing {field}"))
}

fn array<'a>(value: &'a Value, field: &str) -> Result<&'a [Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .with_context(|| format!("{field} must be an array"))
}

fn map<'a>(value: &'a Value, field: &str) -> Result<&'a [(Value, Value)]> {
    value
        .as_map()
        .map(Vec::as_slice)
        .with_context(|| format!("{field} must be a map"))
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .as_str()
        .with_context(|| format!("{field} must be a valid UTF-8 string"))
}

fn unsigned(value: &Value, field: &str) -> Result<u64> {
    value
        .as_u64()
        .with_context(|| format!("{field} must be a nonnegative integer"))
}

fn signed(value: &Value, field: &str) -> Result<i64> {
    value
        .as_i64()
        .with_context(|| format!("{field} must be a signed integer"))
}

fn index(value: &Value, field: &str) -> Result<usize> {
    usize::try_from(unsigned(value, field)?)
        .with_context(|| format!("{field} exceeds addressable size"))
}

fn boolean(value: &Value, field: &str) -> Result<bool> {
    value
        .as_bool()
        .with_context(|| format!("{field} must be a boolean"))
}

fn color(value: &Value, field: &str) -> Result<Option<u32>> {
    match signed(value, field)? {
        -1 => Ok(None),
        rgb @ 0..=0xffffff => Ok(Some(rgb as u32)),
        _ => bail!("{field} must be a 24-bit RGB color or -1"),
    }
}

fn percentage(value: &Value, field: &str) -> Result<u8> {
    let value = unsigned(value, field)?;
    ensure!(value <= 100, "{field} must be in 0..=100");
    Ok(value as u8)
}

fn single_grid(value: &Value) -> Result<()> {
    let grid = unsigned(value, "grid")?;
    ensure!(
        grid == 1,
        "unsupported grid {grid}: ext_multigrid is disabled"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, args: Vec<Value>) -> Value {
        Value::Array(vec![name.into(), Value::Array(args)])
    }

    fn cells(values: Vec<Vec<Value>>) -> Value {
        Value::Array(values.into_iter().map(Value::Array).collect())
    }

    fn attributes(values: &[(&str, Value)]) -> Value {
        Value::Map(
            values
                .iter()
                .map(|(name, value)| ((*name).into(), value.clone()))
                .collect(),
        )
    }

    fn resize(cols: usize, rows: usize) -> Value {
        event(
            "grid_resize",
            vec![1.into(), (cols as u64).into(), (rows as u64).into()],
        )
    }

    fn line(row: usize, col: usize, content: Vec<Vec<Value>>) -> Value {
        event(
            "grid_line",
            vec![
                1.into(),
                (row as u64).into(),
                (col as u64).into(),
                cells(content),
                false.into(),
            ],
        )
    }

    fn highlight(id: u64, attrs: Value) -> Value {
        event(
            "hl_attr_define",
            vec![id.into(), attrs, Value::Map(vec![]), Value::Array(vec![])],
        )
    }

    fn flush() -> Value {
        event("flush", vec![])
    }

    fn chunks(values: &[(u64, &str)]) -> Value {
        cells(
            values
                .iter()
                .map(|(id, text)| vec![(*id).into(), (*text).into()])
                .collect(),
        )
    }

    fn cmdline(level: u64, pos: usize, content: &[(u64, &str)]) -> Value {
        event(
            "cmdline_show",
            vec![
                chunks(content),
                (pos as u64).into(),
                ":".into(),
                "".into(),
                0.into(),
                level.into(),
            ],
        )
    }

    #[test]
    fn cell_text_round_trips_graphemes_inline_or_shared() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
        let boundary = "界".repeat(7) + "x";
        let beyond = boundary.clone() + "y";
        let long = "e\u{301}".repeat(40);
        for text in [
            "",
            " ",
            "a",
            "界",
            "e\u{301}",
            "a\u{301}\u{302}\u{303}",
            "🙂",
            "\u{1f44d}\u{1f3fd}",
            family,
            &boundary,
            &beyond,
            &long,
        ] {
            let cell = CellText::new(text);
            assert_eq!(cell.as_str(), text);
            assert_eq!(&*cell, text);
            assert_eq!(cell.as_bytes(), text.as_bytes());
            assert_eq!(cell.len(), text.len());
            assert_eq!(cell.is_empty(), text.is_empty());
            assert_eq!(cell, text);
            assert_eq!(cell, *text);
            assert_eq!(cell, CellText::from(text.to_owned()));
            assert_eq!(cell.clone(), cell);
            assert_eq!(format!("{cell:?}"), format!("{text:?}"));
            assert_eq!(cell.to_string(), text);
            assert_eq!(cell.is_heap(), text.len() > INLINE_CAPACITY, "{text:?}");
        }
        assert_eq!(boundary.len(), INLINE_CAPACITY);
        assert_ne!(CellText::new("a"), CellText::new("b"));
        assert_ne!(CellText::new(""), CellText::BLANK);
        assert_eq!(CellText::BLANK, " ");
        let shared = CellText::new(&long);
        let CellTextRepr::Heap(original) = &shared.0 else {
            unreachable!("long text is shared")
        };
        let CellTextRepr::Heap(copy) = &shared.clone().0 else {
            unreachable!("long text is shared")
        };
        assert!(Arc::ptr_eq(original, copy), "cloning shares long clusters");
        // A cell is no larger than the former `String` plus highlight id.
        assert!(std::mem::size_of::<Cell>() <= std::mem::size_of::<(String, u64)>());
    }

    #[test]
    fn blank_and_redrawn_rows_keep_cells_inline_and_unchanged_rows_shared() -> Result<()> {
        let mut reducer = GridReducer::new(Uuid::nil());
        reducer.apply(&[resize(80, 24), flush()])?;
        let blank = reducer.snapshot();
        assert!(
            blank
                .rows
                .iter()
                .all(|row| Arc::ptr_eq(row, &blank.rows[0]))
        );
        assert!(blank.rows[0].iter().all(|cell| *cell == Cell::default()));
        reducer.apply(&[
            line(
                3,
                0,
                vec![
                    vec!["界".into(), 1.into()],
                    vec!["".into()],
                    vec!["e\u{301}".into(), 2.into(), 10.into()],
                ],
            ),
            flush(),
        ])?;
        let drawn = reducer.snapshot();
        assert!(!Arc::ptr_eq(&drawn.rows[3], &blank.rows[3]));
        for row in (0..24).filter(|row| *row != 3) {
            assert!(Arc::ptr_eq(&drawn.rows[row], &blank.rows[row]));
        }
        assert!(drawn.rows[3].iter().all(|cell| !cell.text.is_heap()));
        assert_eq!(drawn.rows[3][2].text, "e\u{301}");
        assert_eq!(drawn.rows[3][12].text, " ");
        assert!(blank.rows[3].iter().all(|cell| *cell == Cell::default()));
        Ok(())
    }

    #[test]
    fn flush_shares_unchanged_editor_state_with_the_previous_snapshot() -> Result<()> {
        let mut reducer = GridReducer::new(Uuid::nil());
        let state = || {
            vec![
                event("option_set", vec!["guifont".into(), "Menlo:h13".into()]),
                event(
                    "mode_info_set",
                    vec![
                        true.into(),
                        Value::Array(vec![attributes(&[("cursor_shape", "block".into())])]),
                    ],
                ),
                event("mode_change", vec!["insert".into(), 0.into()]),
            ]
        };
        let mut events = vec![resize(4, 2)];
        events.extend(state());
        events.extend([
            highlight(7, attributes(&[("bold", true.into())])),
            cmdline(1, 0, &[(0, "w")]),
            event(
                "cmdline_block_show",
                vec![Value::Array(vec![chunks(&[(3, "if 1")])])],
            ),
            flush(),
        ]);
        reducer.apply(&events)?;
        let first = reducer.snapshot();
        let mut events = state();
        events.extend([
            event("grid_cursor_goto", vec![1.into(), 1.into(), 2.into()]),
            flush(),
        ]);
        reducer.apply(&events)?;
        let second = reducer.snapshot();
        assert_eq!(second.cursor, (1, 2));
        assert!(Arc::ptr_eq(&first.options, &second.options));
        assert!(Arc::ptr_eq(&first.mode_styles, &second.mode_styles));
        assert!(Arc::ptr_eq(&first.mode, &second.mode));
        assert!(Arc::ptr_eq(&first.cmdline, &second.cmdline));
        assert!(Arc::ptr_eq(&first.cmdline_block, &second.cmdline_block));
        assert!(Arc::ptr_eq(&first.highlights, &second.highlights));
        reducer.apply(&[
            event("option_set", vec!["guifont".into(), "Iosevka:h14".into()]),
            event("cmdline_pos", vec![1.into(), 1.into()]),
            flush(),
        ])?;
        let third = reducer.snapshot();
        assert!(!Arc::ptr_eq(&second.options, &third.options));
        assert!(!Arc::ptr_eq(&second.cmdline, &third.cmdline));
        assert!(Arc::ptr_eq(&second.mode_styles, &third.mode_styles));
        assert!(Arc::ptr_eq(&second.cmdline_block, &third.cmdline_block));
        assert_eq!(second.options["guifont"], Value::from("Menlo:h13"));
        assert_eq!(third.options["guifont"], Value::from("Iosevka:h14"));
        assert_eq!((second.cmdline[&1].pos, third.cmdline[&1].pos), (0, 1));
        reducer.apply(&[
            event("cmdline_hide", vec![2.into()]),
            event("cmdline_block_hide", vec![]),
            flush(),
        ])?;
        let hidden = reducer.snapshot();
        assert!(Arc::ptr_eq(&third.cmdline, &hidden.cmdline));
        assert!(hidden.cmdline_block.is_empty());
        assert_eq!(third.cmdline_block.len(), 1);
        Ok(())
    }

    #[test]
    fn zero_repeat_preserves_cells_and_updates_highlight_inheritance() -> Result<()> {
        let mut reducer = GridReducer::new(Uuid::nil());
        reducer.apply(&[
            resize(3, 1),
            line(0, 0, vec![vec!["a".into(), 1.into(), 3.into()]]),
            line(
                0,
                1,
                vec![vec!["ignored".into(), 7.into(), 0.into()], vec!["b".into()]],
            ),
            flush(),
        ])?;
        let snapshot = reducer.snapshot();
        assert_eq!(snapshot.rows[0][0].text, "a");
        assert_eq!(snapshot.rows[0][1].text, "b");
        assert_eq!(snapshot.rows[0][1].hl_id, 7);
        assert_eq!(snapshot.rows[0][2].text, "a");
        Ok(())
    }

    #[test]
    fn grid_lines_preserve_wide_continuations_combining_text_and_inherited_repeats() -> Result<()> {
        let mut reducer = GridReducer::new(Uuid::nil());
        let mut lines = line(
            0,
            0,
            vec![
                vec!["界".into(), 7.into()],
                vec!["".into()],
                vec!["e\u{301}".into()],
                vec!["x".into(), 9.into(), 2.into()],
                vec!["!".into()],
            ],
        );
        // Multiple tuples of one event are independent: inheritance starts anew.
        let Value::Array(tuples) = &mut lines else {
            unreachable!()
        };
        tuples.push(Value::Array(vec![
            1.into(),
            1.into(),
            0.into(),
            cells(vec![
                vec!["🙂".into(), 0.into()],
                vec!["".into()],
                vec![" ".into(), 0.into(), 4.into()],
            ]),
            false.into(),
        ]));
        reducer.apply(&[
            resize(6, 2),
            highlight(
                7,
                attributes(&[
                    ("foreground", 0x112233.into()),
                    ("background", 0x445566.into()),
                    ("special", 0x778899.into()),
                    ("reverse", true.into()),
                    ("bold", true.into()),
                    ("italic", true.into()),
                    ("underline", true.into()),
                    ("undercurl", true.into()),
                    ("strikethrough", true.into()),
                    ("blend", 25.into()),
                    ("future_attribute", Value::Nil),
                ]),
            ),
            lines,
            flush(),
        ])?;
        let snapshot = reducer.snapshot();
        assert_eq!(
            snapshot.rows[0].as_slice(),
            &[
                Cell {
                    text: "界".into(),
                    hl_id: 7
                },
                Cell {
                    text: "".into(),
                    hl_id: 7
                },
                Cell {
                    text: "e\u{301}".into(),
                    hl_id: 7
                },
                Cell {
                    text: "x".into(),
                    hl_id: 9
                },
                Cell {
                    text: "x".into(),
                    hl_id: 9
                },
                Cell {
                    text: "!".into(),
                    hl_id: 9
                },
            ]
        );
        assert_eq!(
            snapshot.rows[1][0],
            Cell {
                text: "🙂".into(),
                hl_id: 0
            }
        );
        assert_eq!(
            snapshot.rows[1][1],
            Cell {
                text: "".into(),
                hl_id: 0
            }
        );
        assert_eq!(snapshot.rows[1][5], Cell::default());
        assert_eq!(
            snapshot.highlights[&7],
            Highlight {
                foreground: Some(0x112233),
                background: Some(0x445566),
                special: Some(0x778899),
                reverse: true,
                bold: true,
                italic: true,
                underline: true,
                undercurl: true,
                strikethrough: true,
                blend: 25,
            }
        );
        Ok(())
    }

    #[test]
    fn row_identity_survives_noop_redraws_but_changes_with_cell_content() -> Result<()> {
        let mut reducer = GridReducer::new(Uuid::nil());
        reducer.apply(&[
            resize(4, 2),
            line(
                0,
                0,
                vec![
                    vec!["界".into(), 7.into()],
                    vec!["".into()],
                    vec!["e\u{301}".into()],
                ],
            ),
            highlight(7, attributes(&[("bold", true.into())])),
            flush(),
        ])?;
        let original = reducer.snapshot();
        reducer.apply(&[
            line(
                0,
                0,
                vec![
                    vec!["界".into(), 7.into()],
                    vec!["".into()],
                    vec!["e\u{301}".into()],
                ],
            ),
            highlight(7, attributes(&[("bold", true.into())])),
            event("grid_cursor_goto", vec![1.into(), 1.into(), 2.into()]),
            flush(),
        ])?;
        let cursor = reducer.snapshot();
        assert_eq!(cursor.cursor, (1, 2));
        assert!(Arc::ptr_eq(&original.rows[0], &cursor.rows[0]));
        assert!(Arc::ptr_eq(&original.rows[1], &cursor.rows[1]));
        assert!(Arc::ptr_eq(&original.highlights, &cursor.highlights));
        reducer.apply(&[
            line(0, 2, vec![vec!["changed".into(), 7.into()]]),
            highlight(7, attributes(&[("italic", true.into())])),
            flush(),
        ])?;
        let changed = reducer.snapshot();
        assert!(!Arc::ptr_eq(&cursor.rows[0], &changed.rows[0]));
        assert!(Arc::ptr_eq(&cursor.rows[1], &changed.rows[1]));
        assert!(!Arc::ptr_eq(&cursor.highlights, &changed.highlights));
        assert_eq!(original.rows[0][2].text, "e\u{301}");
        assert_eq!(changed.rows[0][2].text, "changed");
        Ok(())
    }

    #[test]
    fn only_flush_publishes_and_old_rows_and_highlights_remain_immutable() -> Result<()> {
        let generation = Uuid::new_v4();
        let mut reducer = GridReducer::new(generation);
        let empty = reducer.snapshot();
        assert!(
            reducer
                .apply(&[
                    resize(3, 2),
                    line(0, 0, vec![vec!["a".into(), 7.into(), 3.into()]]),
                    highlight(7, attributes(&[("bold", true.into())])),
                ])?
                .is_empty()
        );
        assert_eq!(reducer.snapshot().serial, 0);
        assert!(reducer.snapshot().rows.is_empty());
        let first = reducer.apply(&[flush()])?.remove(0);
        assert_eq!(first.generation, generation);
        assert_eq!(first.serial, 1);
        reducer.apply(&[
            line(0, 0, vec![vec!["b".into(), 7.into()]]),
            highlight(7, attributes(&[("italic", true.into())])),
        ])?;
        assert_eq!(reducer.snapshot().rows[0][0].text, "a");
        assert!(reducer.snapshot().highlights[&7].bold);
        let publications = reducer.apply(&[
            flush(),
            line(1, 0, vec![vec!["c".into(), 0.into()]]),
            flush(),
        ])?;
        assert_eq!(publications.len(), 2);
        assert_eq!(publications[0].serial, 2);
        assert_eq!(publications[1].serial, 3);
        assert_eq!(publications[0].rows[1][0].text, " ");
        assert_eq!(publications[1].rows[1][0].text, "c");
        assert_eq!(publications[0].rows[0][0].text, "b");
        assert!(publications[0].highlights[&7].italic);
        assert!(!publications[0].highlights[&7].bold);
        assert_eq!(first.rows[0][0].text, "a");
        assert!(first.highlights[&7].bold);
        assert!(!first.highlights[&7].italic);
        assert_eq!(empty.cols, 0);
        Ok(())
    }

    #[test]
    fn scrolling_copies_overlapping_rectangles_in_both_axes_without_touching_outside() -> Result<()>
    {
        // Each sign changes the safe traversal direction. Full-width cases also
        // exercise row sharing followed by a mutation of one formerly shared row.
        for (top, bottom, left, right, dy, dx) in [
            (0, 5, 0, 6, 1_i64, 0_i64),
            (0, 5, 0, 6, -1, 0),
            (1, 4, 1, 5, 0, 1),
            (1, 4, 1, 5, 0, -1),
            (1, 4, 1, 5, 1, 1),
            (1, 4, 1, 5, -1, -1),
            (1, 4, 1, 5, 1, -1),
            (1, 4, 1, 5, -1, 1),
        ] {
            let mut reducer = GridReducer::new(Uuid::nil());
            let mut initial = vec![resize(6, 5)];
            for row in 0..5 {
                initial.push(line(
                    row,
                    0,
                    (0..6)
                        .map(|col| {
                            vec![
                                char::from(b'A' + (row * 6 + col) as u8).to_string().into(),
                                ((row * 6 + col) as u64).into(),
                            ]
                        })
                        .collect(),
                ));
            }
            initial.push(flush());
            reducer.apply(&initial)?;
            let original = reducer.snapshot();
            reducer.apply(&[
                event(
                    "grid_scroll",
                    vec![
                        1.into(),
                        top.into(),
                        bottom.into(),
                        left.into(),
                        right.into(),
                        dy.into(),
                        dx.into(),
                    ],
                ),
                flush(),
            ])?;
            let scrolled = reducer.snapshot();
            for row in 0..5 {
                for col in 0..6 {
                    let source_row = row as i64 + dy;
                    let source_col = col as i64 + dx;
                    let copied = (top as usize..bottom as usize).contains(&row)
                        && (left as usize..right as usize).contains(&col)
                        && (i64::from(top)..i64::from(bottom)).contains(&source_row)
                        && (i64::from(left)..i64::from(right)).contains(&source_col);
                    let expected = if copied {
                        &original.rows[source_row as usize][source_col as usize]
                    } else {
                        &original.rows[row][col]
                    };
                    assert_eq!(
                        &scrolled.rows[row][col], expected,
                        "rectangle ({top}, {bottom}, {left}, {right}), shift ({dy}, {dx}), cell ({row}, {col})"
                    );
                }
            }
            reducer.apply(&[line(2, 2, vec![vec!["*".into(), 0.into()]]), flush()])?;
            assert_eq!(original.rows[2][2].text, "O");
            assert_ne!(scrolled.rows[2][2].text, "*");
            assert_eq!(reducer.snapshot().rows[2][2].text, "*");
        }
        Ok(())
    }

    #[test]
    fn resize_clear_destroy_and_oversized_scroll_keep_the_grid_consistent() -> Result<()> {
        let mut reducer = GridReducer::new(Uuid::nil());
        reducer.apply(&[
            resize(3, 2),
            line(1, 2, vec![vec!["z".into(), 4.into()]]),
            event("grid_cursor_goto", vec![1.into(), 1.into(), 2.into()]),
            flush(),
        ])?;
        let old = reducer.snapshot();
        reducer.apply(&[resize(2, 1), flush()])?;
        assert_eq!(reducer.snapshot().cursor, (0, 1));
        assert_eq!(reducer.snapshot().rows[0].len(), 2);
        reducer.apply(&[
            resize(4, 3),
            line(1, 2, vec![vec!["q".into(), 0.into()]]),
            flush(),
        ])?;
        let before_scroll = reducer.snapshot();
        reducer.apply(&[
            event(
                "grid_scroll",
                vec![
                    1.into(),
                    0.into(),
                    3.into(),
                    0.into(),
                    4.into(),
                    i64::MIN.into(),
                    i64::MAX.into(),
                ],
            ),
            flush(),
        ])?;
        assert_eq!(reducer.snapshot().rows, before_scroll.rows);
        reducer.apply(&[event("grid_clear", vec![1.into()]), flush()])?;
        assert!(
            reducer
                .snapshot()
                .rows
                .iter()
                .all(|row| row.iter().all(|cell| *cell == Cell::default()))
        );
        assert_eq!(old.rows[1][2].text, "z");
        reducer.apply(&[event("grid_destroy", vec![2.into()]), flush()])?;
        assert_eq!(reducer.snapshot().cols, 4);
        reducer.apply(&[event("grid_destroy", vec![1.into()]), flush()])?;
        assert_eq!(reducer.snapshot().cols, 0);
        assert!(reducer.snapshot().rows.is_empty());
        reducer.apply(&[resize(1, 1), flush()])?;
        assert_eq!(reducer.snapshot().rows[0][0], Cell::default());
        Ok(())
    }

    #[test]
    fn global_state_and_mode_specific_cursor_changes_wait_for_flush() -> Result<()> {
        let mut reducer = GridReducer::new(Uuid::nil());
        let styles = Value::Array(vec![
            attributes(&[("cursor_shape", "block".into())]),
            attributes(&[
                ("cursor_shape", "vertical".into()),
                ("cell_percentage", 20.into()),
                ("blinkwait", 700.into()),
                ("blinkon", 400.into()),
                ("blinkoff", 250.into()),
                ("attr_id", 12.into()),
                ("name", "insert".into()),
            ]),
        ]);
        reducer.apply(&[
            event("mode_info_set", vec![true.into(), styles]),
            event("mode_change", vec!["insert".into(), 1.into()]),
            event(
                "default_colors_set",
                vec![
                    0x123456.into(),
                    0x654321.into(),
                    0xabcdef.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            highlight(12, attributes(&[("reverse", true.into())])),
            event("option_set", vec!["guifont".into(), "Menlo:h13".into()]),
            event("busy_start", vec![]),
            event("mouse_on", vec![]),
            Value::Array(vec![
                "bell".into(),
                Value::Array(vec![]),
                Value::Array(vec![]),
            ]),
            event("visual_bell", vec![]),
        ])?;
        assert!(!reducer.snapshot().busy);
        assert_eq!(reducer.snapshot().bell, 0);
        reducer.apply(&[flush()])?;
        let snapshot = reducer.snapshot();
        assert!(snapshot.busy && snapshot.mouse_enabled && snapshot.cursor_style_enabled);
        assert_eq!(snapshot.bell, 3);
        assert_eq!(&*snapshot.mode, "insert");
        assert_eq!(
            snapshot.mode_styles[snapshot.mode_index],
            CursorStyle {
                shape: "vertical".into(),
                cell_percentage: 20,
                blinkwait: 700,
                blinkon: 400,
                blinkoff: 250,
                attr_id: 12,
            }
        );
        assert_eq!(
            (
                snapshot.default_fg,
                snapshot.default_bg,
                snapshot.default_sp
            ),
            (0x123456, 0x654321, 0xabcdef)
        );
        assert_eq!(snapshot.highlights[&12].foreground, None);
        assert_eq!(snapshot.options["guifont"], Value::from("Menlo:h13"));
        reducer.apply(&[
            event("busy_stop", vec![]),
            event("mouse_off", vec![]),
            flush(),
        ])?;
        assert!(!reducer.snapshot().busy);
        assert!(!reducer.snapshot().mouse_enabled);
        Ok(())
    }

    #[test]
    fn nested_command_lines_preserve_highlighted_bytes_positions_specials_and_blocks() -> Result<()>
    {
        let mut reducer = GridReducer::new(Uuid::nil());
        reducer.apply(&[
            cmdline(1, "界".len(), &[(7, "界"), (8, "e\u{301}🙂")]),
            event(
                "cmdline_special_char",
                vec!["^".into(), true.into(), 1.into()],
            ),
            cmdline(2, "🙂".len(), &[(9, "🙂"), (10, "+1")]),
            event("cmdline_pos", vec![5.into(), 2.into()]),
            event(
                "cmdline_block_show",
                vec![Value::Array(vec![chunks(&[(3, "function 界()")])])],
            ),
            event(
                "cmdline_block_append",
                vec![chunks(&[(4, "  echo "), (5, "'🙂'")])],
            ),
            flush(),
        ])?;
        let nested = reducer.snapshot();
        assert_eq!(
            nested.cmdline.keys().copied().collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(nested.cmdline[&1].pos, 3);
        assert_eq!(
            nested.cmdline[&1].content,
            vec![
                HighlightedChunk {
                    hl_id: 7,
                    text: "界".into()
                },
                HighlightedChunk {
                    hl_id: 8,
                    text: "e\u{301}🙂".into()
                },
            ]
        );
        assert_eq!(nested.cmdline[&1].special, Some(("^".into(), true)));
        assert_eq!(nested.cmdline[&2].pos, 5);
        assert_eq!(
            *nested.cmdline_block,
            vec![
                vec![HighlightedChunk {
                    hl_id: 3,
                    text: "function 界()".into()
                }],
                vec![
                    HighlightedChunk {
                        hl_id: 4,
                        text: "  echo ".into()
                    },
                    HighlightedChunk {
                        hl_id: 5,
                        text: "'🙂'".into()
                    }
                ],
            ]
        );
        reducer.apply(&[
            event("cmdline_hide", vec![2.into(), true.into()]),
            cmdline(1, "界e\u{301}".len(), &[(7, "界"), (8, "e\u{301}🙂")]),
            event("cmdline_block_hide", vec![]),
            flush(),
        ])?;
        let restored = reducer.snapshot();
        assert_eq!(
            restored.cmdline.keys().copied().collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(restored.cmdline[&1].pos, 6);
        assert_eq!(restored.cmdline[&1].special, None);
        assert!(restored.cmdline_block.is_empty());
        assert!(nested.cmdline.contains_key(&2));
        reducer.apply(&[
            event(
                "cmdline_show",
                vec![
                    chunks(&[(0, "x")]),
                    1.into(),
                    ":".into(),
                    false.into(),
                    2.into(),
                    1.into(),
                    (-1).into(),
                    Value::Nil,
                ],
            ),
            flush(),
        ])?;
        assert_eq!(reducer.snapshot().cmdline[&1].prompt, "");
        assert_eq!(reducer.snapshot().cmdline[&1].indent, 2);
        Ok(())
    }

    #[test]
    fn malformed_known_events_are_refused_without_publishing() -> Result<()> {
        let malformed = [
            Value::Array(vec!["flush".into()]),
            Value::Array(vec!["busy_start".into(), Value::Nil]),
            event("grid_resize", vec![1.into(), 0.into(), 2.into()]),
            event("grid_resize", vec![1.into(), u64::MAX.into(), 2.into()]),
            event("grid_resize", vec![2.into(), 2.into(), 2.into()]),
            line(0, 0, vec![vec!["missing highlight".into()]]),
            line(0, 1, vec![vec!["a".into(), 0.into(), 2.into()]]),
            line(2, 0, vec![vec!["a".into(), 0.into()]]),
            line(0, 0, vec![vec!["a".into(), Value::Nil]]),
            event(
                "grid_line",
                vec![1.into(), 0.into(), 0.into(), cells(vec![]), Value::Nil],
            ),
            event("grid_cursor_goto", vec![1.into(), (-1).into(), 0.into()]),
            event("grid_cursor_goto", vec![1.into(), 0.into(), 2.into()]),
            event(
                "grid_scroll",
                vec![
                    1.into(),
                    0.into(),
                    3.into(),
                    0.into(),
                    2.into(),
                    1.into(),
                    0.into(),
                ],
            ),
            event(
                "grid_scroll",
                vec![
                    1.into(),
                    0.into(),
                    2.into(),
                    2.into(),
                    1.into(),
                    1.into(),
                    0.into(),
                ],
            ),
            highlight(7, attributes(&[("bold", 1.into())])),
            highlight(7, attributes(&[("foreground", 0x1000000.into())])),
            highlight(7, attributes(&[("blend", 101.into())])),
            event(
                "mode_info_set",
                vec![
                    true.into(),
                    Value::Array(vec![attributes(&[("blinkon", (-1).into())])]),
                ],
            ),
            event("mode_change", vec!["insert".into()]),
            cmdline(1, 1, &[(0, "界")]),
            cmdline(1, 4, &[(0, "界")]),
            event("cmdline_pos", vec![0.into(), 9.into()]),
            event("cmdline_block_append", vec![cells(vec![vec![0.into()]])]),
        ];
        for bad in malformed {
            let mut reducer = GridReducer::new(Uuid::nil());
            reducer.apply(&[
                resize(2, 2),
                line(0, 0, vec![vec!["k".into(), 0.into()]]),
                flush(),
            ])?;
            let before = reducer.snapshot();
            reducer
                .apply(std::slice::from_ref(&bad))
                .expect_err("malformed redraw must fail");
            let after = reducer.snapshot();
            assert_eq!(
                after.serial, before.serial,
                "published malformed event {bad:?}"
            );
            assert_eq!(
                after.rows, before.rows,
                "changed committed rows for {bad:?}"
            );
        }
        // Forward-compatible event and tuple extensions are not malformed.
        let mut reducer = GridReducer::new(Uuid::nil());
        reducer.apply(&[
            Value::Array(vec!["future_event".into(), Value::Nil]),
            event(
                "grid_resize",
                vec![1.into(), 1.into(), 1.into(), Value::Nil],
            ),
            line(0, 0, vec![vec!["x".into(), 0.into(), 1.into(), Value::Nil]]),
            event("flush", vec![Value::Nil]),
        ])?;
        assert_eq!(reducer.snapshot().rows[0][0].text, "x");
        Ok(())
    }
}
