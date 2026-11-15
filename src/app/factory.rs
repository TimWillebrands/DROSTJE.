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
        if self.selected_thread == Some(snapshot.launch.factory_thread_id)
            && self.selected_launch != Some(id)
        {
            self.select_launch_output(Some(id));
        }
        self.factory_runs.insert(id, snapshot);
    }

    pub fn refresh_factory_runs(&mut self) {
        let Some(manager) = self.factory.clone() else {
            self.notice(
                NoticeLevel::Error,
                "Factory tracking is unavailable; see startup diagnostics",
            );
            return;
        };
        let sender = self.events.clone();
        smol::spawn(async move {
            if let Err(error) = manager.rescan().await {
                let _ = sender
                    .send(AppEvent::Notice(WorkspaceNotice {
                        level: NoticeLevel::Error,
                        text: format!("Factory refresh failed: {error:#}"),
                    }))
                    .await;
            }
        })
        .detach();
    }

    pub fn select_factory_launch(&mut self, id: LaunchId, cx: &mut Context<Self>) {
        let Some(owner) = self
            .snapshot
            .launches
            .iter()
            .find(|launch| launch.id == id)
            .map(|launch| launch.factory_thread_id)
            .filter(|owner| {
                self.snapshot
                    .threads
                    .iter()
                    .any(|thread| thread.id == *owner && thread.kind == ThreadKind::Factory)
            })
        else {
            return;
        };
        self.select_thread(owner);
        cx.notify();
    }

    pub(super) fn select_launch_output(&mut self, id: Option<LaunchId>) {
        self.selected_launch = id;
        self.factory_text = None;
        self.factory_text_path = None;
        self.factory_text_generation = Uuid::new_v4();
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
                        if store_text_page(&mut state.factory_text, page) {
                            cx.notify();
                        }
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

/// Store a freshly read page; false when it equals the one shown, so an unchanged tail
/// is neither replaced nor announced again.
fn store_text_page(
    shown: &mut Option<Result<Arc<factory::TextPage>, String>>,
    page: Result<factory::TextPage, String>,
) -> bool {
    let unchanged = match (shown.as_ref(), &page) {
        (Some(Ok(shown)), Ok(page)) => **shown == *page,
        (Some(Err(shown)), Err(page)) => shown == page,
        _ => false,
    };
    if !unchanged {
        *shown = Some(page.map(Arc::new));
    }
    !unchanged
}

#[cfg(test)]
mod tests {
    use super::store_text_page;
    use crate::factory::TextPage;
    use std::{path::PathBuf, sync::Arc};

    fn page(text: &str) -> TextPage {
        TextPage {
            path: PathBuf::from("stdout.log"),
            text: text.into(),
            start_offset: 0,
            end_offset: text.len() as u64,
            has_older: false,
        }
    }

    #[test]
    fn an_unchanged_tail_keeps_the_shown_page() {
        let mut shown = None;
        assert!(store_text_page(&mut shown, Ok(page("one\n"))));
        let Some(Ok(first)) = shown.clone() else {
            panic!("page stored");
        };
        assert!(!store_text_page(&mut shown, Ok(page("one\n"))));
        assert!(
            matches!(&shown, Some(Ok(kept)) if Arc::ptr_eq(kept, &first)),
            "the same page stays shared"
        );
        assert!(store_text_page(&mut shown, Ok(page("one\ntwo\n"))));
        assert!(store_text_page(&mut shown, Err("gone".into())));
        assert!(!store_text_page(&mut shown, Err("gone".into())));
        assert!(store_text_page(&mut shown, Ok(page("one\ntwo\n"))));
    }
}
