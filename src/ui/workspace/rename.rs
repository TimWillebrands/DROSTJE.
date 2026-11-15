//! Inline rename of a project, Space or thread.

use super::*;

pub(super) struct InlineRename {
    pub(super) target: RenameTarget,
    pub(super) surface: RenameSurface,
    pub(super) input: Entity<InputState>,
    pub(super) error: Option<String>,
    pub(super) return_focus: Option<FocusHandle>,
    pub(super) _subscription: Subscription,
}

impl Workspace {
    pub(in crate::ui) fn rename_thread(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_rename(RenameTarget::Thread(id), RenameSurface::Sidebar, window, cx);
    }
    pub(in crate::ui) fn start_rename(
        &mut self,
        target: RenameTarget,
        surface: RenameSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.read(cx);
        let (name, reason) = match target {
            RenameTarget::Project(id) => {
                let project = state
                    .snapshot
                    .projects
                    .iter()
                    .find(|project| project.id == id);
                (
                    project.map(|project| project.name.clone()),
                    state.mutable_project_reason().or_else(|| {
                        project
                            .filter(|project| project.archived)
                            .map(|_| "Archived projects are read-only.".into())
                    }),
                )
            }
            RenameTarget::Space(id) => (
                state
                    .spaces
                    .iter()
                    .find(|space| space.id == id)
                    .map(|space| space.name.clone()),
                state.space_mutation_reason(),
            ),
            RenameTarget::Thread(id) => (
                state
                    .snapshot
                    .threads
                    .iter()
                    .find(|thread| thread.id == id)
                    .map(|thread| thread.title.clone()),
                state.rename_thread_reason_for(id),
            ),
        };
        if let Some(reason) = reason {
            self.state.update(cx, |state, cx| {
                state.notice(NoticeLevel::Warning, reason);
                cx.notify();
            });
            return;
        }
        let Some(name) = name else { return };
        let return_focus = window.focused(cx);
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(name, window, cx);
            input
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |workspace, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    workspace.commit_rename(window, cx);
                }
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        input.update(cx, |input, cx| input.select_all(window, cx));
        self.inline_rename = Some(InlineRename {
            target,
            surface,
            input,
            error: None,
            return_focus,
            _subscription: subscription,
        });
        cx.notify();
    }
    pub(in crate::ui) fn inline_rename(
        &self,
    ) -> Option<(
        RenameTarget,
        RenameSurface,
        Entity<InputState>,
        Option<String>,
    )> {
        self.inline_rename.as_ref().map(|rename| {
            (
                rename.target,
                rename.surface,
                rename.input.clone(),
                rename.error.clone(),
            )
        })
    }
    pub(in crate::ui) fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(rename) = self.inline_rename.take() {
            rename
                .return_focus
                .as_ref()
                .unwrap_or(&self.focus)
                .focus(window, cx);
            cx.notify();
        }
    }
    pub(in crate::ui) fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.inline_rename.as_ref() else {
            return;
        };
        let target = rename.target;
        let value = rename.input.read(cx).value().to_string();
        let result = self.state.update(cx, |state, cx| -> anyhow::Result<()> {
            anyhow::ensure!(state.storage_error.is_none(), "Storage is not writable");
            anyhow::ensure!(!value.trim().is_empty(), "Name cannot be blank");
            match target {
                RenameTarget::Project(id) => {
                    if let Some(reason) = state.mutable_project_reason() {
                        anyhow::bail!(reason);
                    }
                    let mut project = state
                        .snapshot
                        .projects
                        .iter()
                        .find(|project| project.id == id)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("Project is unavailable"))?;
                    anyhow::ensure!(!project.archived, "Archived projects are read-only.");
                    project.name = value;
                    state.save_project(project);
                }
                RenameTarget::Space(id) => {
                    if let Some(reason) = state.space_mutation_reason() {
                        anyhow::bail!(reason);
                    }
                    state.rename_space(id, &value)?;
                }
                RenameTarget::Thread(id) => {
                    if let Some(reason) = state.rename_thread_reason_for(id) {
                        anyhow::bail!(reason);
                    }
                    let mut thread = state
                        .snapshot
                        .threads
                        .iter()
                        .find(|thread| thread.id == id)
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("Thread is unavailable"))?;
                    thread.title = value;
                    thread.updated_at = crate::platform::now();
                    state.save_thread(thread);
                }
            }
            anyhow::ensure!(state.storage_error.is_none(), "Could not save the new name");
            cx.notify();
            Ok(())
        });
        match result {
            Ok(()) => self.cancel_rename(window, cx),
            Err(error) => {
                if let Some(rename) = self.inline_rename.as_mut() {
                    rename.error = Some(format!("{error:#}"));
                    rename.input.update(cx, |input, cx| input.focus(window, cx));
                }
            }
        }
        cx.notify();
    }
}
