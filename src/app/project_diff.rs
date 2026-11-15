use super::*;

#[derive(Default)]
pub struct ProjectDiffs {
    trees: HashMap<WorkingTree, ProjectDiffState>,
    remote: HashMap<ThreadId, ProjectDiffState>,
}

pub struct ProjectDiffState {
    pub result: Option<Result<platform::RepositoryDiff, String>>,
    fingerprint: Option<DiffFingerprint>,
    pub loading: bool,
    root: PathBuf,
    interested: HashSet<ThreadId>,
    generation: Uuid,
    in_flight: Option<Uuid>,
    scheduled: Option<Uuid>,
    dirty: bool,
}

struct DiffCompletion {
    changed: bool,
    refresh_again: bool,
}

type DiffRead = (Result<platform::RepositoryDiff, String>, DiffFingerprint);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DiffFingerprint {
    patch_len: usize,
    hash: u64,
}

impl DiffFingerprint {
    fn of(result: &Result<platform::RepositoryDiff, String>) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let patch_len = match result {
            Ok(diff) => {
                true.hash(&mut hasher);
                diff.head_branch.hash(&mut hasher);
                diff.patch.hash(&mut hasher);
                (diff.changed_files, diff.additions, diff.deletions).hash(&mut hasher);
                diff.truncated.hash(&mut hasher);
                diff.patch.len()
            }
            Err(error) => {
                false.hash(&mut hasher);
                error.hash(&mut hasher);
                error.len()
            }
        };
        Self {
            patch_len,
            hash: hasher.finish(),
        }
    }
}

fn read_with_fingerprint(result: Result<platform::RepositoryDiff, String>) -> DiffRead {
    let fingerprint = DiffFingerprint::of(&result);
    (result, fingerprint)
}

impl ProjectDiffState {
    fn new(root: PathBuf) -> Self {
        Self {
            result: None,
            fingerprint: None,
            loading: false,
            root,
            interested: HashSet::new(),
            generation: Uuid::new_v4(),
            in_flight: None,
            scheduled: None,
            dirty: false,
        }
    }

    fn set_interest(&mut self, root: Option<PathBuf>, interested: HashSet<ThreadId>) -> bool {
        let was_visible = !self.interested.is_empty();
        let visible = !interested.is_empty();
        let root_changed = root.as_ref().is_some_and(|root| root != &self.root);
        if root_changed || was_visible != visible {
            self.generation = Uuid::new_v4();
            self.scheduled = None;
            self.dirty = false;
            self.loading = false;
        }
        if root_changed && let Some(root) = root {
            self.root = root;
            self.result = None;
            self.fingerprint = None;
        }
        self.interested = interested;
        visible && (!was_visible || root_changed)
    }

    fn begin_refresh(&mut self, explicit: bool) -> (Option<(Uuid, PathBuf)>, bool) {
        if self.interested.is_empty() {
            return (None, false);
        }
        let loading = explicit || self.result.is_none();
        let changed = loading && !self.loading;
        self.loading |= loading;
        if self.in_flight.is_some() {
            return (None, changed);
        }
        self.scheduled = None;
        self.dirty = false;
        self.in_flight = Some(self.generation);
        (Some((self.generation, self.root.clone())), changed)
    }

    fn complete(
        &mut self,
        generation: Uuid,
        (result, fingerprint): DiffRead,
    ) -> Option<DiffCompletion> {
        if self.in_flight != Some(generation) {
            return None;
        }
        self.in_flight = None;
        if generation != self.generation {
            return Some(DiffCompletion {
                changed: false,
                refresh_again: !self.interested.is_empty(),
            });
        }
        let result_changed = self.fingerprint != Some(fingerprint);
        let changed = self.loading || result_changed;
        self.loading = false;
        if result_changed {
            self.result = Some(result);
            self.fingerprint = Some(fingerprint);
        }
        Some(DiffCompletion {
            changed,
            refresh_again: std::mem::take(&mut self.dirty),
        })
    }

    fn queue(&mut self) -> Option<Uuid> {
        self.dirty = true;
        self.schedule()
    }

    fn schedule(&mut self) -> Option<Uuid> {
        if self.interested.is_empty() || self.in_flight.is_some() || self.scheduled.is_some() {
            return None;
        }
        let token = Uuid::new_v4();
        self.scheduled = Some(token);
        Some(token)
    }

    fn take_scheduled(&mut self, token: Uuid) -> bool {
        if self.scheduled != Some(token) {
            return false;
        }
        self.scheduled = None;
        !self.interested.is_empty()
    }
}

