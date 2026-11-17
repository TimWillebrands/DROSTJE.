use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneView {
    pub desktop: String,
    pub spaces: Vec<SpaceView>,
    pub threads: BTreeMap<Uuid, ThreadView>,
    pub transcript: Option<Transcript>,
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
            let earlier_changed = same_thread.is_none_or(|old| old.earlier != new.earlier);
            if !order_changed && items.is_empty() && !earlier_changed {
                return None;
            }
            Some(TranscriptPatch::Changed {
                thread: new.thread,
                earlier: new.earlier,
                order: order_changed
                    .then(|| new.items.iter().map(|item| item.id.clone()).collect()),
                items,
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
            }),
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
            }),
            ..Patch::default()
        };
        assert_eq!(phone.apply(orderless), Err(PatchError::OtherThread));
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
