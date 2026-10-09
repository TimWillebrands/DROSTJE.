//! Composer slash-command completion: which harness commands match the draft, the caret
//! popup that offers them, and the row every picker shows for one.
use crate::harness::{CommandCatalog, SlashCommand};
use crate::model::ThreadId;
use gpui_kit::component::{
    ActiveTheme, Sizable, ThemeStyled, h_flex, list::ListItem, tag::Tag, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

/// The partial command name being typed: the draft starts with `/` and the cursor is still
/// inside that first word. `cursor` is `(row, utf8 byte column)`.
pub fn completion_query(draft: &str, cursor: (usize, usize)) -> Option<&str> {
    if cursor.0 != 0 {
        return None;
    }
    let line = draft.split('\n').next().unwrap_or("");
    let rest = line.strip_prefix('/')?;
    let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let typed = rest.get(..cursor.1.checked_sub(1)?)?;
    (cursor.1 <= 1 + word && !typed.contains('/')).then_some(typed)
}

/// Commands DROSTJE. may send whose name, alias or description matches `query`, best first:
/// name prefix, then alias prefix, then any other name match, then description matches.
/// Skills and user commands sort ahead of builtins within each rank.
pub fn matches<'a>(catalog: &'a CommandCatalog, query: &str) -> Vec<&'a SlashCommand> {
    let query = query.to_lowercase();
    let mut ranked: Vec<_> = catalog
        .commands()
        .iter()
        .filter(|command| command.blocked.is_none())
        .filter_map(|command| {
            let name = command.name.to_lowercase();
            let rank = if name.starts_with(&query) {
                0
            } else if command
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase().starts_with(&query))
            {
                1
            } else if name.contains(&query) {
                2
            } else if command.description.to_lowercase().contains(&query) {
                3
            } else {
                return None;
            };
            let builtin = command.source == crate::harness::CommandSource::Builtin;
            Some(((rank, builtin), command))
        })
        .collect();
    ranked.sort_by(|(a, left), (b, right)| a.cmp(b).then_with(|| left.name.cmp(&right.name)));
    ranked.into_iter().map(|(_, command)| command).collect()
}

