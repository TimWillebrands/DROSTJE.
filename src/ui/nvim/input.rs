//! Keyboard and mouse input translated into Neovim key notation and mouse events.
use super::{
    NvimView,
    clipboard::{ClipboardChord, clipboard_chord},
};
use crate::neovim::NvimCommand;
use ghostty_terminal::input_surface::native_text_key;
use gpui_kit::*;
use std::time::Instant;

impl NvimView {
    pub(super) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn mouse(
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

    pub(super) fn screen_cell(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
    ) -> (u32, u32) {
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

    pub(super) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus(window, cx);
        self.dragging = Some(event.button);
        self.mouse(event.button, "press", event.position, event.modifiers, cx);
    }

    pub(super) fn mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dragging.take().is_some() {
            self.mouse(event.button, "release", event.position, event.modifiers, cx);
        }
    }

    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(button) = self.dragging {
            self.mouse(button, "drag", event.position, event.modifiers, cx);
        }
    }

    pub(super) fn scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
}

pub(super) fn key_notation(key: &Keystroke) -> Option<String> {
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

pub(super) fn mouse_modifiers(modifiers: Modifiers) -> String {
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
