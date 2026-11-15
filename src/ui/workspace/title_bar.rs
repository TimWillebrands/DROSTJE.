use crate::app::{AppState, PresentationChange};
use crate::ui::{
    commands::{
        AddProject, Command, NewConversation, NewFactoryThread, OpenPalette, ResetLayout, Settings,
        ShowArchived, ToggleOutput, ToggleProjects,
    },
    workspace::{PanelKind, Workspace},
};
use gpui_kit::{
    component::{
        ActiveTheme, IconName, Sizable, TitleBar,
        button::{Button, ButtonVariants},
        dock::{
            DockAreaRenderer, DockContext, DockPlacement, DockSkin, NodeId, PanelState,
            TabGroupContext, TabGroupRenderer,
        },
        h_flex,
        menu::DropdownMenu,
        tag::Tag,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{rc::Rc, sync::Arc};

pub(in crate::ui) struct SaveStatus {
    state: Entity<AppState>,
    presentation: (usize, bool, bool),
    _subscriptions: Vec<Subscription>,
}

impl SaveStatus {
    pub(in crate::ui) fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let presentation = Self::presentation(state.read(cx));
        let subscriptions = vec![
            cx.observe(&state, |this, _, cx| this.refresh(cx)),
            cx.subscribe(&state, |this, _, _: &PresentationChange, cx| {
                this.refresh(cx)
            }),
            cx.observe_global::<gpui_kit::component::Theme>(|_, cx| cx.notify()),
        ];
        Self {
            state,
            presentation,
            _subscriptions: subscriptions,
        }
    }

    fn presentation(state: &AppState) -> (usize, bool, bool) {
        (
            state.active_threads.len(),
            state.pending_writes > 0,
            state.storage_error.is_some() || state.shutdown_error.is_some(),
        )
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let next = Self::presentation(self.state.read(cx));
        if next != self.presentation {
            self.presentation = next;
            cx.notify();
        }
    }
}

impl Render for SaveStatus {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (running, saving, unsaved) = self.presentation;
        h_flex()
            .gap_2()
            .when(running > 0, |view| {
                view.child(
                    div()
                        .id("running-turns")
                        .debug_selector(|| "running-turns".into())
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(match running {
                            1 => "1 turn running".to_owned(),
                            n => format!("{n} turns running"),
                        }),
                )
            })
            .when(saving, |view| {
                view.child(
                    div()
                        .id("save-indicator")
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Saving changes…"),
                )
            })
            .when(unsaved, |view| {
                view.child(Tag::danger().child("Unsaved changes"))
            })
    }
}

#[derive(Clone, PartialEq)]
struct TitlePresentation {
    project_name: SharedString,
    saved_recovery: bool,
    storage_error: Option<String>,
    shutdown_error: bool,
}

impl TitlePresentation {
    fn of(state: &AppState) -> Self {
        Self {
            project_name: state
                .project()
                .map(|project| project.name.clone())
                .unwrap_or_else(|| "DROSTJE.".into())
                .into(),
            saved_recovery: state
                .snapshot
                .settings
                .get("recovered_composition")
                .is_some_and(|value| !value.is_null()),
            storage_error: state.storage_error.clone(),
            shutdown_error: state.shutdown_error.is_some(),
        }
    }
}

