# Glossary

Use these terms, spelled this way, in UI strings, the palette, menus, tooltips, dialogs, docs and code names. `README.md` explains them to users in Dutch.

| Term | Meaning |
|---|---|
| **Space** | A work context grouping Projects, with its own open tabs, selection and panel layout. *Default* always exists. |
| **Project** | A local Git repository added to DROSTJE. It can belong to several Spaces. |
| **Thread** | A Conversation or a Factory thread. Belongs to exactly one Project and one Space. |
| **Conversation** | An interactive session with one harness: OMP, Pi, Claude Code, Codex or OpenCode. |
| **Factory thread** | The configuration and history of Factory work for one Thread; read-only once launched. |
| **Draft** | A Conversation's unsent prompt, kept in the composer and persisted. Only a committed revision is submitted. |
| **Turn** | One prompt and the harness's work on it until it reports an end. |
| **Launch** | DROSTJE's durable record of starting the `factory` CLI for a Factory thread (`FactoryLaunch`). |
| **Run** | The Factory-side work a Launch starts, followed through its event file. |
| **Attempt** | The Factory attempt a Launch is linked to (`FactoryLaunch::attempt_id`). |
| **Task** | One unit of work inside a Run, as Factory reports it (`TaskSnapshot`). |
| **Worktree** | A Git worktree DROSTJE created for Conversations, on its own `drostje/<name>` branch. |
| **Host** | A remote machine that runs Conversations, reached through the SEINTJE relay. |
| **Phone** | An Android device paired with a QR code that follows Spaces, Threads and the opened Conversation and can send it prompts, over an ntfy server with end-to-end encryption. Questions from the agent are answered on the computer. |
| **Region** | A keyboard focus area of the workspace: Projects or Thread. |
| **Changes** | The sidebar tab showing a Thread's uncommitted working-tree diff. |
