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
    use super::{JobNotice, JobOutput};
    use serde_json::json;

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
