//! GPUI presentation for a retained Servo session. The runtime owns all I/O and web state.
use super::commands::{
    BrowserAddress, BrowserBack, BrowserForward, BrowserReload, Command, LeaveEditor,
    ToggleArtifact,
};
use crate::browser::{BrowserCommand, BrowserFrame, BrowserId, BrowserRuntime, ChannelEvent};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, IconName, Sizable,
        alert::Alert,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        menu::DropdownMenu,
        spinner::Spinner,
        tag::Tag,
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

/// Largest share of the window height a docked artifact takes above the composer.
const DOCKED_SHARE: f32 = 0.45;

/// Polling the engine while the page changes or the user works in it.
const POLL_ACTIVE: Duration = Duration::from_millis(33);
/// Polling a page nobody sees, or one that stopped changing.
const POLL_IDLE: Duration = Duration::from_millis(264);
/// Unchanged polls at the active rate before backing off.
const POLL_GRACE: u32 = 15;

/// How often to poll the engine: at the active rate while anything changes and shortly
/// after, then backing off to the idle rate, which hidden and collapsed panes keep.
#[derive(Debug, Default)]
struct PollSchedule {
    unchanged: u32,
}

impl PollSchedule {
    fn wake(&mut self) {
        self.unchanged = 0;
    }

    fn next(&mut self, changed: bool, shown: bool) -> Duration {
        self.unchanged = if changed {
            0
        } else {
            self.unchanged.saturating_add(1)
        };
        if !shown {
            return POLL_IDLE;
        }
        let backoff = self.unchanged.saturating_sub(POLL_GRACE).min(8);
        (POLL_ACTIVE * 2_u32.pow(backoff)).min(POLL_IDLE)
    }
}

/// The engine's RGBA frame as the BGRA image GPUI paints. Runs off the GPUI thread.
fn bgra_image(frame: &BrowserFrame) -> Option<Arc<RenderImage>> {
    let mut pixels = frame.rgba.clone();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, pixels)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

pub(super) struct BrowserNavigation;
impl EventEmitter<BrowserNavigation> for BrowserPane {}
pub(super) struct BrowserFocus;
impl EventEmitter<BrowserFocus> for BrowserPane {}
pub(super) struct BrowserLayout;
impl EventEmitter<BrowserLayout> for BrowserPane {}
/// Channel events an artifact page sent, in order; the workspace routes them to the
/// artifact's Conversation.
pub(super) struct BrowserChannel(pub Vec<ChannelEvent>);
impl EventEmitter<BrowserChannel> for BrowserPane {}

