use super::{super::commands::PasteImage, NvimView};
use crate::neovim::{ClipboardContent, NvimCommand};
use gpui_kit::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClipboardChord {
    Copy,
    Cut,
    Paste,
}

pub(super) fn clipboard_chord(key: &Keystroke) -> Option<ClipboardChord> {
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

impl NvimView {
    pub(super) fn paste_item(
        &mut self,
        item: ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if clipboard_has_image(&item) {
            window.dispatch_action(Box::new(PasteImage), cx);
        } else if let Some(text) = item.text() {
            self.send(NvimCommand::Paste(text), cx);
        }
    }
}

pub(super) fn clipboard_has_image(item: &ClipboardItem) -> bool {
    item.entries()
        .iter()
        .any(|entry| matches!(entry, ClipboardEntry::Image(_)))
}

pub(super) fn clipboard_content(item: &ClipboardItem) -> Option<ClipboardContent> {
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

#[cfg(test)]
mod tests {
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
        assert_eq!(chord("ctrl-v"), None);
        assert_eq!(chord("ctrl-c"), None);
        assert_eq!(chord("v"), None);
    }
}
