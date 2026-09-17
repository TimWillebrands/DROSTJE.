# AGENTS.md — DROSTJE.

DROSTJE. is a macOS desktop app (Rust 2024, GPUI via `gpui-kit = "=0.6.1"`) that
drives local coding agents (OMP over native RPC), edits prompts in embedded
Neovim within a GPUI Kit-styled composer, and dispatches work to the Compliance Software Factory CLI. The README
is Dutch and user-facing; code, comments, commits, docs and UI strings are
English. 

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
cargo run                                  # real app; set DROSTJE_DATA_DIR for a scratch data dir

# Opt-in native editor/window tests, isolated temp state:
cargo test --features runtime-qa native_composition_is_recovery_only_until_committed -- --ignored --nocapture
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
| Domain seams | `src/harness`, `src/factory`, `src/store`, `src/attachments` | One capability each behind a small typed API. Only `harness/omp.rs` knows OMP wire fields; only `factory/launch.rs` builds argv. Draft identities and revisions live in `model`. |
| Platform | `src/platform.rs` | Process groups, locks, paths. All `unsafe` lives here or in a transport module with a `// SAFETY:` comment. |

- No process, file, or network I/O in `render` or on the GPUI thread. Background executors do the work; results come back with a revision/identity and stale results are dropped.
- Do not add a plugin registry, generic transport, or second harness abstraction. The seam is a concrete handle (`harness::open` → `ConversationHandle`).
- DROSTJE. never guesses outcomes: unknown stays `Unknown`, exit 0 without a terminal event is not success, nothing auto-retries or auto-resends.

## UI: keyboard-first, gpui-kit components

Design and cleanup rules for UI live in the project skill
`.agents/skills/gpui-kit-ui/` (`SKILL.md`, `COMPONENTS.md`, `CLEANUP.md`,
`scripts/audit.sh`). Read it before touching `src/ui/`; this section holds
only what is specific to DROSTJE.

- GPUI is reached only through the facade: `use gpui_kit::*;` for GPUI itself, `gpui_kit::component::<module>::<Type>` for components, `gpui_kit::actions!` for actions. Never add `gpui`, `gpui-component` or `gpui-base` as direct dependencies, and never import Zed's crates.
- Exact signatures live in the pinned source under `~/.cargo/registry/src/*/gpui-component-0.6.1/src` and `gpui-base-0.6.1/src`; search there; never infer an API from Zed's crates or older gpui examples.
- A missing theme role is added to the theme setup, never at the call site. `src/ui/theme.rs` owns theme loading and application.
- Monospace only for code, identifiers, shortcuts and aligned numbers, via `cx.theme().mono_font_family`.
- Workspace shell: Projects left, thread tabs centre (a lone thread still shows its tab); each thread's sidebar holds Changes and Output tabs; native TitleBar and dock headers; no permanent Factory sidebar or redundant footer. Save/error status lives in the title bar. Persist open thread identities and user-controlled layout; clamp restored values to preserve usable Thread space.
- Terminology is stable across palette, menu, tooltip, dialog and docs: Project, Thread, Conversation, Factory thread, Draft, Turn, Launch, Run, Attempt, Task, Region.
- Instant feedback; keep frames cheap: virtualise history and logs, deliver bounded event batches, never rebuild the whole conversation per token.

### Keyboard-driven, Vim-style navigation

The mouse is optional. Every screen is operated end to end from the keyboard,
with Vim-style motion keys for navigation and chords between panes. Hover may
reveal a shortcut to an action, never the only copy of it; the command palette
(`Cmd-Shift-P`) is the discoverability backstop.

