//! All Servo, WebView and rendering handles are created, pumped and dropped on
//! the browser worker. Only bounded commands and retained snapshots cross threads.
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use content_security_policy::Destination;
use dpi::PhysicalSize;
use euclid::{Box2D, Point2D, Scale, Size2D};
use serde_json::Value;
use servo::{
    Code, CompositionEvent, CompositionState, ConsoleLogLevel, EventLoopWaker, ImeEvent,
    InputEvent, JSValue, Key, KeyState, KeyboardEvent, LoadStatus, Location, Modifiers,
    MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, Opts, Preferences,
    RenderingContext, Servo, ServoBuilder, ServoDelegate, ServoError, SoftwareRenderingContext,
    UserContentManager, WebResourceLoad, WebResourceResponse, WebView, WebViewBuilder,
    WebViewDelegate, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
};
use url::Url;
use uuid::Uuid;

use super::command::{self, BrowserCommand, Event, PageState, Request};
use super::{Shared, apply_event, fail};
use closing::{CLEAR_STORAGE_DOCUMENT, CLEAR_STORAGE_PATH, ClosingPage, storage_origin};
use profile::EngineProfile;

mod closing;
mod profile;

const MAX_WEBVIEWS: usize = 16;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const CONTENT_MEASUREMENT_INTERVAL: Duration = Duration::from_millis(250);

// Do not use document/body.scrollHeight: the root scrolling element is at least
// viewport-high, so it cannot report a shrink after the embedder grows the viewport.
// A marked, naturally-sized root is authoritative. Ordinary body content uses
// child rectangles and text-node ranges, never a range selecting the whole body:
// Servo 0.7 returns that range's container box, including its viewport-height floor.
// Neither path changes page styles or installs timers/observers inside the page.
const CONTENT_HEIGHT_SCRIPT: &str = r#"(() => {
    const body = document.body;
    if (!body) return null;
    const root = document.querySelector('[data-drostje-artifact]') || body;
    const box = root.getBoundingClientRect();
    const style = getComputedStyle(root);
    const pixels = value => parseFloat(value) || 0;
    let bottom = box.bottom;
    if (root === body) {
        bottom = box.top + pixels(style.paddingTop) + pixels(style.borderTopWidth);
        const range = document.createRange();
        const include = node => {
            if (node.nodeType === Node.TEXT_NODE && node.textContent.trim()) {
                range.selectNodeContents(node);
                bottom = Math.max(bottom, range.getBoundingClientRect().bottom);
            } else if (node.nodeType === Node.ELEMENT_NODE) {
                const childStyle = getComputedStyle(node);
                if (childStyle.display === 'contents') {
                    for (const child of node.childNodes) include(child);
                } else if (node.getClientRects().length) {
                    bottom = Math.max(bottom, node.getBoundingClientRect().bottom
                        + Math.max(0, pixels(childStyle.marginBottom)));
                }
            }
        };
        for (const child of body.childNodes) include(child);
        bottom += pixels(style.paddingBottom) + pixels(style.borderBottomWidth);
    } else {
        bottom = Math.max(bottom, box.top + root.scrollHeight
            + pixels(style.borderTopWidth) + pixels(style.borderBottomWidth));
        const bodyStyle = getComputedStyle(body);
        const inset = pixels(bodyStyle.paddingBottom) + pixels(bodyStyle.borderBottomWidth);
        const bodyMargin = Math.max(0, pixels(bodyStyle.marginBottom));
        const rootMargin = Math.max(0, pixels(style.marginBottom));
        bottom += inset + (inset ? bodyMargin + rootMargin : Math.max(bodyMargin, rootMargin));
    }
    if (root === body) bottom += Math.max(0, pixels(style.marginBottom));
    return Math.ceil(Math.max(0, bottom + window.scrollY));
})()"#;

struct ContentSizing {
    enabled: bool,
    generation: u64,
    pending: bool,
    next_measurement: Instant,
}

impl ContentSizing {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            generation: 0,
            pending: false,
            next_measurement: Instant::now(),
        }
    }

    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.next_measurement = Instant::now();
        // Keep the outstanding slot occupied until Servo calls it back, even
        // across navigation or disable/re-enable. Never pile up native evaluations.
    }

    fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.invalidate();
        }
    }

    fn begin(&mut self, now: Instant) -> Option<u64> {
        if !self.enabled || self.pending || now < self.next_measurement {
            return None;
        }
        self.pending = true;
        self.next_measurement = now + CONTENT_MEASUREMENT_INTERVAL;
        Some(self.generation)
    }

    fn finish(&mut self, generation: u64) -> bool {
        self.pending = false;
        self.enabled && self.generation == generation
    }
}

struct Output(Arc<Shared>);

