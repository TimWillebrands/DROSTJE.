use anyhow::Result;
use drost::{
    model::{FactoryLaunch, ImageAttachment, ModelKey, Project, ProjectDefaults, Thread},
    store::{Store, StoreError},
};
use rusqlite::Connection;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("drost-store-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn project(root: PathBuf) -> Project {
    Project {
        id: Uuid::new_v4(),
        name: "Working tree".into(),
        root,
        archived: false,
        defaults: ProjectDefaults::default(),
    }
}

fn conversation(project: &Project) -> Thread {
    Thread {
        id: Uuid::new_v4(),
        project_id: project.id,
        title: "Conversation".into(),
        harness: "omp".into(),
        session_ref: None,
        model: None,
        draft: "Unsent draft\n日本語 é".into(),
        attachments: Vec::new(),
        submission: None,
        archived: false,
        created_at: "2026-09-15T12:00:00Z".into(),
        updated_at: "2026-09-15T12:00:00Z".into(),
    }
}

fn launch(project: &Project, thread: &Thread) -> FactoryLaunch {
    FactoryLaunch {
        id: Uuid::new_v4(),
        project_id: project.id,
        thread_id: Some(thread.id),
        descriptor: json!({"source": {"azure_work_item": 42}, "target_repository": project.root}),
        event_path: None,
        work_item_id: Some(42),
        attempt_id: Some("attempt/42/a".into()),
        process_exit: None,
        launch_error: None,
        created_at: "2026-09-15T12:01:00Z".into(),
    }
}

// Last-handle drop intentionally does not block a GPUI thread. Reopening waits only
// for the previous background owner to drain/close, never retries recovery errors.
fn reopen(path: &Path) -> Store {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Store::open(path.to_path_buf()) {
            Ok(store) => return store,
            Err(error)
                if matches!(
                    error.downcast_ref::<StoreError>(),
                    Some(StoreError::InstanceInUse(_))
                ) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5))
            }
            Err(error) => panic!("Cannot reopen workspace: {error:#}"),
        }
    }
}

fn open_error(path: &Path) -> anyhow::Error {
    match Store::open(path.to_path_buf()) {
        Ok(_) => panic!("Expected opening this workspace to be refused"),
        Err(error) => error,
    }
}

