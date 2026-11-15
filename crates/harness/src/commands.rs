use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandSource {
    Builtin,
    Skill,
    Extension,
    Plugin,
    Custom,
    Template,
    File,
    McpPrompt,
    Other(String),
}

impl CommandSource {
    pub fn label(&self) -> &str {
        match self {
            Self::Builtin => "Builtin",
            Self::Skill => "Skill",
            Self::Extension => "Extension",
            Self::Plugin => "Plugin",
            Self::Custom => "Command",
            Self::Template => "Template",
            Self::File => "File",
            Self::McpPrompt => "MCP prompt",
            Self::Other(source) => source,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommand {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub argument_hint: Option<String>,
    pub source: CommandSource,
    pub blocked: Option<Cow<'static, str>>,
}

impl SlashCommand {
    pub fn answers_to(&self, name: &str) -> bool {
        self.name == name || self.aliases.iter().any(|alias| alias == name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CommandCatalog {
    #[default]
    Loading,
    Ready(Vec<SlashCommand>),
    Unavailable(String),
}

impl CommandCatalog {
    pub fn commands(&self) -> &[SlashCommand] {
        match self {
            Self::Ready(commands) => commands,
            Self::Loading | Self::Unavailable(_) => &[],
        }
    }

    pub fn invoked(&self, text: &str) -> Option<&SlashCommand> {
        let (name, _) = invocation(text)?;
        let commands = self.commands();
        commands
            .iter()
            .filter(|command| command.name == name)
            .max_by_key(|command| command.source == CommandSource::Builtin)
            .or_else(|| commands.iter().find(|command| command.answers_to(name)))
    }

    pub fn unavailable_reason(&self) -> Option<String> {
        match self {
            Self::Loading => Some("The harness has not listed its commands yet".into()),
            Self::Unavailable(reason) => Some(format!("Commands are unavailable: {reason}")),
            Self::Ready(commands) if commands.iter().all(|c| c.blocked.is_some()) => {
                Some("The harness offers no commands".into())
            }
            Self::Ready(_) => None,
        }
    }

    pub fn refusal(&self, text: &str) -> Option<String> {
        let command = self.invoked(text)?;
        let reason = command.blocked.as_deref()?;
        Some(format!(
            "/{} is not available in DROSTJE.: {reason}",
            command.name
        ))
    }
}

pub fn invocation(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    (!name.is_empty() && !name.contains('/')).then(|| (name, rest[end..].trim_start()))
}

pub(super) const NEW_CONVERSATION: &str = "start a new Conversation instead";
pub(super) const MODEL_PICKER: &str = "choose the model in the composer";

#[cfg(test)]
mod tests {
    use super::*;

    fn command(name: &str, aliases: &[&str], source: CommandSource) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            aliases: aliases.iter().map(|alias| (*alias).into()).collect(),
            description: String::new(),
            argument_hint: None,
            source,
            blocked: None,
        }
    }

    #[test]
    fn invocation_needs_a_leading_slash_and_a_name() {
        assert_eq!(
            invocation("/compact keep tests"),
            Some(("compact", "keep tests"))
        );
        assert_eq!(
            invocation("/skill:tdd\nfix it"),
            Some(("skill:tdd", "fix it"))
        );
        assert_eq!(invocation("/review"), Some(("review", "")));
        assert_eq!(invocation(" /compact"), None);
        assert_eq!(invocation("/"), None);
        assert_eq!(invocation("/ compact"), None);
        assert_eq!(invocation("/usr/bin/env is missing"), None);
        assert_eq!(invocation("please /compact"), None);
    }

    #[test]
    fn invoked_resolves_aliases_and_prefers_builtins() {
        let mut clear = command("clear", &["reset", "new"], CommandSource::Builtin);
        clear.blocked = Some(NEW_CONVERSATION.into());
        let catalog = CommandCatalog::Ready(vec![
            command("review", &[], CommandSource::Custom),
            command("review", &[], CommandSource::Builtin),
            clear,
        ]);
        assert_eq!(
            catalog.invoked("/review now").map(|c| &c.source),
            Some(&CommandSource::Builtin)
        );
        assert_eq!(
            catalog.invoked("/reset").map(|c| c.name.as_str()),
            Some("clear")
        );
        assert!(catalog.invoked("/unknown").is_none());
        assert_eq!(
            catalog.refusal("/new").as_deref(),
            Some("/clear is not available in DROSTJE.: start a new Conversation instead")
        );
        assert_eq!(catalog.refusal("/review"), None);
        assert_eq!(CommandCatalog::Loading.refusal("/clear"), None);
    }

    #[test]
    fn unavailable_reason_covers_every_empty_state() {
        assert!(CommandCatalog::Loading.unavailable_reason().is_some());
        assert_eq!(
            CommandCatalog::Unavailable("Unknown command".into())
                .unavailable_reason()
                .as_deref(),
            Some("Commands are unavailable: Unknown command")
        );
        assert!(
            CommandCatalog::Ready(Vec::new())
                .unavailable_reason()
                .is_some()
        );
        let mut clear = command("clear", &[], CommandSource::Builtin);
        clear.blocked = Some(NEW_CONVERSATION.into());
        assert!(
            CommandCatalog::Ready(vec![clear.clone()])
                .unavailable_reason()
                .is_some()
        );
        let compact = command("compact", &[], CommandSource::Builtin);
        assert_eq!(
            CommandCatalog::Ready(vec![clear, compact]).unavailable_reason(),
            None
        );
    }
}