impl Output {
    fn send(&self, event: Event, bytes: Vec<u8>) {
        let result = apply_event(&mut self.0.memory.lock(), event, bytes);
        if let Err(error) = result {
            fail(&self.0, error.to_string());
        }
    }
}

#[derive(Clone)]
struct Waker(thread::Thread);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        self.0.unpark();
    }
}

struct EngineDelegate(Arc<Output>);

impl ServoDelegate for EngineDelegate {
    fn notify_error(&self, error: ServoError) {
        self.0.send(
            Event::Fatal {
                error: format!("Servo engine failed: {error:?}"),
            },
            vec![],
        );
    }
}

struct Delegate {
    id: Uuid,
    state: RefCell<PageState>,
    dirty: Cell<bool>,
    content_sizing: RefCell<ContentSizing>,
    output: Arc<Output>,
    frame_revision: Cell<u64>,
    frame_generation: Cell<u64>,
    frame_current: Cell<bool>,
    capture_pending: Cell<bool>,
    /// Storage origins of every document this surface loaded, frames included.
    loaded_origins: RefCell<Vec<Url>>,
    closing: Cell<bool>,
}

impl Delegate {
    fn update(&self, change: impl FnOnce(&mut PageState)) {
        let mut state = self.state.borrow_mut();
        change(&mut state);
        self.output.send(
            Event::State {
                id: self.id,
                state: state.clone(),
            },
            vec![],
        );
    }

    fn error(&self, error: String) {
        self.update(|state| {
            state.error = Some(error);
            state.loading = false;
        });
    }

    fn invalidate_content_height(&self) {
        self.content_sizing.borrow_mut().invalidate();
        if self.state.borrow().content_height.is_some() {
            self.update(|state| state.content_height = None);
        }
    }

    fn invalidate_frame(&self) {
        self.frame_generation
            .set(self.frame_generation.get().wrapping_add(1));
        self.frame_current.set(false);
        self.dirty.set(false);
    }

    fn record_document(&self, url: &Url) {
        if let Some(origin) = storage_origin(url) {
            let mut origins = self.loaded_origins.borrow_mut();
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }
    }

    fn has_loaded_origin(&self, origin: &Url) -> bool {
        self.loaded_origins.borrow().contains(origin)
    }

    fn diagnostic(&self, kind: &'static str, level: String, message: String) {
        self.output.send(
            Event::Diagnostic {
                id: self.id,
                kind,
                level,
                message,
            },
            vec![],
        );
    }

    fn frame(&self, image: servo::RgbaImage, scale: f32, document_revision: u64) {
        let revision = self.frame_revision.get() + 1;
        self.frame_revision.set(revision);
        self.output.send(
            Event::Frame {
                id: self.id,
                revision,
                document_revision,
                scale,
                width: image.width(),
                height: image.height(),
            },
            image.into_raw(),
        );
    }
}

impl WebViewDelegate for Delegate {
    fn notify_url_changed(&self, _: WebView, url: Url) {
        self.record_document(&url);
        self.invalidate_content_height();
        self.update(|state| state.url = url.to_string());
    }

    fn notify_page_title_changed(&self, _: WebView, title: Option<String>) {
        self.update(|state| state.title = title.unwrap_or_default());
    }

    fn notify_load_status_changed(&self, _: WebView, status: LoadStatus) {
        if status != LoadStatus::Complete {
            self.invalidate_content_height();
        }
        if status == LoadStatus::Started {
            self.invalidate_frame();
        }
        self.update(|state| {
            if status == LoadStatus::Started {
                state.document_revision += 1;
            }
            state.loading = status != LoadStatus::Complete;
        });
        if status == LoadStatus::Complete {
            self.dirty.set(true);
        }
    }

    fn notify_history_changed(&self, _: WebView, entries: Vec<Url>, current: usize) {
        self.update(|state| {
            state.can_go_back = current > 0;
            state.can_go_forward = current + 1 < entries.len();
        });
    }

    fn notify_traversal_complete(&self, webview: WebView, _: servo::TraversalId) {
        // Cached history traversal need not emit a load-status change.
        self.invalidate_content_height();
        self.invalidate_frame();
        self.update(|state| {
            state.document_revision += 1;
            state.loading = webview.load_status() != LoadStatus::Complete;
        });
        self.dirty.set(true);
    }

    fn notify_new_frame_ready(&self, _: WebView) {
        self.dirty.set(true);
    }

    fn notify_crashed(&self, _: WebView, reason: String, backtrace: Option<String>) {
        if let Some(backtrace) = backtrace {
            eprintln!("Servo page crash: {backtrace}");
        }
        self.error(format!(
            "Servo page crashed: {reason}. Reload the page to try again"
        ));
    }

    fn notify_closed(&self, _: WebView) {
        self.error("The page closed itself. Navigate to a URL to open another page".into());
    }

