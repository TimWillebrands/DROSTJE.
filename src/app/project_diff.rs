use super::*;

#[derive(Default)]
pub struct ProjectDiffs {
    projects: HashMap<ProjectId, ProjectDiffState>,
}

/// A project has one cached result and at most one working-tree read, even when
/// several conversations show it. Hiding invalidates the result of an existing
/// read without releasing its slot: reopening cannot overlap that read.
pub struct ProjectDiffState {
    pub result: Option<Result<platform::RepositoryDiff, String>>,
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

impl ProjectDiffState {
    fn new(root: PathBuf) -> Self {
        Self {
            result: None,
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
        result: Result<platform::RepositoryDiff, String>,
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
        let result_changed = self.result.as_ref() != Some(&result);
        let changed = self.loading || result_changed;
        self.loading = false;
        if result_changed {
            self.result = Some(result);
        }
        Some(DiffCompletion {
            changed,
            refresh_again: std::mem::take(&mut self.dirty),
        })
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
    pub fn project_diff(&self, id: ProjectId) -> Option<&ProjectDiffState> {
        self.project_diffs.projects.get(&id)
    }

    pub fn conversation_diff(&self, id: ThreadId) -> Option<&ProjectDiffState> {
        let thread = self
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)?;
        self.project_diff(thread.project_id)
    }

    /// Supply the actual visible conversation panes, not every retained tab.
    /// Reconcile again when a pane's Changes sidebar is opened or closed.
    pub fn set_visible_conversations(
        &mut self,
        ids: impl IntoIterator<Item = ThreadId>,
        cx: &mut Context<Self>,
    ) {
        let mut interests: HashMap<ProjectId, (PathBuf, HashSet<ThreadId>)> = HashMap::new();
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
                let Some(project) = self
                    .snapshot
                    .projects
                    .iter()
                    .find(|project| project.id == thread.project_id)
                else {
                    continue;
                };
                interests
                    .entry(project.id)
                    .or_insert_with(|| (project.root.clone(), HashSet::new()))
                    .1
                    .insert(id);
            }
        }
        let mut refresh = Vec::new();
        for (id, diff) in &mut self.project_diffs.projects {
            let (root, interested) = match interests.remove(id) {
                Some((root, interested)) => (Some(root), interested),
                None => (None, HashSet::new()),
            };
            if diff.set_interest(root, interested) {
                refresh.push(*id);
            }
        }
        for (id, (root, interested)) in interests {
            let mut diff = ProjectDiffState::new(root);
            diff.set_interest(None, interested);
            self.project_diffs.projects.insert(id, diff);
            refresh.push(id);
        }
        for id in refresh {
            self.refresh_project_diff(id, true, cx);
        }
    }

    pub fn refresh_conversation_diff(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        let Some(project_id) = self.snapshot.threads.iter().find_map(|thread| {
            (thread.id == id && thread.kind == ThreadKind::Conversation)
                .then_some(thread.project_id)
        }) else {
            return;
        };
        self.refresh_project_diff(project_id, true, cx);
    }

    pub(super) fn queue_conversation_diff(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        let Some(project_id) = self
            .snapshot
            .threads
            .iter()
            .find_map(|thread| (thread.id == id).then_some(thread.project_id))
        else {
            return;
        };
        if let Some(diff) = self.project_diffs.projects.get_mut(&project_id) {
            diff.dirty = true;
        }
        self.schedule_project_diff(project_id, cx);
    }

    fn refresh_project_diff(&mut self, id: ProjectId, explicit: bool, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let Some(diff) = self.project_diffs.projects.get_mut(&id) else {
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
            let result = smol::unblock(move || {
                platform::working_tree_diff(&root).map_err(|error| format!("{error:#}"))
            })
            .await;
            let _ = this.update(cx, |state, cx| {
                let Some(diff) = state.project_diffs.projects.get_mut(&id) else {
                    return;
                };
                let Some(completion) = diff.complete(generation, result) else {
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

    fn project_has_active_conversation(&self, id: ProjectId) -> bool {
        self.snapshot
            .threads
            .iter()
            .any(|thread| thread.project_id == id && self.active_threads.contains(&thread.id))
    }

    fn schedule_project_diff(&mut self, id: ProjectId, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let Some(token) = self
            .project_diffs
            .projects
            .get_mut(&id)
            .and_then(|diff| diff.schedule())
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(750)).await;
            let _ = this.update(cx, |state, cx| {
                if state
                    .project_diffs
                    .projects
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

#[cfg(test)]
mod tests {
    use super::ProjectDiffState;
    use crate::platform;
    use std::{collections::HashSet, path::PathBuf};
    use uuid::Uuid;

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
        // Closing one pane neither cancels nor duplicates the shared read.
        assert!(!diff.set_interest(None, HashSet::from([Uuid::from_u128(2)])));
        assert!(diff.begin_refresh(true).0.is_none());
        assert!(
            diff.complete(generation, Ok(platform::RepositoryDiff::default()))
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
        let completion = diff.complete(generation, Err("obsolete".into())).unwrap();
        assert!(!completion.changed);
        assert!(completion.refresh_again);
        assert!(diff.loading);
        assert!(diff.result.is_none());
        let (reopened, _) = diff.begin_refresh(true).0.unwrap();
        assert_ne!(generation, reopened);
        assert!(
            diff.complete(generation, Err("late duplicate".into()))
                .is_none()
        );
        diff.complete(reopened, Ok(platform::RepositoryDiff::default()))
            .unwrap();
        assert!(matches!(diff.result, Some(Ok(_))));
        assert!(!diff.loading);
    }

    #[test]
    fn automatic_refresh_only_publishes_changed_results() {
        let mut diff = visible_diff();
        let clean = Ok(platform::RepositoryDiff::default());
        let (generation, _) = diff.begin_refresh(true).0.unwrap();
        assert!(diff.complete(generation, clean.clone()).unwrap().changed);
        let (request, loading_changed) = diff.begin_refresh(false);
        assert!(!loading_changed);
        assert!(!diff.complete(request.unwrap().0, clean).unwrap().changed);
        let failure = Err("working tree unavailable".to_owned());
        let (generation, _) = diff.begin_refresh(false).0.unwrap();
        assert!(diff.complete(generation, failure.clone()).unwrap().changed);
        let (generation, _) = diff.begin_refresh(false).0.unwrap();
        assert!(!diff.complete(generation, failure).unwrap().changed);
        let (request, loading_changed) = diff.begin_refresh(true);
        assert!(loading_changed);
        assert!(diff.loading);
        assert!(matches!(&diff.result, Some(Err(error)) if error == "working tree unavailable"));
        assert!(
            diff.complete(request.unwrap().0, Ok(platform::RepositoryDiff::default()))
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
            diff.complete(generation, Err("old root".into()))
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
            diff.complete(generation, Ok(platform::RepositoryDiff::default()))
                .unwrap()
                .refresh_again
        );
        let token = diff.schedule().unwrap();
        assert!(diff.schedule().is_none());
        assert!(diff.take_scheduled(token));
        let (generation, _) = diff.begin_refresh(false).0.unwrap();
        assert!(
            !diff
                .complete(generation, Ok(platform::RepositoryDiff::default()))
                .unwrap()
                .refresh_again
        );
    }
}
