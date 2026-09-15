# gpui-kit 0.6.1 component map

Paths are `gpui_kit::component::<module>::<Type>` unless marked *root*
(`gpui_kit::component::<Type>`). Builders listed are the ones that decide
whether the component fits; verify the rest in the pinned source:

```sh
K=~/.cargo/registry/src/*/gpui-component-0.6.1/src   # B=~/.cargo/registry/src/*/gpui-base-0.6.1/src
grep -n "pub fn " $K/<module>.rs            # flat module
grep -rn "pub fn " $K/<module>/             # directory module
```

## Triggers and commands

| Need | Component | Decisive builders |
|---|---|---|
| Any command trigger | `button::Button::new(id)` | `.label()`, `.icon(IconName::…)`, `.tooltip()`, `.loading(bool)`, `.disabled(bool)`, `.compact()`, `.on_click()`; variants via `ButtonVariants`: `.primary()` `.secondary()` `.danger()` `.ghost()` `.outline()` `.link()` `.text()`; size via `Sizable`: `.xsmall()` `.small()` `.large()` |
| Button + menu | `button::DropdownButton` | `.outline()` |
| Pressed/toggle button | `button::Toggle`, `toggle_group` (base) | `.ghost()`, `.outline()` |
| External URL only | `link::Link::new(id)` | `.href()` (never for in-app commands) |
| Secondary actions | `menu::{DropdownMenu, ContextMenu, PopupMenu, PopupMenuItem}` | `PopupMenu::menu(label, Box<dyn Action>)`, `.menu_with_icon()`, `.menu_with_disabled()`, `.separator()`, `PopupMenuItem::new(label).on_click()`; `SidebarMenuItem::context_menu()` |
| Key hint | `kbd::Kbd::new(keystroke)`, `Kbd::binding_for_action(..)` | `.outline()`; `Tooltip::new(text).action(&action, Some(context))` shows binding itself |
| Palette | `command::{Command, CommandItem, CommandGroup, CommandState}` | `Command::new(&state)`, `.item()`, `.group()`, `.separator()`, `.on_confirm()`, `.on_cancel()`, `.empty()`, `.placeholder()`; `CommandItem::action` binds a real `Action` |
| Hover hint | `tooltip::Tooltip` (or `Button::tooltip`) | `.action()`, `.key_binding()` |

## Feedback, from smallest surface up

| Need | Component | Decisive builders |
|---|---|---|
| Tooltip | `tooltip::Tooltip::new(text)` | — |
| Floating content | `popover::Popover::new(id)` | `.trigger()`, `.anchor()`, `.open(bool)`, `.on_open_change()` |
| Menu | `menu::DropdownMenu` / `ContextMenu` | see above |
| Async status, no decision | `WindowExt::push_notification` (*root* `WindowExt`) | `notification::Notification::{info,success,warning,error}(msg)`, `.title()`, `.autohide(bool)` |
| Persistent in-task warning | `alert::Alert::{info,success,warning,error}(id, message)` | `.title()`, `.banner()`, `.visible(bool)`, `.on_close()` |
| Decision / short form | `WindowExt::open_dialog` → `dialog::Dialog::new(cx)` | `.title()`, `.content(builder)`, `.footer()`, `.on_ok()`, `.on_cancel()`, `.keyboard(bool)`, `.overlay_closable(bool)` |
| Irreversible confirm | `WindowExt::open_alert_dialog` → `dialog::AlertDialog::new(cx)` | `.title()`, `.description()`, `.confirm()`, `.button_props(DialogButtonProps::ok_text/ok_variant)` — name the object in the title |
| Supplementary panel | `WindowExt::open_sheet` / `open_sheet_at(Placement, ..)` → `sheet::Sheet::new(window, cx)` | `.title()`, `.footer()`, `.size()`, `.resizable(bool)`, `.on_close()` |
| Loading | `spinner::Spinner`, `skeleton::Skeleton`, `progress::Progress`, `Button::loading(true)` | — |

## Text and data

