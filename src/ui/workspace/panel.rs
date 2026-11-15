//! `WorkspacePanel`: one dock leaf (Projects, Welcome, a thread, terminal or
//! browser tab), its tab title, menu and content.

use super::*;

pub struct WorkspacePanel {
    pub(super) kind: PanelKind,
    pub(super) state: Entity<AppState>,
    pub(super) workspace: WeakEntity<Workspace>,
    pub(super) focus: FocusHandle,
    pub(super) scroll: ScrollHandle,
    /// The sidebar Output tab scrolls on its own, beside the thread's main scroll.
    pub(super) output_scroll: ScrollHandle,
    pub(super) group: Option<WeakEntity<TabGroup>>,
    pub(super) bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub(super) transcript: Option<Entity<conversation::TranscriptView>>,
    pub(super) network: Option<Entity<crate::ui::network::NetworkPanel>>,
    pub(super) terminal: Option<Entity<TerminalPane>>,
    pub(super) browser: Option<Entity<BrowserPane>>,
    /// The Projects panel's prepared rows; empty for every other kind.
    pub(super) rows: projects::RowsCache,
    pub(super) _subscriptions: Vec<Subscription>,
}
impl WorkspacePanel {
    pub(super) fn new(
        kind: PanelKind,
        state: Entity<AppState>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let mut subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.observe_global::<gpui_kit::component::Theme>(|_, cx| cx.notify()),
            cx.on_focus_in(&focus, window, |_, _, cx| cx.notify()),
            cx.on_focus_out(&focus, window, |_, _, _, cx| cx.notify()),
            // Moving between the Projects list and its native search input stays
            // inside the pane, but must still move the row's keyboard focus cue.
            cx.on_focus(&focus, window, |_, _, cx| cx.notify()),
            cx.on_blur(&focus, window, |_, _, cx| cx.notify()),
            cx.observe_window_activation(window, |_, _, cx| cx.notify()),
            cx.subscribe(&state, |panel, _, event: &PresentationChange, cx| {
                let affected = match (*event, panel.kind) {
                    (
                        PresentationChange::Draft(id)
                        | PresentationChange::Thread(id)
                        | PresentationChange::Review(id),
                        PanelKind::Thread(thread),
                    ) => id == thread,
                    (PresentationChange::Diff(key), PanelKind::Thread(thread)) => {
                        let state = panel.state.read(cx);
                        state
                            .snapshot
                            .threads
                            .iter()
                            .find(|candidate| candidate.id == thread)
                            .and_then(|candidate| state.working_tree_of(candidate))
                            .is_some_and(|(tree, _)| tree == key)
                    }
                    (
                        PresentationChange::Projects | PresentationChange::Selection,
                        PanelKind::Projects,
                    ) => true,
                    _ => false,
                };
                if affected {
                    cx.notify();
                }
            }),
        ];
        let transcript =
            match kind {
                PanelKind::Thread(id)
                    if state.read(cx).snapshot.threads.iter().any(|thread| {
                        thread.id == id && thread.kind == ThreadKind::Conversation
                    }) =>
                {
                    Some(cx.new(|cx| {
                        conversation::TranscriptView::new(id, state.clone(), workspace.clone(), cx)
                    }))
                }
                _ => None,
            };
        let network = match (kind, transcript.is_some()) {
            (PanelKind::Thread(id), true) => Some(cx.new(|cx| {
                crate::ui::network::NetworkPanel::new(
                    id,
                    state.clone(),
                    workspace.clone(),
                    window,
                    cx,
                )
            })),
            _ => None,
        };
        if let PanelKind::Thread(id) = kind {
            // Focus, however it arrived (click, tab, shortcut), selects a thread.
            let owner = workspace.clone();
            let entity_id = cx.entity().entity_id();
            subscriptions.push(cx.on_focus_in(&focus, window, move |_, window, cx| {
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.thread_focused(id, entity_id, window, cx)
                });
            }));
        }
        if let PanelKind::Terminal(id) = kind {
            let owner = workspace.clone();
            subscriptions.push(cx.on_focus_in(&focus, window, move |_, window, cx| {
                let owner = owner.clone();
                window.defer(cx, move |window, cx| {
                    let _ = owner.update(cx, |workspace, cx| {
                        if workspace
                            .focused_panel(window, cx)
                            .is_some_and(|panel| panel.read(cx).kind == PanelKind::Terminal(id))
                        {
                            workspace.terminal_focused(id, window, cx);
                            if let Some(pane) = workspace.active_terminal(cx) {
                                pane.update(cx, |pane, cx| pane.focus(window, cx));
                            }
                        }
                    });
                });
            }));
        }
        if let PanelKind::Browser(id) = kind {
            let owner = workspace.clone();
            subscriptions.push(cx.on_focus_in(&focus, window, move |_, _, cx| {
                let owner = owner.clone();
                cx.defer(move |cx| {
                    let _ = owner.update(cx, |workspace, cx| workspace.browser_focused(id, cx));
                });
            }));
        }
        Self {
            kind,
            state,
            workspace,
            focus,
            scroll: ScrollHandle::new(),
            output_scroll: ScrollHandle::new(),
            group: None,
            bounds: Rc::new(Cell::new(None)),
            transcript,
            network,
            terminal: None,
            browser: None,
            rows: Default::default(),
            _subscriptions: subscriptions,
        }
    }
    /// Give a Terminal panel its pane. A Neovim pane that exits closes its tab.
    pub(super) fn attach_terminal(
        &mut self,
        id: TerminalId,
        pane: Entity<TerminalPane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._subscriptions.push(cx.subscribe_in(
            &pane,
            window,
            move |panel, _, _: &NeovimExited, window, cx| {
                let owner = panel.workspace.clone();
                // The pane's observer runs inside its own update; close afterwards.
                window.defer(cx, move |window, cx| {
                    let _ =
                        owner.update(cx, |workspace, cx| workspace.neovim_exited(id, window, cx));
                });
            },
        ));
        self.terminal = Some(pane);
    }
    pub(super) fn panel_title(&self, cx: &App) -> String {
        match self.kind {
            PanelKind::Projects => "Projects".into(),
            PanelKind::Welcome => "Threads".into(),
            PanelKind::Browser(_) => self
                .browser
                .as_ref()
                .map(|pane| pane.read(cx).title.clone())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| "Browser".into()),
            PanelKind::Terminal(_) => {
                let (project, worktree, neovim) = self
                    .terminal
                    .as_ref()
                    .map(|pane| {
                        let pane = pane.read(cx);
                        (Some(pane.project_id), pane.worktree, pane.neovim_target())
                    })
                    .unwrap_or((None, None, None));
                if let Some(name) = neovim
                    .and_then(|target| target.file.as_ref())
                    .and_then(|file| file.file_name())
                {
                    return format!("Neovim — {}", name.to_string_lossy());
                }
                let kind = if neovim.is_some() {
                    "Neovim"
                } else {
                    "Terminal"
                };
                let state = self.state.read(cx);
                let name = state
                    .snapshot
                    .projects
                    .iter()
                    .find(|candidate| Some(candidate.id) == project)
                    .map(|project| project.name.as_str())
                    .unwrap_or("Project");
                match worktree.and_then(|id| state.worktree(id)) {
                    Some(worktree) => format!("{kind} — {name} · {}", worktree.codename),
                    None => format!("{kind} — {name}"),
                }
            }
            PanelKind::Thread(id) => self
                .state
                .read(cx)
                .snapshot
                .threads
                .iter()
                .find(|thread| thread.id == id)
                .map(|thread| thread.title.clone())
                .unwrap_or_else(|| "Thread".into()),
        }
    }
}
impl BasePanel for WorkspacePanel {
    fn panel_name(&self) -> &'static str {
        self.kind.name()
    }
    fn closable(&self, cx: &App) -> bool {
        match self.kind {
            PanelKind::Thread(id) => !self
                .workspace
                .upgrade()
                .and_then(|workspace| workspace.read(cx).sidebar_terminal(id))
                .is_some_and(|pane| pane.read(cx).is_alive(cx)),
            PanelKind::Browser(_) => true,
            _ => false,
        }
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = &self.terminal {
            terminal.update(cx, |terminal, cx| terminal.set_visible(false, cx));
            let owner = self.workspace.clone();
            // Do not re-lease this panel: visibility reads every live dock leaf.
            window.defer(cx, move |window, cx| {
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.sync_terminal_visibility(window, cx)
                });
            });
        }
        if !active {
            self.bounds.set(None);
        }
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
        self.bounds.set(None);
    }
    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.group = None;
        self.bounds.set(None);
        if let Some(terminal) = &self.terminal {
            terminal.update(cx, |terminal, cx| terminal.set_visible(false, cx));
        }
        if let PanelKind::Thread(id) = self.kind {
            // The dock often fires this from inside a `Workspace` update
            // (`sync_thread_panels`, `reset_layout`), and updating an entity that
            // is already leased panics. Report after that update has finished;
            // a panel the dock has since re-added is not a close.
            cx.defer_in(window, move |panel, window, cx| {
                if panel.group.is_some() {
                    return;
                }
                let removed = cx.entity();
                let _ = panel.workspace.update(cx, |workspace, cx| {
                    workspace.thread_panel_removed(id, &removed, window, cx)
                });
            });
        }
        if let PanelKind::Browser(id) = self.kind {
            cx.defer_in(window, move |panel, window, cx| {
                if panel.group.is_none() {
                    let _ = panel.workspace.update(cx, |workspace, cx| {
                        workspace.browser_panel_removed(id, window, cx)
                    });
                }
            });
        }
    }
    fn dump(&self, cx: &App) -> PanelState {
        let mut state = PanelState::new(self.kind.name());
        if let PanelKind::Thread(id) = self.kind {
            state.info = PanelInfo::panel(json!({ "thread": id }));
        }
        if let PanelKind::Terminal(id) = self.kind {
            // Enough to reopen the tab after a restart; see `TerminalTab`.
            let pane = self.terminal.as_ref().map(|pane| pane.read(cx));
            state.info = PanelInfo::panel(json!({
                "terminal": id,
                "project": pane.map(|pane| pane.project_id),
                "worktree": pane.and_then(|pane| pane.worktree),
                "neovim": pane.and_then(|pane| pane.neovim_target()),
            }));
        }
        if let PanelKind::Browser(id) = self.kind {
            state.info = PanelInfo::panel(
                json!({ "browser": id, "url": self.browser.as_ref().map(|pane| pane.read(cx).url.clone()) }),
            );
        }
        state
    }
}
impl Panel for WorkspacePanel {
    /// A thread tab shows its activity mark, with a sweeping underline while its agent
    /// works, and a close control on hover; a middle click closes it too. Closing only
    /// closes the tab.
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.panel_title(cx);
        if let PanelKind::Browser(id) = self.kind {
            let owner = self.workspace.clone();
            let panel = cx.entity().downgrade();
            return h_flex()
                .id(SharedString::from(format!("browser-tab-{id}")))
                .items_center()
                .gap_1()
                .min_w_0()
                .child(title)
                .child(
                    Button::new(SharedString::from(format!("close-browser-{id}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tab_stop(false)
                        .tooltip_with_action(Command::CloseThread.label(), &CloseThread, None)
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let Some(panel) = panel.upgrade() else {
                                return;
                            };
                            let _ = owner.update(cx, |workspace, cx| {
                                workspace.focus_panel(panel, window, cx);
                            });
                            window.dispatch_action(Command::CloseThread.action(), cx);
                        }),
                )
                .into_any_element();
        }
        if let PanelKind::Terminal(id) = self.kind {
            let close = self.workspace.clone();
            let middle = close.clone();
            return h_flex()
                .gap_1()
                .items_center()
                .min_w_0()
                .id(SharedString::from(format!("terminal-tab-{id}")))
                .on_aux_click(move |event, window, cx| {
                    if event.is_middle_click() {
                        cx.stop_propagation();
                        let _ = middle.update(cx, |workspace, cx| {
                            workspace.close_terminal_tab(id, window, cx)
                        });
                    }
                })
                .child(title)
                .child(
                    Button::new(SharedString::from(format!("close-terminal-{id}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Close tab")
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let _ = close.update(cx, |workspace, cx| {
                                workspace.close_terminal_tab(id, window, cx)
                            });
                        }),
                )
                .into_any_element();
        }
        let PanelKind::Thread(id) = self.kind else {
            return title.into_any_element();
        };
        let state = self.state.read(cx);
        let activity = state.thread_activity(id);
        let titling = state.is_titling(id);
        let project = state
            .snapshot
            .threads
            .iter()
            .find(|thread| thread.id == id)
            .and_then(|thread| {
                state
                    .snapshot
                    .projects
                    .iter()
                    .find(|project| project.id == thread.project_id)
            });
        let group = SharedString::from(format!("thread-tab-{id}"));
        let (middle, close) = (self.workspace.clone(), self.workspace.clone());
        let rename_input = self.workspace.upgrade().and_then(|workspace| {
            workspace
                .read(cx)
                .inline_rename()
                .and_then(|(target, surface, input, error)| {
                    (target == RenameTarget::Thread(id) && surface == RenameSurface::Tab)
                        .then_some((input, error))
                })
        });
        let selector = group.clone();
        h_flex()
            .id(group.clone())
            .debug_selector(move || selector.to_string())
            .group(group.clone())
            .relative()
            .items_center()
            .gap_1()
            .min_w_0()
            .on_aux_click(move |event, window, cx| {
                if event.is_middle_click() {
                    cx.stop_propagation();
                    let _ = middle.update(cx, |workspace, cx| {
                        workspace.close_thread_tab(id, window, cx)
                    });
                }
            })
            .children(activity::mark(
                SharedString::from(format!("thread-tab-activity-{id}")),
                activity,
                cx,
            ))
            .when_some(project, |tab, project| {
                tab.child(
                    div()
                        .id(SharedString::from(format!("thread-tab-project-{id}")))
                        .debug_selector(move || format!("thread-tab-project-{id}"))
                        .max_w(rems(12.))
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{} ·", project.name)),
                )
            })
            .map(|tab| match rename_input {
                Some((input, error)) => {
                    tab.child(projects::inline_editor(&input, error, &self.workspace, cx))
                }
                None if titling => tab.child(ShimmerText::new(title)),
                None => tab.child(title),
            })
            .child(
                div()
                    .invisible()
                    .group_hover(group, |style| style.visible())
                    .child(
                        Button::new(SharedString::from(format!("close-thread-tab-{id}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Close tab")
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                let _ = close.update(cx, |workspace, cx| {
                                    workspace.close_thread_tab(id, window, cx)
                                });
                            }),
                    ),
            )
            // Inside the tab's clipped inner box, just below the label.
            .when(activity == ThreadActivity::Running, |tab| {
                let selector = format!("thread-tab-sweep-{id}");
                tab.child(
                    div()
                        .debug_selector(move || selector)
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(rems(-0.25))
                        .child(activity::sweep(cx)),
                )
            })
            .into_any_element()
    }
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        // The dock's closable flag adds a menu item, not a tab close button.
        // Content tabs supply their own title controls instead of a plain name.
        match self.kind {
            PanelKind::Thread(_) | PanelKind::Terminal(_) | PanelKind::Browser(_) => None,
            _ => Some(self.panel_title(cx).into()),
        }
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let state = self.state.read(cx);
        match self.kind {
            PanelKind::Terminal(id) => {
                let owned = |command: Command| {
                    let owner = self.workspace.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.terminal_focused(id, window, cx);
                            workspace.execute(command, window, cx);
                        });
                    }
                };
                menu.item(
                    PopupMenuItem::new(Command::NewTerminal.label())
                        .on_click(owned(Command::NewTerminal)),
                )
                .item(
                    PopupMenuItem::new(Command::NewNeovim.label())
                        .on_click(owned(Command::NewNeovim)),
                )
                .item(
                    PopupMenuItem::new(
                        if self
                            .terminal
                            .as_ref()
                            .is_some_and(|pane| pane.read(cx).neovim_target().is_some())
                        {
                            RESTART_NEOVIM_LABEL
                        } else {
                            Command::RestartTerminal.label()
                        },
                    )
                    .on_click(owned(Command::RestartTerminal)),
                )
                .item(
                    PopupMenuItem::new(Command::CloseThread.label())
                        .on_click(owned(Command::CloseThread)),
                )
            }
            PanelKind::Thread(id) => {
                // Rename, archive and close act on this tab's thread without selecting it.
                let targeted =
                    |run: fn(&mut Workspace, ThreadId, &mut Window, &mut Context<Workspace>)| {
                        let owner = self.workspace.clone();
                        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                            let _ = owner.update(cx, |workspace, cx| {
                                if workspace.state.read(cx).thread_in_active_space(id) {
                                    run(workspace, id, window, cx);
                                }
                            });
                        }
                    };
                let archived = state
                    .snapshot
                    .threads
                    .iter()
                    .any(|thread| thread.id == id && thread.archived);
                let details = self.workspace.clone();
                menu.item(
                    PopupMenuItem::new(Command::RenameThread.label())
                        .action(Command::RenameThread.action())
                        .disabled(state.rename_thread_reason_for(id).is_some())
                        .on_click(targeted(|workspace, id, window, cx| {
                            workspace.start_rename(
                                RenameTarget::Thread(id),
                                RenameSurface::Tab,
                                window,
                                cx,
                            )
                        })),
                )
                .map(|menu| {
                    if archived {
                        menu.item(
                            PopupMenuItem::new(Command::UnarchiveThread.label())
                                .action(Command::UnarchiveThread.action())
                                .disabled(state.unarchive_thread_reason_for(id).is_some())
                                .on_click(targeted(|workspace, id, _, cx| {
                                    workspace.unarchive_thread(id, cx)
                                })),
                        )
                    } else {
                        menu.item(
                            PopupMenuItem::new(Command::ArchiveThread.label())
                                .action(Command::ArchiveThread.action())
                                .disabled(state.archive_thread_reason_for(id).is_some())
                                .on_click(targeted(|workspace, id, window, cx| {
                                    workspace.archive_thread(id, window, cx)
                                })),
                        )
                    }
                })
                .item(
                    PopupMenuItem::new(Command::CloseThread.label())
                        .action(Command::CloseThread.action())
                        .on_click(targeted(|workspace, id, window, cx| {
                            workspace.close_thread_tab(id, window, cx)
                        })),
                )
                .item(
                    PopupMenuItem::new("Thread details").on_click(move |_, window, cx| {
                        let _ = details.update(cx, |workspace, cx| {
                            if !workspace.state.read(cx).thread_in_active_space(id) {
                                return;
                            }
                            workspace.toggle_thread_details(id, window, cx)
                        });
                    }),
                )
                .separator()
                .menu(Command::SwitchBranch.label(), Box::new(SwitchBranch))
                .menu(Command::CommitChanges.label(), Box::new(CommitChanges))
                .separator()
                .menu("New conversation", Box::new(NewConversation))
                .menu(
                    Command::NewConversationInWorktree.label(),
                    Box::new(NewConversationInWorktree),
                )
                .menu("New Factory thread", Box::new(NewFactoryThread))
            }
            PanelKind::Browser(_) => menu
                .menu(Command::NewBrowser.label(), Box::new(NewBrowser))
                .menu(Command::BrowserAddress.label(), Box::new(BrowserAddress))
                .menu(Command::BrowserReload.label(), Box::new(BrowserReload))
                .menu(Command::CloseThread.label(), Box::new(CloseThread)),
            PanelKind::Welcome => menu
                .menu("New conversation", Box::new(NewConversation))
                .menu(
                    Command::NewConversationInWorktree.label(),
                    Box::new(NewConversationInWorktree),
                )
                .menu("New Factory thread", Box::new(NewFactoryThread)),
            PanelKind::Projects => menu
                .menu("Add project…", Box::new(AddProject))
                .menu("Show archived", Box::new(ShowArchived)),
        }
    }
}
impl Focusable for WorkspacePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl EventEmitter<PanelEvent> for WorkspacePanel {}
impl Render for WorkspacePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pane_focused = window.is_window_active() && self.focus.contains_focused(window, cx);
        let list_focused = window.is_window_active() && self.focus.is_focused(window);
        let content = self.workspace.upgrade().map(|owner| {
            let workspace = owner.read(cx);
            let state = self.state.read(cx);
            let scroll = &self.scroll;
            match self.kind {
                PanelKind::Projects => {
                    let (slide, animating) = if cx.reduce_motion() {
                        (0., false)
                    } else {
                        workspace.space_slide.sample(Instant::now())
                    };
                    if animating {
                        window.request_animation_frame();
                    }
                    projects::render(
                        state,
                        &self.workspace,
                        projects::SidebarPresentation {
                            cursor: workspace.cursor.filter(|_| list_focused),
                            hovered: workspace.hovered_sidebar_row,
                            slide,
                            neighbor: workspace.space_slide.neighbor,
                            neighbor_query: &workspace.space_slide.neighbor_query,
                            neighbor_scroll: workspace.space_slide.neighbor_scroll.as_ref(),
                            revision: workspace.state_revision,
                        },
                        &workspace.thread_search,
                        scroll,
                        &mut self.rows,
                        cx,
                    )
                }
                PanelKind::Welcome => conversation::welcome(state, &self.workspace, cx),
                PanelKind::Browser(_) => self
                    .browser
                    .as_ref()
                    .map(|pane| pane.clone().into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
                PanelKind::Terminal(_) => self
                    .terminal
                    .as_ref()
                    .map(|terminal| terminal.clone().into_any_element())
                    .unwrap_or_else(|| div().into_any_element()),
                PanelKind::Thread(id) => {
                    let Some(thread) = state.snapshot.threads.iter().find(|t| t.id == id) else {
                        return div()
                            .p_3()
                            .text_color(cx.theme().muted_foreground)
                            .child("This thread is no longer available.")
                            .into_any_element();
                    };
                    match thread.kind {
                        ThreadKind::Conversation => {
                            if let Some(transcript) = &self.transcript {
                                conversation::render(
                                    state,
                                    id,
                                    &self.workspace,
                                    &self.output_scroll,
                                    transcript,
                                    self.network.as_ref(),
                                    cx,
                                )
                            } else {
                                div().into_any_element()
                            }
                        }
                        ThreadKind::Factory => {
                            thread_sidebar::SidebarLayout::new(state, id, &self.workspace, cx).wrap(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(if factory::launch_for(state, id).is_some() {
                                        factory::render(state, id, &self.workspace, scroll, cx)
                                    } else if let Some(form) = workspace
                                        .factory_form
                                        .as_ref()
                                        .filter(|form| form.read(cx).thread_id == id)
                                    {
                                        form.clone()
                                            .cached(StyleRefinement::default().size_full())
                                            .into_any_element()
                                    } else {
                                        div()
                                            .p_3()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(if state.selected_thread == Some(id) {
                                                "Loading Factory configuration…"
                                            } else {
                                                "Focus this pane to configure the Factory thread."
                                            })
                                            .into_any_element()
                                    })
                                    .into_any_element(),
                                thread_sidebar::render(
                                    state,
                                    id,
                                    false,
                                    &self.workspace,
                                    &self.output_scroll,
                                    None,
                                    cx,
                                ),
                            )
                        }
                    }
                }
            }
        });
        let focus = self.focus.clone();
        let bounds = self.bounds.clone();
        let name = self.kind.debug_name();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .id(SharedString::from(name.clone()))
            .debug_selector(move || name.clone())
            .test_support()
            .key_context(self.kind.context())
            .track_focus(&self.focus)
            .bg(if self.kind == PanelKind::Projects {
                cx.theme().sidebar
            } else {
                cx.theme().background
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                if !focus.contains_focused(window, cx) {
                    focus.focus(window, cx);
                }
            })
            .children(content)
            .when(pane_focused, |pane| {
                pane.child(
                    div()
                        .id("pane-focus")
                        .test_support()
                        .absolute()
                        .inset_0()
                        .border_1()
                        .border_color(cx.theme().input),
                )
            })
            .child(
                canvas(
                    move |measured, _, _| bounds.set(Some(measured)),
                    |_, _, _, _| {},
                )
                .absolute()
                // Measure the pane, not the static position after its content.
                .inset_0()
                .size_full(),
            )
    }
}
