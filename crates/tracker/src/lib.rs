//! Issue tracker seam: the pull request a worktree branch belongs to, its state and linked
//! work items, and its review threads. Only `azdo.rs` knows the `azdo` CLI's argv and JSON;
//! a GitHub tracker would sit beside it behind the same functions. Every call blocks on a
//! child process, so callers run it off the GPUI thread.
//!
//! Without the tracker's CLI none of this exists: the App layer reports it as a setup issue
//! and shows no pull request state rather than a guess.

pub mod azdo;

use drostje_model::{PullRequestLink, PullRequestState};
use std::path::{Path, PathBuf};

/// Which tracker hosts a repository, recognised from its `origin` URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tracker {
    AzureDevOps,
}

impl Tracker {
    /// The tracker behind `remote`, or `None` when DROSTJE. has no integration for its host.
    pub fn recognize(remote: &str) -> Option<Self> {
        let remote = remote.trim().to_ascii_lowercase();
        let host = remote
            .split_once("://")
            .map_or(remote.as_str(), |(_, rest)| rest)
            .split(['/', ':'])
            .next()
            .unwrap_or_default();
        let host = host.rsplit('@').next().unwrap_or(host);
        (host == "dev.azure.com"
            || host == "ssh.dev.azure.com"
            || host == "vs-ssh.visualstudio.com"
            || host.ends_with(".visualstudio.com"))
        .then_some(Self::AzureDevOps)
    }

    /// The CLI DROSTJE. drives for this tracker; the Settings executable override uses it.
    pub fn executable(self) -> &'static str {
        match self {
            Self::AzureDevOps => "azdo",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AzureDevOps => "Azure DevOps",
        }
    }

    pub fn pull_requests_for_branch(
        self,
        executable: &Path,
        dir: &Path,
        branch: &str,
    ) -> Result<Vec<PullRequestSummary>, TrackerError> {
        match self {
            Self::AzureDevOps => azdo::pull_requests_for_branch(executable, dir, branch),
        }
    }

    pub fn pull_request(
        self,
        executable: &Path,
        dir: &Path,
        id: u64,
    ) -> Result<PullRequestLink, TrackerError> {
        match self {
            Self::AzureDevOps => azdo::pull_request(executable, dir, id),
        }
    }

    pub fn review_threads(
        self,
        executable: &Path,
        dir: &Path,
        id: u64,
    ) -> Result<ReviewThreads, TrackerError> {
        match self {
            Self::AzureDevOps => azdo::review_threads(executable, dir, id),
        }
    }
}

/// A resolved tracker CLI for one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackerTool {
    pub tracker: Tracker,
    pub executable: PathBuf,
}

/// One pull request with a given source branch, as listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestSummary {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub state: PullRequestState,
    /// RFC 3339 UTC creation time as reported.
    pub created: String,
}

/// Whether a branch's pull requests identify exactly one to pin, and if not, why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinDecision {
    Pin(PullRequestSummary),
    /// No pull request was opened from the branch since the worktree was created.
    NoneYet,
    /// Several candidates; picking one would be a guess.
    Ambiguous(usize),
}

/// Pins the single active pull request opened from the branch since `since` (the worktree's
/// creation). With none active, a single closed one created since then is pinned too, so a
/// pull request completed while DROSTJE. was closed still ends its Conversation. Pull
/// requests older than the worktree belong to an earlier use of the branch name and never
/// count.
pub fn pin_decision(pull_requests: &[PullRequestSummary], since: &str) -> PinDecision {
    let candidates: Vec<_> = pull_requests
        .iter()
        .filter(|pr| not_before(&pr.created, since))
        .collect();
    let active: Vec<_> = candidates
        .iter()
        .filter(|pr| pr.state == PullRequestState::Active)
        .collect();
    match (active.as_slice(), candidates.as_slice()) {
        ([pr], _) => PinDecision::Pin((**pr).clone()),
        ([], []) => PinDecision::NoneYet,
        ([], [pr]) => PinDecision::Pin((*pr).clone()),
        ([], many) => PinDecision::Ambiguous(many.len()),
        (many, _) => PinDecision::Ambiguous(many.len()),
    }
}

/// Compares RFC 3339 UTC timestamps to the second; both sides use the `Z` form.
fn not_before(time: &str, since: &str) -> bool {
    let seconds = |t: &str| t.get(..19).map(str::to_owned);
    match (seconds(time), seconds(since)) {
        (Some(time), Some(since)) => time >= since,
        // An unreadable timestamp cannot prove the pull request is new.
        _ => false,
    }
}

