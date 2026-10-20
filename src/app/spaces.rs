use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const SETTING: &str = "spaces";
const VERSION: u32 = 3;
const DEFAULT_SPACE: SpaceId = Uuid::from_u128(0);
const MAX_NAME_LENGTH: usize = 80;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpaceSession {
    pub selected_project: Option<ProjectId>,
    pub selected_thread: Option<ThreadId>,
    pub open_threads: Vec<ThreadId>,
    pub show_archived: bool,
    pub collapsed: HashSet<ProjectId>,
    pub layout: Option<Value>,
}

/// One settings write owns membership and every workspace projection. Provider
/// sessions and drafts deliberately remain outside this navigation-only state.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Spaces {
    version: u32,
    pub active: SpaceId,
    pub spaces: Vec<Space>,
    projects: BTreeMap<ProjectId, BTreeSet<SpaceId>>,
    threads: BTreeMap<ThreadId, SpaceId>,
    sessions: BTreeMap<SpaceId, SpaceSession>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpacesV1 {
    version: u32,
    active: SpaceId,
    spaces: Vec<Space>,
    projects: BTreeMap<ProjectId, SpaceId>,
    sessions: BTreeMap<SpaceId, SpaceSession>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpacesV2 {
    version: u32,
    active: SpaceId,
    spaces: Vec<Space>,
    projects: BTreeMap<ProjectId, BTreeSet<SpaceId>>,
    sessions: BTreeMap<SpaceId, SpaceSession>,
}

impl SpacesV1 {
    fn upgrade(self) -> SpacesV2 {
        SpacesV2 {
            version: 2,
            active: self.active,
            spaces: self.spaces,
            projects: self
                .projects
                .into_iter()
                .map(|(project, space)| (project, BTreeSet::from([space])))
                .collect(),
            sessions: self.sessions,
        }
    }
}

impl SpacesV2 {
    fn upgrade(self, snapshot: &WorkspaceSnapshot) -> Spaces {
        let mut spaces = Spaces {
            version: VERSION,
            active: self.active,
            spaces: self.spaces,
            projects: self.projects,
            threads: BTreeMap::new(),
            sessions: self.sessions,
        };
        spaces.recover_missing_memberships(snapshot);
        spaces.remove_cross_space_thread_state();
        spaces
    }
}

impl Spaces {
    fn register_project(&mut self, project: ProjectId, space: SpaceId) -> (SpaceId, bool) {
        let destination = if self.sessions.contains_key(&space) {
            space
        } else {
            DEFAULT_SPACE
        };
        let inserted = self
            .projects
            .entry(project)
            .or_default()
            .insert(destination);
        (destination, inserted)
    }

    fn register_thread(
        &mut self,
        thread: ThreadId,
        project: ProjectId,
        space: SpaceId,
    ) -> Result<SpaceId> {
        let destination = if self.sessions.contains_key(&space) {
            space
        } else {
            DEFAULT_SPACE
        };
        self.projects
            .entry(project)
            .or_default()
            .insert(destination);
        if let Some(existing) = self.threads.get(&thread) {
            anyhow::ensure!(
                *existing == destination,
                "Thread already belongs to another space"
            );
        } else {
            self.threads.insert(thread, destination);
        }
        Ok(destination)
    }

    fn inferred_thread_space(&self, thread: &Thread) -> SpaceId {
        let project_spaces = self.projects.get(&thread.project_id);
        let member =
            |space: SpaceId| project_spaces.is_some_and(|memberships| memberships.contains(&space));
        let mentioned = |space: SpaceId| {
            self.sessions.get(&space).is_some_and(|session| {
                session.selected_thread == Some(thread.id)
                    || session.open_threads.contains(&thread.id)
            })
        };
        std::iter::once(self.active)
            .chain(
                self.spaces
                    .iter()
                    .map(|space| space.id)
                    .filter(|space| *space != self.active),
            )
            .find(|space| member(*space) && mentioned(*space))
            .or_else(|| {
                std::iter::once(self.active)
                    .chain(
                        self.spaces
                            .iter()
                            .map(|space| space.id)
                            .filter(|space| *space != self.active),
                    )
                    .find(|space| member(*space))
            })
            .unwrap_or(DEFAULT_SPACE)
    }

    fn recover_missing_memberships(&mut self, snapshot: &WorkspaceSnapshot) {
        for project in &snapshot.projects {
            self.projects
                .entry(project.id)
                .or_insert_with(|| BTreeSet::from([DEFAULT_SPACE]));
        }
        for thread in &snapshot.threads {
            if !self.threads.contains_key(&thread.id) {
                let destination = self.inferred_thread_space(thread);
                self.projects
                    .entry(thread.project_id)
                    .or_default()
                    .insert(destination);
                self.threads.insert(thread.id, destination);
            }
        }
    }

    fn remove_cross_space_thread_state(&mut self) {
        for (space, session) in &mut self.sessions {
            let before = session.open_threads.len();
            session
                .open_threads
                .retain(|thread| self.threads.get(thread) == Some(space));
            let cleared_selection = session
                .selected_thread
                .is_some_and(|thread| self.threads.get(&thread) != Some(space));
            if cleared_selection {
                session.selected_thread = None;
            }
            if before != session.open_threads.len() || cleared_selection {
                session.layout = None;
            }
        }
    }

    pub fn load(snapshot: &WorkspaceSnapshot) -> Result<Self> {
        let Some(value) = snapshot.settings.get(SETTING) else {
            return Ok(Self::migrate(snapshot));
        };
        let version = value.get("version").and_then(Value::as_u64).context(
            "Cannot read saved spaces version; the original metadata has been preserved",
        )?;
        let mut spaces = match version {
            1 => {
                let legacy: SpacesV1 = serde_json::from_value(value.clone()).context(
                    "Cannot read saved spaces; the original metadata has been preserved",
                )?;
                anyhow::ensure!(
                    legacy.version == 1,
                    "Unsupported saved spaces version {}; original metadata preserved",
                    legacy.version
                );
                legacy.upgrade().upgrade(snapshot)
            }
            2 => {
                let legacy: SpacesV2 = serde_json::from_value(value.clone()).context(
                    "Cannot read saved spaces; the original metadata has been preserved",
                )?;
                anyhow::ensure!(
                    legacy.version == 2,
                    "Unsupported saved spaces version {}; original metadata preserved",
                    legacy.version
                );
                legacy.upgrade(snapshot)
            }
            version if version == u64::from(VERSION) => serde_json::from_value(value.clone())
                .context("Cannot read saved spaces; the original metadata has been preserved")?,
            version => anyhow::bail!(
                "Unsupported saved spaces version {version}; original metadata preserved"
            ),
        };
        spaces.recover_missing_memberships(snapshot);
        spaces.validate(snapshot)?;
        Ok(spaces)
    }

    pub fn recovery(snapshot: &WorkspaceSnapshot) -> Self {
        let mut spaces = Self::migrate(snapshot);
        // Once metadata exists, legacy selection/layout must never replace it,
        // even in the explicitly read-only recovery workspace.
        spaces
            .sessions
            .insert(DEFAULT_SPACE, SpaceSession::default());
        spaces
    }

    pub fn migrate(snapshot: &WorkspaceSnapshot) -> Self {
        let setting_id = |key| {
            snapshot
                .settings
                .get(key)
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
        };
        let selected_thread = setting_id("selected_thread")
            .filter(|id| snapshot.threads.iter().any(|thread| thread.id == *id))
            .or_else(|| {
                let id = setting_id("selected_launch")?;
                let launch = snapshot.launches.iter().find(|launch| launch.id == id)?;
                snapshot
                    .threads
                    .iter()
                    .find(|thread| {
                        thread.id == launch.factory_thread_id
                            && thread.project_id == launch.project_id
                            && thread.kind == ThreadKind::Factory
                    })
                    .map(|thread| thread.id)
            });
        let selected_project = selected_thread
            .and_then(|id| {
                snapshot
                    .threads
                    .iter()
                    .find(|thread| thread.id == id)
                    .map(|thread| thread.project_id)
            })
            .or_else(|| {
                setting_id("selected_project")
                    .filter(|id| snapshot.projects.iter().any(|project| project.id == *id))
            })
            .or_else(|| {
                snapshot
                    .projects
                    .iter()
                    .find(|project| !project.archived)
                    .map(|project| project.id)
            });
        let mut open_threads: Vec<ThreadId> = snapshot
            .settings
            .get("open_threads")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        let mut seen = HashSet::new();
        open_threads.retain(|id| {
            seen.insert(*id) && snapshot.threads.iter().any(|thread| thread.id == *id)
        });
        if let Some(id) = selected_thread
            && !open_threads.contains(&id)
        {
            open_threads.push(id);
        }
        let collapsed = snapshot
            .settings
            .get("collapsed")
            .and_then(|value| serde_json::from_value::<HashSet<ProjectId>>(value.clone()).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|id| snapshot.projects.iter().any(|project| project.id == *id))
            .collect();
        Self {
            version: VERSION,
            active: DEFAULT_SPACE,
            spaces: vec![Space {
                id: DEFAULT_SPACE,
                name: "Default".into(),
            }],
            projects: snapshot
                .projects
                .iter()
                .map(|project| (project.id, BTreeSet::from([DEFAULT_SPACE])))
                .collect(),
            threads: snapshot
                .threads
                .iter()
                .map(|thread| (thread.id, DEFAULT_SPACE))
                .collect(),
            sessions: BTreeMap::from([(
                DEFAULT_SPACE,
                SpaceSession {
                    selected_project,
                    selected_thread,
                    open_threads,
                    show_archived: snapshot
                        .settings
                        .get("show_archived")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    collapsed,
                    layout: snapshot.settings.get("layout").cloned(),
                },
            )]),
        }
    }

    fn validate(&self, snapshot: &WorkspaceSnapshot) -> Result<()> {
        anyhow::ensure!(
            self.version == VERSION,
            "Unsupported saved spaces version {}; original metadata preserved",
            self.version
        );
        let ids: HashSet<_> = self.spaces.iter().map(|space| space.id).collect();
        anyhow::ensure!(
            ids.len() == self.spaces.len()
                && ids.contains(&DEFAULT_SPACE)
                && ids.contains(&self.active),
            "Invalid saved space identities; original metadata preserved"
        );
        anyhow::ensure!(
            ids.len() == self.sessions.len() && self.sessions.keys().all(|id| ids.contains(id)),
            "Invalid saved space sessions; original metadata preserved"
        );
        for space in &self.spaces {
            anyhow::ensure!(
                self.valid_name(&space.name, Some(space.id))? == space.name,
                "Saved space names must be trimmed"
            );
        }
        anyhow::ensure!(
            self.projects.iter().all(|(project, spaces)| {
                !spaces.is_empty()
                    && spaces.iter().all(|space| ids.contains(space))
                    && snapshot.projects.iter().any(|item| item.id == *project)
            }),
            "Invalid saved project membership; original metadata preserved"
        );
        anyhow::ensure!(
            self.threads.len() == snapshot.threads.len()
                && self.threads.iter().all(|(thread, space)| {
                    ids.contains(space)
                        && snapshot.threads.iter().any(|item| {
                            item.id == *thread
                                && self
                                    .projects
                                    .get(&item.project_id)
                                    .is_some_and(|spaces| spaces.contains(space))
                        })
                }),
            "Invalid saved thread membership; original metadata preserved"
        );
        for (id, session) in &self.sessions {
            let project_member = |project: &ProjectId| {
                self.projects
                    .get(project)
                    .is_some_and(|spaces| spaces.contains(id))
            };
            let thread_member = |thread: &ThreadId| self.threads.get(thread) == Some(id);
            let mut seen = HashSet::new();
            anyhow::ensure!(
                session.selected_project.as_ref().is_none_or(project_member)
                    && session.selected_thread.as_ref().is_none_or(thread_member)
                    && session
                        .open_threads
                        .iter()
                        .all(|thread| thread_member(thread) && seen.insert(*thread))
                    && session.collapsed.iter().all(project_member),
                "Invalid saved space selection; original metadata preserved"
            );
            if let Some(thread) = session.selected_thread {
                anyhow::ensure!(
                    session.open_threads.contains(&thread)
                        && snapshot.threads.iter().any(|item| item.id == thread
                            && Some(item.project_id) == session.selected_project),
                    "Inconsistent saved space selection; original metadata preserved"
                );
            }
        }
        Ok(())
    }

    fn valid_name(&self, name: &str, except: Option<SpaceId>) -> Result<String> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "Enter a space name");
        anyhow::ensure!(
            name.chars().count() <= MAX_NAME_LENGTH,
            "Space names must be at most {MAX_NAME_LENGTH} characters"
        );
        anyhow::ensure!(
            !name.chars().any(char::is_control),
            "Space names cannot contain control characters"
        );
        let normalized = name.to_lowercase();
        anyhow::ensure!(
            !self
                .spaces
                .iter()
                .any(|space| Some(space.id) != except && space.name.to_lowercase() == normalized),
            "A space with that name already exists"
        );
        Ok(name.to_owned())
    }

    fn create(&mut self, name: &str) -> Result<SpaceId> {
        let name = self.valid_name(name, None)?;
        let id = Uuid::new_v4();
        self.spaces.push(Space { id, name });
        self.sessions.insert(id, SpaceSession::default());
        Ok(id)
    }

    fn rename(&mut self, id: SpaceId, name: &str) -> Result<()> {
        let name = self.valid_name(name, Some(id))?;
        let space = self
            .spaces
            .iter_mut()
            .find(|space| space.id == id)
            .context("Space no longer exists")?;
        space.name = name;
        Ok(())
    }

    fn switch(&mut self, id: SpaceId) -> Result<()> {
        anyhow::ensure!(self.sessions.contains_key(&id), "Space no longer exists");
        self.active = id;
        Ok(())
    }

    fn move_project(
        &mut self,
        project: ProjectId,
        source: SpaceId,
        target: SpaceId,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<()> {
        anyhow::ensure!(
            self.sessions.contains_key(&target),
            "Destination space no longer exists"
        );
        let memberships = self
            .projects
            .get_mut(&project)
            .context("Project no longer exists")?;
        anyhow::ensure!(
            memberships.contains(&source),
            "Project is not in the source space"
        );
        if source == target {
            return Ok(());
        }
        memberships.insert(target);
        memberships.remove(&source);
        let threads: HashSet<_> = snapshot
            .threads
            .iter()
            .filter(|thread| {
                thread.project_id == project && self.threads.get(&thread.id) == Some(&source)
            })
            .map(|thread| thread.id)
            .collect();
        for thread in &threads {
            self.threads.insert(*thread, target);
        }
        if let Some(session) = self.sessions.get_mut(&source) {
            session
                .open_threads
                .retain(|thread| !threads.contains(thread));
            session.collapsed.remove(&project);
            if session
                .selected_thread
                .is_some_and(|thread| threads.contains(&thread))
            {
                session.selected_thread = None;
            }
            if session.selected_project == Some(project) {
                session.selected_project = None;
            }
            // Dock JSON belongs to the UI; rebuild the source layout after its
            // project panels leave. An existing destination layout stays valid.
            session.layout = None;
        }
        Ok(())
    }

    fn move_thread(
        &mut self,
        thread: ThreadId,
        project: ProjectId,
        source: SpaceId,
        target: SpaceId,
    ) -> Result<()> {
        anyhow::ensure!(
            self.sessions.contains_key(&target),
            "Destination space no longer exists"
        );
        anyhow::ensure!(
            self.threads.get(&thread) == Some(&source),
            "Thread is not in the source space"
        );
        if source == target {
            return Ok(());
        }
        self.projects.entry(project).or_default().insert(target);
        self.threads.insert(thread, target);
        if let Some(session) = self.sessions.get_mut(&source) {
            session.open_threads.retain(|open| *open != thread);
            if session.selected_thread == Some(thread) {
                session.selected_thread = None;
            }
            session.layout = None;
        }
        Ok(())
    }

    fn delete(&mut self, id: SpaceId, snapshot: &WorkspaceSnapshot) -> Result<()> {
        anyhow::ensure!(id != DEFAULT_SPACE, "Default cannot be deleted");
        anyhow::ensure!(
            id != self.active,
            "Switch away from this space before deleting it"
        );
        anyhow::ensure!(self.sessions.contains_key(&id), "Space no longer exists");
        let moved_projects: HashSet<_> = snapshot
            .threads
            .iter()
            .filter(|thread| self.threads.get(&thread.id) == Some(&id))
            .map(|thread| thread.project_id)
            .collect();
        for space in self.threads.values_mut() {
            if *space == id {
                *space = DEFAULT_SPACE;
            }
        }
        for (project, memberships) in &mut self.projects {
            memberships.remove(&id);
            if memberships.is_empty() || moved_projects.contains(project) {
                memberships.insert(DEFAULT_SPACE);
            }
        }
        self.sessions.remove(&id);
        self.spaces.retain(|space| space.id != id);
        Ok(())
    }

    pub fn active_session(&self) -> SpaceSession {
        self.sessions.get(&self.active).cloned().unwrap_or_default()
    }
}

