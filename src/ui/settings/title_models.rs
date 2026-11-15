//! Conversation title generator settings: which settings the generator reads, the model
//! choices built from the installed models, and the "Conversation titles" group.
use super::{
    SurfaceContent,
    fields::{Choice, field_error},
};
use crate::{app::AppState, titles};
use gpui_kit::{
    component::{
        Disableable, Sizable,
        button::Button,
        setting::{SettingField, SettingGroup, SettingItem},
    },
    *,
};

/// Title model select value that reveals the Hugging Face reference field.
pub(super) const OTHER_LLAMA_MODEL: &str = "<other>";
/// Settings field for a llama.cpp model that is not in the cache yet.
pub(super) const LLAMA_MODEL_REFERENCE: &str = "llama_title_model_reference";

/// Conversation title settings the selected generator reads, in display order.
pub(super) fn title_setting_keys(provider: &str, llama_model: &str) -> Vec<&'static str> {
    let llama = provider != "ollama";
    let ollama = provider != "llama_cpp";
    let mut keys = vec![titles::PROVIDER_SETTING];
    if llama {
        keys.push(titles::LLAMA_MODEL_SETTING);
        if llama_model == OTHER_LLAMA_MODEL {
            keys.push(LLAMA_MODEL_REFERENCE);
        }
    }
    if ollama {
        keys.push(titles::MODEL_SETTING);
    }
    keys
}

/// The default, every cached model, the saved model when the cache lacks it (llama.cpp
/// downloads it on first use), and an entry for naming another model.
pub(super) fn llama_model_choices(
    cached: Option<&Result<Vec<String>, String>>,
    selected: &str,
) -> Vec<Choice> {
    let cached = cached.and_then(|cached| cached.as_ref().ok());
    let default = titles::DEFAULT_LLAMA_MODEL;
    let mut choices = vec![Choice::new(
        "",
        match cached {
            Some(cached) if !cached.iter().any(|model| model == default) => {
                format!("Default ({default}, not downloaded)")
            }
            _ => format!("Default ({default})"),
        },
    )];
    // The default is listed separately only when it was chosen explicitly.
    choices.extend(
        cached
            .into_iter()
            .flatten()
            .filter(|model| *model != default || selected == default)
            .map(|model| Choice::new(model.clone(), model.clone())),
    );
    if selected != OTHER_LLAMA_MODEL && !choices.iter().any(|choice| choice.value == selected) {
        choices.push(Choice::new(
            selected,
            if cached.is_some() {
                format!("{selected} (not downloaded)")
            } else {
                selected.to_owned()
            },
        ));
    }
    choices.push(Choice::new(OTHER_LLAMA_MODEL, "Other Hugging Face model…"));
    choices
}

/// The first-installed default, every installed model, and the saved model when Ollama is
/// known not to have it.
pub(super) fn ollama_model_choices(
    installed: Option<&Result<Vec<String>, String>>,
    selected: &str,
) -> Vec<Choice> {
    let installed = installed.and_then(|installed| installed.as_ref().ok());
    let mut choices = vec![Choice::new(
        "",
        match installed.and_then(|models| models.first()) {
            Some(first) => format!("First installed model ({first})"),
            None => "First installed model".to_owned(),
        },
    )];
    choices.extend(
        installed
            .into_iter()
            .flatten()
            .map(|model| Choice::new(model.clone(), model.clone())),
    );
    if !choices.iter().any(|choice| choice.value == selected) {
        choices.push(Choice {
            value: selected.to_owned(),
            label: if installed.is_some() {
                format!("{selected} (not installed)").into()
            } else {
                selected.to_owned().into()
            },
            unavailable: installed.is_some(),
        });
    }
    choices
}

/// The llama.cpp model to save: the selected entry, or the typed reference for another model.
pub(super) fn chosen_llama_model(selected: String, reference: String) -> anyhow::Result<String> {
    if selected != OTHER_LLAMA_MODEL {
        return Ok(selected);
    }
    if reference.is_empty() {
        return Err(field_error(
            LLAMA_MODEL_REFERENCE,
            format!(
                "Enter a Hugging Face model reference, such as {}.",
                titles::DEFAULT_LLAMA_MODEL
            ),
        ));
    }
    Ok(reference)
}

impl SurfaceContent {
    /// Rebuilds the title model choices when an inspection lands, keeping the draft selection.
    pub(super) fn refresh_title_model_choices(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let installed = &self.state.read(cx).title_models;
        if *installed == self.title_models {
            return;
        }
        self.title_models = installed.clone();
        for key in [titles::LLAMA_MODEL_SETTING, titles::MODEL_SETTING] {
            let Some(select) = self.select(key).cloned() else {
                continue;
            };
            let selected = self.value(key, cx);
            let models = self.title_models.as_ref();
            let choices = if key == titles::LLAMA_MODEL_SETTING {
                llama_model_choices(models.map(|models| &models.llama), &selected)
            } else {
                ollama_model_choices(models.map(|models| &models.ollama), &selected)
            };
            select.update(cx, |select, cx| {
                select.set_items(choices, window, cx);
                select.set_selected_value(&selected, window, cx);
                cx.notify();
            });
        }
        cx.notify();
    }

