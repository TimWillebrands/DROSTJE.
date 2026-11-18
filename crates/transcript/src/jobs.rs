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
    pub agent: Option<String>,
    pub label: Option<String>,
    pub status: Option<String>,
    pub duration: Option<String>,
    pub output: JobOutput,
    pub full_output: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobNotice {
    pub jobs: Vec<JobResult>,
}
pub fn system_notice_body(text: &str) -> Option<&str> {
    let body = text
        .trim()
        .strip_prefix("<system-notice>")?
        .strip_suffix("</system-notice>")?;
    if body.contains("<system-notice>") || body.contains("</system-notice>") {
        return None;
    }
    Some(body.trim())
}

impl JobNotice {
    pub fn parse(text: &str) -> Option<Self> {
        let body = system_notice_body(text)?;
        let (first, rest) = body.split_once('\n').unwrap_or((body, ""));
        let (single, count) = if let Some(rest) = first.strip_prefix("Background job ") {
            let (id, tail) = rest.split_once(" has completed.")?;
            if !valid_id(id) || !matches!(tail, "" | " Resume your work using the result below.") {
                return None;
            }
            (Some(id), 1)
        } else {
            let (count, tail) = first.split_once(" background jobs have completed.")?;
            if !matches!(tail, "" | " Resume your work using the results below.") {
                return None;
            }
            (None, count.parse::<usize>().ok()?)
        };
        if count == 0 {
            return None;
        }

        let mut separators = Vec::new();
        let mut offset = 0;
        let mut in_task = false;
        for line in rest.split_inclusive('\n') {
            let line_body = line.trim_end_matches(['\r', '\n']);
            if !in_task && line_body.starts_with("── Job ") {
                let (id, label) = job_separator(line_body)?;
                separators.push((offset, offset + line.len(), id, label));
            }
            if line_body.starts_with("<task-result ") {
                in_task = true;
            }
            if in_task && line_body.contains("</task-result>") {
                in_task = false;
            }
            offset += line.len();
        }

        let jobs = if separators.is_empty() {
            let content = rest.trim_matches(['\r', '\n']);
            if content.starts_with("<task-result") {
                parse_task_sequence(content)?
            } else {
                let id = single?;
                if content.contains("<task-result") || content.contains("</task-result>") {
                    return None;
                }
                vec![parse_shell_result(id, None, content)]
            }
        } else {
            if !rest[..separators[0].0].trim().is_empty() {
                return None;
            }
            let mut jobs = Vec::with_capacity(separators.len());
            for (index, &(_, start, id, label)) in separators.iter().enumerate() {
                let end = separators
                    .get(index + 1)
                    .map_or(rest.len(), |separator| separator.0);
                let content = rest[start..end].trim_matches(['\r', '\n']);
                let mut job = if content.starts_with("<task-result") {
                    let mut parsed = parse_task_sequence(content)?;
                    if parsed.len() != 1 || parsed[0].id != id {
                        return None;
                    }
                    parsed.remove(0)
                } else {
                    if content.contains("<task-result") || content.contains("</task-result>") {
                        return None;
                    }
                    parse_shell_result(id, None, content)
                };
                job.label = Some(label.to_owned());
                jobs.push(job);
            }
            jobs
        };
        if jobs.len() != count
            || single.is_some_and(|id| jobs[0].id != id)
            || jobs
                .iter()
                .enumerate()
                .any(|(index, job)| jobs[..index].iter().any(|other| other.id == job.id))
        {
            return None;
        }
        Some(Self { jobs })
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.chars().any(char::is_whitespace)
}

fn job_separator(line: &str) -> Option<(&str, &str)> {
    let (id, label) = line
        .strip_prefix("── Job ")?
        .strip_suffix(") ──")?
        .split_once(" (")?;
    valid_id(id).then_some((id, label))
}

fn parse_shell_result(id: &str, label: Option<String>, output: &str) -> JobResult {
    let mut status = None;
    for line in output.lines() {
        if let Some(code) = line
            .strip_prefix("Command exited with code ")
            .and_then(|code| code.parse::<i32>().ok())
        {
            if code != 0 {
                status = Some("failed".to_owned());
                break;
            }
            status = Some("completed".to_owned());
        }
    }
    let duration = output.lines().rev().find_map(|line| {
        let seconds = line.strip_prefix("Wall time: ")?.strip_suffix(" seconds")?;
        let value = seconds.parse::<f64>().ok()?;
        (value.is_finite() && value >= 0.0).then(|| format!("{seconds}s"))
    });
    JobResult {
        id: id.to_owned(),
        agent: None,
        label,
        status,
        duration,
        output: text_output(output),
        full_output: None,
    }
}

fn text_output(output: &str) -> JobOutput {
    if output.is_empty() {
        JobOutput::Empty
    } else {
        JobOutput::Text(output.to_owned())
    }
}

fn parse_task_sequence(mut text: &str) -> Option<Vec<JobResult>> {
    let mut jobs = Vec::new();
    while !text.trim().is_empty() {
        text = text.trim_start();
        if text.starts_with("<task-result ") {
            let (block, tail) = text.split_once("</task-result>")?;
            jobs.push(parse_task_result(block)?);
            text = tail;
        } else {
            let (line, tail) = text.split_once('\n').unwrap_or((text, ""));
            let idle = jobs.iter().any(|job: &JobResult| {
                line.strip_prefix(job.id.as_str())
                    .is_some_and(|tail| tail.starts_with(" is now idle — message it via "))
            });
            if !idle && !line.starts_with("Structured output: ") {
                return None;
            }
            text = tail;
        }
    }
    (!jobs.is_empty()).then_some(jobs)
}

fn parse_task_result(block: &str) -> Option<JobResult> {
    let (head, mut inner) = block.split_once('>')?;
    let attribute = |name: &str| attribute(head, name);
    let id = attribute("id")?;
    if !valid_id(&id) {
        return None;
    }
    inner = inner.trim();
    if let Some(meta) = inner.strip_prefix("<meta ") {
        let (_, rest) = meta.split_once("/>")?;
        inner = rest.trim();
    }
    let (output, full_output) = if let Some(rest) = inner.strip_prefix("<output>") {
        let (output, tail) = rest.split_once("</output>")?;
        if !tail.trim().is_empty() {
            return None;
        }
        (output, None)
    } else if inner.starts_with("<preview") {
        let (head, rest) = inner.split_once('>')?;
        let (preview, tail) = rest.split_once("</preview>")?;
        if !tail.trim().is_empty() {
            return None;
        }
        (preview, attribute_in(head, "full-output"))
    } else if inner.is_empty() {
        ("", None)
    } else {
        return None;
    };
    let output = output.trim_matches('\n');
    let output = match serde_json::from_str::<Value>(output) {
        Ok(value @ (Value::Object(_) | Value::Array(_))) => JobOutput::Structured(value),
        _ => text_output(output),
    };
    Some(JobResult {
        id,
        agent: attribute("agent"),
        label: None,
        status: attribute("status"),
        duration: attribute("duration"),
        output,
        full_output,
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobKind {
    Agent(String),
    Shell,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobStart {
    pub id: String,
    pub kind: JobKind,
    pub label: String,
}
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

#[cfg(test)]
mod tests {
    use super::{
        JobKind, JobNotice, JobOutput, JobStart, job_starts, settled_job_ids, system_notice_body,
    };
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
        assert_eq!(job.label, None);
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
        assert_eq!(job.label, None);
        assert_eq!(job.status.as_deref(), Some("failed"));
        assert_eq!(job.duration.as_deref(), Some("2.40s"));
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
        assert_eq!(notice.jobs[0].label.as_deref(), Some("BackendLint"));
        assert_eq!(notice.jobs[1].status.as_deref(), Some("failed"));
        assert_eq!(notice.jobs[1].output, JobOutput::Empty);
    }

    #[test]
    fn batched_shell_jobs_keep_every_output_and_command() {
        let command = format!(
            "env CARGO_TARGET_DIR=/tmp/drost cargo run --example space_network_smoke -- {}",
            "長い command (outer (inner)) → output ".repeat(20)
        );
        let command = command.trim_end();
        let first_output =
            "warning: Unicode Δ kept\n[raw output: artifact://53]\nWall time: 40.52 seconds";
        let second_output =
            "cargo test: 1 passed; 0 failed\n[raw output: artifact://54]\nWall time: 20.23 seconds";
        let text = format!(
            "<system-notice>\n2 background jobs have completed. Resume your work using the results below.\n\n── Job bg_16 ({command}) ──\n{first_output}\n── Job bg_17 (rustfmt src/ui.rs && env cargo test) ──\n{second_output}\n</system-notice>"
        );
        let notice = JobNotice::parse(&text).unwrap();
        assert_eq!(notice.jobs.len(), 2);
        let first = &notice.jobs[0];
        assert_eq!(first.id, "bg_16");
        assert_eq!(first.label.as_deref(), Some(command));
        assert_eq!(first.agent, None);
        assert_eq!(first.status, None);
        assert_eq!(first.duration.as_deref(), Some("40.52s"));
        assert_eq!(first.output, JobOutput::Text(first_output.into()));
        let second = &notice.jobs[1];
        assert_eq!(second.id, "bg_17");
        assert_eq!(
            second.label.as_deref(),
            Some("rustfmt src/ui.rs && env cargo test")
        );
        assert_eq!(second.status, None);
        assert_eq!(second.duration.as_deref(), Some("20.23s"));
        assert_eq!(second.output, JobOutput::Text(second_output.into()));
    }

    #[test]
    fn mixed_task_and_shell_batches_parse_in_either_order() {
        let task = "── Job Audit (Review (UI)) ──\n<task-result id=\"Audit\" agent=\"reviewer\" status=\"completed\" duration=\"1m\">\n<preview full-output=\"agent://Audit\">\nReview Δ\n── Job fake (inside task output) ──\n</preview>\n</task-result>\n\nAudit is now idle — message it via `hub` to follow up\nStructured output: schema valid; full payload at agent://Audit";
        let shell = "── Job bg_8 (cargo check) ──\nerror: compilation failed\nCommand exited with code 101\nWall time: 1.5 seconds";
        for (first, second, task_index, shell_index) in [(task, shell, 0, 1), (shell, task, 1, 0)] {
            let text = format!(
                "<system-notice>\n2 background jobs have completed.\n{first}\n{second}\n</system-notice>"
            );
            let notice = JobNotice::parse(&text).unwrap();
            assert_eq!(notice.jobs.len(), 2);
            let task = &notice.jobs[task_index];
            assert_eq!(task.id, "Audit");
            assert_eq!(task.agent.as_deref(), Some("reviewer"));
            assert_eq!(task.label.as_deref(), Some("Review (UI)"));
            assert_eq!(task.status.as_deref(), Some("completed"));
            assert_eq!(task.full_output.as_deref(), Some("agent://Audit"));
            assert_eq!(
                task.output,
                JobOutput::Text("Review Δ\n── Job fake (inside task output) ──".into())
            );
            let shell = &notice.jobs[shell_index];
            assert_eq!(shell.id, "bg_8");
            assert_eq!(shell.label.as_deref(), Some("cargo check"));
            assert_eq!(shell.status.as_deref(), Some("failed"));
            assert_eq!(shell.duration.as_deref(), Some("1.5s"));
            assert_eq!(
                shell.output,
                JobOutput::Text(
                    "error: compilation failed\nCommand exited with code 101\nWall time: 1.5 seconds"
                        .into()
                )
            );
        }
    }

    #[test]
    fn shell_status_requires_an_explicit_exit_code() {
        for (output, expected) in [
            ("error: not an exit status", None),
            ("test result: ok. 1 passed", None),
            ("Command exited with code unknown", None),
            ("Command exited with code 0", Some("completed")),
            ("Command exited with code 1", Some("failed")),
            ("Command exited with code -9", Some("failed")),
        ] {
            let text = format!(
                "<system-notice>\nBackground job bg_1 has completed.\n{output}\n</system-notice>"
            );
            let notice = JobNotice::parse(&text).unwrap();
            let job = &notice.jobs[0];
            assert_eq!(job.status.as_deref(), expected);
            assert_eq!(job.duration, None);
            assert_eq!(job.output, JobOutput::Text(output.into()));
        }
    }

    #[test]
    fn inline_separator_text_is_shell_output_not_another_job() {
        let output = "printed ── Job fake (not a delimiter) ── inline\n  indented ── Job fake2 (also output) ──\nWall time: 0.25 seconds";
        let text = format!(
            "<system-notice>\nBackground job bg_1 has completed.\n{output}\n</system-notice>"
        );
        let notice = JobNotice::parse(&text).unwrap();
        assert_eq!(notice.jobs.len(), 1);
        assert_eq!(notice.jobs[0].id, "bg_1");
        assert_eq!(notice.jobs[0].output, JobOutput::Text(output.into()));
    }

    #[test]
    fn task_batches_without_separators_preserve_all_results() {
        let text = "<system-notice>\n2 background jobs have completed.\n<task-result id=\"A\" agent=\"task\"><output>{\"ok\":true}</output></task-result>\n<task-result id=\"B\" agent=\"scout\"><output>[1,2]</output></task-result>\n</system-notice>";
        let notice = JobNotice::parse(text).unwrap();
        assert_eq!(notice.jobs.len(), 2);
        assert_eq!(notice.jobs[0].label, None);
        assert_eq!(notice.jobs[1].label, None);
        assert_eq!(
            notice.jobs[0].output,
            JobOutput::Structured(json!({"ok": true}))
        );
        assert_eq!(notice.jobs[1].output, JobOutput::Structured(json!([1, 2])));
    }

    #[test]
    fn malformed_batches_fall_back_instead_of_dropping_parts() {
        let valid_task =
            "<task-result id=\"Audit\" agent=\"task\"><output>kept</output></task-result>";
        for body in [
            format!("2 background jobs have completed.\n{valid_task}"),
            format!("2 background jobs have completed.\n{valid_task}\n<task-result id=\"Broken\"><output>unfinished"),
            format!("2 background jobs have completed.\n{valid_task}\n<task-result agent=\"task\"><output>missing id</output></task-result>"),
            format!("2 background jobs have completed.\n{valid_task}\n<task-result id=\"Broken\"><preview>missing close</task-result>"),
            format!("2 background jobs have completed.\n{valid_task}\n<task-result id=\"Broken\"><output>missing close</task-result>"),
            format!("2 background jobs have completed.\n{valid_task}\nunknown text\n<task-result id=\"B\"><output>kept</output></task-result>"),
            format!("2 background jobs have completed.\n── Job Other (Audit) ──\n{valid_task}\n── Job bg_1 (true) ──\noutput"),
            "2 background jobs have completed.\n── Job bg_1 (true) ──\nfirst\n── Job bg_2 missing label ──\nsecond".into(),
            "2 background jobs have completed.\n── Job bg_1 (true) ──\nfirst\n── Job bg_1 (false) ──\nsecond".into(),
            "2 background jobs have completed.\nunassigned output\n── Job bg_1 (true) ──\nfirst\n── Job bg_2 (false) ──\nsecond".into(),
            "Background job bg_1 has completed.\n── Job bg_2 (true) ──\nwrong id".into(),
            "not-a-count background jobs have completed.\n── Job bg_1 (true) ──\noutput".into(),
        ] {
            let text = format!("<system-notice>\n{body}\n</system-notice>");
            assert_eq!(JobNotice::parse(&text), None, "{body}");
            assert_eq!(system_notice_body(&text), Some(body.as_str()));
        }
    }

    #[test]
    fn notice_wrapper_requires_one_complete_outer_wrapper() {
        assert_eq!(
            system_notice_body(" \n<system-notice>\n unknown body \n</system-notice>\t"),
            Some("unknown body")
        );
        assert_eq!(
            system_notice_body("<system-notice></system-notice>"),
            Some("")
        );
        for text in [
            "raw",
            "<system-notice>missing close",
            "missing open</system-notice>",
            "prefix<system-notice>body</system-notice>",
            "<system-notice>body</system-notice>suffix",
            "<system-notice>a</system-notice><system-notice>b</system-notice>",
            "<system-notice><system-notice>nested</system-notice></system-notice>",
            "<system-notice extra=\"attribute\">body</system-notice>",
            "<system-reminder>body</system-reminder>",
        ] {
            assert_eq!(system_notice_body(text), None, "{text}");
            assert_eq!(JobNotice::parse(text), None, "{text}");
        }
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
