use super::{
    DialogKind, SurfaceContent,
    appearance::{ThemeNames, theme_choices},
    diagnostics::diagnostics_group,
    fields::{Choice, Controls, field_error},
    title_models::{
        LLAMA_MODEL_REFERENCE, chosen_llama_model, llama_model_choices, ollama_model_choices,
    },
};
use crate::ui::theme;
use crate::{app::AppState, harness, model::ModelKey, titles};
use gpui_kit::{
    component::{
        ActiveTheme, Sizable, ThemeMode,
        alert::Alert,
        setting::{SettingGroup, SettingItem, SettingPage, Settings},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use uuid::Uuid;

pub(super) fn retain_concurrent_value(initial: &str, draft: String, current: &str) -> String {
    if draft == initial && current != initial {
        current.to_owned()
    } else {
        draft
    }
}

fn target_branch_choices(branches: Option<&[String]>, selected: &str) -> Vec<Choice> {
    let mut choices = vec![Choice::new("", "Current branch when the thread starts")];
    choices.extend(
        branches
            .into_iter()
            .flatten()
            .map(|branch| Choice::new(branch.clone(), branch.clone())),
    );
    if !choices.iter().any(|choice| choice.value == selected) {
        choices.push(Choice {
            value: selected.to_owned(),
            label: if branches.is_some() {
                format!("{selected} (not a local branch)").into()
            } else {
                selected.to_owned().into()
            },
            unavailable: branches.is_some(),
        });
    }
    choices
}

pub(super) fn settings_controls(
    project: &Option<Uuid>,
    theme_names: &ThemeNames,
    state: &mut AppState,
    controls: &mut Controls,
    window: &mut Window,
    cx: &mut App,
) {
    for (key, label) in [
        ("remote_host_id", "Host ID"),
        ("remote_host_name", "Host name"),
        ("remote_relay_url", "SEINTJE relay URL (wss://…)"),
        ("remote_token_env", "Credential environment variable name"),
        (
            "remote_project_key",
            "Host project key for the selected Project",
        ),
    ] {
        controls.field(key, label, String::new(), window, cx);
    }
    for (key, label) in [
        ("nvim", "Neovim executable (blank: discover on PATH)"),
        ("omp", "OMP executable (blank: discover on PATH)"),
        ("pi", "Pi executable (blank: discover on PATH)"),
        ("claude", "Claude Code executable (blank: discover on PATH)"),
        ("codex", "Codex executable (blank: discover on PATH)"),
        ("opencode", "OpenCode executable (blank: discover on PATH)"),
        (
            "llama",
            "llama.cpp executable (blank: discover automatically)",
        ),
        ("factory", "Factory executable (blank: discover on PATH)"),
        (
            "azdo",
            "azdo executable for Azure DevOps (blank: discover on PATH)",
        ),
    ] {
        controls.field(
            key,
            label,
            state
                .snapshot
                .settings
                .get("executables")
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
            window,
            cx,
        );
    }
    controls.field(
        "factory_data_root",
        "Shared Factory data root (absolute path)",
        state
            .snapshot
            .settings
            .get("factory_data_root")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .into(),
        window,
        cx,
    );
    controls.field(
        LLAMA_MODEL_REFERENCE,
        "Hugging Face model (owner/repository:quantisation)",
        String::new(),
        window,
        cx,
    );
    if let Some(project) = project.and_then(|id| {
        state
            .snapshot
            .projects
            .iter()
            .find(|p| p.id == id && !p.archived)
    }) {
        controls.field(
            "provider",
            "Interactive default provider (blank: harness default)",
            project
                .defaults
                .interactive_model
                .as_ref()
                .map(|m| m.provider.clone())
                .unwrap_or_default(),
            window,
            cx,
        );
        controls.field(
            "model",
            "Interactive default model ID",
            project
                .defaults
                .interactive_model
                .as_ref()
                .map(|m| m.id.clone())
                .unwrap_or_default(),
            window,
            cx,
        );
        controls.field(
            "factory_model",
            "Factory model (blank: harness default)",
            project.defaults.factory_model.clone().unwrap_or_default(),
            window,
            cx,
        );
        controls.field(
            "factory_thought_level",
            "Factory thought level (optional)",
            project
                .defaults
                .factory_thought_level
                .clone()
                .unwrap_or_default(),
            window,
            cx,
        );
        let target_branch = project
            .defaults
            .factory_target_branch
            .clone()
            .unwrap_or_default();
        let branches = state
            .repositories
            .get(&project.id)
            .and_then(|repository| repository.as_ref().ok())
            .map(|repository| repository.branches.as_slice());
        controls.select(
            "factory_target_branch",
            "Factory target branch",
            target_branch_choices(branches, &target_branch),
            &target_branch,
            window,
            cx,
        );
    }
    controls.field(
        crate::app::PHONE_SERVER_SETTING,
        "Server (https://…)",
        state.phone_server(),
        window,
        cx,
    );
    controls.field(
        crate::app::PHONE_TOKEN_ENV_SETTING,
        "Token environment variable name (optional)",
        state.phone_token_env(),
        window,
        cx,
    );
    controls.select(
        "phone",
        "Paired phone",
        super::phones::phone_choices(state),
        "",
        window,
        cx,
    );
    let hosts = state.remote_hosts();
    controls.select(
        "remote_host",
        "Configured Host",
        std::iter::once(Choice::new("", "Add a Host…"))
            .chain(
                hosts.iter().map(|host| {
                    Choice::new(host.id.clone(), format!("{} ({})", host.name, host.id))
                }),
            )
            .collect(),
        "",
        window,
        cx,
    );

    let title_provider = state
        .snapshot
        .settings
        .get(titles::PROVIDER_SETTING)
        .and_then(|value| value.as_str())
        .filter(|value| ["automatic", "llama_cpp", "ollama"].contains(value))
        .unwrap_or("automatic");
    controls.select(
        titles::PROVIDER_SETTING,
        "Title generator",
        vec![
            Choice::new("automatic", "Automatic (llama.cpp, then Ollama)"),
            Choice::new("llama_cpp", "Llama.cpp"),
            Choice::new("ollama", "Ollama"),
        ],
        title_provider,
        window,
        cx,
    );
    if !state.title_models_loading() {
        state.refresh_title_models();
    }
    let installed = state.title_models.as_ref();
    let saved = |key| {
        state
            .snapshot
            .settings
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let llama_model = saved(titles::LLAMA_MODEL_SETTING);
    controls.select(
        titles::LLAMA_MODEL_SETTING,
        "llama.cpp model",
        llama_model_choices(installed.map(|models| &models.llama), &llama_model),
        &llama_model,
        window,
        cx,
    );
    let ollama_model = saved(titles::MODEL_SETTING);
    controls.select(
        titles::MODEL_SETTING,
        "Ollama model",
        ollama_model_choices(installed.map(|models| &models.ollama), &ollama_model),
        &ollama_model,
        window,
        cx,
    );
    let mode = state
        .snapshot
        .settings
        .get("theme_mode")
        .and_then(|value| value.as_str())
        .filter(|mode| ["system", "light", "dark"].contains(mode))
        .unwrap_or("system");
    controls.select(
        "theme_mode",
        "Appearance",
        vec![
            Choice::new("system", "System"),
            Choice::new("light", "Light"),
            Choice::new("dark", "Dark"),
        ],
        mode,
        window,
        cx,
    );
    for (key, label, mode) in [
        ("theme_light", "Light theme", ThemeMode::Light),
        ("theme_dark", "Dark theme", ThemeMode::Dark),
    ] {
        let selected = state
            .snapshot
            .settings
            .get(key)
            .and_then(|value| value.as_str())
            .map_or("", theme::saved_theme_name);
        controls.select(
            key,
            label,
            theme_choices(theme_names.of(mode), selected),
            selected,
            window,
            cx,
        );
    }
    if let Some(project) = project.and_then(|id| {
        state
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == id && !project.archived)
    }) {
        let selected = &project.defaults.interactive_harness;
        let mut choices = harness::CONVERSATION_HARNESSES
            .into_iter()
            .map(|name| Choice::new(name, name))
            .collect::<Vec<_>>();
        if !choices.iter().any(|choice| &choice.value == selected) {
            choices.push(Choice {
                value: selected.clone(),
                label: format!("{selected} (unsupported)").into(),
                unavailable: true,
            });
        }
        controls.select(
            "interactive_harness",
            "Conversation harness",
            choices,
            selected,
            window,
            cx,
        );
        let selected = &project.defaults.factory_harness;
        let mut choices = ["opencode", "codex", "claude-code"]
            .into_iter()
            .map(|name| Choice::new(name, name))
            .collect::<Vec<_>>();
        if !choices.iter().any(|choice| &choice.value == selected) {
            choices.push(Choice {
                value: selected.clone(),
                label: format!("{selected} (unsupported)").into(),
                unavailable: true,
            });
        }
        controls.select("factory_harness", "Harness", choices, selected, window, cx);
    }
}

impl SurfaceContent {
    pub(super) fn apply_settings(
        &self,
        project_id: Option<Uuid>,
        state: &mut AppState,
        cx: &App,
    ) -> anyhow::Result<()> {
        let llama_model = chosen_llama_model(
            self.value(titles::LLAMA_MODEL_SETTING, cx),
            self.value(LLAMA_MODEL_REFERENCE, cx),
        )?;
        let root = self.value("factory_data_root", cx);
        if !PathBuf::from(&root).is_absolute() {
            return Err(field_error(
                "factory_data_root",
                "Factory data root must be an absolute path.",
            ));
        }
        let mut project = project_id.and_then(|id| {
            state
                .snapshot
                .projects
                .iter()
                .find(|p| p.id == id && !p.archived)
                .cloned()
        });
        if let Some(project) = &mut project {
            let interactive_harness = self.value("interactive_harness", cx);
            if !harness::CONVERSATION_HARNESSES.contains(&interactive_harness.as_str()) {
                return Err(field_error(
                    "interactive_harness",
                    "Choose omp, pi, or claude-code.",
                ));
            }
            let provider = self.value("provider", cx);
            let model = self.value("model", cx);
            if provider.is_empty() != model.is_empty() {
                return Err(field_error(
                    if provider.is_empty() {
                        "provider"
                    } else {
                        "model"
                    },
                    "Set both interactive provider and model ID, or leave both blank.",
                ));
            }
            let harness = self.value("factory_harness", cx);
            if !["opencode", "codex", "claude-code"].contains(&harness.as_str()) {
                return Err(field_error(
                    "factory_harness",
                    "Choose opencode, codex, or claude-code.",
                ));
            }
            project.defaults.interactive_model = if model.is_empty() {
                None
            } else {
                Some(ModelKey {
                    provider,
                    id: model,
                })
            };
            project.defaults.interactive_harness = interactive_harness;
            project.defaults.factory_harness = harness;
            let optional = |key| {
                let value = self.value(key, cx);
                if value.is_empty() { None } else { Some(value) }
            };
            project.defaults.factory_model = optional("factory_model");
            project.defaults.factory_thought_level = optional("factory_thought_level");
            project.defaults.factory_target_branch = optional("factory_target_branch");
        }
        let mode = self.value("theme_mode", cx);
        let light = self.value("theme_light", cx);
        let dark = self.value("theme_dark", cx);
        let named = |value: String| (!value.is_empty()).then_some(value);
        let light = named(light);
        let dark = named(dark);
        theme::validate_preferences(&mode, None, None, cx)
            .map_err(|error| field_error("theme_mode", format!("{error:#}")))?;
        theme::validate_preferences(&mode, light.as_deref(), None, cx)
            .map_err(|error| field_error("theme_light", format!("{error:#}")))?;
        theme::validate_preferences(&mode, None, dark.as_deref(), cx)
            .map_err(|error| field_error("theme_dark", format!("{error:#}")))?;
        if let Some(host) = self.remote_host_config(state, project_id, cx)? {
            state.save_remote_host(host)?;
        }
        state
            .save_phone_server(
                &self.value(crate::app::PHONE_SERVER_SETTING, cx),
                &self.value(crate::app::PHONE_TOKEN_ENV_SETTING, cx),
            )
            .map_err(|error| field_error(crate::app::PHONE_SERVER_SETTING, format!("{error:#}")))?;
        let pi = {
            let value = self.value("pi", cx);
            let initial = &self
                .fields
                .iter()
                .find(|field| field.key == "pi")
                .expect("Pi settings field")
                .initial;
            let current = state
                .snapshot
                .settings
                .get("executables")
                .and_then(|value| value.get("pi"))
                .and_then(Value::as_str)
                .unwrap_or("");
            retain_concurrent_value(initial, value, current)
        };
        state.set_setting(
            "executables",
            json!({
                "nvim": self.value("nvim", cx),
                "omp": self.value("omp", cx),
                "pi": pi,
                "claude": self.value("claude", cx),
                "codex": self.value("codex", cx),
                "opencode": self.value("opencode", cx),
                "llama": self.value("llama", cx),
                "factory": self.value("factory", cx),
                "azdo": self.value("azdo", cx),
            }),
        );
        state.set_setting("factory_data_root", json!(root));
        state.set_setting(titles::LLAMA_MODEL_SETTING, json!(llama_model));
        state.set_setting(
            titles::MODEL_SETTING,
            json!(self.value(titles::MODEL_SETTING, cx)),
        );
        state.set_setting(
            titles::PROVIDER_SETTING,
            json!(self.value(titles::PROVIDER_SETTING, cx)),
        );
        state.set_setting("theme_mode", json!(mode));
        state.set_setting("theme_light", json!(light));
        state.set_setting("theme_dark", json!(dark));
        if let Some(project) = project {
            state.save_project(project);
        }
        state.refresh_diagnostics();
        state.refresh_title_models();
        Ok(())
    }

    fn field_description(&self, key: &str, state: &AppState) -> Option<String> {
        let installed = state.title_models.as_ref();
        let count = |models: &[String], noun: &str| match models.len() {
            1 => format!("1 {noun}"),
            n => format!("{n} {noun}s"),
        };
        match key {
            titles::LLAMA_MODEL_SETTING => Some(match installed.map(|models| &models.llama) {
                None => "Looking for downloaded models…".into(),
                Some(Ok(models)) if models.is_empty() => {
                    "The llama.cpp cache is empty. The chosen model downloads on first use.".into()
                }
                Some(Ok(models)) => format!(
                    "{} in the llama.cpp cache. Other models download on first use.",
                    count(models, "model")
                ),
                Some(Err(error)) => error.clone(),
            }),
            titles::MODEL_SETTING => {
                let host = titles::ollama_host();
                Some(match installed.map(|models| &models.ollama) {
                    None => "Looking for installed models…".into(),
                    Some(Ok(models)) if models.is_empty() => {
                        format!("Ollama at {host} has no models. Pull one, then refresh models.")
                    }
                    Some(Ok(models)) => {
                        format!("{} installed at {host}", count(models, "model"))
                    }
                    Some(Err(error)) => format!("{error}. Start Ollama, then refresh models."),
                })
            }
            LLAMA_MODEL_REFERENCE => Some("Downloads from Hugging Face on first use.".into()),
            "factory_target_branch" => {
                let DialogKind::Settings(Some(project)) = self.kind else {
                    return None;
                };
                match state.repositories.get(&project) {
                    Some(Err(error)) => Some(format!("Local branches are unavailable: {error}")),
                    None => Some("Reading local branches…".into()),
                    Some(Ok(_)) => None,
                }
            }
            _ => None,
        }
    }

    pub(super) fn group(
        &self,
        title: &'static str,
        keys: &[&'static str],
        readonly: bool,
        state: &AppState,
        owner: &WeakEntity<SurfaceContent>,
    ) -> SettingGroup {
        SettingGroup::new()
            .title(title)
            .items(keys.iter().filter_map(|key| {
                let item = self.setting_item(key, title, readonly, owner)?;
                Some(match self.field_description(key, state) {
                    Some(description) => item.description(description),
                    None => item,
                })
            }))
    }

    pub(super) fn settings_body(
        &self,
        project_id: Option<Uuid>,
        state: &AppState,
        owner: &WeakEntity<SurfaceContent>,
        cx: &App,
    ) -> AnyElement {
        let readonly = state.storage_error.is_some();
        let fields = |title: &'static str, keys: &[&'static str]| {
            self.group(title, keys, readonly, state, owner)
        };
        let appearance = self.appearance_group(state, readonly, owner);
        let tools = fields(
            "Tools",
            &[
                "nvim", "omp", "pi", "claude", "codex", "opencode", "llama", "factory", "azdo",
            ],
        )
        .description(
            "Tool installation and upgrades run only at your explicit request. DROSTJE. does not authenticate for you.",
        );
        let titles = self.titles_group(state, readonly, owner, cx);
        let factory = fields(
            "Factory defaults",
            &[
                "factory_data_root",
                "factory_harness",
                "factory_model",
                "factory_thought_level",
                "factory_target_branch",
            ],
        );
        let mut project = fields(
            "Project defaults",
            &["interactive_harness", "provider", "model"],
        );
        if let Some(selected) = project_id.and_then(|id| {
            state
                .snapshot
                .projects
                .iter()
                .find(|project| project.id == id)
        }) {
            project = project.description(format!(
                "{}{}",
                selected.name,
                if selected.archived {
                    " (archived, read-only)"
                } else {
                    ""
                }
            ));
        }
        if !self.fields.iter().any(|field| field.key == "provider") {
            project = project.item(
                SettingItem::render(|_, _, cx| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Select an active project to edit its conversation and Factory defaults.")
                })
                .keywords(["project", "defaults", "conversation", "factory", "archived"]),
            );
        }
        let hosts = self.hosts_group(project_id, state, readonly, owner, cx);
        let diagnostics = diagnostics_group(state, owner);
        let phones = self.phones_groups(state, readonly, owner);
        let settings = Settings::new(self.settings_id.clone())
            .page(
                SettingPage::new("Settings")
                    .default_open(true)
                    .resettable(false)
                    .groups([appearance, tools, titles, factory, project, diagnostics]),
            )
            .page(SettingPage::new("Hosts").resettable(false).groups([hosts]))
            .page(SettingPage::new("Phones").resettable(false).groups(phones));
        v_flex()
            .id("dialog-body")
            .key_context("NativeInput")
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .child(div().flex_1().min_h_0().min_w_0().child(settings))
            .when_some(self.error.clone(), |view, error| {
                view.child(Alert::error("settings-validation", error).small())
            })
            .when(readonly, |view| {
                view.child(Alert::error("settings-storage-error",
                    "Storage is not writable. Changes are disabled; existing drafts remain in memory.").small())
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{retain_concurrent_value, target_branch_choices};
    use crate::ui::settings::fields::shown;

    #[test]
    fn target_branch_choices_offer_local_branches() {
        let branches = ["main".to_owned(), "release".to_owned()];
        assert_eq!(
            shown(&target_branch_choices(Some(&branches), "gone")),
            [
                ("", "Current branch when the thread starts", false),
                ("main", "main", false),
                ("release", "release", false),
                ("gone", "gone (not a local branch)", true),
            ]
        );
        assert_eq!(target_branch_choices(Some(&branches), "main").len(), 3);
        assert_eq!(
            shown(&target_branch_choices(None, "main"))[1],
            ("main", "main", false)
        );
    }

    #[test]
    fn unchanged_settings_draft_preserves_completed_install_path() {
        assert_eq!(
            retain_concurrent_value(
                "",
                String::new(),
                "/managed/pi/lib/node_modules/pi/dist/cli.js"
            ),
            "/managed/pi/lib/node_modules/pi/dist/cli.js"
        );
        assert_eq!(
            retain_concurrent_value(
                "/old/pi",
                "/chosen/pi".into(),
                "/managed/pi/lib/node_modules/pi/dist/cli.js"
            ),
            "/chosen/pi"
        );
    }
}