- **Draft safety.** `Cmd-Escape` leaves the editor. Uncommitted IME text remains separate recovery text; submission and replacement require a committed revision. Never write editor buffers to project files or replay raw keys.
- **Lists are Vim lists.** In every list, tree and palette: `j`/`k` (and arrows) move, `gg`/`G` jump, `h`/`l` collapse/expand, Return opens. Selection and focus are distinct and both visible. A new list that does not answer to these keys is not done.
- **Panes are Vim windows.** `Ctrl-W` then `h/j/k/l` (also held-Control variants) moves to a visible neighbouring pane. App chords exclude native inputs and overlays. `i` from a Conversation focuses its composer, mirroring insert mode.
- **One command, one `Action`.** Palette item, menu item, button and key binding dispatch the same Action; label, enabled state and disabled reason come from one place (`src/ui/commands.rs`). Every command is reachable from the palette. Bindings are context-scoped (`Workspace`, `List`, `Thread`, `PromptEditor`, `NativeInput`, `Command`, `Dialog`, `Sheet`); `key_context` and `on_action` sit on the same focused region. Tooltips and menu items show the bound key automatically; never format shortcuts by hand.
- **Fixed bindings, do not reassign** (`Cmd` is GPUI `secondary-`: Cmd on macOS, Ctrl on Linux; `Cmd-W` and `Cmd-V` are `Ctrl-Shift-W/V` on Linux because a lone Ctrl-W is the pane-chord prefix and Ctrl-V is Vim's): `Cmd-Shift-P` palette; `Cmd-1/2/3` focus Projects / Thread / Output; `Cmd-B/J` toggle Projects / Output; `Cmd-N` new Conversation; `Cmd-Shift-N` new Factory thread; `Ctrl-Tab` / `Ctrl-Shift-Tab` cycle thread tabs; `Cmd-W` closes the active tab; `Cmd-,` settings; `Cmd-Return` submit prompt, start editable Factory configuration, or confirm a form; `Cmd-.` interrupt interactive turn. New bindings must not shadow native input editing or list motions.
- **Layers and focus.** Escape dismisses the topmost layer and restores previous focus. Overlays trap focus and restore it on dismiss; nested overlays dismiss from the top; never stack overlays. Transient, centred, keyboard-driven pickers instead of OS dialogs.
- **Feedback.** Smallest surface that fits: tooltip → popover → menu → notification → dialog → alert dialog → sheet. Instant feedback on activation; disable submit while in flight; keep user input; show the result next to the object. Reversible → undo or notification; irreversible → `AlertDialog` naming the object. Missing tools or auth are visible setup issues in Settings with a retry. Install or upgrade dependencies only in response to an explicit user request; never do so silently or as a fallback.
- **Copy.** Sentence case, no trailing period on labels, `…` on commands that need more input, verbs for commands, nouns for destinations. Errors say what happened and the next action. No `Are you sure…`, no `successfully`.

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

1. Pure tests for parsers, reducers, state machines (`factory/events.rs`, `harness/omp.rs` framing). Use real temp files and pipes, not assertions on command strings.
2. Integration tests in `tests/` for the store, factory launch and events, attachments.
3. UI tests with `#[gpui_kit::test]` and `gpui_kit::test::{TestWindowExt, TestAppContextExt, TestSupportExt}`: give elements stable IDs plus `.test_support()` (before `.track_focus`), then drive `click`/`press`/`input` and assert snapshots. Prefer this over raw `VisualTestContext` mouse simulation.
4. Native runtime tests (`runtime-qa`) for editor composition and window contracts; opt-in real-process tests for OMP protocol, never with provider credentials from fixtures.

Add a regression test before fixing a deterministic bug. "Compiles" and "screenshot looks right" are not evidence for UI work; keyboard, focus, resize, both themes and failure states are.

## Never

- Fabricate progress, completion, PR URLs, model names or defaults. `null` model means the harness default.
- Install or upgrade dependencies without an explicit user request, or authenticate or configure `omp`, `factory` or Docker on the user's behalf.
- Pass `--yolo`, `--ignore-capacity`, `--clean`, `--tui` or shell strings to child processes; use separated argv.
- Touch the user's working repositories from tests. Fixtures come from `mktemp -d` per the plan.
- Write editor buffers to project files, replay raw keys, auto-submit or auto-resend after a crash or reconnect.
- Commit with `--no-verify`, or push without being asked.
