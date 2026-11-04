# Coding standards

Read when writing or reviewing Rust in this repo. Repo-wide invariants live in `AGENTS.md`; UI rules in `docs/agents/ui.md`.

## Layering

Dependencies point downward; a lower layer never imports a higher one.

| Layer | Modules | Owns |
|---|---|---|
| UI | `src/ui/*` | GPUI views, focus, actions, presentation. Reads `AppState` and mutates it only through its methods. |
| App | `src/app.rs`, `src/app/*` | `AppState`, orchestration, persistence scheduling, shutdown. |
| Domain seams | `src/harness`, `src/factory`, `src/store`, `src/attachments`, `src/diff.rs` | One capability each behind a small typed API. Draft identities and revisions live in `src/model.rs`. |
| Platform | `src/platform.rs`, `src/platform/*` | Process groups, locks, paths, Git plumbing. |

- The harness seam is the concrete handle `harness::open` → `ConversationHandle`. Add a harness beside the existing ones rather than a plugin registry, generic transport or second abstraction.
- Parse and derive data off the GPUI thread, next to where it is read (for example `diff::parse` builds both diff layouts in the background), so `render` only indexes.
- A user preference belongs to `AppState` and persists through `set_setting`; per-view retained state (scroll, cursor, focus handle) lives in the `Workspace`, keyed by thread.

## Rust

- Edition 2024, stable toolchain, no nightly features.
- Errors: `thiserror` enums at seams whose callers match on them (`store::StoreError`, `diff::LocationError`); `anyhow` with `.context(..)` for orchestration and boundaries.
- `unwrap`/`expect` only in tests and in startup invariants that carry a comment saying why they hold.
- Silence a lint with `#[expect(lint, reason = "..")]` on the item; `[lints]` in `Cargo.toml` stays as it is.
- Match domain enums exhaustively; a new case gets a new variant, not a wildcard arm.
- Keep unknown external data (events, RPC) raw and display it generically.
- Names: views after product concepts (`Workspace`, `FactoryForm`), handlers after intent (`confirm_dispatch`), `<Control>State` for retained state, `_ix` for indices, `Id` suffix for identities.
- One responsibility per module; split when ownership can't be understood without reading unrelated behaviour. Doc comments state invariants and lifecycle constraints, not builder calls.

## Tests

Test at the lowest layer that proves the behaviour:

1. **Pure tests** for parsers, reducers and state machines (`src/diff.rs`, `src/factory/events.rs`, `src/harness/omp.rs` framing). Use real temp files and pipes; assert on behaviour, not on command strings.
2. **Integration tests** in `tests/` for the store, Factory launch and events, attachments.
3. **UI tests** with `#[gpui_kit::test]` and `gpui_kit::test::{TestWindowExt, TestAppContextExt, TestSupportExt}`: stable element IDs plus `.test_support()` (before `.track_focus`), then `click`/`press`/`input` and snapshots. These compile only with `--features runtime-qa`, so logic that plain `cargo test` must cover goes in a struct with no GPUI handles (see `ui::changes::RowList`).
4. **Native runtime tests** (`runtime-qa`, `#[ignore]`) for editor composition, real windows and pane focus. Drive them with `simulate_keystrokes`, draw a few frames before asserting on bounds or pane navigation, and restore the fixture repo at the end. Opt-in real-process OMP tests never use provider credentials from fixtures.

Evidence for UI work is keyboard, focus, resize, both themes and failure states, exercised in a test.
