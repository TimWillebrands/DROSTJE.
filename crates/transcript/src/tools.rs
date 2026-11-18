use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Shell,
    Read,
    Write,
    Edit,
    Grep,
    Glob,
    Todo,
    Task,
    Eval,
    Ask,
    WebSearch,
    WebFetch,
    Other,
}

impl ToolKind {
    pub fn of(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "bash" => Self::Shell,
            "read" => Self::Read,
            "write" => Self::Write,
            "edit" => Self::Edit,
            "grep" => Self::Grep,
            "glob" => Self::Glob,
            "todo" => Self::Todo,
            "task" => Self::Task,
            "eval" => Self::Eval,
            "ask" => Self::Ask,
            "web_search" | "websearch" => Self::WebSearch,
            "webfetch" => Self::WebFetch,
            _ => Self::Other,
        }
    }
}

pub fn has_own_card(name: &str) -> bool {
    name == "todo" || name.ends_with("show_artifact")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolOutcome {
    Running,
    Succeeded,
    Failed,
    #[serde(other)]
    Unknown,
}

impl ToolOutcome {
    pub fn recorded(error: bool, exit_code: Option<i64>) -> Self {
        if error || exit_code.is_some_and(|code| code != 0) {
            Self::Failed
        } else {
            Self::Succeeded
        }
    }

    pub fn pending(active: bool) -> Self {
        if active { Self::Running } else { Self::Unknown }
    }
}

pub fn result_language<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    match ToolKind::of(name) {
        ToolKind::Shell | ToolKind::Eval | ToolKind::Grep | ToolKind::Glob => Some("text"),
        ToolKind::Read | ToolKind::Write | ToolKind::Edit => Some(
            path_field(input)
                .and_then(|path| path.rsplit_once('.'))
                .map(|(_, extension)| extension)
                .filter(|extension| {
                    !extension.is_empty() && extension.chars().all(char::is_alphanumeric)
                })
                .unwrap_or("text"),
        ),
        ToolKind::Todo
        | ToolKind::Task
        | ToolKind::Ask
        | ToolKind::WebSearch
        | ToolKind::WebFetch
        | ToolKind::Other => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSummary {
    pub kind: ToolKind,
    pub body: String,
}

pub const SUMMARY_LIMIT: usize = 200;

pub fn text_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input[key].as_str().filter(|value| !value.trim().is_empty())
}

fn path_field(input: &Value) -> Option<&str> {
    text_field(input, "path").or_else(|| text_field(input, "file_path"))
}

fn compact_json(input: &Value) -> String {
    const PREFIX_BYTES: usize = (SUMMARY_LIMIT + 1) * 4;
    struct Prefix(Vec<u8>);
    impl std::io::Write for Prefix {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let room = PREFIX_BYTES - self.0.len();
            if room == 0 {
                return Err(std::io::Error::other("summary prefix is complete"));
            }
            let taken = room.min(bytes.len());
            self.0.extend_from_slice(&bytes[..taken]);
            Ok(taken)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let serialized;
    let text = match input {
        Value::Null => "",
        Value::String(text) => text.as_str(),
        other => {
            let mut prefix = Prefix(Vec::with_capacity(PREFIX_BYTES));
            let _ = serde_json::to_writer(&mut prefix, other);
            serialized = match String::from_utf8(prefix.0) {
                Ok(text) => text,
                Err(error) => {
                    let valid = error.utf8_error().valid_up_to();
                    let mut bytes = error.into_bytes();
                    bytes.truncate(valid);
                    String::from_utf8(bytes).unwrap_or_default()
                }
            };
            serialized.as_str()
        }
    };
    match text.char_indices().nth(SUMMARY_LIMIT) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

fn patch_paths(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .filter(|line| line.starts_with('[') && line.ends_with(']'))
        .map(|line| {
            let inner = &line[1..line.len() - 1];
            inner.rsplit_once('#').map_or(inner, |(path, _)| path)
        })
        .filter(|path| !path.is_empty())
        .collect()
}

pub fn tool_summary(name: &str, input: &Value) -> ToolSummary {
    let kind = ToolKind::of(name);
    let body = match kind {
        ToolKind::Shell => text_field(input, "command").map(str::to_owned),
        ToolKind::Read | ToolKind::Write => path_field(input).map(str::to_owned),
        ToolKind::Edit => text_field(input, "input")
            .map(patch_paths)
            .filter(|paths| !paths.is_empty())
            .map(|paths| paths.join("\n"))
            .or_else(|| path_field(input).map(str::to_owned)),
        ToolKind::Grep => text_field(input, "pattern").map(|pattern| match path_field(input) {
            Some(path) => format!("{pattern}  in {path}"),
            None => pattern.to_owned(),
        }),
        ToolKind::Glob => text_field(input, "pattern")
            .or_else(|| path_field(input))
            .map(str::to_owned),
        ToolKind::Todo => crate::todo::describe_change(input),
        ToolKind::Task => input["tasks"]
            .as_array()
            .map(|tasks| match tasks.len() {
                1 => "1 task".to_owned(),
                n => format!("{n} tasks"),
            })
            .or_else(|| text_field(input, "description").map(str::to_owned)),
        ToolKind::Eval => text_field(input, "title")
            .or_else(|| text_field(input, "language"))
            .map(str::to_owned),
        ToolKind::Ask => input["questions"][0]["question"]
            .as_str()
            .or_else(|| text_field(input, "question"))
            .map(str::to_owned),
        ToolKind::WebSearch => text_field(input, "query").map(str::to_owned),
        ToolKind::WebFetch => text_field(input, "url").map(str::to_owned),
        ToolKind::Other => None,
    };
    ToolSummary {
        kind,
        body: body.unwrap_or_else(|| compact_json(input)),
    }
}

pub fn summary_line(body: &str) -> String {
    let mut lines = body.lines().filter(|line| !line.trim().is_empty());
    let first = lines.next().unwrap_or_default().trim();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SUMMARY_LIMIT, ToolKind, ToolOutcome, ToolSummary, Value, compact_json, has_own_card,
        result_language, summary_line, tool_summary,
    };
    use serde_json::json;

    #[test]
    fn collapsed_tools_show_one_line_of_the_call() {
        assert_eq!(summary_line("cargo test"), "cargo test");
        assert_eq!(summary_line("\n  src/a.rs\nsrc/b.rs"), "src/a.rs …");
        assert_eq!(summary_line(""), "");
    }

    #[test]
    fn plain_output_tools_get_a_code_block_in_the_file_language() {
        assert_eq!(result_language("bash", &Value::Null), Some("text"));
        assert_eq!(
            result_language("read", &json!({"path":"src/main.rs"})),
            Some("rs")
        );
        assert_eq!(
            result_language("read", &json!({"path":"Makefile"})),
            Some("text")
        );
        assert_eq!(
            result_language("write", &json!({"path":"notes.tar.gz"})),
            Some("gz")
        );
        assert_eq!(result_language("task", &Value::Null), None);
        assert_eq!(result_language("web_search", &Value::Null), None);
    }

    #[test]
    fn compact_json_stops_serializing_at_the_cut() {
        let whole = |input: &Value| {
            let text = match input {
                Value::Null => String::new(),
                Value::String(text) => text.clone(),
                other => serde_json::to_string(other).unwrap(),
            };
            if text.chars().count() > SUMMARY_LIMIT {
                let cut: String = text.chars().take(SUMMARY_LIMIT).collect();
                format!("{cut}…")
            } else {
                text
            }
        };
        let wide = "é".repeat(SUMMARY_LIMIT);
        let emoji = "🦀".repeat(3 * SUMMARY_LIMIT);
        for input in [
            Value::Null,
            json!("short"),
            json!("x".repeat(SUMMARY_LIMIT)),
            json!("x".repeat(SUMMARY_LIMIT + 1)),
            json!({"op": "cancel", "ids": ["bg_7"]}),
            json!({"text": wide}),
            json!({"text": emoji}),
            json!({"rows": (0..10_000).collect::<Vec<_>>()}),
            json!([wide, "x".repeat(SUMMARY_LIMIT * 8)]),
        ] {
            assert_eq!(compact_json(&input), whole(&input), "{input:.40}");
        }
    }

    #[test]
    fn tool_summary_prefers_command_path_pattern_and_falls_back_to_json() {
        assert_eq!(
            tool_summary("bash", &json!({"command":"ls -la","cwd":"/tmp","i":"List"})),
            ToolSummary {
                kind: ToolKind::Shell,
                body: "ls -la".into()
            }
        );
        assert_eq!(
            tool_summary("read", &json!({"path":"src/main.rs"})).body,
            "src/main.rs"
        );
        assert_eq!(
            tool_summary(
                "edit",
                &json!({"input":"*** Begin Patch\n[src/a.rs#69AE]\nPUT >1:\n+x\n[src/b.rs]\n"})
            ),
            ToolSummary {
                kind: ToolKind::Edit,
                body: "src/a.rs\nsrc/b.rs".into()
            }
        );
        assert_eq!(
            tool_summary("grep", &json!({"pattern":"fn main","path":"src"})).body,
            "fn main  in src"
        );
        assert_eq!(
            tool_summary("ask", &json!({"questions":[{"question":"Which one?"}]})),
            ToolSummary {
                kind: ToolKind::Ask,
                body: "Which one?".into()
            }
        );
        assert_eq!(
            tool_summary("Read", &json!({"file_path":"src/effects/pools.ts"})),
            ToolSummary {
                kind: ToolKind::Read,
                body: "src/effects/pools.ts".into()
            }
        );
        assert_eq!(
            tool_summary("Bash", &json!({"command":"ls","description":"List"})).body,
            "ls"
        );
        assert_eq!(
            tool_summary("WebFetch", &json!({"url":"https://example.org"})),
            ToolSummary {
                kind: ToolKind::WebFetch,
                body: "https://example.org".into()
            }
        );
        let unknown = tool_summary("hub", &json!({"op":"cancel","ids":["bg_7"]}));
        assert_eq!(unknown.kind, ToolKind::Other);
        assert_eq!(
            serde_json::from_str::<Value>(&unknown.body).unwrap(),
            json!({"op":"cancel","ids":["bg_7"]})
        );
        assert_eq!(tool_summary("bash", &Value::Null).body, "");
        let long = tool_summary("other", &json!({"text": "x".repeat(500)}));
        assert!(long.body.ends_with('…'));
        assert_eq!(long.body.chars().count(), SUMMARY_LIMIT + 1);
    }

    #[test]
    fn todo_and_artifact_calls_keep_their_own_card() {
        assert!(has_own_card("todo"));
        for artifact in [
            "show_artifact",
            "mcp__drostje__show_artifact",
            "drostje_show_artifact",
        ] {
            assert!(has_own_card(artifact), "{artifact}");
        }
        assert!(!has_own_card("bash"));
        assert!(!has_own_card("Todo"));
    }

    #[test]
    fn an_outcome_is_failed_on_error_or_nonzero_exit_and_unknown_once_idle() {
        assert_eq!(ToolOutcome::recorded(false, None), ToolOutcome::Succeeded);
        assert_eq!(
            ToolOutcome::recorded(false, Some(0)),
            ToolOutcome::Succeeded
        );
        assert_eq!(ToolOutcome::recorded(false, Some(2)), ToolOutcome::Failed);
        assert_eq!(ToolOutcome::recorded(true, None), ToolOutcome::Failed);
        assert_eq!(ToolOutcome::pending(true), ToolOutcome::Running);
        assert_eq!(ToolOutcome::pending(false), ToolOutcome::Unknown);
    }

    #[test]
    fn an_outcome_from_a_newer_desktop_reads_as_unknown() {
        let read = |json: &str| serde_json::from_str::<ToolOutcome>(json).unwrap();
        assert_eq!(read(r#""Failed""#), ToolOutcome::Failed);
        assert_eq!(read(r#""Cancelled""#), ToolOutcome::Unknown);
    }
}
