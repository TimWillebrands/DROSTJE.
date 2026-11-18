use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneView {
    pub desktop: String,
    pub spaces: Vec<SpaceView>,
    pub threads: BTreeMap<Uuid, ThreadView>,
    pub transcript: Option<Transcript>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<PromptReceipt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptReceipt {
    pub id: Uuid,
    pub outcome: PromptOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptOutcome {
    Started,
    Refused(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Prompting {
    #[default]
    Unsupported,
    Open,
    Closed(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeView {
    pub appearance: Appearance,
    pub light: serde_json::Value,
    pub dark: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceView {
    pub id: Uuid,
    pub name: String,
    pub projects: Vec<ProjectView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectView {
    pub id: Uuid,
    pub name: String,
    pub threads: Vec<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadView {
    pub id: Uuid,
    pub title: String,
    pub kind: ThreadKind,
    pub status: ThreadStatus,
    pub unseen: bool,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadKind {
    Conversation { harness: String },
    Factory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadStatus {
    Idle,
    Starting,
    Running,
    Waiting,
    Stopping,
    Failed,
    Disconnected,
    Unresolved,
}

impl ThreadStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Waiting => "Waiting for input",
            Self::Stopping => "Stopping",
            Self::Failed => "Failed",
            Self::Disconnected => "Disconnected",
            Self::Unresolved => "Unresolved",
        }
    }

    pub fn needs_attention(self) -> bool {
        match self {
            Self::Waiting | Self::Failed | Self::Disconnected | Self::Unresolved => true,
            Self::Idle | Self::Starting | Self::Running | Self::Stopping => false,
        }
    }

    pub fn busy(self) -> bool {
        match self {
            Self::Starting | Self::Running | Self::Stopping => true,
            Self::Idle | Self::Waiting | Self::Failed | Self::Disconnected | Self::Unresolved => {
                false
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    pub thread: Uuid,
    pub earlier: bool,
    pub items: Vec<TranscriptItem>,
    #[serde(default)]
    pub prompting: Prompting,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptItem {
    pub id: String,
    pub kind: ItemKind,
    pub text: String,
    pub clipped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    User,
    Assistant,
    Thinking,
    Tool { name: String, state: ToolState },
    Notice,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolState {
    Running,
    Done,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Patch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spaces: Option<Vec<SpaceView>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<ThreadView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<TranscriptPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipts: Option<Vec<PromptReceipt>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptPatch {
    Cleared,
    Changed {
        thread: Uuid,
        earlier: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        order: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        items: Vec<TranscriptItem>,
        #[serde(default)]
        prompting: Prompting,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PatchError {
    #[error("Transcript patch names an item the phone does not have")]
    MissingItem,
    #[error("Transcript patch is for a Thread the phone is not showing")]
    OtherThread,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.desktop.is_none()
            && self.spaces.is_none()
            && self.threads.is_empty()
            && self.removed.is_empty()
            && self.transcript.is_none()
            && self.theme.is_none()
            && self.receipts.is_none()
    }
}

fn transcript_patch(old: Option<&Transcript>, new: Option<&Transcript>) -> Option<TranscriptPatch> {
    match (old, new) {
        (None, None) => None,
        (Some(_), None) => Some(TranscriptPatch::Cleared),
        (old, Some(new)) => {
            let same_thread = old.filter(|old| old.thread == new.thread);
            let previous: BTreeMap<&str, &TranscriptItem> = same_thread
                .map(|old| {
                    old.items
                        .iter()
                        .map(|item| (item.id.as_str(), item))
                        .collect()
                })
                .unwrap_or_default();
            let order_changed = same_thread.is_none_or(|old| {
                old.items.len() != new.items.len()
                    || old.items.iter().zip(&new.items).any(|(a, b)| a.id != b.id)
            });
            let items: Vec<TranscriptItem> = new
                .items
                .iter()
                .filter(|item| previous.get(item.id.as_str()) != Some(item))
                .cloned()
                .collect();
            let header_changed = same_thread
                .is_none_or(|old| old.earlier != new.earlier || old.prompting != new.prompting);
            if !order_changed && items.is_empty() && !header_changed {
                return None;
            }
            Some(TranscriptPatch::Changed {
                thread: new.thread,
                earlier: new.earlier,
                order: order_changed
                    .then(|| new.items.iter().map(|item| item.id.clone()).collect()),
                items,
                prompting: new.prompting.clone(),
            })
        }
    }
}

impl PhoneView {
    pub fn diff(&self, new: &PhoneView) -> Patch {
        Patch {
            desktop: (self.desktop != new.desktop).then(|| new.desktop.clone()),
            spaces: (self.spaces != new.spaces).then(|| new.spaces.clone()),
            threads: new
                .threads
                .values()
                .filter(|thread| self.threads.get(&thread.id) != Some(thread))
                .cloned()
                .collect(),
            removed: self
                .threads
                .keys()
                .filter(|id| !new.threads.contains_key(id))
                .copied()
                .collect(),
            transcript: transcript_patch(self.transcript.as_ref(), new.transcript.as_ref()),
            theme: new
                .theme
                .clone()
                .filter(|theme| self.theme.as_ref() != Some(theme)),
            receipts: (self.receipts != new.receipts).then(|| new.receipts.clone()),
        }
    }

    pub fn apply(&mut self, patch: Patch) -> Result<(), PatchError> {
        let transcript = match patch.transcript {
            None => self.transcript.take(),
            Some(TranscriptPatch::Cleared) => None,
            Some(TranscriptPatch::Changed {
                thread,
                earlier,
                order,
                items,
                prompting,
            }) => {
                let current = self.transcript.as_ref().filter(|old| old.thread == thread);
                let mut known: BTreeMap<String, TranscriptItem> = current
                    .map(|old| {
                        old.items
                            .iter()
                            .map(|item| (item.id.clone(), item.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let order = match order {
                    Some(order) => order,
                    None => current
                        .ok_or(PatchError::OtherThread)?
                        .items
                        .iter()
                        .map(|item| item.id.clone())
                        .collect(),
                };
                for item in items {
                    known.insert(item.id.clone(), item);
                }
                let items = order
                    .into_iter()
                    .map(|id| known.remove(&id).ok_or(PatchError::MissingItem))
                    .collect::<Result<Vec<_>, _>>()?;
                Some(Transcript {
                    thread,
                    earlier,
                    items,
                    prompting,
                })
            }
        };
        if let Some(desktop) = patch.desktop {
            self.desktop = desktop;
        }
        if let Some(spaces) = patch.spaces {
            self.spaces = spaces;
        }
        for thread in patch.threads {
            self.threads.insert(thread.id, thread);
        }
        for id in patch.removed {
            self.threads.remove(&id);
        }
        self.transcript = transcript;
        if let Some(theme) = patch.theme {
            self.theme = Some(theme);
        }
        if let Some(receipts) = patch.receipts {
            self.receipts = receipts;
        }
        Ok(())
    }

    pub fn space(&self, id: Uuid) -> Option<&SpaceView> {
        self.spaces.iter().find(|space| space.id == id)
    }

    pub fn space_threads(&self, space: &SpaceView) -> impl Iterator<Item = &ThreadView> {
        space
            .projects
            .iter()
            .flat_map(|project| project.threads.iter())
            .filter_map(|id| self.threads.get(id))
    }

    pub fn thread_space(&self, thread: Uuid) -> Option<&SpaceView> {
        self.spaces.iter().find(|space| {
            space
                .projects
                .iter()
                .any(|project| project.threads.contains(&thread))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn thread(n: u128, status: ThreadStatus) -> ThreadView {
        ThreadView {
            id: id(n),
            title: format!("Thread {n}"),
            kind: ThreadKind::Conversation {
                harness: "claude-code".into(),
            },
            status,
            unseen: false,
            updated_at: "2026-11-17T10:00:00Z".into(),
        }
    }

    fn item(id: &str, text: &str) -> TranscriptItem {
        TranscriptItem {
            id: id.into(),
            kind: ItemKind::Assistant,
            text: text.into(),
            clipped: false,
        }
    }

    fn view() -> PhoneView {
        PhoneView {
            desktop: "Mac".into(),
            spaces: vec![SpaceView {
                id: id(100),
                name: "Default".into(),
                projects: vec![ProjectView {
                    id: id(200),
                    name: "drost".into(),
                    threads: vec![id(1), id(2)],
                }],
            }],
            threads: [
                thread(1, ThreadStatus::Idle),
                thread(2, ThreadStatus::Running),
            ]
            .into_iter()
            .map(|thread| (thread.id, thread))
            .collect(),
            transcript: Some(Transcript {
                thread: id(1),
                earlier: false,
                items: vec![item("a", "one"), item("b", "two")],
                prompting: Prompting::Open,
            }),
            theme: Some(theme(Appearance::System, "#2453a6")),
            receipts: Vec::new(),
        }
    }

    fn theme(appearance: Appearance, primary: &str) -> ThemeView {
        ThemeView {
            appearance,
            light: serde_json::json!({"name": "Delft Light", "colors": {"primary.background": primary}}),
            dark: serde_json::json!({"name": "Delft Dark", "grain": 0.25}),
        }
    }

    fn roundtrip(old: &PhoneView, new: &PhoneView) -> Patch {
        let patch = old.diff(new);
        let wire: Patch = serde_json::from_str(&serde_json::to_string(&patch).unwrap()).unwrap();
        let mut applied = old.clone();
        applied.apply(wire).unwrap();
        assert_eq!(&applied, new);
        patch
    }

    #[test]
    fn a_full_view_is_the_patch_from_nothing() {
        let patch = roundtrip(&PhoneView::default(), &view());
        assert_eq!(patch.threads.len(), 2);
        assert!(patch.spaces.is_some());
    }

    #[test]
    fn unchanged_views_produce_an_empty_patch() {
        assert!(view().diff(&view()).is_empty());
        assert_eq!(serde_json::to_string(&Patch::default()).unwrap(), "{}");
    }

    #[test]
    fn a_status_change_sends_only_that_thread() {
        let old = view();
        let mut new = view();
        new.threads.get_mut(&id(2)).unwrap().status = ThreadStatus::Waiting;
        let patch = roundtrip(&old, &new);
        assert_eq!(patch.threads, vec![new.threads[&id(2)].clone()]);
        assert!(patch.spaces.is_none() && patch.transcript.is_none());
    }

    #[test]
    fn removed_threads_disappear() {
        let old = view();
        let mut new = view();
        new.threads.remove(&id(2));
        new.spaces[0].projects[0]
            .threads
            .retain(|thread| *thread != id(2));
        assert_eq!(roundtrip(&old, &new).removed, vec![id(2)]);
    }

    #[test]
    fn streaming_text_sends_only_the_growing_item() {
        let old = view();
        let mut new = view();
        new.transcript.as_mut().unwrap().items[1].text = "two and more".into();
        let patch = roundtrip(&old, &new);
        let Some(TranscriptPatch::Changed { order, items, .. }) = patch.transcript else {
            panic!("expected a transcript change")
        };
        assert!(order.is_none());
        assert_eq!(items, vec![item("b", "two and more")]);
    }

    #[test]
    fn appended_and_scrolled_out_items_resend_the_order_only_with_new_items() {
        let old = view();
        let mut new = view();
        let transcript = new.transcript.as_mut().unwrap();
        transcript.items.remove(0);
        transcript.items.push(item("c", "three"));
        transcript.earlier = true;
        let patch = roundtrip(&old, &new);
        let Some(TranscriptPatch::Changed { order, items, .. }) = patch.transcript else {
            panic!("expected a transcript change")
        };
        assert_eq!(order, Some(vec!["b".into(), "c".into()]));
        assert_eq!(items, vec![item("c", "three")]);
    }

    #[test]
    fn switching_or_closing_the_transcript_is_complete() {
        let old = view();
        let mut other = view();
        other.transcript = Some(Transcript {
            thread: id(2),
            earlier: false,
            items: vec![item("a", "one")],
            prompting: Prompting::Closed("Busy".into()),
        });
        roundtrip(&old, &other);
        let mut closed = view();
        closed.transcript = None;
        assert_eq!(
            roundtrip(&old, &closed).transcript,
            Some(TranscriptPatch::Cleared)
        );
    }

    #[test]
    fn a_patch_for_items_the_phone_lacks_is_refused_so_it_resyncs() {
        let mut phone = PhoneView::default();
        let patch = Patch {
            transcript: Some(TranscriptPatch::Changed {
                thread: id(1),
                earlier: false,
                order: Some(vec!["a".into()]),
                items: vec![],
                prompting: Prompting::Open,
            }),
            ..Patch::default()
        };
        assert_eq!(phone.apply(patch), Err(PatchError::MissingItem));
        let orderless = Patch {
            transcript: Some(TranscriptPatch::Changed {
                thread: id(1),
                earlier: false,
                order: None,
                items: vec![item("a", "x")],
                prompting: Prompting::Open,
            }),
            ..Patch::default()
        };
        assert_eq!(phone.apply(orderless), Err(PatchError::OtherThread));
    }

    #[test]
    fn the_desktop_theme_travels_once_and_again_only_when_it_changes() {
        assert!(roundtrip(&PhoneView::default(), &view()).theme.is_some());
        let mut retinted = view();
        retinted.theme = Some(theme(Appearance::Dark, "#3a7545"));
        let patch = roundtrip(&view(), &retinted);
        assert_eq!(patch.theme, retinted.theme);
        assert!(patch.spaces.is_none() && patch.threads.is_empty());
        assert!(view().diff(&view()).theme.is_none());
    }

    #[test]
    fn views_from_before_themes_still_load_and_keep_the_last_theme() {
        let mut old = serde_json::to_value(view()).unwrap();
        old.as_object_mut().unwrap().remove("theme");
        let cached: PhoneView = serde_json::from_value(old).unwrap();
        assert_eq!(cached.theme, None);
        let mut phone = view();
        let silent = Patch {
            desktop: Some("Mac".into()),
            ..Patch::default()
        };
        phone.apply(silent).unwrap();
        assert_eq!(
            phone.theme,
            view().theme,
            "a desktop without themes keeps the last one"
        );
    }

    #[test]
    fn closing_the_composer_sends_only_the_transcript_header() {
        let old = view();
        let mut busy = view();
        busy.transcript.as_mut().unwrap().prompting =
            Prompting::Closed("Wait for this Turn to finish".into());
        let patch = roundtrip(&old, &busy);
        let Some(TranscriptPatch::Changed { order, items, .. }) = patch.transcript else {
            panic!("expected a transcript change")
        };
        assert!(order.is_none() && items.is_empty());
    }

    #[test]
    fn prompt_receipts_travel_until_the_desktop_forgets_them() {
        let mut answered = view();
        answered.receipts = vec![
            PromptReceipt {
                id: id(7),
                outcome: PromptOutcome::Started,
            },
            PromptReceipt {
                id: id(8),
                outcome: PromptOutcome::Refused("Archived history is read-only".into()),
            },
        ];
        let patch = roundtrip(&view(), &answered);
        assert_eq!(patch.receipts.as_deref(), Some(&answered.receipts[..]));
        assert!(answered.diff(&answered).receipts.is_none());
        assert_eq!(roundtrip(&answered, &view()).receipts, Some(Vec::new()));
    }

    #[test]
    fn a_desktop_from_before_phone_prompts_leaves_prompting_unsupported() {
        let mut old = serde_json::to_value(view()).unwrap();
        old["transcript"]
            .as_object_mut()
            .unwrap()
            .remove("prompting");
        let cached: PhoneView = serde_json::from_value(old).unwrap();
        assert_eq!(cached.transcript.unwrap().prompting, Prompting::Unsupported);
        let patch = serde_json::json!({"transcript": {"Changed": {"thread": id(1), "earlier": false, "items": []}}});
        let mut phone = view();
        phone.apply(serde_json::from_value(patch).unwrap()).unwrap();
        assert_eq!(phone.transcript.unwrap().prompting, Prompting::Unsupported);
    }

    #[test]
    fn threads_are_found_through_their_space() {
        let view = view();
        let space = view.space(id(100)).unwrap();
        assert_eq!(view.space_threads(space).count(), 2);
        assert_eq!(view.thread_space(id(2)).unwrap().name, "Default");
        assert!(view.thread_space(id(9)).is_none());
    }
}
