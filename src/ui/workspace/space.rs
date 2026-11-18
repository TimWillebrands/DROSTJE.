use super::*;

#[derive(Clone, Copy)]
pub(super) enum SpaceChange {
    Switch(SpaceId),
    Move(ProjectId, SpaceId),
    MoveThread(ThreadId, SpaceId),
    Delete(SpaceId),
}

#[derive(Default)]
pub(super) struct SpaceSlide {
    pub(super) offset: f32,
    pub(super) started: Option<Instant>,
    pub(super) neighbor: Option<SpaceId>,
    pub(super) neighbor_query: SharedString,
    pub(super) neighbor_scroll: Option<ScrollHandle>,
}

impl SpaceSlide {
    pub(super) const DURATION: Duration = Duration::from_millis(160);

    pub(super) fn drag(&mut self, progress: f32) {
        self.offset = progress;
        self.started = None;
    }

    pub(super) fn settle(&mut self, now: Instant) {
        if self.started.is_none() && self.offset != 0. {
            self.started = Some(now);
        }
    }

    pub(super) fn sample(&self, now: Instant) -> (f32, bool) {
        let Some(started) = self.started else {
            return (self.offset, false);
        };
        let elapsed =
            now.saturating_duration_since(started).as_secs_f32() / Self::DURATION.as_secs_f32();
        if elapsed >= 1. {
            return (0., false);
        }
        (self.offset * (1. - elapsed).powi(3), true)
    }
}

impl Workspace {
    pub(super) fn space_transition_reason(&self, cx: &App) -> Option<String> {
        self.state.read(cx).space_mutation_reason().or_else(|| {
            if self.terminal_confirmation {
                return Some(
                    "Finish or cancel the terminal confirmation before changing spaces.".into(),
                );
            }
            if self.preparing_space_change {
                Some("Wait for the editor draft to finish saving before changing spaces.".into())
            } else if self.extension.is_some() {
                Some("Finish or dismiss the agent request before changing spaces.".into())
            } else if self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
            {
                Some("Complete or cancel input composition before changing spaces.".into())
            } else {
                None
            }
        })
    }

    pub(in crate::ui) fn swipe_space(
        &mut self,
        event: &ScrollEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ScrollDelta::Pixels(delta) = event.delta else {
            return;
        };
        if self.has_open_form(cx)
            || self.palette.is_some()
            || self.theme_editor.is_some()
            || window.has_active_dialog(cx)
            || window.has_active_sheet(cx)
            || self.state.read(cx).spaces.len() < 2
            || self.space_transition_reason(cx).is_some()
        {
            self.space_swipe.finish();
            self.space_swipe_idle.take();
            if self.space_slide.offset != 0. && self.space_slide.started.is_none() {
                self.space_slide.settle(Instant::now());
                self.projects.update(cx, |_, cx| cx.notify());
            }
            return;
        }
        let now = Instant::now();
        let command =
            self.space_swipe
                .feed(delta.map(|axis| axis.as_f32()), event.touch_phase, now);
        if let Some(command) = command {
            self.space_swipe_idle.take();
            self.execute(command, window, cx);
            if !self.preparing_space_change {
                self.space_slide.settle(now);
            }
        } else if let Some(distance) = self.space_swipe.progress() {
            if distance == 0. && self.space_slide.sample(now).0 == 0. {
                return;
            }
            let width = self
                .projects
                .read(cx)
                .bounds
                .get()
                .map_or(1., |bounds| bounds.size.width.as_f32())
                .max(1.);
            let progress = (distance / width).clamp(-1., 1.);
            self.space_slide.drag(progress);
            let state = self.state.read(cx);
            let current = state
                .spaces
                .iter()
                .position(|space| space.id == state.active_space)
                .unwrap_or(0);
            let count = state.spaces.len();
            let adjacent = if progress > 0. {
                (current + count - 1) % count
            } else {
                (current + 1) % count
            };
            self.space_slide.neighbor = (progress != 0.).then_some(state.spaces[adjacent].id);
            self.space_slide.neighbor_query = SharedString::default();
            self.space_slide.neighbor_scroll = None;
            self.space_swipe_idle.take();
            if self.space_swipe.needs_idle_release() {
                self.space_swipe_idle =
                    Some(cx.spawn_in(window, async move |workspace, window| {
                        smol::Timer::after(projects::SPACE_SWIPE_IDLE).await;
                        let _ = workspace.update_in(window, |workspace, window, cx| {
                            if let Some(command) = workspace.space_swipe.release() {
                                workspace.execute(command, window, cx);
                            }
                            if !workspace.preparing_space_change {
                                workspace.space_slide.settle(Instant::now());
                            }
                            workspace.projects.update(cx, |_, cx| cx.notify());
                        });
                    }));
            }
        } else {
            self.space_swipe_idle.take();
            self.space_slide.settle(now);
        }
        if cx.reduce_motion() {
            self.space_slide = SpaceSlide::default();
        }
        self.projects.update(cx, |_, cx| cx.notify());
    }

