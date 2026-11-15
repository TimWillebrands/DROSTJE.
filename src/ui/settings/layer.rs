//! Hosting a form in a dialog, alert dialog or sheet, its footer actions, and dismissal.
use super::{DialogKind, SurfaceContent};
use crate::ui::workspace::Workspace;
use gpui_kit::{
    component::{
        Disableable, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
    },
    *,
};
use uuid::Uuid;

struct SurfaceFooter {
    content: WeakEntity<SurfaceContent>,
    _subscription: Subscription,
}

impl Render for SurfaceFooter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(content) = self.content.upgrade() else {
            return div().into_any_element();
        };
        footer(content.read(cx))
    }
}

pub(super) fn footer(dialog: &SurfaceContent) -> AnyElement {
    let readonly = dialog.presentation.readonly;
    let pending = dialog.request.is_some();
    let id = dialog.surface_id;
    let cancel = dialog.owner.clone();
    let confirm = dialog.owner.clone();
    h_flex()
        .w_full()
        .justify_end()
        .gap_2()
        .child(
            Button::new("dialog-cancel")
                .ghost()
                .label(
                    if matches!(dialog.kind, DialogKind::Settings(_) | DialogKind::Commit(_)) {
                        "Close"
                    } else {
                        "Cancel"
                    },
                )
                .on_click(move |_, window, cx| {
                    let _ = cancel.update(cx, |workspace, cx| {
                        if workspace
                            .dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.read(cx).surface_id == id)
                        {
                            workspace.dismiss(window, cx);
                        }
                    });
                }),
        )
        .child(
            Button::new("dialog-confirm")
                .primary()
                .disabled(readonly || pending)
                .loading(pending)
                .label(if matches!(dialog.kind, DialogKind::DeleteSpace(_)) {
                    "Delete space"
                } else if matches!(dialog.kind, DialogKind::NewSpace) {
                    "Create space"
                } else if matches!(
                    dialog.kind,
                    DialogKind::NewWorktreeConversation(_)
                        | DialogKind::NewWorkspaceConversation(_)
                ) {
                    "Create conversation"
                } else if matches!(dialog.kind, DialogKind::SwitchBranch(_)) {
                    "Switch"
                } else if matches!(dialog.kind, DialogKind::Commit(_)) {
                    "Commit"
                } else if matches!(dialog.kind, DialogKind::RemoveWorktree(_)) {
                    if dialog.force {
                        "Discard changes and remove"
                    } else {
                        "Remove worktree"
                    }
                } else {
                    "Save"
                })
                .on_click(move |_, window, cx| {
                    confirm_surface(&confirm, id, window, cx);
                }),
        )
        .into_any_element()
}

pub(super) fn confirm_surface(
    owner: &WeakEntity<Workspace>,
    id: Uuid,
    window: &mut Window,
    cx: &mut App,
) {
    let _ = owner.update(cx, |workspace, cx| {
        if workspace
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).surface_id == id)
        {
            workspace.confirm_dialog(window, cx);
        }
    });
}

/// Closing a layer hides it; the Workspace retains this entity as the draft owner.
fn unhost_surface(owner: &WeakEntity<Workspace>, id: Uuid, cx: &mut App) {
    let _ = owner.update(cx, |workspace, cx| {
        if let Some(dialog) = workspace
            .dialog
            .as_ref()
            .filter(|dialog| dialog.read(cx).surface_id == id)
        {
            dialog.update(cx, |dialog, _| dialog.hosted = false);
            cx.notify();
        }
    });
}

/// The Root captures trigger focus and owns trapping, dismissal, and one focus restoration.
pub(in crate::ui) fn open_surface(owner: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| {
        let Ok(Some(content)) = owner.update(cx, |workspace, cx| {
            if workspace.quit_requested {
                return None;
            }
            workspace
                .dialog
                .as_ref()
                .filter(|dialog| !dialog.read(cx).hosted)
                .cloned()
        }) else {
            return;
        };
        let (id, kind, title) = content.update(cx, |dialog, _| {
            dialog.hosted = true;
            (dialog.surface_id, dialog.kind.clone(), dialog.title())
        });
        let footer = cx.new(|cx| SurfaceFooter {
            content: content.downgrade(),
            _subscription: cx.observe(&content, |_, _, cx| cx.notify()),
        });
        let host_owner = owner.clone();
        match kind {
            DialogKind::Settings(_) => {
                window.open_sheet(cx, move |sheet, window, _| {
                    let close = host_owner.clone();
                    sheet
                        .title(title)
                        .size((window.rem_size() * 56.).min(window.viewport_size().width))
                        .resizable(false)
                        .px_0()
                        .child(
                            content
                                .clone()
                                .cached(StyleRefinement::default().size_full()),
                        )
                        .footer(footer.clone())
                        .on_close(move |_, _, cx| unhost_surface(&close, id, cx))
                });
            }
            DialogKind::DeleteSpace(_) | DialogKind::RemoveWorktree(_) => {
                window.open_alert_dialog(cx, move |alert, window, _| {
                    let close = host_owner.clone();
                    let confirm = host_owner.clone();
                    alert
                        .title(title)
                        .width((window.rem_size() * 30.).min(window.viewport_size().width))
                        .child(content.clone())
                        .footer(footer.clone())
                        .on_ok(move |_, window, cx| {
                            confirm_surface(&confirm, id, window, cx);
                            false
                        })
                        .on_close(move |_, _, cx| unhost_surface(&close, id, cx))
                });
            }
            _ => {
                window.open_dialog(cx, move |dialog, window, _| {
                    let close = host_owner.clone();
                    let confirm = host_owner.clone();
                    dialog
                        .title(title)
                        .width((window.rem_size() * 30.).min(window.viewport_size().width))
                        .child(content.clone())
                        .footer(footer.clone())
                        .on_ok(move |_, window, cx| {
                            confirm_surface(&confirm, id, window, cx);
                            false
                        })
                        .on_close(move |_, _, cx| unhost_surface(&close, id, cx))
                });
            }
        }
        window.on_next_frame(move |window, cx| {
            let Some(owner) = owner.upgrade() else { return };
            let workspace = owner.read(cx);
            if workspace.quit_requested {
                return;
            }
            let Some(dialog) = workspace.dialog.as_ref().filter(|dialog| {
                let dialog = dialog.read(cx);
                dialog.surface_id == id && dialog.hosted
            }) else {
                return;
            };
            let dialog = dialog.read(cx);
            if matches!(dialog.kind, DialogKind::Settings(_)) {
                // Settings retains its search and selected group. A saved field can
                // be virtualized away; focus the first mounted control instead.
                window.focus_next(cx);
                return;
            }
            let focus = if let Some(select) = dialog
                .select("theme_mode")
                .or_else(|| dialog.select("branch"))
            {
                Some(select.focus_handle(cx))
            } else if let Some(commit) = &dialog.commit {
                Some(commit.message.focus_handle(cx))
            } else {
                dialog
                    .fields
                    .first()
                    .map(|field| field.input.focus_handle(cx))
                    .or_else(|| {
                        dialog
                            .selects
                            .first()
                            .map(|select| select.state.focus_handle(cx))
                    })
            };
            if let Some(focus) = focus {
                focus.focus(window, cx);
            }
        });
    });
}

pub(in crate::ui) fn close_surface(kind: &DialogKind, window: &mut Window, cx: &mut App) {
    if matches!(kind, DialogKind::Settings(_)) {
        window.close_sheet(cx);
    } else {
        window.close_dialog(cx);
    }
}
