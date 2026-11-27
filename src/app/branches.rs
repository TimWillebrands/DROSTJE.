use super::*;
use std::{path::Path, time::Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchStatus {
    Fetching,
    Fetched,
    NoRemotes,
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct BranchList {
    pub branches: Option<Result<Vec<platform::BranchStatus>, String>>,
    pub fetch: Option<FetchStatus>,
    listed_at: Option<Instant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchAction {
    Switch,
    Current,
    OpenWorkspace(Option<WorktreeId>),
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchChoice {
    pub name: String,
    pub action: BranchAction,
    pub detail: String,
    pub remote: bool,
}

pub enum BranchListing {
    Listed {
        project: ProjectId,
        started: Instant,
        result: Result<Vec<platform::BranchStatus>, String>,
    },
    Fetched {
        project: ProjectId,
        result: Result<bool, String>,
    },
}

impl BranchList {
    fn record_listing(
        &mut self,
        started: Instant,
        result: Result<Vec<platform::BranchStatus>, String>,
    ) -> bool {
        if self.listed_at.is_some_and(|listed| listed > started) {
            return false;
        }
        self.listed_at = Some(started);
        self.branches = Some(result);
        true
    }

    fn record_fetch(&mut self, result: Result<bool, String>) {
        self.fetch = Some(match result {
            Ok(true) => FetchStatus::Fetched,
            Ok(false) => FetchStatus::NoRemotes,
            Err(error) => FetchStatus::Failed(error),
        });
    }
}

fn choices_for(
    branches: &[platform::BranchStatus],
    own: &Path,
    root: Option<&Path>,
    worktrees: &[&Worktree],
) -> Vec<BranchChoice> {
    branches
        .iter()
        .map(|branch| {
            let (action, place) = match branch.worktree.as_deref() {
                None => (BranchAction::Switch, None),
                Some(path) if path == own => (BranchAction::Current, Some("current".into())),
                Some(path) if Some(path) == root => (
                    BranchAction::OpenWorkspace(None),
                    Some("in current checkout".to_owned()),
                ),
                Some(path) => match worktrees.iter().find(|w| w.is_live() && w.path == path) {
                    Some(worktree) => (
                        BranchAction::OpenWorkspace(Some(worktree.id)),
                        Some(format!("in worktree {}", worktree.codename)),
                    ),
                    None => (
                        BranchAction::Unavailable,
                        Some(format!("checked out in {}", path.display())),
                    ),
                },
            };
            let detail = [
                place,
                branch.remote.as_ref().map(|remote| format!("on {remote}")),
                tracking(branch),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            BranchChoice {
                name: branch.name.clone(),
                action,
                detail,
                remote: branch.is_remote_only(),
            }
        })
        .collect()
}

fn tracking(branch: &platform::BranchStatus) -> Option<String> {
    if branch.upstream_gone {
        return Some("upstream gone".into());
    }
    branch.upstream.as_ref()?;
    match (branch.ahead, branch.behind) {
        (0, 0) => Some("up to date".into()),
        (ahead, 0) => Some(format!("↑{ahead}")),
        (0, behind) => Some(format!("↓{behind}")),
        (ahead, behind) => Some(format!("↑{ahead} ↓{behind}")),
    }
}

impl AppState {
    pub fn branch_list(&self, project: ProjectId) -> Option<&BranchList> {
        self.branch_lists.get(&project)
    }

    pub fn refresh_branches(&mut self, project: ProjectId, fetch: bool) {
        let Some(root) = self
            .snapshot
            .projects
            .iter()
            .find(|candidate| candidate.id == project)
            .map(|project| project.root.clone())
        else {
            return;
        };
        let list = self.branch_lists.entry(project).or_default();
        let fetch = fetch && list.fetch != Some(FetchStatus::Fetching);
        if fetch {
            list.fetch = Some(FetchStatus::Fetching);
        }
        let sender = self.events.clone();
        smol::spawn(async move {
            let list = |root: PathBuf| async move {
                smol::unblock(move || {
                    let started = Instant::now();
                    let result =
                        platform::list_branches(&root).map_err(|error| format!("{error:#}"));
                    BranchListing::Listed {
                        project,
                        started,
                        result,
                    }
                })
                .await
            };
            if sender
                .send(AppEvent::Branches(list(root.clone()).await))
                .await
                .is_err()
                || !fetch
            {
                return;
            }
            let fetched = {
                let root = root.clone();
                smol::unblock(move || {
                    platform::fetch_remotes(&root).map_err(|error| format!("{error:#}"))
                })
                .await
            };
            let _ = sender
                .send(AppEvent::Branches(BranchListing::Fetched {
                    project,
                    result: fetched,
                }))
                .await;
            let _ = sender.send(AppEvent::Branches(list(root).await)).await;
        })
        .detach();
    }

    pub(super) fn branches_received(&mut self, listing: BranchListing, cx: &mut Context<Self>) {
        let changed = match listing {
            BranchListing::Listed {
                project,
                started,
                result,
            } => self
                .branch_lists
                .entry(project)
                .or_default()
                .record_listing(started, result),
            BranchListing::Fetched { project, result } => {
                self.branch_lists
                    .entry(project)
                    .or_default()
                    .record_fetch(result);
                true
            }
        };
        if changed {
            cx.emit(PresentationChange::Projects);
        }
    }

    pub(super) fn relist_branches(&mut self, project: ProjectId) {
        if self.branch_lists.contains_key(&project) {
            self.refresh_branches(project, false);
        }
    }

    pub fn branch_choices(&self, thread: ThreadId) -> Option<Result<Vec<BranchChoice>, String>> {
        let thread = self.snapshot.threads.iter().find(|t| t.id == thread)?;
        let (_, own) = self.working_tree_of(thread)?;
        let root = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == thread.project_id)
            .map(|project| project.root.as_path());
        let branches = match self
            .branch_lists
            .get(&thread.project_id)?
            .branches
            .as_ref()?
        {
            Ok(branches) => branches,
            Err(error) => return Some(Err(error.clone())),
        };
        let worktrees: Vec<&Worktree> = self
            .snapshot
            .worktrees
            .iter()
            .filter(|worktree| worktree.project_id == thread.project_id)
            .collect();
        Some(Ok(choices_for(branches, &own, root, &worktrees)))
    }
}

#[cfg(test)]
mod tests {
    use super::{BranchAction, BranchList, FetchStatus, choices_for};
    use crate::{model::Worktree, platform};
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };
    use uuid::Uuid;

    fn worktree(codename: &str, live: bool) -> Worktree {
        Worktree {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            path: PathBuf::from(format!("/worktrees/{codename}")),
            codename: codename.into(),
            branch: format!("{codename}-branch"),
            base: "main".into(),
            named: true,
            created_by: Uuid::new_v4(),
            created_at: "2026-01-01T00:00:00Z".into(),
            removed_at: (!live).then(|| "2026-01-02T00:00:00Z".into()),
        }
    }

    fn branch(name: &str, worktree: Option<&str>) -> platform::BranchStatus {
        platform::BranchStatus {
            name: name.into(),
            worktree: worktree.map(PathBuf::from),
            ..Default::default()
        }
    }

    #[test]
    fn choices_say_where_each_branch_is_checked_out_and_how_it_tracks() {
        let otter = worktree("otter", true);
        let heron = worktree("heron", false);
        let branches = [
            branch("main", Some("/project")),
            branch("feature", Some("/worktrees/otter")),
            branch("abandoned", Some("/worktrees/heron")),
            branch("external", Some("/elsewhere")),
            platform::BranchStatus {
                upstream: Some("origin/diverged".into()),
                ahead: 2,
                behind: 1,
                ..branch("diverged", None)
            },
            platform::BranchStatus {
                upstream: Some("origin/synced".into()),
                ..branch("synced", None)
            },
            platform::BranchStatus {
                upstream: Some("origin/merged".into()),
                upstream_gone: true,
                ..branch("merged", None)
            },
            branch("local-only", None),
            platform::BranchStatus {
                remote: Some("origin".into()),
                ..branch("theirs", None)
            },
            platform::BranchStatus {
                upstream: Some("origin/tracked".into()),
                ahead: 1,
                ..branch("tracked", Some("/worktrees/otter"))
            },
        ];
        let worktrees = [&otter, &heron];
        let root = PathBuf::from("/project");

        let from_root = choices_for(&branches, &root, Some(&root), &worktrees);
        let shown = |name: &str| {
            let choice = from_root.iter().find(|c| c.name == name).unwrap();
            (choice.action.clone(), choice.detail.clone(), choice.remote)
        };
        assert_eq!(
            shown("main"),
            (BranchAction::Current, "current".into(), false)
        );
        assert_eq!(
            shown("feature"),
            (
                BranchAction::OpenWorkspace(Some(otter.id)),
                "in worktree otter".into(),
                false
            )
        );
        assert_eq!(
            shown("abandoned"),
            (
                BranchAction::Unavailable,
                "checked out in /worktrees/heron".into(),
                false
            ),
            "a removed worktree cannot be opened"
        );
        assert_eq!(
            shown("external"),
            (
                BranchAction::Unavailable,
                "checked out in /elsewhere".into(),
                false
            )
        );
        assert_eq!(
            shown("diverged"),
            (BranchAction::Switch, "↑2 ↓1".into(), false)
        );
        assert_eq!(shown("synced").1, "up to date");
        assert_eq!(shown("merged").1, "upstream gone");
        assert_eq!(shown("local-only").1, "");
        assert_eq!(
            shown("theirs"),
            (BranchAction::Switch, "on origin".into(), true)
        );
        assert_eq!(shown("tracked").1, "in worktree otter · ↑1");

        let from_otter = choices_for(&branches, &otter.path, Some(&root), &worktrees);
        let main = from_otter.iter().find(|c| c.name == "main").unwrap();
        assert_eq!(main.action, BranchAction::OpenWorkspace(None));
        assert_eq!(main.detail, "in current checkout");
        assert_eq!(
            from_otter
                .iter()
                .find(|c| c.name == "feature")
                .unwrap()
                .action,
            BranchAction::Current
        );
    }

    #[test]
    fn an_older_listing_never_replaces_a_newer_one() {
        let earlier = Instant::now();
        let later = earlier + Duration::from_millis(5);
        let mut list = BranchList::default();
        assert!(list.record_listing(later, Ok(vec![branch("fresh", None)])));
        assert!(!list.record_listing(earlier, Ok(vec![branch("stale", None)])));
        assert_eq!(
            list.branches,
            Some(Ok(vec![branch("fresh", None)])),
            "the listing that started last wins, whatever order results arrive in"
        );
        assert!(list.record_listing(later, Err("Git for-each-ref: broken".into())));
        assert_eq!(list.branches, Some(Err("Git for-each-ref: broken".into())));
    }

    #[test]
    fn fetch_outcomes_are_kept_as_reported() {
        let mut list = BranchList::default();
        for (result, status) in [
            (Ok(true), FetchStatus::Fetched),
            (Ok(false), FetchStatus::NoRemotes),
            (
                Err("Git fetch: offline".to_owned()),
                FetchStatus::Failed("Git fetch: offline".into()),
            ),
        ] {
            list.record_fetch(result);
            assert_eq!(list.fetch, Some(status));
        }
    }
}