    pub(in crate::ui) fn switch_space(
        &mut self,
        id: SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if id == self.state.read(cx).active_space {
            return Ok(());
        }
        let result = self.change_space(SpaceChange::Switch(id), window, cx);
        if result.is_err() {
            self.space_direction = None;
            self.space_slide.settle(Instant::now());
            cx.notify();
        }
        result
    }

    pub(in crate::ui) fn move_project_to_space(
        &mut self,
        project: ProjectId,
        target: SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        self.change_space(SpaceChange::Move(project, target), window, cx)
    }

    pub(in crate::ui) fn move_thread_to_space(
        &mut self,
        thread: ThreadId,
        target: SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        self.change_space(SpaceChange::MoveThread(thread, target), window, cx)
    }

    pub(super) fn change_space(
        &mut self,
        change: SpaceChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if let Some(reason) = self.space_transition_reason(cx) {
            anyhow::bail!("{reason}");
        }
        let editor = self.editor.clone().and_then(|editor| {
            let key = editor.read(cx).active_key().cloned()?;
            Some((editor, key))
        });
        let Some((editor, key)) = editor else {
            return self.apply_space_change(change, window, cx);
        };
        let snapshot = editor.update(cx, |editor, cx| editor.snapshot(&key, window, cx));
        let origin = self.state.read(cx).active_space;
        self.preparing_space_change = true;
        cx.spawn_in(window, async move |workspace, window| {
            let result = snapshot.await;
            let _ = workspace.update_in(window, |workspace, window, cx| {
                workspace.preparing_space_change = false;
                let result = result.and_then(|draft| {
                    workspace.state.update(cx, |state, _| {
                        let thread = match &draft.key {
                            crate::model::DraftKey::Thread(id) | crate::model::DraftKey::Extension { thread: id, .. } => *id,
                        };
                        state.record_draft(draft);
                        state.commit_thread_edits(thread);
                    });
                    anyhow::ensure!(workspace.state.read(cx).active_space == origin,
                        "Space changed while saving the draft; choose the destination again");
                    if let Some(reason) = workspace.space_transition_reason(cx) {
                        anyhow::bail!("{reason}");
                    }
                    workspace.apply_space_change(change, window, cx)
                });
                if let Err(error) = result {
                    workspace.space_direction = None;
                    workspace.space_slide.settle(Instant::now());
                    workspace.space_error(error.context("Space was not changed; finish or cancel the editor command and try again"), cx);
                }
                cx.notify();
            });
        }).detach();
        Ok(())
    }

    pub(super) fn apply_space_change(
        &mut self,
        change: SpaceChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        self.layout_save.take();
        self.flush_layout(cx);
        self.state.update(cx, |state, _| -> anyhow::Result<()> {
            match change {
                SpaceChange::Switch(id) => state.switch_space(id)?,
                SpaceChange::Move(project, target) => {
                    state.move_project_to_space(project, target)?
                }
                SpaceChange::MoveThread(thread, target) => {
                    state.move_thread_to_space(thread, target)?
                }
                SpaceChange::Delete(id) => {
                    anyhow::ensure!(id != state.default_space_id(), "Default cannot be deleted");
                    if state.active_space == id {
                        state.switch_space(state.default_space_id())?;
                    }
                    state.delete_space(id)?;
                }
            }
            Ok(())
        })?;
        for panel in self.terminals.borrow().values() {
            if let Some(pane) = panel.read(cx).terminal.clone() {
                let target = match change {
                    SpaceChange::Move(project, target) if pane.read(cx).project_id == project => {
                        Some(target)
                    }
                    SpaceChange::Delete(space) if pane.read(cx).space_id == space => {
                        Some(self.state.read(cx).default_space_id())
                    }
                    _ => None,
                };
                if let Some(target) = target {
                    pane.update(cx, |pane, cx| {
                        pane.space_id = target;
                        pane.set_visible(false, cx);
                    });
                }
            }
        }
        let terminal_spaces: BTreeMap<_, _> = self
            .terminals
            .borrow()
            .iter()
            .filter_map(|(id, panel)| {
                panel
                    .read(cx)
                    .terminal
                    .as_ref()
                    .map(|pane| (*id, pane.read(cx).space_id))
            })
            .collect();
        for (space, layout) in &mut self.live_layouts {
            *layout = filter_layout(layout.clone(), &|panel| {
                terminal_of(panel).is_none_or(|id| terminal_spaces.get(&id) == Some(space))
            });
        }
        self.sync_space(window, cx);
        self.sync_thread_panels(window, cx);
        self.sync_session(window, cx);
        if matches!(change, SpaceChange::Move(..) | SpaceChange::MoveThread(..)) {
            self.cursor = None;
            self.flush_layout(cx);
            self.focus_region(Region::Projects, window, cx);
        }
        self.state.update(cx, |_, cx| cx.notify());
        Ok(())
    }

