//! Servo is intentionally not linked into GPUI: both engines require different
//! freetype-sys versions. stdout is exclusively the binary protocol; diagnostics
//! go to stderr. All Servo handles stay on this process's main thread.
mod protocol;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    io,
    rc::Rc,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use dpi::PhysicalSize;
use euclid::{Box2D, Point2D, Scale, Size2D};
use parking_lot::{Condvar, Mutex};
use serde_json::Value;
use servo::{
    Code, CompositionEvent, CompositionState, ConsoleLogLevel, EventLoopWaker, ImeEvent,
    InputEvent, JSValue, Key, KeyState, KeyboardEvent, LoadStatus, Location, Modifiers,
    MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, RenderingContext, Servo,
    ServoBuilder, ServoDelegate, ServoError, SoftwareRenderingContext, WebView, WebViewBuilder,
    WebViewDelegate, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
};
use url::Url;
use uuid::Uuid;

use protocol::{BrowserCommand, Event, MAX_PENDING, PageState, Request};

const MAX_WEBVIEWS: usize = 16;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

type Packet = (Event, Vec<u8>);

#[derive(Default)]
struct Outbox {
    states: HashMap<Uuid, Packet>,
    frames: HashMap<Uuid, Packet>,
    replies: VecDeque<Packet>,
    closed: bool,
}

#[derive(Default)]
struct Output {
    pending: Mutex<Outbox>,
    ready: Condvar,
}

impl Output {
    fn send(&self, event: Event, bytes: Vec<u8>) {
        let mut pending = self.pending.lock();
        if pending.closed {
            return;
        }
        match &event {
            Event::State { id, .. } => {
                pending.states.insert(*id, (event, bytes));
            }
            Event::Frame { id, .. } => {
                pending.frames.insert(*id, (event, bytes));
            }
            Event::Evaluation { .. } | Event::Fatal { .. } => {
                if pending.replies.len() >= MAX_PENDING {
                    eprintln!("Servo result queue is full; parent stopped consuming responses");
                    pending.closed = true;
                } else {
                    pending.replies.push_back((event, bytes));
                }
            }
        }
        self.ready.notify_one();
    }

    fn frame_pending(&self, id: Uuid) -> bool {
        self.pending.lock().frames.contains_key(&id)
    }

    fn remove(&self, id: Uuid) {
        let mut pending = self.pending.lock();
        pending.states.remove(&id);
        pending.frames.remove(&id);
    }

