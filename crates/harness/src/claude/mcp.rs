use super::TurnOutcome;
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

const ERROR_CHARS: usize = 300;
const USAGE: &str = "Usage: `/mcp [reconnect|enable|disable [<server>|all]]`. Without a server name the action applies to all servers.";

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Output {
    pub text: String,
    pub outcome: TurnOutcome,
}

impl Output {
    fn done(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            outcome: TurnOutcome::Completed,
        }
    }

    fn failed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            outcome: TurnOutcome::Failed,
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Step {
    Wait,
    Send(Vec<Value>),
    Done(Output),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Reconnect,
    Enable,
    Disable,
}

impl Change {
    fn request(self, server: &str) -> Value {
        match self {
            Self::Reconnect => json!({"subtype":"mcp_reconnect","serverName":server}),
            Self::Enable => json!({"subtype":"mcp_toggle","serverName":server,"enabled":true}),
            Self::Disable => json!({"subtype":"mcp_toggle","serverName":server,"enabled":false}),
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Reconnect => "reconnect",
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }

    fn past(self) -> &'static str {
        match self {
            Self::Reconnect => "Reconnected",
            Self::Enable => "Enabled",
            Self::Disable => "Disabled",
        }
    }

    fn took_effect(self, status: &str) -> bool {
        match self {
            Self::Reconnect => status == "connected",
            Self::Enable => status != "disabled",
            Self::Disable => status == "disabled",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Target {
    All,
    Named(String),
}

#[derive(Debug, PartialEq, Eq)]
enum Request {
    Status,
    Change(Change, Target),
}

fn parse(arguments: &str) -> Result<Request, Output> {
    let arguments = arguments.trim();
    let (action, rest) = arguments
        .split_once(char::is_whitespace)
        .map(|(action, rest)| (action, rest.trim()))
        .unwrap_or((arguments, ""));
    let change = match action.to_lowercase().as_str() {
        "" | "status" | "list" => return Ok(Request::Status),
        "help" => return Err(Output::done(USAGE)),
        "reconnect" => Change::Reconnect,
        "enable" => Change::Enable,
        "disable" => Change::Disable,
        _ => {
            return Err(Output::failed(format!(
                "\"{action}\" isn't a `/mcp` action. {USAGE}"
            )));
        }
    };
    let target = match rest {
        "" | "all" => Target::All,
        name => Target::Named(name.to_owned()),
    };
    Ok(Request::Change(change, target))
}

#[derive(Debug)]
struct Server {
    name: String,
    status: String,
    error: Option<String>,
    tools: Option<usize>,
}

fn servers(answer: &Value) -> Option<Vec<Server>> {
    let rows = answer["mcpServers"].as_array()?;
    Some(
        rows.iter()
            .filter_map(|row| {
                Some(Server {
                    name: row["name"].as_str().filter(|name| !name.is_empty())?.into(),
                    status: row["status"].as_str().unwrap_or("unknown").into(),
                    error: row["error"].as_str().map(brief),
                    tools: row["tools"].as_array().map(Vec::len),
                })
            })
            .collect(),
    )
}

fn describe(server: &Server) -> String {
    let mut text = match server.status.as_str() {
        "connected" => "connected",
        "cached" => "cached (connects on first use)",
        "pending" => "connecting",
        "disabled" => "disabled",
        "failed" => "not connected",
        "needs-auth" => "needs authentication",
        "needs-approval" => "pending approval",
        other => other,
    }
    .to_owned();
    if let Some(tools) = server.tools.filter(|_| server.status == "connected") {
        text.push_str(&format!(", {tools} tool{}", plural(tools)));
    }
    if let Some(error) = &server.error {
        text.push_str(&format!(": {error}"));
    }
    text
}

fn brief(error: &str) -> String {
    let line = error.trim().lines().next().unwrap_or_default();
    match line.char_indices().nth(ERROR_CHARS) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn status_report(servers: &[Server]) -> Output {
    if servers.is_empty() {
        return Output::done(format!("No MCP servers are configured.\n\n{USAGE}"));
    }
    let mut text = format!("{} MCP server{}:\n", servers.len(), plural(servers.len()));
    for server in servers {
        text.push_str(&format!("\n- `{}`: {}", server.name, describe(server)));
    }
    text.push_str(&format!("\n\n{USAGE}"));
    Output::done(text)
}

fn plan(
    change: Change,
    target: &Target,
    servers: &[Server],
) -> Result<(Vec<String>, Option<String>), Output> {
    let approval = |name: &str| {
        Output::failed(format!(
            "`{name}` is pending approval. Approve it with `/mcp` in a Claude Code terminal first."
        ))
    };
    let name = match target {
        Target::All => {
            if servers.is_empty() {
                return Err(Output::done("No MCP servers are configured."));
            }
            let disabled = servers.iter().filter(|s| s.status == "disabled").count();
            let (targets, note, none): (Vec<&Server>, _, _) = match change {
                Change::Reconnect => (
                    servers
                        .iter()
                        .filter(|s| matches!(s.status.as_str(), "failed" | "needs-auth"))
                        .collect(),
                    (disabled > 0).then(|| {
                        format!(
                            "{disabled} disabled MCP server{} left alone; `/mcp enable all` brings them back.",
                            plural(disabled)
                        )
                    }),
                    "All enabled MCP servers are already connected or connecting. To reconnect one anyway, run `/mcp reconnect <server>`.",
                ),
                Change::Enable => (
                    servers.iter().filter(|s| s.status == "disabled").collect(),
                    None,
                    "All MCP servers are already enabled.",
                ),
                Change::Disable => (
                    servers
                        .iter()
                        .filter(|s| !matches!(s.status.as_str(), "disabled" | "needs-approval"))
                        .collect(),
                    None,
                    "All MCP servers are already disabled.",
                ),
            };
            if targets.is_empty() {
                let text = match note {
                    Some(note) => format!("{none} {note}"),
                    None => none.to_owned(),
                };
                return Err(Output::done(text));
            }
            return Ok((targets.iter().map(|s| s.name.clone()).collect(), note));
        }
        Target::Named(name) => name,
    };
    let Some(server) = servers.iter().find(|server| server.name == *name) else {
        return Err(Output::failed(format!(
            "There's no MCP server named `{name}`. Run `/mcp` to list the servers."
        )));
    };
    match (change, server.status.as_str()) {
        (_, "needs-approval") => Err(approval(name)),
        (Change::Reconnect, "disabled") => Err(Output::failed(format!(
            "`{name}` is disabled. Run `/mcp enable {name}` to bring it back."
        ))),
        (Change::Reconnect, "pending") => Err(Output::done(format!(
            "`{name}` is already connecting; retries can take a few minutes when a server keeps failing."
        ))),
        (Change::Enable, "failed" | "needs-auth") => Err(Output::done(format!(
            "`{name}` is already enabled but not connected ({}). Run `/mcp reconnect {name}` to retry.",
            describe(server)
        ))),
        (Change::Enable, status) if status != "disabled" => {
            Err(Output::done(format!("`{name}` is already enabled.")))
        }
        (Change::Disable, "disabled") => {
            Err(Output::done(format!("`{name}` is already disabled.")))
        }
        _ => Ok((vec![name.clone()], None)),
    }
}

fn change_report(
    change: Change,
    answers: &[(String, Option<Result<(), String>>)],
    after: Option<&[Server]>,
    note: Option<&str>,
) -> Output {
    let mut took_effect = 0;
    let mut unknown = 0;
    let mut lines = Vec::new();
    for (name, answer) in answers {
        let line = match answer {
            Some(Err(error)) => format!("couldn't {}: {error}", change.verb()),
            None => {
                unknown += 1;
                format!("Claude Code did not answer the {} request", change.verb())
            }
            Some(Ok(())) => match after.and_then(|after| after.iter().find(|s| s.name == *name)) {
                Some(server) => {
                    if change.took_effect(&server.status) {
                        took_effect += 1;
                    }
                    describe(server)
                }
                None => {
                    unknown += 1;
                    "Claude Code accepted the change; its state is unknown".into()
                }
            },
        };
        lines.push(format!("- `{name}`: {line}"));
    }
    let headline = match answers {
        [(name, _)] if took_effect == 1 => format!("{} `{name}`.", change.past()),
        [(name, _)] => format!("Couldn't {} `{name}`.", change.verb()),
        _ => format!(
            "{} {took_effect} of {} MCP servers.",
            change.past(),
            answers.len()
        ),
    };
    let mut text = format!("{headline}\n\n{}", lines.join("\n"));
    if let Some(note) = note {
        text.push_str(&format!("\n\n{note}"));
    }
    let outcome = if took_effect > 0 {
        TurnOutcome::Completed
    } else if unknown > 0 {
        TurnOutcome::Unknown
    } else {
        TurnOutcome::Failed
    };
    Output { text, outcome }
}

fn control(request: Value) -> (String, Value) {
    let id = format!("drostje-mcp-{}", Uuid::new_v4());
    let frame = json!({"type":"control_request","request_id":id,"request":request});
    (id, frame)
}

fn status_request() -> (String, Value) {
    control(json!({"subtype":"mcp_status"}))
}

fn outcome(response: &Value) -> Result<&Value, String> {
    if response["subtype"] == "success" {
        Ok(&response["response"])
    } else {
        Err(brief(
            response["error"]
                .as_str()
                .unwrap_or("Claude Code rejected the request"),
        ))
    }
}

enum Stage {
    Listing {
        request_id: String,
        request: Request,
    },
    Changing {
        change: Change,
        waiting: HashMap<String, usize>,
        answers: Vec<(String, Option<Result<(), String>>)>,
        note: Option<String>,
    },
    Confirming {
        request_id: String,
        change: Change,
        answers: Vec<(String, Option<Result<(), String>>)>,
        note: Option<String>,
    },
}

pub(super) struct McpCommand {
    stage: Stage,
}

impl McpCommand {
    pub fn start(arguments: &str) -> (Option<Self>, Step) {
        match parse(arguments) {
            Err(output) => (None, Step::Done(output)),
            Ok(request) => {
                let (request_id, frame) = status_request();
                let stage = Stage::Listing {
                    request_id,
                    request,
                };
                (Some(Self { stage }), Step::Send(vec![frame]))
            }
        }
    }

    pub fn owns(&self, request_id: &str) -> bool {
        match &self.stage {
            Stage::Listing { request_id: id, .. } | Stage::Confirming { request_id: id, .. } => {
                id == request_id
            }
            Stage::Changing { waiting, .. } => waiting.contains_key(request_id),
        }
    }

    pub fn answer(&mut self, response: &Value) -> Step {
        let request_id = response["request_id"].as_str().unwrap_or("");
        match &mut self.stage {
            Stage::Listing { request, .. } => {
                let listed = outcome(response).and_then(|answer| {
                    servers(answer).ok_or_else(|| "Claude Code did not list its MCP servers".into())
                });
                let servers = match listed {
                    Ok(servers) => servers,
                    Err(error) => {
                        return Step::Done(Output::failed(format!(
                            "Couldn't read the MCP servers: {error}"
                        )));
                    }
                };
                let (change, target) = match request {
                    Request::Status => return Step::Done(status_report(&servers)),
                    Request::Change(change, target) => (*change, &*target),
                };
                let (targets, note) = match plan(change, target, &servers) {
                    Ok(plan) => plan,
                    Err(output) => return Step::Done(output),
                };
                let mut waiting = HashMap::new();
                let mut frames = Vec::new();
                for (index, name) in targets.iter().enumerate() {
                    let (id, frame) = control(change.request(name));
                    waiting.insert(id, index);
                    frames.push(frame);
                }
                self.stage = Stage::Changing {
                    change,
                    waiting,
                    answers: targets.into_iter().map(|name| (name, None)).collect(),
                    note,
                };
                Step::Send(frames)
            }
            Stage::Changing {
                change,
                waiting,
                answers,
                note,
            } => {
                if let Some(index) = waiting.remove(request_id) {
                    answers[index].1 = Some(outcome(response).map(|_| ()));
                }
                if !waiting.is_empty() {
                    return Step::Wait;
                }
                let (request_id, frame) = status_request();
                self.stage = Stage::Confirming {
                    request_id,
                    change: *change,
                    answers: std::mem::take(answers),
                    note: note.take(),
                };
                Step::Send(vec![frame])
            }
            Stage::Confirming {
                change,
                answers,
                note,
                ..
            } => {
                let after = outcome(response).ok().and_then(servers);
                Step::Done(change_report(
                    *change,
                    answers,
                    after.as_deref(),
                    note.as_deref(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn success(request_id: &str, response: Value) -> Value {
        json!({"subtype":"success","request_id":request_id,"response":response})
    }

    fn request_id(frame: &Value) -> String {
        frame["request_id"].as_str().unwrap().to_owned()
    }

    fn sent(step: Step) -> Vec<Value> {
        match step {
            Step::Send(frames) => frames,
            other => panic!("expected control requests, got {other:?}"),
        }
    }

    fn done(step: Step) -> Output {
        match step {
            Step::Done(output) => output,
            other => panic!("expected output, got {other:?}"),
        }
    }

    fn listing() -> Value {
        json!({"mcpServers":[
            {"name":"excalidraw","status":"failed","error":"Connection closed","config":{"type":"stdio"}},
            {"name":"claude.ai Claude Docs","status":"connected","tools":[{"name":"read"},{"name":"batch"}]},
            {"name":"plugin:design:slack","status":"needs-auth"},
            {"name":"old","status":"disabled"},
            {"name":"pending-one","status":"pending"},
            {"status":"connected"}
        ]})
    }

    fn run(
        arguments: &str,
        listing: Value,
        change: impl Fn(&Value) -> Value,
        after: Value,
    ) -> (Vec<Value>, Output) {
        let (command, step) = McpCommand::start(arguments);
        let mut command = command.expect("the command asks Claude Code");
        let status = sent(step);
        assert_eq!(status[0]["request"]["subtype"], "mcp_status");
        assert!(command.owns(&request_id(&status[0])));
        assert!(!command.owns("drostje-usage"));
        let step = command.answer(&success(&request_id(&status[0]), listing));
        let requests = match step {
            Step::Send(frames) => frames,
            Step::Done(output) => return (Vec::new(), output),
            Step::Wait => panic!("listing answered"),
        };
        let mut confirm = None;
        for (index, request) in requests.iter().enumerate() {
            let id = request_id(request);
            assert!(command.owns(&id));
            match command.answer(&change(request)) {
                Step::Wait => assert!(index + 1 < requests.len(), "every answer arrived"),
                Step::Send(frames) => confirm = Some(frames),
                Step::Done(output) => panic!("finished before confirming: {output:?}"),
            }
        }
        let confirm = confirm.expect("confirms after the last answer");
        assert_eq!(confirm[0]["request"]["subtype"], "mcp_status");
        let output = done(command.answer(&success(&request_id(&confirm[0]), after)));
        (requests, output)
    }

    #[test]
    fn arguments_read_like_the_terminal_command() {
        assert_eq!(parse(""), Ok(Request::Status));
        assert_eq!(parse("  status "), Ok(Request::Status));
        assert_eq!(
            parse("Reconnect"),
            Ok(Request::Change(Change::Reconnect, Target::All))
        );
        assert_eq!(
            parse("enable all"),
            Ok(Request::Change(Change::Enable, Target::All))
        );
        assert_eq!(
            parse("disable claude.ai Claude Docs"),
            Ok(Request::Change(
                Change::Disable,
                Target::Named("claude.ai Claude Docs".into())
            ))
        );
        assert_eq!(
            parse("help").map_err(|o| o.outcome),
            Err(TurnOutcome::Completed)
        );
        let unknown = parse("restart excalidraw").unwrap_err();
        assert_eq!(unknown.outcome, TurnOutcome::Failed);
        assert!(unknown.text.contains("\"restart\""), "{}", unknown.text);
    }

    #[test]
    fn long_server_errors_keep_their_first_line() {
        assert_eq!(brief("  spawn ENOENT\n  at x.js:1 "), "spawn ENOENT");
        let body = format!("Error POSTing to endpoint: {}", "é".repeat(400));
        let short = brief(&body);
        assert_eq!(short.chars().count(), ERROR_CHARS + 1);
        assert!(short.ends_with('…'));
    }

    #[test]
    fn usage_errors_finish_without_asking_claude_code() {
        let (command, step) = McpCommand::start("frobnicate");
        assert!(command.is_none());
        assert_eq!(done(step).outcome, TurnOutcome::Failed);
    }

    #[test]
    fn status_lists_every_server_with_its_state() {
        let (requests, output) = run("", listing(), |_| unreachable!(), json!(null));
        assert!(requests.is_empty());
        assert_eq!(output.outcome, TurnOutcome::Completed);
        assert!(output.text.starts_with("5 MCP servers:"), "{}", output.text);
        for line in [
            "- `excalidraw`: not connected: Connection closed",
            "- `claude.ai Claude Docs`: connected, 2 tools",
            "- `plugin:design:slack`: needs authentication",
            "- `old`: disabled",
            "- `pending-one`: connecting",
        ] {
            assert!(output.text.contains(line), "{line} in {}", output.text);
        }
        assert!(output.text.contains(USAGE));
    }

    #[test]
    fn reconnect_all_retries_failed_servers_and_reports_each() {
        let (requests, output) = run(
            "reconnect",
            listing(),
            |request| match request["request"]["serverName"].as_str() {
                Some("excalidraw") => success(&request_id(request), json!({})),
                _ => {
                    json!({"subtype":"error","request_id":request_id(request),"error":"Needs OAuth"})
                }
            },
            json!({"mcpServers":[
                {"name":"excalidraw","status":"connected","tools":[{"name":"draw"}]},
                {"name":"plugin:design:slack","status":"needs-auth"}
            ]}),
        );
        let targets: Vec<_> = requests
            .iter()
            .map(|r| {
                (
                    r["request"]["subtype"].clone(),
                    r["request"]["serverName"].clone(),
                )
            })
            .collect();
        assert_eq!(
            targets,
            [
                (json!("mcp_reconnect"), json!("excalidraw")),
                (json!("mcp_reconnect"), json!("plugin:design:slack")),
            ],
            "connected, connecting and disabled servers are left alone"
        );
        assert_eq!(output.outcome, TurnOutcome::Completed);
        assert_eq!(
            output.text,
            "Reconnected 1 of 2 MCP servers.\n\n\
             - `excalidraw`: connected, 1 tool\n\
             - `plugin:design:slack`: couldn't reconnect: Needs OAuth\n\n\
             1 disabled MCP server left alone; `/mcp enable all` brings them back."
        );
    }

    #[test]
    fn a_named_server_reconnects_even_when_connected() {
        let (requests, output) = run(
            "reconnect claude.ai Claude Docs",
            listing(),
            |request| success(&request_id(request), json!({})),
            json!({"mcpServers":[{"name":"claude.ai Claude Docs","status":"connected","tools":[]}]}),
        );
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["request"],
            json!({"subtype":"mcp_reconnect","serverName":"claude.ai Claude Docs"})
        );
        assert_eq!(
            output,
            Output::done(
                "Reconnected `claude.ai Claude Docs`.\n\n- `claude.ai Claude Docs`: connected, 0 tools"
            )
        );
    }

    #[test]
    fn a_reconnect_that_does_not_connect_failed() {
        let (_, output) = run(
            "reconnect excalidraw",
            listing(),
            |request| success(&request_id(request), json!({})),
            json!({"mcpServers":[{"name":"excalidraw","status":"failed","error":"spawn ENOENT"}]}),
        );
        assert_eq!(output.outcome, TurnOutcome::Failed);
        assert!(output.text.starts_with("Couldn't reconnect `excalidraw`."));
        assert!(output.text.contains("not connected: spawn ENOENT"));
    }

    #[test]
    fn enable_and_disable_toggle_the_server() {
        let (requests, output) = run(
            "enable old",
            listing(),
            |request| success(&request_id(request), json!({})),
            json!({"mcpServers":[{"name":"old","status":"pending"}]}),
        );
        assert_eq!(
            requests[0]["request"],
            json!({"subtype":"mcp_toggle","serverName":"old","enabled":true})
        );
        assert_eq!(
            output,
            Output::done("Enabled `old`.\n\n- `old`: connecting")
        );

        let (requests, output) = run(
            "disable all",
            listing(),
            |request| success(&request_id(request), json!({})),
            json!({"mcpServers":[]}),
        );
        assert_eq!(requests.len(), 4, "every enabled server");
        assert!(
            requests
                .iter()
                .all(|r| r["request"]["enabled"] == false && r["request"]["serverName"] != "old")
        );
        assert_eq!(
            output.outcome,
            TurnOutcome::Unknown,
            "no state confirms the change"
        );
        assert!(
            output.text.contains("its state is unknown"),
            "{}",
            output.text
        );
    }

    #[test]
    fn requests_that_cannot_apply_answer_without_changes() {
        use TurnOutcome::{Completed, Failed};
        for (arguments, outcome, text) in [
            (
                "reconnect old",
                Failed,
                "`old` is disabled. Run `/mcp enable old`",
            ),
            ("reconnect pending-one", Completed, "already connecting"),
            (
                "reconnect nobody",
                Failed,
                "There's no MCP server named `nobody`",
            ),
            (
                "enable excalidraw",
                Completed,
                "already enabled but not connected",
            ),
            (
                "enable claude.ai Claude Docs",
                Completed,
                "already enabled.",
            ),
            ("disable old", Completed, "already disabled."),
            ("enable all", Completed, ""),
        ] {
            let (requests, output) = run(
                arguments,
                listing(),
                |request| success(&request_id(request), json!({})),
                json!({"mcpServers":[{"name":"old","status":"connected"}]}),
            );
            if arguments == "enable all" {
                assert_eq!(requests.len(), 1, "only the disabled server");
                continue;
            }
            assert!(requests.is_empty(), "{arguments} changed nothing");
            assert_eq!(output.outcome, outcome, "{arguments}: {}", output.text);
            assert!(output.text.contains(text), "{arguments}: {}", output.text);
        }
        let connected = json!({"mcpServers":[{"name":"a","status":"connected"}]});
        let (requests, output) = run("reconnect all", connected, |_| unreachable!(), json!(null));
        assert!(requests.is_empty());
        assert_eq!(output.outcome, Completed);
        assert!(
            output
                .text
                .starts_with("All enabled MCP servers are already connected")
        );
        let (_, output) = run(
            "",
            json!({"mcpServers":[]}),
            |_| unreachable!(),
            json!(null),
        );
        assert!(output.text.starts_with("No MCP servers are configured."));
    }

    #[test]
    fn an_unreadable_server_list_fails_the_command() {
        let (command, step) = McpCommand::start("reconnect");
        let mut command = command.unwrap();
        let id = request_id(&sent(step)[0]);
        let output = done(command.answer(
            &json!({"subtype":"error","request_id":id,"error":"Unknown subtype: mcp_status"}),
        ));
        assert_eq!(
            output,
            Output::failed("Couldn't read the MCP servers: Unknown subtype: mcp_status")
        );
    }
}
