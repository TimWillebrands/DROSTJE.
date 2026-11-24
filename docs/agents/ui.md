# DROSTJE. UI contract

Read this before changing `src/ui/`. Generic component and cleanup rules live in the `gpui-kit-ui` skill (`.agents/skills/gpui-kit-ui/`); where the two differ, this file wins.

## Shell and look

- Workspace shell: Projects left, thread tabs centre (a lone thread still shows its tab), one sidebar per thread with tabs (Changes, Details, Review, Output, Terminal, Agents, Browser). Native TitleBar and dock headers. Save and error status live in the title bar; there is no footer or permanent Factory sidebar.
- Persist open thread identities and user-controlled layout; clamp restored sizes so the Thread keeps usable space.
- Theme roles are added in the theme setup (`src/ui/theme/mod.rs`), never at a call site.
- Focus and selection use subtle, theme-derived neutral background or border changes. Keyboard focus, the open selection and inactive editors stay visibly distinct from each other. Existing pattern: a 2px left border in `muted_foreground` plus `list_active` background for the focused cursor, a `border`-coloured left border when the list is unfocused.
- Monospace (`cx.theme().mono_font_family`) only for code, identifiers, shortcuts and aligned numbers.
- Keep frames cheap: virtualise history, diffs and logs with `list`, deliver events in bounded batches, update rows in place instead of rebuilding the conversation per token. A long `list` gets `.vertical_scrollbar(&state)` plus `conversation::estimate_unmeasured_rows` so the thumb spans unmeasured rows.

## Keyboard

The mouse is optional: every screen works end to end from the keyboard, and the palette (`Cmd-Shift-P`) reaches every command. Hover may reveal a shortcut, never the only copy of an action.

- **Lists are Vim lists.** Every list, tree and picker answers `j`/`k` and arrows, `gg`/`G`, page keys and Return; trees also `h`/`l`. Give the list root `key_context("List …")` plus its own context, `track_focus`, and `on_action` for `SelectNext`, `SelectPrevious`, `SelectFirst`, `SelectLast`, `SelectPageUp`, `SelectPageDown`. Selection and focus are both visible and distinct.
- **Panes are Vim windows.** `Cmd-Alt-h/j/k/l` and `Cmd-Alt`-arrows move from any surface, native inputs included, to the nearest visible pane beyond that edge that overlaps on the other axis. Focus stays put when there is no neighbour; it never wraps, jumps diagonally or skips a pane. Projects is reached from its adjacent thread only.
- **Register focusable sidebar tabs in `Workspace::sidebar_focus_for`** (`src/ui/workspace/sidebar_tabs.rs`). Navigation and sidebar commands use the same target to detect focus and enter the tab. On return to a Conversation, restore focus to its prompt if it owned focus before the handoff.
- **Bindings are context-scoped** (`Workspace`, `List`, `Thread`, `PromptEditor`, `NativeInput`, `Command`, `Dialog`, `Sheet`, plus surface contexts such as `ChangesList`); `key_context` and `on_action` sit on the same focused element. A deeper context's binding wins, which is how a surface overrides `enter` or adds single-letter keys. New bindings leave native text editing and list motions alone.
- **Tooltips show the bound key** through `tooltip_with_action(label, &Action, Some(context))`; shortcuts are never formatted by hand.
- **Fixed bindings** (`Cmd` is GPUI `secondary-`: Cmd on macOS, Ctrl on Linux; on Linux `Cmd-W`/`Cmd-V` are `Ctrl-Shift-W`/`V` because `Ctrl-W` and `Ctrl-V` belong to editors and shells): `Cmd-Alt-h/j/k/l` panes, `Cmd-Shift-P` palette, `Cmd-1/2/3` focus Projects/Thread/Output, `Cmd-B/J` toggle Projects/Output, `Cmd-N` new Conversation, `Cmd-Shift-N` new Factory thread, `Ctrl-Tab`/`Ctrl-Shift-Tab` cycle tabs, `Cmd-W` close tab, `Cmd-,` Settings, `Cmd-Return` submit or confirm, `Cmd-.` interrupt the turn, `i` from a Conversation focuses its composer.
- **Draft safety.** `Cmd-Escape` leaves the editor. Uncommitted IME text stays separate recovery text; submit and replace act only on a committed revision.
- **Layers.** Escape dismisses the topmost layer and restores the previous focus. Overlays trap focus; nested overlays dismiss from the top, one overlay at a time. Pickers are transient, centred and keyboard-driven, in place of OS dialogs.

## Feedback and copy

- Use the smallest surface that fits: tooltip, popover, menu, notification, dialog, alert dialog, sheet.
- Activation gives instant feedback; submit is disabled while in flight; user input is kept; the result appears next to the object.
- Reversible actions get undo or a notification; irreversible ones get an `AlertDialog` that names the object.
- Disabled controls say why, from the command's disabled reason.
- Copy is sentence case with no trailing period on labels, `…` on commands that need more input, verbs for commands and nouns for destinations. Errors say what happened and the next action. Write the plain result ("Committed 3 files") in place of `Are you sure…` or `successfully`.
