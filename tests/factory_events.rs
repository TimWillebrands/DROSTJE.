use drostje::{
    factory::{
        EventEntry, FactoryLaunchSpec, FactorySnapshot, FactorySource, Liveness, ReportedState,
        TaskSnapshot,
    },
    model::{self, FactoryLaunch, LaunchId},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

// Exercise the internal tracker without exposing implementation-only operations
// in the application's public API. This is the same module used by the manager.
#[path = "../src/factory/events.rs"]
mod events;
use events::{EventTracker, read_text};

struct TempDirectory(PathBuf);
impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("drostje-factory-events-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn launch(root: &Path) -> FactoryLaunch {
    let id = Uuid::new_v4();
    let spec = FactoryLaunchSpec {
        id,
        project_id: Uuid::new_v4(),
        source: FactorySource::AzureWorkItem(42),
        target_repo: root.join("repo"),
        target_branch: "main".into(),
        data_root: root.join("shared"),
        log_dir: root
            .join("factory-launches")
            .join(id.to_string())
            .join("logs"),
        harness: "opencode".into(),
        model: None,
        thought_level: None,
        executable: None,
        confirm_unknown: false,
    };
    fs::create_dir_all(&spec.log_dir).unwrap();
    fs::create_dir_all(spec.data_root.join("events")).unwrap();
    FactoryLaunch {
        id,
        project_id: spec.project_id,
        factory_thread_id: Uuid::new_v4(),
        descriptor: serde_json::to_value(spec).unwrap(),
        event_path: None,
        work_item_id: None,
        attempt_id: None,
        process_exit: None,
        launch_error: None,
        created_at: "2026-09-15T12:00:00Z".into(),
    }
}

fn event(kind: &str, payload: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1, "timestampUtc": "2026-09-15T12:01:00Z",
        "eventType": kind, "payload": payload,
    }))
    .unwrap();
    bytes.push(b'\n');
    bytes
}

fn start(launch: &FactoryLaunch) -> Vec<u8> {
    let spec: FactoryLaunchSpec = serde_json::from_value(launch.descriptor.clone()).unwrap();
    event(
        "run-started",
        json!({"userStoryId": 42, "logPath": spec.log_dir.join("run.log")}),
    )
}

fn log_path(root: &Path, name: &str) -> PathBuf {
    let directory = root.join("shared/events/nested");
    fs::create_dir_all(&directory).unwrap();
    directory.join(format!("{name}.jsonl"))
}

