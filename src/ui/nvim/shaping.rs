//! Shaping grid rows: each cell's grapheme is shaped on its own and compatible cells are
//! batched into one layout, so glyphs stay on Neovim's columns.
use crate::neovim::grid::{Cell, GridSnapshot, Highlight};
use gpui_kit::*;

pub(super) struct PaintedText {
    pub(super) origin: Point<Pixels>,
    pub(super) line: ShapedLine,
}

/// Each cell is shaped independently. Only compatible, undecorated layouts are
/// combined: glyph positions stay relative to Neovim columns, not text advances.
pub(super) struct GridText {
    pub(super) origin: Point<Pixels>,
    pub(super) layout: LineLayout,
    pub(super) decorations: Vec<DecorationRun>,
}

pub(super) enum RowText {
    Batched(GridText),
    Decorated(usize),
}

#[derive(Default)]
pub(super) struct RowShape {
    pub(super) texts: Vec<RowText>,
    // Indexed in paint order: avoid a large common variant or a box per cell.
    pub(super) decorated: Vec<PaintedText>,
    pub(super) backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
}

pub(super) fn colors(grid: &GridSnapshot, id: u64) -> (Hsla, Hsla, Hsla) {
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

pub(super) fn text_run(grid: &GridSnapshot, font: &Font, id: u64, len: usize) -> TextRun {
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

impl GridText {
    pub(super) fn append(&mut self, origin: Point<Pixels>, line: &LineLayout, color: Hsla) {
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
    pub(super) fn push_text(&mut self, origin: Point<Pixels>, line: ShapedLine, run: TextRun) {
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

pub(super) fn shape_row(
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
            value.text.as_str().into(),
            font_size,
            std::slice::from_ref(&run),
            None,
        );
        shape.push_text(origin, line, run);
    }
    shape
}

#[cfg(all(test, feature = "runtime-qa"))]
mod tests {
    use gpui_kit::TestAppContext;

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
                    value.text.as_str().into(),
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
}
