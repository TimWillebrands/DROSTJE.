//! Agent operations run off the GPUI thread. Delivered input is not application completion.
use super::page;
use base64::{Engine, engine::general_purpose::STANDARD};
use drostje_browser::{BrowserCommand, BrowserFrame, BrowserId, BrowserRuntime};
use image::ImageEncoder;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub enum BrowserAutomation {
    Inspect,
    Commands(Vec<BrowserCommand>),
    Evaluate {
        script: String,
    },
    Observe {
        limit: usize,
    },
    Target {
        target: String,
        button: Option<u8>,
    },
    Wait {
        script: Option<String>,
        timeout: Duration,
    },
    Screenshot {
        path: Option<String>,
        after_frame: Option<u64>,
        timeout: Duration,
    },
    Diagnostics {
        after: u64,
    },
}

pub(super) fn pointer_commands(x: f32, y: f32, button: Option<u8>) -> Vec<BrowserCommand> {
    match button {
        None => vec![BrowserCommand::Move { x, y }],
        Some(button) => vec![
            BrowserCommand::Pointer {
                x,
                y,
                button,
                pressed: true,
            },
            BrowserCommand::Pointer {
                x,
                y,
                button,
                pressed: false,
            },
        ],
    }
}

impl BrowserAutomation {
    pub async fn execute(self, runtime: BrowserRuntime, id: BrowserId) -> Result<Value, String> {
        let deadline = Instant::now() + Duration::from_secs(23);
        match self {
            Self::Inspect => inspect(&runtime, id),
            Self::Diagnostics { after } => runtime.diagnostics(id, after).ok_or_else(missing),
            Self::Commands(commands) => {
                let request = runtime.dispatch(id, commands)?;
                response(&runtime, &request, deadline).await
            }
            Self::Evaluate { script } => evaluate(&runtime, id, script, deadline).await,
            Self::Observe { limit } => {
                evaluate(&runtime, id, page::observe_script(limit), deadline).await
            }
            Self::Target { target, button } => {
                let document = runtime.snapshot(id).ok_or_else(missing)?.document_revision;
                let point = evaluate(&runtime, id, page::resolve_script(&target), deadline).await?;
                let coordinate = |field| {
                    point[field]
                        .as_f64()
                        .filter(|v| v.is_finite())
                        .map(|v| v as f32)
                        .ok_or_else(|| "Target did not resolve to browser coordinates".to_owned())
                };
                let request = runtime.dispatch_in_document(
                    id,
                    pointer_commands(coordinate("x")?, coordinate("y")?, button),
                    document,
                )?;
                response(&runtime, &request, deadline).await
            }
            Self::Wait { script, timeout } => {
                let deadline = Instant::now() + timeout;
                let mut capture = Capture::new(&runtime, id);
                loop {
                    if script.is_none() {
                        capture.poll();
                    }
                    let snapshot = runtime.snapshot(id).ok_or_else(missing)?;
                    if let Some(error) = snapshot.error {
                        return Err(error);
                    }
                    let matched = if let Some(script) = &script {
                        evaluate(&runtime, id, format!("({script}) === true"), deadline).await?
                            == Value::Bool(true)
                    } else {
                        !snapshot.loading
                            && snapshot.frame.as_ref().is_some_and(|frame| {
                                frame.document_revision == snapshot.document_revision
                            })
                    };
                    if matched {
                        return Ok(json!({"matched":true,"state":inspect(&runtime,id)?}));
                    }
                    pause(deadline, "Browser condition did not become true before the timeout; no input was replayed").await?;
                }
            }
            Self::Screenshot {
                path,
                after_frame,
                timeout,
            } => {
                let deadline = Instant::now() + timeout;
                let mut capture = Capture::new(&runtime, id);
                let frame = loop {
                    capture.poll();
                    let snapshot = runtime.snapshot(id).ok_or_else(missing)?;
                    if let Some(error) = snapshot.error {
                        return Err(error);
                    }
                    // Only a frame published after this call: a hidden page's retained
                    // frame can be older than its content.
                    if capture.settled
                        && let Some(frame) = snapshot.frame
                        && frame.document_revision == snapshot.document_revision
                        && after_frame.is_none_or(|revision| frame.revision > revision)
                    {
                        break frame;
                    }
                    pause(deadline, "No matching browser frame before the timeout; inspect the page before trying again").await?;
                };
                drop(capture);
                smol::unblock(move || encode_screenshot(id, frame, path)).await
            }
        }
    }

    /// GPUI applies the viewport policy; this waits for the actual engine frame.
    pub async fn viewport_result(
        runtime: BrowserRuntime,
        id: BrowserId,
        dimensions: (u32, u32, f32),
        fixed: bool,
    ) -> Result<Value, String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        // Queue behind the UI's resize before observing the current viewport.
        evaluate(&runtime, id, "null".into(), deadline).await?;
        let mut capture = Capture::new(&runtime, id);
        loop {
            capture.poll();
            let snapshot = runtime.snapshot(id).ok_or_else(missing)?;
            if let Some(error) = snapshot.error {
                return Err(error);
            }
            if let Some(frame) = snapshot.frame {
                let expected = if fixed {
                    dimensions
                } else {
                    (
                        snapshot.viewport_width,
                        snapshot.viewport_height,
                        snapshot.scale,
                    )
                };
                let matches = frame.width == expected.0
                    && frame.height == expected.1
                    && frame.scale == expected.2
                    && frame.document_revision == snapshot.document_revision;
                if matches {
                    let mut state = inspect(&runtime, id)?;
                    state["viewport_mode"] = json!(if fixed { "fixed" } else { "pane" });
                    return Ok(state);
                }
            }
            pause(deadline, "Browser viewport has not rendered the requested dimensions; inspect before trying again").await?;
        }
    }
}

