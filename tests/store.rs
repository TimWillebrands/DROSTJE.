use anyhow::Result;
use drostje::{
    model::{
        FactoryLaunch, FactorySourceKind, FactoryThreadConfig, ImageAttachment, ModelKey, Project,
        ProjectDefaults, PullRequestLink, PullRequestState, QueuedMessage, RemoteLocation, Thread,
        ThreadKind, Worktree,
    },
    store::{Store, StoreError},
};
use rusqlite::Connection;
use serde_json::json;
use std::{
    fs::{self, TryLockError},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("drostje-store-{}", Uuid::new_v4()));
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
        kind: ThreadKind::Conversation,
        factory: None,
        title: "Conversation".into(),
        harness: "omp".into(),
        session_ref: None,
        model: None,
        effort: None,
        permission_mode: None,
        worktree_id: None,
        remote: None,
        execution_started: false,
        pull_request: None,
        draft: "Unsent draft\n日本語 é".into(),
        attachments: Vec::new(),
        submission: None,
        queued: Vec::new(),
        archived: false,
        created_at: "2026-09-15T12:00:00Z".into(),
        updated_at: "2026-09-15T12:00:00Z".into(),
    }
}

fn factory_thread(project: &Project) -> Thread {
    Thread {
        kind: ThreadKind::Factory,
        factory: Some(FactoryThreadConfig {
            source_kind: FactorySourceKind::WorkItem,
            source: String::new(),
            harness: "opencode".into(),
            model: String::new(),
            thought_level: String::new(),
            target_branch: "main".into(),
        }),
        title: "Factory".into(),
        harness: String::new(),
        draft: String::new(),
        ..conversation(project)
    }
}

fn worktree(project: &Project, creator: &Thread, codename: &str) -> Worktree {
    Worktree {
        id: Uuid::new_v4(),
        project_id: project.id,
        path: project.root.join("..").join("worktrees").join(codename),
        codename: codename.into(),
        branch: format!("drostje/{codename}"),
        base: "main".into(),
        named: false,
        created_by: creator.id,
        created_at: "2026-09-22T12:00:00Z".into(),
        removed_at: None,
    }
}

fn launch(project: &Project, owner: &Thread) -> FactoryLaunch {
    FactoryLaunch {
        id: Uuid::new_v4(),
        project_id: project.id,
        factory_thread_id: owner.id,
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

// Dropping the last handle is nonblocking: the worker still owns the SQLite connection and
// the instance lock until it drains. Opening the database file directly while that WAL
// connection closes deadlocks inside SQLite's unix VFS (the inode mutex against the global
// VFS mutex), so wait for the worker to release the instance lock before touching the file.
fn wait_for_release(directory: &Path) {
    let lock = fs::File::options()
        .read(true)
        .write(true)
        .open(directory.join("instance.lock"))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match lock.try_lock() {
            Ok(()) => return,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(error) => panic!("Storage worker did not release the instance lock: {error}"),
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
        thread.queued.push(QueuedMessage {
            id: Uuid::new_v4(),
            text: "Waiting for the turn\nsecond line".into(),
            attachments: thread.attachments.clone(),
        });
        store.save_thread(thread.clone()).await.unwrap();
        let owner = factory_thread(&project);
        store.save_thread(owner.clone()).await.unwrap();
        let mut launch = launch(&project, &owner);
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
        assert_eq!(snapshot.threads.len(), 2);
        assert_eq!(snapshot.launches.len(), 1);
        let saved_project = &snapshot.projects[0];
        let saved_thread = snapshot
            .threads
            .iter()
            .find(|saved| saved.id == thread.id)
            .unwrap();
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
        assert_eq!(saved_thread.queued, thread.queued);
        assert_eq!(saved_launch.project_id, saved_project.id);
        assert_eq!(saved_launch.factory_thread_id, owner.id);
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
    });
}

#[test]
fn remote_location_survives_reopen_and_cannot_move_after_execution_starts() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let project = project(directory.0.join("checkout"));
        let mut thread = conversation(&project);
        let location = RemoteLocation {
            host: "linux-worker".into(),
            project: "backend".into(),
        };
        let store = reopen(&directory.0);
        store.save_project(project.clone()).await.unwrap();
        store.save_thread(thread.clone()).await.unwrap();
        thread.remote = Some(location.clone());
        store.save_thread(thread.clone()).await.unwrap();
        thread.execution_started = true;
        store.save_thread(thread.clone()).await.unwrap();
        store.flush().await.unwrap();
        drop(store);

        let store = reopen(&directory.0);
        let saved = store.snapshot().await.unwrap().threads.remove(0);
        assert_eq!(saved.remote, Some(location.clone()));
        assert!(saved.execution_started);
        let mut moved = saved.clone();
        moved.remote = None;
        assert!(store.save_thread(moved).await.is_err());
        let mut reset = saved.clone();
        reset.execution_started = false;
        assert!(store.save_thread(reset).await.is_err());
        let mut factory = factory_thread(&project);
        factory.remote = Some(location.clone());
        assert!(store.save_thread(factory).await.is_err());
        let mut edited = saved;
        edited.draft = "Follow-up remains editable".into();
        store.save_thread(edited.clone()).await.unwrap();
        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(snapshot.threads.len(), 1);
        assert_eq!(snapshot.threads[0].remote, Some(location));
        assert_eq!(snapshot.threads[0].draft, edited.draft);
    });
}

