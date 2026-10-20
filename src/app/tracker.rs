//! Pull requests of worktree Conversations, owned by the App layer. A poll every few minutes
//! asks the project's tracker (see `crate::tracker`) about each live worktree Conversation:
//! an unlinked one is pinned once its branch has exactly one pull request, a pinned one has
//! its state and work items refreshed, and a Conversation whose pull request completed or was
//! abandoned is archived once, as soon as nothing runs in it. Review threads are read only
//! while a Review tab shows them. Tracker calls run off-thread; results carry a generation
//! and stale ones are dropped. Nothing is retried beyond the next scheduled poll.

use super::*;
use crate::tracker::{
    PinDecision, PullRequestSummary, ReviewThreads, Tracker, TrackerError, TrackerTool,
    pin_decision,
};
use std::{path::Path, time::Duration};

const FIRST_POLL: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_secs(120);

/// Why a worktree Conversation shows no pinned pull request, or what stopped its upkeep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    /// The branch has no pull request opened since the worktree was created.
    NoneYet {
        branch: String,
    },
    /// Several pull requests qualify; DROSTJE. does not pick one.
    Ambiguous {
        branch: String,
        count: usize,
    },
    Detached,
    /// The project's `origin` is not on a tracker DROSTJE. integrates with.
    Untracked,
    /// The pull request closed; archiving waits until this reason clears.
    ArchivePending(String),
    Failed(TrackerError),
}

impl LinkStatus {
    /// One sentence saying what happened and, where there is one, the next action.
    pub fn describe(&self) -> String {
        match self {
            Self::NoneYet { branch } => format!("No pull request from {branch} yet."),
            Self::Ambiguous { branch, count } => {
                format!("{count} pull requests use {branch}; none was linked.")
            }
            Self::Detached => {
                "The worktree is detached; check out a branch to link a pull request.".into()
            }
            Self::Untracked => "This project's origin is not on Azure DevOps.".into(),
            Self::ArchivePending(reason) => {
                format!("The pull request closed; archiving waits: {reason}")
            }
            Self::Failed(error) => tracker_error_text(error),
        }
    }
}

/// A tracker failure with the next action for the kinds the user can fix.
pub fn tracker_error_text(error: &TrackerError) -> String {
    match error {
        TrackerError::Unavailable(message) => {
            format!("Azure DevOps is unavailable: {message}")
        }
        TrackerError::Auth(message) => {
            format!("Azure DevOps refused the request: {message}. Run az login, then refresh.")
        }
        TrackerError::NotFound(message) => format!("Azure DevOps: {message}"),
        TrackerError::Failed(message) => format!("Azure DevOps request failed: {message}"),
    }
}

/// The review threads of one Conversation's pull request as last read.
#[derive(Default)]
pub struct ReviewState {
    pub loading: bool,
    pub result: Option<Result<ReviewThreads, TrackerError>>,
    generation: u64,
}

#[derive(Default)]
pub struct PullRequests {
    pub status: HashMap<ThreadId, LinkStatus>,
    pub reviews: HashMap<ThreadId, ReviewState>,
    /// Threads whose open sidebar shows the Review tab in a visible pane.
    visible_reviews: HashSet<ThreadId>,
    /// A poll is in flight; the next tick waits for it rather than stacking another.
    polling: bool,
    poll_generation: u64,
}

/// One worktree Conversation to ask the tracker about.
struct Job {
    thread: ThreadId,
    project: ProjectId,
    root: PathBuf,
    dir: PathBuf,
    /// `None` when the worktree is detached.
    branch: Option<String>,
    /// When the worktree was created; older pull requests are an earlier use of the branch.
    since: String,
    pinned: Option<u64>,
}

enum Outcome {
    Link(PullRequestLink),
    Status(LinkStatus),
}

impl AppState {
    /// Starts the periodic poll; it ends with the AppState.
    pub(super) fn start_pull_request_polling(&self, cx: &mut Context<Self>) {
        // GPUI's timer, not smol's, so test schedulers drive the poll deterministically.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_POLL).await;
            loop {
                if this
                    .update(cx, |state, cx| state.poll_pull_requests(cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(POLL_INTERVAL).await;
            }
        })
        .detach();
    }

