//! What a form shows from `AppState` beyond its own draft, so unrelated changes skip a render.
use super::DialogKind;
use crate::app::AppState;
use std::collections::BTreeMap;

pub(super) struct SurfacePresentation {
    pub(super) readonly: bool,
    project: Option<(String, bool)>,
    diagnostics: BTreeMap<String, String>,
}

impl SurfacePresentation {
    fn project<'a>(kind: &DialogKind, state: &'a AppState) -> Option<(&'a str, bool)> {
        let DialogKind::Settings(Some(id)) = kind else {
            return None;
        };
        state
            .snapshot
            .projects
            .iter()
            .find(|project| project.id == *id)
            .map(|project| (project.name.as_str(), project.archived))
    }

    pub(super) fn new(kind: &DialogKind, state: &AppState) -> Self {
        Self {
            readonly: state.storage_error.is_some(),
            project: Self::project(kind, state).map(|(name, archived)| (name.to_owned(), archived)),
            diagnostics: if matches!(kind, DialogKind::Settings(_)) {
                state.diagnostics.clone()
            } else {
                BTreeMap::new()
            },
        }
    }

    pub(super) fn matches(&self, kind: &DialogKind, state: &AppState) -> bool {
        self.readonly == state.storage_error.is_some()
            && self
                .project
                .as_ref()
                .map(|(name, archived)| (name.as_str(), *archived))
                == Self::project(kind, state)
            && (!matches!(kind, DialogKind::Settings(_)) || self.diagnostics == state.diagnostics)
    }
}
