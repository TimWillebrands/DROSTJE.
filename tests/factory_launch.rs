use drostje::{
    factory::{
        FactoryLaunchSpec, FactoryManager, FactorySource, Liveness, ReportedState, check_readiness,
    },
    model::{
        FactoryLaunch, FactorySourceKind, FactoryThreadConfig, Project, ProjectDefaults, Thread,
        ThreadKind,
    },
    store::Store,
};
use rusqlite::Connection;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct TempDirectory(PathBuf);
impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("drostje-factory-launch-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    store: Store,
    project: Project,
    data: PathBuf,
    executable: PathBuf,
    folder: PathBuf,
    directory: TempDirectory,
}

impl Fixture {
    async fn new(doctor: &str, run: &str) -> Self {
        let directory = TempDirectory::new();
        let repo = directory.0.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("notes.txt"), "committed fixture\n").unwrap();
        git(&repo, &["add", "notes.txt"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=DROSTJE.",
                "-c",
                "user.email=drostje@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "Factory launcher fixture",
            ],
        );
        let data = directory.0.join("drostje-data");
        let store = Store::open(data.clone()).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Factory fixture".into(),
            root: repo,
            archived: false,
            defaults: ProjectDefaults::default(),
        };
        store.save_project(project.clone()).await.unwrap();
        let folder = directory.0.join("existing-folder");
        fs::create_dir(&folder).unwrap();
        // Intentionally not normalized: opening an existing folder is pass-through.
        fs::write(
            folder.join("PRD.md"),
            "---\ntitle: Original work item\ntags: []\n---\nUnchanged source text\n",
        )
        .unwrap();
        let executable = directory.0.join("process-fixture");
        // This finite local process fixture exercises actual spawn/session/fd
        // behavior. It is not a substitute for Factory's real doctor/e2e proof.
        fs::write(
            &executable,
            format!(
                r#"#!/bin/sh
set -eu
root=${{0%/*}}
case "$1" in
  doctor)
    printf 'doctor\n' >> "$root/invocations"
    {doctor}
    ;;
  run)
    printf 'run\n' >> "$root/invocations"
    {run}
    ;;
  *) exit 64 ;;
esac
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            store,
            project,
            data,
            executable,
            folder,
            directory,
        }
    }

    async fn owner(&self) -> Uuid {
        let id = Uuid::new_v4();
        self.store
            .save_thread(Thread {
                id,
                project_id: self.project.id,
                kind: ThreadKind::Factory,
                factory: Some(FactoryThreadConfig {
                    source_kind: FactorySourceKind::Folder,
                    source: self.folder.to_string_lossy().into_owned(),
                    harness: "opencode".into(),
                    model: String::new(),
                    thought_level: String::new(),
                    target_branch: "main".into(),
                }),
                title: "Factory fixture".into(),
                harness: String::new(),
                session_ref: None,
                model: None,
                draft: String::new(),
                attachments: Vec::new(),
                submission: None,
                archived: false,
                created_at: "2026-09-15T12:00:00Z".into(),
                updated_at: "2026-09-15T12:00:00Z".into(),
            })
            .await
            .unwrap();
        id
    }

    fn spec(&self) -> FactoryLaunchSpec {
        let id = Uuid::new_v4();
        FactoryLaunchSpec {
            id,
            project_id: self.project.id,
            source: FactorySource::LocalFolder(self.folder.clone()),
            target_repo: self.project.root.clone(),
            target_branch: "main".into(),
            data_root: self.directory.0.join("shared"),
            log_dir: self
                .data
                .join("factory-launches")
                .join(id.to_string())
                .join("logs"),
            harness: "opencode".into(),
            model: None,
            thought_level: None,
            executable: Some(self.executable.clone()),
            confirm_unknown: false,
        }
    }

    fn manager(&self, launches: Vec<FactoryLaunch>) -> FactoryManager {
        FactoryManager::open(self.store.clone(), self.data.clone(), launches).unwrap()
    }

    async fn disable_storage(&self) {
        let blocker = Connection::open(self.data.join("drostje.sqlite3")).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(
            self.store
                .set_setting("blocked-write".into(), json!(true))
                .await
                .is_err()
        );
        blocker.execute_batch("ROLLBACK").unwrap();
    }

    fn run_count(&self) -> usize {
        fs::read_to_string(self.directory.0.join("invocations"))
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == "run")
            .count()
    }
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for the process fixture"
        );
        smol::Timer::after(Duration::from_millis(10)).await;
    }
}

fn output_path(spec: &FactoryLaunchSpec) -> PathBuf {
    spec.log_dir.parent().unwrap().join("stdout.log")
}

const WAIT_DOCTOR: &str = r#"
    : > "$root/doctor-started"
    n=0
    while [ ! -f "$root/release-doctor" ]; do
      n=$((n + 1)); [ "$n" -lt 600 ] || exit 75
      [ -d "$root" ] || exit 75
      /bin/sleep 0.025
    done
    exit 0
"#;

const WAIT_RUN: &str = r#"
    printf '%s\nbefore close\n' "$$"
    n=0
    while [ ! -f "$root/release-run" ]; do
      n=$((n + 1)); [ "$n" -lt 600 ] || exit 75
      [ -d "$root" ] || exit 75
      /bin/sleep 0.025
    done
    printf 'after close\n'
    printf 'final diagnostic\n' >&2
    exit 0
"#;

#[test]
fn nonwritable_store_prevents_any_factory_process_or_source_mutation() {
    smol::block_on(async {
        let fixture = Fixture::new("exit 0", "exit 0").await;
        let original = fs::read(fixture.folder.join("PRD.md")).unwrap();
        let owner = fixture.owner().await;
        fixture.disable_storage().await;
        let manager = fixture.manager(Vec::new());
        assert!(manager.dispatch(owner, fixture.spec()).await.is_err());
        assert!(!fixture.directory.0.join("invocations").exists());
        assert!(fixture.store.snapshot().await.unwrap().launches.is_empty());
        assert_eq!(fs::read(fixture.folder.join("PRD.md")).unwrap(), original);
    });
}

#[test]
fn pending_is_durable_blocks_duplicates_and_rechecks_storage_after_doctor() {
    smol::block_on(async {
        let fixture = Fixture::new(WAIT_DOCTOR, "exit 0").await;
        let manager = fixture.manager(Vec::new());
        let spec = fixture.spec();
        let id = spec.id;
        let owner = fixture.owner().await;
        let worker = manager.clone();
        let dispatch = thread::spawn(move || smol::block_on(worker.dispatch(owner, spec)));
        wait_until(|| fixture.directory.0.join("doctor-started").exists()).await;
        let durable = fixture.store.snapshot().await.unwrap();
        assert_eq!(durable.launches.len(), 1);
        assert_eq!(durable.launches[0].id, id);
        assert_eq!(durable.launches[0].factory_thread_id, owner);
        assert_eq!(durable.launches[0].process_exit, None);
        assert_eq!(fixture.run_count(), 0);
        // Confirmation only permits an unknown prior outcome, never an owned or
        // pending duplicate. A second launch ID cannot bypass the pending guard.
        let mut duplicate = fixture.spec();
        duplicate.confirm_unknown = true;
        assert!(
            manager
                .dispatch(fixture.owner().await, duplicate)
                .await
                .is_err()
        );
        assert_eq!(fixture.store.snapshot().await.unwrap().launches.len(), 1);
        fixture.disable_storage().await;
        fs::write(fixture.directory.0.join("release-doctor"), "release").unwrap();
        assert!(dispatch.join().unwrap().is_err());
        assert_eq!(fixture.run_count(), 0);
        let refused = manager.refresh(id).await.unwrap();
        assert!(refused.launch.launch_error.is_some());
        assert_eq!(refused.liveness, Liveness::Unverified);
        assert!(
            refused
                .diagnostics
                .iter()
                .any(|message| message.contains("persistence"))
        );
        // Failure to persist the refusal must not make the in-memory refusal
        // disappear, nor claim it was successfully written to the database.
        assert!(
            fixture.store.snapshot().await.unwrap().launches[0]
                .launch_error
                .is_none()
        );
    });
}

#[test]
fn doctor_core_failure_preserves_output_and_a_durable_refusal_without_a_run() {
    smol::block_on(async {
        let fixture = Fixture::new(
            "printf 'core diagnostic 日本語\\n'; printf 'provider unavailable\\n' >&2; exit 1",
            "exit 0",
        )
        .await;
        let readiness = check_readiness(
            Some(fixture.executable.clone()),
            fixture.project.root.clone(),
            "opencode".into(),
        )
        .await
        .unwrap_err();
        assert!(readiness.to_string().contains("core diagnostic 日本語"));
        assert!(readiness.to_string().contains("provider unavailable"));
        let manager = fixture.manager(Vec::new());
        let spec = fixture.spec();
        let id = spec.id;
        let stderr = spec.log_dir.parent().unwrap().join("stderr.log");
        assert!(manager.dispatch(fixture.owner().await, spec).await.is_err());
        assert_eq!(fixture.run_count(), 0);
        let refused = manager.refresh(id).await.unwrap();
        assert_eq!(refused.reported_state, ReportedState::Pending);
        assert!(
            refused
                .launch
                .launch_error
                .as_ref()
                .unwrap()
                .contains("provider unavailable")
        );
        assert!(
            fs::read_to_string(stderr)
                .unwrap()
                .contains("core diagnostic 日本語")
        );
        let durable = fixture.store.snapshot().await.unwrap().launches.remove(0);
        assert_eq!(durable.launch_error, refused.launch.launch_error);
        assert_eq!(durable.process_exit, None);
    });
}

#[test]
fn detached_child_keeps_fds_across_manager_close_and_restart_never_replays() {
    smol::block_on(async {
        let fixture = Fixture::new("exit 0", WAIT_RUN).await;
        let original_prd = fs::read(fixture.folder.join("PRD.md")).unwrap();
        let manager = fixture.manager(Vec::new());
        let first = fixture.spec();
        let first_output = output_path(&first);
        let first_id = manager
            .dispatch(fixture.owner().await, first.clone())
            .await
            .unwrap();
        wait_until(|| {
            fs::read_to_string(&first_output).is_ok_and(|text| text.contains("before close"))
        })
        .await;
        let pid: libc::pid_t = fs::read_to_string(&first_output)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        // Inspect only the newly owned fixture process; never signal a stored PID.
        assert_eq!(unsafe { libc::getsid(pid) }, pid);
        assert_eq!(
            manager.refresh(first_id).await.unwrap().liveness,
            Liveness::OwnedRunning
        );
        let mut duplicate = fixture.spec();
        duplicate.confirm_unknown = true;
        assert!(
            manager
                .dispatch(fixture.owner().await, duplicate)
                .await
                .is_err()
        );
        fixture.store.flush().await.unwrap();
        drop(manager);
        let recovered = fixture.manager(fixture.store.snapshot().await.unwrap().launches);
        let snapshot = recovered.refresh(first_id).await.unwrap();
        assert_eq!(snapshot.liveness, Liveness::Unverified);
        assert_eq!(fixture.run_count(), 1);
        assert!(
            recovered
                .dispatch(fixture.owner().await, fixture.spec())
                .await
                .is_err()
        );
        assert_eq!(fixture.run_count(), 1);
        let mut retry = fixture.spec();
        retry.confirm_unknown = true;
        let retry_output = output_path(&retry);
        let retry_id = recovered
            .dispatch(fixture.owner().await, retry)
            .await
            .unwrap();
        wait_until(|| fixture.run_count() == 2).await;
        fs::write(fixture.directory.0.join("release-run"), "release").unwrap();
        wait_until(|| {
            [&first_output, &retry_output]
                .iter()
                .all(|path| fs::read_to_string(path).is_ok_and(|text| text.contains("after close")))
        })
        .await;
        let deadline = Instant::now() + Duration::from_secs(5);
        let exited = loop {
            let snapshot = recovered.refresh(retry_id).await.unwrap();
            if snapshot.liveness == Liveness::Exited(0) {
                break snapshot;
            }
            assert!(Instant::now() < deadline);
            smol::Timer::after(Duration::from_millis(10)).await;
        };
        assert!(!exited.reported_state.terminal());
        assert_eq!(fixture.run_count(), 2);
        assert_eq!(
            fs::read(fixture.folder.join("PRD.md")).unwrap(),
            original_prd
        );
        let durable = fixture.store.snapshot().await.unwrap();
        assert_eq!(
            durable
                .launches
                .iter()
                .find(|launch| launch.id == retry_id)
                .unwrap()
                .process_exit,
            Some(0)
        );
        assert_eq!(
            durable
                .launches
                .iter()
                .find(|launch| launch.id == first_id)
                .unwrap()
                .process_exit,
            None
        );
    });
}

#[test]
fn recovered_correlation_is_saved_without_losing_exit_metadata_or_spawning() {
    smol::block_on(async {
        let fixture = Fixture::new("exit 0", "exit 0").await;
        let spec = fixture.spec();
        fs::create_dir_all(&spec.log_dir).unwrap();
        let events = spec.data_root.join("events/nested");
        fs::create_dir_all(&events).unwrap();
        let path = events.join("run.jsonl");
        let records = [
            (
                "run-started",
                json!({"userStoryId": 42, "logPath": spec.log_dir.join("run.log")}),
            ),
            (
                "attempt-ready",
                json!({"userStoryId": 42, "attemptId": "attempt-a", "targetBranch": "main", "baseSha": "0123456789abcdef0123456789abcdef01234567", "mode": "normal"}),
            ),
            ("run-completed", json!({"text": "completed"})),
        ];
        let text: String = records.into_iter().map(|(kind, payload)| {
            format!("{}\n", json!({"schemaVersion": 1, "timestampUtc": "2026-09-15T12:01:00Z", "eventType": kind, "payload": payload}))
        }).collect();
        fs::write(&path, text).unwrap();
        let owner = fixture.owner().await;
        let launch = FactoryLaunch {
            id: spec.id,
            project_id: spec.project_id,
            factory_thread_id: owner,
            descriptor: serde_json::to_value(&spec).unwrap(),
            event_path: None,
            work_item_id: None,
            attempt_id: None,
            process_exit: Some(0),
            launch_error: None,
            created_at: "2026-09-15T12:00:00Z".into(),
        };
        fixture.store.save_launch(launch.clone()).await.unwrap();
        let manager = fixture.manager(vec![launch]);
        let snapshot = manager.refresh(spec.id).await.unwrap();
        assert_eq!(snapshot.reported_state, ReportedState::Completed);
        let durable = fixture.store.snapshot().await.unwrap().launches.remove(0);
        assert_eq!(durable.factory_thread_id, owner);
        assert_eq!(durable.event_path, Some(path));
        assert_eq!(durable.work_item_id, Some(42));
        assert_eq!(durable.attempt_id.as_deref(), Some("attempt-a"));
        assert_eq!(durable.process_exit, Some(0));
        assert!(!fixture.directory.0.join("invocations").exists());
    });
}