    /// The tracker CLI path configured in Settings, if any.
    fn tracker_executable_setting(&self, tracker: Tracker) -> Option<String> {
        self.snapshot
            .settings
            .get("executables")
            .and_then(|executables| executables.get(tracker.executable()))
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .map(str::to_owned)
    }

    fn pull_request_jobs(&self) -> Vec<Job> {
        self.snapshot
            .threads
            .iter()
            .filter(|thread| thread.kind == ThreadKind::Conversation && !thread.archived)
            .filter_map(|thread| {
                let project = self
                    .snapshot
                    .projects
                    .iter()
                    .find(|project| project.id == thread.project_id && !project.archived)?;
                let worktree = self.thread_worktree(thread).filter(|w| w.is_live())?;
                let branch = match self.worktree_heads.get(&worktree.id) {
                    Some(head) => head.clone(),
                    None => Some(worktree.branch.clone()),
                };
                Some(Job {
                    thread: thread.id,
                    project: project.id,
                    root: project.root.clone(),
                    dir: worktree.path.clone(),
                    branch,
                    since: worktree.created_at.clone(),
                    pinned: thread.pull_request.as_ref().map(|pr| pr.id),
                })
            })
            .collect()
    }

    /// Asks the tracker about every live worktree Conversation in one off-thread pass.
    pub fn poll_pull_requests(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down || self.pull_requests.polling {
            return;
        }
        let jobs = self.pull_request_jobs();
        if jobs.is_empty() {
            return;
        }
        let explicit = self.tracker_executable_setting(Tracker::AzureDevOps);
        self.pull_requests.polling = true;
        self.pull_requests.poll_generation += 1;
        let generation = self.pull_requests.poll_generation;
        cx.spawn(async move |this, cx| {
            let outcomes = smol::unblock(move || run_jobs(jobs, explicit.as_deref())).await;
            let _ = this.update(cx, |state, cx| {
                if state.pull_requests.poll_generation != generation {
                    return;
                }
                state.pull_requests.polling = false;
                state.pull_requests_polled(outcomes, cx);
            });
        })
        .detach();
    }

    fn pull_requests_polled(&mut self, outcomes: Vec<(ThreadId, Outcome)>, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let mut changed = false;
        for (id, outcome) in outcomes {
            let Some(thread) = self.snapshot.threads.iter().find(|t| t.id == id) else {
                continue;
            };
            let updated = match outcome {
                Outcome::Link(link) => {
                    let Some(link) = merge_link(thread, link) else {
                        continue;
                    };
                    let cleared = self.pull_requests.status.remove(&id).is_some();
                    if thread.pull_request.as_ref() != Some(&link) {
                        let mut thread = thread.clone();
                        thread.pull_request = Some(link);
                        self.save_thread(thread);
                        true
                    } else {
                        cleared
                    }
                }
                Outcome::Status(status) => {
                    self.pull_requests.status.insert(id, status.clone()) != Some(status)
                }
            };
            // Polls repeat every few minutes; only a real change redraws.
            if updated {
                cx.emit(PresentationChange::Thread(id));
                changed = true;
            }
        }
        if self.settle_closed_pull_requests() || changed {
            cx.emit(PresentationChange::Projects);
            cx.notify();
        }
    }