impl AppState {
    pub(super) fn register_project_membership(
        &mut self,
        id: ProjectId,
        space: SpaceId,
    ) -> (SpaceId, bool) {
        let registration = self.space_state.register_project(id, space);
        if registration.1 {
            self.persist_space_session();
        }
        registration
    }

    pub(super) fn space_name(&self, id: SpaceId) -> Option<&str> {
        self.spaces
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.name.as_str())
    }

    pub fn default_space_id(&self) -> SpaceId {
        DEFAULT_SPACE
    }

    pub fn active_space_name(&self) -> &str {
        self.spaces
            .iter()
            .find(|space| space.id == self.active_space)
            .map_or("Default", |space| space.name.as_str())
    }

    pub fn project_in_active_space(&self, id: ProjectId) -> bool {
        self.project_in_space(id, self.active_space)
    }

    pub fn thread_in_active_space(&self, id: ThreadId) -> bool {
        self.thread_in_space(id, self.active_space)
    }

    pub(crate) fn project_in_space(&self, id: ProjectId, space: SpaceId) -> bool {
        self.space_state
            .projects
            .get(&id)
            .is_some_and(|spaces| spaces.contains(&space))
    }

    pub(crate) fn thread_in_space(&self, id: ThreadId, space: SpaceId) -> bool {
        self.space_state.threads.get(&id) == Some(&space)
    }

    /// Borrow an inactive Space's presentation without activating its editors or layout.
    pub(crate) fn space_session(&self, space: SpaceId) -> Option<&SpaceSession> {
        self.space_state.sessions.get(&space)
    }

    pub(super) fn register_thread_membership(
        &mut self,
        thread: ThreadId,
        project: ProjectId,
        space: SpaceId,
    ) -> Result<SpaceId> {
        self.change_spaces(|spaces, _| spaces.register_thread(thread, project, space))
    }

    pub fn space_mutation_reason(&self) -> Option<String> {
        if self.shutting_down {
            Some("Application is shutting down".into())
        } else {
            self.storage_error
                .as_ref()
                .map(|error| format!("Storage is read-only: {error}"))
        }
    }

    fn capture_space_session(&mut self) {
        if let Some(session) = self.space_state.sessions.get_mut(&self.active_space) {
            session.selected_project = self.selected_project;
            session.selected_thread = self.selected_thread;
            session.open_threads.clone_from(&self.open_threads);
            session.show_archived = self.show_archived;
            session.collapsed.clone_from(&self.collapsed);
        }
    }

    fn apply_space_projection(&mut self) {
        self.active_space = self.space_state.active;
        self.spaces.clone_from(&self.space_state.spaces);
        let session = self.space_state.active_session();
        self.selected_project = session.selected_project;
        self.selected_thread = session.selected_thread;
        if let Some(id) = self.selected_thread {
            self.unseen_turns.seen(id);
        }
        self.open_threads = session.open_threads;
        self.show_archived = session.show_archived;
        self.collapsed = session.collapsed;
        let launch = self.selected_thread.and_then(|id| {
            self.snapshot
                .launches
                .iter()
                .find(|launch| launch.factory_thread_id == id)
                .map(|launch| launch.id)
        });
        if self.selected_launch != launch {
            self.select_launch_output(launch);
        }
    }

    fn write_spaces(&mut self, value: Value) -> Result<()> {
        if let Some(reason) = self.space_mutation_reason() {
            anyhow::bail!(reason);
        }
        if self.snapshot.settings.get(SETTING) == Some(&value) {
            return Ok(());
        }
        anyhow::ensure!(
            self.enqueue(Write::Setting(SETTING.into(), value.clone())),
            "Could not queue space changes; storage is unavailable"
        );
        self.snapshot.settings.insert(SETTING.into(), value);
        Ok(())
    }

    fn change_spaces<T>(
        &mut self,
        change: impl FnOnce(&mut Spaces, &WorkspaceSnapshot) -> Result<T>,
    ) -> Result<T> {
        if let Some(reason) = self.space_mutation_reason() {
            anyhow::bail!(reason);
        }
        self.commit_all_thread_edits();
        self.capture_space_session();
        let mut spaces = self.space_state.clone();
        let result = change(&mut spaces, &self.snapshot)?;
        self.write_spaces(serde_json::to_value(&spaces)?)?;
        self.space_state = spaces;
        self.apply_space_projection();
        Ok(result)
    }

    pub fn create_space(&mut self, name: &str) -> Result<SpaceId> {
        self.change_spaces(|spaces, _| spaces.create(name))
    }

    pub fn rename_space(&mut self, id: SpaceId, name: &str) -> Result<()> {
        self.change_spaces(|spaces, _| spaces.rename(id, name))
    }

    pub fn switch_space(&mut self, id: SpaceId) -> Result<()> {
        self.change_spaces(|spaces, _| spaces.switch(id))
    }

    pub fn move_project_to_space(&mut self, project: ProjectId, target: SpaceId) -> Result<()> {
        anyhow::ensure!(
            self.project_in_active_space(project),
            "Select a project in the active space"
        );
        let source = self.active_space;
        self.change_spaces(|spaces, snapshot| {
            spaces.move_project(project, source, target, snapshot)
        })
    }

    pub fn move_thread_to_space(&mut self, thread: ThreadId, target: SpaceId) -> Result<()> {
        anyhow::ensure!(
            self.thread_in_active_space(thread),
            "Select a thread in the active space"
        );
        let project = self
            .snapshot
            .threads
            .iter()
            .find(|item| item.id == thread)
            .map(|item| item.project_id)
            .context("Thread no longer exists")?;
        let source = self.active_space;
        self.change_spaces(|spaces, _| spaces.move_thread(thread, project, source, target))
    }

    pub fn delete_space(&mut self, id: SpaceId) -> Result<()> {
        self.change_spaces(|spaces, snapshot| spaces.delete(id, snapshot))
    }

    pub fn space_layout(&self) -> Option<&Value> {
        self.space_state
            .sessions
            .get(&self.active_space)
            .and_then(|session| session.layout.as_ref())
    }

    pub fn set_space_layout(&mut self, layout: Value) {
        if self.space_mutation_reason().is_some() || self.space_layout() == Some(&layout) {
            return;
        }
        if let Some(session) = self.space_state.sessions.get_mut(&self.active_space) {
            session.layout = Some(layout);
        }
        self.persist_space_session();
    }

    pub fn set_show_archived(&mut self, visible: bool) {
        if self.space_mutation_reason().is_some() {
            return;
        }
        self.show_archived = visible;
        self.persist_space_session();
    }

    pub fn set_project_collapsed(&mut self, id: ProjectId, collapsed: bool) {
        if self.space_mutation_reason().is_some() || !self.project_in_active_space(id) {
            return;
        }
        if collapsed {
            self.collapsed.insert(id);
        } else {
            self.collapsed.remove(&id);
        }
        self.persist_space_session();
    }

    pub fn persist_space_session(&mut self) {
        if self.space_mutation_reason().is_some() {
            return;
        }
        self.capture_space_session();
        let result = serde_json::to_value(&self.space_state)
            .context("Could not encode spaces")
            .and_then(|value| self.write_spaces(value));
        if let Err(error) = result {
            self.notice(
                NoticeLevel::Error,
                format!("Could not save spaces: {error:#}"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_SPACE, SETTING, Spaces, VERSION};
    use crate::model::{
        FactoryLaunch, Project, ProjectDefaults, Thread, ThreadKind, WorkspaceSnapshot,
    };
    use serde_json::json;
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
    };
    use uuid::Uuid;

    fn fixture() -> WorkspaceSnapshot {
        let project = Project {
            id: Uuid::from_u128(1),
            name: "Existing project".into(),
            root: PathBuf::from("/fixture/project"),
            archived: false,
            defaults: ProjectDefaults::default(),
        };
        let thread = Thread {
            id: Uuid::from_u128(2),
            project_id: project.id,
            kind: ThreadKind::Conversation,
            factory: None,
            title: "Existing conversation".into(),
            harness: "omp".into(),
            session_ref: Some("existing-provider-session".into()),
            model: None,
            effort: None,
            permission_mode: None,
            worktree_id: None,
            pull_request: None,
            draft: "Keep this unsent draft".into(),
            attachments: Vec::new(),
            submission: Some(json!({"recovery": "never replay"})),
            queued: Vec::new(),
            archived: false,
            created_at: "2026-09-17T00:00:00Z".into(),
            updated_at: "2026-09-17T00:00:00Z".into(),
        };
        WorkspaceSnapshot {
            worktrees: Vec::new(),
            settings: BTreeMap::from([
                ("selected_project".into(), json!(project.id)),
                ("selected_thread".into(), json!(thread.id)),
                (
                    "open_threads".into(),
                    json!([thread.id, thread.id, Uuid::from_u128(999)]),
                ),
                ("show_archived".into(), json!(true)),
                ("collapsed".into(), json!([project.id])),
                ("layout".into(), json!({"legacy": "dock layout"})),
            ]),
            projects: vec![project],
            threads: vec![thread],
            launches: Vec::new(),
        }
    }

    fn reload(spaces: &Spaces, snapshot: &WorkspaceSnapshot) -> Spaces {
        let mut saved = snapshot.clone();
        saved
            .settings
            .insert(SETTING.into(), serde_json::to_value(spaces).unwrap());
        Spaces::load(&saved).unwrap()
    }

    #[test]
    fn migration_preserves_legacy_workspace_and_switch_restores_it() {
        let snapshot = fixture();
        let mut spaces = Spaces::load(&snapshot).unwrap();
        let before = serde_json::to_value(spaces.active_session()).unwrap();
        let session = spaces.active_session();
        assert_eq!(session.selected_thread, Some(snapshot.threads[0].id));
        assert_eq!(session.selected_project, Some(snapshot.projects[0].id));
        assert_eq!(session.open_threads, vec![snapshot.threads[0].id]);
        assert!(session.show_archived);
        assert!(session.collapsed.contains(&snapshot.projects[0].id));
        assert_eq!(session.layout, snapshot.settings.get("layout").cloned());

        let other = spaces.create("  Work  ").unwrap();
        assert_eq!(spaces.active, DEFAULT_SPACE, "Creation never switches");
        spaces.switch(other).unwrap();
        let spaces = reload(&spaces, &snapshot);
        let session = spaces.active_session();
        assert!(session.selected_project.is_none());
        assert!(session.selected_thread.is_none());
        assert!(session.open_threads.is_empty());
        assert!(session.layout.is_none());
        assert!(
            !session.show_archived,
            "Legacy globals cannot leak into a new space"
        );
        let mut spaces = spaces;
        spaces.switch(DEFAULT_SPACE).unwrap();
        assert_eq!(
            serde_json::to_value(spaces.active_session()).unwrap(),
            before
        );
        assert_eq!(reload(&spaces, &snapshot).active, DEFAULT_SPACE);
    }

    #[test]
    fn version_one_membership_is_upgraded_without_losing_session_state() {
        let mut snapshot = fixture();
        let spaces = Spaces::load(&snapshot).unwrap();
        let mut legacy = serde_json::to_value(&spaces).unwrap();
        legacy["version"] = json!(1);
        legacy["projects"][snapshot.projects[0].id.to_string()] = json!(DEFAULT_SPACE);
        legacy.as_object_mut().unwrap().remove("threads");
        snapshot.settings.insert(SETTING.into(), legacy);

        let upgraded = Spaces::load(&snapshot).unwrap();

        assert_eq!(
            upgraded.projects[&snapshot.projects[0].id],
            BTreeSet::from([DEFAULT_SPACE])
        );
        assert_eq!(
            upgraded.active_session().selected_thread,
            Some(snapshot.threads[0].id)
        );
        let saved = serde_json::to_value(upgraded).unwrap();
        assert_eq!(saved["version"], json!(VERSION));
        assert!(saved["projects"][snapshot.projects[0].id.to_string()].is_array());
        assert_eq!(
            saved["threads"][snapshot.threads[0].id.to_string()],
            json!(DEFAULT_SPACE)
        );
    }

    #[test]
    fn version_two_shared_thread_is_assigned_to_one_session() {
        let mut snapshot = fixture();
        let mut spaces = Spaces::load(&snapshot).unwrap();
        let project = snapshot.projects[0].id;
        let thread = snapshot.threads[0].id;
        let research = spaces.create("Research").unwrap();
        spaces.register_project(project, research);
        let session = spaces.sessions.get_mut(&research).unwrap();
        session.selected_project = Some(project);
        session.selected_thread = Some(thread);
        session.open_threads.push(thread);
        spaces.active = research;
        let mut legacy = serde_json::to_value(spaces).unwrap();
        legacy["version"] = json!(2);
        legacy.as_object_mut().unwrap().remove("threads");
        snapshot.settings.insert(SETTING.into(), legacy);

        let upgraded = Spaces::load(&snapshot).unwrap();

        assert_eq!(upgraded.threads[&thread], research);
        assert_eq!(upgraded.sessions[&research].selected_thread, Some(thread));
        assert!(
            !upgraded.sessions[&DEFAULT_SPACE]
                .open_threads
                .contains(&thread)
        );
    }

    #[test]
    fn migration_recovers_legacy_selected_factory_launch() {
        let mut snapshot = fixture();
        let launch = FactoryLaunch {
            id: Uuid::from_u128(3),
            project_id: snapshot.projects[0].id,
            factory_thread_id: snapshot.threads[0].id,
            descriptor: json!({}),
            event_path: None,
            work_item_id: None,
            attempt_id: None,
            process_exit: None,
            launch_error: None,
            created_at: "2026-09-17T00:00:00Z".into(),
        };
        snapshot.threads[0].kind = ThreadKind::Factory;
        snapshot.settings.remove("selected_thread");
        snapshot
            .settings
            .insert("selected_launch".into(), json!(launch.id));
        snapshot.launches.push(launch);
        assert_eq!(
            Spaces::load(&snapshot)
                .unwrap()
                .active_session()
                .selected_thread,
            Some(snapshot.threads[0].id)
        );
    }

    #[test]
    fn moving_project_moves_only_threads_from_the_source_space() {
        let mut snapshot = fixture();
        let project = snapshot.projects[0].id;
        let source_thread = snapshot.threads[0].id;
        let mut other_thread = snapshot.threads[0].clone();
        other_thread.id = Uuid::from_u128(7);
        let other_thread_id = other_thread.id;
        snapshot.threads.push(other_thread);
        let mut spaces = Spaces::load(&snapshot).unwrap();
        let target = spaces.create("Research").unwrap();
        let other = spaces.create("Personal").unwrap();
        spaces.register_project(project, target);
        spaces
            .move_thread(other_thread_id, project, DEFAULT_SPACE, other)
            .unwrap();

        spaces
            .move_project(project, DEFAULT_SPACE, target, &snapshot)
            .unwrap();

        assert_eq!(spaces.threads[&source_thread], target);
        assert_eq!(spaces.threads[&other_thread_id], other);
        assert_eq!(spaces.projects[&project], BTreeSet::from([target, other]));
    }

    #[test]
    fn moving_whole_project_and_deleting_space_never_mutates_threads() {
        let mut snapshot = fixture();
        let mut archived = snapshot.threads[0].clone();
        archived.id = Uuid::from_u128(7);
        archived.archived = true;
        archived.draft = "Archived draft also belongs to this project".into();
        let archived_id = archived.id;
        snapshot.threads.push(archived);
        snapshot.settings.insert(
            "open_threads".into(),
            json!([snapshot.threads[0].id, archived_id]),
        );
        let project = snapshot.projects[0].id;
        let thread = snapshot.threads[0].id;
        let mut spaces = Spaces::load(&snapshot).unwrap();
        let target = spaces.create("Work").unwrap();
        spaces
            .move_project(project, DEFAULT_SPACE, target, &snapshot)
            .unwrap();
        assert_eq!(spaces.active, DEFAULT_SPACE);
        assert_eq!(spaces.projects[&project], BTreeSet::from([target]));
        let source = spaces.active_session();
        assert!(source.selected_project.is_none());
        assert!(source.selected_thread.is_none());
        assert!(source.open_threads.is_empty());
        assert!(source.collapsed.is_empty());
        assert!(source.layout.is_none());
        spaces.switch(target).unwrap();
        assert!(
            spaces.active_session().selected_thread.is_none(),
            "Moving does not auto-focus"
        );
        let destination = spaces.sessions.get_mut(&target).unwrap();
        destination.selected_project = Some(project);
        destination.selected_thread = Some(thread);
        destination.open_threads.push(thread);
        destination.open_threads.push(archived_id);
        destination.layout = Some(json!({"destination": "layout"}));
        spaces.switch(DEFAULT_SPACE).unwrap();
        spaces = reload(&spaces, &snapshot);
        spaces.switch(target).unwrap();
        assert_eq!(spaces.active_session().selected_thread, Some(thread));
        assert_eq!(
            spaces.active_session().layout,
            Some(json!({"destination": "layout"}))
        );
        assert!(
            spaces.delete(target, &snapshot).is_err(),
            "Deleting active space is rejected"
        );
        spaces.switch(DEFAULT_SPACE).unwrap();
        spaces.delete(target, &snapshot).unwrap();
        spaces = reload(&spaces, &snapshot);
        assert_eq!(spaces.projects[&project], BTreeSet::from([DEFAULT_SPACE]));
        assert!(!spaces.sessions.contains_key(&target));
        assert!(spaces.active_session().open_threads.is_empty());
    }

    #[test]
    fn invalid_names_and_identities_leave_spaces_unchanged() {
        let snapshot = fixture();
        let mut spaces = Spaces::load(&snapshot).unwrap();
        let work = spaces.create("Work").unwrap();
        let before = serde_json::to_value(&spaces).unwrap();
        for name in ["", "   ", "default", " WORK ", "bad\nname", &"x".repeat(81)] {
            assert!(spaces.create(name).is_err());
        }
        let missing = Uuid::from_u128(999);
        assert!(spaces.switch(missing).is_err());
        assert!(spaces.rename(missing, "Missing").is_err());
        assert!(spaces.rename(work, "Default").is_err());
        assert!(
            spaces
                .move_project(snapshot.projects[0].id, DEFAULT_SPACE, missing, &snapshot)
                .is_err()
        );
        assert!(
            spaces
                .move_project(missing, DEFAULT_SPACE, work, &snapshot)
                .is_err()
        );
        assert!(spaces.delete(missing, &snapshot).is_err());
        assert!(spaces.delete(DEFAULT_SPACE, &snapshot).is_err());
        assert_eq!(serde_json::to_value(&spaces).unwrap(), before);
        spaces.rename(work, "  Personal  ").unwrap();
        assert_eq!(
            reload(&spaces, &snapshot)
                .spaces
                .iter()
                .find(|space| space.id == work)
                .unwrap()
                .name,
            "Personal"
        );
    }

    #[test]
    fn corrupt_or_future_metadata_is_not_migrated_or_overwritten() {
        let snapshot = fixture();
        let mut valid = Spaces::load(&snapshot).unwrap();
        let work = valid.create("Work").unwrap();
        let mut unknown_version = serde_json::to_value(&valid).unwrap();
        unknown_version["version"] = json!(99);
        let mut invalid_active = serde_json::to_value(&valid).unwrap();
        invalid_active["active"] = json!(Uuid::from_u128(999));
        let mut invalid_membership = serde_json::to_value(&valid).unwrap();
        invalid_membership["projects"][snapshot.projects[0].id.to_string()] =
            json!([Uuid::from_u128(999)]);
        let mut cross_space_selection = serde_json::to_value(&valid).unwrap();
        cross_space_selection["sessions"][work.to_string()]["selected_thread"] =
            json!(snapshot.threads[0].id);
        for invalid in [
            json!("malformed"),
            unknown_version,
            invalid_active,
            invalid_membership,
            cross_space_selection,
        ] {
            let mut saved = snapshot.clone();
            saved.settings.insert(SETTING.into(), invalid.clone());
            assert!(Spaces::load(&saved).is_err());
            let recovery = Spaces::recovery(&saved);
            assert!(recovery.active_session().selected_thread.is_none());
            assert!(recovery.active_session().layout.is_none());
        }
    }

    #[test]
    fn project_saved_before_membership_recovers_into_default() {
        let mut snapshot = fixture();
        let spaces = Spaces::load(&snapshot).unwrap();
        snapshot
            .settings
            .insert(SETTING.into(), serde_json::to_value(spaces).unwrap());
        let mut recovered_project = snapshot.projects[0].clone();
        recovered_project.id = Uuid::from_u128(5);
        recovered_project.root = PathBuf::from("/fixture/interrupted-registration");
        snapshot.projects.push(recovered_project.clone());
        let spaces = Spaces::load(&snapshot).unwrap();
        assert_eq!(
            spaces.projects[&recovered_project.id],
            BTreeSet::from([DEFAULT_SPACE])
        );
        assert_eq!(
            reload(&spaces, &snapshot).projects[&recovered_project.id],
            BTreeSet::from([DEFAULT_SPACE])
        );
    }

    #[test]
    fn registering_project_in_another_space_keeps_both_memberships() {
        let snapshot = fixture();
        let mut spaces = Spaces::load(&snapshot).unwrap();
        let project = snapshot.projects[0].id;
        let other = spaces.create("Research").unwrap();

        spaces.register_project(project, other);

        let saved = serde_json::to_value(&spaces).unwrap();
        let memberships = saved["projects"]
            .as_object()
            .unwrap()
            .get(&project.to_string())
            .unwrap()
            .as_array()
            .expect("a project is persisted with every Space membership");
        assert_eq!(memberships.len(), 2);
        assert!(memberships.contains(&json!(DEFAULT_SPACE)));
        assert!(memberships.contains(&json!(other)));
    }

    #[test]
    fn sharing_project_with_another_space_does_not_share_its_threads() {
        let snapshot = fixture();
        let mut spaces = Spaces::load(&snapshot).unwrap();
        let project = snapshot.projects[0].id;
        let thread = snapshot.threads[0].id;
        let other = spaces.create("Research").unwrap();

        spaces.register_project(project, other);

        let saved = serde_json::to_value(&spaces).unwrap();
        assert_eq!(saved["threads"][thread.to_string()], json!(DEFAULT_SPACE));
        assert_ne!(saved["threads"][thread.to_string()], json!(other));
    }

    #[test]
    fn moving_thread_reassigns_it_once_and_keeps_project_in_both_spaces() {
        let snapshot = fixture();
        let mut spaces = Spaces::load(&snapshot).unwrap();
        let project = snapshot.projects[0].id;
        let thread = snapshot.threads[0].id;
        let other = spaces.create("Research").unwrap();

        spaces
            .move_thread(thread, project, DEFAULT_SPACE, other)
            .unwrap();

        assert_eq!(spaces.threads.get(&thread), Some(&other));
        assert_eq!(
            spaces.projects[&project],
            BTreeSet::from([DEFAULT_SPACE, other])
        );
        let saved = serde_json::to_value(&spaces).unwrap();
        assert_eq!(saved["threads"][thread.to_string()], json!(other));
        assert!(spaces.active_session().open_threads.is_empty());
        assert!(spaces.active_session().selected_thread.is_none());
    }

    #[test]
    fn delayed_registration_keeps_origin_or_recovers_deleted_origin() {
        let snapshot = fixture();
        let mut spaces = Spaces::load(&snapshot).unwrap();
        let origin = spaces.create("Work").unwrap();
        let other = spaces.create("Personal").unwrap();
        spaces.switch(other).unwrap();
        let project = Uuid::from_u128(5);
        spaces.register_project(project, origin);
        assert_eq!(spaces.projects[&project], BTreeSet::from([origin]));
        assert_eq!(spaces.active, other);
        assert!(spaces.active_session().selected_project.is_none());
        spaces.delete(origin, &snapshot).unwrap();
        assert_eq!(spaces.projects[&project], BTreeSet::from([DEFAULT_SPACE]));
        let later_project = Uuid::from_u128(6);
        spaces.register_project(later_project, origin);
        assert_eq!(
            spaces.projects[&later_project],
            BTreeSet::from([DEFAULT_SPACE])
        );
        assert_eq!(spaces.active, other);
    }
}
