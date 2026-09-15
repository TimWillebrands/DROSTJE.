# AGENTS.md — Drost

Drost is a macOS desktop app (Rust 2024, GPUI via `gpui-kit = "=0.6.1"`) that
drives local coding agents (OMP over native RPC), edits prompts in an embedded
Neovim, and dispatches work to the Compliance Software Factory CLI. The README
is Dutch and user-facing; code, comments, commits, docs and UI strings are
English. `DROST_APPLICATION_PLAN.md` records the original scope; the README
documents the current thread workflow and shortcuts.

This is a normative file. **Must** is a correctness or ecosystem constraint,
**should** is the default that needs a stated reason to override, **may** is
optional. Keep it under 200 lines; add a rule only for a mistake that recurred.

## Commands

```sh
cargo check --all-targets                  # must pass after every step
cargo test                                 # no provider calls, no paid turns
cargo test <name> -- --nocapture           # one test
cargo fmt                                  # run on files you touched
cargo clippy --all-targets                 # ~45 warnings today; do not add any
cargo run                                  # real app; set DROST_DATA_DIR for a scratch data dir

# Opt-in runtime tests (real Neovim / real window, isolated temp state):
NVIM=/opt/homebrew/bin/nvim cargo test --test neovim_runtime -- --ignored --nocapture
cargo test --features runtime-qa --lib workspace_actions_with_real_backends -- --ignored --nocapture
```

## Definition of done

- `cargo check --all-targets` and `cargo test` pass; touched files are `cargo fmt` clean; clippy warning count did not grow.
- Every visible control is wired to real behaviour. No mock conversations, fake run rows, placeholder data, or simulated providers, ever, including "temporarily".
- New UI states (empty, loading, error, disabled-with-reason) exist and were exercised.
- New behaviour has a test at the lowest layer that proves it (see Testing).
- You can name, for the change: the state owner, the presentation owner, the focus/keyboard contract, the layout/overflow owner, the theme tokens used, and the test that fails if it regresses.

## Architecture

Dependencies point downward; a lower module never imports a higher one.

| Layer | Modules | Owns |
|---|---|---|
| UI | `src/ui/*` | GPUI views, focus, actions, presentation. Reads `AppState`; mutates only through its methods. |
| App | `src/app*.rs` | `AppState`, orchestration, persistence scheduling, shutdown. |
| Domain seams | `src/harness`, `src/neovim`, `src/factory`, `src/store`, `src/attachments` | One capability each behind a small typed API. Only `harness/omp.rs` knows OMP wire fields; only `neovim/rpc.rs` speaks MessagePack; only `factory/launch.rs` builds argv. |
| Platform | `src/platform.rs` | Process groups, locks, paths. All `unsafe` lives here or in a transport module with a `// SAFETY:` comment. |

- No process, file, or network I/O in `render` or on the GPUI thread. Background executors do the work; results come back with a revision/identity and stale results are dropped.
- Do not add a plugin registry, generic transport, or second harness abstraction. The seam is a concrete handle (`harness::open` → `ConversationHandle`).
- Drost never guesses outcomes: unknown stays `Unknown`, exit 0 without a terminal event is not success, nothing auto-retries or auto-resends.

## UI: gpui-kit first

GPUI is reached only through the facade: `use gpui_kit::*;` for GPUI itself,
`gpui_kit::component::<module>::<Type>` for components, `gpui_kit::actions!`
for actions. Never add `gpui`, `gpui-component` or `gpui-base` as direct
dependencies, and never import Zed's crates.

Design thesis, in priority order: clarity before personality; composition
before invention; tokens before values; desktop before web convention; every
state (hover, focus, selected, disabled, loading, error) visibly distinct.
Exact signatures live in the pinned source under
`~/.cargo/registry/src/*/gpui-component-0.6.1/src` and `gpui-base-0.6.1/src`;
search there; never infer an API from Zed's crates or older gpui examples.

**Composition before invention.** Before writing a `div()` chain, find the
component that owns that behaviour. Reproducing a kit component from `div`s to
match a screenshot is a defect: it loses focus transfer, keyboard navigation,
dismissal, disabled state, geometry and accessibility.