impl AppState {
    pub(super) fn invalidate_remote_diff(&mut self, id: ThreadId) {
        self.project_diffs.remote.remove(&id);
    }
    pub fn working_tree_diff(&self, key: WorkingTree) -> Option<&ProjectDiffState> {
        self.project_diffs.trees.get(&key)
    }

    pub fn conversation_diff(&self, id: ThreadId) -> Option<&ProjectDiffState> {
        let thread = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)?;
        if thread.remote.is_some() {
            return self.project_diffs.remote.get(&id);
        }
        let (key, _) = self.working_tree_of(thread)?;
        self.working_tree_diff(key)
    }

    fn conversation_tree(&self, id: ThreadId) -> Option<WorkingTree> {
        let thread = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id && thread.kind == ThreadKind::Conversation)?;
        self.working_tree_of(thread).map(|(key, _)| key)
    }

    pub fn set_visible_conversations(
        &mut self,
        ids: impl IntoIterator<Item = ThreadId>,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<ThreadId> = ids.into_iter().collect();
        self.set_visible_reviews(&ids, cx);
        let mut interests: HashMap<WorkingTree, (PathBuf, HashSet<ThreadId>)> = HashMap::new();
        let mut remote_interests = HashSet::new();
        if !self.shutting_down {
            for id in ids {
                if !self.thread_sidebar_open(id)
                    || self.thread_sidebar_tab(id) != super::SidebarTab::Changes
                {
                    continue;
                }
                let Some(thread) = self
                    .snapshot
                    .threads
                    .iter()
                    .find(|thread| thread.id == id && thread.kind == ThreadKind::Conversation)
                else {
                    continue;
                };
                if thread.remote.is_some() {
                    remote_interests.insert(id);
                    continue;
                }
                let Some((key, root)) = self.working_tree_of(thread) else {
                    continue;
                };
                interests
                    .entry(key)
                    .or_insert_with(|| (root, HashSet::new()))
                    .1
                    .insert(id);
            }
        }
        let mut refresh = Vec::new();
        for (key, diff) in &mut self.project_diffs.trees {
            let (root, interested) = match interests.remove(key) {
                Some((root, interested)) => (Some(root), interested),
                None => (None, HashSet::new()),
            };
            if diff.set_interest(root, interested) {
                refresh.push(*key);
            }
        }
        for (key, (root, interested)) in interests {
            let mut diff = ProjectDiffState::new(root);
            diff.set_interest(None, interested);
            self.project_diffs.trees.insert(key, diff);
            refresh.push(key);
        }
        for key in refresh {
            self.refresh_project_diff(key, true, cx);
        }
        let mut remote_refresh = Vec::new();
        for (id, diff) in &mut self.project_diffs.remote {
            let interested = if remote_interests.remove(id) {
                HashSet::from([*id])
            } else {
                HashSet::new()
            };
            if diff.set_interest(None, interested) {
                remote_refresh.push(*id);
            }
        }
        for id in remote_interests {
            let mut diff = ProjectDiffState::new(PathBuf::new());
            diff.set_interest(None, HashSet::from([id]));
            self.project_diffs.remote.insert(id, diff);
            remote_refresh.push(id);
        }
        for id in remote_refresh {
            self.refresh_remote_diff(id, true, cx);
        }
    }

    pub fn refresh_conversation_diff(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        if self
            .snapshot
            .threads
            .iter()
            .any(|thread| thread.id == id && thread.remote.is_some())
        {
            self.refresh_remote_diff(id, true, cx);
            return;
        }
        let Some(key) = self.conversation_tree(id) else {
            return;
        };
        self.refresh_project_diff(key, true, cx);
    }

    pub(super) fn queue_conversation_diff(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        if let Some(diff) = self.project_diffs.remote.get_mut(&id) {
            if let Some(token) = diff.queue() {
                self.schedule_remote_diff(id, token, cx);
            }
            return;
        }
        let Some(key) = self.conversation_tree(id) else {
            return;
        };
        if let Some(diff) = self.project_diffs.trees.get_mut(&key) {
            diff.dirty = true;
        }
        self.schedule_project_diff(key, cx);
    }

    fn refresh_remote_diff(&mut self, id: ThreadId, explicit: bool, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let Some(location) = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .and_then(|thread| thread.remote.clone())
        else {
            return;
        };
        let Some(diff) = self.project_diffs.remote.get_mut(&id) else {
            return;
        };
        let (request, changed) = diff.begin_refresh(explicit);
        if changed {
            cx.emit(PresentationChange::Thread(id));
        }
        let Some((generation, _)) = request else {
            return;
        };
        let client = self.remote_client.clone();
        cx.spawn(async move |this, cx| {
            let read = match client.read_diff(&location.host, id, None).await {
                Ok(value) => {
                    smol::unblock(move || read_with_fingerprint(decode_remote_diff(value))).await
                }
                Err(error) => {
                    read_with_fingerprint(Err(format!("Remote Changes unavailable: {error:#}")))
                }
            };
            let _ = this.update(cx, |state, cx| {
                let Some(diff) = state.project_diffs.remote.get_mut(&id) else {
                    return;
                };
                let Some(completion) = diff.complete(generation, read) else {
                    return;
                };
                let restart_now = completion.refresh_again && diff.loading;
                if completion.changed {
                    cx.emit(PresentationChange::Thread(id));
                }
                if restart_now {
                    state.refresh_remote_diff(id, true, cx);
                } else if completion.refresh_again
                    && let Some(token) = diff.schedule()
                {
                    state.schedule_remote_diff(id, token, cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn refresh_project_diff(
        &mut self,
        id: WorkingTree,
        explicit: bool,
        cx: &mut Context<Self>,
    ) {
        if self.shutting_down {
            return;
        }
        let Some(diff) = self.project_diffs.trees.get_mut(&id) else {
            return;
        };
        let (request, changed) = diff.begin_refresh(explicit);
        if changed {
            cx.emit(PresentationChange::Diff(id));
        }
        let Some((generation, root)) = request else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let read = smol::unblock(move || {
                read_with_fingerprint(
                    platform::working_tree_diff(&root).map_err(|error| format!("{error:#}")),
                )
            })
            .await;
            let _ = this.update(cx, |state, cx| {
                let head = read.0.as_ref().ok().map(|diff| diff.head_branch.clone());
                if let Some(head) = head {
                    state.record_head(id, head);
                }
                let Some(diff) = state.project_diffs.trees.get_mut(&id) else {
                    return;
                };
                let Some(completion) = diff.complete(generation, read) else {
                    return;
                };
                let restart_now = completion.refresh_again && diff.loading;
                if completion.changed {
                    cx.emit(PresentationChange::Diff(id));
                }
                if restart_now {
                    state.refresh_project_diff(id, true, cx);
                } else if completion.refresh_again || state.project_has_active_conversation(id) {
                    state.schedule_project_diff(id, cx);
                }
            });
        })
        .detach();
    }

    fn project_has_active_conversation(&self, id: WorkingTree) -> bool {
        self.snapshot.threads.iter().any(|thread| {
            self.active_threads.contains(&thread.id)
                && self.working_tree_of(thread).map(|(key, _)| key) == Some(id)
        })
    }

    fn schedule_project_diff(&mut self, id: WorkingTree, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let Some(token) = self
            .project_diffs
            .trees
            .get_mut(&id)
            .and_then(|diff| diff.schedule())
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            smol::Timer::after(DIFF_SETTLE).await;
            let _ = this.update(cx, |state, cx| {
                if state
                    .project_diffs
                    .trees
                    .get_mut(&id)
                    .is_some_and(|diff| diff.take_scheduled(token))
                {
                    state.refresh_project_diff(id, false, cx);
                }
            });
        })
        .detach();
    }
}

impl AppState {
    fn schedule_remote_diff(&mut self, id: ThreadId, token: Uuid, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        cx.spawn(async move |this, cx| {
            smol::Timer::after(DIFF_SETTLE).await;
            let _ = this.update(cx, |state, cx| {
                if state
                    .project_diffs
                    .remote
                    .get_mut(&id)
                    .is_some_and(|diff| diff.take_scheduled(token))
                {
                    state.refresh_remote_diff(id, false, cx);
                }
            });
        })
        .detach();
    }
}

const DIFF_SETTLE: std::time::Duration = std::time::Duration::from_millis(750);

fn decode_remote_diff(value: Value) -> Result<platform::RepositoryDiff, String> {
    #[derive(serde::Deserialize)]
    struct RemoteDiff {
        head_branch: Option<String>,
        patch: String,
        changed_files: usize,
        additions: usize,
        deletions: usize,
        truncated: bool,
    }
    let diff: RemoteDiff = serde_json::from_value(value)
        .map_err(|error| format!("Invalid remote Changes: {error}"))?;
    let rows = Arc::new(crate::diff::parse(&diff.patch));
    Ok(platform::RepositoryDiff {
        head_branch: diff.head_branch,
        patch: diff.patch,
        changed_files: diff.changed_files,
        additions: diff.additions,
        deletions: diff.deletions,
        truncated: diff.truncated,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::{DiffFingerprint, ProjectDiffState};
    use crate::platform;
    use std::{collections::HashSet, path::PathBuf};
    use uuid::Uuid;

    #[test]
    fn remote_changes_are_decoded_without_a_desktop_checkout() {
        let patch = "diff --git a/remote.txt b/remote.txt\n--- a/remote.txt\n+++ b/remote.txt\n@@ -1 +1 @@\n-before\n+after\n";
        let diff = super::decode_remote_diff(serde_json::json!({
            "head_branch": "host-only", "patch": patch, "changed_files": 1,
            "additions": 1, "deletions": 1, "truncated": false
        }))
        .unwrap();
        assert_eq!(diff.head_branch.as_deref(), Some("host-only"));
        assert_eq!(diff.patch, patch);
        assert_eq!(diff.changed_files, 1);
        assert!(!diff.rows.rows.is_empty());
        assert!(super::decode_remote_diff(serde_json::json!({"patch": patch})).is_err());
    }

    fn read(result: Result<platform::RepositoryDiff, String>) -> super::DiffRead {
        super::read_with_fingerprint(result)
    }

    #[test]
    fn fingerprints_tell_apart_every_shown_field() {
        let diff = |patch: &str| {
            Ok(platform::RepositoryDiff {
                patch: patch.into(),
                ..Default::default()
            })
        };
        let base = DiffFingerprint::of(&diff("+a\n"));
        assert_eq!(base, DiffFingerprint::of(&diff("+a\n")));
        assert_ne!(base, DiffFingerprint::of(&diff("+b\n")), "same length");
        assert_ne!(base, DiffFingerprint::of(&diff("+a\n+b\n")));
        let truncated = Ok(platform::RepositoryDiff {
            patch: "+a\n".into(),
            truncated: true,
            ..Default::default()
        });
        assert_ne!(base, DiffFingerprint::of(&truncated));
        let branch = Ok(platform::RepositoryDiff {
            patch: "+a\n".into(),
            head_branch: Some("main".into()),
            ..Default::default()
        });
        assert_ne!(base, DiffFingerprint::of(&branch));
        assert_ne!(
            DiffFingerprint::of(&Err("+a\n".into())),
            base,
            "an error is never the diff"
        );
    }

    #[test]
    fn queued_refreshes_settle_into_one_read() {
        let mut diff = visible_diff();
        let token = diff.queue().unwrap();
        assert!(diff.queue().is_none(), "already scheduled");
        assert!(diff.take_scheduled(token));
        let (generation, _) = diff.begin_refresh(false).0.unwrap();
        assert!(diff.queue().is_none());
        let completion = diff
            .complete(generation, read(Ok(Default::default())))
            .unwrap();
        assert!(completion.refresh_again);
        assert!(
            !diff.loading,
            "so the caller schedules rather than restarts"
        );
        assert!(diff.schedule().is_some());
        let mut hidden = ProjectDiffState::new(PathBuf::from("project"));
        assert!(hidden.queue().is_none());
    }

    fn visible_diff() -> ProjectDiffState {
        let mut diff = ProjectDiffState::new(PathBuf::from("project"));
        diff.set_interest(None, HashSet::from([Uuid::from_u128(1)]));
        diff
    }

    #[test]
    fn project_refresh_is_shared_across_visible_threads() {
        let mut diff = visible_diff();
        diff.set_interest(
            None,
            HashSet::from([Uuid::from_u128(1), Uuid::from_u128(2)]),
        );
        let (request, loading_changed) = diff.begin_refresh(true);
        let (generation, _) = request.unwrap();
        assert!(loading_changed);
        assert!(diff.begin_refresh(true).0.is_none());
        assert!(!diff.set_interest(None, HashSet::from([Uuid::from_u128(2)])));
        assert!(diff.begin_refresh(true).0.is_none());
        assert!(
            diff.complete(generation, read(Ok(platform::RepositoryDiff::default())))
                .unwrap()
                .changed
        );
        assert!(!diff.loading);
        assert!(matches!(diff.result, Some(Ok(_))));
    }

    #[test]
    fn hiding_rejects_completion_and_reopening_waits_for_in_flight_read() {
        let mut diff = visible_diff();
        let (generation, _) = diff.begin_refresh(true).0.unwrap();
        diff.set_interest(None, HashSet::new());
        assert!(diff.schedule().is_none());
        assert!(diff.begin_refresh(true).0.is_none());
        assert!(diff.set_interest(None, HashSet::from([Uuid::from_u128(1)])));
        assert!(diff.begin_refresh(true).0.is_none());
        let completion = diff
            .complete(generation, read(Err("obsolete".into())))
            .unwrap();
        assert!(!completion.changed);
        assert!(completion.refresh_again);
        assert!(diff.loading);
        assert!(diff.result.is_none());
        let (reopened, _) = diff.begin_refresh(true).0.unwrap();
        assert_ne!(generation, reopened);
        assert!(
            diff.complete(generation, read(Err("late duplicate".into())))
                .is_none()
        );
        diff.complete(reopened, read(Ok(platform::RepositoryDiff::default())))
            .unwrap();
        assert!(matches!(diff.result, Some(Ok(_))));
        assert!(!diff.loading);
    }

    #[test]
    fn automatic_refresh_only_publishes_changed_results() {
        let mut diff = visible_diff();
        let clean = Ok(platform::RepositoryDiff::default());
        let (generation, _) = diff.begin_refresh(true).0.unwrap();
        assert!(
            diff.complete(generation, read(clean.clone()))
                .unwrap()
                .changed
        );
        let (request, loading_changed) = diff.begin_refresh(false);
        assert!(!loading_changed);
        assert!(
            !diff
                .complete(request.unwrap().0, read(clean))
                .unwrap()
                .changed
        );
        let failure = Err("working tree unavailable".to_owned());
        let (generation, _) = diff.begin_refresh(false).0.unwrap();
        assert!(
            diff.complete(generation, read(failure.clone()))
                .unwrap()
                .changed
        );
        let (generation, _) = diff.begin_refresh(false).0.unwrap();
        assert!(!diff.complete(generation, read(failure)).unwrap().changed);
        let (request, loading_changed) = diff.begin_refresh(true);
        assert!(loading_changed);
        assert!(diff.loading);
        assert!(matches!(&diff.result, Some(Err(error)) if error == "working tree unavailable"));
        assert!(
            diff.complete(
                request.unwrap().0,
                read(Ok(platform::RepositoryDiff::default()))
            )
            .unwrap()
            .changed
        );
        assert!(!diff.loading);
        assert!(matches!(diff.result, Some(Ok(_))));
    }

    #[test]
    fn obsolete_timers_and_roots_cannot_publish() {
        let mut diff = visible_diff();
        let old_timer = diff.schedule().unwrap();
        assert!(diff.schedule().is_none());
        diff.set_interest(None, HashSet::new());
        diff.set_interest(None, HashSet::from([Uuid::from_u128(1)]));
        let current_timer = diff.schedule().unwrap();
        assert!(!diff.take_scheduled(old_timer));
        assert!(diff.take_scheduled(current_timer));
        let (generation, _) = diff.begin_refresh(false).0.unwrap();
        assert!(diff.set_interest(
            Some(PathBuf::from("new-root")),
            HashSet::from([Uuid::from_u128(1)])
        ));
        assert!(diff.begin_refresh(true).0.is_none());
        assert!(
            diff.complete(generation, read(Err("old root".into())))
                .unwrap()
                .refresh_again
        );
        assert!(diff.result.is_none());
        let (_, root) = diff.begin_refresh(true).0.unwrap();
        assert_eq!(root, PathBuf::from("new-root"));
    }

    #[test]
    fn changes_during_a_read_are_refreshed_after_completion() {
        let mut diff = visible_diff();
        let (generation, _) = diff.begin_refresh(true).0.unwrap();
        diff.dirty = true;
        assert!(diff.schedule().is_none());
        assert!(
            diff.complete(generation, read(Ok(platform::RepositoryDiff::default())))
                .unwrap()
                .refresh_again
        );
        let token = diff.schedule().unwrap();
        assert!(diff.schedule().is_none());
        assert!(diff.take_scheduled(token));
        let (generation, _) = diff.begin_refresh(false).0.unwrap();
        assert!(
            !diff
                .complete(generation, read(Ok(platform::RepositoryDiff::default())))
                .unwrap()
                .refresh_again
        );
    }
}
