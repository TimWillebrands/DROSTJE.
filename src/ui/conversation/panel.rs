use super::{
    disclosure::{disclosure, is_expanded},
    images::{ThumbnailLayout, image_thumbnail},
    queue::{queued_messages, queued_messages_height},
    requests::request_card,
    sticky_dock::{dock_contents, sticky_dock},
    transcript::TranscriptView,
    usage::usage_meters,
    {content_column, inset, mono},
};
use crate::ui::{
    commands::{Command, SwitchHarness, UnarchiveThread, button},
    workspace::{Workspace, activity},
};
use crate::{
    app::{AppState, ThreadActivity},
    model::ThreadId,
};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{
        ActiveTheme, Disableable, IconName, Sizable,
        alert::Alert,
        button::{Button, ButtonVariants},
        collapsible::Collapsible,
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        select::Select,
        tag::Tag,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::time::SystemTime;

pub(in crate::ui) fn render(
    state: &AppState,
    id: ThreadId,
    owner: &WeakEntity<Workspace>,
    scroll: &ScrollHandle,
    transcript: &Entity<TranscriptView>,
    network: Option<&Entity<crate::ui::network::NetworkPanel>>,
    cx: &App,
) -> AnyElement {
    render_content(state, id, owner, scroll, transcript, network, cx)
}

fn frame(
    layout: crate::ui::workspace::thread_sidebar::SidebarLayout,
    root: Stateful<Div>,
    history: Div,
    sidebar: AnyElement,
    composer: Option<super::composer::ComposerSplit>,
) -> AnyElement {
    let content = match composer {
        Some(mut split) => {
            split.history = history.into_any_element();
            split.into_any_element()
        }
        None => history.into_any_element(),
    };
    root.child(layout.wrap(content, sidebar)).into_any_element()
}

fn render_content(
    state: &AppState,
    id: ThreadId,
    owner: &WeakEntity<Workspace>,
    output_scroll: &ScrollHandle,
    transcript: &Entity<TranscriptView>,
    network: Option<&Entity<crate::ui::network::NetworkPanel>>,
    cx: &App,
) -> AnyElement {
    let drop_owner = owner.clone();
    let sidebar = crate::ui::workspace::thread_sidebar::render(
        state,
        id,
        true,
        owner,
        output_scroll,
        network,
        cx,
    );
    let layout = crate::ui::workspace::thread_sidebar::SidebarLayout::new(state, id, owner, cx);
    let root = v_flex()
        .id(SharedString::from(format!("conversation-view-{id}")))
        .size_full()
        .min_w_0()
        .min_h_0()
        .text_color(cx.theme().foreground)
        .on_drop(move |paths: &ExternalPaths, window, cx| {
            let _ = drop_owner.update(cx, |workspace, cx| {
                if workspace.editor_override.is_none()
                    && workspace.state.read(cx).selected_thread == Some(id)
                {
                    workspace.import_images(id, paths.paths().to_vec(), window, cx);
                }
            });
        });
    let mut history = v_flex().flex_1().min_w_0().min_h_0().gap_2().pb_2();
    let Some(thread) = state.snapshot.threads.iter().find(|thread| thread.id == id) else {
        return frame(
            layout,
            root,
            history.child(div().p_3().child("This thread is no longer available.")),
            sidebar,
            None,
        );
    };
    let Some(project) = state
        .snapshot
        .projects
        .iter()
        .find(|project| project.id == thread.project_id)
    else {
        return frame(
            layout,
            root,
            history.child(
                div()
                    .p_3()
                    .child("This thread's project is no longer available."),
            ),
            sidebar,
            None,
        );
    };
    let active = state.active_threads.contains(&id);
    let host_reason = state.thread_host_selection_reason(id).or_else(|| {
        (state.selected_thread != Some(id)).then(|| "Select this Conversation first.".to_owned())
    });
    let host_owner = owner.clone();
    history = history.child(
        h_flex()
            .id("conversation-execution-location")
            .test_support()
            .items_center()
            .gap_2()
            .px_3()
            .flex_wrap()
            .child(
                Button::new("switch-execution-host")
                    .small()
                    .ghost()
                    .label(state.thread_host_label(id))
                    .disabled(host_reason.is_some())
                    .tooltip(
                        host_reason
                            .unwrap_or_else(|| Command::SwitchExecutionHost.label().to_owned()),
                    )
                    .on_click(move |_, window, cx| {
                        let _ = host_owner.update(cx, |workspace, cx| {
                            workspace.execute(Command::SwitchExecutionHost, window, cx)
                        });
                    }),
            )
            .child(gpui_kit::component::label::Label::new(
                state.thread_host_status(id),
            )),
    );
    if thread.remote.is_none()
        && let Some(Err(error)) = state.repositories.get(&project.id)
    {
        history = history.child(
            Alert::error(
                "conversation-root-error",
                format!("Root unavailable: {error}"),
            )
            .small(),
        );
    }
    let mut statuses: Vec<String> = Vec::new();
    if let Some(session) = state.conversations.get(&id) {
        if let Some(error) = &session.error {
            history = history.child(inset(
                Alert::error("conversation-session-error", error.clone()).small(),
            ));
        }
        statuses = session.statuses.values().cloned().collect();
        history = history.child(
            div().relative().flex_1().min_h(rems(4.)).min_w_0().child(
                transcript
                    .clone()
                    .cached(StyleRefinement::default().absolute().size_full()),
            ),
        );
        if state.is_untouched(thread) && state.switch_harness_reason_for(id).is_none() {
            let switch = owner.clone();
            history = history.child(inset(
                h_flex()
                    .id("untouched-harness-hint")
                    .test_support()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Starts on {}. New conversation again picks another harness.",
                        crate::harness::display_name(&thread.harness)
                    ))
                    .child(
                        Button::new("untouched-switch-harness")
                            .small()
                            .ghost()
                            .label(Command::SwitchHarness.label())
                            .tooltip_with_action(
                                Command::SwitchHarness.label(),
                                &SwitchHarness,
                                Some("Workspace"),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = switch.update(cx, |workspace, cx| {
                                    workspace.execute(Command::SwitchHarness, window, cx)
                                });
                            }),
                    ),
            ));
        }
        for request in session.requests.values() {
            history = history.child(inset(request_card(id, &thread.harness, request, owner, cx)));
        }
        for (index, (key, lines)) in session.widgets.iter().enumerate() {
            if key.starts_with("Proposed editor replacement") {
                let owner = owner.clone();
                let text = lines.join("\n");
                history = history.child(inset(
                    Button::new(("apply-editor-proposal", index))
                        .small()
                        .ghost()
                        .label("Review replacement…")
                        .on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |workspace, cx| {
                                workspace.replace_draft_reviewed(
                                    id,
                                    text.clone(),
                                    None,
                                    false,
                                    window,
                                    cx,
                                )
                            });
                        }),
                ));
                continue;
            }
            let folded_key = format!("widget-folded:{id}:{key}");
            let folded = is_expanded(owner, &folded_key, cx);
            history = history.child(inset(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(disclosure(
                                folded_key,
                                !folded,
                                owner,
                                Some(id),
                                if folded { "Show widget" } else { "Hide widget" },
                            ))
                            .child(key.clone()),
                    )
                    .child(
                        Collapsible::new()
                            .open(!folded)
                            .content(mono(cx).pl_6().child(lines.join("\n"))),
                    ),
            ));
        }
        let dock = dock_contents(&transcript.read(cx).dock, session);
        if let Some(dock) = sticky_dock(id, &dock, owner, cx) {
            history = history.child(dock);
        }
        if let Some(pane) = owner
            .upgrade()
            .and_then(|workspace| workspace.read(cx).docked_artifact(id))
        {
            history = history.child(inset(
                div()
                    .id("composer-artifact")
                    .debug_selector(|| "composer-artifact".into())
                    .test_support()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .child(pane),
            ));
        }
    } else {
        history = history.child(div().flex_1().p_4().child(if thread.archived {
            "Archived conversation has no provider history.".into()
        } else {
            format!(
                "Opening {} session…",
                crate::harness::display_name(&thread.harness)
            )
        }));
    }
    let unresolved_submission = thread.submission.as_ref().filter(|submission| {
        let submission_state = submission
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        !active || !matches!(submission_state, "pending" | "accepted")
    });
    if let Some(submission) = unresolved_submission {
        let restore = owner.clone();
        let dismiss = owner.clone();
        let submission_state = submission
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let detail = submission
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map(|error| format!(" {error}."))
            .unwrap_or_default();
        history = history.child(inset(
            v_flex()
                .gap_1()
                .child(
                    Alert::warning(
                        "unresolved-submission",
                        format!("Submission {submission_state}.{detail} The draft was not resent."),
                    )
                    .small(),
                )
                .when(!active, |view| {
                    view.child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("restore-submitted")
                                    .small()
                                    .ghost()
                                    .label("Restore submitted draft…")
                                    .on_click(move |_, window, cx| {
                                        let _ = restore.update(cx, |workspace, cx| {
                                            workspace.restore_submitted(id, window, cx)
                                        });
                                    }),
                            )
                            .child(
                                Button::new("dismiss-submitted")
                                    .small()
                                    .ghost()
                                    .label("Keep current draft")
                                    .on_click(move |_, _, cx| {
                                        let _ = dismiss.update(cx, |workspace, cx| {
                                            workspace.state.update(cx, |state, cx| {
                                                state.acknowledge_recovery(id);
                                                cx.notify();
                                            })
                                        });
                                    }),
                            ),
                    )
                }),
        ));
    }
    if thread.archived || project.archived {
        let unarchive = (thread.archived && !project.archived).then(|| {
            let reason = state.unarchive_thread_reason_for(id);
            let owner = owner.clone();
            Button::new("unarchive-thread")
                .small()
                .ghost()
                .label(Command::UnarchiveThread.label())
                .disabled(reason.is_some())
                .map(|button| match reason {
                    Some(reason) => button.tooltip(reason),
                    None => button.tooltip_with_action(
                        Command::UnarchiveThread.label(),
                        &UnarchiveThread,
                        Some("Workspace"),
                    ),
                })
                .on_click(move |_, _, cx| {
                    let _ = owner.update(cx, |workspace, cx| workspace.unarchive_thread(id, cx));
                })
        });
        return frame(
            layout,
            root,
            history.child(inset(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Archived history · read-only"),
                    )
                    .children(unarchive),
            )),
            sidebar,
            None,
        );
    }
    let mut composer = v_flex()
        .id("composer-draft-content")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll();
    if !thread.queued.is_empty() {
        composer = composer.child(queued_messages(
            thread,
            active,
            state.queued_dispatch_reason(id),
            owner,
            cx,
        ));
    }
    let mut previews = h_flex()
        .image_cache(transcript.read(cx).images.clone())
        .id("draft-attachments")
        .overflow_x_scroll()
        .gap_3()
        .px_2()
        .pt_2();
    for image in &thread.attachments {
        let remove = owner.clone();
        let attachment_id = image.id;
        previews = previews.child(
            v_flex()
                .gap_1()
                .child(image_thumbnail(image, ThumbnailLayout::Chip, owner, cx))
                .child(
                    Button::new(SharedString::from(format!("remove-{attachment_id}")))
                        .small()
                        .ghost()
                        .label("Remove")
                        .on_click(move |_, _, cx| {
                            let _ = remove.update(cx, |workspace, cx| {
                                workspace.state.update(cx, |state, cx| {
                                    if let Some(mut thread) =
                                        state.snapshot.threads.iter().find(|t| t.id == id).cloned()
                                    {
                                        thread.attachments.retain(|a| a.id != attachment_id);
                                        state.save_thread(thread);
                                    }
                                    cx.notify();
                                })
                            });
                        }),
                ),
        );
    }
    if !thread.attachments.is_empty() {
        composer = composer.child(previews);
    }
    let hosts_editor = owner.upgrade().is_some_and(|workspace| {
        let workspace = workspace.read(cx);
        workspace.editor.is_some()
            && workspace.editor_override.is_none()
            && workspace.editor_selection == Some(crate::model::DraftKey::Thread(id))
    });
    let editor = owner
        .upgrade()
        .and_then(|workspace| workspace.read(cx).editor.clone())
        .filter(|_| hosts_editor);
    composer = composer.child(match editor {
        Some(editor) => div()
            .flex_1()
            .min_h(rems(2.))
            .child(editor)
            .into_any_element(),
        None => div()
            .id(SharedString::from(format!("draft-preview-{id}")))
            .flex_1()
            .min_h(rems(2.))
            .overflow_y_scroll()
            .px_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .whitespace_normal()
            .child(if thread.draft.trim().is_empty() {
                format!(
                    "Message {} — press i to write",
                    crate::harness::display_name(&thread.harness)
                )
            } else {
                format!("{}\n\nPress i to edit", thread.draft)
            })
            .into_any_element(),
    });
    let mut leading = h_flex().flex_1().min_w_0().items_center().gap_1();
    let mut trailing = h_flex().flex_shrink_0().items_center().gap_1();
    let attach = owner.clone();
    let menu_owner = owner.clone();
    let no_images = thread.attachments.is_empty();
    leading = leading
        .child(
            Button::new("attach-images")
                .small()
                .ghost()
                .icon(Lucide::Plus)
                .tooltip("Attach images…")
                .disabled(state.storage_error.is_some())
                .on_click(move |_, window, cx| {
                    let _ = attach.update(cx, |workspace, cx| workspace.attach_images(window, cx));
                }),
        )
        .child(
            Button::new("composer-actions")
                .small()
                .ghost()
                .icon(IconName::Ellipsis)
                .tooltip("Conversation actions")
                .dropdown_menu(move |menu, _, _| {
                    let details = menu_owner.clone();
                    let optimize = menu_owner.clone();
                    let recovery = menu_owner.clone();
                    menu.item(PopupMenuItem::new("Thread details").on_click(
                        move |_, window, cx| {
                            let _ = details.update(cx, |workspace, cx| {
                                workspace.toggle_thread_details(id, window, cx)
                            });
                        },
                    ))
                    .item(
                        PopupMenuItem::new("Optimize images")
                            .disabled(no_images)
                            .on_click(move |_, window, cx| {
                                let _ = optimize.update(cx, |workspace, cx| {
                                    workspace.optimize_images(window, cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Copy recovery").on_click(move |_, _, cx| {
                            let _ = recovery
                                .update(cx, |workspace, cx| workspace.copy_draft_recovery(cx));
                        }),
                    )
                }),
        );
    let switch_reason = state.switch_harness_reason_for(id).or_else(|| {
        (state.selected_thread != Some(id)).then(|| "Select this conversation first.".to_owned())
    });
    let switch = owner.clone();
    leading = leading.child(
        Button::new("switch-harness")
            .small()
            .ghost()
            .label(crate::harness::display_name(&thread.harness).to_owned())
            .disabled(switch_reason.is_some())
            .map(|button| match switch_reason {
                Some(reason) => button.tooltip(reason),
                None => button.tooltip_with_action(
                    Command::SwitchHarness.label(),
                    &SwitchHarness,
                    Some("Workspace"),
                ),
            })
            .on_click(move |_, window, cx| {
                let _ = switch.update(cx, |workspace, cx| {
                    workspace.execute(Command::SwitchHarness, window, cx)
                });
            }),
    );
    leading = leading.child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(statuses.join(" · ")),
    );
    if let Some(session) = state.conversations.get(&id) {
        trailing = trailing.children(usage_meters(
            session.context_usage,
            &session.limits,
            SystemTime::now(),
            cx,
        ));
    }
    if active {
        trailing = trailing.children(activity::mark(
            "turn-running".into(),
            ThreadActivity::Running,
            cx,
        ));
    }
    if let Some(workspace) = owner.upgrade() {
        let launch_settings_reason = state.remote_launch_settings_reason(id);
        if let Some(picker) = workspace
            .read(cx)
            .model_picker
            .as_ref()
            .filter(|picker| picker.thread_id == id && !picker.options.is_empty())
        {
            trailing = trailing.child(
                div()
                    .min_w_0()
                    .max_w(rems(20.))
                    .key_context("NativeInput")
                    .child(
                        Select::new(&picker.select)
                            .small()
                            .appearance(false)
                            .disabled(active)
                            .placeholder("Select model"),
                    ),
            );
        }
        if let Some(picker) = workspace
            .read(cx)
            .effort_picker
            .as_ref()
            .filter(|picker| picker.thread_id == id)
        {
            trailing = trailing.child(
                div()
                    .id("conversation-effort")
                    .when_some(launch_settings_reason.clone(), |view, reason| {
                        view.tooltip(move |window, cx| {
                            Tooltip::new(reason.clone()).build(window, cx)
                        })
                    })
                    .min_w_0()
                    .max_w(rems(10.))
                    .key_context("NativeInput")
                    .child(
                        Select::new(&picker.select)
                            .small()
                            .appearance(false)
                            .disabled(active || launch_settings_reason.is_some())
                            .placeholder("Effort"),
                    ),
            );
        }
        if let Some(picker) = workspace
            .read(cx)
            .permission_mode_picker
            .as_ref()
            .filter(|picker| picker.thread_id == id)
        {
            trailing = trailing.child(
                div()
                    .id("conversation-permissions")
                    .when_some(launch_settings_reason.clone(), |view, reason| {
                        view.tooltip(move |window, cx| {
                            Tooltip::new(reason.clone()).build(window, cx)
                        })
                    })
                    .min_w_0()
                    .max_w(rems(12.))
                    .key_context("NativeInput")
                    .child(
                        Select::new(&picker.select)
                            .small()
                            .appearance(false)
                            .disabled(active || launch_settings_reason.is_some())
                            .placeholder("Permissions"),
                    ),
            );
        }
    }
    let unavailable = state
        .conversations
        .get(&id)
        .is_none_or(|session| session.error.is_some());
    if unavailable {
        trailing = trailing.child(
            button(
                "retry-model-discovery",
                Command::RetryConversation,
                owner,
                state,
            )
            .label(if thread.remote.is_some() {
                "Reconnect Host"
            } else {
                "Models unavailable · Retry"
            })
            .disabled(
                state.selected_thread != Some(id)
                    || Command::RetryConversation.disabled_reason(state).is_some(),
            ),
        );
    }
    if active {
        let queue = owner.clone();
        let queue_reason = state.queue_reason(id).or_else(|| {
            (!hosts_editor).then(|| "Focus this conversation to edit and queue".to_owned())
        });
        trailing = trailing.child(
            Button::new("queue-prompt")
                .small()
                .ghost()
                .icon(Lucide::ListPlus)
                .label("Queue")
                .disabled(queue_reason.is_some())
                .tooltip(queue_reason.unwrap_or_else(|| "Queue for after this turn".to_owned()))
                .on_click(move |_, window, cx| {
                    let _ =
                        queue.update(cx, |workspace, cx| workspace.queue_prompt(id, window, cx));
                }),
        );
        let stop = owner.clone();
        trailing = trailing.child(
            Button::new("interrupt-prompt")
                .small()
                .danger()
                .outline()
                .icon(Lucide::Square)
                .label("Stop")
                .tooltip("Interrupt turn")
                .on_click(move |_, _, cx| {
                    let _ = stop.update(cx, |workspace, cx| {
                        workspace.state.update(cx, |state, cx| {
                            state.interrupt(id);
                            cx.notify();
                        });
                    });
                }),
        );
    } else {
        let send = owner.clone();
        let reason = state.submit_reason(id).or_else(|| {
            (!hosts_editor).then(|| "Focus this conversation to edit and send".to_owned())
        });
        trailing = trailing.child(
            Button::new("send-prompt")
                .small()
                .primary()
                .icon(Lucide::ArrowUp)
                .disabled(reason.is_some())
                .tooltip(reason.unwrap_or_else(|| "Send".to_owned()))
                .on_click(move |_, window, cx| {
                    let _ = send.update(cx, |workspace, cx| workspace.submit_prompt(window, cx));
                }),
        );
    }
    let controls = h_flex()
        .flex_shrink_0()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .child(leading)
        .child(trailing);
    let preferred_height =
        13. + if thread.attachments.is_empty() {
            0.
        } else {
            9.
        } + queued_messages_height(thread, owner, cx);
    frame(
        layout,
        root,
        history,
        sidebar,
        Some(super::composer::ComposerSplit {
            id,
            owner: owner.clone(),
            height: state.thread_composer_height(id),
            preferred_rems: preferred_height,
            history: div().into_any_element(),
            composer: h_flex()
                .id("composer-row")
                .debug_selector(|| "composer-row".into())
                .test_support()
                .w_full()
                .flex_shrink_0()
                .justify_center()
                .h_full()
                .min_h_0()
                .relative()
                .border_t_1()
                .border_color(cx.theme().border)
                .pt_2()
                .pb_2()
                .when(active, |row| {
                    row.child(
                        div()
                            .debug_selector(|| "composer-sweep".into())
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(rems(-0.0625))
                            .child(activity::sweep(cx)),
                    )
                })
                .child(
                    content_column().h_full().min_h_0().child(
                        v_flex()
                            .id("composer-content")
                            .debug_selector(|| "composer-content".into())
                            .test_support()
                            .h_full()
                            .min_h_0()
                            .child(composer)
                            .child(controls)
                            .child(workspace_controls(thread, state, owner, cx)),
                    ),
                )
                .into_any_element(),
        }),
    )
}

struct WorkspaceEntry {
    worktree: Option<uuid::Uuid>,
    label: String,
    current: bool,
}

fn workspace_entries(state: &AppState, thread: ThreadId) -> Vec<WorkspaceEntry> {
    let Some(thread) = state.snapshot.threads.iter().find(|t| t.id == thread) else {
        return Vec::new();
    };
    let checkout = match state.repositories.get(&thread.project_id) {
        Some(Ok(repository)) => match &repository.branch {
            Some(branch) => format!("Current checkout · {branch}"),
            None => "Current checkout · detached".to_owned(),
        },
        _ => "Current checkout".to_owned(),
    };
    let mut entries = vec![WorkspaceEntry {
        worktree: None,
        label: checkout,
        current: thread.worktree_id.is_none(),
    }];
    entries.extend(
        state
            .snapshot
            .worktrees
            .iter()
            .filter(|worktree| worktree.project_id == thread.project_id && worktree.is_live())
            .map(|worktree| WorkspaceEntry {
                worktree: Some(worktree.id),
                label: format!(
                    "{} · {}",
                    worktree.codename,
                    state.worktree_branch_label(worktree)
                ),
                current: thread.worktree_id == Some(worktree.id),
            }),
    );
    entries
}

fn workspace_menu(
    mut menu: gpui_kit::component::menu::PopupMenu,
    owner: &WeakEntity<Workspace>,
    thread: ThreadId,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
) -> gpui_kit::component::menu::PopupMenu {
    let Some(workspace) = owner.upgrade() else {
        return menu;
    };
    let state = workspace.read(cx).state.read(cx);
    let entries = workspace_entries(state, thread);
    let blocked = state
        .worktree_request_pending()
        .then_some("A worktree operation is in progress");
    menu = menu.label("New conversation in");
    for entry in &entries {
        let target = entry.worktree;
        let owner = owner.clone();
        menu = menu.item(
            PopupMenuItem::new(match blocked {
                Some(reason) if !entry.current => format!("{} — {reason}", entry.label),
                _ => entry.label.clone(),
            })
            .checked(entry.current)
            .disabled(entry.current || blocked.is_some())
            .on_click(move |_, window, cx| {
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.open_conversation_in(target, window, cx)
                });
            }),
        );
    }
    menu = menu.separator();
    for command in [Command::NewConversationInWorktree, Command::SwitchBranch] {
        let reason = command.disabled_reason(state);
        menu = menu.item(
            PopupMenuItem::new(match &reason {
                Some(reason) => format!("{} — {reason}", command.label()),
                None => command.label().to_owned(),
            })
            .action(command.action())
            .disabled(reason.is_some()),
        );
    }
    let removable: Vec<(uuid::Uuid, String, Option<String>)> = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.worktree?;
            Some((
                id,
                entry.label.clone(),
                state.remove_worktree_reason_for(id),
            ))
        })
        .collect();
    if removable.is_empty() {
        return menu;
    }
    let owner = owner.clone();
    menu.separator()
        .submenu("Remove worktree", window, cx, move |mut menu, _, _| {
            for (id, label, reason) in &removable {
                let id = *id;
                let owner = owner.clone();
                menu = menu.item(
                    PopupMenuItem::new(match reason {
                        Some(reason) => format!("{label}… — {reason}"),
                        None => format!("{label}…"),
                    })
                    .disabled(reason.is_some())
                    .on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |workspace, cx| {
                            workspace.confirm_remove_worktree(id, window, cx)
                        });
                    }),
                );
            }
            menu
        })
}

