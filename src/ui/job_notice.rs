//! Background-job notices OMP injects as `<system-notice>` custom messages when
//! a `task` agent or a background shell command finishes, read back so the
//! transcript can show one card per job instead of the raw XML the model sees.
//!
//! A notice opens with `Background job X has completed.` or `N background jobs
//! have completed.` and then carries either raw shell output or one
//! `<task-result …>` block per job with an `<output>` or truncated `<preview>`.
//! Lines addressed to the model (`X is now idle — message it via hub`, job
//! separators, structured-output hints) are not part of any job. Text that does
//! not fit this shape parses to `None` and renders as it did before.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobOutput {
    Structured(Value),
    Text(String),
    Empty,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobResult {
    pub id: String,
    /// Agent kind for task jobs (`task`, `scout`, …); `None` for shell jobs.
    pub agent: Option<String>,
    /// OMP's own status word, untouched; `None` for shell jobs.
    pub status: Option<String>,
    pub duration: Option<String>,
    pub output: JobOutput,
    /// Set when OMP included only a preview; names where the full output lives.
    pub full_output: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobNotice {
    pub jobs: Vec<JobResult>,
}
impl JobNotice {
    pub fn parse(text: &str) -> Option<Self> {
        let body = text
            .trim()
            .strip_prefix("<system-notice>")?
            .strip_suffix("</system-notice>")?
            .trim_matches('\n');
        let (first, rest) = body.split_once('\n').unwrap_or((body, ""));
        let single = first
            .strip_prefix("Background job ")
            .and_then(|line| line.split_once(" has completed."))
            .map(|(id, _)| id.trim());
        let multi = first.contains(" background jobs have completed.");
        if single.is_none() && !multi {
            return None;
        }
        let mut jobs = Vec::new();
        let mut cursor = rest;
        while let Some(start) = cursor.find("<task-result ") {
            let Some(end) = cursor[start..].find("</task-result>") else {
                break;
            };
            if let Some(job) = parse_task_result(&cursor[start..start + end]) {
                jobs.push(job);
            }
            cursor = &cursor[start + end + "</task-result>".len()..];
        }
        if jobs.is_empty() {
            let id = single?;
            let output = rest.trim_matches('\n');
            jobs.push(JobResult {
                id: id.to_owned(),
                agent: None,
                status: None,
                duration: None,
                output: if output.is_empty() {
                    JobOutput::Empty
                } else {
                    JobOutput::Text(output.to_owned())
                },
                full_output: None,
            });
        }
        Some(JobNotice { jobs })
    }
}
/// `block` starts at `<task-result ` and ends just before `</task-result>`.
fn parse_task_result(block: &str) -> Option<JobResult> {
    let (head, inner) = block.split_once('>')?;
    let attribute = |name: &str| attribute(head, name);
    let id = attribute("id")?;
    let (output, full_output) = if let Some(output) = between(inner, "<output>", "</output>") {
        (output, None)
    } else if let Some(start) = inner.find("<preview") {
        let tail = &inner[start..];
        let (head, rest) = tail.split_once('>')?;
        let preview = rest.split_once("</preview>").map_or(rest, |(body, _)| body);
        (preview, attribute_in(head, "full-output"))
    } else {
        ("", None)
    };
    let output = output.trim_matches('\n');
    let output = if output.is_empty() {
        JobOutput::Empty
    } else {
        match serde_json::from_str::<Value>(output) {
            Ok(value @ (Value::Object(_) | Value::Array(_))) => JobOutput::Structured(value),
            _ => JobOutput::Text(output.to_owned()),
        }
    };
    Some(JobResult {
        id,
        agent: attribute("agent"),
        status: attribute("status"),
        duration: attribute("duration"),
        output,
        full_output,
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// A background agent; carries the agent kind (`task`, `scout`, `reviewer`, …).
    Agent(String),
    Shell,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobStart {
    pub id: String,
    pub kind: JobKind,
    /// The agent's name or the shell command.
    pub label: String,
}
/// Jobs a tool result reports as started: `task` spawns agents and lists them
/// as `` - `Name` (job `Id`) ``; `bash` with `async` answers
/// `Backgrounded as job bg_N; …`.
pub fn job_starts(tool: &str, input: &Value, result: &str) -> Vec<JobStart> {
    match tool {
        "task" => {
            let agent = result
                .lines()
                .next()
                .and_then(|line| line.split_once(" using "))
                .and_then(|(_, rest)| rest.split(['.', ' ']).next())
                .filter(|agent| !agent.is_empty())
                .unwrap_or("task")
                .to_owned();
            result
                .lines()
                .filter_map(|line| {
                    let (name, rest) = line.strip_prefix("- `")?.split_once('`')?;
                    let (id, _) = rest.strip_prefix(" (job `")?.split_once('`')?;
                    Some(JobStart {
                        id: id.to_owned(),
                        kind: JobKind::Agent(agent.clone()),
                        label: name.to_owned(),
                    })
                })
                .collect()
        }
        "bash" => result
            .trim_start()
            .strip_prefix("Backgrounded as job ")
            .and_then(|rest| rest.split([';', ' ', '\n']).next())
            .filter(|id| !id.is_empty())
            .map(|id| {
                vec![JobStart {
                    id: id.to_owned(),
                    kind: JobKind::Shell,
                    label: input["command"].as_str().unwrap_or("").to_owned(),
                }]
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}
/// Job ids a `wait` result or historical `hub` snapshot reports as settled:
/// each `### id [type] — status` entry outside “Still Running”, plus cancellations.
pub fn settled_job_ids(tool: &str, result: &str) -> Vec<String> {
    if !matches!(tool, "wait" | "hub") {
        return Vec::new();
    }
    let mut ids = Vec::new();
    let mut still_running = false;
    for line in result.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            still_running = heading.starts_with("Still Running");
            continue;
        }
        if still_running {
            continue;
        }
        if let Some(rest) = line.strip_prefix("### ")
            && let Some((id, tail)) = rest.split_once(" [")
            && tail.contains("] — ")
        {
            ids.push(id.to_owned());
        } else if let Some(rest) = line.strip_prefix("- Cancelled background job ") {
            ids.push(rest.trim_end_matches('.').to_owned());
        }
    }
    ids
}
fn attribute(head: &str, name: &str) -> Option<String> {
    attribute_in(head, name)
}
fn attribute_in(head: &str, name: &str) -> Option<String> {
    let (_, rest) = head.split_once(&format!(" {name}=\""))?;
    let (value, _) = rest.split_once('"')?;
    (!value.is_empty()).then(|| value.to_owned())
}
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(open)?;
    let (inner, _) = rest.split_once(close)?;
    Some(inner)
}

#[cfg(test)]
mod tests {
    use super::{JobKind, JobNotice, JobOutput, JobStart, job_starts, settled_job_ids};
    use serde_json::{Value, json};

    #[test]
    fn task_and_async_bash_results_name_the_jobs_they_started() {
        let task = "Spawned 3 background agents using reviewer. Each result auto-delivers on yield.\n- `ComplexityReview` (job `ComplexityReview`)\n- `SpecReview` (job `job_7`)\nDM these ids via `hub`.";
        assert_eq!(
            job_starts("task", &Value::Null, task),
            vec![
                JobStart {
                    id: "ComplexityReview".into(),
                    kind: JobKind::Agent("reviewer".into()),
                    label: "ComplexityReview".into()
                },
                JobStart {
                    id: "job_7".into(),
                    kind: JobKind::Agent("reviewer".into()),
                    label: "SpecReview".into()
                },
            ]
        );
        assert_eq!(
            job_starts(
                "bash",
                &json!({"command":"cargo test","async":true}),
                "Backgrounded as job bg_1; result will be delivered automatically."
            ),
            vec![JobStart {
                id: "bg_1".into(),
                kind: JobKind::Shell,
                label: "cargo test".into()
            }]
        );
        assert!(job_starts("bash", &json!({"command":"ls"}), "file\n").is_empty());
        assert!(job_starts("hub", &Value::Null, "- `X` (job `X`)").is_empty());
    }

    #[test]
    fn hub_snapshots_settle_jobs_outside_the_running_section() {
        let text = "## Still Running (1)\n\n- `Slow` [task] — Slow\n\n## Completed (2)\n\n### bg_19 [bash] — completed\nLabel: cargo check\n### SpecReview [task] — completed\n\n## Cancelled (1)\n\n- Cancelled background job bg_3.\n\n### bg_3 [bash] — cancelled\n";
        assert_eq!(
            settled_job_ids("hub", text),
            ["bg_19", "SpecReview", "bg_3", "bg_3"]
        );
        assert!(settled_job_ids("bash", text).is_empty());
    }

    #[test]
    fn wait_results_settle_completed_and_failed_jobs_without_hiding_running_jobs() {
        let result = "## Completed (1)\n\n### bg_1 [bash] — completed\nLabel: cargo check --all-targets\nDelivery: not auto-delivered; recovered by this snapshot.\n\n## Failed (1)\n\n### bg_2 [bash] — failed\nError: command exited with code 101\n\n## Still Running (1)\n\n### bg_3 [bash] — running\n";
        assert_eq!(settled_job_ids("wait", result), ["bg_1", "bg_2"]);
    }

    #[test]
    fn task_result_with_structured_output() {
        let text = "<system-notice>\nBackground job EditorRendering has completed. Resume your work using the result below.\n<task-result id=\"EditorRendering\" agent=\"task\" status=\"completed\" duration=\"8m19s\">\n<meta lines=\"27\" size=\"2.6KB\" />\n<output>\n{\n  \"changed\": [\"src/ui/nvim.rs\"],\n  \"verification\": \"No build run.\"\n}\n</output>\n</task-result>\n\nEditorRendering is now idle — message it via `hub` to follow up; transcript at history://EditorRendering\n</system-notice>";
        let notice = JobNotice::parse(text).expect("notice parses");
        assert_eq!(notice.jobs.len(), 1);
        let job = &notice.jobs[0];
        assert_eq!(job.id, "EditorRendering");
        assert_eq!(job.agent.as_deref(), Some("task"));
        assert_eq!(job.status.as_deref(), Some("completed"));
        assert_eq!(job.duration.as_deref(), Some("8m19s"));
        assert_eq!(job.full_output, None);
        assert_eq!(
            job.output,
            JobOutput::Structured(
                json!({"changed": ["src/ui/nvim.rs"], "verification": "No build run."})
            )
        );
    }

    #[test]
    fn shell_job_keeps_its_raw_output() {
        let text = "<system-notice>\nBackground job bg_2 has completed. Resume your work using the result below.\n    Checking drost v0.1.0\nerror: recursion limit reached\nWall time: 2.40 seconds\n\nCommand exited with code 101\n</system-notice>";
        let notice = JobNotice::parse(text).unwrap();
        let job = &notice.jobs[0];
        assert_eq!(job.id, "bg_2");
        assert_eq!(job.agent, None);
        assert_eq!(job.status, None);
        assert_eq!(
            job.output,
            JobOutput::Text(
                "    Checking drost v0.1.0\nerror: recursion limit reached\nWall time: 2.40 seconds\n\nCommand exited with code 101".into()
            )
        );
    }

    #[test]
    fn preview_names_the_full_output_and_text_stays_text() {
        let text = "<system-notice>\nBackground job UiGuideAudit has completed. Resume your work using the result below.\n<task-result id=\"UiGuideAudit\" agent=\"scout\" status=\"completed\" duration=\"5m3s\">\n<meta lines=\"27\" size=\"25.6KB\" />\n<preview full-output=\"agent://UiGuideAudit\">\n{\n  \"summary\": \"Read the guide and trac\n</preview>\n</task-result>\n\nStructured output: schema valid; full payload at agent://UiGuideAudit\n</system-notice>";
        let job = &JobNotice::parse(text).unwrap().jobs[0];
        assert_eq!(job.full_output.as_deref(), Some("agent://UiGuideAudit"));
        assert!(
            matches!(&job.output, JobOutput::Text(text) if text.starts_with("{\n  \"summary\""))
        );
    }

    #[test]
    fn several_jobs_in_one_notice() {
        let text = "<system-notice>\n2 background jobs have completed. Resume your work using the results below.\n\n── Job BackendLint (BackendLint) ──\n<task-result id=\"BackendLint\" agent=\"sonic\" status=\"completed\" duration=\"1m4s\">\n<meta lines=\"8\" size=\"549B\" />\n<output>\n{\"edited\": []}\n</output>\n</task-result>\n\nBackendLint is now idle — message it via `hub` to follow up\n── Job SurfaceLint (SurfaceLint) ──\n<task-result id=\"SurfaceLint\" agent=\"sonic\" status=\"failed\" duration=\"1m7s\">\n<output>\n</output>\n</task-result>\n</system-notice>";
        let notice = JobNotice::parse(text).unwrap();
        let ids: Vec<_> = notice.jobs.iter().map(|job| job.id.as_str()).collect();
        assert_eq!(ids, ["BackendLint", "SurfaceLint"]);
        assert_eq!(notice.jobs[1].status.as_deref(), Some("failed"));
        assert_eq!(notice.jobs[1].output, JobOutput::Empty);
    }

    #[test]
    fn anything_else_stays_raw() {
        assert_eq!(JobNotice::parse("Background job bg_1 has completed."), None);
        assert_eq!(
            JobNotice::parse("<system-notice>\nSomething else entirely.\n</system-notice>"),
            None
        );
        assert_eq!(
            JobNotice::parse(
                "<system-notice>\n2 background jobs have completed.\n</system-notice>"
            ),
            None
        );
        assert_eq!(
            JobNotice::parse("<system-reminder>x</system-reminder>"),
            None
        );
    }
}