#[test]
fn legacy_sessions_keep_their_local_execution_location_after_upgrade() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let project = project(directory.0.join("checkout"));
        let mut started = conversation(&project);
        started.session_ref = Some("existing-provider-session".into());
        let empty = conversation(&project);
        let store = reopen(&directory.0);
        store.save_project(project).await.unwrap();
        store.save_thread(started.clone()).await.unwrap();
        store.save_thread(empty.clone()).await.unwrap();
        store.flush().await.unwrap();
        drop(store);
        wait_for_release(&directory.0);
        let database = Connection::open(directory.0.join("drostje.sqlite3")).unwrap();
        database
            .execute_batch(
                "ALTER TABLE threads DROP COLUMN remote_json;
             ALTER TABLE threads DROP COLUMN execution_started;",
            )
            .unwrap();
        database.pragma_update(None, "user_version", 7).unwrap();
        drop(database);

        let store = reopen(&directory.0);
        let snapshot = store.snapshot().await.unwrap();
        let mut existing = snapshot
            .threads
            .iter()
            .find(|t| t.id == started.id)
            .unwrap()
            .clone();
        let mut draft = snapshot
            .threads
            .iter()
            .find(|t| t.id == empty.id)
            .unwrap()
            .clone();
        assert!(existing.execution_started);
        assert_eq!(existing.session_ref, started.session_ref);
        let location = Some(RemoteLocation {
            host: "worker".into(),
            project: "repo".into(),
        });
        existing.remote = location.clone();
        assert!(store.save_thread(existing).await.is_err());
        draft.remote = location.clone();
        store.save_thread(draft).await.unwrap();
        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(
            snapshot
                .threads
                .iter()
                .find(|t| t.id == empty.id)
                .unwrap()
                .remote,
            location
        );
        assert_eq!(
            snapshot
                .threads
                .iter()
                .find(|t| t.id == started.id)
                .unwrap()
                .remote,
            None
        );
    });
}