    pub(super) fn sync_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.state.read(cx).active_space;
        if active == self.presented_space {
            return;
        }
        let outgoing_layout = self.dock.read(cx).dump(cx);
        let shared_sidebar = ProjectsSidebar::capture(&outgoing_layout);
        let direction = self.space_direction.take().unwrap_or_else(|| {
            let spaces = &self.state.read(cx).spaces;
            let old = spaces
                .iter()
                .position(|space| space.id == self.presented_space);
            let new = spaces.iter().position(|space| space.id == active);
            if new > old { -1. } else { 1. }
        });
        self.space_swipe.finish();
        self.space_swipe_idle.take();
        let now = Instant::now();
        let offset = if self.space_slide.neighbor == Some(active) {
            self.space_slide.sample(now).0 - direction
        } else {
            -direction
        };
        self.space_slide.neighbor = Some(self.presented_space);
        self.space_slide.neighbor_query = self.thread_search.read(cx).value();
        self.space_slide.neighbor_scroll = Some(
            self.projects
                .update(cx, |panel, _| std::mem::take(&mut panel.scroll)),
        );
        self.space_slide.drag(offset);
        self.space_slide.settle(now);
        if cx.reduce_motion() {
            self.space_slide = SpaceSlide::default();
        }
        self.layout_save.take();
        self.hide_terminals(cx);
        self.live_layouts.insert(
            self.presented_space,
            filter_layout(outgoing_layout, &|panel| {
                terminal_of(panel).is_none_or(|id| {
                    self.terminals
                        .borrow()
                        .get(&id)
                        .and_then(|panel| panel.read(cx).terminal.as_ref())
                        .is_some_and(|pane| pane.read(cx).space_id == self.presented_space)
                })
            }),
        );
        if let Some(id) = self.active_terminal {
            self.space_terminals.insert(self.presented_space, id);
        }
        self.active_terminal = self.space_terminals.get(&active).copied();
        self.presented_space = active;
        self.rebuilding = true;
        self.threads.borrow_mut().clear();
        self.welcome.borrow_mut().take();
        self.cursor = None;
        self.presented_thread = None;
        self.unrevealed_thread = None;
        self.reopening = None;
        self.thread_search
            .update(cx, |input, cx| input.set_value("", window, cx));
        let saved = self.live_layouts.get(&active).cloned().map(Ok).or_else(|| {
            self.state
                .read(cx)
                .space_layout()
                .cloned()
                .map(serde_json::from_value)
        });
        let restored = saved.map(|saved| -> anyhow::Result<()> {
            let mut layout: DockAreaState = saved?;
            if let Some(sidebar) = shared_sidebar {
                sidebar.apply(&mut layout);
            }
            let layout = self.restorable_layout(layout, cx);
            validate_layout(&durable_layout(layout.clone()))?;
            self.dock
                .update(cx, |dock, cx| dock.load(layout, window, cx))
        });
        self.rebuilding = false;
        if !matches!(restored, Some(Ok(()))) {
            if let Some(Err(error)) = restored
                && error.to_string() != OUTDATED_LAYOUT
            {
                self.state.update(cx, |state, _| {
                    state.notice(
                        NoticeLevel::Warning,
                        format!("Only this space's saved layout was reset: {error:#}"),
                    )
                });
            }
            self.reset_layout_with_sidebar(shared_sidebar, window, cx);
        } else {
            self.sync_thread_panels(window, cx);
            self.invalidate_panel_bounds(cx);
            self.normalize_layout(None, window, cx);
        }
        self.sync_session(window, cx);
        self.sync_factory_form(window, cx);
        self.sync_selection_panels(window, cx);
        self.projects.update(cx, |_, cx| cx.notify());
        cx.defer_in(window, move |workspace, window, cx| {
            if workspace.presented_space == active {
                let region = if workspace.state.read(cx).selected_thread.is_some()
                    || workspace.has_active_terminal(cx)
                    || ProjectsSidebar::capture(&workspace.dock.read(cx).dump(cx))
                        .is_some_and(|sidebar| !sidebar.open)
                {
                    Region::Thread
                } else {
                    Region::Projects
                };
                workspace.focus_region(region, window, cx);
            }
        });
        cx.notify();
    }

    pub(super) fn space_error(&self, error: anyhow::Error, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.notice(NoticeLevel::Warning, format!("{error:#}"));
            cx.notify();
        });
    }

    pub(super) fn open_space_picker(
        &mut self,
        moving: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(reason) = self.space_transition_reason(cx) {
            self.space_error(anyhow::anyhow!(reason), cx);
            return;
        }
        self.dismiss(window, cx);
        if self.navigation_blocked(window, cx) {
            return;
        }
        self.palette = Some(Palette::open_spaces(
            cx.entity().downgrade(),
            self.state.clone(),
            moving,
            window,
            cx,
        ));
    }
}
