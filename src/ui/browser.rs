//! GPUI presentation for a retained Servo session. The runtime owns all I/O and web state.
use super::commands::{
    BrowserAddress, BrowserBack, BrowserForward, BrowserReload, Command, LeaveEditor,
    ToggleArtifact,
};
use crate::browser::{BrowserCommand, BrowserId, BrowserRuntime};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, IconName, Sizable,
        alert::Alert,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        menu::DropdownMenu,
        spinner::Spinner,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{ops::Range, sync::Arc, time::Duration};

#[derive(Clone, PartialEq, Action)]
#[action(no_json)]
pub struct OpenBrowserUrl {
    pub url: String,
}

pub(super) struct BrowserNavigation;
impl EventEmitter<BrowserNavigation> for BrowserPane {}
pub(super) struct BrowserFocus;
impl EventEmitter<BrowserFocus> for BrowserPane {}
pub(super) struct BrowserLayout;
impl EventEmitter<BrowserLayout> for BrowserPane {}

pub struct BrowserPane {
    pub id: BrowserId,
    runtime: BrowserRuntime,
    address: Entity<InputState>,
    focus: FocusHandle,
    header_focus: FocusHandle,
    artifact: Option<String>,
    collapsed: bool,
    content_height: Option<f32>,
    frame: Option<Arc<RenderImage>>,
    revision: u64,
    bounds: Bounds<Pixels>,
    dimensions: (u32, u32, u32),
    pub url: String,
    pub title: String,
    loading: bool,
    error: Option<String>,
    address_error: Option<String>,
    back: bool,
    forward: bool,
    composition: String,
    copy_request: Option<String>,
    _subscriptions: Vec<Subscription>,
    _updates: Task<()>,
}
impl BrowserPane {
    pub fn new(
        id: BrowserId,
        url: String,
        runtime: BrowserRuntime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        runtime.open(id, url.clone());
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("Enter a URL…"));
        address.update(cx, |input, cx| input.set_value(url.clone(), window, cx));
        let address_focus = address.focus_handle(cx);
        let focus = cx.focus_handle();
        let header_focus = cx.focus_handle();
        let subscriptions = vec![
            cx.subscribe_in(&address, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let value = this.address.read(cx).value().trim().to_owned();
                    match navigation_url(&value) {
                        Ok(url) => {
                            this.runtime
                                .command(this.id, BrowserCommand::Navigate { url });
                            this.focus.focus(window, cx);
                            this.address_error = None;
                        }
                        Err(error) => this.address_error = Some(error),
                    }
                    cx.notify();
                }
            }),
            cx.on_focus(&focus, window, |this, _, cx| {
                this.runtime
                    .command(this.id, BrowserCommand::Focus { focused: true });
                cx.emit(BrowserFocus);
                cx.notify();
            }),
            cx.on_blur(&focus, window, |this, _, cx| {
                this.runtime
                    .command(this.id, BrowserCommand::Focus { focused: false });
                cx.notify();
            }),
            cx.on_focus(&address_focus, window, |_, _, cx| {
                cx.emit(BrowserFocus);
                cx.notify();
            }),
            cx.on_blur(&address_focus, window, |_, _, cx| cx.notify()),
            cx.on_focus_in(&header_focus, window, |_, _, cx| {
                cx.emit(BrowserFocus);
                cx.notify();
            }),
        ];
        let updates = cx.spawn_in(window, async move |pane, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                if pane
                    .update_in(cx, |pane, window, cx| pane.refresh(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            id,
            runtime,
            address,
            focus,
            header_focus,
            artifact: None,
            collapsed: false,
            content_height: None,
            frame: None,
            revision: 0,
            bounds: Bounds::default(),
            dimensions: (0, 0, 0),
            url,
            title: "Browser".into(),
            loading: true,
            error: None,
            address_error: None,
            back: false,
            forward: false,
            composition: String::new(),
            copy_request: None,
            _subscriptions: subscriptions,
            _updates: updates,
        }
    }
    /// Presentation belongs to the retained pane, never to a virtualized row.
    pub(super) fn set_artifact(&mut self, title: String) {
        if self.artifact.is_none() {
            self.runtime
                .command(self.id, BrowserCommand::SetContentSizing { enabled: true });
        }
        self.artifact = Some(title);
    }

    fn toggle_artifact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.artifact.is_none() {
            return;
        }
        self.collapsed = !self.collapsed;
        // Never leave IME or keyboard focus attached to a removed surface.
        self.composition.clear();
        self.header_focus.focus(window, cx);
        self.runtime
            .command(self.id, BrowserCommand::Focus { focused: false });
        self.runtime.command(
            self.id,
            BrowserCommand::SetContentSizing {
                enabled: !self.collapsed,
            },
        );
        cx.emit(BrowserLayout);
        cx.notify();
    }

    fn artifact_height(&self, window: &Window) -> f32 {
        // Keep physical RGBA frames below the engine's 64 MiB limit, with headroom.
        let scale = window.scale_factor();
        let width = f32::from(self.bounds.size.width).max(1.);
        let cap = (48. * 1024. * 1024. / (4. * width * scale * scale))
            .floor()
            .clamp(1., 1600.)
            .min((8192. / scale).floor());
        self.content_height
            .unwrap_or(240.)
            .ceil()
            .max(120.)
            .min(cap)
    }
    pub fn focus_address(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.artifact.is_some() {
            self.header_focus.focus(window, cx);
            return;
        }
        self.address
            .update(cx, |address, cx| address.focus(window, cx));
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.collapsed {
            self.header_focus.focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
    }
    pub fn command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::BrowserAddress => self.focus_address(window, cx),
            Command::ToggleArtifact => self.toggle_artifact(window, cx),
            Command::BrowserBack => self.runtime.command(self.id, BrowserCommand::Back),
            Command::BrowserForward => self.runtime.command(self.id, BrowserCommand::Forward),
            Command::BrowserReload => self.runtime.command(self.id, BrowserCommand::Reload),
            _ => {}
        }
    }
    pub fn disabled_reason(&self, command: Command) -> Option<String> {
        match command {
            Command::BrowserAddress if self.artifact.is_some() => {
                Some("Inline artifacts have no address bar".into())
            }
            Command::ToggleArtifact if self.artifact.is_none() => {
                Some("Focus an inline artifact first".into())
            }
            Command::BrowserBack if !self.back => Some("No previous page".into()),
            Command::BrowserForward if !self.forward => Some("No next page".into()),
            _ => None,
        }
    }
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(request) = &self.copy_request
            && let Some(result) = self.runtime.evaluation(request)
        {
            if let Ok(serde_json::Value::String(text)) = result {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            self.copy_request = None;
        }
        let Some(snapshot) = self.runtime.snapshot(self.id) else {
            return;
        };
        // Resize invalidates the engine measurement. Keep the previous usable height
        // until its replacement arrives, rather than oscillating through the fallback.
        if (snapshot.content_height.is_some() || self.url != snapshot.url)
            && self.content_height != snapshot.content_height
        {
            self.content_height = snapshot.content_height;
            if self.artifact.is_some() && !self.collapsed {
                cx.emit(BrowserLayout);
                cx.notify();
            }
        }
        if self.artifact.is_some()
            && (self.error != snapshot.error
                || (self.url == "about:blank") != (snapshot.url == "about:blank"))
        {
            cx.emit(BrowserLayout);
        }
        let navigated = self.url != snapshot.url || self.title != snapshot.title;
        let mut changed = self.url != snapshot.url
            || self.title != snapshot.title
            || self.loading != snapshot.loading
            || self.error != snapshot.error
            || self.back != snapshot.can_go_back
            || self.forward != snapshot.can_go_forward;
        if self.url != snapshot.url {
            self.url = snapshot.url;
            if !self.address.read(cx).focus_handle(cx).is_focused(window) {
                self.address.update(cx, |input, cx| {
                    input.set_value(self.url.clone(), window, cx)
                });
            }
        }
        self.title = snapshot.title;
        self.loading = snapshot.loading;
        self.error = snapshot.error;
        self.back = snapshot.can_go_back;
        self.forward = snapshot.can_go_forward;
        if let Some(frame) = snapshot.frame
            && !self.collapsed
            && (self.frame.is_none() || frame.revision != self.revision)
        {
            // GPUI consumes BGRA while the engine/screenshot contract uses RGBA.
            let mut pixels = frame.rgba.clone();
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            if let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, pixels) {
                self.frame = Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])));
                self.revision = frame.revision;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
        if navigated {
            cx.emit(BrowserNavigation);
        }
    }
    fn prepare(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.bounds = bounds;
        let scale = window.scale_factor();
        let width = (f32::from(bounds.size.width) * scale).round().max(1.) as u32;
        let height = (f32::from(bounds.size.height) * scale).round().max(1.) as u32;
        let dimensions = (width, height, scale.to_bits());
        if dimensions != self.dimensions {
            if self.artifact.is_some()
                && (self.dimensions.0 != width || self.dimensions.2 != scale.to_bits())
            {
                // The physical frame cap can change even if natural document height does not.
                cx.emit(BrowserLayout);
            }
            self.dimensions = dimensions;
            self.runtime.command(
                self.id,
                BrowserCommand::Resize {
                    width,
                    height,
                    scale,
                },
            );
        }
    }
    fn position(&self, point: Point<Pixels>) -> (f32, f32) {
        (
            f32::from(point.x - self.bounds.origin.x),
            f32::from(point.y - self.bounds.origin.y),
        )
    }
    fn pointer(&self, position: Point<Pixels>, button: MouseButton, pressed: bool) {
        let (x, y) = self.position(position);
        let button = match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => return,
        };
        self.runtime.command(
            self.id,
            BrowserCommand::Pointer {
                x,
                y,
                button,
                pressed,
            },
        );
    }
    fn key(&mut self, key: &Keystroke, pressed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let shortcut = if cfg!(target_os = "macos") {
            key.modifiers.platform
        } else {
            key.modifiers.control
        };
        if shortcut
            && matches!(
                key.key.as_str(),
                "l" | "r" | "escape" | "w" | "1" | "2" | "3" | "b" | "j" | "n"
            )
        {
            return;
        }
        if key.modifiers.control && key.key == "tab" {
            return;
        }
        if shortcut
            && (key.key == "," || (key.modifiers.shift && matches!(key.key.as_str(), "p" | "a")))
        {
            return;
        }
        if pressed && shortcut && key.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.runtime.command(self.id, BrowserCommand::Text { text });
            }
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if pressed && shortcut && key.key == "c" {
            let request_id = uuid::Uuid::new_v4().to_string();
            self.runtime.command(
                self.id,
                BrowserCommand::Evaluate {
                    request_id: request_id.clone(),
                    script: "String(window.getSelection() || '')".into(),
                },
            );
            self.copy_request = Some(request_id);
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if !self.composition.is_empty() {
            return;
        }
        // Native committed text is delivered separately; avoid inserting printable keys twice.
        if key.key.chars().count() == 1 && !key.modifiers.control && !key.modifiers.platform {
            return;
        }
        let name = match key.key.as_str() {
            "enter" => "Enter",
            "backspace" => "Backspace",
            "delete" => "Delete",
            "escape" => "Escape",
            "tab" => "Tab",
            "left" => "ArrowLeft",
            "right" => "ArrowRight",
            "up" => "ArrowUp",
            "down" => "ArrowDown",
            "home" => "Home",
            "end" => "End",
            "pageup" => "PageUp",
            "pagedown" => "PageDown",
            other => other,
        };
        let code = match name {
            " " => "Space".to_owned(),
            name if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() => {
                format!("Key{}", name.to_ascii_uppercase())
            }
            name if name.len() == 1 && name.as_bytes()[0].is_ascii_digit() => {
                format!("Digit{name}")
            }
            name if name.len() > 1 => name.to_owned(),
            _ => String::new(),
        };
        self.runtime.command(
            self.id,
            BrowserCommand::Key {
                key: name.into(),
                code,
                pressed,
                shift: key.modifiers.shift,
                control: key.modifiers.control,
                alt: key.modifiers.alt,
                meta: key.modifiers.platform,
            },
        );
        window.prevent_default();
        cx.stop_propagation();
    }
}
impl Focusable for BrowserPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        if self.collapsed {
            self.header_focus.clone()
        } else {
            self.focus.clone()
        }
    }
}
impl Render for BrowserPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prepare = cx.entity();
        let input = cx.entity();
        let toolbar = self.artifact.is_none().then(|| {
            let address_focused = self.address.focus_handle(cx).is_focused(window);
            h_flex()
                .id("browser-address-toolbar")
                .test_support()
                .items_center()
                .gap_1()
                .p_1()
                .when(self.focus.is_focused(window), |row| {
                    row.bg(cx.theme().secondary)
                })
                .child(
                    Button::new("browser-back")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowLeft)
                        .disabled(!self.back)
                        .tooltip_with_action(
                            self.disabled_reason(Command::BrowserBack)
                                .unwrap_or_else(|| Command::BrowserBack.label().into()),
                            &BrowserBack,
                            Some("BrowserPane"),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus(window, cx);
                            window.dispatch_action(Command::BrowserBack.action(), cx);
                        })),
                )
                .child(
                    Button::new("browser-forward")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowRight)
                        .disabled(!self.forward)
                        .tooltip_with_action(
                            self.disabled_reason(Command::BrowserForward)
                                .unwrap_or_else(|| Command::BrowserForward.label().into()),
                            &BrowserForward,
                            Some("BrowserPane"),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus(window, cx);
                            window.dispatch_action(Command::BrowserForward.action(), cx);
                        })),
                )
                .child(
                    Button::new("browser-reload")
                        .ghost()
                        .small()
                        .label("Reload")
                        .tooltip_with_action(
                            Command::BrowserReload.label(),
                            &BrowserReload,
                            Some("BrowserPane"),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus(window, cx);
                            window.dispatch_action(Command::BrowserReload.action(), cx);
                        })),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&self.address)
                            .small()
                            .focus_bordered(false)
                            .when(address_focused, |input| {
                                input
                                    .border_color(cx.theme().muted_foreground)
                                    .bg(cx.theme().secondary)
                            }),
                    ),
                )
                .when(self.loading, |row| row.child(Spinner::new().small()))
        });
        let artifact_header = self.artifact.as_ref().map(|title| {
            let pane = cx.entity().downgrade();
            h_flex()
                .id("artifact-header")
                .test_support()
                .w_full()
                .min_w_0()
                .flex_none()
                .gap_1()
                .px_1()
                .bg(cx.theme().muted)
                .text_color(cx.theme().muted_foreground)
                .child(
                    div()
                        .id("artifact-disclosure-focus")
                        .test_support()
                        .key_context("ArtifactDisclosure")
                        .track_focus(&self.header_focus)
                        .flex_1()
                        .min_w_0()
                        .border_1()
                        .border_color(cx.theme().muted)
                        .when(self.header_focus.is_focused(window), |root| {
                            root.bg(cx.theme().secondary)
                                .border_color(cx.theme().muted_foreground)
                        })
                        .child(
                            Button::new("artifact-disclosure")
                                .ghost()
                                .xsmall()
                                .compact()
                                .w_full()
                                .icon(if self.collapsed {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronDown
                                })
                                .label(title.clone())
                                .toggled(!self.collapsed)
                                .tooltip_with_action(
                                    if self.collapsed {
                                        "Expand artifact"
                                    } else {
                                        "Collapse artifact"
                                    },
                                    &ToggleArtifact,
                                    Some("BrowserPane"),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.header_focus.focus(window, cx);
                                    window.dispatch_action(Command::ToggleArtifact.action(), cx);
                                })),
                        ),
                )
                .when(self.loading, |row| row.child(Spinner::new().xsmall()))
                .child(
                    Button::new("artifact-navigation")
                        .ghost()
                        .xsmall()
                        .compact()
                        .icon(IconName::Ellipsis)
                        .tooltip("Artifact navigation")
                        .dropdown_menu(move |mut menu, _, cx| {
                            let Some(pane) = pane.upgrade() else {
                                return menu;
                            };
                            pane.update(cx, |_, cx| cx.emit(BrowserFocus));
                            menu = menu.action_context(pane.read(cx).header_focus.clone());
                            for command in [
                                Command::BrowserBack,
                                Command::BrowserForward,
                                Command::BrowserReload,
                            ] {
                                let reason = pane.read(cx).disabled_reason(command);
                                let label = match &reason {
                                    Some(reason) => format!("{} — {reason}", command.label()),
                                    None => command.label().into(),
                                };
                                menu = menu.menu_with_disabled(
                                    label,
                                    command.action(),
                                    reason.is_some(),
                                );
                            }
                            menu
                        }),
                )
        });
        let surface = div()
            .id(if self.artifact.is_some() {
                "artifact-surface"
            } else {
                "servo-surface"
            })
            .test_support()
            .key_context("Browser NativeInput")
            .track_focus(&self.focus)
            .relative()
            .when(self.artifact.is_none(), |surface| surface.flex_1())
            .when(self.artifact.is_some(), |surface| {
                surface
                    .flex_none()
                    .w_full()
                    .h(px(self.artifact_height(window)))
            })
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, w, cx| {
                this.key(&event.keystroke, true, w, cx)
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, w, cx| {
                this.key(&event.keystroke, false, w, cx)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, w, cx| {
                    this.focus(w, cx);
                    this.pointer(event.position, event.button, true);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.pointer(event.position, event.button, true);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.pointer(event.position, event.button, true);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    this.pointer(event.position, event.button, false);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    this.pointer(event.position, event.button, false);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    this.pointer(event.position, event.button, false);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, _| {
                    this.pointer(event.position, event.button, false)
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, _| {
                let (x, y) = this.position(event.position);
                this.runtime.command(this.id, BrowserCommand::Move { x, y });
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                // A fitted artifact belongs to the transcript's scroll surface. Only the
                // bounded fallback viewport scrolls internally.
                if this.artifact.is_some()
                    && this
                        .content_height
                        .is_none_or(|height| height <= f32::from(this.bounds.size.height) + 1.)
                {
                    return;
                }
                let (x, y) = this.position(event.position);
                let (dx, dy) = match event.delta {
                    ScrollDelta::Pixels(p) => (-f32::from(p.x), -f32::from(p.y)),
                    ScrollDelta::Lines(p) => (-p.x * 40., -p.y * 40.),
                };
                this.runtime
                    .command(this.id, BrowserCommand::Scroll { x, y, dx, dy });
                cx.stop_propagation();
            }))
            .when_some(self.frame.clone(), |surface, frame| {
                surface.child(
                    img(frame)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Fill),
                )
            })
            .child(
                canvas(
                    move |bounds, window, cx| {
                        prepare.update(cx, |pane, cx| pane.prepare(bounds, window, cx))
                    },
                    move |bounds, (), window, cx| {
                        input.update(cx, |pane, cx| {
                            window.handle_input(
                                &pane.focus,
                                ElementInputHandler::new(bounds, cx.entity()),
                                cx,
                            )
                        })
                    },
                )
                .absolute()
                .inset_0()
                .size_full(),
            );
        v_flex()
            .id(if self.artifact.is_some() {
                "inline-artifact"
            } else {
                "browser-pane"
            })
            .test_support()
            .key_context("BrowserPane")
            .w_full()
            .when(self.artifact.is_none(), |root| root.h_full())
            .when(self.artifact.is_some(), |root| {
                root.flex_none().border_1().border_color(cx.theme().border)
            })
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .on_action(
                cx.listener(|this, _: &BrowserBack, w, cx| {
                    this.command(Command::BrowserBack, w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &BrowserForward, w, cx| {
                this.command(Command::BrowserForward, w, cx)
            }))
            .on_action(cx.listener(|this, _: &BrowserReload, w, cx| {
                this.command(Command::BrowserReload, w, cx)
            }))
            .on_action(cx.listener(|this, _: &BrowserAddress, w, cx| {
                this.command(Command::BrowserAddress, w, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleArtifact, w, cx| {
                this.command(Command::ToggleArtifact, w, cx)
            }))
            .on_action(cx.listener(|this, _: &LeaveEditor, w, cx| this.focus_address(w, cx)))
            .when_some(toolbar, |root, toolbar| root.child(toolbar))
            .when_some(artifact_header, |root, header| root.child(header))
            .when_some(
                self.address_error.clone().or_else(|| self.error.clone()),
                |root, error| root.child(Alert::error("browser-error", error).small()),
            )
            .when(self.url == "about:blank" && !self.collapsed, |root| {
                root.child(div().p_2().text_color(cx.theme().muted_foreground).child(
                    if self.artifact.is_some() {
                        "This artifact is empty"
                    } else {
                        "Enter a URL to browse with Servo"
                    },
                ))
            })
            .when(!self.composition.is_empty(), |root| {
                root.child(div().p_1().child(self.composition.clone()))
            })
            .when(!self.collapsed, |root| root.child(surface))
    }
}
impl EntityInputHandler for BrowserPane {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let chars: Vec<u16> = self.composition.encode_utf16().collect();
        let range = range.start.min(chars.len())..range.end.min(chars.len());
        *adjusted = Some(range.clone());
        String::from_utf16(&chars[range]).ok()
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let n = self.composition.encode_utf16().count();
        Some(UTF16Selection {
            range: n..n,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.composition.is_empty()).then(|| 0..self.composition.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.composition.clear();
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition.clear();
        self.runtime
            .command(self.id, BrowserCommand::Text { text: text.into() });
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition = text.into();
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
pub fn navigation_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Enter a URL first".into());
    }
    let value = if value.starts_with('/') {
        return url::Url::from_file_path(value)
            .map(|url| url.to_string())
            .map_err(|_| "Invalid file path".into());
    } else if value.contains("://") || value.starts_with("about:") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    url::Url::parse(&value)
        .map(|url| url.to_string())
        .map_err(|error| {
            format!("URL could not be opened: {error}. Edit the address and try again.")
        })
}
#[cfg(test)]
mod tests {
    use super::navigation_url;
    #[test]
    fn browser_addresses_preserve_local_ports_and_file_escaping() {
        assert_eq!(
            navigation_url("http://localhost:3000/a?q=x").unwrap(),
            "http://localhost:3000/a?q=x"
        );
        assert_eq!(
            navigation_url("/tmp/my plot.html").unwrap(),
            "file:///tmp/my%20plot.html"
        );
        assert!(navigation_url(" ").is_err());
        assert_eq!(
            navigation_url("example.org").unwrap(),
            "https://example.org/"
        );
    }
}