    /// Archives each Conversation whose pinned pull request closed and that DROSTJE. has not
    /// archived for it yet. A Conversation that cannot be archived now (a running turn,
    /// another space) keeps its reason and is tried again after its turn or the next poll.
    /// Returns whether anything was archived.
    pub(super) fn settle_closed_pull_requests(&mut self) -> bool {
        let mut archived = false;
        let closed: Vec<_> = self
            .snapshot
            .threads
            .iter()
            .filter(|thread| awaits_archive(thread))
            .map(|thread| thread.id)
            .collect();
        for id in closed {
            let cause = self
                .snapshot
                .threads
                .iter()
                .find(|thread| thread.id == id)
                .and_then(|thread| thread.pull_request.as_ref())
                .map(|pr| {
                    let verb = match pr.state {
                        PullRequestState::Completed => "completed",
                        PullRequestState::Abandoned => "abandoned",
                        PullRequestState::Active | PullRequestState::Unknown(_) => "closed",
                    };
                    format!("pull request {} was {verb}", pr.id)
                })
                .unwrap_or_default();
            match self.archive_thread_because(id, Some(&cause)) {
                Ok(()) => {
                    archived = true;
                    self.pull_requests.status.remove(&id);
                    if let Some(mut thread) = self
                        .snapshot
                        .threads
                        .iter()
                        .find(|thread| thread.id == id)
                        .cloned()
                        && let Some(pr) = thread.pull_request.as_mut()
                    {
                        pr.archived_on_close = true;
                        self.save_thread(thread);
                    }
                }
                Err(error) => {
                    self.pull_requests
                        .status
                        .insert(id, LinkStatus::ArchivePending(format!("{error:#}")));
                }
            }
        }
        archived
    }

    /// Records which threads show their Review tab; a tab that just appeared is read now.
    pub(super) fn set_visible_reviews(&mut self, ids: &[ThreadId], cx: &mut Context<Self>) {
        let visible: HashSet<_> = ids
            .iter()
            .copied()
            .filter(|&id| {
                self.thread_sidebar_open(id) && self.thread_sidebar_tab(id) == SidebarTab::Review
            })
            .collect();
        let appeared: Vec<_> = visible
            .difference(&self.pull_requests.visible_reviews)
            .copied()
            .collect();
        self.pull_requests.visible_reviews = visible;
        for id in appeared {
            self.refresh_reviews(id, cx);
        }
    }

    /// Re-reads the pinned pull request and its review threads for `id`.
    pub fn refresh_reviews(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let Some(thread) = self.snapshot.threads.iter().find(|t| t.id == id) else {
            return;
        };
        let Some(pr) = thread.pull_request.as_ref().map(|pr| pr.id) else {
            return;
        };
        let Some((_, dir)) = self.working_tree_of(thread) else {
            return;
        };
        let Some(root) = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == thread.project_id)
            .map(|project| project.root.clone())
        else {
            return;
        };
        let explicit = self.tracker_executable_setting(Tracker::AzureDevOps);
        let review = self.pull_requests.reviews.entry(id).or_default();
        review.generation += 1;
        review.loading = true;
        let generation = review.generation;
        cx.emit(PresentationChange::Review(id));
        cx.spawn(async move |this, cx| {
            let result = smol::unblock(move || {
                let dir = if dir.is_dir() { dir } else { root.clone() };
                let tool = match resolve_tool(&root, explicit.as_deref())? {
                    Some(tool) => tool,
                    None => {
                        return Err(TrackerError::Unavailable(
                            "This project's origin is not on Azure DevOps".into(),
                        ));
                    }
                };
                let link = tool.tracker.pull_request(&tool.executable, &dir, pr)?;
                let threads = tool.tracker.review_threads(&tool.executable, &dir, pr)?;
                Ok((link, threads))
            })
            .await;
            let _ = this.update(cx, |state, cx| {
                let Some(review) = state.pull_requests.reviews.get_mut(&id) else {
                    return;
                };
                if review.generation != generation {
                    return;
                }
                review.loading = false;
                let (link, threads) = match result {
                    Ok((link, threads)) => (Some(link), Ok(threads)),
                    Err(error) => (None, Err(error)),
                };
                review.result = Some(threads);
                cx.emit(PresentationChange::Review(id));
                if let Some(link) = link {
                    state.pull_requests_polled(vec![(id, Outcome::Link(link))], cx);
                }
            });
        })
        .detach();
    }

    /// Why the selected Conversation has no pull request to review or open.
    pub fn pull_request_reason(&self) -> Option<String> {
        let Some(thread) = self.conversation_thread() else {
            return Some("Select a conversation first.".into());
        };
        if thread.pull_request.is_some() {
            return None;
        }
        if thread.worktree_id.is_none() {
            return Some("Only conversations in their own worktree link a pull request.".into());
        }
        Some(
            self.pull_request_status(thread.id)
                .map(LinkStatus::describe)
                .unwrap_or_else(|| "No pull request is linked yet.".into()),
        )
    }

    /// Why no pull request poll can start now.
    pub fn refresh_pull_requests_reason(&self) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down.".into());
        }
        if self.pull_requests.polling {
            return Some("Pull requests are being read.".into());
        }
        self.pull_request_jobs()
            .is_empty()
            .then(|| "No conversation runs in its own worktree.".into())
    }

    /// The tracker failure the user must fix outside DROSTJE. (a missing CLI or a refused
    /// sign-in), as last seen by any poll or review read.
    pub fn tracker_setup_issue(&self) -> Option<String> {
        let setup = |error: &TrackerError| match error {
            TrackerError::Unavailable(_) | TrackerError::Auth(_) => Some(tracker_error_text(error)),
            TrackerError::NotFound(_) | TrackerError::Failed(_) => None,
        };
        self.pull_requests
            .status
            .values()
            .find_map(|status| match status {
                LinkStatus::Failed(error) => setup(error),
                _ => None,
            })
            .or_else(|| {
                self.pull_requests
                    .reviews
                    .values()
                    .find_map(|review| match &review.result {
                        Some(Err(error)) => setup(error),
                        _ => None,
                    })
            })
    }

    pub fn review_state(&self, id: ThreadId) -> Option<&ReviewState> {
        self.pull_requests.reviews.get(&id)
    }

    pub fn pull_request_status(&self, id: ThreadId) -> Option<&LinkStatus> {
        self.pull_requests.status.get(&id)
    }
}

