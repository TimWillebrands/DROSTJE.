//! The theme editor: appearance, accent and grain over whichever theme is in effect,
//! previewed live and saved together. Cancel, Escape and dismissal restore the saved
//! choices; nothing is written until Save.

use std::{cell::Cell, rc::Rc};

use gpui_kit::{
    base::ColorSwatch,
    component::{
        ActiveTheme, Disableable, IconName, Selectable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        kbd::Kbd,
        slider::{Slider, SliderEvent, SliderState, SliderValue},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use serde_json::json;

use crate::app::AppState;
use crate::ui::{
    theme::{self, Accent, Adjustments, Appearance, MAX_CHROMA, ThemeDraft, grain},
    workspace::Workspace,
};

gpui_kit::actions!(
    theme_editor,
    [
        StepLeft, StepRight, StepUp, StepDown, JumpLeft, JumpRight, JumpUp, JumpDown, First, Last,
    ]
);

const FIELD: &str = "ThemeAccentField";
const SWATCHES: &str = "ThemeSwatches";
const GRAIN: &str = "ThemeGrain";

/// Degrees per hue step; a jump is six steps.
const HUE_STEP: f32 = 5.;
/// Chroma per intensity step; a jump is five steps.
const CHROMA_STEP: f32 = 0.01;
/// Grain percent per step; a jump is ten steps.
const GRAIN_STEP: f32 = 1.;
/// Dots across and down the accent field.
const FIELD_COLUMNS: usize = 24;
const FIELD_ROWS: usize = 9;

/// Accents offered beside the theme's own, in OKLCH hue and chroma.
const PRESETS: [(&str, f32, f32); 8] = [
    ("Blue", 262., 0.14),
    ("Teal", 200., 0.1),
    ("Moss", 140., 0.11),
    ("Saffron", 75., 0.15),
    ("Terracotta", 40., 0.14),
    ("Rose", 0., 0.13),
    ("Plum", 320., 0.12),
    ("Graphite", 260., 0.015),
];

pub fn bind_keys(cx: &mut App) {
    let mut bindings = Vec::new();
    for context in [FIELD, SWATCHES, GRAIN] {
        let context = Some(context);
        bindings.extend([
            KeyBinding::new("h", StepLeft, context),
            KeyBinding::new("left", StepLeft, context),
            KeyBinding::new("l", StepRight, context),
            KeyBinding::new("right", StepRight, context),
            KeyBinding::new("shift-h", JumpLeft, context),
            KeyBinding::new("shift-left", JumpLeft, context),
            KeyBinding::new("shift-l", JumpRight, context),
            KeyBinding::new("shift-right", JumpRight, context),
        ]);
    }
    for context in [SWATCHES, GRAIN] {
        let context = Some(context);
        bindings.extend([
            KeyBinding::new("g g", First, context),
            KeyBinding::new("home", First, context),
            KeyBinding::new("shift-g", Last, context),
            KeyBinding::new("end", Last, context),
        ]);
    }
    let field = Some(FIELD);
    bindings.extend([
        KeyBinding::new("k", StepUp, field),
        KeyBinding::new("up", StepUp, field),
        KeyBinding::new("j", StepDown, field),
        KeyBinding::new("down", StepDown, field),
        KeyBinding::new("shift-k", JumpUp, field),
        KeyBinding::new("shift-up", JumpUp, field),
        KeyBinding::new("shift-j", JumpDown, field),
        KeyBinding::new("shift-down", JumpDown, field),
    ]);
    cx.bind_keys(bindings);
}

/// Which swatch shows `accent`: 0 is the theme's own, then [`PRESETS`] in order.
fn swatch_ix(accent: Option<Accent>) -> Option<usize> {
    match accent {
        None => Some(0),
        Some(accent) => PRESETS
            .iter()
            .position(|(_, hue, chroma)| Accent::new(*hue, *chroma) == accent)
            .map(|ix| ix + 1),
    }
}

fn swatch_accent(ix: usize) -> Option<Accent> {
    ix.checked_sub(1)
        .and_then(|ix| PRESETS.get(ix))
        .map(|(_, hue, chroma)| Accent::new(*hue, *chroma))
}

pub struct ThemeEditor {
    owner: WeakEntity<Workspace>,
    state: Entity<AppState>,
    draft: ThemeDraft,
    field: FocusHandle,
    swatches: FocusHandle,
    grain_focus: FocusHandle,
    grain: Entity<SliderState>,
    /// Where the accent field was last painted, for pointer positions.
    field_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    dragging: bool,
    /// Saved, cancelled, or dismissed; later callbacks from the dialog do nothing.
    closed: bool,
    _subscriptions: Vec<Subscription>,
}

impl ThemeEditor {
    pub fn open(
        owner: WeakEntity<Workspace>,
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let draft = theme::draft(cx);
        let grain_value = draft
            .adjustments
            .grain
            .unwrap_or_else(|| theme::theme_grain(cx));
        let editor = cx.new(|cx| {
            let grain = cx.new(|_| {
                SliderState::new()
                    .min(0.)
                    .max(100.)
                    .step(GRAIN_STEP)
                    .default_value(grain_value * 100.)
            });
            let subscriptions = vec![cx.subscribe_in(
                &grain,
                window,
                |this: &mut Self, _, event: &SliderEvent, window, cx| {
                    let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                    this.set_grain(value.start() / 100., window, cx);
                },
            )];
            Self {
                owner,
                state,
                draft,
                field: cx.focus_handle().tab_stop(true),
                swatches: cx.focus_handle().tab_stop(true),
                grain_focus: cx.focus_handle().tab_stop(true),
                grain,
                field_bounds: Rc::default(),
                dragging: false,
                closed: false,
                _subscriptions: subscriptions,
            }
        });
        let content = editor.clone();
        let ok_owner = editor.downgrade();
        let close_owner = editor.downgrade();
        let width =
            (window.rem_size() * 26.).min(window.viewport_size().width - window.rem_size() * 2.);
        window.open_dialog(cx, move |dialog, _, cx| {
            let ok_owner = ok_owner.clone();
            let close_owner = close_owner.clone();
            dialog
                .title(
                    h_flex().gap_2().child("Theme").child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(theme::active_name(cx)),
                    ),
                )
                .width(width)
                .child(content.clone())
                .footer(Self::footer(&content, cx))
                .on_ok(move |_, window, cx| {
                    let _ = ok_owner.update(cx, |editor, cx| editor.save(window, cx));
                    true
                })
                .on_close(move |_, window, cx| {
                    let _ = close_owner.update(cx, |editor, cx| editor.cleanup(window, cx));
                })
        });
        let focus_owner = editor.downgrade();
        window.on_next_frame(move |window, cx| {
            if let Some(editor) = focus_owner.upgrade() {
                let field = editor.read(cx).field.clone();
                field.focus(window, cx);
            }
        });
        editor
    }

    fn footer(editor: &Entity<Self>, cx: &App) -> impl IntoElement {
        let adjusted = editor.read(cx).draft.adjustments != Adjustments::default();
        let reset = editor.downgrade();
        let cancel = editor.downgrade();
        let save = editor.downgrade();
        h_flex()
            .w_full()
            .justify_between()
            .child(
                Button::new("theme-editor-reset")
                    .label("Reset")
                    .ghost()
                    .disabled(!adjusted)
                    .tooltip(if adjusted {
                        "Use the theme's own accent and grain"
                    } else {
                        "Already the theme's own accent and grain"
                    })
                    .on_click(move |_, window, cx| {
                        let _ = reset.update(cx, |editor, cx| {
                            let draft = ThemeDraft {
                                adjustments: Adjustments::default(),
                                ..editor.draft
                            };
                            editor.set_draft(draft, window, cx);
                        });
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("theme-editor-cancel")
                            .label("Cancel")
                            .ghost()
                            .on_click(move |_, window, cx| {
                                let _ = cancel.update(cx, |editor, cx| editor.dismiss(window, cx));
                            }),
                    )
                    .child(
                        Button::new("theme-editor-save")
                            .label("Save")
                            .primary()
                            .on_click(move |_, window, cx| {
                                let _ = save.update(cx, |editor, cx| editor.save(window, cx));
                                window.close_dialog(cx);
                            }),
                    ),
            )
    }

    fn set_draft(&mut self, draft: ThemeDraft, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed || draft == self.draft {
            return;
        }
        let grain_changed = draft.adjustments.grain != self.draft.adjustments.grain;
        self.draft = draft;
        theme::preview(draft, &self.state, window, cx);
        if grain_changed {
            let value = draft
                .adjustments
                .grain
                .unwrap_or_else(|| theme::theme_grain(cx));
            self.grain.update(cx, |slider, cx| {
                if slider.value() != SliderValue::Single(value * 100.) {
                    slider.set_value(value * 100., window, cx);
                }
            });
        }
        cx.notify();
    }

    fn set_accent(&mut self, accent: Option<Accent>, window: &mut Window, cx: &mut Context<Self>) {
        let mut draft = self.draft;
        draft.adjustments.accent = accent;
        self.set_draft(draft, window, cx);
    }

    fn set_grain(&mut self, grain: f32, window: &mut Window, cx: &mut Context<Self>) {
        let mut draft = self.draft;
        draft.adjustments.grain = Some(grain.clamp(0., 1.));
        self.set_draft(draft, window, cx);
    }

    fn accent(&self, cx: &App) -> Accent {
        self.draft
            .adjustments
            .accent
            .unwrap_or_else(|| theme::theme_accent(cx))
    }

    fn nudge_accent(&mut self, hue: f32, chroma: f32, window: &mut Window, cx: &mut Context<Self>) {
        let accent = self.accent(cx);
        let accent = Accent::new(accent.hue + hue, accent.chroma + chroma);
        self.set_accent(Some(accent), window, cx);
    }

    fn step_swatch(&mut self, to: SwatchStep, window: &mut Window, cx: &mut Context<Self>) {
        let last = PRESETS.len();
        let current = swatch_ix(self.draft.adjustments.accent);
        let ix = match (to, current) {
            (SwatchStep::First, _) => 0,
            (SwatchStep::Last, _) => last,
            (SwatchStep::By(delta), Some(ix)) => ix.saturating_add_signed(delta).min(last),
            // A custom accent steps onto the row from its start or end.
            (SwatchStep::By(delta), None) => {
                if delta < 0 {
                    last
                } else {
                    0
                }
            }
        };
        self.set_accent(swatch_accent(ix), window, cx);
    }

    fn step_grain(&mut self, percent: f32, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .draft
            .adjustments
            .grain
            .unwrap_or_else(|| theme::theme_grain(cx));
        let stepped = ((current * 100. + percent) / GRAIN_STEP).round() * GRAIN_STEP;
        self.set_grain(stepped / 100., window, cx);
    }

    fn set_from_pointer(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self.field_bounds.get() else {
            return;
        };
        let x = ((position.x - bounds.left()) / bounds.size.width).clamp(0., 1.);
        let y = ((position.y - bounds.top()) / bounds.size.height).clamp(0., 1.);
        self.set_accent(
            Some(Accent::new(x * 360., (1. - y) * MAX_CHROMA)),
            window,
            cx,
        );
    }

    /// Save the draft as the user's appearance and adjustments.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        let draft = self.draft;
        self.state.update(cx, |state, cx| {
            state.set_setting("theme_mode", json!(draft.appearance.setting()));
            state.set_setting(theme::ADJUSTMENTS_SETTING, draft.adjustments.to_setting());
            cx.notify();
        });
        theme::end_preview(&self.state, window, cx);
        self.release(cx);
    }

    /// Close without saving: Cancel, or another surface taking over.
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.cleanup(window, cx);
        window.close_dialog(cx);
    }

    /// The dialog closed itself (Escape, the close button, the overlay); restore the
    /// saved choices unless they were just saved.
    fn cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        theme::end_preview(&self.state, window, cx);
        self.release(cx);
    }

    fn release(&mut self, cx: &mut Context<Self>) {
        let id = cx.entity().entity_id();
        let _ = self.owner.update(cx, |workspace, cx| {
            if workspace
                .theme_editor
                .as_ref()
                .is_some_and(|editor| editor.entity_id() == id)
            {
                workspace.theme_editor = None;
                cx.notify();
            }
        });
    }

    fn region(focus: &FocusHandle, window: &Window, cx: &App) -> Div {
        div()
            .border_1()
            .rounded(cx.theme().radius_lg)
            .border_color(if focus.is_focused(window) {
                cx.theme().input
            } else {
                cx.theme().transparent
            })
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().gap_1().children(Appearance::ALL.map(|appearance| {
            let icon = match appearance {
                Appearance::System => None,
                Appearance::Light => Some(IconName::Sun),
                Appearance::Dark => Some(IconName::Moon),
            };
            Button::new(SharedString::from(format!(
                "theme-appearance-{}",
                appearance.setting()
            )))
            .label(appearance.label())
            .when_some(icon, |button, icon| button.icon(icon))
            .small()
            .ghost()
            .selected(self.draft.appearance == appearance)
            .on_click(cx.listener(move |this, _, window, cx| {
                let draft = ThemeDraft {
                    appearance,
                    ..this.draft
                };
                this.set_draft(draft, window, cx);
            }))
        }))
    }

    fn render_field(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = self.accent(cx);
        let handle = theme::accent_color(accent, cx);
        // One color per dot, computed here so painting stays a plain copy.
        let dots: Vec<Hsla> = (0..FIELD_ROWS)
            .flat_map(|row| (0..FIELD_COLUMNS).map(move |column| (row, column)))
            .map(|(row, column)| {
                let x = (column as f32 + 0.5) / FIELD_COLUMNS as f32;
                let y = (row as f32 + 0.5) / FIELD_ROWS as f32;
                theme::accent_color(Accent::new(x * 360., (1. - y) * MAX_CHROMA), cx)
            })
            .collect();
        let ring = cx.theme().background;
        let border = cx.theme().border;
        let field_bounds = self.field_bounds.clone();
        let position = (accent.hue / 360., 1. - accent.chroma / MAX_CHROMA);
        Self::region(&self.field, window, cx)
            .id("theme-editor-field")
            .test_support()
            .key_context(FIELD)
            .track_focus(&self.field)
            .w_full()
            .h(rems(10.))
            .p_1()
            .bg(cx.theme().secondary)
            .cursor_crosshair()
            .on_action(
                cx.listener(|this, _: &StepLeft, w, cx| this.nudge_accent(-HUE_STEP, 0., w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &StepRight, w, cx| this.nudge_accent(HUE_STEP, 0., w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &JumpLeft, w, cx| {
                    this.nudge_accent(-HUE_STEP * 6., 0., w, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &JumpRight, w, cx| {
                    this.nudge_accent(HUE_STEP * 6., 0., w, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &StepUp, w, cx| this.nudge_accent(0., CHROMA_STEP, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &StepDown, w, cx| this.nudge_accent(0., -CHROMA_STEP, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &JumpUp, w, cx| {
                    this.nudge_accent(0., CHROMA_STEP * 5., w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &JumpDown, w, cx| {
                this.nudge_accent(0., -CHROMA_STEP * 5., w, cx)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.field.focus(window, cx);
                    this.dragging = true;
                    this.set_from_pointer(event.position, window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if this.dragging && event.pressed_button == Some(MouseButton::Left) {
                    this.set_from_pointer(event.position, window, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = false),
            )
            .child(
                canvas(
                    move |bounds, _, _| field_bounds.set(Some(bounds)),
                    move |bounds, _, window, _| {
                        let dot = px(4.);
                        for (ix, color) in dots.into_iter().enumerate() {
                            let (row, column) = (ix / FIELD_COLUMNS, ix % FIELD_COLUMNS);
                            let center = point(
                                bounds.left()
                                    + bounds.size.width
                                        * ((column as f32 + 0.5) / FIELD_COLUMNS as f32),
                                bounds.top()
                                    + bounds.size.height * ((row as f32 + 0.5) / FIELD_ROWS as f32),
                            );
                            window.paint_quad(
                                fill(Bounds::centered_at(center, size(dot, dot)), color)
                                    .corner_radii(dot / 2.),
                            );
                        }
                        let center = point(
                            bounds.left() + bounds.size.width * position.0,
                            bounds.top() + bounds.size.height * position.1,
                        );
                        let outer = px(22.);
                        let inner = px(16.);
                        window.paint_quad(quad(
                            Bounds::centered_at(center, size(outer, outer)),
                            outer / 2.,
                            ring,
                            px(1.),
                            border,
                            BorderStyle::Solid,
                        ));
                        window.paint_quad(
                            fill(Bounds::centered_at(center, size(inner, inner)), handle)
                                .corner_radii(inner / 2.),
                        );
                    },
                )
                .size_full(),
            )
    }

    fn render_swatches(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = swatch_ix(self.draft.adjustments.accent);
        let theme_accent = theme::theme_accent(cx);
        let owner = cx.entity().downgrade();
        let swatches = std::iter::once(("Theme accent", None))
            .chain(
                PRESETS
                    .iter()
                    .map(|(name, hue, chroma)| (*name, Some(Accent::new(*hue, *chroma)))),
            )
            .enumerate()
            .map(|(ix, (name, accent))| {
                let color = theme::accent_color(accent.unwrap_or(theme_accent), cx);
                let is_selected = selected == Some(ix);
                ColorSwatch::new(
                    SharedString::from(format!(
                        "theme-swatch-{}",
                        name.to_lowercase().replace(' ', "-")
                    )),
                    color,
                )
                .selected(is_selected)
                .accessibility_label(name)
                // The row is one tab stop; h and l move along it.
                .tab_stop(false)
                .size_6()
                .rounded_full()
                .bg(color)
                .border_2()
                .border_color(if is_selected {
                    cx.theme().foreground
                } else {
                    cx.theme().transparent
                })
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(name).build(window, cx)
                })
                .on_click({
                    let owner = owner.clone();
                    move |_, _, window, cx| {
                        let _ = owner.update(cx, |this, cx| {
                            this.swatches.focus(window, cx);
                            this.set_accent(accent, window, cx);
                        });
                    }
                })
            });
        Self::region(&self.swatches, window, cx)
            .id("theme-editor-swatches")
            .test_support()
            .key_context(SWATCHES)
            .track_focus(&self.swatches)
            .p_1()
            .on_action(
                cx.listener(|this, _: &StepLeft, w, cx| {
                    this.step_swatch(SwatchStep::By(-1), w, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &StepRight, w, cx| {
                    this.step_swatch(SwatchStep::By(1), w, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &JumpLeft, w, cx| this.step_swatch(SwatchStep::First, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &JumpRight, w, cx| this.step_swatch(SwatchStep::Last, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &First, w, cx| this.step_swatch(SwatchStep::First, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Last, w, cx| this.step_swatch(SwatchStep::Last, w, cx)),
            )
            .child(h_flex().justify_between().children(swatches))
    }

    fn render_grain(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strength = self
            .draft
            .adjustments
            .grain
            .unwrap_or_else(|| theme::theme_grain(cx));
        let percent = (strength * 100.).round();
        Self::region(&self.grain_focus, window, cx)
            .id("theme-editor-grain")
            .test_support()
            .key_context(GRAIN)
            .track_focus(&self.grain_focus)
            .px_2()
            .py_1()
            .on_action(cx.listener(|this, _: &StepLeft, w, cx| this.step_grain(-GRAIN_STEP, w, cx)))
            .on_action(cx.listener(|this, _: &StepRight, w, cx| this.step_grain(GRAIN_STEP, w, cx)))
            .on_action(
                cx.listener(|this, _: &JumpLeft, w, cx| this.step_grain(-GRAIN_STEP * 10., w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &JumpRight, w, cx| this.step_grain(GRAIN_STEP * 10., w, cx)),
            )
            .on_action(cx.listener(|this, _: &First, w, cx| this.set_grain(0., w, cx)))
            .on_action(cx.listener(|this, _: &Last, w, cx| this.set_grain(1., w, cx)))
            .child(
                h_flex()
                    .gap_3()
                    .child(div().flex_1().child(Slider::new(&self.grain)))
                    .child(
                        div()
                            .w(rems(2.5))
                            .text_right()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if percent == 0. {
                                "Off".to_owned()
                            } else {
                                format!("{percent}%")
                            }),
                    ),
            )
    }
}

#[derive(Clone, Copy)]
enum SwatchStep {
    By(isize),
    First,
    Last,
}

fn section(label: &'static str, hint: Option<AnyElement>, cx: &App) -> Div {
    v_flex().gap_1().child(
        h_flex()
            .justify_between()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label)
            .children(hint),
    )
}

fn hint(
    actions: [&dyn Action; 2],
    label: &'static str,
    context: &str,
    window: &Window,
) -> AnyElement {
    h_flex()
        .gap_1()
        .children(
            actions
                .into_iter()
                .filter_map(|action| Kbd::binding_for_action(action, Some(context), window)),
        )
        .child(label)
        .into_any_element()
}

impl Render for ThemeEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accent_hint = h_flex()
            .gap_3()
            .child(hint([&StepLeft, &StepRight], "Hue", FIELD, window))
            .child(hint([&StepDown, &StepUp], "Intensity", FIELD, window))
            .into_any_element();
        let grain_preview = grain::layer(theme::grain(cx));
        v_flex()
            .id("theme-editor")
            .gap_4()
            .child(section("Appearance", None, cx).child(self.render_appearance(cx)))
            .child(
                section("Accent", Some(accent_hint), cx)
                    .child(self.render_field(window, cx))
                    .child(self.render_swatches(window, cx)),
            )
            .child(
                section("Grain", None, cx)
                    .child(self.render_grain(window, cx))
                    .child(
                        // A sample of the chrome surface, so grain is visible while the
                        // dialog's overlay dims the window behind it.
                        div()
                            .relative()
                            .h(rems(2.5))
                            .rounded(cx.theme().radius)
                            .overflow_hidden()
                            .bg(cx.theme().sidebar)
                            .border_1()
                            .border_color(cx.theme().border)
                            .children(grain_preview),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{PRESETS, swatch_accent, swatch_ix};
    use crate::ui::theme::Accent;

    #[test]
    fn swatches_round_trip_their_accents() {
        assert_eq!(swatch_ix(None), Some(0));
        assert_eq!(swatch_accent(0), None);
        for ix in 1..=PRESETS.len() {
            assert_eq!(swatch_ix(swatch_accent(ix)), Some(ix));
        }
        assert_eq!(swatch_ix(Some(Accent::new(123., 0.05))), None, "custom");
    }
}