const SCHEMA_V2: &str = "
CREATE TABLE settings(key TEXT PRIMARY KEY, value_json TEXT NOT NULL);
CREATE TABLE projects(
    id TEXT PRIMARY KEY, name TEXT NOT NULL, root TEXT UNIQUE NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0, defaults_json TEXT NOT NULL
);
CREATE TABLE threads(
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    title TEXT NOT NULL, harness TEXT NOT NULL, session_ref TEXT, model_json TEXT,
    draft TEXT NOT NULL DEFAULT '', attachments_json TEXT NOT NULL DEFAULT '[]',
    submission_json TEXT, archived INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'conversation', factory_config_json TEXT
);
CREATE TABLE factory_launches(
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    factory_thread_id TEXT NOT NULL REFERENCES threads(id), descriptor_json TEXT NOT NULL,
    event_path TEXT UNIQUE, work_item_id INTEGER, attempt_id TEXT,
    process_exit INTEGER, launch_error TEXT, created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX factory_launches_factory_thread ON factory_launches(factory_thread_id);";

#[test]
fn version_2_database_upgrades_in_place_and_persists_effort() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let path = directory.0.join("drostje.sqlite3");
        let project = project(directory.0.join("checkout"));
        let mut thread = conversation(&project);
        thread.session_ref = Some("saved".into());
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA_V2).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id, name, root, defaults_json) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    project.id.to_string(),
                    project.name,
                    project.root.to_string_lossy().into_owned(),
                    serde_json::to_string(&project.defaults).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads(id, project_id, title, harness, session_ref, draft, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    thread.id.to_string(),
                    project.id.to_string(),
                    thread.title,
                    thread.harness,
                    thread.session_ref,
                    thread.draft,
                    thread.created_at,
                    thread.updated_at,
                ],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        drop(connection);

        let store = reopen(&directory.0);
        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(snapshot.threads.len(), 1);
        assert_eq!(snapshot.threads[0].id, thread.id);
        assert_eq!(snapshot.threads[0].session_ref.as_deref(), Some("saved"));
        assert_eq!(snapshot.threads[0].effort, None);
        assert_eq!(snapshot.threads[0].permission_mode, None);
        let mut upgraded = snapshot.threads[0].clone();
        upgraded.effort = Some("low".into());
        upgraded.permission_mode = Some("plan".into());
        store.save_thread(upgraded).await.unwrap();
        store.flush().await.unwrap();
        drop(store);

        let reopened = reopen(&directory.0);
        let threads = reopened.snapshot().await.unwrap().threads;
        assert_eq!(threads[0].effort.as_deref(), Some("low"));
        assert_eq!(threads[0].permission_mode.as_deref(), Some("plan"));
        drop(reopened);
    });
}

#[test]
fn version_3_database_upgrades_in_place_and_persists_permission_mode() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let path = directory.0.join("drostje.sqlite3");
        let project = project(directory.0.join("checkout"));
        let thread = conversation(&project);
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA_V2).unwrap();
        connection
            .execute_batch("ALTER TABLE threads ADD COLUMN effort TEXT")
            .unwrap();
        connection
            .execute(
                "INSERT INTO projects(id, name, root, defaults_json) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    project.id.to_string(),
                    project.name,
                    project.root.to_string_lossy().into_owned(),
                    serde_json::to_string(&project.defaults).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads(id, project_id, title, harness, draft, created_at, updated_at, effort)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'high')",
                rusqlite::params![
                    thread.id.to_string(),
                    project.id.to_string(),
                    thread.title,
                    thread.harness,
                    thread.draft,
                    thread.created_at,
                    thread.updated_at,
                ],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 3).unwrap();
        drop(connection);

        let store = reopen(&directory.0);
        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(snapshot.threads[0].effort.as_deref(), Some("high"));
        assert_eq!(snapshot.threads[0].permission_mode, None);
        let mut upgraded = snapshot.threads[0].clone();
        upgraded.permission_mode = Some("acceptEdits".into());
        store.save_thread(upgraded).await.unwrap();
        store.flush().await.unwrap();
        drop(store);

        let reopened = reopen(&directory.0);
        let threads = reopened.snapshot().await.unwrap().threads;
        assert_eq!(threads[0].effort.as_deref(), Some("high"));
        assert_eq!(threads[0].permission_mode.as_deref(), Some("acceptEdits"));
        drop(reopened);
    });
}

