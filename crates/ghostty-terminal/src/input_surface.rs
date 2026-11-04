//! Keyboard ownership for embedded native input surfaces.
//!
//! NativeSurface opts out of workspace Vim/list/chord navigation, not explicit
//! app actions. Cmd-Escape leaves the surface; plain Escape belongs to it.
//! Printable text (including shifted and Option text) and IME commits must enter
//! through EntityInputHandler exactly once, never by replaying a raw key. Each
//! backend decides how committed text is encoded, separately from clipboard paste.

use gpui_kit::{KeyContext, Keystroke};

/// Add the surface's own action context while sharing native input ownership.
/// Additional surface-specific contexts (such as CaretPopup) may be added by its view.
pub fn native_input_context(surface: &'static str) -> KeyContext {
    let mut context = KeyContext::default();
    context.add("NativeInput");
    context.add("NativeSurface");
    context.add(surface);
    context
}

/// Leave native text/dead keys to the OS input handler. Return and Tab may also
/// carry key_char, but remain protocol keys rather than text commits.
pub fn native_text_key(key: &Keystroke) -> bool {
    key.key_char
        .as_deref()
        .is_some_and(|text| text.chars().all(|character| !character.is_control()))
        && !key.modifiers.control
        && !key.modifiers.platform
}

#[cfg(test)]
mod tests {
    use super::native_text_key;
    use gpui_kit::{Keystroke, Modifiers};

    #[test]
    fn shifted_option_and_dead_keys_remain_native_text() {
        for (key, text, modifiers) in [
            (":", ":", Modifiers::default()),
            (
                "g",
                "G",
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
            ),
            (
                "e",
                "é",
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
            ),
            (
                "e",
                "",
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
            ),
        ] {
            assert!(native_text_key(&Keystroke {
                key: key.into(),
                key_char: Some(text.into()),
                modifiers,
            }));
        }
        for key in ["enter", "tab", "escape", "ctrl-w", "secondary-escape"] {
            assert!(!native_text_key(
                &Keystroke::parse(key).unwrap().with_simulated_ime()
            ));
        }
    }
}