/// One command: `/name` and its argument syntax in mono, the description, and its source.
pub fn row(command: &SlashCommand, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .font_family(theme.mono_font_family.clone())
                .child(format!("/{}", command.name))
                .when_some(command.argument_hint.clone(), |row, hint| {
                    row.child(div().text_color(theme.muted_foreground).child(hint))
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(command.description.clone()),
        )
        .child(
            Tag::secondary()
                .xsmall()
                .flex_none()
                .child(command.source.label().to_owned()),
        )
}

/// Rows shown at once; the window follows the selection instead of scrolling.
const VISIBLE_ROWS: usize = 8;

pub enum SlashCompletionEvent {
    Accept(String),
}

/// The caret popup's model: the commands matching what is typed after `/`, or why there
/// are none. The prompt editor owns it and decides when it is shown.
pub struct SlashCompletion {
    thread: ThreadId,
    query: String,
    commands: Vec<SlashCommand>,
    status: Option<String>,
    selected_ix: usize,
}

impl EventEmitter<SlashCompletionEvent> for SlashCompletion {}

impl SlashCompletion {
    pub fn new(thread: ThreadId) -> Self {
        Self {
            thread,
            query: String::new(),
            commands: Vec::new(),
            status: None,
            selected_ix: 0,
        }
    }

    /// Refilters for `query`, keeping the selected command while it still matches.
    pub fn set(
        &mut self,
        thread: ThreadId,
        query: &str,
        catalog: &CommandCatalog,
        cx: &mut Context<Self>,
    ) {
        let unchanged = self.thread == thread && self.query == query;
        let selected = self.selected().map(|command| command.name.clone());
        let commands: Vec<SlashCommand> = matches(catalog, query).into_iter().cloned().collect();
        let status = catalog.unavailable_reason().or_else(|| {
            commands
                .is_empty()
                .then(|| format!("No commands match /{query}"))
        });
        if unchanged && commands == self.commands && status == self.status {
            return;
        }
        self.selected_ix = selected
            .filter(|_| self.thread == thread)
            .and_then(|name| commands.iter().position(|command| command.name == name))
            .unwrap_or(0);
        self.thread = thread;
        self.query = query.to_owned();
        self.commands = commands;
        self.status = status;
        cx.notify();
    }

    pub fn thread(&self) -> ThreadId {
        self.thread
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> Option<&SlashCommand> {
        self.commands.get(self.selected_ix)
    }

    /// Moves the selection by `delta`, stopping at either end like the app's other lists.
    pub fn select(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.commands.len().saturating_sub(1);
        let next = self.selected_ix.saturating_add_signed(delta).min(last);
        if next != self.selected_ix {
            self.selected_ix = next;
            cx.notify();
        }
    }
}

impl Render for SlashCompletion {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let start = self
            .selected_ix
            .saturating_sub(VISIBLE_ROWS - 1)
            .min(self.commands.len().saturating_sub(VISIBLE_ROWS));
        let hidden = self.commands.len().saturating_sub(start + VISIBLE_ROWS);
        v_flex()
            .id("slash-completion")
            .test_support()
            .w(rems(36.))
            .p_1()
            .gap_0p5()
            .popover_style(cx)
            .font_family(cx.theme().font_family.clone())
            .text_sm()
            .when_some(self.status.clone(), |popup, status| {
                popup.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_color(cx.theme().muted_foreground)
                        .child(status),
                )
            })
            .children(
                self.commands
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(VISIBLE_ROWS)
                    .map(|(ix, command)| {
                        let name = command.name.clone();
                        ListItem::new(SharedString::from(format!("slash-command-{name}")))
                            .selected(ix == self.selected_ix)
                            .px_2()
                            .py_0p5()
                            .child(row(command, cx))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(SlashCompletionEvent::Accept(name.clone()));
                            }))
                    }),
            )
            .when(hidden > 0, |popup| {
                popup.child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{hidden} more")),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{completion_query, matches};
    use crate::harness::{CommandCatalog, CommandSource, SlashCommand};

    fn command(
        name: &str,
        aliases: &[&str],
        description: &str,
        source: CommandSource,
    ) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            aliases: aliases.iter().map(|alias| (*alias).into()).collect(),
            description: description.into(),
            argument_hint: None,
            source,
            blocked: None,
        }
    }

    #[test]
    fn completion_query_follows_the_cursor_inside_the_first_word() {
        assert_eq!(completion_query("/", (0, 1)), Some(""));
        assert_eq!(completion_query("/com", (0, 4)), Some("com"));
        assert_eq!(completion_query("/com", (0, 2)), Some("c"));
        assert_eq!(completion_query("/compact now", (0, 8)), Some("compact"));
        assert_eq!(completion_query("/compact now", (0, 9)), None);
        assert_eq!(completion_query("/com", (0, 0)), None);
        assert_eq!(completion_query("/com\n", (1, 0)), None);
        assert_eq!(completion_query("hello /com", (0, 10)), None);
        assert_eq!(completion_query("/usr/bin", (0, 8)), None);
    }

    #[test]
    fn matches_rank_prefixes_first_and_hide_blocked_commands() {
        let mut clear = command(
            "clear",
            &["new"],
            "Start a new session",
            CommandSource::Builtin,
        );
        clear.blocked = Some("start a new Conversation instead");
        let catalog = CommandCatalog::Ready(vec![
            command(
                "compact",
                &[],
                "Compact the conversation",
                CommandSource::Builtin,
            ),
            command("commit", &[], "Write a commit", CommandSource::Custom),
            command(
                "usage",
                &["cost"],
                "Show session cost",
                CommandSource::Builtin,
            ),
            command(
                "skill:tdd",
                &[],
                "Test-driven development",
                CommandSource::Skill,
            ),
            clear,
        ]);
        let names = |query| -> Vec<_> {
            matches(&catalog, query)
                .into_iter()
                .map(|command| command.name.as_str())
                .collect()
        };
        assert_eq!(names("com"), ["commit", "compact"]);
        assert_eq!(names("cos"), ["usage"]);
        assert_eq!(names("tdd"), ["skill:tdd"]);
        assert_eq!(names("session"), ["usage"]);
        assert!(names("new").is_empty());
        assert_eq!(names("").len(), 4);
    }
}