#[test]
fn reopen_retains_archived_history_drafts_and_launch_associations() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let root = directory.0.join("checkout");
        fs::create_dir(&root).unwrap();
        let checkout_file = root.join("notes.txt");
        let attachment_file = directory.0.join("attachment.png");
        let session_file = directory.0.join("session.jsonl");
        let events_file = directory.0.join("events.jsonl");
        for file in [
            &checkout_file,
            &attachment_file,
            &session_file,
            &events_file,
        ] {
            fs::write(file, b"retained original bytes").unwrap();
        }
        let store = Store::open(directory.0.clone()).unwrap();
        let mut project = project(root);
        project.defaults.interactive_model = Some(ModelKey {
            provider: "configured-provider".into(),
            id: "configured-model".into(),
        });
        project.defaults.factory_target_branch = Some("main".into());
        store.save_project(project.clone()).await.unwrap();
        let mut thread = conversation(&project);
        thread.session_ref = Some(json!({"id": "saved-session", "file": session_file}).to_string());
        thread.attachments.push(ImageAttachment {
            id: Uuid::new_v4(),
            path: attachment_file.clone(),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            original: None,
        });
        thread.submission = Some(
            json!({"request_id": Uuid::new_v4(), "text": "not automatically resent", "state": "pending"}),
        );
        store.save_thread(thread.clone()).await.unwrap();
        let mut launch = launch(&project, &thread);
        launch.event_path = Some(events_file.clone());
        store.save_launch(launch.clone()).await.unwrap();
        store
            .set_setting("selected_thread".into(), json!(thread.id))
            .await
            .unwrap();
        store
            .set_setting(
                "layout".into(),
                json!({"version": 1, "panels": ["projects", "conversation"]}),
            )
            .await
            .unwrap();
        project.name = "Renamed project".into();
        project.archived = true;
        thread.title = "Renamed conversation".into();
        thread.archived = true;
        store.save_project(project.clone()).await.unwrap();
        store.save_thread(thread.clone()).await.unwrap();
        store.flush().await.unwrap();
        drop(store);

        let reopened = reopen(&directory.0);
        let snapshot = reopened.snapshot().await.unwrap();
        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(snapshot.threads.len(), 1);
        assert_eq!(snapshot.launches.len(), 1);
        let saved_project = &snapshot.projects[0];
        let saved_thread = &snapshot.threads[0];
        let saved_launch = &snapshot.launches[0];
        assert!(saved_project.archived && saved_thread.archived);
        assert_eq!(saved_project.name, "Renamed project");
        assert_eq!(
            saved_project.defaults.interactive_model,
            project.defaults.interactive_model
        );
        assert_eq!(saved_thread.title, "Renamed conversation");
        assert_eq!(saved_thread.project_id, saved_project.id);
        assert_eq!(saved_thread.draft, thread.draft);
        assert_eq!(saved_thread.session_ref, thread.session_ref);
        assert_eq!(saved_thread.attachments[0].path, attachment_file);
        assert_eq!(saved_thread.submission, thread.submission);
        assert_eq!(saved_launch.project_id, saved_project.id);
        assert_eq!(saved_launch.thread_id, Some(saved_thread.id));
        assert_eq!(saved_launch.descriptor, launch.descriptor);
        assert_eq!(saved_launch.event_path, Some(events_file.clone()));
        assert_eq!(saved_launch.work_item_id, Some(42));
        assert_eq!(saved_launch.attempt_id.as_deref(), Some("attempt/42/a"));
        assert_eq!(snapshot.settings["selected_thread"], json!(thread.id));
        assert_eq!(snapshot.settings["layout"]["version"], 1);
        for file in [
            &checkout_file,
            &attachment_file,
            &session_file,
            &events_file,
        ] {
            assert_eq!(fs::read(file).unwrap(), b"retained original bytes");
        }
        let database = Connection::open(directory.0.join("drost.sqlite3")).unwrap();
        assert_eq!(
            database
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    });
}

