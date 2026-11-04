# DROSTJE.

Desktop app (Rust 2024, GPUI through `gpui-kit`) that drives local coding agents, edits prompts in embedded Neovim and launches Compliance Software Factory runs. Code, comments, commits, docs and UI strings are English; `README.md` is Dutch and written for users.

## Done means

- `cargo check --all-targets` and `cargo test` pass. `cargo test` makes no provider calls, so run it freely.
- `cargo fmt -- <files>` on the files you touched only; the tree often holds other people's work in progress.
- `cargo clippy --all-targets` shows no warning your change introduced.
- Every visible control does real work. Mock conversations, fake rows, placeholder data and simulated providers are never acceptable, not even temporarily.
- New behaviour has a test at the lowest layer that proves it; a deterministic bug gets its failing test before the fix.
- A UI change renders each new state (empty, loading, error, disabled with reason) and was driven from the keyboard in a test. "Compiles" and "looks right" are not proof.

## Invariants that break silently

- GPUI is reached only through the `gpui_kit` facade; `gpui`, `gpui-component`, `gpui-base` and Zed crates stay out of `Cargo.toml`, and `gpui-kit` stays pinned with `=`. Look up APIs in the pinned source under `~/.cargo/registry/src/*/gpui-component-0.6.6/src` and `gpui-base-0.6.6/src`.
- Only `src/harness/omp.rs` knows OMP wire fields; only `src/factory/launch.rs` builds Factory argv.
- Child processes get separated argv, never a shell string, and never `--yolo`, `--ignore-capacity`, `--clean` or `--tui`.
- `unsafe` lives only in `src/platform.rs` or a transport module, each block with a `// SAFETY:` comment.
- Process, file and network I/O run on background executors, never in `render` or on the GPUI thread; results carry a revision or identity so stale ones are dropped.
- An unknown outcome stays `Unknown`. Exit 0 without a terminal event is not success, and nothing auto-retries, auto-resends or auto-submits, including after a crash or reconnect.
- Editor buffers stay out of project files and raw keys are never replayed; submitting needs a committed Draft revision.
- A `null` model means the harness default; leave it `null` rather than filling in a name.
- Each command is one `Action` whose label and disabled reason live in `src/ui/commands.rs`; palette, menu, button and key binding all dispatch that Action.
- The app shows missing or unauthenticated tools as setup issues in Settings and leaves installing, upgrading and logging in to the user.
- Add or upgrade crates only when the user asks, with `cargo add`, and commit `Cargo.lock`.

## Read first

- Changing anything under `src/ui/`: read `docs/agents/ui.md` and use the `gpui-kit-ui` skill (`.agents/skills/gpui-kit-ui/`, including `scripts/audit.sh`).
- Writing or reviewing Rust: follow `CODING_STANDARDS.md` (layering, errors, naming, test layers).
- Naming anything a user sees: use the terms in `CONTEXT.md`.
- Running `#[ignore]` window tests (`--features runtime-qa`): they need `DROSTJE_RUNTIME_FIXTURE`; `README.md` › Ontwikkelen has the throwaway-repo recipe. Tests only ever touch `mktemp -d` fixtures, never the user's repositories.

Keep this file short: add a line only when an agent can't find the fact by looking and every session needs it. Everything else goes behind a pointer above or into a check.

## Agent skills

### Issue tracker

GitHub Issues on `TimWillebrands/DROSTJE.` via `gh`; external PRs are a triage surface too. The Azure DevOps remote is a code mirror only. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` (glossary) and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
