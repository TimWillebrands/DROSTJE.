#ifndef GPUI_GHOSTTY_NATIVE_INPUT_H
#define GPUI_GHOSTTY_NATIVE_INPUT_H

#include <stdint.h>

static inline void gpui_ghostty_commit_text(
    ghostty_surface_t surface, const char *text, uintptr_t length
) {
    if (surface == NULL || length == 0) return;
    ghostty_input_key_s event = {
        .action = GHOSTTY_ACTION_PRESS,
        .mods = GHOSTTY_MODS_NONE,
        .consumed_mods = GHOSTTY_MODS_NONE,
        .keycode = UINT32_MAX,
        .text = text,
        .unshifted_codepoint = 0,
        .composing = false,
    };
    (void)ghostty_surface_key(surface, event);
}

#endif