#[test]
fn newer_schema_is_refused_without_changing_database_bytes() {
    let directory = TempDirectory::new();
    let path = directory.0.join("drost.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE future_data(payload TEXT NOT NULL);
        INSERT INTO future_data VALUES ('must survive an older application'); PRAGMA user_version = 2;").unwrap();
    drop(connection);
    let before = fs::read(&path).unwrap();
    let error = open_error(&directory.0);
    assert!(matches!(
        error.downcast_ref::<StoreError>(),
        Some(StoreError::UnsupportedSchema { found: 2, .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn corrupt_database_is_refused_without_replacement() {
    let directory = TempDirectory::new();
    let path = directory.0.join("drost.sqlite3");
    let original =
        b"This is not SQLite, and it must not be replaced with an empty workspace.\0\xff";
    fs::write(&path, original).unwrap();
    let error = open_error(&directory.0);
    assert!(matches!(
        error.downcast_ref::<StoreError>(),
        Some(StoreError::RecoveryRequired { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn initialization_refuses_nonempty_unversioned_schema() {
    let directory = TempDirectory::new();
    let path = directory.0.join("drost.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE valuable_data(payload TEXT); INSERT INTO valuable_data VALUES ('retain me');").unwrap();
    drop(connection);
    let before = fs::read(&path).unwrap();
    let error = open_error(&directory.0);
    assert!(matches!(
        error.downcast_ref::<StoreError>(),
        Some(StoreError::RecoveryRequired { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn instance_lock_survives_clones_and_directory_aliases() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let data = directory.0.join("data");
        let alias = directory.0.join("alias");
        let store = Store::open(data.clone()).unwrap();
        let clone = store.clone();
        drop(store);
        assert!(matches!(
            open_error(&data).downcast_ref::<StoreError>(),
            Some(StoreError::InstanceInUse(_))
        ));
        std::os::unix::fs::symlink(&data, &alias).unwrap();
        assert!(matches!(
            open_error(&alias).downcast_ref::<StoreError>(),
            Some(StoreError::InstanceInUse(_))
        ));
        clone.flush().await.unwrap();
        drop(clone);
        let reopened = reopen(&alias);
        reopened.writable().await.unwrap();
    });
}

async fn launch_side_effect(store: &Store, marker: &Path) -> Result<()> {
    store.writable().await?;
    let status = Command::new("/usr/bin/touch").arg(marker).status()?;
    anyhow::ensure!(status.success(), "fixture side effect failed");
    Ok(())
}

#[test]
fn write_failure_is_sticky_preserves_durable_draft_and_blocks_side_effects() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let store = Store::open(directory.0.clone()).unwrap();
        let project = project(directory.0.join("checkout"));
        let mut thread = conversation(&project);
        store.save_project(project.clone()).await.unwrap();
        store.save_thread(thread.clone()).await.unwrap();
        let durable_draft = thread.draft.clone();
        let blocker = Connection::open(directory.0.join("drost.sqlite3")).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        thread.draft = "Newer text still held in the editor after the failed save".into();
        assert!(store.save_thread(thread.clone()).await.is_err());
        assert!(store.failures.try_recv().is_ok());
        blocker.execute_batch("ROLLBACK").unwrap();
        drop(blocker);

        let marker = directory.0.join("spawned-untracked-work");
        let error = launch_side_effect(&store, &marker).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<StoreError>(),
            Some(StoreError::WriteDisabled(_))
        ));
        assert!(!marker.exists());
        assert!(store.save_thread(thread.clone()).await.is_err());
        assert!(store.save_launch(launch(&project, &thread)).await.is_err());
        assert!(store.flush().await.is_err());
        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(snapshot.threads[0].draft, durable_draft);
        assert!(snapshot.launches.is_empty());
        // Receiving the first failure must not reset the write gate or trigger retries.
        assert!(store.failures.try_recv().is_err());
        drop(store);
        let reopened = reopen(&directory.0);
        reopened.save_thread(thread.clone()).await.unwrap();
        assert_eq!(
            reopened.snapshot().await.unwrap().threads[0].draft,
            thread.draft
        );
    });
}

#[test]
fn invalid_associations_are_rejected_without_poisoning_healthy_storage() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let store = Store::open(directory.0.clone()).unwrap();
        let first = project(directory.0.join("first"));
        let second = project(directory.0.join("second"));
        let mut thread = conversation(&first);
        let error = store.save_thread(thread.clone()).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<StoreError>(),
            Some(StoreError::InvalidData(_))
        ));
        store.save_project(first.clone()).await.unwrap();
        store.save_project(second.clone()).await.unwrap();
        store.save_thread(thread.clone()).await.unwrap();
        let incorrect_launch = launch(&second, &thread);
        assert!(store.save_launch(incorrect_launch).await.is_err());
        let correct_launch = launch(&first, &thread);
        store.save_launch(correct_launch.clone()).await.unwrap();
        thread.project_id = second.id;
        assert!(store.save_thread(thread).await.is_err());
        let mut reassigned_launch = correct_launch;
        reassigned_launch.project_id = second.id;
        reassigned_launch.thread_id = None;
        assert!(store.save_launch(reassigned_launch).await.is_err());
        store.writable().await.unwrap();
        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(snapshot.threads[0].project_id, first.id);
        assert_eq!(snapshot.launches.len(), 1);
        assert_eq!(snapshot.launches[0].project_id, first.id);
        assert_eq!(snapshot.launches[0].thread_id, Some(snapshot.threads[0].id));
    });
}
