//! The `azdo` CLI (Azure DevOps): argv, its `--json` command surface and its exit codes.
//! `azdo` resolves organisation, project and repository from the working directory's
//! `origin` remote, so every call runs in the Conversation's worktree.

use super::{
    PullRequestSummary, ReviewComment, ReviewStatus, ReviewThread, ReviewThreads, TrackerError,
};
use crate::model::{PullRequestLink, PullRequestState};
use serde::Deserialize;
use std::{
    path::Path,
    process::{Command, Stdio},
};

/// Branch PR lists are short; more than this many is reported as unreadable, not trimmed.
const PULL_REQUEST_LIMIT: &str = "100";
const THREAD_LIMIT: &str = "500";
/// `azdo` appends this to a comment preview cut at its bound.
const TRUNCATION_MARK: &str = "\n\n[truncated;";

pub fn pull_requests_for_branch(
    executable: &Path,
    dir: &Path,
    branch: &str,
) -> Result<Vec<PullRequestSummary>, TrackerError> {
    let page: Page<PullRequestJson> = run(
        executable,
        dir,
        &[
            "pr",
            "list",
            "--source-branch",
            branch,
            "--status",
            "all",
            "--limit",
            PULL_REQUEST_LIMIT,
            "--json",
        ],
    )?;
    if page.truncated {
        return Err(TrackerError::Failed(format!(
            "More than {PULL_REQUEST_LIMIT} pull requests use branch {branch}"
        )));
    }
    Ok(page
        .items
        .into_iter()
        .map(|pr| PullRequestSummary {
            id: pr.id,
            title: pr.title,
            url: pr.url,
            state: state(&pr.state),
            created: pr.created,
        })
        .collect())
}

pub fn pull_request(
    executable: &Path,
    dir: &Path,
    id: u64,
) -> Result<PullRequestLink, TrackerError> {
    let pr: PullRequestJson = run(executable, dir, &["pr", "view", &id.to_string(), "--json"])?;
    Ok(PullRequestLink {
        id: pr.id,
        url: pr.url,
        title: pr.title,
        state: state(&pr.state),
        work_items: pr.work_items,
        archived_on_close: false,
    })
}

pub fn review_threads(
    executable: &Path,
    dir: &Path,
    id: u64,
) -> Result<ReviewThreads, TrackerError> {
    let page: Page<ThreadJson> = run(
        executable,
        dir,
        &[
            "pr",
            "thread",
            "list",
            &id.to_string(),
            "--all",
            "--limit",
            THREAD_LIMIT,
            "--json",
        ],
    )?;
    Ok(ReviewThreads {
        threads: page.items.into_iter().filter_map(review_thread).collect(),
        truncated: page.truncated,
    })
}

fn state(raw: &str) -> PullRequestState {
    match raw {
        "active" => PullRequestState::Active,
        "completed" => PullRequestState::Completed,
        "abandoned" => PullRequestState::Abandoned,
        other => PullRequestState::Unknown(other.to_owned()),
    }
}

fn review_status(raw: &str) -> ReviewStatus {
    match raw {
        "active" => ReviewStatus::Active,
        "pending" => ReviewStatus::Pending,
        "fixed" => ReviewStatus::Fixed,
        "wontFix" | "wontfix" => ReviewStatus::WontFix,
        "closed" => ReviewStatus::Closed,
        "byDesign" | "bydesign" => ReviewStatus::ByDesign,
        other => ReviewStatus::Unknown(other.to_owned()),
    }
}