#[test]
fn version_4_database_upgrades_in_place_and_persists_queued_messages() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let path = directory.0.join("drostje.sqlite3");
        let project = project(directory.0.join("checkout"));
        let thread = conversation(&project);
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA_V2).unwrap();
        connection
            .execute_batch(
                "ALTER TABLE threads ADD COLUMN effort TEXT;
                 ALTER TABLE threads ADD COLUMN permission_mode TEXT",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO projects(id, name, root, defaults_json) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    project.id.to_string(),
                    project.name,
                    project.root.to_string_lossy().into_owned(),
                    serde_json::to_string(&project.defaults).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads(id, project_id, title, harness, draft, created_at, updated_at, permission_mode)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'plan')",
                rusqlite::params![
                    thread.id.to_string(),
                    project.id.to_string(),
                    thread.title,
                    thread.harness,
                    thread.draft,
                    thread.created_at,
                    thread.updated_at,
                ],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 4).unwrap();
        drop(connection);

        let store = reopen(&directory.0);
        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(snapshot.threads[0].permission_mode.as_deref(), Some("plan"));
        assert!(snapshot.threads[0].queued.is_empty());
        let mut upgraded = snapshot.threads[0].clone();
        let queued = QueuedMessage {
            id: Uuid::new_v4(),
            text: "After this one".into(),
            attachments: Vec::new(),
        };
        upgraded.queued.push(queued.clone());
        store.save_thread(upgraded).await.unwrap();
        store.flush().await.unwrap();
        drop(store);

        let reopened = reopen(&directory.0);
        let threads = reopened.snapshot().await.unwrap().threads;
        assert_eq!(threads[0].permission_mode.as_deref(), Some("plan"));
        assert_eq!(threads[0].queued, vec![queued]);
        drop(reopened);
    });
}

#[test]
fn version_5_database_upgrades_in_place_and_persists_worktrees() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let path = directory.0.join("drostje.sqlite3");
        let project = project(directory.0.join("checkout"));
        let thread = conversation(&project);
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA_V2).unwrap();
        connection
            .execute_batch(
                "ALTER TABLE threads ADD COLUMN effort TEXT;
                 ALTER TABLE threads ADD COLUMN permission_mode TEXT;
                 ALTER TABLE threads ADD COLUMN queued_json TEXT NOT NULL DEFAULT '[]'",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO projects(id, name, root, defaults_json) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    project.id.to_string(),
                    project.name,
                    project.root.to_string_lossy().into_owned(),
                    serde_json::to_string(&project.defaults).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads(id, project_id, title, harness, draft, created_at, updated_at, queued_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '[]')",
                rusqlite::params![
                    thread.id.to_string(),
                    project.id.to_string(),
                    thread.title,
                    thread.harness,
                    thread.draft,
                    thread.created_at,
                    thread.updated_at,
                ],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 5).unwrap();
        drop(connection);

        let store = reopen(&directory.0);
        let snapshot = store.snapshot().await.unwrap();
        assert!(snapshot.worktrees.is_empty());
        assert_eq!(snapshot.threads[0].worktree_id, None);
        let mut isolated = conversation(&project);
        let mut worktree = worktree(&project, &isolated, "amber-heron");
        isolated.worktree_id = Some(worktree.id);
        store.save_worktree(worktree.clone()).await.unwrap();
        store.save_thread(isolated.clone()).await.unwrap();
        worktree.branch = "drostje/fix-login".into();
        worktree.named = true;
        store.save_worktree(worktree.clone()).await.unwrap();
        store.flush().await.unwrap();
        drop(store);

        let reopened = reopen(&directory.0);
        let snapshot = reopened.snapshot().await.unwrap();
        assert_eq!(snapshot.worktrees, vec![worktree.clone()]);
        let saved = snapshot
            .threads
            .iter()
            .find(|saved| saved.id == isolated.id)
            .unwrap();
        assert_eq!(saved.worktree_id, Some(worktree.id));
        assert_eq!(
            snapshot
                .threads
                .iter()
                .find(|saved| saved.id == thread.id)
                .unwrap()
                .worktree_id,
            None
        );
        drop(reopened);
    });
}

