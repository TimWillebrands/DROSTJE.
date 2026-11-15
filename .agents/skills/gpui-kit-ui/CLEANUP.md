# Cleanup: replace hand-rolled UI with kit components

Run `scripts/audit.sh [dir]` first. Every hit is a lead; decide each one
against the smell table, then fix in the order below (largest class first).

## Smell → component

| Smell in code | Replace with | Why the div is a defect |
|---|---|---|
| `div().text_color(cx.theme().danger \| warning \| info)` holding an error / warning / caveat | `alert::Alert::{error,warning,info}(id, msg)`; `.banner()` when it spans a region; `.title()` for the object | Alert owns icon, role colour, close, layout; coloured prose encodes meaning by colour alone |
| `for notice in … { div().child(format!("Warning: {}", …)) }` | one `Alert` per notice, level from the domain enum | hand-written level prefix duplicates what the variant already says |
| Status word in coloured text (`"Failed"`, `"Running"`, `"Changes could not be saved"`), or status appended to a label with ` · ` | `tag::Tag::danger()` / `Tag::new()` (neutral) next to the object; `Label::new(name).secondary(state)` for captions | chips keep geometry stable across states; text colour flips are invisible to keyboard users |
| `format!("{key}: {value}")` rows, or a `v_flex` of `"Repository: …"`, `"Branch: …"` lines | `description_list::DescriptionList::new().item(label, value, span)` | alignment spine for labels; shared label width |
| Ghost `Button` per row in a collection; `h_flex` rows with `.hover()` / selected background | `list::ListItem::new(id).selected(b).on_click(..)` inside `list::List` (or `sidebar::SidebarMenuItem`, `tree::Tree`) | rows get j/k navigation, focus, selection, context menu, virtualisation |
| Show/hide `Button` + `.when(expanded, …)` with expansion state in the owner | `collapsible::Collapsible` / `accordion::Accordion` (keep open-state keyed by domain id) | disclosure owns keyboard toggle, chevron, animation |
| Glyph in a label: `"Advanced −"`, `"Advanced +"`, `">"`, `"⌄"` | `Button::new(id).label("Advanced").icon(IconName::ChevronDown \| ChevronRight)` | glyphs are not icons: wrong size, weight, colour, no fixed slot |
| Explanatory `div` above / error `div` under an input | `form::field().label(..).description(..)` and the field's validation slot | field owns label/control/help alignment |
| `div().text_xs()` caption under a title | `label::Label::new(title).secondary(caption)` or `.text_color(cx.theme().muted_foreground)` | one hierarchy level, not an ad-hoc size |
| `div().child("Loading…")` | `spinner::Spinner`, `skeleton::Skeleton`, or `Button::loading(true)` | loading is a distinct visible state |
| Hand-formatted `"⌘K"` / `"Cmd-Return"` | `kbd::Kbd::binding_for_action(..)`, `Tooltip::action(..)`, `Action` on `CommandItem` / menu item | hint and dispatch must not disagree |
| `.absolute().inset_0()` backdrop, or a `div` overlay with its own dismiss | `WindowExt::open_dialog` / `open_alert_dialog` / `open_sheet`, `Popover`, `DropdownMenu` | focus trap, Escape, restore-focus, stacking |
| Clickable `div` / `h_flex` with `on_click` | `Button` (`.ghost()` for quiet), or the row component | accessibility role, disabled state, focus ring, keyboard activation |
| `rgb(..)` / hex / `px(..)` for spacing or geometry | `cx.theme().<role>`, rem helpers, `cx.theme().radius`; a missing role is added to the theme setup, never at the call site | tokens make light/dark and zoom coherent |

Keep as-is: `on_mouse_down` on a pane root that only moves focus; `canvas()`
`.absolute()` measurement overlays; layout-only `div().flex_1().truncate()`;
`px(0.)` / `px(1.)` hairlines, `ListState` overdraw, and window minima or
external sizes named as a `const NAME: Pixels`.

## Fix pass

1. **Group by class, not by file.** One class (e.g. every `Alert` site) per commit; the diff reads as one decision.
2. **Read the component source** for the builders you will use (`COMPONENTS.md` gives the grep). Confirm the slot exists (e.g. does `Field` have a validation slot?) before rewriting around it.
3. **Extract a helper when the same composition appears three times** (`fn error_alert(id, msg)`), placed in the owning view module, not a shared "widgets" file.
4. **Preserve behaviour contracts:** the same `Action` still dispatches, `ElementId`s stay domain-stable, focus still lands where it did, disabled reasons still reach the tooltip.
5. **Verify** per the project's definition of done: check, fmt, tests, clippy count unchanged; exercise the state the component now owns (hover, focus, selected, disabled, loading, error) in both theme modes.
6. **Re-run the audit**; remaining hits are either fixed or listed in the report with the reason they stay.