/// The link to store after the tracker reported `link` for `thread`, or `None` when the
/// report is stale (the thread was pinned to another pull request while the poll ran).
fn merge_link(thread: &Thread, mut link: PullRequestLink) -> Option<PullRequestLink> {
    if let Some(old) = &thread.pull_request {
        if old.id != link.id {
            return None;
        }
        link.archived_on_close = old.archived_on_close;
    }
    // A Conversation the user archived before the pull request closed has nothing left to
    // archive; unarchiving it later must not undo that choice.
    if thread.archived && link.state.closed() {
        link.archived_on_close = true;
    }
    Some(link)
}

/// Its pinned pull request closed and DROSTJE. has not archived it for that yet.
fn awaits_archive(thread: &Thread) -> bool {
    !thread.archived
        && thread
            .pull_request
            .as_ref()
            .is_some_and(|pr| pr.state.closed() && !pr.archived_on_close)
}

/// Which tracker CLI serves the repository at `root`; `Ok(None)` when its `origin` is on
/// no tracker DROSTJE. integrates with, or it has no `origin`.
fn resolve_tool(root: &Path, explicit: Option<&str>) -> Result<Option<TrackerTool>, TrackerError> {
    let Ok(remote) = platform::git(root, &["remote", "get-url", "origin"]) else {
        return Ok(None);
    };
    let Some(tracker) = Tracker::recognize(&remote) else {
        return Ok(None);
    };
    let executable = platform::resolve_executable(tracker.executable(), explicit)
        .map_err(|error| TrackerError::Unavailable(format!("{error:#}")))?;
    Ok(Some(TrackerTool {
        tracker,
        executable,
    }))
}

fn run_jobs(jobs: Vec<Job>, explicit: Option<&str>) -> Vec<(ThreadId, Outcome)> {
    let mut tools: HashMap<ProjectId, Result<Option<TrackerTool>, TrackerError>> = HashMap::new();
    jobs.into_iter()
        .map(|job| {
            let tool = tools
                .entry(job.project)
                .or_insert_with(|| resolve_tool(&job.root, explicit))
                .clone();
            let outcome = match tool {
                Ok(Some(tool)) => run_job(&tool, &job),
                Ok(None) => Outcome::Status(LinkStatus::Untracked),
                Err(error) => Outcome::Status(LinkStatus::Failed(error)),
            };
            (job.thread, outcome)
        })
        .collect()
}

