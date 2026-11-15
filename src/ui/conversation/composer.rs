use crate::model::ThreadId;
use crate::ui::workspace::Workspace;
use gpui_kit::{
    component::{
        ElementExt,
        resizable::{ResizableState, resizable_panel, v_resizable},
    },
    *,
};

#[derive(IntoElement)]
pub(in crate::ui) struct ComposerSplit {
    pub id: ThreadId,
    pub owner: WeakEntity<Workspace>,
    pub height: Option<f32>,
    pub preferred_rems: f32,
    pub history: AnyElement,
    pub composer: AnyElement,
}

impl RenderOnce for ComposerSplit {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let key = SharedString::from(format!("composer-split-{}", self.id));
        let sizes = window.use_keyed_state(key.clone(), cx, |_, _| ResizableState::default());
        let geometry = window.use_keyed_state((key.clone(), 0_usize), cx, |_, _| None::<Pixels>);
        let available = geometry.read(cx).unwrap_or(window.viewport_size().height);
        let maximum = (available - px(80.)).max(px(128.)).min(px(1600.));
        if let Some(owner) = self.owner.upgrade() {
            owner.update(cx, |workspace, _| {
                if workspace.composer_sizes.get(&self.id) != Some(&sizes) {
                    workspace.composer_sizes.insert(self.id, sizes.clone());
                }
            });
        }
        let height = self
            .height
            .map(px)
            .unwrap_or(window.rem_size() * self.preferred_rems);
        let split = v_resizable(key)
            .with_state(&sizes)
            .child(
                resizable_panel()
                    .size_range(px(80.)..Pixels::MAX)
                    .child(self.history),
            )
            .child(
                resizable_panel()
                    .size(height.min(maximum))
                    .size_range(px(128.)..maximum)
                    .flex_none()
                    .child(self.composer),
            )
            .on_resize(move |sizes, _, cx| {
                let Some(height) = sizes.read(cx).sizes().get(1).copied() else {
                    return;
                };
                let owner = self.owner.clone();
                cx.defer(move |cx| {
                    let _ = owner.update(cx, |workspace, cx| {
                        workspace.state.update(cx, |state, _| {
                            state.set_thread_composer_height(self.id, f32::from(height));
                        });
                    });
                });
            });
        gpui_kit::component::v_flex()
            .size_full()
            .min_h_0()
            .on_prepaint(move |bounds, _, cx| {
                let height = bounds.size.height;
                if *geometry.read(cx) != Some(height) {
                    geometry.update(cx, |geometry, cx| {
                        *geometry = Some(height);
                        cx.notify();
                    });
                }
            })
            .child(split)
    }
}

impl Workspace {
    pub(in crate::ui) fn resize_composer(
        &mut self,
        delta: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self
            .state
            .read(cx)
            .conversation_thread()
            .map(|thread| thread.id)
        else {
            return;
        };
        let Some(sizes) = self.composer_sizes.get(&id).cloned() else {
            return;
        };
        let Some(height) = sizes.read(cx).sizes().get(1).copied() else {
            return;
        };
        sizes.update(cx, |sizes, cx| {
            sizes.resize_panel(1, height + px(delta), window, cx);
        });
        if let Some(height) = sizes.read(cx).sizes().get(1).copied() {
            self.state.update(cx, |state, _| {
                state.set_thread_composer_height(id, f32::from(height));
            });
        }
    }
}
