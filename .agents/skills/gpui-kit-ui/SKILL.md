---
name: gpui-kit-ui
description: Design and review GPUI desktop UI with gpui-kit components (Button, Dialog, Sheet, Alert, List, Tag, DescriptionList, Form, TabBar…) instead of hand-rolled div() chains, per the gpui-kit design guides. Use when adding or changing UI in a gpui-kit / GPUI Rust app, when reviewing a render() for kit conformance, or when asked to clean up, audit, or "componentise" UI code that builds controls, overlays, status text or rows from div().
---

# gpui-kit UI

Thesis, in priority order (gpui-kit design guides): clarity before
personality; **composition before invention**; tokens before values; desktop
before web convention; every state (hover, focus, selected, disabled, loading,
error) visibly distinct. A project's own UI rules (here `docs/agents/ui.md`) win
over this skill where they differ.

## Quick start

Before writing a `div()` chain, name the behaviour, then find its owner:

| Behaviour | Owner |
|---|---|
| runs a command | `button::Button` (`.primary()` once per decision area, `.ghost()` for toolbars; `Link` only for external URLs) |
| shows error / warning / caveat | `alert::Alert::{error,warning,info}`; async and no decision → `push_notification` |
| decision, form, irreversible action | `open_dialog` → `Dialog`; `open_alert_dialog` → `AlertDialog` naming the object |
| side panel | `open_sheet` → `Sheet` |
| state word | `tag::Tag` / `badge::Badge`, neutral unless the colour means something |
| key/value, caption, field | `DescriptionList`, `Label::new(a).secondary(b)`, `form::field()` |
| rows, tabs, tree, palette | `list::List`+`ListItem`, `tab::TabBar`, `tree::Tree`, `command::Command` |

Component builders and module paths: [COMPONENTS.md](COMPONENTS.md).

Reproducing a kit component from `div`s to match a picture is a defect, not
a shortcut: it loses focus transfer, keyboard navigation, dismissal, disabled
state, geometry and accessibility.

## Workflow A: design or change UI

1. **State the task in one sentence**: object, primary action, next step. If a new user could not infer it from the screen, redesign before coding.
2. **List the states** the surface has: empty, loading, error, disabled-with-reason, selected/focused. Each must render distinctly; none is "temporary".
3. **Pick components from the map**, smallest surface that fits: tooltip → popover → menu → notification → dialog → alert dialog → sheet. Never stack overlays.
4. **Verify each API in the pinned source** (`~/.cargo/git/checkouts/gpui-kit-*/fbc3ebf/crates/component/src`, `crates/base/src`). Never infer from Zed crates, older gpui examples, or memory.
5. **Assign ownership** before writing: state owner (view / `<X>State` / model), presentation owner, focus and keyboard contract (`key_context` and `on_action` on the same focused region, one `Action` per command), layout/overflow owner (one scroll owner per region, `min_w_0()` on the shrinking child, `items_stretch()` in `h_flex`), theme tokens used (`cx.theme().<role>`, rem helpers, `cx.theme().radius`; no `rgb`/hex/`px` for style).
6. **Write it**: `ElementId`s are domain IDs, never indices or labels; controlled values in, intent out via callback; `render` reads and composes only.
7. **Copy**: sentence case, verbs for commands, nouns for destinations, `…` on commands that need more input, errors say what happened and the next action.
8. **Review with the checklist** below, in both theme modes, keyboard-only, at minimum window size, with real content. Done when every state was exercised and a test at the lowest layer proves the behaviour.

## Workflow B: clean up existing UI

1. Run `scripts/audit.sh [dir]` (default `src/ui`). Output is leads grouped by rule, not verdicts.
2. Open each lead; classify with the smell table in [CLEANUP.md](CLEANUP.md). Note the keep-as-is cases there.
3. Fix one class at a time across all files (largest class first), reading the component source before each class.
4. Preserve contracts: same `Action`s dispatch, same `ElementId`s, focus lands where it did, disabled reasons still reach tooltips.
5. Re-run the audit; report fixed vs. kept-with-reason. Done when no lead is unexplained.

## Design review checklist (gpui-kit)

Judge decisions, not component inventory. In order:

1. Is the task clear without learning or guessing?
2. Does every action keep its promise: label, control, state, scope, feedback, result describe one outcome?
3. Is hierarchy decisive and restrained: one focal point; strong colour, bold type, badges, alerts and primary buttons scarce?
4. Could it do less, better: remove, combine or defer an entry point, option or state?
5. Is structure exact: shared alignment spines, equal gaps equal to the rendered pixel, right-aligned numbers, fixed icon slots, trailing actions in fixed lanes?
6. Does it follow the component system: standard controls keep geometry, states, keyboard behaviour, dismissal model?
7. Usable in every state and constraint: keyboard, empty/loading/failure/permission, long strings, zoom, minimum window, reduced motion?
8. Tested in a real window with real components and representative content, not an ideal screenshot?

## Desktop, not web

Arrow cursor on buttons; persistent panes, sidebars, tabs and menus over page
navigation; explicit focus ownership and region-owned scrolling; resizable
shell with a minimum window size; no hover-only critical actions; no
link-styled commands.