/// A thread with at least one comment a person (or review bot) wrote; vote and push
/// notices are system comments and a thread of only those is not review feedback.
fn review_thread(thread: ThreadJson) -> Option<ReviewThread> {
    let comments: Vec<_> = thread
        .comments
        .into_iter()
        .filter(|comment| !comment.is_deleted && comment.comment_type != "system")
        .map(|comment| {
            let (body, truncated) = match comment.content.find(TRUNCATION_MARK) {
                Some(at) => (&comment.content[..at], true),
                None => (comment.content.as_str(), false),
            };
            ReviewComment {
                id: comment.id,
                author: comment.author.name,
                body: strip_markup(&strip_html_comments(body)).trim().to_owned(),
                truncated,
                published: comment.published,
            }
        })
        .collect();
    if comments.is_empty() {
        return None;
    }
    Some(ReviewThread {
        id: thread.id,
        status: review_status(&thread.status),
        file: thread.location.file,
        line: thread.location.line,
        outdated: thread.is_outdated,
        comments,
        total_comments: thread.total_comments,
    })
}

/// Review bots keep machine state in `<!-- … -->` blocks; they are not part of the message.
/// An unterminated block is cut off by preview truncation and dropped to the end.
fn strip_html_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Presentational HTML that review bots wrap Markdown in. Only these tags are removed, so
/// code such as `Vec<String>` in a comment survives.
const MARKUP_TAGS: &[&str] = &[
    "a", "b", "br", "details", "div", "em", "font", "i", "img", "p", "span", "strong", "sub",
    "summary", "sup", "table", "tbody", "td", "th", "thead", "tr",
];

