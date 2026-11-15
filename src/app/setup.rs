use super::*;

impl AppState {
    pub fn install_pi(&mut self) {
        if self.installing_pi || self.pi_check.is_some() {
            return;
        }
        if self.storage_error.is_some() {
            self.notice(
                NoticeLevel::Warning,
                "Storage is not writable; Pi's managed path cannot be saved.",
            );
            return;
        }
        let explicit = self
            .snapshot
            .settings
            .get("executables")
            .and_then(|value| value.get("pi"))
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .map(str::to_owned);
        // Resolving stats and canonicalizes PATH entries, so it runs off the GPUI thread.
        let request = Uuid::new_v4();
        self.pi_check = Some(request);
        let sender = self.events.clone();
        smol::spawn(async move {
            let found =
                smol::unblock(move || platform::resolve_executable("pi", explicit.as_deref()).ok())
                    .await;
            let _ = sender.send(AppEvent::PiChecked(request, found)).await;
        })
        .detach();
    }

    /// Finishes the Install Pi request `request`: reports a Pi that is already there, or
    /// starts the managed install. Results of any other request are dropped.
    pub(super) fn pi_checked(&mut self, request: Uuid, found: Option<PathBuf>) {
        if self.pi_check != Some(request) {
            return;
        }
        self.pi_check = None;
        if let Some(path) = found {
            self.notice(
                NoticeLevel::Info,
                format!("Pi is already available at {}", path.display()),
            );
            return;
        }
        // Storage can fail while the check runs; the managed path could not be saved then.
        if self.storage_error.is_some() {
            self.notice(
                NoticeLevel::Warning,
                "Storage is not writable; Pi's managed path cannot be saved.",
            );
            return;
        }
        self.installing_pi = true;
        self.diagnostics.insert("pi".into(), "Installing…".into());
        let sender = self.events.clone();
        let data_dir = self.data_dir.clone();
        smol::spawn(async move {
            let result = harness::install_pi(data_dir)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(AppEvent::PiInstalled(result)).await;
        })
        .detach();
    }

    /// Whether an inspection of installed title models is in flight.
    pub fn title_models_loading(&self) -> bool {
        self.title_models_request.is_some()
    }

    /// Lists installed title models in the background; earlier results stay visible meanwhile.
    pub fn refresh_title_models(&mut self) {
        let request = Uuid::new_v4();
        self.title_models_request = Some(request);
        let sender = self.events.clone();
        let llama = self
            .snapshot
            .settings
            .get("executables")
            .and_then(|value| value.get("llama"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        smol::spawn(async move {
            let models =
                smol::unblock(move || crate::titles::installed_models(llama.as_deref())).await;
            let _ = sender.send(AppEvent::TitleModels(request, models)).await;
        })
        .detach();
    }

    pub fn refresh_diagnostics(&self) {
        let sender = self.events.clone();
        let settings = self.snapshot.settings.clone();
        let projects: Vec<_> = self
            .snapshot
            .projects
            .iter()
            .map(|p| (p.id, p.root.clone()))
            .collect();
        smol::spawn(async move {
            let diagnostics = smol::unblock(move || executable_diagnostics(&settings)).await;
            if sender
                .send(AppEvent::Diagnostics(diagnostics))
                .await
                .is_err()
            {
                return;
            }
            for (id, root) in projects {
                let result = smol::unblock(move || {
                    platform::inspect_repository(&root).map_err(|e| format!("{e:#}"))
                })
                .await;
                if sender
                    .send(AppEvent::RepositoryChecked(id, result))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
}

pub(super) fn executable_diagnostics(
    settings: &BTreeMap<String, Value>,
) -> BTreeMap<String, String> {
    [
        "nvim", "omp", "pi", "claude", "codex", "opencode", "llama", "factory", "azdo",
    ]
    .into_iter()
    .map(|name| {
        let explicit = settings
            .get("executables")
            .and_then(|v| v.get(name))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let diagnostic = match platform::resolve_executable(name, explicit) {
            Ok(path) => format!("Resolved: {}", path.display()),
            Err(error) => format!("Unavailable: {error:#}"),
        };
        (name.into(), diagnostic)
    })
    .collect()
}

#[cfg(all(test, feature = "runtime-qa"))]
mod tests {
    use crate::app::{AppEvent, AppState, Bootstrap, NoticeLevel};
    use crate::store::Store;
    use gpui_kit::{AppContext as _, TestAppContext};
    use serde_json::json;
    use std::{collections::BTreeMap, fs, path::PathBuf};
    use uuid::Uuid;

    struct TempDirectory(PathBuf);
    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[gpui_kit::test]
    fn install_pi_looks_for_pi_off_the_gpui_thread_and_drops_stale_answers(
        cx: &mut TestAppContext,
    ) {
        // The look for Pi runs on a real blocking pool, as in production.
        cx.dispatcher.allow_parking();
        let dir =
            TempDirectory(std::env::temp_dir().join(format!("drostje-pi-{}", Uuid::new_v4())));
        fs::create_dir(&dir.0).unwrap();
        let store = Store::open(dir.0.join("data")).unwrap();
        let mut snapshot = smol::block_on(store.snapshot()).unwrap();
        // A real executable, so even the background answer never starts a managed install.
        let pi = dir.0.join("pi");
        fs::write(&pi, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&pi, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        snapshot
            .settings
            .insert("executables".into(), json!({ "pi": pi.to_string_lossy() }));
        let bootstrap = Bootstrap {
            data_dir: dir.0.join("data"),
            store,
            snapshot,
            diagnostics: BTreeMap::new(),
            repositories: BTreeMap::new(),
            browser_tools: None,
        };
        let state = cx.new(|cx| AppState::new(bootstrap, cx));
        state.update(cx, |state, cx| {
            let available = |state: &AppState| {
                state
                    .notices
                    .iter()
                    .filter(|notice| {
                        notice.level == NoticeLevel::Info
                            && notice.text.starts_with("Pi is already available at")
                    })
                    .count()
            };
            state.install_pi();
            let request = state.pi_check.expect("the look for Pi is in flight");
            assert_eq!(available(state), 0, "nothing was resolved on this thread");
            assert!(!state.installing_pi);

            // A second request while the first is in flight changes nothing.
            state.install_pi();
            assert_eq!(state.pi_check, Some(request));

            state.receive(AppEvent::PiChecked(Uuid::new_v4(), None), cx);
            assert_eq!(state.pi_check, Some(request), "another request's answer");
            assert!(!state.installing_pi);

            state.receive(AppEvent::PiChecked(request, Some(pi.clone())), cx);
            assert_eq!(state.pi_check, None);
            assert_eq!(available(state), 1);
            assert!(!state.installing_pi, "an existing Pi is not reinstalled");

            // The same answer arriving again is stale.
            state.receive(AppEvent::PiChecked(request, None), cx);
            assert!(!state.installing_pi);
            assert_eq!(available(state), 1);

            // Storage that failed during the look stops the managed install.
            state.install_pi();
            let request = state.pi_check.unwrap();
            state.storage_error = Some("read-only test store".into());
            state.receive(AppEvent::PiChecked(request, None), cx);
            assert!(!state.installing_pi);
            assert_eq!(state.pi_check, None);
            assert!(state.notices.back().is_some_and(|notice| {
                notice.level == NoticeLevel::Warning && notice.text.starts_with("Storage")
            }));
        });
    }
}
