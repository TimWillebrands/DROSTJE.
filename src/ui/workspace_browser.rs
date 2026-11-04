use super::*;
use crate::{
    browser::{BrowserCommand, BrowserId, BrowserRuntime},
    browser_tools::{
        ArtifactPlacement, BrowserAutomation, BrowserToolOperation, BrowserToolRequest,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct BrowserRecord {
    id: BrowserId,
    url: String,
    title: String,
    space: SpaceId,
    thread: Option<ThreadId>,
    artifact: Option<String>,
    /// Where the artifact shows now. Invariant: at most one `Composer` per thread.
    #[serde(default)]
    placement: ArtifactPlacement,
}
/// Session lifetime is independent of virtualised transcript rows and dock attachments.
pub(in crate::ui) struct Browsers {
    pub runtime: BrowserRuntime,
    pub panes: BTreeMap<BrowserId, Entity<BrowserPane>>,
    pub panels: Rc<RefCell<BTreeMap<BrowserId, Entity<WorkspacePanel>>>>,
    records: BTreeMap<BrowserId, BrowserRecord>,
    pub active: Option<BrowserId>,
    pub focused: Option<BrowserId>,
    observed: std::collections::HashSet<BrowserId>,
    /// Theme tokens artifacts were last given; unchanged themes are not pushed again.
    artifact_theme: String,
}
impl Drop for Browsers {
    fn drop(&mut self) {
        self.runtime.shutdown();
    }
}
impl Browsers {
    pub fn new(
        state: &Entity<AppState>,
        owner: &WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let runtime = BrowserRuntime::new();
        let records: Vec<BrowserRecord> = state
            .read(cx)
            .snapshot
            .settings
            .get("browser_sessions")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        let panels: Rc<RefCell<BTreeMap<BrowserId, Entity<WorkspacePanel>>>> = Rc::default();
        let mut panes = BTreeMap::new();
        let theme = crate::ui::theme::artifact_tokens(cx);
        let mut records = records;
        single_dock(&mut records);
        for record in &records {
            let pane = cx.new(|cx| {
                if record.artifact.is_some() {
                    let mut pane = BrowserPane::artifact(
                        record.id,
                        record.url.clone(),
                        record.title.clone(),
                        theme.clone(),
                        runtime.clone(),
                        window,
                        cx,
                    );
                    pane.set_docked(record.placement == ArtifactPlacement::Composer, cx);
                    pane
                } else {
                    BrowserPane::new(record.id, record.url.clone(), runtime.clone(), window, cx)
                }
            });
            if record.thread.is_none() && record.artifact.is_none() {
                let panel = cx.new(|cx| {
                    let mut panel = WorkspacePanel::new(
                        PanelKind::Browser(record.id),
                        state.clone(),
                        owner.clone(),
                        window,
                        cx,
                    );
                    panel.browser = Some(pane.clone());
                    panel
                });
                panels.borrow_mut().insert(record.id, panel);
            }
            panes.insert(record.id, pane);
        }
        let registered = panels.clone();
        let state = state.clone();
        let owner = owner.clone();
        let engine = runtime.clone();
        register_panel(cx, "Browser", move |ctx, window, cx| {
            let id = browser_of(ctx.state()).unwrap_or_else(Uuid::new_v4);
            if let Some(panel) = registered.borrow().get(&id).cloned() {
                return panel_handle(panel);
            }
            let url = match &ctx.state().info {
                PanelInfo::Panel(value) => {
                    value["url"].as_str().unwrap_or("about:blank").to_owned()
                }
                _ => "about:blank".into(),
            };
            let pane = cx.new(|cx| BrowserPane::new(id, url, engine.clone(), window, cx));
            let panel = cx.new(|cx| {
                let mut panel = WorkspacePanel::new(
                    PanelKind::Browser(id),
                    state.clone(),
                    owner.clone(),
                    window,
                    cx,
                );
                panel.browser = Some(pane);
                panel
            });
            registered.borrow_mut().insert(id, panel.clone());
            panel_handle(panel)
        });
        Self {
            runtime,
            panes,
            panels,
            records: records
                .into_iter()
                .map(|record| (record.id, record))
                .collect(),
            active: None,
            focused: None,
            observed: Default::default(),
            artifact_theme: theme,
        }
    }
}
/// The placement changes that show `id` at `placement` while `thread` keeps at most one
/// docked artifact: docking `id` moves any other docked artifact into the transcript.
fn placement_moves<'a>(
    records: impl Iterator<Item = &'a BrowserRecord>,
    thread: ThreadId,
    id: BrowserId,
    placement: ArtifactPlacement,
) -> Vec<(BrowserId, ArtifactPlacement)> {
    records
        .filter(|record| record.thread == Some(thread) && record.artifact.is_some())
        .filter_map(|record| {
            let wanted = if record.id == id {
                placement
            } else if placement == ArtifactPlacement::Composer {
                ArtifactPlacement::Conversation
            } else {
                record.placement
            };
            (wanted != record.placement).then_some((record.id, wanted))
        })
        .collect()
}

/// Restored layouts keep the one-dock-per-thread invariant; only Conversation artifacts dock.
fn single_dock(records: &mut [BrowserRecord]) {
    let mut docked = std::collections::HashSet::new();
    for record in records {
        if record.placement == ArtifactPlacement::Composer
            && !(record.artifact.is_some()
                && record.thread.is_some_and(|thread| docked.insert(thread)))
        {
            record.placement = ArtifactPlacement::Conversation;
        }
    }
}

pub(super) fn browser_of(state: &PanelState) -> Option<BrowserId> {
    match &state.info {
        PanelInfo::Panel(value) => serde_json::from_value(value.get("browser")?.clone()).ok(),
        _ => None,
    }
}
impl Workspace {
    /// Runtime QA observes the actual virtualized transcript cache, not a detached pane.
    #[cfg(feature = "runtime-qa")]
    pub fn artifact_transcript_scroll_extent(&self, thread: ThreadId, cx: &App) -> f32 {
        self.threads
            .borrow()
            .get(&thread)
            .and_then(|panel| panel.read(cx).transcript.as_ref())
            .map_or(0., |transcript| transcript.read(cx).scroll_extent())
    }
    pub(super) fn observe_browsers(&mut self, cx: &mut Context<Self>) {
        for (&id, pane) in &self.browsers.panes {
            if !self.browsers.observed.insert(id) {
                continue;
            }
            self._subscriptions.push(cx.subscribe(
                pane,
                move |workspace, _, _: &super::super::browser::BrowserNavigation, cx| {
                    let owner = cx.entity().downgrade();
                    cx.defer(move |cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.save_browsers(cx);
                            if let Some(panel) = workspace.browsers.panels.borrow().get(&id) {
                                panel.update(cx, |_, cx| cx.notify());
                            }
                        });
                    });
                    let _ = workspace;
                },
            ));
            // Only artifacts a Conversation showed have a channel, and only to that
            // Conversation. Ordinary pages never reach an agent this way.
            self._subscriptions.push(cx.subscribe(
                pane,
                move |workspace, _, channel: &super::super::browser::BrowserChannel, cx| {
                    let Some(record) = workspace.browsers.records.get(&id) else {
                        return;
                    };
                    let (Some(thread), Some(_)) = (record.thread, record.artifact.as_ref()) else {
                        return;
                    };
                    let title = record.title.clone();
                    let events = channel.0.clone();
                    workspace.state.update(cx, |state, _| {
                        state.artifact_events(thread, id, title, events)
                    });
                },
            ));
            self._subscriptions.push(cx.subscribe(
                pane,
                move |_, _, _: &super::super::browser::BrowserFocus, cx| {
                    let owner = cx.entity().downgrade();
                    cx.defer(move |cx| {
                        let _ = owner.update(cx, |workspace, cx| workspace.browser_focused(id, cx));
                    });
                },
            ));
            self._subscriptions.push(cx.subscribe(
                pane,
                move |_, _, _: &super::super::browser::BrowserLayout, cx| {
                    let owner = cx.entity().downgrade();
                    // Browser refresh and canvas layout must not borrow the list during layout.
                    cx.defer(move |cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            if let Some(thread) =
                                workspace.browsers.records.get(&id).and_then(|r| r.thread)
                            {
                                workspace.state.update(cx, |_, cx| {
                                    cx.emit(PresentationChange::Thread(thread));
                                });
                            }
                        });
                    });
                },
            ));
        }
    }
    pub(super) fn save_browsers(&self, cx: &mut Context<Self>) {
        let records: Vec<_> = self
            .browsers
            .records
            .values()
            .map(|record| {
                let mut record = record.clone();
                if let Some(pane) = self.browsers.panes.get(&record.id) {
                    record.url = pane.read(cx).url.clone();
                    if record.artifact.is_none() {
                        record.title = pane.read(cx).title.clone();
                    }
                }
                record
            })
            .collect();
        let value = json!(records);
        if self
            .state
            .read(cx)
            .snapshot
            .settings
            .get("browser_sessions")
            != Some(&value)
        {
            self.state
                .update(cx, |state, _| state.set_setting("browser_sessions", value));
        }
    }
    pub(super) fn browser_tab_count(&self, cx: &App) -> usize {
        let space = self.state.read(cx).active_space;
        self.browsers
            .records
            .values()
            .filter(|r| r.space == space && r.thread.is_none() && r.artifact.is_none())
            .count()
    }
    pub(in crate::ui) fn selected_browser(&self, cx: &App) -> Option<Entity<BrowserPane>> {
        self.browsers
            .focused
            .or(self.browsers.active)
            .and_then(|id| self.browsers.panes.get(&id).cloned())
            .or_else(|| {
                let state = self.state.read(cx);
                state
                    .selected_thread
                    .filter(|thread| {
                        state.thread_sidebar_open(*thread)
                            && state.thread_sidebar_tab(*thread) == SidebarTab::Browser
                    })
                    .and_then(|thread| self.sidebar_browser(thread))
            })
    }
    pub(in crate::ui) fn sidebar_browser(&self, thread: ThreadId) -> Option<Entity<BrowserPane>> {
        self.browsers
            .records
            .values()
            .find(|record| record.thread == Some(thread) && record.artifact.is_none())
            .and_then(|record| self.browsers.panes.get(&record.id))
            .cloned()
    }
    /// The artifact a Conversation's tool call showed, with where it shows now.
    pub(in crate::ui) fn artifact_browser(
        &self,
        thread: ThreadId,
        id: BrowserId,
    ) -> Option<(Entity<BrowserPane>, ArtifactPlacement)> {
        let record = self
            .browsers
            .records
            .get(&id)
            .filter(|record| record.thread == Some(thread) && record.artifact.is_some())?;
        Some((self.browsers.panes.get(&id)?.clone(), record.placement))
    }
    /// The artifact docked above a Conversation's composer, if any.
    pub(in crate::ui) fn docked_artifact(&self, thread: ThreadId) -> Option<Entity<BrowserPane>> {
        self.browsers
            .records
            .values()
            .find(|record| {
                record.thread == Some(thread)
                    && record.artifact.is_some()
                    && record.placement == ArtifactPlacement::Composer
            })
            .and_then(|record| self.browsers.panes.get(&record.id))
            .cloned()
    }
    /// Show an artifact in its transcript row or docked above the composer. Docking moves
    /// the previously docked artifact into the transcript; nothing is closed or reloaded.
    pub(super) fn place_artifact(
        &mut self,
        thread: ThreadId,
        id: BrowserId,
        placement: ArtifactPlacement,
        cx: &mut Context<Self>,
    ) {
        let moves = placement_moves(self.browsers.records.values(), thread, id, placement);
        if moves.is_empty() {
            return;
        }
        for (browser, wanted) in moves {
            if let Some(record) = self.browsers.records.get_mut(&browser) {
                record.placement = wanted;
            }
            if let Some(pane) = self.browsers.panes.get(&browser) {
                pane.update(cx, |pane, cx| {
                    pane.set_docked(wanted == ArtifactPlacement::Composer, cx)
                });
            }
        }
        self.save_browsers(cx);
        self.state
            .update(cx, |_, cx| cx.emit(PresentationChange::Thread(thread)));
    }
    pub(super) fn move_docked_artifact_into_conversation(
        &mut self,
        thread: ThreadId,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.docked_artifact(thread) {
            let id = pane.read(cx).id;
            self.place_artifact(thread, id, ArtifactPlacement::Conversation, cx);
        }
    }
    pub(super) fn browser_focused(&mut self, id: BrowserId, cx: &mut Context<Self>) {
        if !self.rebuilding
            && self
                .browsers
                .records
                .get(&id)
                .is_some_and(|record| record.space == self.presented_space)
        {
            self.browsers.focused = Some(id);
            if self
                .browsers
                .records
                .get(&id)
                .is_some_and(|record| record.thread.is_none())
            {
                self.browsers.active = Some(id);
            }
            self.active_terminal = None;
            cx.notify();
        }
    }
    pub(super) fn ensure_sidebar_browser(
        &mut self,
        thread: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_browser(thread).is_none() {
            self.open_browser("about:blank".into(), Some(thread), None, window, cx);
        }
    }
    pub(super) fn open_browser(
        &mut self,
        url: String,
        thread: Option<ThreadId>,
        artifact: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> BrowserId {
        if artifact.is_none()
            && let Some(thread) = thread
            && let Some(pane) = self.sidebar_browser(thread)
        {
            let id = pane.read(cx).id;
            self.browsers
                .runtime
                .command(id, BrowserCommand::Navigate { url });
            self.show_sidebar_tab(thread, SidebarTab::Browser, window, cx);
            return id;
        }
        let id = Uuid::new_v4();
        let theme = crate::ui::theme::artifact_tokens(cx);
        let pane = cx.new(|cx| {
            if artifact.is_some() {
                BrowserPane::artifact(
                    id,
                    url.clone(),
                    "HTML artifact".into(),
                    theme,
                    self.browsers.runtime.clone(),
                    window,
                    cx,
                )
            } else {
                BrowserPane::new(id, url.clone(), self.browsers.runtime.clone(), window, cx)
            }
        });
        let record = BrowserRecord {
            id,
            url,
            title: "Browser".into(),
            space: self.presented_space,
            thread,
            artifact: artifact.clone(),
            placement: ArtifactPlacement::Conversation,
        };
        self.browsers.records.insert(id, record);
        self.browsers.panes.insert(id, pane.clone());
        self.observe_browsers(cx);
        if let Some(thread) = thread {
            if artifact.is_none() {
                self.show_sidebar_tab(thread, SidebarTab::Browser, window, cx);
            }
            self.state
                .update(cx, |_, cx| cx.emit(PresentationChange::Thread(thread)));
        } else {
            let owner = cx.entity().downgrade();
            let state = self.state.clone();
            let panel = cx.new(|cx| {
                let mut panel =
                    WorkspacePanel::new(PanelKind::Browser(id), state, owner, window, cx);
                panel.browser = Some(pane.clone());
                panel
            });
            self.browsers.panels.borrow_mut().insert(id, panel.clone());
            self.dock.update(cx, |dock, cx| {
                dock.add_panel_view(
                    panel_handle(panel.clone()),
                    DockPlacement::Center,
                    None,
                    window,
                    cx,
                )
            });
            self.sync_thread_panels(window, cx);
            self.focus_panel(panel, window, cx);
            pane.update(cx, |pane, cx| pane.focus_address(window, cx));
            self.flush_layout(cx);
        }
        self.save_browsers(cx);
        cx.notify();
        id
    }
    pub(super) fn close_browser(
        &mut self,
        id: BrowserId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let restore_focus = self.browsers.active == Some(id);
        let record = self.browsers.records.remove(&id);
        self.browsers.runtime.close(id);
        self.browsers.panes.remove(&id);
        let panel = self.browsers.panels.borrow_mut().remove(&id);
        if let Some(panel) = panel {
            self.dock
                .update(cx, |dock, cx| dock.remove_panel(panel, window, cx));
        }
        if self.browsers.active == Some(id) {
            self.browsers.active = None;
        }
        if self.browsers.focused == Some(id) {
            self.browsers.focused = None;
        }
        if let Some(record) = record
            && let Some(thread) = record.thread
        {
            self.state.update(cx, |state, cx| {
                if record.artifact.is_none()
                    && state.thread_sidebar_tab(thread) == SidebarTab::Browser
                {
                    state.show_thread_sidebar(thread, SidebarTab::Changes);
                }
                cx.emit(PresentationChange::Thread(thread));
            });
        }
        self.save_browsers(cx);
        self.sync_thread_panels(window, cx);
        if restore_focus {
            let target = self
                .thread_target(cx)
                .unwrap_or_else(|| self.projects.clone());
            self.focus_panel(target, window, cx);
        }
        self.flush_layout(cx);
        cx.notify();
    }
    pub(super) fn browser_panel_removed(
        &mut self,
        id: BrowserId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rebuilding {
            return;
        }
        let current = self
            .browsers
            .records
            .get(&id)
            .is_some_and(|record| record.space == self.presented_space);
        if current
            && self
                .browsers
                .panels
                .borrow()
                .get(&id)
                .is_some_and(|panel| panel.read(cx).group.is_none())
        {
            self.close_browser(id, window, cx);
        }
    }
    pub(super) fn sync_browser_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panels: Vec<_> = self
            .browsers
            .panels
            .borrow()
            .iter()
            .map(|(id, panel)| (*id, panel.clone()))
            .collect();
        for (id, panel) in panels {
            let current = self
                .browsers
                .records
                .get(&id)
                .is_some_and(|record| record.space == self.presented_space);
            let attached = panel
                .read(cx)
                .group
                .as_ref()
                .and_then(WeakEntity::upgrade)
                .is_some();
            if current && !attached {
                self.dock.update(cx, |dock, cx| {
                    dock.add_panel_view(
                        panel_handle(panel),
                        DockPlacement::Center,
                        None,
                        window,
                        cx,
                    )
                });
            } else if !current && attached {
                self.dock
                    .update(cx, |dock, cx| dock.remove_panel(panel, window, cx));
            }
        }
        if self.browsers.active.is_some_and(|id| {
            self.browsers
                .records
                .get(&id)
                .is_none_or(|record| record.space != self.presented_space)
        }) {
            self.browsers.active = None;
        }
        if self.browsers.focused.is_some_and(|id| {
            self.browsers
                .records
                .get(&id)
                .is_none_or(|record| record.space != self.presented_space)
        }) {
            self.browsers.focused = None;
        }
    }
    pub(super) fn start_browser_tools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Receipts tell each artifact page what happened to its events.
        let receipts = self.state.read(cx).artifact_receipts();
        cx.spawn(async move |workspace, cx| {
            while let Ok(batch) = receipts.recv().await {
                let delivered = workspace.update(cx, |workspace, _| {
                    for receipt in batch {
                        workspace.browsers.runtime.receipt(
                            receipt.browser,
                            receipt.event,
                            receipt.status,
                            receipt.reason,
                        );
                    }
                });
                if delivered.is_err() {
                    break;
                }
            }
        })
        .detach();
        // Artifacts follow the theme in effect, including documents already shown.
        self._subscriptions
            .push(
                cx.observe_global::<gpui_kit::component::Theme>(|workspace, cx| {
                    let theme = crate::ui::theme::artifact_tokens(cx);
                    if theme == workspace.browsers.artifact_theme {
                        return;
                    }
                    workspace.browsers.artifact_theme = theme.clone();
                    for (&id, record) in &workspace.browsers.records {
                        if record.artifact.is_some() {
                            workspace
                                .browsers
                                .runtime
                                .command(id, BrowserCommand::ArtifactTheme { css: theme.clone() });
                        }
                    }
                }),
            );
        let Some(server) = self.state.read(cx).browser_tools.clone() else {
            return;
        };
        let requests = server.requests();
        cx.spawn_in(window, async move |workspace, cx| {
            while let Ok(request) = requests.recv().await {
                let _ = workspace.update_in(cx, |workspace, window, cx| {
                    workspace.browser_tool(request, window, cx)
                });
            }
        })
        .detach();
    }
    fn browser_tool(
        &mut self,
        request: BrowserToolRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let BrowserToolRequest {
            thread,
            operation,
            reply,
        } = request;
        if self.state.read(cx).shutting_down {
            let _ = reply.send(Err("Application is shutting down".into()));
            return;
        }
        if !self
            .state
            .read(cx)
            .snapshot
            .threads
            .iter()
            .any(|candidate| candidate.id == thread)
        {
            let _ = reply.send(Err("Conversation no longer exists".into()));
            return;
        }
        let result: Result<Value, String> = match operation {
            BrowserToolOperation::Network { token, action } => {
                self.state.update(cx, |state, _| {
                    state.network_tool(thread, token, action, reply)
                });
                return;
            }
            BrowserToolOperation::ShowArtifact {
                path,
                title,
                placement,
            } => match url::Url::from_file_path(&path) {
                Ok(url) => {
                    let id = self.open_browser(
                        url.to_string(),
                        Some(thread),
                        Some(path.clone()),
                        window,
                        cx,
                    );
                    if let Some(record) = self.browsers.records.get_mut(&id) {
                        record.title = title.clone().unwrap_or_else(|| "Artifact".into());
                    }
                    if let Some(pane) = self.browsers.panes.get(&id) {
                        pane.update(cx, |pane, cx| {
                            pane.set_artifact(
                                title.clone().unwrap_or_else(|| "HTML artifact".into()),
                            );
                            cx.notify();
                        });
                    }
                    self.place_artifact(thread, id, placement, cx);
                    self.save_browsers(cx);
                    self.state
                        .update(cx, |_, cx| cx.emit(PresentationChange::Thread(thread)));
                    Ok(
                        json!({"id":id,"path":path,"title":title,"placement":placement,"loading":true}),
                    )
                }
                Err(_) => Err("Artifact path is not an absolute file path".into()),
            },
            // The remote client writes a local copy and submits it as ShowArtifact.
            BrowserToolOperation::ShowRemoteArtifact { .. } => {
                Err("Remote artifacts are shown through their remote host connection".into())
            }
            BrowserToolOperation::Open { url, placement } => {
                match super::super::browser::navigation_url(&url) {
                    Ok(url) => {
                        let id = self.open_browser(
                            url,
                            (placement == "sidebar").then_some(thread),
                            None,
                            window,
                            cx,
                        );
                        Ok(json!({"id":id,"loading":true}))
                    }
                    Err(error) => Err(error),
                }
            }
            BrowserToolOperation::Close { id } => {
                self.close_browser(id, window, cx);
                Ok(json!({"closed":id}))
            }
            BrowserToolOperation::Runtime { id, operation } => {
                let runtime = self.browsers.runtime.clone();
                smol::spawn(async move {
                    let _ = reply.send(operation.execute(runtime, id).await);
                })
                .detach();
                return;
            }
            BrowserToolOperation::Viewport { id, size } => match self.browsers.panes.get(&id) {
                None => Err("Browser session does not exist".into()),
                Some(pane) => {
                    match pane.update(cx, |pane, cx| {
                        pane.set_viewport(size, cx)?;
                        Ok::<_, String>(pane.viewport_dimensions())
                    }) {
                        Err(error) => Err(error),
                        Ok(dimensions) => {
                            let runtime = self.browsers.runtime.clone();
                            smol::spawn(async move {
                                let _ = reply.send(
                                    BrowserAutomation::viewport_result(
                                        runtime,
                                        id,
                                        dimensions,
                                        size.is_some(),
                                    )
                                    .await,
                                );
                            })
                            .detach();
                            return;
                        }
                    }
                }
            },
        };
        let _ = reply.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::{ArtifactPlacement, BrowserRecord, placement_moves, single_dock};
    use uuid::Uuid;

    fn record(thread: Option<Uuid>, artifact: bool, placement: ArtifactPlacement) -> BrowserRecord {
        BrowserRecord {
            id: Uuid::new_v4(),
            url: "file:///tmp/a.html".into(),
            title: "A".into(),
            space: Uuid::nil(),
            thread,
            artifact: artifact.then(|| "/tmp/a.html".into()),
            placement,
        }
    }

    #[test]
    fn docking_an_artifact_moves_the_previous_one_into_the_transcript_only_in_its_thread() {
        let (thread, other) = (Uuid::new_v4(), Uuid::new_v4());
        let docked = record(Some(thread), true, ArtifactPlacement::Composer);
        let inline = record(Some(thread), true, ArtifactPlacement::Conversation);
        let elsewhere = record(Some(other), true, ArtifactPlacement::Composer);
        let records = [&docked, &inline, &elsewhere];
        assert_eq!(
            placement_moves(
                records.into_iter(),
                thread,
                inline.id,
                ArtifactPlacement::Composer
            ),
            [
                (docked.id, ArtifactPlacement::Conversation),
                (inline.id, ArtifactPlacement::Composer)
            ]
        );
        assert_eq!(
            placement_moves(
                records.into_iter(),
                thread,
                docked.id,
                ArtifactPlacement::Conversation
            ),
            [(docked.id, ArtifactPlacement::Conversation)],
            "moving the dock into the transcript leaves other artifacts alone"
        );
        assert!(
            placement_moves(
                records.into_iter(),
                thread,
                docked.id,
                ArtifactPlacement::Composer
            )
            .is_empty(),
            "re-docking the docked artifact changes nothing"
        );
    }

    #[test]
    fn restored_layouts_keep_one_docked_artifact_per_thread() {
        let thread = Uuid::new_v4();
        let mut records = vec![
            record(Some(thread), true, ArtifactPlacement::Composer),
            record(Some(thread), true, ArtifactPlacement::Composer),
            record(None, true, ArtifactPlacement::Composer),
            record(Some(thread), false, ArtifactPlacement::Composer),
        ];
        single_dock(&mut records);
        let placements: Vec<_> = records.iter().map(|record| record.placement).collect();
        assert_eq!(
            placements,
            [
                ArtifactPlacement::Composer,
                ArtifactPlacement::Conversation,
                ArtifactPlacement::Conversation,
                ArtifactPlacement::Conversation,
            ]
        );
        let legacy: BrowserRecord = serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(), "url": "file:///tmp/a.html", "title": "A",
            "space": Uuid::nil(), "thread": thread, "artifact": "/tmp/a.html"
        }))
        .unwrap();
        assert_eq!(legacy.placement, ArtifactPlacement::Conversation);
    }
}
