//! The `todo` tool's list, read back from the result text OMP prints so the
//! transcript can show a checklist instead of a wall of monospace lines.
//!
//! OMP prints the list after its `Overall:` and `Active phase` lines as
//! two-space `Phase:` headers with four-space `- [X] task (marker)` rows, where
//! the marker is `in progress`, `dropped` or `blocked: reason`. Anything that
//! does not fit this shape parses to `None` and stays raw text; DROSTJE. never
//! invents a task state.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Dropped,
    Blocked(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoTask {
    pub content: String,
    pub status: TodoStatus,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoPhase {
    pub name: String,
    pub tasks: Vec<TodoTask>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoList {
    pub phases: Vec<TodoPhase>,
    /// OMP's own tally line, such as `3/7 done, 3 open, 1 blocked`.
    pub overall: Option<String>,
    pub active_phase: Option<String>,
}
impl TodoPhase {
    /// Completed tasks over tasks still counting; dropped tasks count for neither.
    pub fn progress(&self) -> (usize, usize) {
        let mut done = 0;
        let mut total = 0;
        for task in &self.tasks {
            match task.status {
                TodoStatus::Dropped => {}
                TodoStatus::Completed => {
                    done += 1;
                    total += 1;
                }
                TodoStatus::Pending | TodoStatus::InProgress | TodoStatus::Blocked(_) => {
                    total += 1;
                }
            }
        }
        (done, total)
    }
}
impl TodoList {
    pub fn parse(text: &str) -> Option<Self> {
        let mut list = TodoList {
            phases: Vec::new(),
            overall: None,
            active_phase: None,
        };
        for line in text.lines() {
            let line = line.trim_end();
            if let Some(rest) = line.strip_prefix("Overall: ") {
                list.overall = Some(rest.trim_end_matches('.').to_owned());
            } else if let Some(rest) = line.strip_prefix("Active phase ") {
                list.active_phase = rest
                    .split_once('"')
                    .and_then(|(_, tail)| tail.split_once('"'))
                    .map(|(name, _)| name.to_owned());
            } else if let Some(task) = line.strip_prefix("    - [") {
                let (checked, content) = match task.split_once("] ") {
                    Some(("X" | "x", content)) => (true, content),
                    Some((" ", content)) => (false, content),
                    _ => continue,
                };
                let Some(phase) = list.phases.last_mut() else {
                    continue;
                };
                phase.tasks.push(parse_task(checked, content));
            } else if let Some(header) = line.strip_prefix("  ")
                && !header.starts_with([' ', '-'])
                && let Some(name) = header.strip_suffix(':')
                && !name.is_empty()
            {
                list.phases.push(TodoPhase {
                    name: name.to_owned(),
                    tasks: Vec::new(),
                });
            }
        }
        list.phases.retain(|phase| !phase.tasks.is_empty());
        (!list.phases.is_empty()).then_some(list)
    }
    pub fn progress(&self) -> (usize, usize) {
        self.phases
            .iter()
            .map(TodoPhase::progress)
            .fold((0, 0), |(done, total), (d, t)| (done + d, total + t))
    }
}
fn parse_task(checked: bool, content: &str) -> TodoTask {
    if checked {
        return TodoTask {
            content: content.to_owned(),
            status: TodoStatus::Completed,
        };
    }
    if let Some(content) = content.strip_suffix(" (in progress)") {
        return TodoTask {
            content: content.to_owned(),
            status: TodoStatus::InProgress,
        };
    }
    if let Some(content) = content.strip_suffix(" (dropped)") {
        return TodoTask {
            content: content.to_owned(),
            status: TodoStatus::Dropped,
        };
    }
    if let Some((content, reason)) = content.split_once(" (blocked: ")
        && let Some(reason) = reason.strip_suffix(')')
    {
        return TodoTask {
            content: content.to_owned(),
            status: TodoStatus::Blocked(reason.to_owned()),
        };
    }
    TodoTask {
        content: content.to_owned(),
        status: TodoStatus::Pending,
    }
}

/// One line saying what a `todo` call changed, from its arguments alone.
pub fn describe_change(input: &Value) -> Option<String> {
    let field = |key: &str| input[key].as_str().filter(|value| !value.is_empty());
    let subject = || field("task").or_else(|| field("phase"));
    let op = field("op")?;
    Some(match op {
        "init" => {
            let phases = input["list"].as_array();
            let tasks: usize = phases
                .into_iter()
                .flatten()
                .filter_map(|phase| phase["items"].as_array())
                .map(Vec::len)
                .sum();
            match (tasks, phases.map_or(0, Vec::len)) {
                (0, _) => "Started a list".to_owned(),
                (tasks, 1) => format!("Started {} in one phase", plural(tasks, "task")),
                (tasks, phases) => format!(
                    "Started {} in {}",
                    plural(tasks, "task"),
                    plural(phases, "phase")
                ),
            }
        }
        "append" => {
            let items: Vec<&str> = input["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            match (field("phase"), items.is_empty()) {
                (Some(phase), false) => format!("Added to {phase}: {}", items.join("; ")),
                (Some(phase), true) => format!("Added to {phase}"),
                (None, false) => format!("Added: {}", items.join("; ")),
                (None, true) => "Added tasks".to_owned(),
            }
        }
        "done" => match (field("task"), field("phase")) {
            (Some(task), _) => format!("Done: {task}"),
            (None, Some(phase)) => format!("Phase done: {phase}"),
            (None, None) => "Done".to_owned(),
        },
        "block" => match (subject(), field("reason")) {
            (Some(task), Some(reason)) => format!("Blocked: {task}. {reason}"),
            (Some(task), None) => format!("Blocked: {task}"),
            (None, _) => "Blocked".to_owned(),
        },
        "unblock" => subject().map_or("Unblocked".to_owned(), |task| format!("Unblocked: {task}")),
        "drop" => subject().map_or("Dropped".to_owned(), |task| format!("Dropped: {task}")),
        other => subject().map_or(other.to_owned(), |task| format!("{other}: {task}")),
    })
}
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::{TodoList, TodoStatus, describe_change};
    use serde_json::json;

    const RESULT: &str = "Remaining items (2):\n  - Run checks [in_progress] (Verification)\n  - Ship it [pending] (Verification)\nOverall: 3/6 done, 2 open, 1 blocked.\nActive phase 2/2 \"Verification\" (1/3).\n  Implementation:\n    - [X] Build the parser\n    - [X] Wire the card (kept)\n    - [ ] Polish (dropped)\n    - [ ] Fix login (blocked: Exact rejects the client ID (see ticket))\n  Verification:\n    - [X] Add tests\n    - [ ] Run checks (in progress)\n    - [ ] Ship it\n";

    #[test]
    fn parses_phases_tasks_markers_and_tally() {
        let list = TodoList::parse(RESULT).expect("checklist parses");
        assert_eq!(list.overall.as_deref(), Some("3/6 done, 2 open, 1 blocked"));
        assert_eq!(list.active_phase.as_deref(), Some("Verification"));
        assert_eq!(list.phases.len(), 2);
        let implementation = &list.phases[0];
        assert_eq!(implementation.name, "Implementation");
        let statuses: Vec<_> = implementation
            .tasks
            .iter()
            .map(|task| (task.content.as_str(), task.status.clone()))
            .collect();
        assert_eq!(
            statuses,
            vec![
                ("Build the parser", TodoStatus::Completed),
                ("Wire the card (kept)", TodoStatus::Completed),
                ("Polish", TodoStatus::Dropped),
                (
                    "Fix login",
                    TodoStatus::Blocked("Exact rejects the client ID (see ticket)".into())
                ),
            ]
        );
        let verification = &list.phases[1];
        assert_eq!(verification.tasks[1].status, TodoStatus::InProgress);
        assert_eq!(verification.tasks[1].content, "Run checks");
        assert_eq!(verification.tasks[2].status, TodoStatus::Pending);
        assert_eq!(list.progress(), (3, 6));
    }

    #[test]
    fn anything_else_stays_raw() {
        assert_eq!(TodoList::parse("Unknown task: Ship it"), None);
        assert_eq!(TodoList::parse(""), None);
        // Headers without rows and rows without a header never invent a phase.
        assert_eq!(TodoList::parse("  Orphans:\n"), None);
        assert_eq!(TodoList::parse("    - [ ] Homeless task\n"), None);
        // The remaining-items lines are not phase headers.
        assert_eq!(
            TodoList::parse("Remaining items (1):\n  - Task [pending] (Phase)\n"),
            None
        );
    }

    #[test]
    fn describes_every_operation() {
        assert_eq!(
            describe_change(&json!({"op":"init","list":[{"phase":"A","items":["x","y"]},{"phase":"B","items":["z"]}]})).unwrap(),
            "Started 3 tasks in 2 phases"
        );
        assert_eq!(
            describe_change(&json!({"op":"init","list":[{"phase":"A","items":["x"]}]})).unwrap(),
            "Started 1 task in one phase"
        );
        assert_eq!(
            describe_change(&json!({"op":"done","task":"Run checks","phase":null})).unwrap(),
            "Done: Run checks"
        );
        assert_eq!(
            describe_change(&json!({"op":"done","phase":"Implementation"})).unwrap(),
            "Phase done: Implementation"
        );
        assert_eq!(
            describe_change(&json!({"op":"append","phase":"Cleanup","items":["a","b"]})).unwrap(),
            "Added to Cleanup: a; b"
        );
        assert_eq!(
            describe_change(&json!({"op":"block","task":"Login","reason":"No credentials"}))
                .unwrap(),
            "Blocked: Login. No credentials"
        );
        assert_eq!(
            describe_change(&json!({"op":"drop","task":"Polish"})).unwrap(),
            "Dropped: Polish"
        );
        assert_eq!(
            describe_change(&json!({"op":"unblock","task":"Login"})).unwrap(),
            "Unblocked: Login"
        );
        assert_eq!(
            describe_change(&json!({"op":"reset","task":"x"})).unwrap(),
            "reset: x"
        );
        assert_eq!(describe_change(&json!({"task":"x"})), None);
    }
}