pub struct BrowserPane {
    pub id: BrowserId,
    runtime: BrowserRuntime,
    address: Entity<InputState>,
    focus: FocusHandle,
    header_focus: FocusHandle,
    artifact: Option<String>,
    /// Docked above the composer rather than in its transcript row.
    docked: bool,
    /// The agent has what it needed; the page stays collapsed until reopened.
    answered: bool,
    collapsed: bool,
    content_height: Option<f32>,
    /// The image painted now; a replaced one leaves the sprite atlas at once.
    frame: Option<Arc<RenderImage>>,
    frame_dimensions: Option<(u32, u32, f32)>,
    revision: u64,
    /// The frame revision being converted off the GPUI thread.
    converting: Option<u64>,
    /// The newest frame revision the engine reported.
    seen_revision: Option<u64>,
    /// The surface was laid out in a window frame; false once a poll's notify drew no
    /// surface for a whole poll interval.
    visible: bool,
    /// The surface was laid out since the last poll.
    painted: bool,
    /// Polling notified since the last poll.
    notified: bool,
    /// A notify before the last poll has not drawn the surface yet.
    unpainted_notify: bool,
    /// What the engine was last told: whether anyone sees the page. Hidden pages
    /// publish frames only when an agent captures them.
    reported_shown: bool,
    schedule: PollSchedule,
    poll_delay: Duration,
    wake: async_channel::Sender<()>,
    bounds: Bounds<Pixels>,
    dimensions: (u32, u32, u32),
    layout_dimensions: (u32, u32, u32),
    fixed_viewport: Option<(u32, u32, u32)>,
    pressed_buttons: [bool; 3],
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
        Self::open(id, url, None, runtime, window, cx)
    }

    /// An inline artifact: the engine injects the artifact kit with `theme` tokens and
    /// accepts its channel events. Artifact-ness is fixed for the session's lifetime.
    pub fn artifact(
        id: BrowserId,
        url: String,
        title: String,
        theme: String,
        runtime: BrowserRuntime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self::open(id, url, Some(theme), runtime, window, cx);
        pane.set_artifact(title);
        pane
    }

    fn open(
        id: BrowserId,
        url: String,
        artifact_theme: Option<String>,
        runtime: BrowserRuntime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        match artifact_theme {
            Some(theme) => runtime.open_artifact(id, url.clone(), theme),
            None => runtime.open(id, url.clone()),
        }
        let dimensions = runtime
            .snapshot(id)
            .map(|snapshot| {
                (
                    snapshot.viewport_width,
                    snapshot.viewport_height,
                    snapshot.scale.to_bits(),
                )
            })
            // A concurrently closed session has no viewport; resize skips zero dimensions.
            .unwrap_or((0, 0, 1_f32.to_bits()));
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("Enter a URL…"));
        address.update(cx, |input, cx| input.set_value(url.clone(), window, cx));
        let address_focus = address.focus_handle(cx);
        let focus = cx.focus_handle();
        let header_focus = cx.focus_handle();
        let mut subscriptions = vec![
            cx.subscribe_in(&address, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let value = this.address.read(cx).value().trim().to_owned();
                    match navigation_url(&value) {
                        Ok(url) => {
                            this.send(BrowserCommand::Navigate { url });
                            this.focus.focus(window, cx);
                            this.address_error = None;
                        }
                        Err(error) => this.address_error = Some(error),
                    }
                    cx.notify();
                }
            }),
            cx.on_focus(&focus, window, |this, _, cx| {
                this.send(BrowserCommand::Focus { focused: true });
                cx.emit(BrowserFocus);
                cx.notify();
            }),
            cx.on_blur(&focus, window, |this, _, cx| {
                this.send(BrowserCommand::Focus { focused: false });
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
        // Free the painted frame's atlas tile in every window once the pane is gone.
        subscriptions.push(cx.on_release(|pane, cx| {
            if let Some(frame) = pane.frame.take() {
                cx.drop_image(frame, None);
            }
        }));
        let (wake, woken) = async_channel::bounded(1);
        let updates = cx.spawn_in(window, async move |pane, cx| {
            let mut delay = POLL_ACTIVE;
            loop {
                let timer = cx.background_executor().timer(delay);
                smol::future::or(timer, async {
                    let _ = woken.recv().await;
                })
                .await;
                match pane.update_in(cx, |pane, window, cx| pane.poll(window, cx)) {
                    Ok(next) => delay = next,
                    Err(_) => break,
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
            docked: false,
            answered: false,
            collapsed: false,
            content_height: None,
            frame: None,
            frame_dimensions: None,
            revision: 0,
            converting: None,
            seen_revision: None,
            visible: true,
            painted: false,
            notified: false,
            unpainted_notify: false,
            reported_shown: true,
            schedule: PollSchedule::default(),
            poll_delay: POLL_ACTIVE,
            wake,
            bounds: Bounds::default(),
            dimensions,
            layout_dimensions: dimensions,
            fixed_viewport: None,
            pressed_buttons: [false; 3],
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
        if self.artifact.is_none() && self.fixed_viewport.is_none() {
            self.send(BrowserCommand::SetContentSizing { enabled: true });
        }
        self.artifact = Some(title);
    }

    /// Docked artifacts stay at most a share of the window tall, so the composer stays in
    /// view; taller pages scroll inside the surface.
    pub(super) fn set_docked(&mut self, docked: bool, cx: &mut Context<Self>) {
        if self.docked != docked {
            self.docked = docked;
            cx.emit(BrowserLayout);
            cx.notify();
        }
    }

    /// The agent has what it needed: collapse to the header, marked answered. The page
    /// keeps its document and form values, and the user can reopen it. Focus is the
    /// caller's to move; this never focuses the header.
    pub(super) fn complete(&mut self, cx: &mut Context<Self>) {
        if self.artifact.is_none() {
            return;
        }
        self.answered = true;
        if !self.collapsed {
            self.collapsed = true;
            self.composition.clear();
            self.send(BrowserCommand::Focus { focused: false });
            self.send(BrowserCommand::SetContentSizing { enabled: false });
            self.report_shown();
        }
        cx.emit(BrowserLayout);
        cx.notify();
    }

    pub(super) fn artifact_title(&self) -> Option<&str> {
        self.artifact.as_deref()
    }

    fn toggle_artifact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.artifact.is_none() {
            return;
        }
        self.collapsed = !self.collapsed;
        // Never leave IME or keyboard focus attached to a removed surface.
        self.composition.clear();
        self.header_focus.focus(window, cx);
        self.send(BrowserCommand::Focus { focused: false });
        self.send(BrowserCommand::SetContentSizing {
            enabled: !self.collapsed && self.fixed_viewport.is_none(),
        });
        self.report_shown();
        cx.emit(BrowserLayout);
        cx.notify();
    }

    /// Fix the CSS viewport and device scale, or restore following the pane's layout.
    pub fn set_viewport(
        &mut self,
        viewport: Option<(u32, u32, f32)>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let dimensions = viewport
            .map(|(width, height, scale)| {
                if !scale.is_finite() || !(0.25..=8.).contains(&scale) {
                    return Err("Browser scale must be between 0.25 and 8".to_owned());
                }
                if width == 0 || height == 0 {
                    return Err("Browser CSS viewport dimensions must be positive".to_owned());
                }
                let width = (width as f64 * f64::from(scale)).round() as u32;
                let height = (height as f64 * f64::from(scale)).round() as u32;
                crate::browser::frame_len(width, height).map_err(|error| error.to_string())?;
                Ok((width, height, scale.to_bits()))
            })
            .transpose()?;
        if self.fixed_viewport == dimensions {
            return Ok(());
        }
        self.fixed_viewport = dimensions;
        if self.artifact.is_some() {
            self.send(BrowserCommand::SetContentSizing {
                enabled: dimensions.is_none() && !self.collapsed,
            });
            cx.emit(BrowserLayout);
        }
        self.resize(dimensions.unwrap_or(self.layout_dimensions));
        cx.notify();
        Ok(())
    }

    /// Physical dimensions most recently requested from Servo.
    pub(super) fn viewport_dimensions(&self) -> (u32, u32, f32) {
        (
            self.dimensions.0,
            self.dimensions.1,
            f32::from_bits(self.dimensions.2),
        )
    }

    fn artifact_height(&self, window: &Window) -> f32 {
        if let Some((width, height, _)) = self.fixed_viewport {
            return (f32::from(self.bounds.size.width).max(1.) * height as f32 / width as f32)
                .clamp(120., 1600.);
        }
        // Keep physical RGBA frames below the engine's 64 MiB limit, with headroom.
        let scale = window.scale_factor();
        let width = f32::from(self.bounds.size.width).max(1.);
        let cap = (48. * 1024. * 1024. / (4. * width * scale * scale))
            .floor()
            .clamp(1., 1600.)
            .min((8192. / scale).floor());
        let cap = if self.docked {
            cap.min((f32::from(window.viewport_size().height) * DOCKED_SHARE).max(120.))
        } else {
            cap
        };
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
    /// Keyboard focus is anywhere in this pane: the page, its address bar or the artifact
    /// header (including its menu button).
    pub(super) fn contains_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
            || self.header_focus.contains_focused(window, cx)
            || self.address.focus_handle(cx).contains_focused(window, cx)
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
            Command::BrowserBack => self.send(BrowserCommand::Back),
            Command::BrowserForward => self.send(BrowserCommand::Forward),
            Command::BrowserReload => self.send(BrowserCommand::Reload),
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
    /// One poll of the engine; returns the delay until the next. A notify that drew no
    /// surface for a whole poll interval means nobody sees the pane: its frames wait
    /// until it is laid out again.
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Duration {
        if self.unpainted_notify && !self.painted {
            self.visible = false;
        }
        self.unpainted_notify = self.notified && !self.painted;
        self.notified = false;
        self.painted = false;
        self.report_shown();
        let changed = self.refresh(window, cx);
        self.poll_delay = self.schedule.next(changed, self.visible && !self.collapsed);
        self.poll_delay
    }

    /// Tell the engine when the page becomes shown or hidden. A shown page publishes
    /// frames as it changes; a page shown again publishes what changed meanwhile.
    fn report_shown(&mut self) {
        let shown = self.visible && !self.collapsed;
        if self.reported_shown != shown {
            self.reported_shown = shown;
            self.runtime.set_visible(self.id, shown);
        }
    }

    /// Poll at the active rate again, now if the poller is backing off.
    fn wake_poll(&mut self) {
        self.schedule.wake();
        if self.poll_delay > POLL_ACTIVE {
            self.poll_delay = POLL_ACTIVE;
            let _ = self.wake.try_send(());
        }
    }

    /// Input and navigation make the page change soon; poll for it at the active rate.
    fn send(&mut self, command: BrowserCommand) {
        self.runtime.command(self.id, command);
        self.wake_poll();
    }

    /// Reads the engine's state; true when anything in it changed.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut activity = false;
        if let Some(request) = &self.copy_request
            && let Some(result) = self.runtime.evaluation(request)
        {
            if let Ok(serde_json::Value::String(text)) = result {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            self.copy_request = None;
            activity = true;
        }
        if self.artifact.is_some() {
            let events = self.runtime.take_channel(self.id);
            if !events.is_empty() {
                cx.emit(BrowserChannel(events));
                activity = true;
            }
        }
        let Some(snapshot) = self.runtime.snapshot(self.id) else {
            return activity;
        };
        // Resize invalidates the engine measurement. Keep the previous usable height
        // until its replacement arrives, rather than oscillating through the fallback.
        if (snapshot.content_height.is_some() || self.url != snapshot.url)
            && self.content_height != snapshot.content_height
        {
            self.content_height = snapshot.content_height;
            activity = true;
            if self.artifact.is_some() && !self.collapsed && self.fixed_viewport.is_none() {
                cx.emit(BrowserLayout);
                self.notify(cx);
            }
        }
        if self.artifact.is_some()
            && (self.error != snapshot.error
                || (self.url == "about:blank") != (snapshot.url == "about:blank"))
        {
            cx.emit(BrowserLayout);
        }
        let navigated = self.url != snapshot.url || self.title != snapshot.title;
        let changed = self.url != snapshot.url
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
        if let Some(frame) = snapshot.frame {
            activity |= self.seen_revision.replace(frame.revision) != Some(frame.revision);
            self.show_frame(frame, window, cx);
        }
        if changed {
            self.notify(cx);
        }
        if navigated {
            cx.emit(BrowserNavigation);
        }
        activity || changed
    }

    /// A notify from polling doubles as a probe of whether the surface is still shown.
    fn notify(&mut self, cx: &mut Context<Self>) {
        self.notified = true;
        cx.notify();
    }

    /// Converts a new engine frame on a background thread, one at a time. Hidden and
    /// collapsed panes skip frames; the newest is converted once the surface shows.
    fn show_frame(
        &mut self,
        frame: Arc<BrowserFrame>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.collapsed
            || !self.visible
            || self.converting.is_some()
            || (self.frame.is_some() && frame.revision == self.revision)
        {
            return;
        }
        let revision = frame.revision;
        let dimensions = (frame.width, frame.height, frame.scale);
        self.converting = Some(revision);
        let image = cx
            .background_executor()
            .spawn(async move { bgra_image(&frame) });
        cx.spawn_in(window, async move |pane, cx| {
            let image = image.await;
            let _ = pane.update_in(cx, |pane, window, cx| {
                if pane.converting != Some(revision) {
                    return;
                }
                pane.converting = None;
                // A frame finished after collapsing is stale; expanding converts anew.
                if let Some(image) = image.filter(|_| !pane.collapsed) {
                    pane.replace_frame(image, revision, dimensions, window, cx);
                }
            });
        })
        .detach();
    }

    /// Paint `image` from now on and free the replaced frame's atlas tile: nothing
    /// paints it again, since this notify redraws the surface with the new image.
    fn replace_frame(
        &mut self,
        image: Arc<RenderImage>,
        revision: u64,
        dimensions: (u32, u32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(previous) = self.frame.replace(image) {
            cx.drop_image(previous, Some(window));
        }
        self.revision = revision;
        self.frame_dimensions = Some(dimensions);
        self.notify(cx);
    }

    fn prepare(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.painted = true;
        if !self.visible {
            // Shown again: convert the newest frame without waiting for the idle poll.
            self.visible = true;
            self.report_shown();
            self.wake_poll();
        }
        if self.fixed_viewport.is_some()
            && self.artifact.is_some()
            && self.bounds.size.width != bounds.size.width
        {
            cx.emit(BrowserLayout);
        }
        self.bounds = bounds;
        let scale = window.scale_factor();
        let width = (f32::from(bounds.size.width) * scale).round().max(1.) as u32;
        let height = (f32::from(bounds.size.height) * scale).round().max(1.) as u32;
        let dimensions = (width, height, scale.to_bits());
        self.layout_dimensions = dimensions;
        if self.fixed_viewport.is_none() {
            if self.artifact.is_some()
                && (self.dimensions.0 != width || self.dimensions.2 != scale.to_bits())
            {
                // The physical frame cap can change even if natural document height does not.
                cx.emit(BrowserLayout);
            }
            self.resize(dimensions);
        }
    }
    fn resize(&mut self, dimensions: (u32, u32, u32)) {
        if dimensions == self.dimensions || dimensions.0 == 0 || dimensions.1 == 0 {
            return;
        }
        self.dimensions = dimensions;
        self.send(BrowserCommand::Resize {
            width: dimensions.0,
            height: dimensions.1,
            scale: f32::from_bits(dimensions.2),
        });
    }

    /// Use the same fit operation as the image, including while a resize is pending.
    fn presentation(&self) -> Option<(Bounds<Pixels>, f32)> {
        let (width, height, scale) = self.frame_dimensions?;
        if self.bounds.size.width <= px(0.) || self.bounds.size.height <= px(0.) {
            return None;
        }
        let bounds = ObjectFit::Contain.get_bounds(self.bounds, size(width.into(), height.into()));
        let css_per_pixel = width as f32 / scale / f32::from(bounds.size.width);
        Some((bounds, css_per_pixel))
    }

    fn position(&self, point: Point<Pixels>) -> Option<(f32, f32)> {
        let (bounds, scale) = self.presentation()?;
        bounds.contains(&point).then(|| {
            (
                f32::from(point.x - bounds.origin.x) * scale,
                f32::from(point.y - bounds.origin.y) * scale,
            )
        })
    }
    fn pointer(&mut self, position: Point<Pixels>, button: MouseButton, pressed: bool) {
        let button = match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => return,
        };
        let Some((bounds, scale)) = self.presentation() else {
            return;
        };
        // Ignore letterboxing, but always release a press begun on the web surface.
        if pressed && !bounds.contains(&position)
            || !pressed && !self.pressed_buttons[button as usize]
        {
            return;
        }
        self.pressed_buttons[button as usize] = pressed;
        let x = f32::from(position.x - bounds.origin.x) * scale;
        let y = f32::from(position.y - bounds.origin.y) * scale;
        self.send(BrowserCommand::Pointer {
            x,
            y,
            button,
            pressed,
        });
    }
    fn key(&mut self, key: &Keystroke, pressed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let shortcut = if cfg!(target_os = "macos") {
            key.modifiers.platform
        } else {
            key.modifiers.control
        };
        // Pane shortcuts belong to the app on both key-down and key-up.
        if shortcut
            && key.modifiers.alt
            && matches!(
                key.key.as_str(),
                "h" | "j" | "k" | "l" | "left" | "down" | "up" | "right"
            )
        {
            return;
        }
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
                self.send(BrowserCommand::Text { text });
            }
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if pressed && shortcut && key.key == "c" {
            let request_id = uuid::Uuid::new_v4().to_string();
            self.send(BrowserCommand::Evaluate {
                request_id: request_id.clone(),
                script: "String(window.getSelection() || '')".into(),
            });
            self.copy_request = Some(request_id);
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if !self.composition.is_empty() {
            return;
        }
        // Native committed text is delivered separately; avoid inserting printable keys twice.
        if (key.key.chars().count() == 1 || key.key == "space")
            && !key.modifiers.control
            && !key.modifiers.platform
        {
            return;
        }
        // Keys without a DOM equivalent stay with GPUI instead of failing the whole input.
        let Some((name, code)) = dom_key(&key.key) else {
            return;
        };
        self.send(BrowserCommand::Key {
            key: name,
            code,
            pressed,
            shift: key.modifiers.shift,
            control: key.modifiers.control,
            alt: key.modifiers.alt,
            meta: key.modifiers.platform,
        });
        window.prevent_default();
        cx.stop_propagation();
    }
}
/// Maps a GPUI key name to a DOM `KeyboardEvent.key` and `code`, or `None` when
/// the key has no DOM equivalent Servo would accept.
fn dom_key(key: &str) -> Option<(String, String)> {
    let named = match key {
        "space" => return Some((" ".into(), "Space".into())),
        "enter" => "Enter",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
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
        _ => "",
    };
    if !named.is_empty() {
        return Some((named.into(), named.into()));
    }
    if let Some(n) = key.strip_prefix('f')
        && let Ok(n) = n.parse::<u8>()
        && (1..=24).contains(&n)
    {
        return Some((format!("F{n}"), format!("F{n}")));
    }
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let code = if c.is_ascii_alphabetic() {
        format!("Key{}", c.to_ascii_uppercase())
    } else if c.is_ascii_digit() {
        format!("Digit{c}")
    } else {
        String::new()
    };
    Some((key.into(), code))
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
                .when(self.answered, |row| {
                    row.child(
                        div()
                            .id("artifact-answered")
                            .test_support()
                            .flex_none()
                            .child(Tag::secondary().outline().xsmall().child("Answered")),
                    )
                })
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
                            if pane.read(cx).docked {
                                menu = menu
                                    .menu(
                                        Command::MoveArtifactIntoConversation.label(),
                                        Command::MoveArtifactIntoConversation.action(),
                                    )
                                    .separator();
                            }
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
            .id(if self.docked {
                "docked-artifact-surface"
            } else if self.artifact.is_some() {
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
                    if this.position(event.position).is_none() {
                        return;
                    }
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
                    // A dock drop target around this pane handles the release in the
                    // bubble phase too; a dragged tab must still land on it.
                    if !cx.has_active_drag() {
                        cx.stop_propagation();
                    }
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
                if let Some((x, y)) = this.position(event.position) {
                    this.send(BrowserCommand::Move { x, y });
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                // A fitted artifact belongs to the transcript's scroll surface. Only the
                // bounded fallback viewport scrolls internally.
                if this.artifact.is_some()
                    && this.fixed_viewport.is_none()
                    && this
                        .content_height
                        .is_none_or(|height| height <= f32::from(this.bounds.size.height) + 1.)
                {
                    return;
                }
                let Some((x, y)) = this.position(event.position) else {
                    return;
                };
                let Some((_, scale)) = this.presentation() else {
                    return;
                };
                let (dx, dy) = match event.delta {
                    ScrollDelta::Pixels(p) => (-f32::from(p.x), -f32::from(p.y)),
                    ScrollDelta::Lines(p) => (-p.x * 40., -p.y * 40.),
                };
                this.send(BrowserCommand::Scroll {
                    x,
                    y,
                    dx: dx * scale,
                    dy: dy * scale,
                });
                cx.stop_propagation();
            }))
            .when_some(self.frame.clone(), |surface, frame| {
                surface.child(
                    img(frame)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain),
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
        self.send(BrowserCommand::Text { text: text.into() });
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
    use super::{
        POLL_ACTIVE, POLL_GRACE, POLL_IDLE, PollSchedule, bgra_image, dom_key, navigation_url,
    };
    use crate::browser::BrowserFrame;

    #[test]
    fn polling_backs_off_only_after_a_quiet_spell() {
        let mut schedule = PollSchedule::default();
        assert_eq!(schedule.next(true, true), POLL_ACTIVE);
        for _ in 0..POLL_GRACE {
            assert_eq!(schedule.next(false, true), POLL_ACTIVE);
        }
        assert_eq!(schedule.next(false, true), POLL_ACTIVE * 2);
        assert_eq!(schedule.next(false, true), POLL_ACTIVE * 4);
        for _ in 0..40 {
            assert!(schedule.next(false, true) <= POLL_IDLE);
        }
        assert_eq!(schedule.next(false, true), POLL_IDLE);
        schedule.wake();
        assert_eq!(schedule.next(false, true), POLL_ACTIVE, "input wakes it");
        assert_eq!(schedule.next(true, false), POLL_IDLE, "hidden panes idle");
        assert_eq!(schedule.next(false, true), POLL_ACTIVE, "shown again");
    }

    #[test]
    fn frames_convert_from_rgba_to_bgra() {
        let frame = BrowserFrame {
            revision: 1,
            document_revision: 1,
            scale: 1.,
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        let image = bgra_image(&frame).unwrap();
        assert_eq!(image.as_bytes(0).unwrap(), [3, 2, 1, 4, 7, 6, 5, 8]);
        let short = BrowserFrame {
            rgba: vec![0; 4],
            ..frame
        };
        assert!(
            bgra_image(&short).is_none(),
            "a frame that does not fit its size"
        );
    }

    #[test]
    fn gpui_key_names_map_to_dom_keys_servo_accepts() {
        for name in [
            "space",
            "enter",
            "backspace",
            "delete",
            "insert",
            "escape",
            "tab",
            "left",
            "right",
            "up",
            "down",
            "home",
            "end",
            "pageup",
            "pagedown",
            "f1",
            "f12",
            "a",
            "7",
            "-",
        ] {
            let (key, code) = dom_key(name).unwrap();
            crate::browser::check_dom_key(&key, &code)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        assert_eq!(dom_key("space"), Some((" ".into(), "Space".into())));
        assert_eq!(dom_key("f5"), Some(("F5".into(), "F5".into())));
        assert_eq!(dom_key("a"), Some(("a".into(), "KeyA".into())));
        assert_eq!(dom_key("f99"), None);
        assert_eq!(dom_key("menu"), None);
    }

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

#[cfg(all(test, feature = "runtime-qa"))]
mod drop_tests {
    use super::BrowserPane;
    use crate::browser::{BrowserId, BrowserRuntime};
    use gpui_kit::{
        AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
        StatefulInteractiveElement, Styled, TestSupportExt, Window, component::Root, div, px,
        test::TestWindowExt,
    };

    #[derive(Clone)]
    struct Payload;
    impl Render for Payload {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    struct DropZone {
        pane: Entity<BrowserPane>,
        dropped: bool,
    }
    impl Render for DropZone {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(
                    div()
                        .id("drag-source")
                        .test_support()
                        .h(px(40.))
                        .on_drag(Payload, |_, _, _, cx| cx.new(|_| Payload)),
                )
                .child(
                    div()
                        .id("drop-zone")
                        .w(px(400.))
                        .h(px(300.))
                        .on_drop(cx.listener(|this, _: &Payload, _, _| this.dropped = true))
                        .child(self.pane.clone()),
                )
        }
    }

    struct Host {
        pane: Option<Entity<BrowserPane>>,
    }
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().children(self.pane.clone())
        }
    }

    fn frame(revision: u64) -> std::sync::Arc<crate::browser::BrowserFrame> {
        std::sync::Arc::new(crate::browser::BrowserFrame {
            revision,
            document_revision: revision,
            scale: 1.,
            width: 4,
            height: 3,
            rgba: vec![revision as u8; 4 * 4 * 3],
        })
    }

    /// Each engine frame is a new atlas image. The replaced one leaves the sprite atlas
    /// as soon as its successor is installed, and the last one when the pane goes.
    #[gpui_kit::test]
    fn replaced_and_released_frames_leave_the_sprite_atlas(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let runtime = BrowserRuntime::new();
        runtime.shutdown();
        let mut pane = None;
        let window = cx.add_window(|window, cx| {
            let browser = cx.new(|cx| {
                BrowserPane::new(
                    BrowserId::new_v4(),
                    "about:blank".into(),
                    runtime,
                    window,
                    cx,
                )
            });
            pane = Some(browser.clone());
            let host = cx.new(|_| Host {
                pane: Some(browser),
            });
            Root::new(host, window, cx)
        });
        let pane = pane.unwrap();
        let show = |cx: &mut gpui_kit::TestAppContext, revision| {
            cx.update_window(window.into(), |_, window, cx| {
                pane.update(cx, |pane, cx| pane.show_frame(frame(revision), window, cx));
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                pane.read(cx).frame.clone().unwrap()
            })
            .unwrap()
        };
        let in_atlas = |cx: &mut gpui_kit::TestAppContext, image: &gpui_kit::RenderImage| {
            cx.update_window(window.into(), |_, window, _| {
                window.has_image_atlas_entry(image)
            })
            .unwrap()
        };
        let first = show(cx, 1);
        assert!(in_atlas(cx, &first), "the painted frame is in the atlas");
        let second = show(cx, 2);
        assert!(!std::sync::Arc::ptr_eq(&first, &second));
        assert!(!in_atlas(cx, &first), "the replaced frame was dropped");
        assert!(in_atlas(cx, &second));
        let host = cx
            .update_window(window.into(), |root, _, cx| {
                root.downcast::<Root>()
                    .unwrap()
                    .read(cx)
                    .view()
                    .clone()
                    .downcast::<Host>()
                    .unwrap()
            })
            .unwrap();
        cx.update(|cx| {
            host.update(cx, |host, cx| {
                host.pane = None;
                cx.notify();
            })
        });
        drop(pane);
        // The last drawn frame still holds the pane's view until the next draw.
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        cx.run_until_parked();
        assert!(!in_atlas(cx, &second), "a released pane frees its frame");
    }

    /// A pane whose notifies draw no surface stops converting frames and polls at the
    /// idle rate; laying it out again resumes both.
    #[gpui_kit::test]
    fn hidden_panes_stop_converting_frames_until_shown(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let runtime = BrowserRuntime::new();
        runtime.shutdown();
        let engine = runtime.clone();
        let mut pane = None;
        let window = cx.add_window(|window, cx| {
            let browser = cx.new(|cx| {
                BrowserPane::new(
                    BrowserId::new_v4(),
                    "about:blank".into(),
                    engine,
                    window,
                    cx,
                )
            });
            pane = Some(browser);
            Root::new(cx.new(|_| Host { pane: None }), window, cx)
        });
        let pane = pane.unwrap();
        let poll = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                pane.update(cx, |pane, cx| {
                    // As if this poll's refresh found a change and notified.
                    pane.notified = true;
                    pane.poll(window, cx)
                })
            })
            .unwrap()
        };
        let draw = |cx: &mut gpui_kit::TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
        };
        let id = pane.read_with(cx, |pane, _| pane.id);
        assert_eq!(runtime.is_visible(id), Some(true), "panes open shown");
        assert_eq!(poll(cx), super::POLL_ACTIVE, "a notify gets a poll to draw");
        assert_eq!(poll(cx), super::POLL_IDLE, "nothing drew the surface");
        assert!(!pane.read_with(cx, |pane, _| pane.visible));
        assert_eq!(
            runtime.is_visible(id),
            Some(false),
            "the engine stops publishing frames nobody sees"
        );
        cx.update_window(window.into(), |_, window, cx| {
            pane.update(cx, |pane, cx| pane.show_frame(frame(1), window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            pane.read_with(cx, |pane, _| pane.frame.is_none()),
            "a hidden pane skips frames"
        );
        let host = cx
            .update_window(window.into(), |root, _, cx| {
                root.downcast::<Root>()
                    .unwrap()
                    .read(cx)
                    .view()
                    .clone()
                    .downcast::<Host>()
                    .unwrap()
            })
            .unwrap();
        let shown = pane.clone();
        cx.update(|cx| host.update(cx, |host, _| host.pane = Some(shown)));
        draw(cx);
        assert!(pane.read_with(cx, |pane, _| pane.visible), "laid out again");
        assert_eq!(
            runtime.is_visible(id),
            Some(true),
            "and told the engine at once"
        );
        assert_eq!(poll(cx), super::POLL_ACTIVE);
        draw(cx);
        assert_eq!(poll(cx), super::POLL_ACTIVE, "drawn surfaces stay visible");
        cx.update_window(window.into(), |_, window, cx| {
            pane.update(cx, |pane, cx| pane.show_frame(frame(2), window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| pane.frame.is_some()));
    }

    /// Answering collapses an artifact, which hides its page from the engine; the
    /// ToggleArtifact command shows it again.
    #[gpui_kit::test]
    fn collapsed_artifacts_are_hidden_from_the_engine(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let runtime = BrowserRuntime::new();
        runtime.shutdown();
        let engine = runtime.clone();
        let id = BrowserId::new_v4();
        let mut pane = None;
        let window = cx.add_window(|window, cx| {
            let browser = cx.new(|cx| {
                BrowserPane::artifact(
                    id,
                    "about:blank".into(),
                    "Scope".into(),
                    ":root{}".into(),
                    engine,
                    window,
                    cx,
                )
            });
            pane = Some(browser.clone());
            Root::new(
                cx.new(|_| Host {
                    pane: Some(browser),
                }),
                window,
                cx,
            )
        });
        let pane = pane.unwrap();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        assert_eq!(runtime.is_visible(id), Some(true));
        cx.update(|cx| pane.update(cx, |pane, cx| pane.complete(cx)));
        assert_eq!(
            runtime.is_visible(id),
            Some(false),
            "answered and collapsed"
        );
        cx.update_window(window.into(), |_, window, cx| {
            pane.update(cx, |pane, cx| {
                pane.command(super::Command::ToggleArtifact, window, cx)
            });
        })
        .unwrap();
        assert_eq!(runtime.is_visible(id), Some(true), "reopened");
    }

    /// A dock tab dropped over a displayed browser reaches the tab group's drop
    /// handler, which runs after the surface's own mouse-up in the bubble phase.
    #[gpui_kit::test]
    fn drags_dropped_on_the_web_surface_reach_the_drop_target(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::ui::theme::initialize(None, None, cx);
        });
        let runtime = BrowserRuntime::new();
        // No engine worker: the pane keeps its session state without Servo.
        runtime.shutdown();
        let window = cx.add_window(|window, cx| {
            let pane = cx.new(|cx| {
                BrowserPane::new(
                    BrowserId::new_v4(),
                    "about:blank".into(),
                    runtime,
                    window,
                    cx,
                )
            });
            let zone = cx.new(|_| DropZone {
                pane,
                dropped: false,
            });
            Root::new(zone, window, cx)
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.drag_to("drag-source", "servo-surface", cx);
        })
        .unwrap();
        let dropped = cx
            .update_window(window.into(), |root, _, cx| {
                root.downcast::<Root>()
                    .unwrap()
                    .read(cx)
                    .view()
                    .clone()
                    .downcast::<DropZone>()
                    .unwrap()
                    .read(cx)
                    .dropped
            })
            .unwrap();
        assert!(dropped, "the browser surface swallowed the drop");
    }
}
