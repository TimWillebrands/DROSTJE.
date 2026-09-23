//! Naming and lifecycle rules for Conversation worktrees. Pure: no I/O and no Git here;
//! `platform` runs the commands and `app::worktrees` orchestrates them.
//!
//! A worktree starts on a random codename branch so the first turn has a directory before any
//! title exists, and is renamed once from the generated title. Names never derive from
//! user-editable text: the directory keeps the codename for the worktree's whole life.

use crate::{model::WorktreeId, platform::WorktreeStatus};
use std::path::{Path, PathBuf};

/// Every branch DROSTJE. creates lives under this namespace.
pub const BRANCH_PREFIX: &str = "drostje/";
/// Longest title slug used in a branch name.
const SLUG_LIMIT: usize = 40;
/// Highest numeric suffix tried before giving up on a branch name.
const SUFFIX_LIMIT: u32 = 99;

const ADJECTIVES: [&str; 64] = [
    "amber", "bold", "brisk", "calm", "clear", "cool", "crisp", "deep", "early", "fair", "fast",
    "fine", "firm", "fresh", "glad", "gold", "green", "grey", "hazy", "high", "keen", "kind",
    "late", "light", "lively", "lucid", "lunar", "mild", "misty", "neat", "noble", "north", "opal",
    "pale", "plain", "polar", "proud", "quick", "quiet", "rapid", "rare", "ripe", "rosy", "royal",
    "rustic", "sage", "sharp", "silent", "silver", "sleek", "slow", "snowy", "soft", "solar",
    "spry", "steady", "still", "stout", "sunny", "swift", "tidy", "warm", "wild", "young",
];
const NOUNS: [&str; 64] = [
    "heron", "otter", "falcon", "badger", "willow", "cedar", "maple", "birch", "river", "brook",
    "ridge", "harbor", "meadow", "orchard", "canyon", "glacier", "comet", "meteor", "nebula",
    "aurora", "ember", "lantern", "anchor", "compass", "beacon", "tide", "dune", "fjord", "delta",
    "marsh", "lynx", "fox", "hare", "wren", "finch", "robin", "sparrow", "swan", "crane", "pike",
    "trout", "salmon", "walrus", "seal", "bison", "elk", "moose", "raven", "magpie", "plover",
    "kestrel", "osprey", "quartz", "granite", "basalt", "marble", "cobalt", "copper", "iron",
    "pewter", "thistle", "clover", "fern", "moss",
];

/// A readable two-word name derived from the worktree identity, so a path never depends on a
/// title and the same identity always names the same directory.
pub fn codename(id: WorktreeId) -> String {
    let bits = id.as_u128();
    let adjective = ADJECTIVES[(bits & 0x3f) as usize];
    let noun = NOUNS[((bits >> 6) & 0x3f) as usize];
    format!("{adjective}-{noun}")
}

/// The codename, suffixed with identity hex when `taken` already claims it.
pub fn unique_codename(id: WorktreeId, taken: impl Fn(&str) -> bool) -> String {
    let base = codename(id);
    if !taken(&base) {
        return base;
    }
    let hex = id.simple().to_string();
    let short = format!("{base}-{}", &hex[..4]);
    if !taken(&short) {
        return short;
    }
    format!("{base}-{hex}")
}

/// The branch a worktree starts on before its Conversation has a title.
pub fn initial_branch(codename: &str) -> String {
    format!("{BRANCH_PREFIX}{codename}")
}

/// A Git-safe slug of a title: lowercase ASCII letters and digits joined by single dashes,
/// bounded, never empty. `None` when the title has nothing usable.
pub fn branch_slug(title: &str) -> Option<String> {
    let mut slug = String::new();
    let mut pending_dash = false;
    for ch in title.chars().flat_map(char::to_lowercase) {
        if !ch.is_ascii_alphanumeric() {
            pending_dash = true;
            continue;
        }
        let dash = pending_dash && !slug.is_empty();
        if slug.len() + usize::from(dash) + 1 > SLUG_LIMIT {
            break;
        }
        if dash {
            slug.push('-');
        }
        pending_dash = false;
        slug.push(ch);
    }
    let slug = slug.trim_end_matches('-');
    (!slug.is_empty()).then(|| slug.to_owned())
}

/// The namespaced branch for a generated title, or `None` when the title yields no slug.
pub fn titled_branch(title: &str) -> Option<String> {
    branch_slug(title).map(|slug| format!("{BRANCH_PREFIX}{slug}"))
}

/// `candidate` or the first `candidate-N` (N from 2) that `exists` rejects; `None` when every
/// suffix up to the limit is taken.
pub fn unique_branch(candidate: &str, exists: impl Fn(&str) -> bool) -> Option<String> {
    if !exists(candidate) {
        return Some(candidate.to_owned());
    }
    (2..=SUFFIX_LIMIT)
        .map(|n| format!("{candidate}-{n}"))
        .find(|name| !exists(name))
}