    fn show_console_message(&self, _: WebView, level: ConsoleLogLevel, message: String) {
        if let Some(encoded) = message.strip_prefix("__DROSTJE_DIAGNOSTIC__")
            && encoded.len() <= 16 * 1024
            && let Ok(event) = serde_json::from_str::<Value>(encoded)
            && let Some(kind) = event["kind"].as_str()
        {
            match kind {
                "network" => self.diagnostic("network", "error".into(), encoded.into()),
                "runtime" => self.diagnostic("runtime", "error".into(), encoded.into()),
                _ => self.diagnostic("console", format!("{level:?}"), message),
            }
        } else {
            self.diagnostic("console", format!("{level:?}"), message);
        }
    }

    fn load_web_resource(&self, _: WebView, load: WebResourceLoad) {
        let request = load.request();
        if self.closing.get() {
            if request.is_for_main_frame && request.url.path() == CLEAR_STORAGE_PATH {
                let url = request.url.clone();
                let mut response = load.intercept(WebResourceResponse::new(url));
                response.send_body_data(CLEAR_STORAGE_DOCUMENT.to_vec());
                response.finish();
            }
            // A closing surface is invisible; its loads are not diagnostics.
            return;
        }
        if matches!(
            request.destination,
            Destination::Document
                | Destination::Frame
                | Destination::IFrame
                | Destination::Object
                | Destination::Embed
        ) {
            self.record_document(&request.url);
        }
        self.diagnostic(
            "request",
            "info".into(),
            format!("{} {}", load.request().method, load.request().url),
        );
        // Dropping the responder explicitly leaves Servo's network request intact.
    }
}

struct Page {
    view: WebView,
    context: Rc<SoftwareRenderingContext>,
    delegate: Rc<Delegate>,
    last_frame: Instant,
    scale: f32,
}

impl Page {
    fn new(servo: &Servo, output: Arc<Output>, id: Uuid, url: Url) -> Result<Self, String> {
        let context = Rc::new(SoftwareRenderingContext::new(PhysicalSize::new(800, 600))
            .map_err(|error| format!("Could not initialize Servo software rendering: {error:?}. On Linux install Mesa EGL/GL software drivers and run in a desktop session"))?);
        context.make_current().map_err(|error| {
            format!("Could not make Servo rendering context current: {error:?}")
        })?;
        // Retain the opt-in even when an earlier open failed (for example a local
        // artifact was missing), so navigating to a valid file restores sizing.
        let sizing_enabled = {
            let memory = output.0.memory.lock();
            memory
                .wire_ids
                .get(&id)
                .and_then(|id| memory.sessions.get(id))
                .is_some_and(|session| session.content_sizing)
        };
        let delegate = Rc::new(Delegate {
            id,
            state: RefCell::new(PageState {
                url: url.to_string(),
                loading: true,
                viewport_width: 800,
                viewport_height: 600,
                scale: 1.0,
                ..PageState::default()
            }),
            dirty: Cell::new(false),
            content_sizing: RefCell::new(ContentSizing::new(sizing_enabled)),
            output,
            frame_revision: Cell::new(0),
            frame_generation: Cell::new(0),
            frame_current: Cell::new(false),
            capture_pending: Cell::new(false),
            loaded_origins: RefCell::default(),
            closing: Cell::new(false),
        });
        let user_contents = Rc::new(UserContentManager::new(servo));
        user_contents.add_script(Rc::new(include_str!("diagnostics.js").into()));
        let view = WebViewBuilder::new(servo, context.clone())
            .url(url)
            .delegate(delegate.clone())
            .user_content_manager(user_contents)
            .build();
        view.show();
        delegate.update(|_| {});
        Ok(Self {
            view,
            context,
            delegate,
            last_frame: Instant::now() - FRAME_INTERVAL,
            scale: 1.0,
        })
    }

    fn point(&self, x: f32, y: f32) -> Result<WebViewPoint, String> {
        if !x.is_finite() || !y.is_finite() {
            return Err("Browser input coordinates must be finite".into());
        }
        Ok(WebViewPoint::Device(Point2D::new(
            x * self.scale,
            y * self.scale,
        )))
    }