| You need | Use | Not |
|---|---|---|
| Any command trigger | `button::Button` (`.ghost()`/`.outline()`/`.primary()`/`.danger()`) | clickable `div`, `Link` |
| Icon-only trigger | `Button::new(id).icon(IconName::…).tooltip(…)` | text glyphs like `>` / `⌄` |
| Modal decision / short form | `WindowExt::open_dialog`, `dialog::Dialog`, `AlertDialog` for irreversible | absolute backdrop `div` |
| Supplementary panel | `sheet::Sheet` via `open_sheet` | hand-rolled overlay |
| Async status, no decision | `window.push_notification(Notification::…)` | inline banner `div` |
| Persistent in-task warning | `alert::Alert` | coloured box |
| Command palette | `command::{Command, CommandItem, CommandGroup, CommandState}` | `Input` + `Button` list |
| Rows with selection | `list::{List, ListItem}` / `sidebar::{SidebarMenu, SidebarMenuItem}` / `tree::Tree` | ghost buttons per row |
| Key hint | `kbd::Kbd`, or an `Action` on a `CommandItem`/menu item | formatted shortcut string |
| Secondary actions | `menu::DropdownMenu` / `ContextMenu` | row of hover icons |
| Caption / secondary text | `label::Label::new(main).secondary(detail)` or `.text_color(cx.theme().muted_foreground)` | `text_color(rgb(..))` |
| State chip | `badge::Badge` / `tag::Tag`, neutral by default | coloured text |
| Key/value details | `description_list::DescriptionList` | ad-hoc rows |
| Labelled fields | `form::{v_form, field}` | label `div` + `Input` |
| Tabs / segmented | `tab::{TabBar, Tab}` | button row with selected styling |
| Panels, docks, splits | `dock::DockArea` (already in use), `resizable::*` | nested flex with manual drag |
| Long collections | `VirtualList`, `Table` | mapping every item to an element |
| Markdown | `text::markdown(..)` | second Markdown engine |

Command palette contract: build static entries on
`Command`; keep dynamic entries in the owning view and rebuild `Command` on
render; give each rendered palette its own `CommandState`; bind real `Action`s
on `CommandItem::action` so hint and dispatch cannot disagree; let the hosting
Dialog own dismissal after `on_cancel`; keep child factories side-effect-free.
Apply the same ownership split to every kit component: the component owns
behaviour, `<X>State` owns interaction state, the owning view owns the data.

### Theme and scale

- Read every colour from `cx.theme()` (import `gpui_kit::component::ActiveTheme`) by semantic role: `background`, `foreground`, `border`, `muted`, `muted_foreground`, `primary`, `danger`, `warning`, `success`, `info`, `popover`, `sidebar*`, `list*`, `tab*`, `status_bar`, `title_bar`.
- Application UI must not contain raw `rgb(..)`, `rgba(..)`, `hsla(..)` or hex. Missing role → add it to the theme setup in `src/lib.rs` (`Theme::global_mut`, then `Theme::sync_base`), never at the call site.
- Layout uses rem helpers (`p_2()`, `gap_3()`, `text_sm()`, `w_64()`) and component `Size` (`.small()`, `.large()`). Direct `px(..)` is allowed only for hairlines, window minima, bitmap dimensions and external geometry, with a comment saying which.
- Monospace only for code, identifiers, shortcuts, aligned numbers and the Neovim grid, via `cx.theme().mono_font_family`.
- Verify custom surfaces in both `ThemeMode::Dark` and `Light`.

### State, render, identity

- Retain `Entity`, `WeakEntity`, `FocusHandle`, IDs. Never retain `&mut Window`, `&mut App`, or `&mut Context<_>`.
- `render` reads state and composes elements. No mutation, no `cx.notify()`, no async start, no focus request from `render`.
- `RenderOnce` for value-like fragments; `Entity<T>` only where behaviour spans frames. Store entities in the owner, never create them in `render`.
- Controlled values: pass the value in, receive intent via callback, update owner, notify once.
- `ElementId`s are stable domain IDs (`("thread-row", thread.id)`), never indices or labels.
- Flex: `min_w_0()`/`min_h_0()` on the child that may shrink; `h_flex` centres, so full-height columns get `items_stretch()`; one scroll owner per region with the scrollbar on its edge.

### Commands, keyboard, focus

- One logical command = one `Action`. Palette item, menu item, button and key binding dispatch the same Action; label, enabled state and reason come from one place.
- Every command is reachable from the command palette and by keyboard. Bindings are context-scoped (`Workspace`, `List`, `Thread`, `NvimEditor`, `NativeInput`, `Command`, `Dialog`, `Sheet`); `key_context` and `on_action` sit on the same focused region.
- Existing bindings, do not reassign: `Cmd-Shift-P` palette; `Cmd-1/2/3` focus Projects / Thread / Output; `Cmd-B/J` toggle Projects / Output; `Cmd-N` new Conversation; `Cmd-Shift-N` new Factory thread; `Cmd-,` settings; `Cmd-Return` submit prompt, start editable Factory configuration, or confirm a form; `Cmd-.` interrupt interactive turn; `i` from a Conversation focuses its composer. In lists: `j`/`k`/arrows move, `gg`/`G` jump, `h`/`l` collapse/expand, Return opens.
- `Ctrl-W` then `h/j/k/l` (also held-Control variants) moves to a visible neighboring pane. App chords exclude native inputs and overlays; Neovim owns normal-mode buffer-local mappings and keeps insert/command-line word deletion. Do not intercept typing inside `NativeInput` or `NvimEditor`; Return inside an editor is always newline.
- Escape dismisses the topmost layer and restores previous focus. Inside Neovim, Escape is Neovim's; `Cmd-Escape` leaves the editor.
- Overlays trap focus and restore it on dismiss. Nested overlays dismiss from the top.

