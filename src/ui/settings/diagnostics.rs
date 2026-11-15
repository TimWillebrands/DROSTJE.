//! The Diagnostics group: each tool's setup status, with its retry or install control.
use super::SurfaceContent;
use crate::app::AppState;
use crate::ui::commands;
use gpui_kit::{
    component::{
        Disableable, Sizable,
        button::Button,
        setting::{SettingField, SettingGroup, SettingItem},
    },
    *,
};

pub(super) fn diagnostics_group(
    state: &AppState,
    owner: &WeakEntity<SurfaceContent>,
) -> SettingGroup {
    let mut diagnostics = SettingGroup::new()
            .title("Diagnostics")
            .description(format!(
                "Application data: {}. Conversation harnesses run with local user privileges. Factory uses its configured sandbox policy.",
                state.data_dir.display()
            ));
    for (name, title) in [
        ("nvim", "Neovim"),
        ("omp", "OMP"),
        ("pi", "Pi"),
        ("claude", "Claude Code"),
        ("codex", "Codex"),
        ("opencode", "OpenCode"),
        ("llama", "llama.cpp"),
        ("factory", "Factory"),
        ("azdo", "Azure DevOps (azdo)"),
    ] {
        let status = state
            .diagnostics
            .get(name)
            .cloned()
            .unwrap_or_else(|| "Not inspected".into());
        // A resolved azdo can still be signed out; that is a setup issue too.
        let tracker_issue = (name == "azdo")
            .then(|| state.tracker_setup_issue())
            .flatten();
        let status = match &tracker_issue {
            Some(issue) => format!("{status}. {issue}"),
            None => status,
        };
        let install_pi =
            name == "pi" && (state.installing_pi || status.starts_with("Unavailable:"));
        let install_reason = install_pi
            .then(|| commands::Command::InstallPi.disabled_reason(state))
            .flatten();
        let owner = owner.clone();
        diagnostics = diagnostics.item(
            SettingItem::new(
                title,
                SettingField::render(move |_, _, _| {
                    if install_pi {
                        let tooltip = install_reason
                            .clone()
                            .unwrap_or_else(|| "Install Pi in DROSTJE.'s application data".into());
                        Button::new("install-pi")
                            .small()
                            .label(
                                if install_reason.as_deref()
                                    == Some("Pi installation is already running.")
                                {
                                    "Installing…"
                                } else {
                                    "Install Pi"
                                },
                            )
                            .disabled(install_reason.is_some())
                            .tooltip(tooltip)
                            .on_click(move |_, window, cx| {
                                window.dispatch_action(Box::new(commands::InstallPi), cx)
                            })
                    } else if tracker_issue.is_some() {
                        Button::new("refresh-pull-requests")
                            .small()
                            .label("Check again")
                            .on_click(move |_, window, cx| {
                                window.dispatch_action(Box::new(commands::RefreshPullRequests), cx)
                            })
                    } else {
                        let owner = owner.clone();
                        Button::new(SharedString::from(format!("refresh-diagnostics-{name}")))
                            .small()
                            .label("Refresh diagnostics")
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |surface, cx| {
                                    surface.state.read(cx).refresh_diagnostics()
                                });
                            })
                    }
                }),
            )
            .description(status)
            .keywords([name, "diagnostics", "setup", "retry"]),
        );
    }
    diagnostics
}