    fn command(&mut self, command: BrowserCommand) -> Result<(), String> {
        match command {
            BrowserCommand::Navigate { url } => {
                let url = parse_url(&url)?;
                self.delegate.invalidate_content_height();
                self.delegate.invalidate_frame();
                self.delegate.update(|state| {
                    state.url = url.to_string();
                    state.error = None;
                    state.loading = true;
                });
                self.view.load(url);
            }
            BrowserCommand::Back => {
                self.delegate.update(|state| {
                    state.error = None;
                });
                if self.delegate.state.borrow().can_go_back {
                    self.delegate.invalidate_content_height();
                    self.delegate.invalidate_frame();
                    self.delegate.update(|state| state.loading = true);
                    self.view.go_back(1);
                }
            }
            BrowserCommand::Forward => {
                self.delegate.update(|state| {
                    state.error = None;
                });
                if self.delegate.state.borrow().can_go_forward {
                    self.delegate.invalidate_content_height();
                    self.delegate.invalidate_frame();
                    self.delegate.update(|state| state.loading = true);
                    self.view.go_forward(1);
                }
            }
            BrowserCommand::Reload => {
                // A local artifact may have been removed since its previous load.
                if let Some(url) = self.view.url() {
                    parse_url(url.as_str())?;
                }
                self.delegate.invalidate_content_height();
                self.delegate.invalidate_frame();
                self.delegate.update(|state| {
                    state.error = None;
                    state.loading = true;
                    // Servo::WebView::reload sets Started internally without
                    // notifying its delegate, so invalidate this identity here.
                    state.document_revision += 1;
                });
                self.view.reload();
            }
            BrowserCommand::Resize {
                width,
                height,
                scale,
            } => {
                command::frame_len(width, height).map_err(|error| error.to_string())?;
                if !scale.is_finite() || !(0.25..=8.0).contains(&scale) {
                    return Err("Browser scale must be between 0.25 and 8".into());
                }
                let size = PhysicalSize::new(width, height);
                if self.context.size() != size || self.scale != scale {
                    self.context
                        .make_current()
                        .map_err(|error| format!("Could not resize Servo surface: {error:?}"))?;
                    self.delegate.content_sizing.borrow_mut().invalidate();
                    self.delegate.invalidate_frame();
                    self.scale = scale;
                    self.view.set_hidpi_scale_factor(Scale::new(scale));
                    self.view.resize(size);
                    self.delegate.update(|state| {
                        state.viewport_width = width;
                        state.viewport_height = height;
                        state.scale = scale;
                    });
                    self.delegate.dirty.set(true);
                }
            }
            BrowserCommand::SetContentSizing { enabled } => {
                self.delegate
                    .content_sizing
                    .borrow_mut()
                    .set_enabled(enabled);
            }
            BrowserCommand::Pointer {
                x,
                y,
                button,
                pressed,
            } => {
                let point = self.point(x, y)?;
                if pressed {
                    self.view.set_focused(true);
                }
                self.view
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point)));
                self.view
                    .notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                        if pressed {
                            MouseButtonAction::Down
                        } else {
                            MouseButtonAction::Up
                        },
                        MouseButton::from(button),
                        point,
                    )));
            }
            BrowserCommand::Move { x, y } => {
                self.view
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(
                        self.point(x, y)?,
                    )));
            }
            BrowserCommand::Scroll { x, y, dx, dy } => {
                let point = self.point(x, y)?;
                if !dx.is_finite() || !dy.is_finite() {
                    return Err("Scroll deltas must be finite".into());
                }
                // Servo wheel deltas have the opposite sign of DOM scroll offsets,
                // and its default wheel handler expects device-pixel deltas.
                self.view
                    .notify_input_event(InputEvent::Wheel(WheelEvent::new(
                        WheelDelta {
                            x: -f64::from(dx * self.scale),
                            y: -f64::from(dy * self.scale),
                            z: 0.0,
                            mode: WheelMode::DeltaPixel,
                        },
                        point,
                    )));
            }
            BrowserCommand::Key {
                key,
                code,
                pressed,
                shift,
                control,
                alt,
                meta,
            } => {
                let key = key
                    .parse::<Key>()
                    .map_err(|error| format!("Invalid DOM key {key:?}: {error:?}"))?;
                let code = if code.is_empty() {
                    Code::Unidentified
                } else {
                    code.parse::<Code>()
                        .map_err(|error| format!("Invalid DOM code {code:?}: {error:?}"))?
                };
                let mut modifiers = Modifiers::empty();
                modifiers.set(Modifiers::SHIFT, shift);
                modifiers.set(Modifiers::CONTROL, control);
                modifiers.set(Modifiers::ALT, alt);
                modifiers.set(Modifiers::META, meta);
                self.view.notify_input_event(InputEvent::Keyboard(
                    KeyboardEvent::new_without_event(
                        if pressed {
                            KeyState::Down
                        } else {
                            KeyState::Up
                        },
                        key,
                        code,
                        Location::Standard,
                        modifiers,
                        false,
                        false,
                    ),
                ));
            }
            BrowserCommand::Text { text } => {
                if !text.is_empty() {
                    self.view
                        .notify_input_event(InputEvent::Ime(ImeEvent::Composition(
                            CompositionEvent {
                                state: CompositionState::Start,
                                data: String::new(),
                            },
                        )));
                    self.view
                        .notify_input_event(InputEvent::Ime(ImeEvent::Composition(
                            CompositionEvent {
                                state: CompositionState::End,
                                data: text,
                            },
                        )));
                }
            }
            BrowserCommand::Focus { focused } => self.view.set_focused(focused),
            BrowserCommand::Evaluate { request_id, script } => {
                let delegate = self.delegate.clone();
                let id = delegate.id;
                self.view.evaluate_javascript(script, move |result| {
                    let result = result
                        .map(js_value)
                        .map_err(|error| format!("JavaScript evaluation failed: {error:?}"));
                    if let Err(error) = &result {
                        delegate.diagnostic("evaluation", "error".into(), error.clone());
                    }
                    delegate.output.send(
                        Event::Evaluation {
                            id,
                            request_id,
                            result,
                        },
                        vec![],
                    );
                });
            }
        }
        Ok(())
    }

    fn measure_content_if_due(&self) {
        {
            let state = self.delegate.state.borrow();
            if state.loading || state.error.is_some() {
                return;
            }
        }
        let Some(generation) = self
            .delegate
            .content_sizing
            .borrow_mut()
            .begin(Instant::now())
        else {
            return;
        };
        let delegate = Rc::downgrade(&self.delegate);
        self.view
            .evaluate_javascript(CONTENT_HEIGHT_SCRIPT, move |result| {
                let Some(delegate) = delegate.upgrade() else {
                    return;
                };
                if !delegate.content_sizing.borrow_mut().finish(generation) {
                    return;
                }
                let Ok(JSValue::Number(height)) = result else {
                    // Internal measurements must not consume tool evaluation IDs or
                    // replace the page's loading/error state if a document disappears.
                    return;
                };
                let height = height as f32;
                if !height.is_finite() || height < 0.0 {
                    return;
                }
                let state = delegate.state.borrow();
                let changed =
                    !state.loading && state.error.is_none() && state.content_height != Some(height);
                drop(state);
                if changed {
                    delegate.update(|state| state.content_height = Some(height));
                }
            });
    }

    fn paint_if_dirty(&mut self) {
        if !self.delegate.dirty.get()
            || self.delegate.state.borrow().loading
            || self.last_frame.elapsed() < FRAME_INTERVAL
        {
            return;
        }
        if !self.delegate.frame_current.get() {
            if self.delegate.capture_pending.replace(true) {
                // Readiness requests trigger a repaint; the callback is delivered
                // during paint, not spin_event_loop. Do not publish unverified pixels.
                self.delegate.dirty.set(false);
                self.last_frame = Instant::now();
                if let Err(error) = self.context.make_current() {
                    self.delegate
                        .error(format!("Could not activate Servo framebuffer: {error:?}"));
                    return;
                }
                self.view.paint();
                return;
            }
            self.delegate.dirty.set(false);
            self.last_frame = Instant::now();
            let delegate = self.delegate.clone();
            let generation = delegate.frame_generation.get();
            let scale = self.scale;
            let document_revision = delegate.state.borrow().document_revision;
            // Servo's screenshot barrier waits for the current pipeline's layout
            // and compositor frames. A normal paint may still contain old pixels.
            self.view.take_screenshot(None, move |result| {
                delegate.capture_pending.set(false);
                if delegate.frame_generation.get() != generation || delegate.state.borrow().loading
                {
                    delegate.dirty.set(true);
                    return;
                }
                match result {
                    Ok(image) => {
                        delegate.frame_current.set(true);
                        delegate.frame(image, scale, document_revision);
                    }
                    Err(error) => delegate.error(format!(
                        "Could not capture current Servo document: {error:?}"
                    )),
                }
            });
            return;
        }
        self.delegate.dirty.set(false);
        self.last_frame = Instant::now();
        if let Err(error) = self.context.make_current() {
            self.delegate
                .error(format!("Could not activate Servo framebuffer: {error:?}"));
            return;
        }
        self.view.paint();
        let size = self.context.size();
        let rect = Box2D::from_origin_and_size(
            Point2D::new(0, 0),
            Size2D::new(size.width as i32, size.height as i32),
        );
        let Some(image) = self.context.read_to_image(rect) else {
            self.delegate
                .error("Could not read pixels from Servo's software framebuffer".into());
            return;
        };
        let document_revision = self.delegate.state.borrow().document_revision;
        self.delegate.frame(image, self.scale, document_revision);
    }
}