### Feedback and copy

- Smallest surface that fits: tooltip → popover → menu → notification → dialog → alert dialog → sheet. Never stack overlays.
- Instant feedback on activation; disable submit while in flight; keep user input; show the result next to the object. Reversible → undo/notification; irreversible → `AlertDialog` naming the object.
- Errors say what happened and the next action. Missing tools or auth are visible setup issues in Settings with a retry, never silent installs or fallbacks.
- Sentence case (`Reset layout`), no trailing period on labels, `…` (single character) on commands that open a dialog or need more input, verbs for commands, nouns for destinations. No `Are you sure…`, no `successfully`.

## Zed conventions we adopt

Drost follows Zed's UX model on top of the kit's design rules. Where they
conflict, the kit rule wins, in particular: hover may reveal a shortcut to an
action, never the only copy of it.

- Usable without a mouse; every shortcut discoverable in tooltips, menus or the palette. Command palette (`Cmd-Shift-P`) is the discoverability backstop.
- Workspace shell: Projects left, selected Thread center, optional Output below; no permanent Factory sidebar or duplicate conversation tabs. Use native TitleBar, StatusBar and dock headers. Persist user-controlled layout; clamp restored values to preserve usable Thread space.
- Pick an elevation layer, not a colour: window background → panel surface → editor surface → popover/menu → modal.
- Transient, centred, keyboard-driven pickers instead of OS dialogs; toasts for non-blocking info; alert modal only for consequential confirmation.
- `Tooltip`/menu items show the bound key automatically; never format shortcuts by hand.
- Terminology is stable across palette, menu, tooltip, dialog and docs: Project, Thread, Conversation, Factory thread, Draft, Turn, Launch, Run, Attempt, Task, Region.
- Instant feedback; keep frames cheap: virtualise history and logs, deliver bounded event batches, never rebuild the whole conversation per token.

## Rust conventions

- Edition 2024, stable toolchain, no nightly features. Dependencies via `cargo add`; commit `Cargo.lock`. Keep `gpui-kit` pinned with `=`.
- Errors: `thiserror` enums at seams that callers match on (`store::StoreError`); `anyhow` with `.context(..)` for orchestration and boundaries. No `unwrap`/`expect` outside tests except documented invariants in startup.
- `unsafe` only for FFI/process control in `platform.rs` and transport modules, each block with `// SAFETY:` and its error checked.
- Prefer `#[expect(lint, reason = "..")]` over `#[allow]`; never edit `[lints]` to silence a warning.
- Exhaustive `match` on domain enums; add a variant rather than a wildcard arm. Unknown external data (events, RPC) is preserved raw and displayed generically, not dropped.
- Names: views after product concepts (`Workspace`, `FactoryForm`), handlers after intent (`confirm_dispatch`), `<Control>State` for retained state, `_ix` for indices, `Id` suffix for identity.
- Modules: one responsibility; split when ownership cannot be understood without reading unrelated behaviour. Document invariants and lifecycle constraints, not builder calls.

## Testing

Test at the lowest layer that proves the behaviour:

1. Pure tests for parsers, reducers, state machines (`neovim/grid.rs`, `factory/events.rs`, `harness/omp.rs` framing). Use real temp files and pipes, not assertions on command strings.
2. Integration tests in `tests/` for the store, factory launch and events, attachments.
3. UI tests with `#[gpui_kit::test]` and `gpui_kit::test::{TestWindowExt, TestAppContextExt, TestSupportExt}`: give elements stable IDs plus `.test_support()` (before `.track_focus`), then drive `click`/`press`/`input` and assert snapshots. Prefer this over raw `VisualTestContext` mouse simulation.
4. Real-process tests (`--ignored`, `runtime-qa`) for Neovim barriers and OMP protocol; never in default `cargo test`, never with provider credentials from fixtures.

Add a regression test before fixing a deterministic bug. "Compiles" and "screenshot looks right" are not evidence for UI work; keyboard, focus, resize, both themes and failure states are.

## Never

- Fabricate progress, completion, PR URLs, model names or defaults. `null` model means the harness default.
- Install, upgrade, authenticate or configure `omp`, `nvim`, `factory` or Docker on the user's behalf.
- Pass `--yolo`, `--ignore-capacity`, `--clean`, `--tui` or shell strings to child processes; use separated argv.
- Touch the user's working repositories from tests. Fixtures come from `mktemp -d` per the plan.
- Write editor buffers to project files, replay raw keys, auto-submit or auto-resend after a crash or reconnect.
- Commit with `--no-verify`, or push without being asked.