/// Keeps a page publishing frames while an operation waits on its pixels, whether or
/// not a pane shows it: at most one capture is outstanding, and the next one is asked
/// for after it completes. Dropping it abandons the outstanding capture.
struct Capture<'a> {
    runtime: &'a BrowserRuntime,
    id: BrowserId,
    request: Option<String>,
    /// The snapshot now holds a frame of the content as it was when this began.
    settled: bool,
}

impl<'a> Capture<'a> {
    fn new(runtime: &'a BrowserRuntime, id: BrowserId) -> Self {
        Self {
            runtime,
            id,
            request: None,
            settled: false,
        }
    }

    fn poll(&mut self) {
        if let Some(request) = &self.request {
            if self.runtime.evaluation(request).is_some() {
                self.request = None;
                self.settled = true;
            }
            return;
        }
        match self.runtime.capture(self.id) {
            Ok(request) => self.request = Some(request),
            // Not admitted (closed, shut down, failed or saturated): use the retained
            // frame, as without captures. Closing and failures surface in the snapshot.
            Err(_) => self.settled = true,
        }
    }
}

impl Drop for Capture<'_> {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            self.runtime.discard_evaluation(&request);
        }
    }
}

fn missing() -> String {
    "Browser session does not exist".into()
}
fn inspect(runtime: &BrowserRuntime, id: BrowserId) -> Result<Value, String> {
    runtime.inspect(id).ok_or_else(missing)
}

async fn pause(deadline: Instant, message: &str) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err(message.into());
    }
    smol::Timer::after(
        Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
    )
    .await;
    Ok(())
}

async fn response(
    runtime: &BrowserRuntime,
    request: &str,
    deadline: Instant,
) -> Result<Value, String> {
    loop {
        if let Some(result) = runtime.evaluation(request) {
            return result;
        }
        if Instant::now() >= deadline {
            runtime.discard_evaluation(request);
            return Err("Browser operation timed out; outcome is unknown. Inspect the page; do not replay input automatically".into());
        }
        smol::Timer::after(Duration::from_millis(10)).await;
    }
}

async fn evaluate(
    runtime: &BrowserRuntime,
    id: BrowserId,
    script: String,
    deadline: Instant,
) -> Result<Value, String> {
    if Instant::now() >= deadline {
        return Err("Browser condition timed out".into());
    }
    let request = runtime.evaluate(id, script)?;
    response(runtime, &request, deadline).await
}

fn encode_screenshot(
    id: BrowserId,
    frame: Arc<BrowserFrame>,
    path: Option<String>,
) -> Result<Value, String> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &frame.rgba,
            frame.width,
            frame.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| format!("Screenshot could not be encoded: {error}"))?;
    if png.len() > (super::MAX_RESPONSE - 64 * 1024) / 4 * 3 {
        return Err("Screenshot exceeds the tool response limit; use a smaller viewport".into());
    }
    if let Some(path) = &path {
        std::fs::write(path, &png)
            .map_err(|error| format!("Screenshot could not be saved: {error}"))?;
    }
    // The base64 text is moved into the result, and the PNG freed as soon as it is
    // encoded: `json!` would deep-copy a String expression. Fields keep their order.
    let data = STANDARD.encode(&png);
    drop(png);
    let mut image = json!({"mimeType": "image/png"});
    image["data"] = Value::String(data);
    let mut result = json!({"id":id,"path":path,"width":frame.width,"height":frame.height,
        "frame_revision":frame.revision,"document_revision":frame.document_revision,
        "viewport":{"width":frame.width as f32/frame.scale,"height":frame.height as f32/frame.scale,"scale":frame.scale}});
    result["image"] = image;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `encode_screenshot`'s output for `golden_frame` before the copy reductions.
    const GOLDEN: &str = r#"{"id":"00000000-0000-0000-0000-000000001234","path":null,"width":3,"height":2,"frame_revision":7,"document_revision":3,"viewport":{"width":1.5,"height":1.0,"scale":2.0},"image":{"mimeType":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAYAAACddGYaAAAAJUlEQVR4AQEaAOX/AAAlSm+Uud4DKE1ylwC84QYrUHWav+QJLlN7EAnl1WCnAwAAAABJRU5ErkJggg=="}}"#;

    fn golden_frame() -> Arc<BrowserFrame> {
        Arc::new(BrowserFrame {
            revision: 7,
            document_revision: 3,
            scale: 2.0,
            width: 3,
            height: 2,
            rgba: (0..24u8).map(|value| value.wrapping_mul(37)).collect(),
        })
    }

    #[test]
    fn screenshot_results_and_their_wire_bytes_match_the_golden_png() {
        let id = BrowserId::from_u128(0x1234);
        let value = encode_screenshot(id, golden_frame(), None).unwrap();
        let golden: Value = serde_json::from_str(GOLDEN).unwrap();
        assert_eq!(value, golden);
        // Byte-exact where this build keeps insertion order, as the desktop does.
        if serde_json::to_string(&golden).unwrap() == GOLDEN {
            assert_eq!(value.to_string(), GOLDEN);
        }
        // The MCP response as handle_mcp builds it, written as the old writer did:
        // the status line and headers, then `Value::to_string`.
        let mut result = json!({"isError": false});
        result["content"] = Value::Array(crate::tool_content(value, true));
        let mut response = json!({"jsonrpc": "2.0", "id": 1});
        response["result"] = result;
        let body = response.to_string();
        let expected = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
            body.len()
        );
        assert!(body.contains(golden["image"]["data"].as_str().unwrap()));
        let mut written = Vec::new();
        crate::write_response(&mut written, crate::HttpResponse::json(200, response)).unwrap();
        assert_eq!(String::from_utf8(written).unwrap(), expected);
    }
}