fn validate_command(command: &BrowserCommand) -> Result<(), String> {
    let point = |x: f32, y: f32| {
        if x.is_finite() && y.is_finite() {
            Ok(())
        } else {
            Err("Browser input coordinates must be finite".to_owned())
        }
    };
    match command {
        BrowserCommand::Navigate { url } => {
            parse_url(url)?;
        }
        BrowserCommand::Resize {
            width,
            height,
            scale,
        } => {
            command::frame_len(*width, *height).map_err(|error| error.to_string())?;
            if !scale.is_finite() || !(0.25..=8.0).contains(scale) {
                return Err("Browser scale must be between 0.25 and 8".into());
            }
        }
        BrowserCommand::Pointer { x, y, button, .. } => {
            point(*x, *y)?;
            if *button > 4 {
                return Err("Mouse button must be between 0 and 4".into());
            }
        }
        BrowserCommand::Move { x, y } => point(*x, *y)?,
        BrowserCommand::Scroll { x, y, dx, dy } => {
            point(*x, *y)?;
            point(*dx, *dy)?;
        }
        BrowserCommand::Key { key, code, .. } => {
            key.parse::<Key>()
                .map_err(|error| format!("Invalid DOM key {key:?}: {error:?}"))?;
            if !code.is_empty() {
                code.parse::<Code>()
                    .map_err(|error| format!("Invalid DOM code {code:?}: {error:?}"))?;
            }
        }
        BrowserCommand::Evaluate { .. } => {
            return Err("Evaluation cannot be batched with input".into());
        }
        _ => {}
    }
    Ok(())
}

