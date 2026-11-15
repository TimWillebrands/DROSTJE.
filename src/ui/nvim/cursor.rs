use super::{
    NvimView,
    shaping::{colors, text_run},
};
use gpui_kit::*;

impl NvimView {
    pub(super) fn cursor_origin(&self) -> Point<Pixels> {
        if let Some(cursor) = self.frame.as_ref().and_then(|frame| frame.cmd_cursor) {
            return cursor;
        }
        point(
            self.cell.width * self.grid.cursor.1 as f32,
            self.cell.height * self.grid.cursor.0 as f32,
        )
    }

    pub(super) fn arm_blink(&mut self, window: &Window, cx: &mut Context<Self>) {
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

    pub(super) fn paint_cursor(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
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
                cell.text.as_str().into(),
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
