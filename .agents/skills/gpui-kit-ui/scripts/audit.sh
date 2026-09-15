#!/usr/bin/env bash
# Heuristic audit for hand-rolled UI in a gpui-kit codebase.
# Usage: audit.sh [dir ...]   (default: src/ui)
# Each finding is `path:line: <text>` under a numbered rule. Findings are
# leads, not verdicts: open the line, then decide against COMPONENTS.md.
set -u
dirs=("${@:-src/ui}")
rg_or_grep() {
  if command -v rg >/dev/null 2>&1; then rg -n --no-heading -t rust "$@" "${dirs[@]}"; else grep -rn --include='*.rs' -E "$@" "${dirs[@]}"; fi
}
rule() { printf '\n== %s ==\n' "$1"; }
count=0
run() { out=$(rg_or_grep "$1" | grep -Ev "${2:-__never__}"); if [ -n "$out" ]; then echo "$out"; count=$((count + $(echo "$out" | wc -l | tr -d ' '))); else echo "(none)"; fi; }

rule "1 raw colours (must come from cx.theme() by role)"
run '\b(rgb|rgba|hsla|hsl)\(|0x[0-9a-fA-F]{6}\b' 'to_rgb\(\)|// theme bridge|/theme\.rs:'

rule "2 px(..) outside hairline / window minimum / bitmap / external geometry (needs a comment naming which)"
run '\bpx\(' 'px\(0\.\)|px\(1\.\)|// px:|simulate_window_resize'

rule "3 clickable non-Button surfaces (should be Button, ListItem, SidebarMenuItem, Tab, MenuItem)"
run 'cursor_pointer\(\)|\.on_mouse_down\(|\.on_mouse_up\('
# on_click whose chain root (within 6 lines above) is a bare div/h_flex/v_flex
for f in $(rg_or_grep -l '\.on_click\(' 2>/dev/null); do
  awk -v F="$f" '
    { buf[NR]=$0 }
    /\.on_click\(/ {
      root=""; ok=0
      for (i=NR; i>NR-6 && i>0; i--) {
        if (buf[i] ~ /(Button|ListItem|SidebarMenuItem|PopupMenuItem|Tab|Toggle|Checkbox|Switch|Link|MenuItem|DropdownButton)::new|Button::new|\.item\(|PopupMenuItem/) { ok=1; break }
        if (buf[i] ~ /(^|[^a-z_])(div|h_flex|v_flex)\(\)/) { root=i; break }
      }
      if (!ok && root != "") printf "%s:%d: %s\n", F, root, buf[root]
    }' "$f"
done

rule "4 text glyphs used as icons (use Button::icon / Icon with IconName)"
run '"(>|<|›|‹|⌄|⌃|▸|▾|▶|▼|✓|✕|×|•)"|"[^"]* (−|\+|-)"'

rule "4b level prefix or interpunct-joined status in a label (Alert variant / Tag / Label::secondary)"
run '"(Info|Warning|Error): |\{\} · \{\}|" · "'

rule "5 hand-formatted shortcuts (use Kbd, Tooltip::action, or Action on a CommandItem/menu item)"
run '"[^"]*(⌘|⇧|⌥|⌃|Cmd-|Ctrl-|Shift-|Alt-)[^"]*"' 'keystroke|KeyBinding::new|bind\('

rule "6 manual hover/selected styling (component owns state styles)"
run '\.hover\(\||\.bg\(cx\.theme\(\)\.(list_active|list_hover|sidebar_accent|tab_active|accent)\)'

rule "7 bordered/coloured status boxes (Alert / Notification / Badge / Tag)"
run '\.border_color\(cx\.theme\(\)\.(danger|warning|success|info)\)|\.bg\(cx\.theme\(\)\.(danger|warning|success|info)'

rule "8 absolute overlays / backdrops (Dialog, Sheet, Popover, DropdownMenu via WindowExt)"
# skip canvas() measurement overlays (chain root within 30 lines above)
for f in $(rg_or_grep -l '\.absolute\(\)|\.occlude\(\)' 2>/dev/null); do
  awk -v F="$f" '{ buf[NR]=$0 }
    /\.absolute\(\)|\.occlude\(\)/ { skip=0; for (i=NR; i>NR-30 && i>0; i--) if (buf[i] ~ /canvas\(|deferred|anchored/) skip=1
      if (!skip) { printf "%s:%d: %s\n", F, NR, $0; n++ } }
    END { if (!n) print "(none)" }' "$f"
done

rule "9 label-above-input pairs (form::{v_form, field})"
rg_or_grep -A2 'Label::new' | grep -E 'Input|TextInput|NumberInput|Select' | sed 's/^/  suspect: /' || true

rule "10 status colour on prose (is it an Alert, Tag, Badge, or Label::secondary instead?)"
run '\.text_color\(cx\.theme\(\)\.(danger|warning|success|info)\)'

printf '\n%s findings across %s\n' "$count" "${dirs[*]}"
