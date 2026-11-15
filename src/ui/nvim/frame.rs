use super::{
    offsets::byte_floor,
    shaping::{PaintedText, RowShape, colors, text_run},
};
use crate::neovim::grid::{GridSnapshot, HighlightedChunk};
use gpui_kit::*;
use std::{ops::Range, sync::Arc};

pub(super) struct Frame {
    pub(super) overlays: Vec<PaintedText>,
    pub(super) texts: Vec<PaintedText>,
    pub(super) rows: Vec<Arc<RowShape>>,
    pub(super) cmd_cursor: Option<Point<Pixels>>,
    pub(super) cmd_layout: Option<(ShapedLine, Point<Pixels>, usize)>,
}

pub(super) fn shape_chunks(
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

pub(super) fn make_frame(
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

pub(super) fn chunk_range(
    chunks: &[HighlightedChunk],
    range: Range<usize>,
) -> Vec<HighlightedChunk> {
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
