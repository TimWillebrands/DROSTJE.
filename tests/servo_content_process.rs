//! Opt-in real Servo regression: a page whose content process dies shows the
//! crash state, DROSTJE keeps running, and Reload restores the page.
//! cargo test --features runtime-qa --test servo_content_process
//!
//! Servo re-executes the current binary to host content processes, so this test
//! owns `main` and serves them exactly as DROSTJE's binary does.
use std::{
    process::Command,
    thread,
    time::{Duration, Instant},
};

use drostje::browser::{self, BrowserCommand, BrowserId, BrowserRuntime, BrowserSnapshot};
use serde_json::Value;

fn main() {
    if browser::serve_content_process() {
        return;
    }
    let directory =
        std::env::temp_dir().join(format!("drostje-content-process-{}", BrowserId::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    // A live timer reproduces the original crash shape: script work pending
    // when the process holding the page goes away. The `:has()` selector list
    // needs the Stylo pref, which each content process must set for its layout.
    std::fs::write(
        directory.join("index.html"),
        "<!doctype html><title>Content process</title>\
         <style>:where(:root),:root:has(input:checked){--radius-field:7px}</style>\
         <script>window.ticks=0; setInterval(() => window.ticks++, 10)</script>",
    )
    .unwrap();
    let url = format!("file://{}", directory.join("index.html").display());

    let runtime = BrowserRuntime::new();
    let id = BrowserId::new_v4();
    runtime.open(id, url);
    until("initial load", || {
        snapshot(&runtime, id).is_some_and(|s| !s.loading && s.error.is_none())
    });
    until("page timers running", || {
        evaluate(&runtime, id, "window.ticks > 0") == Value::Bool(true)
    });
    assert_eq!(
        evaluate(
            &runtime,
            id,
            "getComputedStyle(document.documentElement).getPropertyValue('--radius-field').trim()"
        ),
        "7px"
    );

    let processes = content_processes();
    assert!(
        !processes.is_empty(),
        "Pages must run in a Servo content process, not in DROSTJE's process"
    );
    for pid in &processes {
        let status = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap();
        assert!(status.success(), "Could not kill content process {pid}");
    }

    until("crash reported on the page", || {
        snapshot(&runtime, id)
            .and_then(|s| s.error)
            .is_some_and(|error| error.contains("content process exited unexpectedly"))
    });

    runtime.command(id, BrowserCommand::Reload);
    until("reloaded page running in a new process", || {
        snapshot(&runtime, id).is_some_and(|s| !s.loading && s.error.is_none())
            && evaluate(&runtime, id, "document.title + ':' + (window.ticks > 0)")
                == Value::String("Content process:true".into())
    });
    let replacements = content_processes();
    assert!(
        replacements.iter().all(|pid| !processes.contains(pid)) && !replacements.is_empty(),
        "Reload must start a new content process"
    );

    runtime.shutdown();
    assert!(
        runtime.wait_for_shutdown(Duration::from_secs(30)),
        "Servo did not shut down"
    );
    std::fs::remove_dir_all(&directory).unwrap();
    println!("servo_content_process: ok");
}

fn snapshot(runtime: &BrowserRuntime, id: BrowserId) -> Option<BrowserSnapshot> {
    runtime.snapshot(id)
}

fn evaluate(runtime: &BrowserRuntime, id: BrowserId, script: &str) -> Value {
    let Ok(request_id) = runtime.evaluate(id, script.into()) else {
        return Value::Null;
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(result) = runtime.evaluation(&request_id) {
            return result.unwrap_or(Value::Null);
        }
        thread::sleep(Duration::from_millis(10));
    }
    runtime.discard_evaluation(&request_id);
    Value::Null
}

fn until(label: &str, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(45);
    while !predicate() {
        assert!(Instant::now() < deadline, "Timed out: {label}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Live content processes spawned by this process. Killed ones linger as
/// zombies until Servo reaps them, so their state is excluded.
fn content_processes() -> Vec<u32> {
    let output = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,stat=,command="])
        .output()
        .unwrap();
    let parent = std::process::id();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid: u32 = fields.next()?.parse().ok()?;
            let stat = fields.next()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            (ppid == parent && !stat.starts_with('Z') && command.contains("--content-process"))
                .then_some(pid)
        })
        .collect()
}
