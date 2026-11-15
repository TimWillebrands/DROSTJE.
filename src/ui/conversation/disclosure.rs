use super::transcript::TranscriptDisclosure;
use crate::{model::ThreadId, ui::workspace::Workspace};
use gpui_kit::{
    assets::IconName as Lucide,
    component::{
        Sizable,
        button::{Button, ButtonVariants},
    },
    *,
};

pub(super) fn toggle(
    owner: &WeakEntity<Workspace>,
    thread: Option<ThreadId>,
    key: String,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + use<> {
    let owner = owner.clone();
    move |_, _, cx| {
        cx.stop_propagation();
        let _ = owner.update(cx, |workspace, cx| {
            if !workspace.expanded.remove(&key) {
                workspace.expanded.insert(key.clone());
            }
            match thread {
                Some(thread) => cx.emit(TranscriptDisclosure { thread }),
                None => cx.notify(),
            }
        });
    }
}
pub(super) fn is_expanded(owner: &WeakEntity<Workspace>, key: &str, cx: &App) -> bool {
    owner
        .upgrade()
        .is_some_and(|workspace| workspace.read(cx).expanded.contains(key))
}
pub(super) fn disclosure(
    id: String,
    expanded: bool,
    owner: &WeakEntity<Workspace>,
    thread: Option<ThreadId>,
    tooltip: &str,
) -> Button {
    Button::new(SharedString::from(id.clone()))
        .xsmall()
        .ghost()
        .cursor_default()
        .icon(if expanded {
            Lucide::ChevronDown
        } else {
            Lucide::ChevronRight
        })
        .tooltip(tooltip.to_owned())
        .on_click(toggle(owner, thread, id))
}