/// Drops `MARKUP_TAGS` (opening, closing and self-closing), keeping their text.
fn strip_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let name: String = tail[1..]
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        match tail.find('>') {
            Some(end) if MARKUP_TAGS.contains(&name.as_str()) => rest = &tail[end + 1..],
            // A preview cut inside a tag.
            None if MARKUP_TAGS.contains(&name.as_str()) => return out,
            _ => {
                out.push('<');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn run<T: for<'de> Deserialize<'de>>(
    executable: &Path,
    dir: &Path,
    args: &[&str],
) -> Result<T, TrackerError> {
    let output = Command::new(executable)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            TrackerError::Unavailable(format!("Could not run {}: {error}", executable.display()))
        })?;
    if output.status.success() {
        return serde_json::from_slice(&output.stdout).map_err(|error| {
            TrackerError::Failed(format!(
                "azdo {} returned unreadable JSON: {error}",
                args[..2].join(" ")
            ))
        });
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // `azdo --json` reports a failure as one `{"error": …}` line on stderr.
    let message = stderr
        .lines()
        .find_map(|line| serde_json::from_str::<ErrorJson>(line).ok())
        .map(|error| error.error.message)
        .unwrap_or_else(|| stderr.trim().to_owned());
    let message = if message.is_empty() {
        format!("azdo {} failed", args[..2].join(" "))
    } else {
        message
    };
    Err(match output.status.code() {
        Some(3) => TrackerError::NotFound(message),
        Some(4) => TrackerError::Auth(message),
        _ => TrackerError::Failed(message),
    })
}

#[derive(Deserialize)]
struct Page<T> {
    items: Vec<T>,
    truncated: bool,
}

#[derive(Deserialize)]
struct PullRequestJson {
    id: u64,
    title: String,
    state: String,
    url: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    work_items: Vec<u64>,
}

#[derive(Deserialize)]
struct ThreadJson {
    id: u64,
    status: String,
    #[serde(default)]
    is_outdated: bool,
    location: LocationJson,
    comments: Vec<CommentJson>,
    total_comments: u64,
}

#[derive(Deserialize)]
struct LocationJson {
    file: Option<String>,
    line: Option<u64>,
}

#[derive(Deserialize)]
struct CommentJson {
    id: u64,
    author: AuthorJson,
    content: String,
    published: String,
    is_deleted: bool,
    comment_type: String,
}

#[derive(Deserialize)]
struct AuthorJson {
    name: String,
}

#[derive(Deserialize)]
struct ErrorJson {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    /// A stand-in `azdo` that records its argv and working directory, then replays a reply.
    fn fake_azdo(dir: &Path, stdout: &str, stderr: &str, code: i32) -> PathBuf {
        let path = dir.join("azdo");
        fs::write(dir.join("stdout"), stdout).unwrap();
        fs::write(dir.join("stderr"), stderr).unwrap();
        fs::write(
            &path,
            format!(
                "#!/bin/sh\npwd > '{d}/cwd'\nprintf '%s\\n' \"$@\" > '{d}/argv'\ncat '{d}/stdout'\ncat '{d}/stderr' >&2\nexit {code}\n",
                d = dir.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn temp() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("drostje-azdo-{}", uuid::Uuid::new_v4()));
        let work = root.join("worktree");
        fs::create_dir_all(&work).unwrap();
        (
            fs::canonicalize(&root).unwrap(),
            fs::canonicalize(work).unwrap(),
        )
    }

    #[test]
    fn lists_branch_pull_requests_from_the_worktree() {
        let (root, work) = temp();
        let azdo = fake_azdo(
            &root,
            r#"{"items":[{"id":3170,"title":"Fix upload","state":"completed","is_draft":false,"source_branch":"drostje/x","target_branch":"dev","author":{"name":"A","unique_name":"a@x","id":"1"},"created":"2026-10-20T10:35:40.3697993Z","url":"https://dev.azure.com/o/p/_git/r/pullrequest/3170"}],"truncated":false,"next":null}"#,
            "",
            0,
        );
        let prs = pull_requests_for_branch(&azdo, &work, "drostje/x").unwrap();
        assert_eq!(
            prs,
            vec![PullRequestSummary {
                id: 3170,
                title: "Fix upload".into(),
                url: "https://dev.azure.com/o/p/_git/r/pullrequest/3170".into(),
                state: PullRequestState::Completed,
                created: "2026-10-20T10:35:40.3697993Z".into(),
            }]
        );
        assert_eq!(
            fs::read_to_string(root.join("cwd")).unwrap().trim(),
            work.to_str().unwrap()
        );
        assert_eq!(
            fs::read_to_string(root.join("argv"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "pr",
                "list",
                "--source-branch",
                "drostje/x",
                "--status",
                "all",
                "--limit",
                "100",
                "--json"
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_state_and_work_items_and_keeps_unknown_states_raw() {
        let (root, work) = temp();
        let azdo = fake_azdo(
            &root,
            r#"{"id":7,"title":"T","state":"notSet","is_draft":false,"url":"u","description":"","reviewers":[],"work_items":[2596,12],"merge_status":"succeeded"}"#,
            "",
            0,
        );
        let pr = pull_request(&azdo, &work, 7).unwrap();
        assert_eq!(pr.state, PullRequestState::Unknown("notSet".into()));
        assert!(!pr.state.closed());
        assert_eq!(pr.work_items, [2596, 12]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failures_map_exit_codes_and_keep_the_message() {
        let (root, work) = temp();
        for (code, expected) in [
            (4, TrackerError::Auth("token expired".into())),
            (3, TrackerError::NotFound("token expired".into())),
            (1, TrackerError::Failed("token expired".into())),
        ] {
            let azdo = fake_azdo(
                &root,
                "",
                r#"{"error":{"code":"x","message":"token expired","hint":"az login"}}"#,
                code,
            );
            assert_eq!(pull_request(&azdo, &work, 1), Err(expected));
        }
        let azdo = fake_azdo(&root, "not json", "", 0);
        assert!(matches!(
            pull_request(&azdo, &work, 1),
            Err(TrackerError::Failed(_))
        ));
        assert!(matches!(
            pull_request(&root.join("missing"), &work, 1),
            Err(TrackerError::Unavailable(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn review_threads_drop_system_and_deleted_comments() {
        let (root, work) = temp();
        let azdo = fake_azdo(
            &root,
            r#"{"items":[
              {"id":1,"status":"active","is_outdated":true,"location":{"file":"/src/a.rs","line":42},"iteration_id":2,
               "comments":[
                 {"id":1,"author":{"name":"Rev","unique_name":"r","id":"r"},"content":"<!-- state -->Edge case?\n\n[truncated; run 'azdo pr thread view 1 7' to view full body]","published":"2026-10-20T10:00:00Z","is_deleted":false,"comment_type":"text"},
                 {"id":2,"author":{"name":"Gone","unique_name":"g","id":"g"},"content":"x","published":"2026-10-20T10:01:00Z","is_deleted":true,"comment_type":"text"}],
               "total_comments":2,"reply_count":1},
              {"id":2,"status":"unknown","is_outdated":false,"location":{"file":null,"line":null},"iteration_id":null,
               "comments":[{"id":1,"author":{"name":"Sys","unique_name":"s","id":"s"},"content":"voted 10","published":"2026-10-20T10:00:00Z","is_deleted":false,"comment_type":"system"}],
               "total_comments":1,"reply_count":0},
              {"id":3,"status":"wontFix","is_outdated":false,"location":{"file":null,"line":null},"iteration_id":null,
               "comments":[{"id":1,"author":{"name":"Bot","unique_name":"b","id":"b"},"content":"<!-- open","published":"2026-10-20T10:00:00Z","is_deleted":false,"comment_type":""}],
               "total_comments":1,"reply_count":0}
            ],"truncated":true,"next":"azdo pr thread list 7 --all --json --limit 500 --skip 500"}"#,
            "",
            0,
        );
        let threads = review_threads(&azdo, &work, 7).unwrap();
        assert!(threads.truncated);
        assert_eq!(threads.threads.len(), 2);
        let first = &threads.threads[0];
        assert_eq!(first.status, ReviewStatus::Active);
        assert_eq!(
            (first.file.as_deref(), first.line),
            (Some("/src/a.rs"), Some(42))
        );
        assert!(first.outdated);
        assert_eq!(first.comments.len(), 1);
        assert_eq!(first.comments[0].body, "Edge case?");
        assert!(first.comments[0].truncated);
        assert_eq!(threads.threads[1].status, ReviewStatus::WontFix);
        assert_eq!(threads.threads[1].comments[0].body, "");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn markup_is_dropped_but_code_keeps_its_angle_brackets() {
        assert_eq!(
            strip_markup(
                "<details>\n<summary>🔎 Fix</summary>\nUse `Vec<String>` if a < b</details><br/>"
            ),
            "\n🔎 Fix\nUse `Vec<String>` if a < b"
        );
        assert_eq!(strip_markup("<a href=\"x\">link</a> <T>"), "link <T>");
        assert_eq!(strip_markup("Table <span style=\"color:wh"), "Table ");
    }

    /// Opt-in against a real Azure DevOps checkout, read-only:
    /// `DROSTJE_AZDO_DIR=<checkout> DROSTJE_AZDO_BRANCH=<branch with a PR>
    /// cargo test --lib real_azdo -- --ignored --nocapture`
    #[test]
    #[ignore = "needs azdo, az login and an Azure DevOps checkout"]
    fn real_azdo_reads_a_branch_pull_request_and_its_threads() {
        let dir = PathBuf::from(std::env::var("DROSTJE_AZDO_DIR").unwrap());
        let branch = std::env::var("DROSTJE_AZDO_BRANCH").unwrap();
        let azdo = crate::platform::resolve_executable("azdo", None).unwrap();
        let prs = pull_requests_for_branch(&azdo, &dir, &branch).unwrap();
        println!("{prs:#?}");
        let pr = pull_request(&azdo, &dir, prs[0].id).unwrap();
        println!("{pr:#?}");
        let threads = review_threads(&azdo, &dir, pr.id).unwrap();
        for thread in &threads.threads {
            println!(
                "{} {:?} {:?}:{:?} {} comment(s): {:.80}",
                thread.id,
                thread.status,
                thread.file,
                thread.line,
                thread.comments.len(),
                thread.comments[0].body.replace('\n', " ")
            );
        }
        assert_eq!(pr.id, prs[0].id);
    }
}
