//! Parity of the Git plumbing with the one-question-per-process implementation it replaced,
//! and the number of Git processes each call costs. Every fixture is a fresh `git init` under
//! the temp directory.

use super::{
    GIT_SPAWNS, RepositoryInfo, branch_is_merged, commit_all, create_worktree, has_changes,
    inspect_repository, inspect_worktree, obviously_valid_branch_name, switch_branch,
    validate_branch,
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// The implementations as they were before Git questions were combined, with uncounted
/// processes: the oracle every new result and error text is compared with.
mod before {
    use super::*;

    pub fn git(root: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .context("Run Git")?;
        ensure!(
            output.status.success(),
            "Git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8(output.stdout)
            .context("Git returned non-UTF-8 text")?
            .trim_end_matches(['\n', '\r'])
            .to_owned())
    }

    pub fn inspect_repository(path: &Path) -> Result<(PathBuf, Option<String>, Vec<String>, bool)> {
        let path =
            fs::canonicalize(path).with_context(|| format!("Read project {}", path.display()))?;
        ensure!(path.is_dir(), "Project is not a directory");
        ensure!(
            git(&path, &["rev-parse", "--is-bare-repository"])? == "false",
            "Bare repositories cannot be registered as projects"
        );
        let root = fs::canonicalize(git(&path, &["rev-parse", "--show-toplevel"])?)?;
        fs::read_dir(&root).context("Project root is unreadable")?;
        let branch = git(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
        let branches = git(
            &root,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )?
        .lines()
        .map(str::to_owned)
        .collect();
        let dirty = !git(&root, &["status", "--porcelain"])?.is_empty();
        Ok((root, branch, branches, dirty))
    }

    pub fn validate_branch(root: &Path, branch: &str) -> Result<()> {
        ensure!(!branch.trim().is_empty(), "Choose a target branch");
        git(root, &["check-ref-format", "--branch", branch])?;
        git(
            root,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{branch}^{{commit}}"),
            ],
        )?;
        Ok(())
    }

    pub fn branch_is_merged(root: &Path, branch: &str, base: &str) -> Result<bool> {
        validate_branch(root, base)?;
        validate_branch(root, branch)?;
        let code = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["merge-base", "--is-ancestor", branch, base])
            .output()?
            .status
            .code();
        match code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            code => anyhow::bail!("Git merge-base exited with {code:?}"),
        }
    }

    /// `validate_source`'s branch checks in `src/ui/factory/form.rs` before the change.
    pub fn factory_branch(root: &Path, branches: &[String], branch: &str) -> Result<(), String> {
        validate_branch(root, branch).map_err(|error| format!("{error:#}"))?;
        if !branches.iter().any(|name| name == branch) {
            return Err("Choose an existing local branch".to_owned());
        }
        Ok(())
    }
}

/// Git processes `f` starts on this thread.
fn spawns<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = GIT_SPAWNS.with(|spawns| spawns.get());
    let value = f();
    (value, GIT_SPAWNS.with(|spawns| spawns.get()) - start)
}

struct Fixture(PathBuf);