    pub(super) fn titles_group(
        &self,
        state: &AppState,
        readonly: bool,
        owner: &WeakEntity<SurfaceContent>,
        cx: &App,
    ) -> SettingGroup {
        let fields = |title: &'static str, keys: &[&'static str]| {
            self.group(title, keys, readonly, state, owner)
        };
        let provider = self.value(titles::PROVIDER_SETTING, cx);
        let loading = state.title_models_loading();
        let refresh = owner.clone();
        fields(
            "Conversation titles",
            &title_setting_keys(&provider, &self.value(titles::LLAMA_MODEL_SETTING, cx)),
        )
        .item(
            SettingItem::new(
                "Installed models",
                SettingField::render(move |options, _, _| {
                    let refresh = refresh.clone();
                    Button::new("refresh-title-models")
                        .small()
                        .label("Refresh models")
                        .loading(loading)
                        .disabled(loading || options.is_disabled())
                        .on_click(move |_, _, cx| {
                            let _ = refresh.update(cx, |surface, cx| {
                                surface.state.update(cx, |state, cx| {
                                    state.refresh_title_models();
                                    cx.notify();
                                })
                            });
                        })
                }),
            )
            .description(if loading {
                "Looking for downloaded and installed models…"
            } else {
                "Lists the llama.cpp cache and the models Ollama has installed."
            })
            .keywords(["titles", "models", "refresh", "llama", "ollama"]),
        )
        .description(match provider.as_str() {
            "llama_cpp" => "Runs the local llama.cpp CLI.".to_owned(),
            "ollama" => format!("Asks the Ollama server at {}.", titles::ollama_host()),
            _ => format!(
                "Runs the local llama.cpp CLI first and asks Ollama at {} when llama.cpp is unavailable.",
                titles::ollama_host()
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LLAMA_MODEL_REFERENCE, OTHER_LLAMA_MODEL, chosen_llama_model, llama_model_choices,
        ollama_model_choices, title_setting_keys,
    };
    use crate::titles;
    use crate::ui::settings::fields::{FieldError, shown};

    #[test]
    fn title_settings_follow_the_selected_generator() {
        use titles::{LLAMA_MODEL_SETTING as LLAMA, MODEL_SETTING as OLLAMA, PROVIDER_SETTING};
        assert_eq!(
            title_setting_keys("automatic", ""),
            [PROVIDER_SETTING, LLAMA, OLLAMA]
        );
        assert_eq!(
            title_setting_keys("llama_cpp", ""),
            [PROVIDER_SETTING, LLAMA]
        );
        assert_eq!(
            title_setting_keys("ollama", OTHER_LLAMA_MODEL),
            [PROVIDER_SETTING, OLLAMA]
        );
        assert_eq!(
            title_setting_keys("llama_cpp", OTHER_LLAMA_MODEL),
            [PROVIDER_SETTING, LLAMA, LLAMA_MODEL_REFERENCE]
        );
    }

    #[test]
    fn llama_choices_list_the_cache_and_keep_a_saved_download() {
        let default = titles::DEFAULT_LLAMA_MODEL;
        let missing_default = format!("Default ({default}, not downloaded)");
        let cached = Ok(vec!["org/a-GGUF:Q8_0".to_owned()]);
        assert_eq!(
            shown(&llama_model_choices(Some(&cached), "org/b-GGUF:Q4_0")),
            [
                ("", missing_default.as_str(), false),
                ("org/a-GGUF:Q8_0", "org/a-GGUF:Q8_0", false),
                ("org/b-GGUF:Q4_0", "org/b-GGUF:Q4_0 (not downloaded)", false),
                (OTHER_LLAMA_MODEL, "Other Hugging Face model…", false),
            ]
        );
        // Unknown cache: the saved model is listed without a claim about its download.
        assert_eq!(
            shown(&llama_model_choices(None, "org/b-GGUF:Q4_0"))[1],
            ("org/b-GGUF:Q4_0", "org/b-GGUF:Q4_0", false)
        );
        // A cached default appears once, unless it was chosen explicitly.
        let cached = Ok(vec![default.to_owned()]);
        assert_eq!(
            shown(&llama_model_choices(Some(&cached), "")),
            [
                ("", format!("Default ({default})").as_str(), false),
                (OTHER_LLAMA_MODEL, "Other Hugging Face model…", false),
            ]
        );
        assert_eq!(llama_model_choices(Some(&cached), default).len(), 3);
        let failed = Err("llama was not found".to_owned());
        assert_eq!(llama_model_choices(Some(&failed), "").len(), 2);
    }

    #[test]
    fn ollama_choices_mark_a_saved_model_ollama_lacks() {
        let installed = Ok(vec!["lfm2:24b".to_owned()]);
        assert_eq!(
            shown(&ollama_model_choices(Some(&installed), "gone:1b")),
            [
                ("", "First installed model (lfm2:24b)", false),
                ("lfm2:24b", "lfm2:24b", false),
                ("gone:1b", "gone:1b (not installed)", true),
            ]
        );
        let unreachable = Err("Ollama is not running".to_owned());
        assert_eq!(
            shown(&ollama_model_choices(Some(&unreachable), "gone:1b"))[1],
            ("gone:1b", "gone:1b", false)
        );
        assert_eq!(ollama_model_choices(None, "").len(), 1);
    }

    #[test]
    fn other_llama_model_requires_a_reference() {
        assert_eq!(
            chosen_llama_model("org/a:Q8_0".into(), "ignored".into()).unwrap(),
            "org/a:Q8_0"
        );
        assert_eq!(
            chosen_llama_model(OTHER_LLAMA_MODEL.into(), "org/new:Q4_0".into()).unwrap(),
            "org/new:Q4_0"
        );
        let error = chosen_llama_model(OTHER_LLAMA_MODEL.into(), String::new()).unwrap_err();
        assert_eq!(
            error.downcast_ref::<FieldError>().map(|error| error.key),
            Some(LLAMA_MODEL_REFERENCE)
        );
    }
}
