use super::git;
use anyhow::{Context, Result, bail};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchStatus {
    pub name: String,
    pub remote: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub upstream_gone: bool,
    pub worktree: Option<PathBuf>,
}

impl BranchStatus {
    pub fn is_remote_only(&self) -> bool {
        self.remote.is_some()
    }
}

pub fn list_branches(root: &Path) -> Result<Vec<BranchStatus>> {
    const FORMAT: &str = "--format=%(refname)%00%(refname:lstrip=2)%00%(symref)%00%(upstream:short)%00%(upstream:track,nobracket)%00%(worktreepath)";
    let remotes: Vec<String> = git(root, &["remote"])?
        .lines()
        .map(str::to_owned)
        .filter(|remote| !remote.is_empty())
        .collect();
    let listing = git(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            FORMAT,
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    parse_branches(&listing, &remotes)
}

fn parse_branches(listing: &str, remotes: &[String]) -> Result<Vec<BranchStatus>> {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for line in listing.lines() {
        let fields: Vec<&str> = line.split('\0').collect();
        let [refname, short, symref, upstream, track, worktree] = fields[..] else {
            bail!("Git listed a branch in an unexpected format: {line:?}");
        };
        if !symref.is_empty() {
            continue;
        }
        if refname.starts_with("refs/heads/") {
            let (ahead, behind, upstream_gone) = parse_track(track);
            local.push(BranchStatus {
                name: short.to_owned(),
                remote: None,
                upstream: (!upstream.is_empty()).then(|| upstream.to_owned()),
                ahead,
                behind,
                upstream_gone,
                worktree: (!worktree.is_empty()).then(|| canonical(worktree)),
            });
        } else if let Some((owner, name)) = remotes
            .iter()
            .filter_map(|candidate| {
                short
                    .strip_prefix(candidate.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(|name| (candidate, name))
            })
            .max_by_key(|(candidate, _)| candidate.len())
        {
            remote.push((short.to_owned(), owner.clone(), name.to_owned()));
        }
    }
    let mut claimed: HashSet<String> = local.iter().map(|branch| branch.name.clone()).collect();
    let tracked: HashSet<String> = local
        .iter()
        .filter_map(|branch| branch.upstream.clone())
        .collect();
    for (short, owner, name) in remote {
        if tracked.contains(&short) || !claimed.insert(name.clone()) {
            continue;
        }
        local.push(BranchStatus {
            name,
            remote: Some(owner),
            ..BranchStatus::default()
        });
    }
    Ok(local)
}

fn parse_track(track: &str) -> (u32, u32, bool) {
    let mut ahead = 0;
    let mut behind = 0;
    for part in track.split(", ") {
        if part == "gone" {
            return (0, 0, true);
        } else if let Some(count) = part.strip_prefix("ahead ") {
            ahead = count.parse().unwrap_or(0);
        } else if let Some(count) = part.strip_prefix("behind ") {
            behind = count.parse().unwrap_or(0);
        }
    }
    (ahead, behind, false)
}

fn canonical(path: &str) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

pub fn fetch_remotes(root: &Path) -> Result<bool> {
    fetch_remotes_within(root, FETCH_TIMEOUT)
}

fn fetch_remotes_within(root: &Path, timeout: Duration) -> Result<bool> {
    if git(root, &["remote"])?.trim().is_empty() {
        return Ok(false);
    }
    let mut child = super::git_command(root)
        .args(["fetch", "--all", "--prune", "--quiet"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("Run Git fetch")?;
    let mut stderr = child.stderr.take().context("Run Git fetch")?;
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().context("Run Git fetch")? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "Git fetch did not finish within {} s; check the network or the remote's credentials",
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        bail!("Git fetch: {}", errors.trim());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn run(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fixture() -> (Fixture, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("drostje-branches-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let dir = canonical(dir.to_str().unwrap());
        let origin = dir.join("origin.git");
        let clone = dir.join("clone");
        run(
            &dir,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );
        run(
            &dir,
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        run(&clone, &["config", "user.name", "DROSTJE."]);
        run(&clone, &["config", "user.email", "drostje@example.invalid"]);
        run(&clone, &["commit", "-q", "--allow-empty", "-m", "first"]);
        run(&clone, &["push", "-q", "-u", "origin", "main"]);
        (Fixture(dir), origin, clone)
    }

    #[test]
    fn tracking_counts_remote_only_branches_and_worktrees_are_listed() {
        let (fixture, _origin, clone) = fixture();
        run(&clone, &["switch", "-q", "-c", "ahead"]);
        run(&clone, &["commit", "-q", "--allow-empty", "-m", "local"]);
        run(&clone, &["push", "-q", "-u", "origin", "ahead"]);
        run(&clone, &["commit", "-q", "--allow-empty", "-m", "unpushed"]);
        run(&clone, &["switch", "-q", "-c", "shared", "main"]);
        run(&clone, &["push", "-q", "origin", "shared:remote-only"]);
        run(&clone, &["switch", "-q", "main"]);
        let linked = fixture.0.join("linked");
        run(
            &clone,
            &["worktree", "add", "-q", linked.to_str().unwrap(), "shared"],
        );
        run(&clone, &["fetch", "-q"]);

        let branches = list_branches(&clone).unwrap();
        let find = |name: &str| {
            branches
                .iter()
                .find(|branch| branch.name == name)
                .unwrap_or_else(|| panic!("{name} listed in {branches:?}"))
                .clone()
        };
        let ahead = find("ahead");
        assert_eq!((ahead.ahead, ahead.behind), (1, 0));
        assert_eq!(ahead.upstream.as_deref(), Some("origin/ahead"));
        assert_eq!(ahead.worktree, None);
        assert_eq!(find("main").worktree, Some(clone.clone()));
        assert_eq!(find("shared").worktree, Some(linked));
        let remote = find("remote-only");
        assert_eq!(remote.remote.as_deref(), Some("origin"));
        assert!(remote.is_remote_only());
        assert_eq!(
            branches
                .iter()
                .filter(|branch| branch.name == "main")
                .count(),
            1,
            "a remote branch already tracked locally is listed once"
        );
        assert!(
            branches.iter().all(|branch| branch.name != "HEAD"),
            "origin/HEAD is a symbolic ref, not a branch"
        );
    }

    #[test]
    fn fetching_brings_in_new_remote_branches_and_marks_deleted_upstreams_gone() {
        let (fixture, origin, clone) = fixture();
        run(&clone, &["switch", "-q", "-c", "doomed"]);
        run(&clone, &["push", "-q", "-u", "origin", "doomed"]);
        run(&clone, &["switch", "-q", "main"]);
        let other = fixture.0.join("other");
        run(
            &fixture.0,
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        run(&other, &["config", "user.name", "DROSTJE."]);
        run(&other, &["config", "user.email", "drostje@example.invalid"]);
        run(&other, &["commit", "-q", "--allow-empty", "-m", "upstream"]);
        run(&other, &["push", "-q", "origin", "main", "main:fresh"]);
        run(&other, &["push", "-q", "origin", "--delete", "doomed"]);

        assert!(fetch_remotes(&clone).unwrap());
        let branches = list_branches(&clone).unwrap();
        let main = branches.iter().find(|b| b.name == "main").unwrap();
        assert_eq!((main.ahead, main.behind), (0, 1));
        assert!(
            branches
                .iter()
                .find(|b| b.name == "doomed")
                .unwrap()
                .upstream_gone
        );
        assert_eq!(
            branches
                .iter()
                .find(|b| b.name == "fresh")
                .and_then(|b| b.remote.as_deref()),
            Some("origin")
        );
    }

    #[test]
    fn a_repository_without_remotes_skips_fetching_and_a_broken_remote_says_why() {
        let (_fixture, _origin, clone) = fixture();
        run(&clone, &["remote", "remove", "origin"]);
        assert!(!fetch_remotes(&clone).unwrap());
        run(
            &clone,
            &["remote", "add", "origin", "/nonexistent/drostje-remote.git"],
        );
        let error = format!("{:#}", fetch_remotes(&clone).unwrap_err());
        assert!(error.starts_with("Git fetch: "), "{error}");
    }

    #[test]
    fn tracking_text_reads_ahead_behind_and_gone() {
        assert_eq!(parse_track(""), (0, 0, false));
        assert_eq!(parse_track("ahead 3"), (3, 0, false));
        assert_eq!(parse_track("behind 2"), (0, 2, false));
        assert_eq!(parse_track("ahead 3, behind 2"), (3, 2, false));
        assert_eq!(parse_track("gone"), (0, 0, true));
    }

    #[test]
    fn remote_branches_resolve_against_the_longest_remote_name() {
        let listing = "refs/remotes/team/a/feature\0team/a/feature\0\0\0\0\n\
                       refs/remotes/team/feature\0team/feature\0\0\0\0\n";
        let remotes = ["team".to_owned(), "team/a".to_owned()];
        let branches = parse_branches(listing, &remotes).unwrap();
        assert_eq!(
            branches
                .iter()
                .map(|b| (b.name.as_str(), b.remote.as_deref()))
                .collect::<Vec<_>>(),
            [("feature", Some("team/a"))],
            "the second `feature` is the same name from another remote and is listed once"
        );
    }
}