fn deliver_commands(
    commands: Vec<BrowserCommand>,
    mut deliver: impl FnMut(BrowserCommand) -> Result<(), String>,
) -> Result<Value, String> {
    // Validate the entire batch before any input is delivered. Runtime surface
    // failures cannot be rolled back; report the exact delivered prefix instead.
    for command in &commands {
        validate_command(command)?;
    }
    let count = commands.len();
    for (index, command) in commands.into_iter().enumerate() {
        deliver(command).map_err(|error| {
            format!(
                "Browser batch failed after {index} of {count} commands were delivered: {error}"
            )
        })?;
    }
    Ok(serde_json::json!({"status": "delivered", "delivered": count}))
}

fn parse_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|error| {
        format!(
            "Invalid browser URL {value:?}: {error}. Use an absolute http:, https: or file: URL"
        )
    })?;
    if url.scheme() == "file" {
        let path = url
            .to_file_path()
            .map_err(|()| "The file URL does not refer to a local path".to_owned())?;
        let metadata = std::fs::metadata(&path)
            .map_err(|error| format!("Cannot open HTML artifact {}: {error}. Restore the file or select an existing HTML file", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("HTML artifact {} is not a file", path.display()));
        }
    }
    Ok(url)
}

fn js_value(value: JSValue) -> Value {
    match value {
        JSValue::Undefined | JSValue::Null => Value::Null,
        JSValue::Boolean(value) => Value::Bool(value),
        JSValue::Number(value) => {
            serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
        }
        JSValue::String(value) => Value::String(value),
        JSValue::Element(value) => {
            serde_json::json!({ "element-6066-11e4-a52e-4f735466cecf": value })
        }
        JSValue::ShadowRoot(value) => {
            serde_json::json!({ "shadow-6066-11e4-a52e-4f735466cecf": value })
        }
        JSValue::Frame(value) => serde_json::json!({ "frame-075b-4da1-b6ba-e579c2d3230a": value }),
        JSValue::Window(value) => {
            serde_json::json!({ "window-fcc6-11e5-b4f8-330a88ab9d7f": value })
        }
        JSValue::Array(values) => Value::Array(values.into_iter().map(js_value).collect()),
        JSValue::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, js_value(value)))
                .collect(),
        ),
    }
}

