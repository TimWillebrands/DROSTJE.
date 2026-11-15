use crate::ui::workspace::Workspace;
use crate::{
    app::AppState,
    model::{ModelKey, ThreadId},
};
use gpui_kit::{
    component::{
        searchable_list::SearchableListItem,
        select::{SelectEvent, SelectState},
    },
    *,
};

#[derive(Clone, PartialEq)]
pub(super) struct ModelOption {
    key: ModelKey,
    label: SharedString,
    name: String,
}

impl SearchableListItem for ModelOption {
    type Value = ModelKey;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &ModelKey {
        &self.key
    }
}

pub(in crate::ui) struct ModelPicker {
    pub(super) thread_id: ThreadId,
    pub(super) options: Vec<ModelOption>,
    pub(super) selected: Option<ModelKey>,
    pub(super) select: Entity<SelectState<Vec<ModelOption>>>,
    _subscription: Subscription,
}

#[derive(Clone, PartialEq)]
pub(super) struct EffortOption {
    value: Option<String>,
    label: SharedString,
}

impl SearchableListItem for EffortOption {
    type Value = Option<String>;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Option<String> {
        &self.value
    }
}

pub(in crate::ui) struct EffortPicker {
    pub(super) thread_id: ThreadId,
    pub(super) options: Vec<EffortOption>,
    pub(super) selected: Option<String>,
    pub(super) select: Entity<SelectState<Vec<EffortOption>>>,
    _subscription: Subscription,
}

pub(in crate::ui) struct PermissionModePicker {
    pub(super) thread_id: ThreadId,
    pub(super) options: Vec<EffortOption>,
    pub(super) selected: Option<String>,
    pub(super) select: Entity<SelectState<Vec<EffortOption>>>,
    _subscription: Subscription,
}

fn effort_levels(state: &AppState, thread: &crate::model::Thread) -> Vec<String> {
    let Some(session) = state.conversations.get(&thread.id) else {
        return Vec::new();
    };
    let selected = session.model.as_ref().or(thread.model.as_ref());
    session
        .models
        .iter()
        .find(|model| Some(&model.key) == selected)
        .or_else(|| {
            session
                .models
                .iter()
                .find(|model| model.key.id == "default")
        })
        .or_else(|| {
            session
                .models
                .iter()
                .find(|model| !model.efforts.is_empty())
        })
        .map(|model| model.efforts.clone())
        .unwrap_or_default()
}