fn workspace_controls(
    thread: &crate::model::Thread,
    state: &AppState,
    owner: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    if let Some(location) = &thread.remote {
        return h_flex()
            .id("composer-remote-workspace")
            .test_support()
            .min_w_0()
            .gap_2()
            .px_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Tag::secondary()
                    .small()
                    .child("Host workspace · read-only here"),
            )
            .child(gpui_kit::component::label::Label::new(
                location.project.clone(),
            ))
            .into_any_element();
    }
    let selected = state.selected_thread == Some(thread.id);
    let worktree = state.thread_worktree(thread);
    let label = match worktree {
        Some(worktree) if worktree.is_live() => worktree.codename.clone(),
        Some(worktree) => format!("{} (removed)", worktree.codename),
        None => "Current checkout".to_owned(),
    };
    let path = state.working_tree_of(thread).map(|(_, path)| path);
    let tooltip = match path {
        Some(path) => format!(
            "{}\nStart a conversation in another worktree or manage worktrees; this conversation stays here",
            path.display()
        ),
        None => "Project directory unavailable".into(),
    };
    let branch = match worktree {
        Some(worktree) => state.worktree_branch_label(worktree),
        None => match state.repositories.get(&thread.project_id) {
            Some(Ok(repository)) => repository
                .branch
                .clone()
                .unwrap_or_else(|| "Detached HEAD".into()),
            Some(Err(_)) => "Branch unavailable".into(),
            None => "Reading branch…".into(),
        },
    };
    let menu_owner = owner.clone();
    let thread_id = thread.id;
    let switching = state.worktree_request.as_ref().is_some_and(|request| {
        request.status == crate::app::WorktreeRequestStatus::Pending
            && matches!(
                request.kind,
                crate::app::WorktreeRequestKind::Switch { tree, .. }
                    if state.working_tree_of(thread).map(|(own, _)| own) == Some(tree)
            )
    });
    h_flex()
        .id("composer-workspace")
        .flex_shrink_0()
        .min_w_0()
        .items_center()
        .gap_2()
        .px_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .text_color(cx.theme().muted_foreground)
        .child(
            h_flex().flex_1().min_w_0().overflow_hidden().child(
                Button::new("composer-workspace-picker")
                    .small()
                    .ghost()
                    .icon(Lucide::Folder)
                    .label(label)
                    .dropdown_caret(true)
                    .disabled(!selected)
                    .tooltip(if selected {
                        tooltip
                    } else {
                        "Select this conversation first".into()
                    })
                    .dropdown_menu(move |menu, window, cx| {
                        workspace_menu(menu, &menu_owner, thread_id, window, cx)
                    }),
            ),
        )
        .child(
            div()
                .min_w_0()
                .max_w(relative(0.5))
                .overflow_hidden()
                .font_family(cx.theme().mono_font_family.clone())
                .child(
                    button(
                        "composer-branch-picker",
                        Command::SwitchBranch,
                        owner,
                        state,
                    )
                    .label(branch)
                    .icon(Lucide::GitBranch)
                    .dropdown_caret(true)
                    .loading(switching)
                    .disabled(!selected || Command::SwitchBranch.disabled_reason(state).is_some())
                    .map(|button| {
                        if !selected {
                            button.tooltip("Select this conversation first")
                        } else if Command::SwitchBranch.disabled_reason(state).is_none() {
                            button.tooltip_with_action(
                                Command::SwitchBranch.label(),
                                &crate::ui::commands::SwitchBranch,
                                Some("Workspace"),
                            )
                        } else {
                            button
                        }
                    }),
                ),
        )
        .into_any_element()
}