#[test]
fn version_6_database_upgrades_in_place_and_persists_pull_request_links() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let path = directory.0.join("drostje.sqlite3");
        let project = project(directory.0.join("checkout"));
        let mut thread = conversation(&project);
        let worktree = worktree(&project, &thread, "amber-heron");
        thread.worktree_id = Some(worktree.id);
        // Reconstruct version 6 by removing all columns introduced since that schema.
        let store = reopen(&directory.0);
        store.save_project(project.clone()).await.unwrap();
        store.save_worktree(worktree.clone()).await.unwrap();
        store.save_thread(thread.clone()).await.unwrap();
        store.flush().await.unwrap();
        drop(store);
        wait_for_release(&directory.0);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "ALTER TABLE threads DROP COLUMN pull_request_json;
                 ALTER TABLE threads DROP COLUMN remote_json;
                 ALTER TABLE threads DROP COLUMN execution_started;",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 6).unwrap();
        drop(connection);

        let store = reopen(&directory.0);
        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(snapshot.threads[0].pull_request, None);
        thread.pull_request = Some(PullRequestLink {
            id: 3170,
            url: "https://dev.azure.com/o/p/_git/r/pullrequest/3170".into(),
            title: "Fix upload".into(),
            state: PullRequestState::Unknown("notSet".into()),
            work_items: vec![2596],
            archived_on_close: true,
        });
        store.save_thread(thread.clone()).await.unwrap();
        // A project-root Conversation has no branch of its own to link.
        let mut rooted = conversation(&project);
        rooted.pull_request = thread.pull_request.clone();
        assert!(store.save_thread(rooted).await.is_err());
        store.flush().await.unwrap();
        drop(store);

        let reopened = reopen(&directory.0);
        let snapshot = reopened.snapshot().await.unwrap();
        assert_eq!(snapshot.threads[0].pull_request, thread.pull_request);
        drop(reopened);
    });
}

