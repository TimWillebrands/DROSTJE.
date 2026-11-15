# Coding standards

Read when writing or reviewing Rust in this repo. Repo-wide invariants live in `AGENTS.md`; UI rules in `docs/agents/ui.md`.

## Layering

The repo is a Cargo workspace. Each domain seam is its own crate under `crates/<feature>/` (package `drostje-<feature>`); the GPUI app is the root `drostje` package. Dependencies point downward; a lower layer never imports a higher one, and Cargo rejects cycles.

| Layer | Where | Owns |
|---|---|---|
| UI | `src/ui/<feature>/` | GPUI views, focus, actions, presentation, one folder per feature (`conversation`, `workspace`, `changes`, `browser`, `terminal`, `nvim`, `factory`, `network`, `settings`, `theme`). Reads `AppState` and mutates it only through its methods. |
| App | `src/app/` | `AppState`, orchestration, persistence scheduling, shutdown; one file per feature. |
| Domain seams | `crates/harness` (+ `attachments`), `crates/remote` (+ the `drostje-host` binary), `crates/browser-tools`, `crates/browser` (the only Servo crate), `crates/factory`, `crates/neovim`, `crates/network`, `crates/store`, `crates/tracker`, `crates/titles`, `crates/worktrees`, `crates/file-refs`, `crates/fonts`, `crates/diff` | One capability each behind a small typed API. Draft identities and revisions live in `crates/model`. |
| Platform | `crates/platform` | Process groups and sessions, locks, paths, Git plumbing, and every `unsafe` block outside transports. |
| Checks | `crates/lint` | Repository rules `cargo test` enforces: the comment lexer per language and the Markdown list. Turn a recurring review correction into a check here, with an error message that says what to do instead. |

- Only the root crate and `crates/ghostty-terminal` depend on `gpui-kit`; only `crates/browser` names Servo types. Domain crates build and test without either, which keeps the headless host (`cargo build -p drostje-remote --bin drostje-host`) free of GPUI and Servo. Convert to GPUI types at the UI edge (`drostje_diff::RowText` is an `Arc<str>`; the view uses the zero-copy `SharedString::from(&row.text)`).
- `src/lib.rs` re-exports each crate under its feature name (`pub use drostje_harness as harness;`), so app and UI code reads `crate::harness::…`. A new seam gets a crate, an entry in `[workspace.dependencies]`, and one alias line.
- External versions live once in `[workspace.dependencies]`; member crates use `dep.workspace = true`. Tests for a crate live in that crate (`crates/<feature>/tests/`); `tests/` at the root holds only window tests that need the app.

- The harness seam is the concrete handle `harness::open` → `ConversationHandle`. Add a harness beside the existing ones rather than a plugin registry, generic transport or second abstraction.
- Parse and derive data off the GPUI thread, next to where it is read (for example `diff::parse` builds both diff layouts in the background), so `render` only indexes.
- A user preference belongs to `AppState` and persists through `set_setting`; per-view retained state (scroll, cursor, focus handle) lives in the `Workspace`, keyed by thread.

## Rust

- Edition 2024, stable toolchain, no nightly features.
- Errors: `thiserror` enums at seams whose callers match on them (`store::StoreError`, `diff::LocationError`); `anyhow` with `.context(..)` for orchestration and boundaries.
- `unwrap` only in tests; `expect` in tests and in startup invariants, with a message saying why it holds.
- Silence a lint with `#[expect(lint, reason = "..")]` on the item; `[lints]` in `Cargo.toml` stays as it is.
- Match domain enums exhaustively; a new case gets a new variant, not a wildcard arm.
- Keep unknown external data (events, RPC) raw and display it generically.
- Names: views after product concepts (`Workspace`, `FactoryForm`), handlers after intent (`confirm_dispatch`), `<Control>State` for retained state, `_ix` for indices, `Id` suffix for identities.
- One responsibility per module; split when ownership can't be understood without reading unrelated behaviour. No comments or doc comments (`crates/lint` rejects them): an invariant goes in a type, an assertion or a test; a lifecycle constraint in the API's shape. The only exceptions are `// SAFETY:` above `unsafe` and a `/// # Safety` section on an `unsafe fn`.

## Tests

Test at the lowest layer that proves the behaviour:

1. **Pure tests** for parsers, reducers and state machines (`crates/diff`, `crates/factory/src/events.rs`, `crates/harness/src/omp.rs` framing). Use real temp files and pipes; assert on behaviour, not on command strings.
2. **Integration tests** in `crates/<feature>/tests/` for the store, Factory launch and events, attachments, Neovim runtime. `cargo test` at the root runs every workspace member.
3. **UI tests** with `#[gpui_kit::test]` and `gpui_kit::test::{TestWindowExt, TestAppContextExt, TestSupportExt}`: stable element IDs plus `.test_support()` (before `.track_focus`), then `click`/`press`/`input` and snapshots. These compile only with `--features runtime-qa`, so logic that plain `cargo test` must cover goes in a struct with no GPUI handles (see `ui::changes::RowList`).
4. **Native runtime tests** (`runtime-qa`, `#[ignore]`) for editor composition, real windows and pane focus. Drive them with `simulate_keystrokes`, draw a few frames before asserting on bounds or pane navigation, and restore the fixture repo at the end. Opt-in real-process OMP tests never use provider credentials from fixtures.

Evidence for UI work is keyboard, focus, resize, both themes and failure states, exercised in a test.