/// Where a worktree lives: `<data_dir>/worktrees/<project directory name>/<codename>`.
pub fn worktree_dir(data_dir: &Path, project_root: &Path, codename: &str) -> PathBuf {
    let project = project_root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("project");
    data_dir.join("worktrees").join(project).join(codename)
}

/// What to do with a worktree that no live Conversation uses any more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    /// Clean, on its own branch and merged into the base: remove directory and branch.
    Remove,
    /// The directory is already gone; record that and prune Git's bookkeeping.
    Forget,
    /// Leave it alone, with the reason shown to the user.
    Keep(&'static str),
}

/// Auto-removal never guesses: every condition must be observed true.
pub fn removal_decision(status: &WorktreeStatus, branch: &str) -> Removal {
    if !status.exists {
        return Removal::Forget;
    }
    if !status.clean {
        return Removal::Keep("it has uncommitted changes");
    }
    if status.head_branch.as_deref() != Some(branch) {
        return Removal::Keep("it is no longer on its own branch");
    }
    if !status.merged {
        return Removal::Keep("its branch is not merged into the base branch");
    }
    Removal::Remove
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn codenames_are_deterministic_two_word_names() {
        let id = Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0);
        assert_eq!(codename(id), codename(id));
        let name = codename(id);
        let (adjective, noun) = name.split_once('-').expect("two words");
        assert!(ADJECTIVES.contains(&adjective));
        assert!(NOUNS.contains(&noun));
        assert_ne!(codename(Uuid::from_u128(0)), codename(Uuid::from_u128(1)));
    }

    #[test]
    fn taken_codenames_get_identity_suffixes() {
        let id = Uuid::from_u128(0);
        let base = codename(id);
        assert_eq!(unique_codename(id, |_| false), base);
        let short = unique_codename(id, |name| name == base);
        assert_eq!(short, format!("{base}-0000"));
        let long = unique_codename(id, |name| name == base || name == short);
        assert_eq!(long, format!("{base}-{}", id.simple()));
    }

    #[test]
    fn slugs_are_lowercase_dashed_bounded_and_never_empty() {
        assert_eq!(
            branch_slug("Fix login  redirect!").as_deref(),
            Some("fix-login-redirect")
        );
        assert_eq!(
            branch_slug("  --Édit: café & thé  ").as_deref(),
            Some("dit-caf-th")
        );
        assert_eq!(
            branch_slug("Résumé 2026/09").as_deref(),
            Some("r-sum-2026-09")
        );
        assert_eq!(branch_slug("…"), None);
        assert_eq!(branch_slug(""), None);
        let long = branch_slug(&"word ".repeat(20)).unwrap();
        assert!(long.len() <= SLUG_LIMIT);
        assert!(!long.ends_with('-'));
        assert_eq!(
            titled_branch("Add worktrees").as_deref(),
            Some("drostje/add-worktrees")
        );
        assert_eq!(initial_branch("amber-heron"), "drostje/amber-heron");
    }

    #[test]
    fn unique_branch_suffixes_from_two_and_gives_up() {
        assert_eq!(
            unique_branch("drostje/x", |_| false).as_deref(),
            Some("drostje/x")
        );
        assert_eq!(
            unique_branch("drostje/x", |name| name == "drostje/x"
                || name == "drostje/x-2")
            .as_deref(),
            Some("drostje/x-3")
        );
        assert_eq!(unique_branch("drostje/x", |_| true), None);
    }

    #[test]
    fn worktree_dir_uses_project_directory_name_and_codename() {
        let dir = worktree_dir(
            Path::new("/data"),
            Path::new("/Users/tim/dev/drost"),
            "amber-heron",
        );
        assert_eq!(dir, PathBuf::from("/data/worktrees/drost/amber-heron"));
        let root = worktree_dir(Path::new("/data"), Path::new("/"), "x");
        assert_eq!(root, PathBuf::from("/data/worktrees/project/x"));
    }

    #[test]
    fn removal_requires_every_condition() {
        let ready = WorktreeStatus {
            exists: true,
            clean: true,
            head_branch: Some("drostje/x".into()),
            merged: true,
        };
        assert_eq!(removal_decision(&ready, "drostje/x"), Removal::Remove);
        let gone = WorktreeStatus {
            exists: false,
            ..ready.clone()
        };
        assert_eq!(removal_decision(&gone, "drostje/x"), Removal::Forget);
        let dirty = WorktreeStatus {
            clean: false,
            ..ready.clone()
        };
        assert!(matches!(
            removal_decision(&dirty, "drostje/x"),
            Removal::Keep(_)
        ));
        let detached = WorktreeStatus {
            head_branch: None,
            ..ready.clone()
        };
        assert!(matches!(
            removal_decision(&detached, "drostje/x"),
            Removal::Keep(_)
        ));
        assert!(matches!(
            removal_decision(&ready, "drostje/other"),
            Removal::Keep(_)
        ));
        let unmerged = WorktreeStatus {
            merged: false,
            ..ready
        };
        assert!(matches!(
            removal_decision(&unmerged, "drostje/x"),
            Removal::Keep(_)
        ));
    }
}