fn run_job(tool: &TrackerTool, job: &Job) -> Outcome {
    let fetch = |id| tool.tracker.pull_request(&tool.executable, &job.dir, id);
    let result = match (job.pinned, &job.branch) {
        (Some(id), _) => fetch(id).map(Outcome::Link),
        (None, None) => Ok(Outcome::Status(LinkStatus::Detached)),
        (None, Some(branch)) => tool
            .tracker
            .pull_requests_for_branch(&tool.executable, &job.dir, branch)
            .and_then(
                |prs: Vec<PullRequestSummary>| match pin_decision(&prs, &job.since) {
                    PinDecision::Pin(pr) => fetch(pr.id).map(Outcome::Link),
                    PinDecision::NoneYet => Ok(Outcome::Status(LinkStatus::NoneYet {
                        branch: branch.clone(),
                    })),
                    PinDecision::Ambiguous(count) => Ok(Outcome::Status(LinkStatus::Ambiguous {
                        branch: branch.clone(),
                        count,
                    })),
                },
            ),
    };
    result.unwrap_or_else(|error| Outcome::Status(LinkStatus::Failed(error)))
}

#[cfg(test)]
mod tests {
    use super::{awaits_archive, merge_link};
    use crate::model::{PullRequestLink, PullRequestState, Thread};
    use serde_json::json;

    fn thread(archived: bool, pull_request: Option<PullRequestLink>) -> Thread {
        let mut thread: Thread = serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "project_id": "00000000-0000-0000-0000-000000000002",
            "kind": "conversation", "factory": null, "title": "T", "harness": "omp",
            "session_ref": null, "model": null,
            "worktree_id": "00000000-0000-0000-0000-000000000003",
            "draft": "", "attachments": [], "submission": null, "archived": archived,
            "created_at": "2026-10-20T10:00:00Z", "updated_at": "2026-10-20T10:00:00Z",
        }))
        .unwrap();
        thread.pull_request = pull_request;
        thread
    }

    fn link(id: u64, state: PullRequestState, archived_on_close: bool) -> PullRequestLink {
        PullRequestLink {
            id,
            url: format!("https://dev.azure.com/o/p/_git/r/pullrequest/{id}"),
            title: "Fix upload".into(),
            state,
            work_items: vec![2596],
            archived_on_close,
        }
    }

    #[test]
    fn a_closed_pull_request_archives_its_conversation_once() {
        let open = thread(false, Some(link(7, PullRequestState::Active, false)));
        assert!(!awaits_archive(&open));
        for state in [PullRequestState::Completed, PullRequestState::Abandoned] {
            let closed = thread(false, Some(link(7, state.clone(), false)));
            assert!(awaits_archive(&closed), "{state:?}");
            // Archived for it once; the user unarchived it afterwards.
            let settled = thread(false, Some(link(7, state, true)));
            assert!(!awaits_archive(&settled));
        }
        // An unrecognised state is never read as closed.
        let unknown = thread(
            false,
            Some(link(7, PullRequestState::Unknown("x".into()), false)),
        );
        assert!(!awaits_archive(&unknown));
        assert!(!awaits_archive(&thread(false, None)));
    }

    #[test]
    fn tracker_reports_keep_the_pin_and_the_users_archive_choice() {
        let pinned = thread(false, Some(link(7, PullRequestState::Active, true)));
        // A report for a pull request the thread is no longer pinned to is stale.
        assert_eq!(
            merge_link(&pinned, link(8, PullRequestState::Completed, false)),
            None
        );
        // A refresh keeps whether DROSTJE. already archived for this pull request.
        let merged = merge_link(&pinned, link(7, PullRequestState::Completed, false)).unwrap();
        assert!(merged.archived_on_close);
        assert_eq!(merged.state, PullRequestState::Completed);
        // A first pin is taken as reported.
        let unpinned = thread(false, None);
        assert_eq!(
            merge_link(&unpinned, link(9, PullRequestState::Active, false)),
            Some(link(9, PullRequestState::Active, false))
        );
        // The user archived before the pull request closed: unarchiving later keeps it.
        let archived = thread(true, Some(link(7, PullRequestState::Active, false)));
        let merged = merge_link(&archived, link(7, PullRequestState::Abandoned, false)).unwrap();
        assert!(merged.archived_on_close);
        let mut unarchived = archived.clone();
        unarchived.archived = false;
        unarchived.pull_request = Some(merged);
        assert!(!awaits_archive(&unarchived));
    }
}