#[test]
fn incompatible_schema_is_refused_without_changing_database_bytes() {
    for version in [1, i64::from(i32::MAX)] {
        let directory = TempDirectory::new();
        let path = directory.0.join("drostje.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE incompatible_data(payload TEXT NOT NULL);
            INSERT INTO incompatible_data VALUES ('must survive an incompatible application');",
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        drop(connection);
        let before = fs::read(&path).unwrap();
        let error = open_error(&directory.0);
        assert!(matches!(
            error.downcast_ref::<StoreError>(),
            Some(StoreError::UnsupportedSchema { found, .. }) if *found == version
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn corrupt_database_is_refused_without_replacement() {
    let directory = TempDirectory::new();
    let path = directory.0.join("drostje.sqlite3");
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
    let path = directory.0.join("drostje.sqlite3");
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
        let blocker = Connection::open(directory.0.join("drostje.sqlite3")).unwrap();
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
        assert!(
            store
                .save_launch(launch(&project, &factory_thread(&project)))
                .await
                .is_err()
        );
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
        let owner = factory_thread(&first);
        let other_owner = factory_thread(&first);
        let foreign_owner = factory_thread(&second);
        for factory in [&owner, &other_owner, &foreign_owner] {
            store.save_thread(factory.clone()).await.unwrap();
        }
        for invalid in [
            launch(&second, &owner),
            launch(&first, &thread),
            launch(&first, &factory_thread(&first)),
        ] {
            assert!(matches!(
                store
                    .save_launch(invalid)
                    .await
                    .unwrap_err()
                    .downcast_ref::<StoreError>(),
                Some(StoreError::InvalidData(_))
            ));
        }
        let mut correct_launch = launch(&first, &owner);
        store.save_launch(correct_launch.clone()).await.unwrap();
        assert!(store.save_launch(launch(&first, &owner)).await.is_err());
        thread.project_id = second.id;
        assert!(store.save_thread(thread.clone()).await.is_err());
        thread.project_id = first.id;
        let mut changed_kind = factory_thread(&first);
        changed_kind.id = thread.id;
        assert!(store.save_thread(changed_kind).await.is_err());
        let mut changed_kind = conversation(&first);
        changed_kind.id = other_owner.id;
        assert!(store.save_thread(changed_kind).await.is_err());
        let mut moved_owner = owner.clone();
        moved_owner.project_id = second.id;
        assert!(store.save_thread(moved_owner).await.is_err());
        for (project_id, factory_thread_id) in
            [(second.id, foreign_owner.id), (first.id, other_owner.id)]
        {
            let mut reassigned = correct_launch.clone();
            reassigned.project_id = project_id;
            reassigned.factory_thread_id = factory_thread_id;
            assert!(store.save_launch(reassigned).await.is_err());
        }
        correct_launch.process_exit = Some(7);
        correct_launch.launch_error = Some("Observed process failure".into());
        store.save_launch(correct_launch.clone()).await.unwrap();

        // Worktrees belong to one project; threads never cross into another project's tree
        // and never move between trees once created.
        let mut orphan = worktree(&first, &thread, "orphan");
        orphan.project_id = Uuid::new_v4();
        assert!(store.save_worktree(orphan).await.is_err());
        let foreign_tree = worktree(&second, &foreign_owner, "foreign-tree");
        store.save_worktree(foreign_tree.clone()).await.unwrap();
        let mut crossing = conversation(&first);
        crossing.worktree_id = Some(foreign_tree.id);
        assert!(store.save_thread(crossing).await.is_err());
        let mut missing = conversation(&first);
        missing.worktree_id = Some(Uuid::new_v4());
        assert!(store.save_thread(missing).await.is_err());
        let mut isolated = conversation(&first);
        let tree = worktree(&first, &isolated, "amber-heron");
        isolated.worktree_id = Some(tree.id);
        store.save_worktree(tree.clone()).await.unwrap();
        store.save_thread(isolated.clone()).await.unwrap();
        let mut moved = isolated.clone();
        moved.worktree_id = None;
        assert!(store.save_thread(moved).await.is_err());
        let mut rooted = thread.clone();
        rooted.worktree_id = Some(tree.id);
        assert!(store.save_thread(rooted).await.is_err());
        let mut relocated = tree.clone();
        relocated.path = first.root.join("elsewhere");
        assert!(store.save_worktree(relocated).await.is_err());
        let mut renamed_dir = tree.clone();
        renamed_dir.codename = "other".into();
        assert!(store.save_worktree(renamed_dir).await.is_err());
        let mut duplicate_path = worktree(&first, &thread, "duplicate");
        duplicate_path.path = tree.path.clone();
        assert!(store.save_worktree(duplicate_path).await.is_err());
        let mut relative = worktree(&first, &thread, "relative");
        relative.path = PathBuf::from("relative/path");
        assert!(store.save_worktree(relative).await.is_err());
        store.writable().await.unwrap();
        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(
            snapshot
                .threads
                .iter()
                .find(|saved| saved.id == thread.id)
                .unwrap()
                .project_id,
            first.id
        );
        assert_eq!(snapshot.launches, vec![correct_launch]);
        let mut worktrees = snapshot.worktrees.clone();
        worktrees.sort_by(|a, b| a.codename.cmp(&b.codename));
        assert_eq!(worktrees, vec![tree.clone(), foreign_tree]);
        assert_eq!(
            snapshot
                .threads
                .iter()
                .find(|saved| saved.id == isolated.id)
                .unwrap()
                .worktree_id,
            Some(tree.id)
        );
    });
}

fn assert_recovery_unchanged(directory: &Path) {
    let path = directory.join("drostje.sqlite3");
    let before = fs::read(&path).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let error = loop {
        let error = open_error(directory);
        if matches!(
            error.downcast_ref::<StoreError>(),
            Some(StoreError::InstanceInUse(_))
        ) && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        break error;
    };
    assert!(
        matches!(
            error.downcast_ref::<StoreError>(),
            Some(StoreError::RecoveryRequired { .. })
        ),
        "{error:#}"
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn incomplete_factory_config_survives_restart_without_interactive_state() {
    smol::block_on(async {
        let directory = TempDirectory::new();
        let store = Store::open(directory.0.clone()).unwrap();
        let project = project(directory.0.join("checkout"));
        store.save_project(project.clone()).await.unwrap();
        let mut factory = factory_thread(&project);
        factory.factory.as_mut().unwrap().harness = "omp".into();
        factory.factory.as_mut().unwrap().source = "not yet a work-item ID 日本語".into();
        factory.factory.as_mut().unwrap().target_branch.clear();
        store.save_thread(factory.clone()).await.unwrap();
        drop(store);
        let reopened = reopen(&directory.0);
        let snapshot = reopened.snapshot().await.unwrap();
        assert_eq!(
            serde_json::to_value(&snapshot.threads[0]).unwrap(),
            serde_json::to_value(&factory).unwrap()
        );
        assert!(snapshot.launches.is_empty());
        let mut missing_config = factory_thread(&project);
        missing_config.factory = None;
        assert!(reopened.save_thread(missing_config).await.is_err());
        let mut conversation = conversation(&project);
        conversation.factory = factory.factory.clone();
        assert!(reopened.save_thread(conversation).await.is_err());
        let mut interactive_factory = factory.clone();
        interactive_factory.draft = "Must not lose this input silently".into();
        assert!(reopened.save_thread(interactive_factory).await.is_err());
        reopened.writable().await.unwrap();
        assert_eq!(
            serde_json::to_value(&reopened.snapshot().await.unwrap().threads[0]).unwrap(),
            serde_json::to_value(&factory).unwrap()
        );
    });
}

#[test]
fn malformed_v2_shapes_and_ownership_are_refused_without_repairing_data() {
    smol::block_on(async {
        for corruption in [
            "UPDATE threads SET kind = 'unknown'",
            "UPDATE threads SET factory_config_json = '{' WHERE kind = 'factory'",
            "UPDATE threads SET factory_config_json = NULL WHERE kind = 'factory'",
            "UPDATE threads SET draft = 'hidden draft' WHERE kind = 'factory'",
            "UPDATE factory_launches SET factory_thread_id = (SELECT id FROM threads WHERE kind = 'conversation')",
            "UPDATE factory_launches SET project_id = (SELECT id FROM projects WHERE name = 'Other project')",
            "DROP INDEX factory_launches_factory_thread",
            "ALTER TABLE factory_launches RENAME COLUMN factory_thread_id TO thread_id",
            "PRAGMA foreign_keys = OFF; UPDATE threads SET worktree_id = '00000000-0000-0000-0000-000000000009'",
            "INSERT INTO worktrees(id, project_id, path, codename, branch, base, created_by, created_at)
             SELECT '00000000-0000-0000-0000-000000000001', id, '/worktrees/amber-heron', 'amber-heron',
             'drostje/amber-heron', 'main', '00000000-0000-0000-0000-000000000002', '2026-09-22T00:00:00Z'
             FROM projects WHERE name = 'Other project';
             UPDATE threads SET worktree_id = '00000000-0000-0000-0000-000000000001' WHERE kind = 'conversation'",
            "INSERT INTO worktrees(id, project_id, path, codename, branch, base, created_by, created_at)
             SELECT '00000000-0000-0000-0000-000000000001', id, 'relative/amber-heron', 'amber-heron',
             'drostje/amber-heron', 'main', '00000000-0000-0000-0000-000000000002', '2026-09-22T00:00:00Z'
             FROM projects WHERE name = 'Other project'",
            "PRAGMA foreign_keys = OFF; DROP TABLE worktrees",
        ] {
            let directory = TempDirectory::new();
            let store = Store::open(directory.0.clone()).unwrap();
            let mut other = project(directory.0.join("other"));
            other.name = "Other project".into();
            let project = project(directory.0.join("checkout"));
            let conversation = conversation(&project);
            let owner = factory_thread(&project);
            let unlaunched = factory_thread(&project);
            store.save_project(project.clone()).await.unwrap();
            store.save_project(other).await.unwrap();
            for thread in [&conversation, &owner, &unlaunched] {
                store.save_thread(thread.clone()).await.unwrap();
            }
            store.save_launch(launch(&project, &owner)).await.unwrap();
            store.flush().await.unwrap();
            let connection = Connection::open(directory.0.join("drostje.sqlite3")).unwrap();
            connection.execute_batch(corruption).unwrap();
            connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                .unwrap();
            drop(connection);
            drop(store);
            assert_recovery_unchanged(&directory.0);
        }
    });
}
