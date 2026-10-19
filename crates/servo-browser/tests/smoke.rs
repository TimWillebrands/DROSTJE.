//! Run explicitly in a desktop session:
//! cargo test --manifest-path crates/servo-browser/Cargo.toml --test smoke -- --ignored
#[path = "../src/protocol.rs"]
mod protocol;

use std::{
    fs, io,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use protocol::{BrowserCommand, Event, Request};
use serde_json::Value;
use uuid::Uuid;

struct Browser {
    child: Child,
    input: ChildStdin,
    events: mpsc::Receiver<io::Result<(Event, Vec<u8>)>>,
    directory: std::path::PathBuf,
}

impl Browser {
    fn start() -> Self {
        let directory =
            std::env::temp_dir().join(format!("drostje-servo-smoke-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("style.css"), "html,body { margin:0; background:rgb(12,34,56); } button { position:absolute; left:20px; top:20px; width:120px; height:60px; } input { position:absolute; left:20px; top:120px; }").unwrap();
        fs::write(directory.join("index.html"), "<!doctype html><title>Servo smoke</title><link rel=stylesheet href=style.css><button onclick='window.clicked=(window.clicked||0)+1'>Click</button><input id=text><script>window.clicked=0</script>").unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_drostje-servo"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let (sender, events) = mpsc::sync_channel(protocol::MAX_PENDING);
        thread::spawn(move || {
            loop {
                match protocol::read_packet::<Event>(&mut output) {
                    Ok(Some(packet)) => {
                        if sender.send(Ok(packet)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        Self {
            child,
            input,
            events,
            directory,
        }
    }

    fn send(&mut self, request: Request) {
        protocol::write_packet(&mut self.input, &request, &[]).unwrap();
    }

    fn next(&self, deadline: Instant) -> (Event, Vec<u8>) {
        let packet = self
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("Servo did not produce the expected event before the deadline")
            .expect("Servo binary protocol failed");
        match &packet.0 {
            Event::Fatal { error } => panic!("Servo failed: {error}"),
            Event::State { state, .. } if state.error.is_some() => {
                panic!("Page failed: {:?}", state.error)
            }
            _ => {}
        }
        packet
    }

    fn evaluate(&mut self, id: Uuid, script: &str, deadline: Instant) -> Value {
        let request_id = Uuid::new_v4().to_string();
        self.send(Request::Command {
            id,
            command: BrowserCommand::Evaluate {
                request_id: request_id.clone(),
                script: script.into(),
            },
        });
        loop {
            if let (
                Event::Evaluation {
                    request_id: actual,
                    result,
                    ..
                },
                _,
            ) = self.next(deadline)
                && actual == request_id
            {
                return result.expect("JavaScript evaluation failed");
            }
        }
    }

    fn click(&mut self, id: Uuid, x: f32, y: f32) {
        for pressed in [true, false] {
            self.send(Request::Command {
                id,
                command: BrowserCommand::Pointer {
                    x,
                    y,
                    button: 0,
                    pressed,
                },
            });
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = protocol::write_packet(&mut self.input, &Request::Shutdown, &[]);
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
#[ignore = "requires a working native Servo renderer and desktop graphics session"]
fn real_html_pixels_javascript_click_text_resize_and_history() {
    let mut browser = Browser::start();
    let id = Uuid::new_v4();
    let url = url::Url::from_file_path(browser.directory.join("index.html"))
        .unwrap()
        .to_string();
    let open = Request::Open {
        id,
        url: url.clone(),
    };
    assert_eq!(open.id(), Some(id));
    browser.send(open);
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut loaded = false;
    let mut painted = false;
    while !loaded || !painted {
        match browser.next(deadline) {
            (Event::State { state, .. }, _) => {
                loaded |= !state.loading && state.title == "Servo smoke"
            }
            (Event::Frame { width, height, .. }, bytes) => {
                assert_eq!(bytes.len(), protocol::frame_len(width, height).unwrap());
                let offset = ((height as usize - 1) * width as usize + width as usize - 1) * 4;
                painted |= bytes[offset..offset + 4] == [12, 34, 56, 255];
            }
            _ => {}
        }
    }
    assert_eq!(
        browser.evaluate(id, "document.title", deadline),
        "Servo smoke"
    );
    browser.click(id, 50.0, 40.0);
    while browser.evaluate(id, "window.clicked", deadline).as_f64() != Some(1.0) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    browser.click(id, 50.0, 130.0);
    while browser.evaluate(id, "document.activeElement.id", deadline) != "text" {
        thread::sleep(Duration::from_millis(20));
    }
    browser.send(Request::Command {
        id,
        command: BrowserCommand::Text {
            text: "Servo ✓".into(),
        },
    });
    while browser.evaluate(id, "document.querySelector('input').value", deadline) != "Servo ✓" {
        thread::sleep(Duration::from_millis(20));
    }
    browser.send(Request::Command {
        id,
        command: BrowserCommand::Resize {
            width: 640,
            height: 480,
            scale: 1.0,
        },
    });
    loop {
        if let (
            Event::Frame {
                width: 640,
                height: 480,
                ..
            },
            bytes,
        ) = browser.next(deadline)
        {
            assert_eq!(bytes.len(), 640 * 480 * 4);
            break;
        }
    }
    browser.send(Request::Command {
        id,
        command: BrowserCommand::Navigate {
            url: "about:blank".into(),
        },
    });
    loop {
        if let (Event::State { state, .. }, _) = browser.next(deadline)
            && state.url == "about:blank"
            && !state.loading
            && state.can_go_back
        {
            break;
        }
    }
    browser.send(Request::Command {
        id,
        command: BrowserCommand::Back,
    });
    loop {
        if let (Event::State { state, .. }, _) = browser.next(deadline)
            && state.url == url
            && !state.loading
            && state.can_go_forward
        {
            break;
        }
    }
}
