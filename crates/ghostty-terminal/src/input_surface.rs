use gpui_kit::{KeyContext, Keystroke};

pub fn native_input_context(surface: &'static str) -> KeyContext {
    let mut context = KeyContext::default();
    context.add("NativeInput");
    context.add("NativeSurface");
    context.add(surface);
    context
}

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
        let os = if cfg!(target_os = "macos") {
            gpui_kit::OperatingSystem::Mac
        } else {
            gpui_kit::OperatingSystem::Linux
        };
        for key in ["enter", "tab", "escape", "ctrl-w", "secondary-escape"] {
            assert!(!native_text_key(
                &Keystroke::parse_with_os(key, os)
                    .unwrap()
                    .with_simulated_ime()
            ));
        }
    }
}
