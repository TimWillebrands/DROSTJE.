use super::{DialogKind, SurfaceContent, fields::Choice};
use crate::app::AppState;
use crate::ui::commands;
use gpui_kit::{
    component::{
        Disableable,
        button::Button,
        setting::{SettingField, SettingGroup, SettingItem},
    },
    prelude::FluentBuilder,
    *,
};
use uuid::Uuid;

impl SurfaceContent {
    pub(super) fn load_remote_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.value("remote_host", cx);
        if selected == self.loaded_remote_host {
            return;
        }
        if self
            .fields
            .iter()
            .filter(|field| field.key.starts_with("remote_"))
            .any(|field| field.input.read(cx).value().trim() != field.initial)
        {
            if let Some(select) = self.select("remote_host") {
                select.update(cx, |select, cx| {
                    select.set_selected_value(&self.loaded_remote_host, window, cx)
                });
            }
            self.error =
                Some("Save this Host before choosing another. Your input has been kept.".into());
            self.error_field = None;
            cx.notify();
            return;
        }
        self.loaded_remote_host = selected.clone();
        let host = self
            .state
            .read(cx)
            .remote_hosts()
            .into_iter()
            .find(|host| host.id == selected);
        let project = match self.kind {
            DialogKind::Settings(project) => project.map(|id| id.to_string()),
            _ => None,
        };
        for (key, value) in [
            ("remote_host_id", host.as_ref().map(|host| host.id.clone())),
            (
                "remote_host_name",
                host.as_ref().map(|host| host.name.clone()),
            ),
            (
                "remote_relay_url",
                host.as_ref().map(|host| host.relay_url.clone()),
            ),
            (
                "remote_token_env",
                host.as_ref().map(|host| host.token_env.clone()),
            ),
            (
                "remote_project_key",
                host.as_ref().and_then(|host| {
                    project
                        .as_ref()
                        .and_then(|project| host.projects.get(project).cloned())
                }),
            ),
        ] {
            if let Some(field) = self.fields.iter_mut().find(|field| field.key == key) {
                let value = value.unwrap_or_default();
                field.initial = value.clone();
                field
                    .input
                    .update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
        cx.notify();
    }

    pub(in crate::ui) fn remote_host_removed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hosts = self.state.read(cx).remote_hosts();
        if let Some(select) = self.select("remote_host") {
            select.update(cx, |select, cx| {
                select.set_items(
                    std::iter::once(Choice::new("", "Add a Host…"))
                        .chain(hosts.into_iter().map(|host| {
                            Choice::new(host.id.clone(), format!("{} ({})", host.name, host.id))
                        }))
                        .collect(),
                    window,
                    cx,
                );
                select.set_selected_value(&String::new(), window, cx);
            });
        }
        self.loaded_remote_host.clear();
        for field in self
            .fields
            .iter_mut()
            .filter(|field| field.key.starts_with("remote_"))
        {
            field.initial.clear();
            field
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.error = None;
        self.error_field = None;
        cx.notify();
    }

    pub(super) fn remote_host_config(
        &self,
        state: &AppState,
        project: Option<Uuid>,
        cx: &App,
    ) -> anyhow::Result<Option<crate::remote::RemoteHostConfig>> {
        let id = self.value("remote_host_id", cx);
        let name = self.value("remote_host_name", cx);
        let relay_url = self.value("remote_relay_url", cx);
        let token_env = self.value("remote_token_env", cx);
        let project_key = self.value("remote_project_key", cx);
        if [&id, &name, &relay_url, &token_env, &project_key]
            .iter()
            .all(|value| value.is_empty())
        {
            return Ok(None);
        }
        let selected = self.value("remote_host", cx);
        anyhow::ensure!(
            selected.is_empty() || selected == id,
            "Host ID is fixed when editing a configured Host. Choose Add a Host to use another ID."
        );
        let mut projects = state
            .remote_hosts()
            .into_iter()
            .find(|host| host.id == id)
            .map(|host| host.projects)
            .unwrap_or_default();
        if let Some(project) = project {
            if project_key.is_empty() {
                projects.remove(&project.to_string());
            } else {
                projects.insert(project.to_string(), project_key);
            }
        } else {
            anyhow::ensure!(
                project_key.is_empty(),
                "Select a local Project before configuring its Host project key."
            );
        }
        Ok(Some(crate::remote::RemoteHostConfig {
            id,
            name,
            relay_url,
            token_env,
            projects,
        }))
    }

    pub(super) fn hosts_group(
        &self,
        project_id: Option<Uuid>,
        state: &AppState,
        readonly: bool,
        owner: &WeakEntity<SurfaceContent>,
        cx: &App,
    ) -> SettingGroup {
        let fields = |title: &'static str, keys: &[&'static str]| {
            self.group(title, keys, readonly, state, owner)
        };
        let selected_host = self.value("remote_host", cx);
        let remove_reason = if selected_host.is_empty() {
            Some("Choose a configured Host first.".to_owned())
        } else {
            state.remove_remote_host_reason(&selected_host)
        };
        let remove_owner = self.owner.clone();
        fields(
            "Hosts",
            &["remote_host", "remote_host_id", "remote_host_name", "remote_relay_url", "remote_token_env", "remote_project_key"],
        )
        .item(SettingItem::new(
            "Remove Host configuration",
            SettingField::render(move |_, _, _| {
                let owner = remove_owner.clone();
                Button::new("remove-remote-host").label(commands::Command::RemoveRemoteHost.label())
                    .disabled(remove_reason.is_some())
                    .when_some(remove_reason.clone(), |button, reason| button.tooltip(reason))
                    .on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |workspace, cx| workspace.execute(commands::Command::RemoveRemoteHost, window, cx));
                    })
            }),
        ).description("Only unused Hosts can be removed. Conversations, including archived history, keep their Host configuration."))
        .description(format!(
            "{} Save before choosing another Host. Credentials are read from the named desktop environment variable, never stored here. Restart DROSTJE. after changing environment variables: the login-shell environment is captured at startup. The relay can read conversation content. Host paths and dependencies are configured on the Host; no files are synchronized or tools installed. Factory always runs locally.",
            match project_id.and_then(|id| state.snapshot.projects.iter().find(|project| project.id == id)) {
                Some(project) => format!("Map Project “{}” to a configured Host project key.", project.name),
                None => "Select a Project to configure its Host project-key mapping.".to_owned(),
            }
        ))
    }
}