impl Fixture {
    fn empty() -> Self {
        let path = std::env::temp_dir().join(format!("drostje-git-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    /// A repository on `main` with one commit.
    fn repository() -> Self {
        let fixture = Self::unborn();
        fs::write(fixture.0.join("tracked.txt"), "before\n").unwrap();
        run(&fixture.0, &["add", "tracked.txt"]);
        commit(&fixture.0, "fixture");
        fixture
    }

    /// A repository on `main` with no commits yet.
    fn unborn() -> Self {
        let fixture = Self::empty();
        run(&fixture.0, &["init", "-q", "-b", "main"]);
        run(&fixture.0, &["config", "user.name", "DROSTJE."]);
        run(
            &fixture.0,
            &["config", "user.email", "drostje@example.invalid"],
        );
        fixture
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(root: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap()
}

fn commit(root: &Path, message: &str) {
    run(root, &["commit", "-q", "--allow-empty", "-m", message]);
}

fn rendered<T>(result: Result<T>) -> Result<T, String> {
    result.map_err(|error| format!("{error:#}"))
}

/// The new `inspect_repository` returns what the old one did, error text included.
fn assert_inspects_like_before(path: &Path) -> Result<RepositoryInfo, String> {
    let now = rendered(inspect_repository(path));
    let before = rendered(before::inspect_repository(path));
    match (&now, before) {
        (Ok(info), Ok((root, branch, branches, dirty))) => {
            assert_eq!(info.root, root, "{}", path.display());
            assert_eq!(info.branch, branch, "{}", path.display());
            assert_eq!(info.branches, branches, "{}", path.display());
            assert_eq!(info.dirty, dirty, "{}", path.display());
        }
        (Err(now), Err(before)) => assert_eq!(now, &before),
        (now, before) => panic!("{}: now {now:?}, before {before:?}", path.display()),
    }
    now
}

#[test]
fn clean_repository_is_inspected_in_three_git_processes_instead_of_five() {
    let repository = Fixture::repository();
    let (info, count) = spawns(|| inspect_repository(&repository.0).unwrap());
    assert_eq!(count, 3, "rev-parse, for-each-ref, status");
    assert_eq!(info.branch.as_deref(), Some("main"));
    assert_eq!(info.branches, ["main"]);
    assert!(!info.dirty);
    assert_inspects_like_before(&repository.0).unwrap();
}

#[test]
fn dirty_staged_and_untracked_changes_read_as_dirty_like_before() {
    let repository = Fixture::repository();
    fs::write(repository.0.join("tracked.txt"), "after\n").unwrap();
    assert!(assert_inspects_like_before(&repository.0).unwrap().dirty);
    run(&repository.0, &["add", "tracked.txt"]);
    assert!(assert_inspects_like_before(&repository.0).unwrap().dirty);
    run(&repository.0, &["commit", "-q", "-m", "staged"]);
    assert!(!assert_inspects_like_before(&repository.0).unwrap().dirty);
    fs::write(repository.0.join("untracked.txt"), "new\n").unwrap();
    let (info, count) = spawns(|| inspect_repository(&repository.0).unwrap());
    assert!(info.dirty);
    assert_eq!(count, 3);
    assert_inspects_like_before(&repository.0).unwrap();
}

#[test]
fn a_status_too_large_to_read_stops_early_and_leaves_no_index_lock() {
    let repository = Fixture::repository();
    // Far more porcelain output than a pipe buffers, so Git is still writing when we stop.
    // Top-level files: `status --porcelain` collapses an untracked directory to one line.
    for ix in 0..6000 {
        fs::write(
            repository
                .0
                .join(format!("untracked-file-with-a-long-name-{ix:05}.txt")),
            "",
        )
        .unwrap();
    }
    assert!(run(&repository.0, &["status", "--porcelain"]).len() > 256 * 1024);
    fs::write(repository.0.join("tracked.txt"), "after\n").unwrap();
    for _ in 0..3 {
        assert!(has_changes(&repository.0).unwrap());
        assert!(!repository.0.join(".git/index.lock").exists());
    }
    assert!(assert_inspects_like_before(&repository.0).unwrap().dirty);
    // Git still works on the repository afterwards.
    run(&repository.0, &["add", "--all"]);
    commit(&repository.0, "many");
    assert!(!has_changes(&repository.0).unwrap());
}

#[test]
fn subdirectories_resolve_to_the_work_tree_root_like_before() {
    let repository = Fixture::repository();
    let nested = repository.0.join("a/b");
    fs::create_dir_all(&nested).unwrap();
    let info = assert_inspects_like_before(&nested).unwrap();
    assert_eq!(info.root, fs::canonicalize(&repository.0).unwrap());
}

#[test]
fn refusals_report_the_same_errors_as_before() {
    let bare = Fixture::empty();
    run(&bare.0, &["init", "-q", "--bare"]);
    let error = assert_inspects_like_before(&bare.0).unwrap_err();
    assert_eq!(error, "Bare repositories cannot be registered as projects");

    let plain = Fixture::empty();
    let error = assert_inspects_like_before(&plain.0).unwrap_err();
    assert!(
        error.starts_with("Git rev-parse --is-bare-repository: fatal: not a git repository"),
        "{error}"
    );

    let repository = Fixture::repository();
    let error = assert_inspects_like_before(&repository.0.join(".git")).unwrap_err();
    assert!(
        error.starts_with("Git rev-parse --show-toplevel: fatal:"),
        "{error}"
    );

    let error = assert_inspects_like_before(&repository.0.join("missing")).unwrap_err();
    assert!(error.starts_with("Read project "), "{error}");
    let error = assert_inspects_like_before(&repository.0.join("tracked.txt")).unwrap_err();
    assert_eq!(error, "Project is not a directory");

    fs::write(repository.0.join(".git/index"), "corrupt").unwrap();
    let error = assert_inspects_like_before(&repository.0).unwrap_err();
    assert!(error.starts_with("Git status --porcelain: "), "{error}");
}

#[test]
fn detached_unborn_and_ambiguous_heads_name_the_branch_like_before() {
    let unborn = Fixture::unborn();
    let (info, count) = spawns(|| assert_inspects_like_before(&unborn.0).unwrap());
    assert_eq!(info.branch.as_deref(), Some("main"));
    assert!(info.branches.is_empty());
    assert_eq!(count, 4, "no listed branch, so symbolic-ref is asked");

    let repository = Fixture::repository();
    run(&repository.0, &["checkout", "-q", "--detach"]);
    assert_eq!(
        assert_inspects_like_before(&repository.0).unwrap().branch,
        None
    );

    // A tag with the branch's name: both commands disambiguate to `heads/v1`.
    run(&repository.0, &["tag", "v1"]);
    run(&repository.0, &["switch", "-q", "-c", "v1"]);
    let info = assert_inspects_like_before(&repository.0).unwrap();
    assert_eq!(info.branch.as_deref(), Some("heads/v1"));

    // A remote with the branch's name: only `for-each-ref` disambiguates, so Git is asked.
    run(&repository.0, &["branch", "origin", "main"]);
    run(
        &repository.0,
        &["update-ref", "refs/remotes/origin/HEAD", "HEAD"],
    );
    run(&repository.0, &["switch", "-q", "origin"]);
    let info = assert_inspects_like_before(&repository.0).unwrap();
    assert_eq!(info.branch.as_deref(), Some("origin"));
    assert!(info.branches.contains(&"heads/origin".to_owned()));

    // HEAD on a symbolic branch: the listing cannot tell which name Git reports, so Git is asked.
    run(&repository.0, &["switch", "-q", "main"]);
    run(
        &repository.0,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );
    run(&repository.0, &["symbolic-ref", "HEAD", "refs/heads/alias"]);
    let (info, count) = spawns(|| assert_inspects_like_before(&repository.0).unwrap());
    assert!(info.branch.is_some());
    assert_eq!(
        count, 4,
        "a symbolic branch makes the listing ask symbolic-ref"
    );
}

#[test]
fn obviously_valid_branch_names_are_names_git_accepts() {
    let root = Fixture::empty();
    let names = [
        "main",
        "drostje/amber-heron",
        "feature/x-1.2_b",
        "v1",
        "a.b",
        "-x",
        "x-",
        "HEAD",
        "HEAD/x",
        "x/HEAD",
        "@",
        "@{-1}",
        "a@b",
        "a..b",
        ".a",
        "a/.b",
        "a.",
        "a./b",
        "a.lock",
        "a.lock/b",
        "a//b",
        "/a",
        "a/",
        "a b",
        "a~1",
        "a^",
        "a:b",
        "a?",
        "a*",
        "a[b",
        "a\\b",
        "é",
        "",
        "-",
        "refs/heads/x",
        "x.LOCK",
    ];
    for name in names {
        let git_accepts = Command::new("git")
            .arg("-C")
            .arg(&root.0)
            .args(["check-ref-format", "--branch", name])
            .output()
            .unwrap()
            .status
            .success();
        if obviously_valid_branch_name(name) {
            assert!(git_accepts, "{name:?} is not a valid branch name");
        }
    }
    for name in ["main", "drostje/amber-heron", "feature/x-1.2_b", "v1"] {
        assert!(obviously_valid_branch_name(name), "{name:?}");
    }
}

#[test]
fn validating_a_plain_branch_costs_one_git_process_and_errors_read_as_before() {
    let repository = Fixture::repository();
    run(&repository.0, &["tag", "v1"]);
    run(&repository.0, &["branch", "v1"]);
    let (result, count) = spawns(|| validate_branch(&repository.0, "main"));
    result.unwrap();
    assert_eq!(count, 1, "check-ref-format is skipped for a plain name");

    let missing_root = repository.0.join("missing");
    for (root, name) in [
        (&repository.0, "main"),
        (&repository.0, "v1"),
        (&repository.0, "heads/v1"),
        (&repository.0, "nope"),
        (&repository.0, ""),
        (&repository.0, "  "),
        (&repository.0, "bad..name"),
        (&repository.0, "-x"),
        (&repository.0, "HEAD"),
        (&repository.0, "@{-1}"),
        (&missing_root, "main"),
        (&missing_root, "bad..name"),
    ] {
        assert_eq!(
            rendered(validate_branch(root, name)),
            rendered(before::validate_branch(root, name)),
            "{} {name:?}",
            root.display()
        );
    }
    assert!(
        rendered(validate_branch(&missing_root, "main"))
            .unwrap_err()
            .starts_with("Git check-ref-format --branch main: ")
    );
}

#[test]
fn verified_branches_are_exactly_ones_validation_would_pass() {
    let repository = Fixture::repository();
    run(&repository.0, &["tag", "v1"]);
    run(&repository.0, &["branch", "v1"]);
    run(&repository.0, &["branch", "drostje/amber-heron"]);
    run(
        &repository.0,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );
    // A branch that does not point at a commit, written past Git's own refusal.
    let tree = run(&repository.0, &["rev-parse", "HEAD^{tree}"]);
    fs::write(repository.0.join(".git/refs/heads/tree-branch"), tree).unwrap();
    let info = inspect_repository(&repository.0).unwrap();
    for name in ["main", "v1", "drostje/amber-heron", "alias"] {
        assert!(info.verified_branch(name), "{name}");
    }
    for name in ["heads/v1", "tree-branch", "nope", ""] {
        assert!(!info.verified_branch(name), "{name}");
    }
    for name in [
        "main",
        "v1",
        "heads/v1",
        "alias",
        "tree-branch",
        "nope",
        "",
        "bad..name",
    ] {
        if info.verified_branch(name) {
            validate_branch(&info.root, name).unwrap();
        }
        // The Factory form's branch checks, skipping validation when the listing proves it.
        let now = if info.verified_branch(name) {
            Ok(())
        } else {
            rendered(validate_branch(&info.root, name))
        }
        .and_then(|()| {
            if info.branches.iter().any(|branch| branch == name) {
                Ok(())
            } else {
                Err("Choose an existing local branch".to_owned())
            }
        });
        assert_eq!(
            now,
            before::factory_branch(&info.root, &info.branches, name),
            "{name:?}"
        );
    }
}

#[test]
fn merge_checks_take_two_git_processes_and_fail_like_before() {
    let repository = Fixture::repository();
    run(&repository.0, &["branch", "merged"]);
    run(&repository.0, &["switch", "-q", "-c", "ahead"]);
    commit(&repository.0, "ahead");
    run(&repository.0, &["switch", "-q", "main"]);

    let (merged, count) = spawns(|| branch_is_merged(&repository.0, "merged", "main").unwrap());
    assert!(merged);
    assert_eq!(count, 2, "one rev-parse for both branches, then merge-base");
    for (branch, base) in [
        ("merged", "main"),
        ("ahead", "main"),
        ("main", "ahead"),
        ("nope", "main"),
        ("merged", "nope"),
        ("nope", "gone"),
        ("bad..name", "main"),
        ("merged", ""),
        ("", "bad..name"),
    ] {
        assert_eq!(
            rendered(branch_is_merged(&repository.0, branch, base)),
            rendered(before::branch_is_merged(&repository.0, branch, base)),
            "{branch:?} into {base:?}"
        );
    }
}

#[test]
fn worktree_inspection_takes_five_git_processes_instead_of_eight() {
    let repository = Fixture::repository();
    let data = Fixture::empty();
    let path = data.0.join("amber-heron");
    let created = create_worktree(&repository.0, &path, "drostje/amber-heron", "main").unwrap();

    let (status, count) = spawns(|| {
        inspect_worktree(&repository.0, &created, "drostje/amber-heron", "main").unwrap()
    });
    assert_eq!(count, 5);
    assert!(status.exists && status.clean && status.merged);
    assert_eq!(status.head_branch.as_deref(), Some("drostje/amber-heron"));

    fs::write(created.join("scratch.txt"), "untracked\n").unwrap();
    commit(&created, "ahead");
    let status = inspect_worktree(&repository.0, &created, "drostje/amber-heron", "main").unwrap();
    assert!(!status.clean && !status.merged);

    run(&created, &["checkout", "-q", "--detach"]);
    let status = inspect_worktree(&repository.0, &created, "drostje/amber-heron", "main").unwrap();
    assert_eq!(status.head_branch, None);

    let error = rendered(inspect_worktree(
        &repository.0,
        &created,
        "drostje/amber-heron",
        "nope",
    ))
    .unwrap_err();
    assert_eq!(
        error,
        rendered(before::validate_branch(&repository.0, "nope")).unwrap_err()
    );
}

#[test]
fn switching_and_committing_check_for_changes_with_the_same_messages() {
    let repository = Fixture::repository();
    let (result, count) = spawns(|| commit_all(&repository.0, "Nothing"));
    assert_eq!(
        rendered(result).unwrap_err(),
        "Nothing to commit; the working tree is clean"
    );
    assert_eq!(count, 1);

    fs::write(repository.0.join("untracked.txt"), "new\n").unwrap();
    assert_eq!(
        rendered(switch_branch(&repository.0, "side", true)).unwrap_err(),
        "The working tree has uncommitted changes; commit or stash them first"
    );
    commit_all(&repository.0, "Add untracked").unwrap();
    switch_branch(&repository.0, "side", true).unwrap();

    let plain = Fixture::empty();
    assert_eq!(
        rendered(has_changes(&plain.0)).unwrap_err(),
        rendered(before::git(&plain.0, &["status", "--porcelain"])).unwrap_err()
    );
}