impl Workspace {
    pub(in crate::ui) fn sync_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(thread) = state.conversation_thread() else {
            self.model_picker = None;
            return;
        };
        let thread_id = thread.id;
        let session = state.conversations.get(&thread_id);
        let selected = session
            .and_then(|session| session.model.as_ref())
            .or(thread.model.as_ref());
        if self.model_picker.as_ref().is_some_and(|picker| {
            picker.thread_id == thread_id
                && picker.selected.as_ref() == selected
                && picker.options.len() == session.map_or(0, |session| session.models.len())
                && session
                    .into_iter()
                    .flat_map(|session| &session.models)
                    .zip(&picker.options)
                    .all(|(model, option)| model.key == option.key && model.name == option.name)
        }) {
            return;
        }
        let options: Vec<_> = session
            .into_iter()
            .flat_map(|session| &session.models)
            .map(|model| ModelOption {
                key: model.key.clone(),
                name: model.name.clone(),
                label: format!("{} · {}/{}", model.name, model.key.provider, model.key.id).into(),
            })
            .collect();
        let selected = selected.cloned();
        if let Some(picker) = self
            .model_picker
            .as_mut()
            .filter(|picker| picker.thread_id == thread_id)
        {
            if picker.options != options || picker.selected != selected {
                picker.select.update(cx, |select, cx| {
                    if picker.options != options {
                        select.set_items(options.clone(), window, cx);
                    }
                    if let Some(selected) = &selected {
                        select.set_selected_value(selected, window, cx);
                    } else {
                        select.set_selected_index(None, window, cx);
                    }
                });
                picker.options = options;
                picker.selected = selected;
            }
            return;
        }
        let select = cx.new(|cx| {
            let mut select = SelectState::new(options.clone(), None, window, cx);
            if let Some(selected) = &selected {
                select.set_selected_value(selected, window, cx);
            }
            select
        });
        let subscription = cx.subscribe_in(
            &select,
            window,
            move |workspace, _, event: &SelectEvent<Vec<ModelOption>>, _, cx| {
                let SelectEvent::Confirm(Some(key)) = event else {
                    return;
                };
                workspace.state.update(cx, |state, cx| {
                    state.select_model(thread_id, key.clone());
                    cx.notify();
                });
            },
        );
        self.model_picker = Some(ModelPicker {
            thread_id,
            options,
            selected,
            select,
            _subscription: subscription,
        });
    }

    pub(in crate::ui) fn sync_effort_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.read(cx);
        let Some(thread) = state.conversation_thread() else {
            self.effort_picker = None;
            return;
        };
        let thread_id = thread.id;
        let levels = effort_levels(state, thread);
        if levels.is_empty() {
            self.effort_picker = None;
            return;
        }
        let options: Vec<_> = std::iter::once(EffortOption {
            value: None,
            label: "Default effort".into(),
        })
        .chain(levels.into_iter().map(|level| EffortOption {
            label: level.clone().into(),
            value: Some(level),
        }))
        .collect();
        let selected = thread.effort.clone();
        if let Some(picker) = self
            .effort_picker
            .as_mut()
            .filter(|picker| picker.thread_id == thread_id)
        {
            if picker.options != options || picker.selected != selected {
                picker.select.update(cx, |select, cx| {
                    if picker.options != options {
                        select.set_items(options.clone(), window, cx);
                    }
                    select.set_selected_value(&selected, window, cx);
                });
                picker.options = options;
                picker.selected = selected;
            }
            return;
        }
        let select = cx.new(|cx| {
            let mut select = SelectState::new(options.clone(), None, window, cx);
            select.set_selected_value(&selected, window, cx);
            select
        });
        let subscription = cx.subscribe_in(
            &select,
            window,
            move |workspace, _, event: &SelectEvent<Vec<EffortOption>>, _, cx| {
                let SelectEvent::Confirm(Some(effort)) = event else {
                    return;
                };
                workspace.state.update(cx, |state, cx| {
                    state.select_effort(thread_id, effort.clone());
                    cx.notify();
                });
            },
        );
        self.effort_picker = Some(EffortPicker {
            thread_id,
            options,
            selected,
            select,
            _subscription: subscription,
        });
    }

    pub(in crate::ui) fn sync_permission_mode_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.read(cx);
        let Some(thread) = state.conversation_thread() else {
            self.permission_mode_picker = None;
            return;
        };
        let thread_id = thread.id;
        let modes = crate::harness::permission_modes(&thread.harness);
        if modes.is_empty() {
            self.permission_mode_picker = None;
            return;
        }
        let options: Vec<_> = std::iter::once(EffortOption {
            value: None,
            label: "Default permissions".into(),
        })
        .chain(modes.iter().map(|(value, label)| EffortOption {
            value: Some((*value).to_owned()),
            label: (*label).into(),
        }))
        .collect();
        let selected = thread.permission_mode.clone();
        if let Some(picker) = self
            .permission_mode_picker
            .as_mut()
            .filter(|picker| picker.thread_id == thread_id)
        {
            if picker.options != options || picker.selected != selected {
                picker.select.update(cx, |select, cx| {
                    if picker.options != options {
                        select.set_items(options.clone(), window, cx);
                    }
                    select.set_selected_value(&selected, window, cx);
                });
                picker.options = options;
                picker.selected = selected;
            }
            return;
        }
        let select = cx.new(|cx| {
            let mut select = SelectState::new(options.clone(), None, window, cx);
            select.set_selected_value(&selected, window, cx);
            select
        });
        let subscription = cx.subscribe_in(
            &select,
            window,
            move |workspace, _, event: &SelectEvent<Vec<EffortOption>>, _, cx| {
                let SelectEvent::Confirm(Some(mode)) = event else {
                    return;
                };
                workspace.state.update(cx, |state, cx| {
                    state.select_permission_mode(thread_id, mode.clone());
                    cx.notify();
                });
            },
        );
        self.permission_mode_picker = Some(PermissionModePicker {
            thread_id,
            options,
            selected,
            select,
            _subscription: subscription,
        });
    }

    pub(in crate::ui) fn toggle_thread_details(
        &mut self,
        id: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_details(id, String::new(), window, cx);
    }
}
