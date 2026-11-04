# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase. This is a single-context repo.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root: the glossary. Its terms are binding for UI strings, the palette, menus, tooltips, dialogs, docs and code names.
- **`docs/adr/`**: read ADRs that touch the area you're about to work in.

If `docs/adr/` doesn't exist yet, **proceed silently**. Don't flag its absence; don't suggest creating it upfront. The `/domain-modeling` skill creates ADRs lazily when decisions actually get resolved, and adds terms to `CONTEXT.md` when they get settled.

## File structure

```
/
├── CONTEXT.md          ← glossary (English; README.md explains the terms to users in Dutch)
├── docs/adr/
│   └── 0001-<decision>.md
└── src/
```

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md`, spelled that way. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0007 (event-sourced orders) — but worth reopening because…_