pub(in crate::ui) struct WorkspaceTitle {
    state: Entity<AppState>,
    owner: WeakEntity<Workspace>,
    save_status: Entity<SaveStatus>,
    presentation: TitlePresentation,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceTitle {
    pub(in crate::ui) fn new(
        state: Entity<AppState>,
        owner: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let save_status = cx.new(|cx| SaveStatus::new(state.clone(), cx));
        let presentation = TitlePresentation::of(state.read(cx));
        let subscriptions = vec![
            cx.observe(&state, |this, _, cx| this.refresh(cx)),
            cx.subscribe(&state, |this, _, _: &PresentationChange, cx| {
                this.refresh(cx)
            }),
        ];
        Self {
            state,
            owner,
            save_status,
            presentation,
            _subscriptions: subscriptions,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let next = TitlePresentation::of(self.state.read(cx));
        if next != self.presentation {
            self.presentation = next;
            cx.notify();
        }
    }
}

impl Render for WorkspaceTitle {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let presentation = &self.presentation;
        let close_owner = self.owner.clone();
        let palette_owner = self.owner.clone();
        let recovery_owner = self.owner.clone();
        let dismiss_recovery = self.owner.clone();
        let left_dock_open = self.owner.upgrade().is_some_and(|workspace| {
            workspace
                .read(cx)
                .dock
                .read(cx)
                .is_dock_open(DockPlacement::Left)
        });
        v_flex()
            .w_full()
            .flex_shrink_0()
            .child(
                TitleBar::new()
                    .relative()
                    .children(crate::ui::theme::grain::layer(crate::ui::theme::grain(cx)))
                    .when(left_dock_open, |bar| bar.pl_3())
                    .on_close_window(move |_, window, cx| {
                        let _ = close_owner
                            .update(cx, |workspace, cx| workspace.quit_review(window, cx));
                    })
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                Button::new("workspace-menu")
                                    .small()
                                    .ghost()
                                    .cursor_default()
                                    .icon(IconName::Menu)
                                    .tooltip("Workspace menu")
                                    .dropdown_menu(|menu, _, _| {
                                        menu.menu("Add project…", Box::new(AddProject))
                                            .menu("New conversation", Box::new(NewConversation))
                                            .menu("New Factory thread", Box::new(NewFactoryThread))
                                            .menu(
                                                Command::NewTerminal.label(),
                                                Command::NewTerminal.action(),
                                            )
                                            .separator()
                                            .menu("Toggle projects", Box::new(ToggleProjects))
                                            .menu("Toggle output", Box::new(ToggleOutput))
                                            .menu("Show archived", Box::new(ShowArchived))
                                            .separator()
                                            .menu("Settings…", Box::new(Settings))
                                            .menu(
                                                Command::CustomizeTheme.label(),
                                                Command::CustomizeTheme.action(),
                                            )
                                            .menu("Reset layout", Box::new(ResetLayout))
                                    }),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .px_1()
                                    .text_sm()
                                    .text_ellipsis()
                                    .child(presentation.project_name.clone()),
                            )
                            .child(self.save_status.clone()),
                    )
                    .child(
                        h_flex().pr_1().child(
                            Button::new("workspace-palette")
                                .small()
                                .ghost()
                                .cursor_default()
                                .icon(IconName::Search)
                                .tooltip_with_action(
                                    "Search commands, projects and threads",
                                    &OpenPalette,
                                    Some("Workspace"),
                                )
                                .on_click(move |_, window, cx| {
                                    let _ = palette_owner.update(cx, |workspace, cx| {
                                        workspace.open_palette(window, cx)
                                    });
                                }),
                        ),
                    ),
            )
            .when_some(presentation.storage_error.clone(), |view, error| {
                view.child(div().px_3().py_2().text_color(cx.theme().danger).child(format!(
                    "Storage failure — new work is disabled. In-memory drafts retained. {error}"
                )))
            })
            .when(
                presentation.saved_recovery
                    || presentation.storage_error.is_some()
                    || presentation.shutdown_error,
                |view| {
                    view.child(
                        h_flex()
                            .px_3()
                            .py_1()
                            .gap_2()
                            .child("Draft recovery remains separate from submitted text.")
                            .child(
                                Button::new("copy-draft-recovery")
                                    .small()
                                    .ghost()
                                    .label("Copy Draft Recovery")
                                    .on_click(move |_, _, cx| {
                                        let _ = recovery_owner.update(cx, |workspace, cx| {
                                            workspace.copy_draft_recovery(cx)
                                        });
                                    }),
                            )
                            .when(presentation.saved_recovery, |view| {
                                view.child(
                                    Button::new("dismiss-saved-recovery")
                                        .small()
                                        .ghost()
                                        .label("Dismiss Saved Preedit")
                                        .on_click(move |_, _, cx| {
                                            let _ = dismiss_recovery.update(cx, |workspace, cx| {
                                                workspace.state.update(cx, |state, cx| {
                                                    state.set_setting(
                                                        "recovered_composition",
                                                        serde_json::Value::Null,
                                                    );
                                                    cx.notify();
                                                })
                                            });
                                        }),
                                )
                            }),
                    )
                },
            )
    }
}

pub(in crate::ui) struct WorkspaceDockSkin {
    pub(in crate::ui) inner: Rc<DockSkin>,
    pub(in crate::ui) title: Entity<WorkspaceTitle>,
}

impl DockAreaRenderer for WorkspaceDockSkin {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.frame(window, cx)
    }

    fn split_frame(
        &self,
        node: NodeId,
        axis: Axis,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.inner.split_frame(node, axis, window, cx)
    }

    fn center_frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner
            .center_frame(window, cx)
            .child(self.title.clone())
    }

    fn render_split_handle(
        &self,
        handle: &gpui_kit::base::ResizeHandleContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_split_handle(handle, window, cx)
    }

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.inner.render_dock(dock, content, window, cx)
    }

    fn build_placeholder(
        &self,
        state: &PanelState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn gpui_kit::component::dock::BasePanelView>> {
        self.inner.build_placeholder(state, window, cx)
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(WorkspaceTabGroups {
            inner: self.inner.tab_group_renderer(),
            title: self.title.clone(),
        })
    }
}

struct WorkspaceTabGroups {
    inner: Rc<dyn TabGroupRenderer>,
    title: Entity<WorkspaceTitle>,
}

impl TabGroupRenderer for WorkspaceTabGroups {
    fn frame(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.frame(group, window, cx)
    }

    fn content_frame(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.inner.content_frame(group, window, cx)
    }

    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        if group.is_zoomed() {
            return v_flex()
                .w_full()
                .child(self.title.clone())
                .child(self.inner.render_tab_bar(group, window, cx))
                .into_any_element();
        }
        let mut visible = group.panels().iter().filter(|panel| panel.visible(cx));
        let projects_alone = visible
            .next()
            .is_some_and(|panel| panel.panel_name(cx) == PanelKind::Projects.name())
            && visible.next().is_none();
        if projects_alone {
            return Empty.into_any_element();
        }
        self.inner.render_tab_bar(group, window, cx)
    }

    fn render_active_panel(
        &self,
        panel: AnyView,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.inner.render_active_panel(panel, group, window, cx)
    }

    fn render_drop_indicator(
        &self,
        indicator: gpui_kit::component::dock::DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_drop_indicator(indicator, window, cx)
    }

    fn render_empty(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_empty(group, window, cx)
    }
}
