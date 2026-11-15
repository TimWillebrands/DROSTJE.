use super::{
    NvimView,
    frame::make_frame,
    offsets::utf16_byte,
    row_cache::RowCacheKey,
    shaping::{RowText, shape_row, text_run},
};
use crate::neovim::{NvimCommand, NvimTheme};
use gpui_kit::{
    component::{ActiveTheme, Theme},
    *,
};
use std::sync::Arc;

impl NvimView {
    pub(super) fn prepare(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            let visible_cols = RowCacheKey::visible_cols(bounds.size, self.cell);
            let key = RowCacheKey {
                font: self.font.clone(),
                font_size,
                cell: self.cell,
                visible_cols,
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
            let row_count = self
                .grid
                .rows
                .len()
                .min((bounds.size.height / self.cell.height).ceil().max(0.) as usize);
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

    pub(super) fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
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
}

pub(super) fn nvim_theme(theme: &Theme) -> NvimTheme {
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
