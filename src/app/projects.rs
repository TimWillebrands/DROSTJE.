use super::*;

impl AppState {
    pub fn project(&self) -> Option<&Project> {
        self.selected_project
            .filter(|id| self.project_in_active_space(*id))
            .and_then(|id| self.snapshot.projects.iter().find(|p| p.id == id))
    }

    pub fn mutable_project_reason(&self) -> Option<String> {
        if self.shutting_down {
            return Some("Application is shutting down".into());
        }
        if let Some(error) = &self.storage_error {
            return Some(format!("Storage is read-only: {error}"));
        }
        match self.project() {
            None => Some("Select a project first.".into()),
            Some(p) if p.archived => Some("This project is archived. History is read-only.".into()),
            _ => None,
        }
    }
    pub fn archive_project_reason(&self) -> Option<String> {
        if let Some(reason) = self.mutable_project_reason() {
            return Some(reason);
        }
        let id = self.selected_project?;
        if self.snapshot.threads.iter().any(|t| {
            t.project_id == id
                && (self.active_threads.contains(&t.id) || self.pending_factory.contains(&t.id))
        }) || self
            .snapshot
            .launches
            .iter()
            .any(|l| l.project_id == id && self.owned_launches.contains(&l.id))
        {
            Some("Stop active interactive turns and wait for owned Factory processes before archiving.".into())
        } else {
            None
        }
    }
    pub fn unarchive_project(&mut self, id: ProjectId) -> anyhow::Result<()> {
        anyhow::ensure!(!self.shutting_down, "Application is shutting down");
        if let Some(error) = &self.storage_error {
            anyhow::bail!("Storage is read-only: {error}");
        }
        let mut project = self
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Project is unavailable"))?;
        anyhow::ensure!(project.archived, "Project is not archived.");
        project.archived = false;
        self.save_project(project);
        Ok(())
    }

    pub fn select_project(&mut self, id: ProjectId) {
        if !self.project_in_active_space(id) {
            return;
        }
        self.selected_project = Some(id);
        if self.thread().is_some_and(|t| t.project_id != id) {
            if let Some(previous) = self.selected_thread {
                self.commit_thread_edits(previous);
            }
            self.selected_thread = None;
            self.select_launch_output(None);
        }
        self.persist_space_session();
    }
    /// A terminal has a project but no thread; keep open tabs and commit the previous draft.
    pub fn select_terminal_project(&mut self, id: ProjectId) {
        if !self.project_in_active_space(id) {
            return;
        }
        if let Some(previous) = self.selected_thread {
            self.commit_thread_edits(previous);
        }
        self.selected_project = Some(id);
        self.selected_thread = None;
        self.select_launch_output(None);
        self.persist_space_session();
    }

    /// Closes every open tab of `project`, committing their drafts.
    pub fn close_project_threads(&mut self, project: ProjectId) {
        let open: Vec<_> = self
            .open_threads
            .iter()
            .copied()
            .filter(|id| {
                self.snapshot
                    .threads
                    .iter()
                    .any(|thread| thread.id == *id && thread.project_id == project)
            })
            .collect();
        for id in open {
            self.close_thread(id);
        }
    }

    pub fn register_project(&mut self, path: PathBuf) {
        if self.space_mutation_reason().is_some() || self.registering {
            return;
        }
        self.registering = true;
        let sender = self.events.clone();
        let space = self.active_space;
        smol::spawn(async move {
            let result = smol::unblock(move || {
                platform::inspect_repository(&path).map_err(|e| format!("{e:#}"))
            })
            .await;
            let _ = sender.send(AppEvent::ProjectResolved(space, result)).await;
        })
        .detach();
    }
}
