use super::*;

pub(super) struct WorktreeProgressNotice;

impl Workspace {
    pub(super) fn show_notices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let count = state
            .notice_sequence
            .wrapping_sub(self.seen_notice_sequence) as usize;
        self.seen_notice_sequence = state.notice_sequence;
        if count == 0 {
            return;
        }
        let notes: Vec<_> = state
            .notices
            .iter()
            .skip(state.notices.len().saturating_sub(count))
            .filter(|notice| matches!(notice.level, NoticeLevel::Warning | NoticeLevel::Error))
            .filter(|notice| {
                ![
                    state.storage_error.as_deref(),
                    state.shutdown_error.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|error| notice.text.contains(error))
            })
            .filter(|notice| {
                !state
                    .conversation_thread()
                    .and_then(|thread| state.conversations.get(&thread.id))
                    .and_then(|session| session.error.as_deref())
                    .is_some_and(|error| notice.text.contains(error))
            })
            .cloned()
            .collect();
        for notice in notes {
            window.push_notification(
                match notice.level {
                    NoticeLevel::Warning => Notification::warning(notice.text),
                    _ => Notification::error(notice.text),
                },
                cx,
            );
        }
    }

    pub(super) fn open_commit_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| thread.id)
        else {
            return;
        };
        self.dismiss(window, cx);
        let kind = DialogKind::Commit(thread);
        let retained = self.take_surface_draft(&kind, cx);
        let new_surface = retained.is_none();
        let state = self.state.clone();
        let owner = cx.entity().downgrade();
        let dialog = retained
            .unwrap_or_else(|| cx.new(|cx| SurfaceContent::new(kind, state, owner, window, cx)));
        if new_surface {
            dialog.update(cx, |dialog, cx| dialog.write_commit_subject(cx));
        }
        self.commit_panel_thread = Some(thread);
        self.show_sidebar_tab(thread, SidebarTab::Changes, window, cx);
        dialog.update(cx, |dialog, cx| dialog.focus_commit_message(window, cx));
        self.dialog = Some(dialog);
        cx.notify();
    }

    pub(in crate::ui) fn selected_remote_host(&self, cx: &App) -> Option<String> {
        let dialog = self.dialog.as_ref()?.read(cx);
        if !matches!(dialog.kind, DialogKind::Settings(_)) {
            return None;
        }
        let id = dialog.value("remote_host", cx);
        self.state
            .read(cx)
            .remote_hosts()
            .iter()
            .any(|host| host.id == id)
            .then_some(id)
    }

    pub(in crate::ui) fn selected_phone(&self, cx: &App) -> Option<uuid::Uuid> {
        let dialog = self.dialog.as_ref()?.read(cx);
        if !matches!(dialog.kind, DialogKind::Settings(_)) {
            return None;
        }
        crate::ui::settings::selected_phone(&dialog.value("phone", cx), self.state.read(cx))
    }

    pub(super) fn confirm_remove_phone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected_phone(cx) else {
            return;
        };
        let Some(surface) = self.dialog.clone() else {
            return;
        };
        let kind = surface.read(cx).kind.clone();
        let name = self
            .state
            .read(cx)
            .paired_phones()
            .into_iter()
            .find(|phone| phone.id == id)
            .map(|phone| phone.name)
            .unwrap_or_else(|| "this phone".into());
        self.dismiss(window, cx);
        if self.navigation_blocked(window, cx) {
            return;
        }
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let confirm = owner.clone();
            let accepted_owner = owner.clone();
            let accepted_kind = kind.clone();
            let restore = owner.clone();
            let surface = surface.clone();
            let kind = kind.clone();
            alert
                .title(
                    div()
                        .id("remove-phone-confirmation")
                        .test_support()
                        .child(format!("Remove phone “{name}”?")),
                )
                .description("The phone stops receiving updates and shows that it was removed. Pair it again with a new code to reconnect.")
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Remove phone")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let _ = confirm.update(cx, |workspace, cx| {
                        let result = workspace.state.update(cx, |state, cx| {
                            let result = state.remove_phone(id);
                            cx.notify();
                            result
                        });
                        surface.update(cx, |surface, cx| match result {
                            Ok(_) => surface.phone_removed(window, cx),
                            Err(error) => surface.set_error(&error, cx),
                        });
                    });
                    Self::restore_host_settings(accepted_owner.clone(), accepted_kind.clone(), window, cx);
                    false
                })
                .on_cancel(move |_, window, cx| {
                    Self::restore_host_settings(restore.clone(), kind.clone(), window, cx);
                    false
                })
        });
    }

    pub(super) fn open_pair_phone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(DialogKind::PairPhone, window, cx);
        let Some(dialog) = self
            .dialog
            .clone()
            .filter(|dialog| dialog.read(cx).kind == DialogKind::PairPhone)
        else {
            return;
        };
        let result = self.state.update(cx, |state, cx| {
            let result = state.start_phone_pairing(cx);
            cx.notify();
            result
        });
        if let Err(error) = result {
            dialog.update(cx, |dialog, cx| dialog.set_error(&error, cx));
        }
    }

    fn confirm_phone_pairing(
        &mut self,
        dialog: Entity<SurfaceContent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let status = self
            .state
            .read(cx)
            .phones
            .pairing
            .as_ref()
            .map(|pairing| pairing.status.clone());
        let result = match status {
            Some(crate::app::PhonePairingStatus::Incoming { .. }) => {
                self.state.update(cx, |state, cx| {
                    let result = state.accept_phone_pairing();
                    cx.notify();
                    result
                })
            }
            Some(crate::app::PhonePairingStatus::Paired { .. }) => {
                self.dismiss(window, cx);
                Ok(())
            }
            Some(
                crate::app::PhonePairingStatus::Expired
                | crate::app::PhonePairingStatus::Problem(_),
            )
            | None => self.state.update(cx, |state, cx| {
                let result = state.start_phone_pairing(cx);
                cx.notify();
                result
            }),
            Some(
                crate::app::PhonePairingStatus::Connecting
                | crate::app::PhonePairingStatus::Showing
                | crate::app::PhonePairingStatus::Accepting { .. },
            ) => Ok(()),
        };
        match result {
            Ok(()) => dialog.update(cx, |dialog, cx| dialog.clear_error(cx)),
            Err(error) => dialog.update(cx, |dialog, cx| dialog.set_error(&error, cx)),
        }
    }

    pub(in crate::ui) fn end_phone_pairing(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.cancel_phone_pairing();
            cx.notify();
        });
        self.discard_surface(id, cx);
        cx.notify();
    }

    pub(super) fn confirm_remove_remote_host(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.selected_remote_host(cx) else {
            return;
        };
        let Some(surface) = self.dialog.clone() else {
            return;
        };
        let kind = surface.read(cx).kind.clone();
        let name = self
            .state
            .read(cx)
            .remote_hosts()
            .into_iter()
            .find(|host| host.id == id)
            .map(|host| host.name)
            .unwrap_or_else(|| id.clone());
        self.dismiss(window, cx);
        if self.navigation_blocked(window, cx) {
            return;
        }
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let confirm = owner.clone();
            let accepted_owner = owner.clone();
            let accepted_kind = kind.clone();
            let restore = owner.clone();
            let id = id.clone();
            let surface = surface.clone();
            let kind = kind.clone();
            alert.title(div().id("remove-host-confirmation").test_support().child(format!("Remove Host “{name}”?")))
                .description("Removes this desktop Host configuration and its unsaved Host fields. It does not stop the Host or delete its files. Other Settings drafts are kept.")
                .button_props(DialogButtonProps::default().ok_text("Remove Host").cancel_text("Cancel").show_cancel(true))
                .on_ok(move |_, window, cx| {
                    let _ = confirm.update(cx, |workspace, cx| {
                        let result = workspace.state.update(cx, |state, cx| {
                            let result = state.remove_remote_host(&id);
                            cx.notify();
                            result
                        });
                        surface.update(cx, |surface, cx| match result {
                            Ok(()) => surface.remote_host_removed(window, cx),
                            Err(error) => surface.set_error(&error, cx),
                        });
                    });
                    Self::restore_host_settings(accepted_owner.clone(), accepted_kind.clone(), window, cx);
                    false
                })
                .on_cancel(move |_, window, cx| {
                    Self::restore_host_settings(restore.clone(), kind.clone(), window, cx);
                    false
                })
        });
    }

    pub(super) fn restore_host_settings(
        owner: WeakEntity<Self>,
        kind: DialogKind,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.defer(cx, move |window, cx| {
            window.close_dialog(cx);
            let _ = owner.update(cx, |workspace, cx| workspace.open_dialog(kind, window, cx));
        });
    }

    pub(super) fn open_dialog(
        &mut self,
        kind: DialogKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.extension.is_some()
            || self.state.read(cx).shutting_down
            || self
                .editor
                .as_ref()
                .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        self.dismiss(window, cx);
        if self.detail_surface.is_some() || self.palette.is_some() || self.theme_editor.is_some() {
            return;
        }
        let retained = self.take_surface_draft(&kind, cx);
        let state = self.state.clone();
        let owner = cx.entity().downgrade();
        let dialog = retained
            .unwrap_or_else(|| cx.new(|cx| SurfaceContent::new(kind, state, owner, window, cx)));
        self.dialog = Some(dialog);
        crate::ui::settings::open_surface(cx.entity().downgrade(), window, cx);
        cx.notify();
    }
    pub(super) fn take_surface_draft(
        &mut self,
        kind: &DialogKind,
        cx: &App,
    ) -> Option<Entity<SurfaceContent>> {
        if let Some(dialog) = self.dialog.take() {
            self.surface_drafts.push(dialog);
        }
        let index = self
            .surface_drafts
            .iter()
            .position(|dialog| dialog.read(cx).kind == *kind)?;
        Some(self.surface_drafts.remove(index))
    }

    pub(super) fn discard_surface(&mut self, id: uuid::Uuid, cx: &App) {
        if self
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).surface_id() == id)
        {
            self.dialog = None;
            self.commit_panel_thread = None;
        }
        self.surface_drafts
            .retain(|dialog| dialog.read(cx).surface_id() != id);
    }

    pub(in crate::ui) fn has_open_form(&self, cx: &App) -> bool {
        self.dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).hosted())
    }

    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).has_composition())
        {
            return;
        }
        if self
            .extension
            .as_ref()
            .is_some_and(|form| !form.inline_answer)
        {
            self.close_extension(true, window, cx);
            if self.extension.is_some() {
                return;
            }
        }
        if !self.close_detail_surface(window, cx) {
            return;
        }
        self.image_preview = None;
        let closed_commit = self.commit_panel_thread.take().is_some();
        if let Some(palette) = self.palette.take() {
            palette.update(cx, |palette, cx| palette.dismiss(window, cx));
        }
        if let Some(editor) = self.theme_editor.take() {
            editor.update(cx, |editor, cx| editor.dismiss(window, cx));
        }
        let pairing = self
            .dialog
            .as_ref()
            .filter(|dialog| dialog.read(cx).kind == DialogKind::PairPhone)
            .map(|dialog| dialog.read(cx).surface_id());
        if let Some(dialog) = self.dialog.as_ref() {
            let (kind, hosted, request) = {
                let dialog = dialog.read(cx);
                (dialog.kind.clone(), dialog.hosted(), dialog.request)
            };
            if hosted {
                dialog.update(cx, |dialog, cx| dialog.unhost(cx));
                crate::ui::settings::close_surface(&kind, window, cx);
            }
            if let Some(request) = request {
                self.background_worktree_request = Some(request);
                window.push_notification(
                    Notification::new()
                        .id1::<WorktreeProgressNotice>(request.to_string())
                        .message("Git operation continues in the background")
                        .autohide(false),
                    cx,
                );
            }
        }
        if let Some(id) = pairing {
            self.end_phone_pairing(id, cx);
        }
        if closed_commit {
            self.focus_region(Region::Thread, window, cx);
        }
        cx.notify();
    }
    pub fn confirm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .extension
            .as_ref()
            .is_some_and(|form| !form.inline_answer)
        {
            self.confirm_extension(window, cx);
            return;
        }
        let Some(dialog) = self.dialog.as_ref().cloned() else {
            return;
        };
        if !dialog.read(cx).hosted() && self.commit_panel_thread.is_none() {
            return;
        }
        if dialog.read(cx).kind == DialogKind::PairPhone {
            self.confirm_phone_pairing(dialog, window, cx);
            return;
        }
        if dialog.read(cx).request.is_some() {
            return;
        }
        if matches!(
            dialog.read(cx).kind,
            DialogKind::NewSpace | DialogKind::DeleteSpace(_)
        ) && let Some(reason) = self.space_transition_reason(cx)
        {
            dialog.update(cx, |dialog, cx| {
                dialog.set_error(&anyhow::anyhow!(reason), cx)
            });
            return;
        }
        let result = self.state.update(cx, |state, cx| {
            let result = dialog.read(cx).apply(state, cx);
            if result.is_ok() {
                cx.notify();
            }
            result
        });
        let result = result.and_then(|outcome| {
            match outcome {
                SurfaceOutcome::Applied => {}
                SurfaceOutcome::SpaceCreated(id) => self.switch_space(id, window, cx)?,
                SurfaceOutcome::DeleteSpace(id) => {
                    self.change_space(SpaceChange::Delete(id), window, cx)?
                }
                SurfaceOutcome::Pending(request) => return Ok(Some(request)),
            }
            Ok(None)
        });
        match result {
            Ok(None) => {
                let (kind, id, hosted) = {
                    let dialog = dialog.read(cx);
                    (dialog.kind.clone(), dialog.surface_id(), dialog.hosted())
                };
                if hosted {
                    crate::ui::settings::close_surface(&kind, window, cx);
                }
                if matches!(kind, DialogKind::Commit(_)) {
                    self.commit_panel_thread = None;
                }
                self.discard_surface(id, cx);
            }
            Ok(Some(request)) => {
                dialog.update(cx, |dialog, cx| dialog.begin_request(request, cx));
            }
            Err(error) => {
                dialog.update(cx, |dialog, cx| dialog.set_error(&error, cx));
            }
        }
        cx.notify();
    }
    pub(super) fn settle_worktree_dialog(
        &mut self,
        request: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let status = self
            .state
            .read(cx)
            .worktree_request
            .as_ref()
            .filter(|pending| pending.id == request)
            .map(|pending| pending.status.clone());
        let Some(status) = status else {
            return;
        };
        let dialog = self
            .dialog
            .iter()
            .chain(self.surface_drafts.iter())
            .find(|dialog| dialog.read(cx).last_request() == Some(request))
            .cloned();
        let hosted = dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).hosted());
        let background = self.background_worktree_request == Some(request);
        if !matches!(status, crate::app::WorktreeRequestStatus::Pending) && background {
            self.background_worktree_request = None;
        }
        match status {
            crate::app::WorktreeRequestStatus::Done(_) => {
                if let Some(dialog) = dialog {
                    let (kind, id, hosted) = {
                        let dialog = dialog.read(cx);
                        (dialog.kind.clone(), dialog.surface_id(), dialog.hosted())
                    };
                    if hosted {
                        crate::ui::settings::close_surface(&kind, window, cx);
                    }
                    if matches!(kind, DialogKind::Commit(_)) {
                        self.commit_panel_thread = None;
                    }
                    self.discard_surface(id, cx);
                }
                if !hosted || background {
                    window.push_notification(
                        Notification::success("Git operation completed")
                            .id1::<WorktreeProgressNotice>(request.to_string()),
                        cx,
                    );
                } else {
                    self.focus_region(Region::Thread, window, cx);
                }
            }
            crate::app::WorktreeRequestStatus::Failed(error)
            | crate::app::WorktreeRequestStatus::Refused(error) => {
                if !hosted || background {
                    window.push_notification(
                        Notification::error(format!("Git operation failed: {error}"))
                            .id1::<WorktreeProgressNotice>(request.to_string()),
                        cx,
                    );
                }
            }
            crate::app::WorktreeRequestStatus::Pending => {}
        }
        cx.notify();
    }
}