pub(super) fn run(shared: &Arc<Shared>) -> Result<(), String> {
    // Respect a provider already selected by the host. Concurrent installers may
    // win the race; either way Rustls has a process-wide provider before Servo starts.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let output = Arc::new(Output(shared.clone()));
    // WebGPU runs on Servo's own wgpu device and presents through WebRender
    // images, so it does not depend on the software GL rendering context.
    // GPU canvas pages routinely pair it with OffscreenCanvas for 2D scratch work.
    let preferences = Preferences {
        dom_webgpu_enabled: true,
        dom_offscreen_canvas_enabled: true,
        // Storage lives in the per-run profile; closing.rs deletes it per closed surface.
        dom_indexeddb_enabled: true,
        ..Preferences::default()
    };
    // Servo does not expose this Stylo pref. Without it a selector list containing
    // :has() invalidates its whole rule, which drops DaisyUI's theme variables.
    // Servo 0.7 matches :has() when an element is styled but does not yet
    // invalidate it on later DOM or state changes.
    stylo_static_prefs::set_pref!("layout.css.has-selector.enabled", true);
    let profile = EngineProfile::create()
        .map_err(|error| format!("Could not create Servo's storage directory: {error}"))?;
    let opts = Opts {
        config_dir: Some(profile.path().to_path_buf()),
        ..Opts::default()
    };
    let servo = ServoBuilder::default()
        .opts(opts)
        .preferences(preferences)
        .event_loop_waker(Box::new(Waker(thread::current())))
        .build();
    // Do not call Servo::setup_logging: it replaces the host's process-wide logger.
    servo.set_delegate(Rc::new(EngineDelegate(output.clone())));
    let mut pages = HashMap::<Uuid, Page>::new();
    let mut closing = Vec::<ClosingPage>::new();
    let mut batch = VecDeque::<Request>::with_capacity(64);
    loop {
        {
            let mut memory = shared.memory.lock();
            if memory.shutdown || memory.failure.is_some() {
                break;
            }
            for evaluation in memory
                .evaluations
                .values_mut()
                .filter(|evaluation| evaluation.result.is_none())
            {
                if evaluation.started.elapsed() > Duration::from_secs(30) {
                    evaluation.result = Some(Err("Browser request did not finish within 30 seconds; the page may be busy or navigating".into()));
                }
            }
            let length = memory.queue.len().min(64);
            batch.extend(memory.queue.drain(..length));
        }
        // Closing is an identity removal, not an unbounded lifecycle queue. Retire
        // obsolete WebViews here, never under the snapshot lock or on the UI thread.
        let closed: Vec<Uuid> = {
            let memory = shared.memory.lock();
            pages
                .keys()
                .filter(|id| !memory.wire_ids.contains_key(id))
                .copied()
                .collect()
        };
        for id in closed {
            if let Some(page) = pages.remove(&id).and_then(ClosingPage::new) {
                closing.push(page);
            }
        }
        while let Some(request) = batch.pop_front() {
            {
                let memory = shared.memory.lock();
                if memory.shutdown || memory.failure.is_some() {
                    break;
                }
                if !memory.wire_ids.contains_key(&request.id()) {
                    continue;
                }
            }
            match request {
                Request::Open { id, url } => {
                    if let Some(page) = pages.get_mut(&id) {
                        if let Err(error) = page.command(BrowserCommand::Navigate { url }) {
                            page.delegate.error(error);
                        }
                        continue;
                    }
                    let result = if pages.len() >= MAX_WEBVIEWS {
                        Err(format!(
                            "At most {MAX_WEBVIEWS} Servo pages can be open at once; close another browser first"
                        ))
                    } else {
                        parse_url(&url).and_then(|url| Page::new(&servo, output.clone(), id, url))
                    };
                    match result {
                        Ok(page) => {
                            pages.insert(id, page);
                        }
                        Err(error) => output.send(
                            Event::State {
                                id,
                                state: PageState {
                                    url,
                                    error: Some(error),
                                    ..PageState::default()
                                },
                            },
                            vec![],
                        ),
                    }
                }
                Request::Command { id, command } => {
                    if let Some(page) = pages.get_mut(&id) {
                        if let Err(error) = page.command(command) {
                            page.delegate.error(error);
                        }
                    } else {
                        match command {
                            BrowserCommand::Navigate { url } => {
                                let result = if pages.len() >= MAX_WEBVIEWS {
                                    Err("Close another browser before opening this page".into())
                                } else {
                                    parse_url(&url).and_then(|url| Page::new(&servo, output.clone(), id, url))
                                };
                                match result {
                                    Ok(page) => { pages.insert(id, page); }
                                    Err(error) => output.send(Event::State { id, state: PageState { url, error: Some(error), ..PageState::default() } }, vec![]),
                                }
                            }
                            BrowserCommand::Evaluate { request_id, .. } => output.send(Event::Evaluation { id, request_id, result: Err("Browser page is not available; navigate to a valid URL first".into()) }, vec![]),
                            BrowserCommand::Back
                            | BrowserCommand::Forward
                            | BrowserCommand::Reload
                            | BrowserCommand::Resize { .. }
                            | BrowserCommand::SetContentSizing { .. }
                            | BrowserCommand::Pointer { .. }
                            | BrowserCommand::Move { .. }
                            | BrowserCommand::Scroll { .. }
                            | BrowserCommand::Key { .. }
                            | BrowserCommand::Text { .. }
                            | BrowserCommand::Focus { .. } => {}
                        }
                    }
                }
                Request::Batch {
                    id,
                    request_id,
                    commands,
                    expected_document_revision,
                } => {
                    let document_matches = expected_document_revision.is_none_or(|expected| {
                        pages.get(&id).is_some_and(|page| {
                            let state = page.delegate.state.borrow();
                            !state.loading && state.document_revision == expected
                        })
                    });
                    let result = if !document_matches {
                        Err("Browser document changed before input delivery; observe the page again".into())
                    } else {
                        deliver_commands(commands, |command| {
                            if let Some(page) = pages.get_mut(&id) {
                                return page.command(command);
                            }
                            let BrowserCommand::Navigate { url } = command else {
                                return Err(
                                    "Browser page is not available; navigate to a valid URL first"
                                        .into(),
                                );
                            };
                            if pages.len() >= MAX_WEBVIEWS {
                                return Err("Close another browser before opening this page".into());
                            }
                            let page = Page::new(&servo, output.clone(), id, parse_url(&url)?)?;
                            pages.insert(id, page);
                            Ok(())
                        })
                    };
                    output.send(
                        Event::Evaluation {
                            id,
                            request_id,
                            result,
                        },
                        vec![],
                    );
                }
            }
        }
        servo.spin_event_loop();
        for page in pages.values_mut() {
            page.paint_if_dirty();
            page.measure_content_if_due();
        }
        closing.retain_mut(|page| page.advance(&pages));
        if !shared.memory.lock().queue.is_empty() {
            continue;
        }
        let animating = pages
            .values()
            .any(|page| page.view.animating() || page.delegate.dirty.get())
            || !closing.is_empty();
        thread::park_timeout(if animating {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(250)
        });
    }
    // Cooperative Servo shutdown stays on this worker. Unlike a helper process,
    // an in-process engine cannot be forcibly killed without killing the app.
    closing.clear();
    pages.clear();
    drop(servo);
    drop(profile);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_validate_all_commands_before_delivery_and_preserve_order() {
        let mut delivered = Vec::new();
        let result = deliver_commands(
            vec![
                BrowserCommand::Move { x: 1.0, y: 0.0 },
                BrowserCommand::Move {
                    x: f32::NAN,
                    y: 0.0,
                },
            ],
            |command| {
                delivered.push(command);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(delivered.is_empty());
        let result = deliver_commands(
            vec![
                BrowserCommand::Move { x: 1.0, y: 0.0 },
                BrowserCommand::Move { x: 2.0, y: 0.0 },
                BrowserCommand::Move { x: 3.0, y: 0.0 },
            ],
            |command| {
                delivered.push(command);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result["status"], "delivered");
        assert_eq!(result["delivered"], 3);
        assert_eq!(
            delivered
                .iter()
                .map(|command| match command {
                    BrowserCommand::Move { x, .. } => *x,
                    _ => panic!("unexpected command"),
                })
                .collect::<Vec<_>>(),
            [1.0, 2.0, 3.0]
        );
    }

    #[test]
    fn delivery_failures_report_the_delivered_prefix() {
        let mut count = 0;
        let error = deliver_commands(vec![BrowserCommand::Reload, BrowserCommand::Reload], |_| {
            count += 1;
            if count == 2 {
                Err("surface unavailable".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(error.contains("after 1 of 2"));
        assert!(error.contains("surface unavailable"));
    }

    #[test]
    fn javascript_results_preserve_nested_json_values() {
        let value = JSValue::Object(HashMap::from([
            ("count".into(), JSValue::Number(2.0)),
            (
                "values".into(),
                JSValue::Array(vec![
                    JSValue::Boolean(true),
                    JSValue::Null,
                    JSValue::String("text".into()),
                ]),
            ),
        ]));
        assert_eq!(
            js_value(value),
            serde_json::json!({ "count": 2.0, "values": [true, null, "text"] })
        );
    }

    #[test]
    fn sizing_is_opt_in_single_flight_rate_limited_and_rejects_stale_results() {
        let mut sizing = ContentSizing::new(false);
        let now = Instant::now();
        assert!(sizing.begin(now).is_none());
        sizing.set_enabled(true);
        let now = Instant::now();
        let first = sizing.begin(now).unwrap();
        assert!(sizing.begin(now + CONTENT_MEASUREMENT_INTERVAL).is_none());
        assert!(sizing.finish(first));
        assert!(sizing.begin(now).is_none());
        let next = sizing.begin(now + CONTENT_MEASUREMENT_INTERVAL).unwrap();
        // Both navigation and viewport changes use this invalidation path.
        sizing.invalidate();
        assert!(sizing.begin(Instant::now()).is_none());
        assert!(!sizing.finish(next));
        let stale = sizing.begin(Instant::now()).unwrap();
        sizing.set_enabled(false);
        sizing.set_enabled(true);
        assert!(sizing.begin(Instant::now()).is_none());
        assert!(!sizing.finish(stale));
        let active = sizing.begin(Instant::now()).unwrap();
        sizing.set_enabled(false);
        assert!(!sizing.finish(active));
        assert!(
            sizing
                .begin(Instant::now() + CONTENT_MEASUREMENT_INTERVAL)
                .is_none()
        );
    }
}
