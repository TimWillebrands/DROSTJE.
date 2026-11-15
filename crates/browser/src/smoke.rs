//! Real engine regression migrated from the former helper-process smoke.
//! cargo test -p drostje-browser smoke -- --ignored --nocapture
use super::*;
use std::time::Duration;

struct Browser {
    runtime: BrowserRuntime,
    directory: std::path::PathBuf,
}

impl Browser {
    fn start() -> Self {
        let directory =
            std::env::temp_dir().join(format!("drostje-servo-smoke-{}", BrowserId::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("style.css"), "html,body { margin:0; background:rgb(12,34,56); } body { min-height:2400px; } button { position:absolute; left:20px; top:20px; width:120px; height:60px; } input { position:absolute; left:20px; top:120px; }").unwrap();
        std::fs::write(directory.join("index.html"), "<!doctype html><title>Servo smoke</title><link rel=stylesheet href=style.css><button onclick='window.clicked=(window.clicked||0)+1'>Click</button><input id=text><script>window.clicked=0</script>").unwrap();
        Self {
            runtime: BrowserRuntime::new(),
            directory,
        }
    }

    fn until(&self, label: &str, predicate: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(45);
        while !predicate() {
            assert!(Instant::now() < deadline, "Timed out: {label}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn snapshot(&self, id: BrowserId) -> BrowserSnapshot {
        let snapshot = self.runtime.snapshot(id).expect("Browser was closed");
        assert!(
            snapshot.error.is_none(),
            "Servo failed: {:?}",
            snapshot.error
        );
        snapshot
    }

    fn evaluate(&self, id: BrowserId, script: &str) -> Value {
        let request_id = BrowserId::new_v4().to_string();
        self.runtime.command(
            id,
            BrowserCommand::Evaluate {
                request_id: request_id.clone(),
                script: script.into(),
            },
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = self.runtime.evaluation(&request_id) {
                return result.expect("JavaScript evaluation failed");
            }
            assert!(Instant::now() < deadline, "Evaluation timed out: {script}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn until_height(&self, id: BrowserId, expected: f32) {
        self.until(&format!("natural content height {expected}px"), || {
            let snapshot = self.snapshot(id);
            !snapshot.loading && snapshot.content_height == Some(expected)
        });
    }

    fn click(&self, id: BrowserId, x: f32, y: f32) {
        let commands = [true, false]
            .into_iter()
            .map(|pressed| BrowserCommand::Pointer {
                x,
                y,
                button: 0,
                pressed,
            })
            .collect();
        let request_id = self.runtime.dispatch(id, commands).unwrap();
        self.until("input delivery acknowledgement", || {
            self.runtime.evaluation(&request_id).is_some_and(|result| {
                let result = result.expect("Input batch was not delivered");
                assert_eq!(result["status"], "delivered");
                assert_eq!(result["delivered"], 2);
                true
            })
        });
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.runtime.shutdown();
        // Let Servo tear down its GL surfaces before the test process exits.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.runtime.0.0.memory.lock().stopped && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn real_content_height_grows_shrinks_reflows_and_resumes(browser: &Browser) {
    let path = browser.directory.join("sizing.html");
    std::fs::write(
        &path,
        r#"<!doctype html><title>Content sizing</title>
        <style>
        html, body { margin:0; padding:0 }
        main { width:100% }
        #block { height:140px; background:#264 }
        #wrap { display:flex; flex-wrap:wrap }
        #wrap > div { flex:0 0 180px; height:60px; background:#426 }
        </style>
        <main data-drostje-artifact>
          <div id=block></div>
          <div id=wrap><div></div><div></div><div></div><div></div></div>
        </main>"#,
    )
    .unwrap();
    let url = url::Url::from_file_path(&path).unwrap().to_string();
    let id = BrowserId::new_v4();
    browser.runtime.open(id, url.clone());
    browser.until("ordinary browser does not opt in to sizing", || {
        let snapshot = browser.snapshot(id);
        !snapshot.loading && snapshot.title == "Content sizing" && snapshot.frame.is_some()
    });
    assert_eq!(browser.snapshot(id).content_height, None);
    browser
        .runtime
        .command(id, BrowserCommand::SetContentSizing { enabled: true });
    browser.until_height(id, 200.0);
    browser.evaluate(
        id,
        "document.getElementById('block').style.height = '920px'",
    );
    browser.until_height(id, 980.0);
    browser.runtime.command(
        id,
        BrowserCommand::Resize {
            width: 800,
            height: 1200,
            scale: 1.0,
        },
    );
    browser.until("grown viewport before content shrink", || {
        browser
            .snapshot(id)
            .frame
            .is_some_and(|frame| frame.width == 800 && frame.height == 1200)
    });
    browser.evaluate(
        id,
        "document.getElementById('block').style.height = '140px'",
    );
    browser.until_height(id, 200.0);
    // The document scrolling extent remains viewport-high, unlike natural content.
    assert!(
        browser
            .evaluate(id, "document.documentElement.scrollHeight")
            .as_f64()
            .is_some_and(|height| height >= 1200.0)
    );
    browser.runtime.command(
        id,
        BrowserCommand::Resize {
            width: 360,
            height: 1200,
            scale: 1.0,
        },
    );
    browser.until_height(id, 260.0);
    browser.until("current frame after narrow resize", || {
        browser
            .snapshot(id)
            .frame
            .is_some_and(|frame| frame.width == 360 && frame.height == 1200)
    });
    browser
        .runtime
        .command(id, BrowserCommand::SetContentSizing { enabled: false });
    let revision = browser.snapshot(id).frame.unwrap().revision;
    browser.evaluate(id, "document.getElementById('block').style.height = '80px'");
    browser.until("disabled sizing still renders real content changes", || {
        browser
            .snapshot(id)
            .frame
            .is_some_and(|frame| frame.revision > revision)
    });
    assert_eq!(browser.snapshot(id).content_height, Some(260.0));
    browser
        .runtime
        .command(id, BrowserCommand::SetContentSizing { enabled: true });
    browser.until_height(id, 200.0);
    browser.evaluate(
        id,
        "document.querySelector('main').removeAttribute('data-drostje-artifact'); \
         document.body.style.minHeight = '100vh'; \
         document.getElementById('block').style.height = '30px'",
    );
    browser.until_height(id, 150.0);
    browser.runtime.command(
        id,
        BrowserCommand::Resize {
            width: 800,
            height: 1200,
            scale: 1.0,
        },
    );
    browser.until_height(id, 90.0);
    // Exercise a page-initiated navigation while polling measurements, then an
    // explicit navigation/resize barrier. Old document dimensions cannot win.
    let replacement = browser.directory.join("sizing-replacement.html");
    std::fs::write(
        &replacement,
        "<!doctype html><title>Replacement sizing</title><style>html,body{margin:0}</style><main data-drostje-artifact style='height:73px'></main>",
    )
    .unwrap();
    let replacement_url = url::Url::from_file_path(&replacement).unwrap().to_string();
    browser.evaluate(
        id,
        &format!(
            "document.getElementById('block').style.height = '1900px'; \
             location.href = {}; true",
            serde_json::to_string(&replacement_url).unwrap()
        ),
    );
    browser.until(
        "page-initiated navigation has only replacement dimensions",
        || {
            let snapshot = browser.snapshot(id);
            if snapshot.title != "Replacement sizing" || snapshot.loading {
                return false;
            }
            assert!(snapshot.content_height.is_none() || snapshot.content_height == Some(73.0));
            snapshot.content_height == Some(73.0)
        },
    );
    browser
        .runtime
        .command(id, BrowserCommand::Navigate { url });
    browser.runtime.command(
        id,
        BrowserCommand::Resize {
            width: 360,
            height: 1200,
            scale: 1.0,
        },
    );
    browser.until_height(id, 260.0);
    assert!(
        browser.runtime.0.0.memory.lock().evaluations.is_empty(),
        "Internal content measurements must not occupy tool evaluation requests"
    );
    browser.runtime.close(id);
}

/// Real loopback HTTP: WebGPU and other origin-keyed APIs are unavailable to
/// the opaque origins of `file:` URLs.
struct HttpFixture {
    url: String,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl HttpFixture {
    fn start(index: &'static str) -> Self {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("HTTP fixture accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut buffer = [0; 8192];
                let length = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..length]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                if path == "/broken" {
                    continue;
                } // Real dropped HTTP connection.
                let (status, body) = if path == "/" {
                    ("200 OK", index)
                } else {
                    ("404 Not Found", "missing")
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            url,
            stopped,
            worker: Some(worker),
        }
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn real_diagnostics_and_document_metadata(browser: &Browser) {
    let server = HttpFixture::start(
        r#"<!doctype html><title>Diagnostics smoke</title>
        <script>
        console.warn('native-console-smoke');
        fetch('/missing').then(r => { window.fetchStatus = r.status; });
        fetch('/broken').catch(() => { window.fetchFailed = true; });
        const xhr = new XMLHttpRequest(); xhr.open('GET', '/missing');
        xhr.onload = () => { window.xhrStatus = xhr.status; }; xhr.send();
        </script><img src='/missing'>"#,
    );
    let id = BrowserId::new_v4();
    browser.runtime.open(id, server.url.clone());
    browser.until("startup network diagnostics", || {
        let diagnostics = browser.runtime.diagnostics(id, 0).unwrap();
        let events = diagnostics["events"].as_array().unwrap();
        let contains = |kind: &str, text: &str| {
            events.iter().any(|event| {
                event["kind"] == kind
                    && event["message"]
                        .as_str()
                        .is_some_and(|message| message.contains(text))
            })
        };
        contains("console", "native-console-smoke")
            && contains("network", "\"api\":\"fetch\"")
            && contains("network", "\"api\":\"xhr\"")
            && contains("network", "request_failed")
            && contains("network", "resource_error")
    });
    browser.until("diagnostic document frame", || {
        let snapshot = browser.snapshot(id);
        !snapshot.loading
            && snapshot.frame.as_ref().is_some_and(|frame| {
                frame.document_revision == snapshot.document_revision
                    && frame.scale == snapshot.scale
            })
    });
    assert_eq!(browser.evaluate(id, "window.fetchStatus"), 404.0);
    assert_eq!(browser.evaluate(id, "window.xhrStatus"), 404.0);
    assert_eq!(browser.evaluate(id, "window.fetchFailed"), true);
    let old_document = browser.snapshot(id).document_revision;
    let request_id = browser
        .runtime
        .dispatch(id, vec![BrowserCommand::Reload])
        .unwrap();
    browser.until("reload delivered", || {
        browser
            .runtime
            .evaluation(&request_id)
            .is_some_and(|result| {
                result.unwrap();
                true
            })
    });
    browser.until("reload frame has a new document identity", || {
        let snapshot = browser.snapshot(id);
        snapshot.document_revision > old_document
            && !snapshot.loading
            && snapshot
                .frame
                .as_ref()
                .is_some_and(|frame| frame.document_revision == snapshot.document_revision)
    });
    let stale = browser
        .runtime
        .dispatch_in_document(
            id,
            vec![BrowserCommand::Move { x: 1.0, y: 1.0 }],
            old_document,
        )
        .unwrap();
    browser.until("stale document batch rejected", || {
        browser.runtime.evaluation(&stale).is_some_and(|result| {
            assert!(result.unwrap_err().contains("document changed"));
            true
        })
    });
    browser.runtime.close(id);
}

#[test]
#[ignore = "requires a working native Servo renderer and desktop graphics session"]
fn real_html_pixels_javascript_click_text_resize_history_errors_and_shutdown() {
    let browser = Browser::start();
    let id = BrowserId::new_v4();
    let url = url::Url::from_file_path(browser.directory.join("index.html"))
        .unwrap()
        .to_string();
    browser.runtime.open(id, url.clone());
    browser.until("HTML, linked CSS and RGBA frame", || {
        let snapshot = browser.snapshot(id);
        !snapshot.loading
            && snapshot.title == "Servo smoke"
            && snapshot.frame.is_some_and(|frame| {
                let offset =
                    ((frame.height as usize - 1) * frame.width as usize + frame.width as usize - 1)
                        * 4;
                frame.rgba[offset..offset + 4] == [12, 34, 56, 255]
            })
    });
    assert_ne!(
        browser.runtime.0.0.worker.lock().as_ref().unwrap().id(),
        thread::current().id()
    );
    assert_eq!(
        browser.evaluate(id, "({title: document.title, nested: [true, null, 2]})"),
        serde_json::json!({"title":"Servo smoke", "nested":[true, null, 2.0]})
    );
    browser.click(id, 50.0, 40.0);
    browser.until("pointer click", || {
        browser.evaluate(id, "window.clicked").as_f64() == Some(1.0)
    });
    browser.click(id, 50.0, 130.0);
    browser.until("input focus", || {
        browser.evaluate(id, "document.activeElement.id") == "text"
    });
    browser.runtime.command(
        id,
        BrowserCommand::Text {
            text: "Servo ✓".into(),
        },
    );
    browser.until("committed Unicode text", || {
        browser.evaluate(id, "document.querySelector('input').value") == "Servo ✓"
    });
    for pressed in [true, false] {
        browser.runtime.command(
            id,
            BrowserCommand::Key {
                key: "Backspace".into(),
                code: "Backspace".into(),
                pressed,
                shift: false,
                control: false,
                alt: false,
                meta: false,
            },
        );
    }
    browser.until("DOM keyboard input", || {
        browser.evaluate(id, "document.querySelector('input').value") == "Servo "
    });
    browser.runtime.command(
        id,
        BrowserCommand::Scroll {
            x: 300.0,
            y: 300.0,
            dx: 0.0,
            dy: 120.0,
        },
    );
    browser.until("wheel scroll", || {
        browser
            .evaluate(id, "window.scrollY")
            .as_f64()
            .is_some_and(|y| y > 0.0)
    });
    browser.runtime.command(
        id,
        BrowserCommand::Resize {
            width: 640,
            height: 480,
            scale: 1.0,
        },
    );
    browser.until("resized RGBA frame", || {
        browser.snapshot(id).frame.is_some_and(|frame| {
            frame.width == 640 && frame.height == 480 && frame.rgba.len() == 640 * 480 * 4
        })
    });
    if let Some(output) = std::env::var_os("DROSTJE_QA_SCREENSHOTS") {
        std::fs::create_dir_all(&output).unwrap();
        let frame = browser.snapshot(id).frame.unwrap();
        image::save_buffer(
            std::path::Path::new(&output).join("servo-engine.png"),
            &frame.rgba,
            frame.width,
            frame.height,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
    real_hidden_pages_publish_frames_only_when_captured(&browser, id);
    real_content_height_grows_shrinks_reflows_and_resumes(&browser);
    real_diagnostics_and_document_metadata(&browser);
    real_webgpu_canvas_presents_its_render_pass(&browser);
    real_webgpu_canvas_survives_resizing_every_frame(&browser);
    real_webgpu_releases_devices_of_documents_that_navigate_away(&browser);
    real_indexeddb_is_deleted_when_the_last_surface_on_its_origin_closes(&browser);
    real_artifact_kit_styles_pages_and_round_trips_channel_events(&browser);
    real_artifact_gallery_renders_in_both_theme_modes(&browser);
    browser.runtime.command(
        id,
        BrowserCommand::Navigate {
            url: "about:blank".into(),
        },
    );
    browser.until("forward navigation", || {
        let s = browser.snapshot(id);
        s.url == "about:blank" && !s.loading && s.can_go_back
    });
    browser.runtime.command(id, BrowserCommand::Back);
    browser.until("back history", || {
        let s = browser.snapshot(id);
        s.url == url && !s.loading && s.can_go_forward
    });
    browser.runtime.command(id, BrowserCommand::Forward);
    browser.until("forward history", || {
        let s = browser.snapshot(id);
        s.url == "about:blank" && !s.loading && s.can_go_back
    });
    browser
        .runtime
        .command(id, BrowserCommand::Navigate { url: url.clone() });
    browser.until("navigate back to artifact", || {
        let s = browser.snapshot(id);
        s.title == "Servo smoke" && !s.loading
    });
    browser.evaluate(id, "window.clicked = 9");
    browser.runtime.command(id, BrowserCommand::Reload);
    browser.until("reload clears JavaScript state", || {
        !browser.snapshot(id).loading
            && browser.evaluate(id, "window.clicked").as_f64() == Some(0.0)
    });

    let other = BrowserId::new_v4();
    browser.runtime.open(other, url.clone());
    browser.until("independent second WebView", || {
        let s = browser.snapshot(other);
        !s.loading && s.title == "Servo smoke"
    });
    browser.evaluate(id, "window.clicked = 7");
    assert_eq!(
        browser.evaluate(other, "window.clicked").as_f64(),
        Some(0.0)
    );
    browser.runtime.command(
        id,
        BrowserCommand::Navigate {
            url: url::Url::from_file_path(browser.directory.join("missing.html"))
                .unwrap()
                .to_string(),
        },
    );
    browser.until("missing artifact error", || {
        let snapshot = browser.runtime.snapshot(id).unwrap();
        snapshot.error.is_some() && !snapshot.loading
    });
    browser
        .runtime
        .command(id, BrowserCommand::Navigate { url: url.clone() });
    browser.until("error recovery", || {
        let s = browser.runtime.snapshot(id).unwrap();
        s.error.is_none() && !s.loading && s.url == url
    });
    browser.runtime.command(
        id,
        BrowserCommand::Evaluate {
            request_id: "throws".into(),
            script: "throw new Error('smoke')".into(),
        },
    );
    browser.until("JavaScript exception result", || {
        browser
            .runtime
            .evaluation("throws")
            .is_some_and(|result| result.is_err())
    });
    assert!(
        browser.snapshot(id).error.is_none(),
        "Evaluation errors must not poison page health"
    );
    browser.runtime.close(id);
    assert!(browser.runtime.snapshot(id).is_none());
    assert_eq!(browser.evaluate(other, "document.title"), "Servo smoke");
    browser.runtime.open(id, "about:blank".into());
    browser.until("reopened identity has fresh state", || {
        let s = browser.snapshot(id);
        s.url == "about:blank" && !s.loading && s.title.is_empty() && !s.can_go_back
    });
    browser.runtime.close(id);
    browser.runtime.close(other);
    browser.runtime.shutdown();
    browser.until("Servo shutdown and worker exit", || {
        browser.runtime.0.0.memory.lock().stopped
    });
    println!(
        "In-process Servo proof: dedicated worker, HTML/CSS pixels, nested JS, click, Unicode text, key, wheel, resize, natural content-height opt-in/grow/shrink/width reflow/disable/resume/body fallback/navigation, WebGPU canvas presentation, per-frame resizing and navigation, IndexedDB deletion on last close, reload, back/forward, isolated WebViews, missing-file recovery, JS exception, close/reopen, cooperative shutdown"
    );
}

/// The bottom-right pixel of `id`'s current frame, with its revision.
fn corner(browser: &Browser, id: BrowserId) -> Option<(u64, [u8; 4])> {
    let frame = browser.snapshot(id).frame?;
    let offset = (frame.width as usize * frame.height as usize - 1) * 4;
    Some((
        frame.revision,
        frame.rgba[offset..offset + 4].try_into().unwrap(),
    ))
}

fn real_hidden_pages_publish_frames_only_when_captured(browser: &Browser, id: BrowserId) {
    const RED: [u8; 4] = [200, 10, 10, 255];
    const ORIGINAL: [u8; 4] = [12, 34, 56, 255];
    browser.runtime.set_visible(id, false);
    // Let a publish that began before the engine saw the change land first.
    thread::sleep(Duration::from_millis(300));
    let (hidden_revision, pixel) = corner(browser, id).unwrap();
    assert_eq!(pixel, ORIGINAL);
    browser.evaluate(id, "document.body.style.background = 'rgb(200,10,10)'");
    thread::sleep(Duration::from_millis(800));
    assert_eq!(
        corner(browser, id),
        Some((hidden_revision, ORIGINAL)),
        "a hidden page publishes no frames"
    );
    let request_id = browser.runtime.capture(id).unwrap();
    browser.until("capture of a hidden page", || {
        browser.runtime.evaluation(&request_id).is_some()
    });
    let (captured_revision, pixel) = corner(browser, id).unwrap();
    assert!(captured_revision > hidden_revision);
    assert_eq!(pixel, RED, "the capture shows the current content");
    browser.runtime.set_visible(id, true);
    browser.evaluate(id, "document.body.style.background = ''");
    browser.until("a shown page publishes again", || {
        corner(browser, id).is_some_and(|(_, pixel)| pixel == ORIGINAL)
    });
}

fn real_webgpu_canvas_presents_its_render_pass(browser: &Browser) {
    let server = HttpFixture::start(
        r#"<!doctype html><title>WebGPU smoke</title>
        <style>html,body{margin:0;background:#fff}canvas{display:block;width:64px;height:64px}</style>
        <canvas width=64 height=64></canvas><script>
        window.gpuResult = 'pending';
        (async () => {
          if (!navigator.gpu) throw new Error('navigator.gpu is missing');
          const adapter = await navigator.gpu.requestAdapter();
          if (!adapter) throw new Error('No WebGPU adapter');
          const limits = adapter.limits;
          window.gpuLimits = [limits.maxSampledTexturesPerShaderStage, limits.maxSamplersPerShaderStage,
            limits.maxBindingsPerBindGroup, limits.maxBindGroups];
          // As PixiJS does: request the adapter's limits, then size shaders from them.
          const device = await adapter.requestDevice({requiredLimits: {
            maxSampledTexturesPerShaderStage: limits.maxSampledTexturesPerShaderStage,
            maxSamplersPerShaderStage: limits.maxSamplersPerShaderStage}});
          const context = document.querySelector('canvas').getContext('webgpu');
          context.configure({device, format: navigator.gpu.getPreferredCanvasFormat(), alphaMode: 'opaque'});
          const encoder = device.createCommandEncoder();
          encoder.beginRenderPass({colorAttachments: [{view: context.getCurrentTexture().createView(),
            clearValue: {r: 0.2, g: 0.4, b: 0.6, a: 1}, loadOp: 'clear', storeOp: 'store'}]}).end();
          device.queue.submit([encoder.finish()]);
          await device.queue.onSubmittedWorkDone();
          window.gpuResult = 'presented';
        })().catch(error => { window.gpuResult = String(error); });
        </script>"#,
    );
    let id = BrowserId::new_v4();
    browser.runtime.open(id, server.url.clone());
    browser.until("WebGPU submission", || {
        !browser.snapshot(id).loading && browser.evaluate(id, "window.gpuResult") != "pending"
    });
    assert_eq!(browser.evaluate(id, "window.gpuResult"), "presented");
    // Raw Vulkan maxima (524288 samplers) made PixiJS generate a 134 MB shader.
    let limits = browser.evaluate(id, "window.gpuLimits");
    let limits: Vec<u64> = limits
        .as_array()
        .unwrap()
        .iter()
        .map(|limit| limit.as_f64().unwrap() as u64)
        .collect();
    assert!(
        limits[0] <= 16 && limits[1] <= 16 && limits[2] <= 1000 && limits[3] <= 4,
        "adapter must expose WebGPU's default binding-count limits, not driver maxima: {limits:?}"
    );
    // sRGB-agnostic: the canvas centre is the clear colour, not the white page.
    browser.until("WebGPU canvas pixels in the Servo frame", || {
        browser.snapshot(id).frame.is_some_and(|frame| {
            let offset = (32 * frame.width as usize + 32) * 4;
            let [r, g, b, a] = frame.rgba[offset..offset + 4] else {
                return false;
            };
            a == 255 && r < g && g < b && b < 220
        })
    });
    browser.runtime.close(id);
}

fn real_webgpu_canvas_survives_resizing_every_frame(browser: &Browser) {
    // A canvas that shrinks while its previous, larger frame is still the
    // WebRender descriptor used to panic the worker during texture upload.
    let server = HttpFixture::start(
        r#"<!doctype html><title>WebGPU resize smoke</title>
        <style>html,body{margin:0;background:#fff}canvas{display:block}</style>
        <canvas></canvas><script>
        window.gpuFrames = 0;
        (async () => {
          const adapter = await navigator.gpu.requestAdapter();
          const device = await adapter.requestDevice();
          const canvas = document.querySelector('canvas');
          const context = canvas.getContext('webgpu');
          const format = navigator.gpu.getPreferredCanvasFormat();
          const sizes = [[640, 480], [64, 48], [500, 300], [32, 640], [320, 24]];
          const draw = () => {
            const [width, height] = sizes[window.gpuFrames % sizes.length];
            canvas.width = width;
            canvas.height = height;
            context.configure({device, format, alphaMode: 'opaque'});
            const encoder = device.createCommandEncoder();
            encoder.beginRenderPass({colorAttachments: [{view: context.getCurrentTexture().createView(),
              clearValue: {r: 0.2, g: 0.4, b: 0.6, a: 1}, loadOp: 'clear', storeOp: 'store'}]}).end();
            device.queue.submit([encoder.finish()]);
            window.gpuFrames += 1;
            requestAnimationFrame(draw);
          };
          requestAnimationFrame(draw);
        })().catch(error => { window.gpuFrames = String(error); });
        </script>"#,
    );
    let id = BrowserId::new_v4();
    browser.runtime.open(id, server.url.clone());
    // `snapshot` fails the test as soon as the worker reports a panic.
    browser.until("200 resized WebGPU frames", || {
        if browser.snapshot(id).loading {
            return false;
        }
        let frames = browser.evaluate(id, "window.gpuFrames");
        assert!(frames.is_number(), "WebGPU resize page failed: {frames}");
        browser.snapshot(id);
        frames.as_f64().is_some_and(|frames| frames >= 200.0)
    });
    browser.runtime.close(id);
}

fn real_webgpu_releases_devices_of_documents_that_navigate_away(browser: &Browser) {
    // A navigated-away document's devices used to live until script collected
    // them, which never happened. A process gets about ten devices; the next
    // request failed and panicked the site's shared WebGPU thread, after which
    // every document on the site waited forever for WebGPU.
    let server = HttpFixture::start(
        r#"<!doctype html><title>WebGPU navigation smoke</title><canvas width=64 height=64></canvas><script>
        window.gpuFrames = 0;
        (async () => {
          const adapter = await navigator.gpu.requestAdapter();
          const device = await adapter.requestDevice();
          // Like PixiJS's support check plus renderer: two devices per document.
          window.spare = await (await navigator.gpu.requestAdapter()).requestDevice();
          const context = document.querySelector('canvas').getContext('webgpu');
          context.configure({device, format: navigator.gpu.getPreferredCanvasFormat(), alphaMode: 'opaque'});
          const draw = () => {
            device.pushErrorScope('validation');
            const encoder = device.createCommandEncoder();
            encoder.beginRenderPass({colorAttachments: [{view: context.getCurrentTexture().createView(),
              clearValue: {r: 0.2, g: 0.4, b: 0.6, a: 1}, loadOp: 'clear', storeOp: 'store'}]}).end();
            device.queue.submit([encoder.finish()]);
            device.popErrorScope();
            window.gpuFrames += 1;
            requestAnimationFrame(draw);
          };
          requestAnimationFrame(draw);
        })().catch(error => { window.gpuFrames = String(error); });
        </script>"#,
    );
    let id = BrowserId::new_v4();
    let rendering = |label: &str| {
        browser.until(label, || {
            if browser.snapshot(id).loading {
                return false;
            }
            let frames = browser.evaluate(id, "window.gpuFrames");
            assert!(
                frames.is_number() || frames.is_null(),
                "WebGPU page failed: {frames}"
            );
            frames.as_f64().is_some_and(|frames| frames >= 10.0)
        });
    };
    browser.runtime.open(id, server.url.clone());
    for _ in 0..12 {
        rendering("WebGPU frames before leaving");
        browser.runtime.command(
            id,
            BrowserCommand::Navigate {
                url: format!("{}missing", server.url),
            },
        );
        browser.until("same-site document without WebGPU", || {
            let snapshot = browser.snapshot(id);
            !snapshot.loading && snapshot.url.ends_with("/missing")
        });
        browser.runtime.command(
            id,
            BrowserCommand::Navigate {
                url: server.url.clone(),
            },
        );
    }
    rendering("WebGPU frames after repeated navigation");
    browser.runtime.close(id);
}

fn real_indexeddb_is_deleted_when_the_last_surface_on_its_origin_closes(browser: &Browser) {
    // Keeps its connection open, as real sites do, so deletion cannot rely on
    // the page closing it.
    let server = HttpFixture::start(
        r#"<!doctype html><title>IndexedDB smoke</title><script>
        indexedDB.databases().then(databases => {
          window.before = databases.map(({ name }) => name).join(',');
          const request = indexedDB.open('drostje-smoke', 1);
          request.onupgradeneeded = () => request.result.createObjectStore('notes');
          request.onsuccess = () => {
            const transaction = request.result.transaction('notes', 'readwrite');
            transaction.objectStore('notes').put('kept', 'note');
            transaction.oncomplete = () => { window.stored = true; };
          };
        });
        </script>"#,
    );
    let open = |label: &str| {
        let id = BrowserId::new_v4();
        browser.runtime.open(id, server.url.clone());
        browser.until(label, || {
            !browser.snapshot(id).loading && browser.evaluate(id, "window.stored === true") == true
        });
        (id, browser.evaluate(id, "window.before"))
    };
    // Servo 0.7 falls back to `./bottles` when it has no usable config directory.
    let working_directory_bottles =
        || std::fs::read_dir("bottles").map_or(0, |entries| entries.flatten().count());
    let bottles_before = working_directory_bottles();
    let (first, before) = open("first surface stores a record");
    assert_eq!(
        working_directory_bottles(),
        bottles_before,
        "IndexedDB must live in the engine profile, not the working directory"
    );
    assert_eq!(before, "");
    let (second, before) = open("second surface on the same origin");
    assert_eq!(before, "drostje-smoke");
    browser.runtime.close(first);
    // Longer than the close deadline: a closing surface never outlives it.
    thread::sleep(Duration::from_secs(6));
    let (third, before) = open("surface after a sibling closed");
    assert_eq!(
        before, "drostje-smoke",
        "a live surface keeps its origin's data"
    );
    browser.runtime.close(second);
    browser.runtime.close(third);
    thread::sleep(Duration::from_secs(6));
    let (fourth, before) = open("surface after every surface closed");
    assert_eq!(before, "", "closing the last surface deletes its IndexedDB");
    browser.runtime.close(fourth);
}

#[test]
#[ignore = "requires a working native Servo renderer and desktop graphics session"]
fn real_has_selector_lists_apply() {
    let browser = Browser::start();
    // DaisyUI 5 declares its theme variables behind a selector list like this one.
    std::fs::write(
        browser.directory.join("has.html"),
        r#"<!doctype html><title>Has selector</title><style>
        :where(:root),:root:has(input.theme-controller:checked),[data-theme=light]{--radius-field:7px}
        </style>"#,
    )
    .unwrap();
    let id = BrowserId::new_v4();
    browser.runtime.open(
        id,
        url::Url::from_file_path(browser.directory.join("has.html"))
            .unwrap()
            .to_string(),
    );
    browser.until("page load", || !browser.snapshot(id).loading);
    assert_eq!(
        browser.evaluate(
            id,
            "getComputedStyle(document.documentElement).getPropertyValue('--radius-field').trim()"
        ),
        "7px"
    );
    browser.runtime.close(id);
}

fn real_artifact_kit_styles_pages_and_round_trips_channel_events(browser: &Browser) {
    std::fs::write(
        browser.directory.join("artifact.html"),
        r#"<!doctype html><title>Artifact kit</title>
        <style>#own { color: rgb(1, 2, 3); }</style>
        <main data-drostje-artifact>
          <p id="own">Page styles win over the kit layer</p>
          <span id="bundled" style="font: 40px 'Lilex', serif">iiiiiiiiii</span>
          <span id="system" style="font: 40px serif">iiiiiiiiii</span>
          <form data-drostje="answer">
            <fieldset class="choices">
              <label><input type="radio" name="scope" value="crate" checked> crate</label>
              <label><input type="radio" name="scope" value="workspace"> workspace</label>
            </fieldset>
            <button type="submit" id="go" class="primary">Send</button>
            <span data-drostje-status id="status"></span>
          </form>
        </main>"#,
    )
    .unwrap();
    let url = url::Url::from_file_path(browser.directory.join("artifact.html"))
        .unwrap()
        .to_string();
    let id = BrowserId::new_v4();
    browser.runtime.open_artifact(
        id,
        url.clone(),
        format!(
            "{}:root{{--dk-accent:#123456;color-scheme:dark}}",
            drostje_fonts::artifact_font_faces(&["Lilex"])
        ),
    );
    browser.until("artifact load", || !browser.snapshot(id).loading);
    assert_eq!(
        browser.evaluate(
            id,
            "({api: typeof window.drostje.send, kit: !!document.getElementById('drostje-kit'), \
              accent: getComputedStyle(document.documentElement).getPropertyValue('--dk-accent').trim(), \
              own: getComputedStyle(document.getElementById('own')).color})"
        ),
        serde_json::json!({"api":"function","kit":true,"accent":"#123456","own":"rgb(1, 2, 3)"})
    );
    // Servo cannot see faces GPUI registered; the engine serves them to artifacts.
    browser.until(
        "the bundled monospace face renders, not the serif fallback",
        || {
            browser.evaluate(
                id,
                "document.getElementById('bundled').getBoundingClientRect().width > \
             1.5 * document.getElementById('system').getBoundingClientRect().width",
            ) == true
        },
    );

    browser.evaluate(id, "document.getElementById('go').click(), true");
    let events = std::cell::RefCell::new(Vec::new());
    browser.until("form submission reaches the channel", || {
        events.borrow_mut().extend(browser.runtime.take_channel(id));
        !events.borrow().is_empty()
    });
    let events = events.into_inner();
    let [event] = events.as_slice() else {
        panic!("one submission is one event: {events:?}");
    };
    assert_eq!(event.kind, "answer");
    let body: Value = serde_json::from_str(&event.body).unwrap();
    assert_eq!(body["data"]["scope"], "crate");
    assert_eq!(
        browser.evaluate(id, "document.querySelector('form').dataset.drostjeState"),
        "sending"
    );
    browser.runtime.receipt(
        id,
        event.id.clone(),
        ReceiptStatus::Queued,
        Some("Answer the agent's pending request first".into()),
    );
    browser.until("queued is shown but not final", || {
        browser.evaluate(id, "document.querySelector('form').dataset.drostjeState") == "queued"
    });
    assert_eq!(
        browser.evaluate(id, "document.getElementById('go').disabled"),
        true,
        "a queued event cannot be sent twice"
    );
    browser
        .runtime
        .receipt(id, event.id.clone(), ReceiptStatus::Delivered, None);
    browser.until("receipt settles the page", || {
        browser.evaluate(id, "document.querySelector('form').dataset.drostjeState") == "sent"
    });

    browser.evaluate(
        id,
        "window.rejected = null; try { drostje.send('Not valid') } catch (e) { window.rejected = e.name }; \
         drostje.send('Not valid').catch(e => { window.rejected = e.name }); true",
    );
    browser.until("invalid types are refused in the page", || {
        browser.evaluate(id, "window.rejected") == "TypeError"
    });
    assert!(browser.runtime.take_channel(id).is_empty());

    browser.runtime.command(
        id,
        BrowserCommand::ArtifactTheme {
            css: ":root{--dk-accent:#654321}".into(),
        },
    );
    browser.until("theme tokens update in place", || {
        browser.evaluate(
            id,
            "getComputedStyle(document.documentElement).getPropertyValue('--dk-accent').trim()",
        ) == "#654321"
    });

    let ordinary = BrowserId::new_v4();
    browser.runtime.open(ordinary, url);
    browser.until("ordinary load", || !browser.snapshot(ordinary).loading);
    assert_eq!(
        browser.evaluate(
            ordinary,
            "({api: typeof window.drostje, kit: !!document.getElementById('drostje-kit')})"
        ),
        serde_json::json!({"api":"undefined","kit":false}),
        "ordinary pages never get the kit or a channel"
    );
    assert_eq!(
        browser.evaluate(
            ordinary,
            "document.getElementById('bundled').getBoundingClientRect().width === \
             document.getElementById('system').getBoundingClientRect().width",
        ),
        true,
        "without the kit, the bundled face is not available to the page"
    );
    browser.runtime.close(id);
    browser.runtime.close(ordinary);
}

/// Renders the kit gallery with the injected kit in both Delft modes and saves PNGs for
/// review; fails on page script errors. Not a pixel assertion.
fn real_artifact_gallery_renders_in_both_theme_modes(browser: &Browser) {
    let gallery =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/artifact-kit");
    // The gallery's own <link> to kit.css is for standalone preview; drop it so this
    // renders exactly what DROSTJE injects.
    let page = std::fs::read_to_string(gallery.join("gallery.html"))
        .unwrap()
        .replace(r#"<link rel="stylesheet" href="kit.css">"#, "")
        .replace(r#"<script src="kit.js"></script>"#, "");
    std::fs::write(browser.directory.join("gallery.html"), page).unwrap();
    let url = url::Url::from_file_path(browser.directory.join("gallery.html"))
        .unwrap()
        .to_string();
    let themes = [
        (
            "light",
            ":root{--dk-bg:#f7f9fc;--dk-fg:#1b2740;--dk-strong:#1b2740;--dk-muted:#5a6680;--dk-line:#dde3ec;--dk-surface:#ecf0f6;--dk-tint:#2453a612;--dk-accent:#2453a6;--dk-ok:#3a7545;--dk-warn:#8a630f;--dk-bad:#b53a3a;--dk-info:#2453a6;--dk-sans:\"IBM Plex Sans\", system-ui, sans-serif;--dk-serif:\"Libron\", Georgia, serif;--dk-mono:\"Lilex\", ui-monospace, Menlo, monospace;--dk-size:15px;color-scheme:light}",
        ),
        (
            "dark",
            ":root{--dk-bg:#121b2e;--dk-fg:#dce3f0;--dk-strong:#dce3f0;--dk-muted:#8c98b0;--dk-line:#243049;--dk-surface:#18233a;--dk-tint:#8aaceb12;--dk-accent:#8aaceb;--dk-ok:#8cc796;--dk-warn:#e3bd62;--dk-bad:#e07a76;--dk-info:#8aaceb;--dk-sans:\"IBM Plex Sans\", system-ui, sans-serif;--dk-serif:\"Libron\", Georgia, serif;--dk-mono:\"Lilex\", ui-monospace, Menlo, monospace;--dk-size:15px;color-scheme:dark}",
        ),
    ];
    for (mode, theme) in themes {
        let id = BrowserId::new_v4();
        let theme = format!(
            "{}{theme}",
            drostje_fonts::artifact_font_faces(&["IBM Plex Sans", "Lilex", "Libron"])
        );
        browser.runtime.open_artifact(id, url.clone(), theme);
        browser.runtime.command(
            id,
            BrowserCommand::Resize {
                width: 760,
                height: 2600,
                scale: 1.0,
            },
        );
        browser.until("gallery frame", || {
            let snapshot = browser.snapshot(id);
            !snapshot.loading && snapshot.viewport_height == 2600 && snapshot.frame.is_some()
        });
        thread::sleep(Duration::from_millis(400));
        let frame = browser.snapshot(id).frame.expect("frame after settling");
        let output = std::env::temp_dir().join(format!("drostje-artifact-gallery-{mode}.png"));
        image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
            .unwrap()
            .save(&output)
            .unwrap();
        println!("{mode}: {}", output.display());
        let diagnostics = browser.runtime.diagnostics(id, 0).unwrap();
        let errors: Vec<_> = diagnostics["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|event| event["kind"] == "runtime")
            .collect();
        assert!(
            errors.is_empty(),
            "{mode} gallery script errors: {errors:?}"
        );
        browser.runtime.close(id);
    }
}
