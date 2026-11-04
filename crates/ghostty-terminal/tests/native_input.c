#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "../vendor/ghostty/include/ghostty.h"

static unsigned key_calls;
static unsigned paste_calls;
static const char *expected;

bool ghostty_surface_key(ghostty_surface_t surface, ghostty_input_key_s event) {
    assert(surface != NULL);
    assert(event.action == GHOSTTY_ACTION_PRESS);
    assert(event.mods == GHOSTTY_MODS_NONE);
    assert(event.consumed_mods == GHOSTTY_MODS_NONE);
    assert(event.keycode == UINT32_MAX);
    assert(event.unshifted_codepoint == 0);
    assert(!event.composing);
    assert(strcmp(event.text, expected) == 0);
    key_calls++;
    return true;
}

void ghostty_surface_text(ghostty_surface_t surface, const char *text, uintptr_t length) {
    (void)surface;
    (void)text;
    (void)length;
    paste_calls++;
}

#include "../shim/native_input.h"

int main(void) {
    // Native commits are typing, even when the program enabled bracketed paste.
    // A normal-mode ':' must reach Neovim as a key, never as a clipboard paste.
    const char *commits[] = {":", "?", "G", "{}|~", "é", "界", "e\xcc\x81", "🙂"};
    for (unsigned index = 0; index < sizeof(commits) / sizeof(commits[0]); index++) {
        expected = commits[index];
        gpui_ghostty_commit_text((ghostty_surface_t)(uintptr_t)1, expected, strlen(expected));
        assert(key_calls == index + 1);
        assert(paste_calls == 0);
    }
    gpui_ghostty_commit_text(NULL, "ignored", 7);
    gpui_ghostty_commit_text((ghostty_surface_t)(uintptr_t)1, "", 0);
    assert(key_calls == sizeof(commits) / sizeof(commits[0]));
    puts("native commits preserve key text without bracketed paste");
    return 0;
}
