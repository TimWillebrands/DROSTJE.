use super::*;

impl Workspace {
    pub(super) fn bind_actions(root: Stateful<Div>, cx: &Context<Self>) -> Stateful<Div> {
        root.on_action(
            cx.listener(|this, _: &NewNeovim, w, cx| this.execute(Command::NewNeovim, w, cx)),
        )
        .on_action(cx.listener(|this, _: &OpenConversationFile, w, cx| {
            this.execute(Command::OpenConversationFile, w, cx)
        }))
        .on_action(cx.listener(|this, action: &OpenFileReference, w, cx| {
            this.open_file_reference(action.reference.clone(), action.thread, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &NewTerminal, w, cx| this.execute(Command::NewTerminal, w, cx)),
        )
        .on_action(cx.listener(|this, _: &ToggleTerminal, w, cx| {
            this.execute(Command::ToggleTerminal, w, cx)
        }))
        .on_action(cx.listener(|this, _: &RestartTerminal, w, cx| {
            this.execute(Command::RestartTerminal, w, cx)
        }))
        .on_action(cx.listener(|this, action: &OpenBrowserUrl, w, cx| {
            this.open_browser(action.url.clone(), None, None, w, cx);
        }))
        .on_action(
            cx.listener(|this, _: &NewBrowser, w, cx| this.execute(Command::NewBrowser, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &ToggleBrowser, w, cx| {
                this.execute(Command::ToggleBrowser, w, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &BrowserBack, w, cx| this.execute(Command::BrowserBack, w, cx)),
        )
        .on_action(cx.listener(|this, _: &BrowserForward, w, cx| {
            this.execute(Command::BrowserForward, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &BrowserReload, w, cx| {
                this.execute(Command::BrowserReload, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &BrowserAddress, w, cx| {
            this.execute(Command::BrowserAddress, w, cx)
        }))
        .on_action(cx.listener(|this, _: &ToggleArtifact, w, cx| {
            this.execute(Command::ToggleArtifact, w, cx)
        }))
        .on_action(cx.listener(|this, _: &FocusComposerArtifact, w, cx| {
            this.execute(Command::FocusComposerArtifact, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &MoveArtifactIntoConversation, w, cx| {
                this.execute(Command::MoveArtifactIntoConversation, w, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &WidenSidebar, w, cx| this.execute(Command::WidenSidebar, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &NarrowSidebar, w, cx| {
                this.execute(Command::NarrowSidebar, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &NewSpace, w, cx| this.execute(Command::NewSpace, w, cx)))
        .on_action(
            cx.listener(|this, _: &RenameSpace, w, cx| this.execute(Command::RenameSpace, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &DeleteSpace, w, cx| this.execute(Command::DeleteSpace, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &SwitchSpace, w, cx| this.execute(Command::SwitchSpace, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &NextSpace, w, cx| this.execute(Command::NextSpace, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &PreviousSpace, w, cx| {
                this.execute(Command::PreviousSpace, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &MoveProjectToSpace, w, cx| {
            this.execute(Command::MoveProjectToSpace, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &AddProject, w, cx| this.execute(Command::AddProject, w, cx)),
        )
        .on_action(cx.listener(|this, _: &NewConversation, w, cx| {
            this.execute(Command::NewConversation, w, cx)
        }))
        .on_action(cx.listener(|this, _: &NewOmpConversation, w, cx| {
            this.execute(Command::NewOmpConversation, w, cx)
        }))
        .on_action(cx.listener(|this, _: &NewPiConversation, w, cx| {
            this.execute(Command::NewPiConversation, w, cx)
        }))
        .on_action(cx.listener(|this, _: &NewClaudeCodeConversation, w, cx| {
            this.execute(Command::NewClaudeCodeConversation, w, cx)
        }))
        .on_action(cx.listener(|this, _: &NewCodexConversation, w, cx| {
            this.execute(Command::NewCodexConversation, w, cx)
        }))
        .on_action(cx.listener(|this, _: &NewOpenCodeConversation, w, cx| {
            this.execute(Command::NewOpenCodeConversation, w, cx)
        }))
        .on_action(cx.listener(|this, _: &NewConversationInWorktree, w, cx| {
            this.execute(Command::NewConversationInWorktree, w, cx)
        }))
        .on_action(cx.listener(|this, _: &NewConversationInWorkspace, w, cx| {
            this.execute(Command::NewConversationInWorkspace, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &SwitchHarness, w, cx| {
                this.execute(Command::SwitchHarness, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &SwitchExecutionHost, w, cx| {
            this.execute(Command::SwitchExecutionHost, w, cx)
        }))
        .on_action(cx.listener(|this, _: &RetryConversation, w, cx| {
            this.execute(Command::RetryConversation, w, cx)
        }))
        .on_action(cx.listener(|this, _: &RetryRemoteFile, w, cx| {
            this.execute(Command::RetryRemoteFile, w, cx)
        }))
        .on_action(cx.listener(|this, _: &RemoveRemoteHost, w, cx| {
            this.execute(Command::RemoveRemoteHost, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &PairPhone, w, cx| this.execute(Command::PairPhone, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &RemovePhone, w, cx| this.execute(Command::RemovePhone, w, cx)),
        )
        .on_action(cx.listener(|this, _: &RemoveWorktree, w, cx| {
            this.execute(Command::RemoveWorktree, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &SwitchBranch, w, cx| this.execute(Command::SwitchBranch, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &CommitChanges, w, cx| {
                this.execute(Command::CommitChanges, w, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &InstallPi, w, cx| this.execute(Command::InstallPi, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &RenameProject, w, cx| {
                this.execute(Command::RenameProject, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &ArchiveProject, w, cx| {
            this.execute(Command::ArchiveProject, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &RenameThread, w, cx| this.execute(Command::RenameThread, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &ArchiveThread, w, cx| {
                this.execute(Command::ArchiveThread, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &UnarchiveThread, w, cx| {
            this.execute(Command::UnarchiveThread, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &CloseThread, w, cx| this.execute(Command::CloseThread, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &NextThread, w, cx| this.execute(Command::NextThread, w, cx)),
        )
        .on_action(cx.listener(|this, _: &PreviousThread, w, cx| {
            this.execute(Command::PreviousThread, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &ShowArchived, w, cx| this.execute(Command::ShowArchived, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &SearchThreads, w, cx| {
                this.execute(Command::SearchThreads, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &NewFactoryThread, w, cx| {
            this.execute(Command::NewFactoryThread, w, cx)
        }))
        .on_action(cx.listener(|this, _: &RefreshFactoryRuns, w, cx| {
            this.execute(Command::RefreshFactoryRuns, w, cx)
        }))
        .on_action(cx.listener(|this, _: &Settings, w, cx| this.execute(Command::Settings, w, cx)))
        .on_action(
            cx.listener(|this, _: &ToggleAgents, w, cx| this.execute(Command::ToggleAgents, w, cx)),
        )
        .on_action(cx.listener(|this, _: &JoinSpaceNetwork, w, cx| {
            this.execute(Command::JoinSpaceNetwork, w, cx)
        }))
        .on_action(cx.listener(|this, _: &ShowNetworkMembers, w, cx| {
            this.execute(Command::ShowNetworkMembers, w, cx)
        }))
        .on_action(cx.listener(|this, _: &ShowNetworkMessages, w, cx| {
            this.execute(Command::ShowNetworkMessages, w, cx)
        }))
        .on_action(cx.listener(|this, _: &ShowThreadAgents, w, cx| {
            this.execute(Command::ShowThreadAgents, w, cx)
        }))
        .on_action(cx.listener(|this, _: &LeaveSpaceNetwork, w, cx| {
            this.execute(Command::LeaveSpaceNetwork, w, cx)
        }))
        .on_action(cx.listener(|this, _: &ResumeNetworkDelivery, w, cx| {
            this.execute(Command::ResumeNetworkDelivery, w, cx)
        }))
        .on_action(cx.listener(|this, _: &CustomizeTheme, w, cx| {
            this.execute(Command::CustomizeTheme, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &ResetLayout, w, cx| this.execute(Command::ResetLayout, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &RestartEditor, w, cx| {
                this.execute(Command::RestartEditor, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &CancelEditorCommand, w, cx| {
            this.execute(Command::CancelEditorCommand, w, cx)
        }))
        .on_action(cx.listener(|this, _: &CopyDraftRecovery, w, cx| {
            this.execute(Command::CopyDraftRecovery, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &PasteImage, w, cx| this.execute(Command::PasteImage, w, cx)),
        )
        .on_action(cx.listener(|this, _: &InsertSlashCommand, w, cx| {
            this.execute(Command::InsertSlashCommand, w, cx)
        }))
        .on_action(cx.listener(|this, _: &ClearQueuedMessages, w, cx| {
            this.execute(Command::ClearQueuedMessages, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &SendQueuedNow, w, cx| {
                this.execute(Command::SendQueuedNow, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &EditQueuedMessage, w, cx| {
            this.execute(Command::EditQueuedMessage, w, cx)
        }))
        .on_action(cx.listener(|this, _: &ToggleConversationDiffs, w, cx| {
            this.execute(Command::ToggleConversationDiffs, w, cx)
        }))
        .on_action(cx.listener(|this, _: &ToggleTaskList, w, cx| {
            this.execute(Command::ToggleTaskList, w, cx)
        }))
        .on_action(cx.listener(|this, _: &ToggleBackgroundJobs, w, cx| {
            this.execute(Command::ToggleBackgroundJobs, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &GrowComposer, w, cx| this.execute(Command::GrowComposer, w, cx)),
        )
        .on_action(cx.listener(|this, _: &ShrinkComposer, w, cx| {
            this.execute(Command::ShrinkComposer, w, cx)
        }))
        .on_action(cx.listener(|this, _: &RegenerateTitle, w, cx| {
            this.execute(Command::RegenerateTitle, w, cx)
        }))
        .on_action(cx.listener(|this, _: &RefreshConversationDiff, w, cx| {
            this.execute(Command::RefreshConversationDiff, w, cx)
        }))
        .on_action(cx.listener(|this, _: &ToggleChangesLayout, w, cx| {
            this.execute(Command::ToggleChangesLayout, w, cx)
        }))
        .on_action(cx.listener(|this, _: &OpenChangeInNeovim, w, cx| {
            this.execute(Command::OpenChangeInNeovim, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &ToggleReview, w, cx| this.execute(Command::ToggleReview, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &RefreshReview, w, cx| {
                this.execute(Command::RefreshReview, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &OpenPullRequest, w, cx| {
            this.execute(Command::OpenPullRequest, w, cx)
        }))
        .on_action(cx.listener(|this, _: &RefreshPullRequests, w, cx| {
            this.execute(Command::RefreshPullRequests, w, cx)
        }))
        .on_action(cx.listener(|this, _: &OpenPalette, w, cx| this.open_palette(w, cx)))
        .on_action(
            cx.listener(|this, _: &FocusProjects, w, cx| {
                this.focus_region(Region::Projects, w, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &FocusThread, w, cx| this.focus_region(Region::Thread, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &FocusOutput, w, cx| this.execute(Command::FocusOutput, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &FocusLeft, w, cx| {
                this.spatial_focus(PaneDirection::Left, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &FocusRight, w, cx| {
            this.spatial_focus(PaneDirection::Right, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &FocusUp, w, cx| this.spatial_focus(PaneDirection::Up, w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &FocusDown, w, cx| {
                this.spatial_focus(PaneDirection::Down, w, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &SelectNext, w, cx| this.navigate(Motion::Line(1), w, cx)))
        .on_action(
            cx.listener(|this, _: &SelectPrevious, w, cx| this.navigate(Motion::Line(-1), w, cx)),
        )
        .on_action(cx.listener(|this, _: &SelectFirst, w, cx| this.navigate(Motion::First, w, cx)))
        .on_action(cx.listener(|this, _: &SelectLast, w, cx| this.navigate(Motion::Last, w, cx)))
        .on_action(
            cx.listener(|this, _: &SelectPageUp, w, cx| this.navigate(Motion::Page(-1), w, cx)),
        )
        .on_action(
            cx.listener(|this, _: &SelectPageDown, w, cx| this.navigate(Motion::Page(1), w, cx)),
        )
        .on_action(cx.listener(|this, _: &CollapseProject, _, cx| this.fold(true, cx)))
        .on_action(cx.listener(|this, _: &ExpandProject, _, cx| this.fold(false, cx)))
        .on_action(cx.listener(|this, _: &OpenSelection, w, cx| this.open_selection(w, cx)))
        .on_action(cx.listener(|this, _: &FocusComposer, w, cx| this.focus_composer(w, cx)))
        .on_action(cx.listener(|this, _: &LeaveEditor, w, cx| {
            if let Some(panel) = this
                .active_terminal
                .and_then(|id| this.terminals.borrow().get(&id).cloned())
            {
                panel.read(cx).focus.clone().focus(w, cx);
            } else {
                this.focus_region(Region::Thread, w, cx)
            }
        }))
        .on_action(cx.listener(|this, _: &Submit, w, cx| {
            if this.state.read(cx).factory_thread().is_some() {
                this.start_factory_run(w, cx);
            } else {
                this.submit_prompt(w, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &Interrupt, _, cx| {
            this.state.update(cx, |state, cx| {
                if let Some(id) = state.selected_thread {
                    state.interrupt(id);
                }
                cx.notify();
            });
        }))
        .on_action(cx.listener(|this, _: &Dismiss, w, cx| {
            if this.inline_rename.is_some() {
                this.cancel_rename(w, cx);
            } else {
                this.dismiss(w, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &Confirm, w, cx| {
            if this.inline_rename.is_some() {
                this.commit_rename(w, cx);
            } else {
                this.confirm_dialog(w, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &Quit, w, cx| this.quit_review(w, cx)))
        .on_action(cx.listener(|this, _: &ToggleProjects, w, cx| {
            this.execute(Command::ToggleProjects, w, cx)
        }))
        .on_action(
            cx.listener(|this, _: &ToggleOutput, w, cx| this.execute(Command::ToggleOutput, w, cx)),
        )
    }
}