| Need | Component | Decisive builders |
|---|---|---|
| Caption / secondary | `label::Label::new(main).secondary(detail)` or `.text_color(cx.theme().muted_foreground)` | `.masked()`, `.highlights()` |
| State chip | `badge::Badge::new()` (`.dot()`, `.count(n)`, `.max()`), `tag::Tag::{new,primary,secondary,danger,success,warning,info}()` (`.outline()`) | neutral by default |
| Key/value | `description_list::{DescriptionList, DescriptionItem}` | `DescriptionList::{vertical,horizontal}()`, `.item(label, value, span)`, `.bordered(bool)`, `.columns(n)`, `.label_width()` |
| Grouped section | `group_box::GroupBox::new().title(..)` | `.id()`, `.content_style()` |
| Divider | `separator::Separator::{horizontal,vertical}()` | `.label()`, `.dashed()` |
| Markdown / HTML | `text::TextView::markdown(id, text)` / `::html` | `.markdown_extensions()` |
| Labelled fields | `form::{v_form, h_form, field}` | `field().label(..).description(..).required(bool).child(input)`, `.col_span()` |
| Text input | `input::{Input, InputState}`; `Textarea`, `NumberInput`, `OtpInput`; `input::Editor` for code | controlled: value in, intent out |
| Choice | `select::Select`, `combobox::Combobox`, `radio::Radio`/`RadioGroup`, `checkbox::Checkbox`, `switch::Switch`, `slider::Slider` | — |
| Chat surfaces | `message::Message`, `bubble::Bubble`, `marker::Marker`, `message_scroller::MessageScroller`, `attachment::Attachment` | tail-following scroll lives in `MessageScroller`, not a hand-rolled list |

## Collections and structure

| Need | Component | Decisive builders |
|---|---|---|
| Rows with selection | `list::{List, ListState, ListDelegate, ListItem}` | `ListState::new(delegate, window, cx)`, `List::new(&state)`; delegate owns `items_count`, `render_item`, `render_empty`, `render_loading`, `confirm`, `set_selected_index`; `ListItem::new(id).selected(b).disabled(b).suffix(..).on_click(..)` |
| Sidebar navigation | `sidebar::{Sidebar, SidebarMenu, SidebarMenuItem, SidebarGroup, SidebarHeader, SidebarFooter}` | `SidebarMenuItem::new(label).icon().active(bool).on_click().children(..).suffix().context_menu()`, `Sidebar::new(id).side().collapsible().header().footer()` |
| Hierarchy | `tree::Tree` | — |
| Tabs / segmented | `tab::{TabBar, Tab}` | `TabBar::new(id).{pill,outline,segmented,underline}().children(tabs)`, `Tab::new().label().icon().prefix()`; `Tab: From<&str>` |
| Long collections | `VirtualList` (*root*; `v_virtual_list`/`h_virtual_list`), `table::Table` | — |
| Panels / docks | `dock::DockArea` and `dock::*`, `resizable::*` | persist layout; clamp on restore |
| Window chrome | `TitleBar` (*root*), `status_bar::StatusBar::new().left(..).right(..)`, `Root` (*root*) | — |
| Settings UI | `setting::*` (`page`, `group`, `item`, `fields`) | — |
| Paging / steps | `pagination::Pagination`, `stepper::Stepper`, `accordion::Accordion`, `collapsible::Collapsible`, `breadcrumb::Breadcrumb` | — |

## Theme and styling helpers (*root*)

- `ActiveTheme` → `cx.theme().<role>`; roles: `background foreground border input ring muted muted_foreground primary primary_foreground secondary danger warning success info popover popover_foreground sidebar sidebar_foreground sidebar_accent list list_active list_hover tab tab_active status_bar title_bar group_box selection caret radius mono_font_family font_family font_size`.
- `Sizable`/`Size` (`XSmall Small Medium Large`), `Disableable`, `Selectable`, `StyledExt` (`h_flex()`, `v_flex()`, `rounded(cx.theme().radius)`), `IconName` + `Icon::new(..)`.
- `WindowExt` also has `has_active_dialog`, `close_dialog`, `has_active_sheet`, `close_sheet`, `focused_input`, `clear_notifications`.