fn append(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn scan(tracker: &mut EventTracker, launch: &FactoryLaunch) -> FactorySnapshot {
    tracker
        .scan(std::slice::from_ref(launch), &BTreeMap::new())
        .remove(0)
}

#[test]
fn split_utf8_json_unknown_events_and_task_failure_do_not_terminalize_a_run() {
    let directory = TempDirectory::new();
    let launch = launch(&directory.0);
    let path = log_path(&directory.0, "split");
    fs::write(&path, start(&launch)).unwrap();
    let mut tracker = EventTracker::new();
    assert_eq!(
        scan(&mut tracker, &launch).reported_state,
        ReportedState::Running
    );

    let story = event(
        "story-loaded",
        json!({"userStoryId": 42, "title": "日本語", "state": "Active", "tags": ["ready-for-agent"]}),
    );
    let split = story.iter().position(|byte| *byte == 0xe6).unwrap() + 1;
    append(&path, &story[..split]);
    let partial = scan(&mut tracker, &launch);
    assert_eq!(partial.notes.len(), 1);
    assert!(partial.diagnostics.is_empty());
    append(&path, &story[split..story.len() - 1]);
    assert_eq!(scan(&mut tracker, &launch).notes.len(), 1);
    append(&path, b"\n");
    assert_eq!(scan(&mut tracker, &launch).title.as_deref(), Some("日本語"));

    append(&path, b"{broken JSON}\n");
    append(
        &path,
        &event("future-event", json!({"message": "retained generic event"})),
    );
    append(
        &path,
        &event(
            "task-dag-loaded",
            json!({
                "tasks": [{"id": 1, "state": "New"}, {"id": 2, "state": "New"}],
                "dependencyLinks": [{"predecessorId": 1, "successorId": 2}],
            }),
        ),
    );
    append(
        &path,
        &event(
            "slice-started",
            json!({"taskId": 2, "sliceBranch": "slice/2"}),
        ),
    );
    append(
        &path,
        &event(
            "slice-step-started",
            json!({"taskId": 2, "slug": "implement", "index": 1, "total": 3}),
        ),
    );
    append(
        &path,
        &event(
            "slice-failed",
            json!({
                "taskId": 2, "role": "implementer", "status": "Failed", "stopReason": "Error",
                "stopFailure": "AgentFailure", "failureDetail": "specific failure", "sawPromptActivity": true, "transcript": null,
            }),
        ),
    );
    append(
        &path,
        &event(
            "agent-note",
            json!({"taskId": 2, "attemptId": "a", "step": "implement", "kind": "done", "text": "I finished", "paths": [], "sha": null, "id": "note1"}),
        ),
    );
    let failed = scan(&mut tracker, &launch);
    assert_eq!(failed.reported_state, ReportedState::Running);
    assert_eq!(failed.tasks["2"].state, "Failed");
    assert_eq!(failed.tasks["2"].dependencies, ["1"]);
    assert_eq!(failed.tasks["2"].branch.as_deref(), Some("slice/2"));
    assert_eq!(failed.tasks["2"].step.as_deref(), Some("implement"));
    assert!(
        failed.tasks["2"]
            .diagnostic
            .as_ref()
            .unwrap()
            .contains("specific failure")
    );
    assert!(
        failed
            .notes
            .iter()
            .any(|entry| entry.event_type == "future-event"
                && entry.payload["message"] == "retained generic event")
    );
    assert!(
        failed
            .diagnostics
            .iter()
            .any(|message| message.contains("Malformed"))
    );

    append(&path, b"{\"schemaVersion\":2,\"timestampUtc\":\"2026-09-15T12:02:00Z\",\"eventType\":\"run-completed\",\"payload\":{\"text\":\"unsupported\"}}\n");
    append(
        &path,
        &event("run-completed", json!({"notText": "malformed terminal"})),
    );
    let unsupported = scan(&mut tracker, &launch);
    assert_eq!(unsupported.reported_state, ReportedState::Running);
    assert!(
        unsupported
            .diagnostics
            .iter()
            .any(|message| message.contains("Unsupported event schema"))
    );
    append(
        &path,
        &event(
            "run-paused",
            json!({"taskId": 2, "reason": "NeedsHuman", "text": "Please inspect"}),
        ),
    );
    assert_eq!(
        scan(&mut tracker, &launch).reported_state,
        ReportedState::Paused
    );
}

#[test]
fn exact_log_directory_not_story_identity_controls_correlation_and_ambiguity() {
    let directory = TempDirectory::new();
    let first = launch(&directory.0);
    let second = launch(&directory.0);
    let missing = launch(&directory.0);
    let first_path = log_path(&directory.0, "first");
    let second_path = log_path(&directory.0, "second");
    fs::write(&first_path, start(&first)).unwrap();
    fs::write(&second_path, start(&second)).unwrap();
    let mut tracker = EventTracker::new();
    let snapshots = tracker.scan(
        &[first.clone(), second.clone(), missing.clone()],
        &BTreeMap::new(),
    );
    assert_eq!(snapshots[0].launch.event_path.as_ref(), Some(&first_path));
    assert_eq!(snapshots[1].launch.event_path.as_ref(), Some(&second_path));
    assert_eq!(snapshots[0].launch.work_item_id, Some(42));
    assert_eq!(snapshots[1].launch.work_item_id, Some(42));
    assert!(snapshots[2].launch.event_path.is_none());
    assert_eq!(snapshots[2].reported_state, ReportedState::Pending);

    let missing_spec: FactoryLaunchSpec =
        serde_json::from_value(missing.descriptor.clone()).unwrap();
    let prefix_collision = PathBuf::from(format!(
        "{}-another/run.log",
        missing_spec.log_dir.display()
    ));
    fs::write(
        log_path(&directory.0, "prefix"),
        event(
            "run-started",
            json!({"userStoryId": 42, "logPath": prefix_collision}),
        ),
    )
    .unwrap();
    assert!(scan(&mut tracker, &missing).launch.event_path.is_none());

    let duplicate = log_path(&directory.0, "duplicate");
    fs::write(&duplicate, start(&first)).unwrap();
    let ambiguous = scan(&mut tracker, &snapshots[0].launch);
    assert_eq!(ambiguous.reported_state, ReportedState::Pending);
    assert_eq!(
        ambiguous.correlation_candidates,
        [duplicate, first_path.clone()]
    );
    assert!(ambiguous.tasks.is_empty());
    // Durable linkage survives without choosing either competing projection.
    assert_eq!(ambiguous.launch.event_path, Some(first_path));
    assert_eq!(
        scan(&mut tracker, &second).launch.event_path,
        Some(second_path)
    );
}

#[test]
fn replay_truncation_and_file_replacement_replace_the_projection_but_missing_files_retain_history()
{
    let directory = TempDirectory::new();
    let launch = launch(&directory.0);
    let path = log_path(&directory.0, "replay");
    let mut bytes = start(&launch);
    bytes.extend(event("attempt-ready", json!({"userStoryId": 42, "attemptId": "old-attempt", "targetBranch": "main", "baseSha": "abc", "mode": "resume"})));
    bytes.extend(event(
        "slice-merged",
        json!({"taskId": 7, "text": "merged"}),
    ));
    bytes.extend(event("run-completed", json!({"text": "completed"})));
    fs::write(&path, &bytes).unwrap();
    let mut tracker = EventTracker::new();
    let original = scan(&mut tracker, &launch);
    tracker.refresh();
    let replayed = scan(&mut tracker, &original.launch);
    assert_eq!(replayed.notes, original.notes);
    assert_eq!(replayed.tasks, original.tasks);
    assert_eq!(replayed.reported_state, ReportedState::Completed);
    fs::write(&path, start(&launch)).unwrap();
    let truncated = scan(&mut tracker, &original.launch);
    assert_eq!(truncated.notes.len(), 1);
    assert!(truncated.tasks.is_empty());
    assert_eq!(truncated.reported_state, ReportedState::Running);
    assert!(truncated.launch.attempt_id.is_none());

    let replacement = path.with_extension("replacement");
    let mut replacement_bytes = start(&launch);
    replacement_bytes.extend(event(
        "run-paused",
        json!({"taskId": 3, "reason": "Review", "text": "needs review"}),
    ));
    fs::write(&replacement, replacement_bytes).unwrap();
    fs::rename(replacement, &path).unwrap();
    let replaced = scan(&mut tracker, &truncated.launch);
    assert_eq!(replaced.reported_state, ReportedState::Paused);
    assert_eq!(replaced.notes.len(), 2);
    fs::remove_file(&path).unwrap();
    tracker.refresh();
    let missing = scan(&mut tracker, &replaced.launch);
    assert_eq!(missing.notes, replaced.notes);
    assert_eq!(missing.reported_state, ReportedState::Paused);
    assert_eq!(missing.launch.event_path.as_ref(), Some(&path));
    assert!(
        missing
            .diagnostics
            .iter()
            .any(|message| message.contains("unavailable"))
    );
    let restarted = scan(&mut EventTracker::new(), &missing.launch);
    assert_eq!(restarted.launch.event_path, Some(path));
    assert_eq!(restarted.reported_state, ReportedState::Pending);
    assert!(
        restarted
            .diagnostics
            .iter()
            .any(|message| message.contains("unavailable"))
    );
}

#[test]
fn exit_zero_is_not_completion_and_terminal_replay_survives_lost_process_ownership() {
    let directory = TempDirectory::new();
    let mut launch = launch(&directory.0);
    let path = log_path(&directory.0, "exit");
    fs::write(&path, start(&launch)).unwrap();
    let mut tracker = EventTracker::new();
    let owned = BTreeMap::<LaunchId, Liveness>::from([(launch.id, Liveness::OwnedRunning)]);
    assert_eq!(
        tracker.scan(&[launch.clone()], &owned)[0].liveness,
        Liveness::OwnedRunning
    );
    launch.process_exit = Some(0);
    let exited = scan(&mut tracker, &launch);
    assert_eq!(exited.liveness, Liveness::Exited(0));
    assert_eq!(exited.reported_state, ReportedState::Running);
    assert!(!exited.status_text().contains("Completed"));
    launch = exited.launch;
    launch.process_exit = None;
    let unverified = scan(&mut EventTracker::new(), &launch);
    assert_eq!(unverified.liveness, Liveness::Unverified);
    assert_eq!(unverified.reported_state, ReportedState::Running);
    append(
        &path,
        &event("run-completed", json!({"text": "Factory says complete"})),
    );
    let terminal = scan(&mut EventTracker::new(), &launch);
    assert_eq!(terminal.reported_state, ReportedState::Completed);
    assert_eq!(terminal.liveness, Liveness::Unverified);
    assert!(terminal.status_text().contains("Completed"));
}

#[test]
fn full_task_terminal_and_artifact_projection_outlives_the_recent_note_window() {
    let directory = TempDirectory::new();
    let launch = launch(&directory.0);
    let path = log_path(&directory.0, "full");
    let transcript = directory.0.join("agent.log");
    let artifact = directory.0.join("findings.json");
    fs::write(&transcript, b"agent output\n").unwrap();
    let mut bytes = start(&launch);
    bytes.extend(event(
        "story-loaded",
        json!({"userStoryId": 42, "title": "Kept title", "state": "Active", "tags": []}),
    ));
    bytes.extend(event("attempt-ready", json!({"userStoryId": 42, "attemptId": "attempt-a", "targetBranch": "main", "baseSha": "abc", "mode": "new", "harness": "opencode", "model": "chosen"})));
    bytes.extend(event(
        "agent-selected",
        json!({"userStoryId": 42, "attemptId": "attempt-a", "harness": "opencode", "model": "actual"}),
    ));
    bytes.extend(event(
        "feature-branch-ready",
        json!({"featureBranch": "feature/42"}),
    ));
    bytes.extend(event("active-sandbox", json!({"identity": "container", "role": "worker", "taskId": 8, "sliceBranch": "slice/8", "containerName": "sandbox8", "transcriptPath": transcript})));
    bytes.extend(event("review-findings-recorded", json!({"taskId": 8, "sliceSha": "sha", "total": 1, "countsBySeverity": {"error": 1}, "artifactRef": artifact})));
    bytes.extend(event(
        "slice-merged",
        json!({"taskId": 8, "text": "Merged"}),
    ));
    bytes.extend(event("run-completed", json!({"text": "Done"})));
    for index in 0..205 {
        bytes.extend(event(
            "log-line",
            json!({"level": 0, "text": format!("line {index}")}),
        ));
    }
    fs::write(&path, bytes).unwrap();
    let snapshot = scan(&mut EventTracker::new(), &launch);
    assert_eq!(snapshot.notes.len(), 200);
    assert_eq!(snapshot.notes[0].payload["text"], "line 5");
    assert_eq!(snapshot.tasks["8"].state, "Merged");
    assert_eq!(snapshot.reported_state, ReportedState::Completed);
    assert_eq!(snapshot.title.as_deref(), Some("Kept title"));
    assert_eq!(snapshot.launch.attempt_id.as_deref(), Some("attempt-a"));
    assert_eq!(snapshot.details["agent-selected"]["model"], "actual");
    assert_eq!(
        snapshot.details["feature-branch-ready"]["featureBranch"],
        "feature/42"
    );
    assert_eq!(snapshot.transcripts, [transcript]);
    assert!(snapshot.artifacts.contains(&artifact));
}

#[test]
fn text_pages_are_bounded_complete_utf8_lines_with_nonoverlapping_older_history() {
    let directory = TempDirectory::new();
    let path = directory.0.join("transcript.log");
    let lines: Vec<_> = (0..500).map(|index| format!("{index}: 日本語\n")).collect();
    fs::write(&path, format!("{}partial 日", lines.concat())).unwrap();
    smol::block_on(async {
        let tail = read_text(path.clone(), None).await.unwrap();
        assert_eq!(tail.text, lines[300..].concat());
        assert!(tail.has_older);
        let older = read_text(path.clone(), Some(tail.start_offset))
            .await
            .unwrap();
        assert_eq!(older.text, lines[100..300].concat());
        assert_eq!(older.end_offset, tail.start_offset);
        let oldest = read_text(path.clone(), Some(older.start_offset))
            .await
            .unwrap();
        assert_eq!(oldest.text, lines[..100].concat());
        assert!(!oldest.has_older);
        append(&path, "本語\n".as_bytes());
        assert!(
            read_text(path.clone(), None)
                .await
                .unwrap()
                .text
                .ends_with("partial 日本語\n")
        );
        fs::write(&path, b"short\n").unwrap();
        assert!(
            read_text(path.clone(), Some(tail.start_offset))
                .await
                .is_err()
        );
        fs::write(&path, format!("{}\n", "x".repeat(1024 * 1024))).unwrap();
        assert!(read_text(path.clone(), None).await.is_err());
        append(&path, lines.concat().as_bytes());
        assert_eq!(
            read_text(path.clone(), None).await.unwrap().text,
            lines[300..].concat()
        );
        fs::remove_file(&path).unwrap();
        assert!(read_text(path.clone(), None).await.is_err());
    });
}