    fn write_loop(&self, main_thread: thread::Thread) -> io::Result<()> {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        loop {
            let packet = {
                let mut pending = self.pending.lock();
                loop {
                    if let Some(packet) = pending.replies.pop_front() {
                        break Some(packet);
                    }
                    if let Some(id) = pending.states.keys().next().copied() {
                        break pending.states.remove(&id);
                    }
                    if let Some(id) = pending.frames.keys().next().copied() {
                        break pending.frames.remove(&id);
                    }
                    if pending.closed {
                        break None;
                    }
                    self.ready.wait(&mut pending);
                }
            };
            let Some((event, bytes)) = packet else {
                return Ok(());
            };
            protocol::write_packet(&mut stdout, &event, &bytes)?;
            main_thread.unpark();
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
    output: Arc<Output>,
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
}

impl WebViewDelegate for Delegate {
    fn notify_url_changed(&self, _: WebView, url: Url) {
        self.update(|state| state.url = url.to_string());
    }

    fn notify_page_title_changed(&self, _: WebView, title: Option<String>) {
        self.update(|state| state.title = title.unwrap_or_default());
    }

    fn notify_load_status_changed(&self, _: WebView, status: LoadStatus) {
        self.update(|state| state.loading = status != LoadStatus::Complete);
    }

    fn notify_history_changed(&self, _: WebView, entries: Vec<Url>, current: usize) {
        self.update(|state| {
            state.can_go_back = current > 0;
            state.can_go_forward = current + 1 < entries.len();
        });
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
        eprintln!("Servo console {level:?}: {message}");
    }
}

struct Page {
    view: WebView,
    context: Rc<SoftwareRenderingContext>,
    delegate: Rc<Delegate>,
    revision: u64,
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
        let delegate = Rc::new(Delegate {
            id,
            state: RefCell::new(PageState {
                url: url.to_string(),
                loading: true,
                ..PageState::default()
            }),
            dirty: Cell::new(false),
            output,
        });
        let view = WebViewBuilder::new(servo, context.clone())
            .url(url)
            .delegate(delegate.clone())
            .build();
        view.show();
        delegate.update(|_| {});
        Ok(Self {
            view,
            context,
            delegate,
            revision: 0,
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
                    self.view.go_back(1);
                }
            }
            BrowserCommand::Forward => {
                self.delegate.update(|state| {
                    state.error = None;
                });
                if self.delegate.state.borrow().can_go_forward {
                    self.view.go_forward(1);
                }
            }
            BrowserCommand::Reload => {
                // A local artifact may have been removed since its previous load.
                if let Some(url) = self.view.url() {
                    parse_url(url.as_str())?;
                }
                self.delegate.update(|state| {
                    state.error = None;
                    state.loading = true;
                });
                self.view.reload();
            }
            BrowserCommand::Resize {
                width,
                height,
                scale,
            } => {
                protocol::frame_len(width, height).map_err(|error| error.to_string())?;
                if !scale.is_finite() || !(0.25..=8.0).contains(&scale) {
                    return Err("Browser scale must be between 0.25 and 8".into());
                }
                let size = PhysicalSize::new(width, height);
                if self.context.size() != size || self.scale != scale {
                    self.context
                        .make_current()
                        .map_err(|error| format!("Could not resize Servo surface: {error:?}"))?;
                    self.scale = scale;
                    self.view.set_hidpi_scale_factor(Scale::new(scale));
                    self.view.resize(size);
                    self.delegate.dirty.set(true);
                }
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
                        delegate.error(error.clone());
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

    fn paint_if_dirty(&mut self) {
        if !self.delegate.dirty.get()
            || self.last_frame.elapsed() < FRAME_INTERVAL
            || self.delegate.output.frame_pending(self.delegate.id)
        {
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
        self.revision += 1;
        self.delegate.output.send(
            Event::Frame {
                id: self.delegate.id,
                revision: self.revision,
                width: image.width(),
                height: image.height(),
            },
            image.into_raw(),
        );
    }
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

fn run() -> Result<(), String> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| "Could not initialize Servo's TLS cryptography provider".to_owned())?;
    let output = Arc::new(Output::default());
    let output_writer = output.clone();
    let main_thread = thread::current();
    let writer_thread = main_thread.clone();
    let writer = thread::Builder::new()
        .name("servo-protocol-output".into())
        .spawn(move || {
            if let Err(error) = output_writer.write_loop(writer_thread.clone()) {
                eprintln!("Could not write Servo output: {error}");
                output_writer.pending.lock().closed = true;
                writer_thread.unpark();
            }
        })
        .map_err(|error| format!("Could not start Servo output: {error}"))?;
    let (sender, requests) = mpsc::sync_channel(MAX_PENDING);
    let input_thread = main_thread.clone();
    thread::Builder::new()
        .name("servo-protocol-input".into())
        .spawn(move || {
            let stdin = io::stdin();
            let mut stdin = stdin.lock();
            loop {
                match protocol::read_packet::<Request>(&mut stdin) {
                    Ok(Some((request, bytes))) if bytes.is_empty() => {
                        // A broken/closed parent pipe terminates the engine, including
                        // when no explicit Shutdown command could be sent.
                        if sender.send(request).is_err() {
                            break;
                        }
                        input_thread.unpark();
                    }
                    Ok(None) => break,
                    Ok(Some((request, _))) => {
                        eprintln!(
                            "Unexpected binary payload in browser command for {:?}",
                            request.id()
                        );
                        break;
                    }
                    Err(error) => {
                        eprintln!("Could not read browser command: {error}");
                        break;
                    }
                }
            }
            input_thread.unpark();
        })
        .map_err(|error| format!("Could not start Servo input: {error}"))?;
    let servo = ServoBuilder::default()
        .event_loop_waker(Box::new(Waker(main_thread)))
        .build();
    servo.setup_logging();
    servo.set_delegate(Rc::new(EngineDelegate(output.clone())));
    let mut pages = HashMap::<Uuid, Page>::new();
    let mut shutdown = false;
    let mut batch = VecDeque::<Request>::with_capacity(64);
    while !shutdown && !output.pending.lock().closed {
        for _ in 0..64 {
            let request = match requests.try_recv() {
                Ok(request) => request,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    shutdown = true;
                    break;
                }
            };
            if batch
                .back()
                .is_some_and(|previous| request.supersedes(previous))
            {
                batch.pop_back();
            }
            batch.push_back(request);
        }
        while let Some(request) = batch.pop_front() {
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
                            _ => {}
                        }
                    }
                }
                Request::Close { id } => {
                    pages.remove(&id);
                    output.remove(id);
                }
                Request::Shutdown => {
                    shutdown = true;
                    break;
                }
            }
        }
        if shutdown {
            break;
        }
        servo.spin_event_loop();
        for page in pages.values_mut() {
            page.paint_if_dirty();
        }
        let animating = pages
            .values()
            .any(|page| page.view.animating() || page.delegate.dirty.get());
        if animating {
            thread::park_timeout(Duration::from_millis(16));
        } else {
            thread::park_timeout(Duration::from_millis(250));
        }
    }
    // Dropping the final Servo handle drains its own shutdown protocol. The parent
    // supervisor bounds this by two seconds and kills/reaps a wedged helper.
    pages.clear();
    drop(servo);
    output.pending.lock().closed = true;
    output.ready.notify_one();
    writer
        .join()
        .map_err(|_| "Servo output worker panicked".to_owned())?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("drostje-servo: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_local_artifact_has_an_actionable_error() {
        let path = std::env::temp_dir().join(format!("drostje-missing-{}.html", Uuid::new_v4()));
        let url = Url::from_file_path(&path).unwrap();
        let error = parse_url(url.as_str()).unwrap_err();
        assert!(error.contains("Restore the file"));
        assert!(error.contains(path.file_name().unwrap().to_str().unwrap()));
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
    fn output_storage_keeps_only_latest_frame_and_status_per_browser() {
        let output = Output::default();
        let id = Uuid::new_v4();
        for revision in 1..100 {
            output.send(
                Event::Frame {
                    id,
                    revision,
                    width: 1,
                    height: 1,
                },
                vec![revision as u8; 4],
            );
            output.send(
                Event::State {
                    id,
                    state: PageState {
                        title: revision.to_string(),
                        ..PageState::default()
                    },
                },
                vec![],
            );
        }
        let pending = output.pending.lock();
        assert_eq!(pending.frames.len(), 1);
        assert_eq!(pending.states.len(), 1);
        assert_eq!(pending.frames[&id].1, [99; 4]);
    }
}