/// A review thread's resolution as reported; unrecognised values are kept raw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewStatus {
    Active,
    Pending,
    Fixed,
    WontFix,
    Closed,
    ByDesign,
    Unknown(String),
}

impl ReviewStatus {
    /// Still asks something of the author.
    pub fn open(&self) -> bool {
        match self {
            Self::Active | Self::Pending => true,
            Self::Fixed | Self::WontFix | Self::Closed | Self::ByDesign | Self::Unknown(_) => false,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Active => "Active",
            Self::Pending => "Pending",
            Self::Fixed => "Resolved",
            Self::WontFix => "Won't fix",
            Self::Closed => "Closed",
            Self::ByDesign => "By design",
            Self::Unknown(raw) => raw,
        }
    }
}

/// A pull request review thread with its human-written comments, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewThread {
    pub id: u64,
    pub status: ReviewStatus,
    /// Repository path the thread is anchored to; `None` for general discussion.
    pub file: Option<String>,
    pub line: Option<u64>,
    /// Later pushes moved the anchored code.
    pub outdated: bool,
    pub comments: Vec<ReviewComment>,
    /// Comments the tracker counts, including any not returned as previews.
    pub total_comments: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewComment {
    pub id: u64,
    pub author: String,
    pub body: String,
    /// The tracker returned a preview; the full body is on the pull request page.
    pub truncated: bool,
    pub published: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewThreads {
    pub threads: Vec<ReviewThread>,
    /// More threads exist than one request returns.
    pub truncated: bool,
}

/// How a tracker call failed, in the terms the user acts on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TrackerError {
    /// The CLI could not be started.
    #[error("{0}")]
    Unavailable(String),
    /// The CLI is not signed in or the token was refused.
    #[error("{0}")]
    Auth(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(id: u64, state: PullRequestState, created: &str) -> PullRequestSummary {
        PullRequestSummary {
            id,
            title: format!("PR {id}"),
            url: format!("https://dev.azure.com/o/p/_git/r/pullrequest/{id}"),
            state,
            created: created.into(),
        }
    }

    #[test]
    fn recognizes_azure_devops_remotes_only() {
        for remote in [
            "git@ssh.dev.azure.com:v3/ComplianceFactory/Cootje%20Platform/Cootje%20Platform",
            "https://ComplianceFactory@dev.azure.com/ComplianceFactory/Proj/_git/Repo",
            "https://dev.azure.com/org/proj/_git/repo",
            "https://contoso.visualstudio.com/Proj/_git/Repo",
            "contoso@vs-ssh.visualstudio.com:v3/contoso/Proj/Repo",
        ] {
            assert_eq!(
                Tracker::recognize(remote),
                Some(Tracker::AzureDevOps),
                "{remote}"
            );
        }
        for remote in [
            "git@github.com:owner/repo.git",
            "https://github.com/owner/dev.azure.com",
            "https://example.com/dev.azure.com/repo",
            "",
        ] {
            assert_eq!(Tracker::recognize(remote), None, "{remote}");
        }
    }

    #[test]
    fn pins_the_single_active_pull_request_since_the_worktree() {
        let since = "2026-10-20T10:00:00Z";
        let old = pr(1, PullRequestState::Active, "2026-09-01T00:00:00.1Z");
        let closed = pr(2, PullRequestState::Abandoned, "2026-10-20T10:05:00.5Z");
        let active = pr(3, PullRequestState::Active, "2026-10-20T11:00:00.0Z");
        assert_eq!(
            pin_decision(&[old.clone(), closed.clone(), active.clone()], since),
            PinDecision::Pin(active.clone())
        );
        // A pull request from an earlier use of the branch name never counts.
        assert_eq!(pin_decision(&[old], since), PinDecision::NoneYet);
        // One closed while DROSTJE. was not watching still pins, so it can archive.
        assert_eq!(
            pin_decision(std::slice::from_ref(&closed), since),
            PinDecision::Pin(closed.clone())
        );
        let second = pr(4, PullRequestState::Completed, "2026-10-20T12:00:00Z");
        assert_eq!(
            pin_decision(&[closed, second], since),
            PinDecision::Ambiguous(2)
        );
        let twin = pr(5, PullRequestState::Active, "2026-10-20T12:00:00Z");
        assert_eq!(
            pin_decision(&[active, twin], since),
            PinDecision::Ambiguous(2)
        );
        assert_eq!(
            pin_decision(&[pr(6, PullRequestState::Active, "garbage")], since),
            PinDecision::NoneYet
        );
    }
}
