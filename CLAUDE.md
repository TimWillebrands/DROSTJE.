@AGENTS.md

## Claude Code

- Before changing anything under `src/ui/`, invoke the `gpui-kit-ui` skill (`.agents/skills/gpui-kit-ui/SKILL.md`) and read the "UI: keyboard-first" section above, then the nearest existing view and the kit component's source in the pinned crate. Do not infer a GPUI API from Zed's crates or older gpui examples.
- Before reporting UI work done, run `.agents/skills/gpui-kit-ui/scripts/audit.sh` on the files you touched and explain any remaining hit.
- Use plan mode for changes under `src/harness/`, `src/neovim/` and `src/factory/`: these are protocol boundaries specified in `DROST_APPLICATION_PLAN.md`.
- Run `cargo check --all-targets` after every step, `cargo test` before reporting done, and `cargo fmt` on touched files.
- Runtime tests that need Neovim, a real window or provider credentials are opt-in; say explicitly when you did not run them.
