use super::*;
use crate::factory::{self, FactorySnapshot, Liveness};

impl AppState {
    pub(super) fn receive_factory(&mut self, snapshot: FactorySnapshot) {
        let id = snapshot.launch.id;
        if matches!(snapshot.liveness, Liveness::OwnedRunning) {
            self.owned_launches.insert(id);
        } else {
            self.owned_launches.remove(&id);
        }
        if let Some(launch) = self
            .snapshot
            .launches
            .iter_mut()
            .find(|launch| launch.id == id)
        {
            *launch = snapshot.launch.clone();
        } else {
            self.snapshot.launches.push(snapshot.launch.clone());
        }
        self.factory_runs.insert(id, snapshot);
    }

    pub fn refresh_factory_runs(&mut self) {
        let Some(manager) = self.factory.clone() else {
            self.notice("Factory tracking is unavailable; see startup diagnostics");
            return;
        };
        let sender = self.events.clone();
        smol::spawn(async move {
            if let Err(error) = manager.rescan().await {
                let _ = sender
                    .send(AppEvent::Notice(format!(
                        "Factory refresh failed: {error:#}"
                    )))
                    .await;
            }
        })
        .detach();
    }

    pub fn select_factory_launch(&mut self, id: LaunchId, cx: &mut Context<Self>) {
        if let Some(project_id) = self
            .snapshot
            .launches
            .iter()
            .find(|launch| launch.id == id)
            .map(|launch| launch.project_id)
        {
            if self.selected_project != Some(project_id) {
                self.select_project(project_id);
            }
        }
        self.selected_launch = Some(id);
        self.set_setting("selected_launch", json!(id));
        self.factory_text = None;
        self.factory_text_path = None;
        self.factory_text_generation = Uuid::new_v4();
        cx.notify();
    }

    pub fn load_factory_text(
        &mut self,
        path: PathBuf,
        before: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let generation = Uuid::new_v4();
        self.factory_text_generation = generation;
        self.factory_text_path = Some(path.clone());
        self.factory_text = None;
        cx.spawn(async move |this, cx| {
            loop {
                let page = factory::read_text(path.clone(), before)
                    .await
                    .map_err(|error| format!("{error:#}"));
                let active = this
                    .update(cx, |state, cx| {
                        if state.shutting_down || state.factory_text_generation != generation {
                            return false;
                        }
                        state.factory_text = Some(page);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !active || before.is_some() {
                    break;
                }
                smol::Timer::after(std::time::Duration::from_millis(500)).await;
            }
        })
        .detach();
        cx.notify();
    }
}
