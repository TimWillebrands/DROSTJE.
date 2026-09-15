use crate::model::{FactoryLaunch, Project, Thread, WorkspaceSnapshot};
use anyhow::{Context, Result, anyhow, bail};
use async_channel::{Receiver, Sender};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params, types::Type,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = "
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
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE factory_launches(
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    thread_id TEXT REFERENCES threads(id), descriptor_json TEXT NOT NULL,
    event_path TEXT UNIQUE, work_item_id INTEGER, attempt_id TEXT,
    process_exit INTEGER, launch_error TEXT, created_at TEXT NOT NULL
);";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Another Drost instance owns {0}; close it before reopening this workspace")]
    InstanceInUse(PathBuf),
    #[error(
        "Database schema {found} is unsupported (this Drost supports {supported}); the database was not changed"
    )]
    UnsupportedSchema { found: i64, supported: i64 },
    #[error("Cannot recover workspace at {path}: {reason}. The database was not replaced or reset")]
    RecoveryRequired { path: PathBuf, reason: String },
    #[error(
        "Storage is disabled after a persistence failure: {0}. Keep unsaved drafts and restart after fixing storage"
    )]
    WriteDisabled(String),
    #[error("Invalid workspace data: {0}")]
    InvalidData(String),
    #[error("The storage worker is unavailable; keep unsaved drafts and do not launch work")]
    Unavailable,
}

type Reply<T> = Sender<Result<T>>;

enum Command {
    Snapshot(Reply<WorkspaceSnapshot>),
    SaveProject(Project, Reply<()>),
    SaveThread(Thread, Reply<()>),
    SaveLaunch(FactoryLaunch, Reply<()>),
    SetSetting(String, Value, Reply<()>),
    Writable(Reply<()>),
    Flush(Reply<()>),
}

/// A bounded command handle; the SQLite connection and instance lock belong to its worker.
/// Dropping the last clone is nonblocking. The worker drains accepted commands before
/// releasing the lock, so use `flush` after scheduling final draft/layout writes on quit.
#[derive(Clone)]
pub struct Store {
    commands: Sender<Command>,
    /// The first persistence failure. Clones share this single-consumer notification;
    /// `writable` remains the authoritative sticky health check for every caller.
    pub failures: Receiver<String>,
}

impl Store {
    /// Blocking startup only: no handle is returned until recovery checks and schema
    /// initialization succeed. Existing corrupt/newer databases are checked read-only.
    pub fn open(data_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&data_dir)
            .with_context(|| format!("Create application data directory {}", data_dir.display()))?;
        let data_dir = fs::canonicalize(&data_dir).context("Resolve application data directory")?;
        let lock = acquire_lock(&data_dir)?;
        let database = data_dir.join("drost.sqlite3");
        let (commands, receiver) = async_channel::bounded(64);
        let (failure_sender, failures) = async_channel::bounded(1);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("drost-storage".into())
            .spawn(move || {
                let _lock = lock;
                match open_database(&database) {
                    Ok(connection) => {
                        if ready_sender.send(Ok(())).is_ok() {
                            serve(connection, receiver, failure_sender);
                        }
                    }
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                    }
                }
            })
            .context("Start storage worker")?;
        ready_receiver
            .recv()
            .map_err(|_| StoreError::Unavailable)??;
        Ok(Self { commands, failures })
    }

    pub async fn snapshot(&self) -> Result<WorkspaceSnapshot> {
        self.request(Command::Snapshot).await
    }

    pub async fn save_project(&self, project: Project) -> Result<()> {
        self.request(|reply| Command::SaveProject(project, reply))
            .await
    }

    pub async fn save_thread(&self, thread: Thread) -> Result<()> {
        self.request(|reply| Command::SaveThread(thread, reply))
            .await
    }

    pub async fn save_launch(&self, launch: FactoryLaunch) -> Result<()> {
        self.request(|reply| Command::SaveLaunch(launch, reply))
            .await
    }

    pub async fn set_setting(&self, key: String, value: Value) -> Result<()> {
        self.request(|reply| Command::SetSetting(key, value, reply))
            .await
    }

    /// An ordered health/writer-lock barrier, not a promise about future disk space.
    /// Submission/dispatch must additionally await its durable record before spawning.
    pub async fn writable(&self) -> Result<()> {
        self.request(Command::Writable).await
    }

    /// Wait for preceding accepted writes and checkpoint the fully synchronous WAL.
    /// This never discards unsaved UI state; callers retain it if persistence fails.
    pub async fn flush(&self) -> Result<()> {
        self.request(Command::Flush).await
    }

    async fn request<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T> {
        let (reply, response) = async_channel::bounded(1);
        self.commands
            .send(command(reply))
            .await
            .map_err(|_| StoreError::Unavailable)?;
        response.recv().await.map_err(|_| StoreError::Unavailable)?
    }
}

fn acquire_lock(data_dir: &Path) -> Result<File> {
    let path = data_dir.join("instance.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .with_context(|| format!("Open instance lock {}", path.display()))?;
    loop {
        // flock is tied to this open file description and released by File::drop.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(lock);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Err(StoreError::InstanceInUse(data_dir.to_path_buf()).into());
        }
        return Err(error).with_context(|| format!("Lock {}", path.display()));
    }
}

fn open_database(path: &Path) -> Result<Connection> {
    let version = if path.try_exists().context("Inspect database path")? {
        inspect_existing(path).map_err(|error| {
            if matches!(
                error.downcast_ref::<StoreError>(),
                Some(StoreError::UnsupportedSchema { .. })
            ) {
                error
            } else {
                StoreError::RecoveryRequired {
                    path: path.to_path_buf(),
                    reason: format!("{error:#}"),
                }
                .into()
            }
        })?
    } else {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("Create database {}", path.display()))?;
        0
    };
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Open writable database {}", path.display()))?;
    connection.busy_timeout(Duration::from_millis(500))?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON; PRAGMA trusted_schema = OFF;
        PRAGMA synchronous = FULL; PRAGMA fullfsync = ON; PRAGMA checkpoint_fullfsync = ON;",
    )?;
    let journal: String =
        connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !journal.eq_ignore_ascii_case("wal") {
        bail!("SQLite could not enable WAL journaling: {journal}");
    }
    if version == 0 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Never merge a partial/unversioned schema or use IF NOT EXISTS to mask one.
        ensure_empty(&transaction)?;
        transaction.execute_batch(SCHEMA)?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction
            .commit()
            .context("Commit initial workspace schema")?;
    }
    Ok(connection)
}

fn inspect_existing(path: &Path) -> Result<i64> {
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("Read existing database without modifying it")?;
    connection.busy_timeout(Duration::from_millis(500))?;
    let transaction = connection.transaction()?;
    let version: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != 0 && version != SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema {
            found: version,
            supported: SCHEMA_VERSION,
        }
        .into());
    }
    let integrity: String = transaction.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if integrity != "ok" {
        bail!("SQLite integrity check failed: {integrity}");
    }
    if version == 0 {
        ensure_empty(&transaction)?;
    } else {
        validate_schema(&transaction)?;
        let mut statement = transaction.prepare("PRAGMA foreign_key_check")?;
        if statement.query([])?.next()?.is_some() {
            bail!("Stored records have broken foreign-key associations");
        }
        drop(statement);
        read_snapshot(&transaction).context("Decode existing workspace records")?;
    }
    transaction.commit()?;
    Ok(version)
}

fn ensure_empty(connection: &Connection) -> Result<()> {
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if count != 0 {
        bail!("Unversioned database is not empty; refusing to initialize over existing data");
    }
    Ok(())
}

// Validate the public v1 shape, including nullable/defaulted columns and the constraints
// on which updates rely. Whitespace or index names in equivalent CREATE SQL do not matter.
fn validate_schema(connection: &Connection) -> Result<()> {
    type Column<'a> = (&'a str, &'a str, bool, Option<&'a str>, bool);
    let tables: &[(&str, &[Column<'_>])] = &[
        (
            "settings",
            &[
                ("key", "TEXT", false, None, true),
                ("value_json", "TEXT", true, None, false),
            ],
        ),
        (
            "projects",
            &[
                ("id", "TEXT", false, None, true),
                ("name", "TEXT", true, None, false),
                ("root", "TEXT", true, None, false),
                ("archived", "INTEGER", true, Some("0"), false),
                ("defaults_json", "TEXT", true, None, false),
            ],
        ),
        (
            "threads",
            &[
                ("id", "TEXT", false, None, true),
                ("project_id", "TEXT", true, None, false),
                ("title", "TEXT", true, None, false),
                ("harness", "TEXT", true, None, false),
                ("session_ref", "TEXT", false, None, false),
                ("model_json", "TEXT", false, None, false),
                ("draft", "TEXT", true, Some("''"), false),
                ("attachments_json", "TEXT", true, Some("'[]'"), false),
                ("submission_json", "TEXT", false, None, false),
                ("archived", "INTEGER", true, Some("0"), false),
                ("created_at", "TEXT", true, None, false),
                ("updated_at", "TEXT", true, None, false),
            ],
        ),
        (
            "factory_launches",
            &[
                ("id", "TEXT", false, None, true),
                ("project_id", "TEXT", true, None, false),
                ("thread_id", "TEXT", false, None, false),
                ("descriptor_json", "TEXT", true, None, false),
                ("event_path", "TEXT", false, None, false),
                ("work_item_id", "INTEGER", false, None, false),
                ("attempt_id", "TEXT", false, None, false),
                ("process_exit", "INTEGER", false, None, false),
                ("launch_error", "TEXT", false, None, false),
                ("created_at", "TEXT", true, None, false),
            ],
        ),
    ];
    let mut statement = connection.prepare(
        "SELECT type, name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' AND type != 'index'",
    )?;
    let objects = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if objects.len() != tables.len()
        || objects.iter().any(|(kind, name)| {
            kind != "table" || !tables.iter().any(|(table, _)| *table == name.as_str())
        })
    {
        bail!("Database objects do not match workspace schema v1");
    }
    for (table, expected) in tables {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if columns.len() != expected.len()
            || columns.iter().zip(*expected).any(|(actual, expected)| {
                actual.0 != expected.0
                    || !actual.1.eq_ignore_ascii_case(expected.1)
                    || actual.2 != expected.2
                    || actual.3.as_deref().map(|s| s.trim_matches(['(', ')', ' '])) != expected.3
                    || actual.4 != expected.4
            })
        {
            bail!("Table {table} does not match workspace schema v1");
        }
        let key = if *table == "settings" { "key" } else { "id" };
        require_unique(connection, table, key)?;
    }
    require_unique(connection, "projects", "root")?;
    require_unique(connection, "factory_launches", "event_path")?;
    for (table, expected) in [
        ("settings", Vec::new()),
        ("projects", Vec::new()),
        ("threads", vec![("project_id", "projects")]),
        (
            "factory_launches",
            vec![("project_id", "projects"), ("thread_id", "threads")],
        ),
    ] {
        let mut statement = connection.prepare(&format!("PRAGMA foreign_key_list({table})"))?;
        let keys = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(1)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if keys.len() != expected.len()
            || keys
                .iter()
                .any(|(from, target, to, update, delete, sequence)| {
                    !expected.iter().any(|(column, parent)| {
                        from.as_str() == *column && target.as_str() == *parent
                    }) || to != "id"
                        || update != "NO ACTION"
                        || delete != "NO ACTION"
                        || *sequence != 0
                })
        {
            bail!("Table {table} has incompatible foreign keys");
        }
    }
    Ok(())
}

fn require_unique(connection: &Connection, table: &str, column: &str) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA index_list({table})"))?;
    let indexes = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, bool>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (index, unique, partial) in indexes {
        if !unique || partial {
            continue;
        }
        let mut statement =
            connection.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?;
        let columns = statement
            .query_map([index], |row| row.get::<_, Option<String>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if columns.len() == 1 && columns[0].as_deref() == Some(column) {
            return Ok(());
        }
    }
    bail!("Table {table} is missing its unique {column} constraint")
}

fn serve(mut connection: Connection, commands: Receiver<Command>, failures: Sender<String>) {
    let mut failure = None;
    while let Ok(command) = commands.recv_blocking() {
        match command {
            Command::Snapshot(reply) => {
                let result = (|| {
                    let transaction = connection.transaction()?;
                    let snapshot = read_snapshot(&transaction)?;
                    transaction.commit()?;
                    Ok(snapshot)
                })();
                if let Err(error) = &result {
                    remember_failure(&mut failure, &failures, error);
                }
                let _ = reply.try_send(result);
            }
            Command::SaveProject(project, reply) => {
                complete_write(
                    &mut connection,
                    &mut failure,
                    &failures,
                    reply,
                    |connection| save_project(connection, project),
                );
            }
            Command::SaveThread(thread, reply) => {
                complete_write(
                    &mut connection,
                    &mut failure,
                    &failures,
                    reply,
                    |connection| save_thread(connection, thread),
                );
            }
            Command::SaveLaunch(launch, reply) => {
                complete_write(
                    &mut connection,
                    &mut failure,
                    &failures,
                    reply,
                    |connection| save_launch(connection, launch),
                );
            }
            Command::SetSetting(key, value, reply) => {
                complete_write(
                    &mut connection,
                    &mut failure,
                    &failures,
                    reply,
                    |connection| {
                        let value = encode(&value)?;
                        let transaction =
                            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                        transaction.execute(
                            "INSERT INTO settings(key, value_json) VALUES (?1, ?2)
                        ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
                            params![key, value],
                        )?;
                        transaction.commit().context("Persist setting")
                    },
                );
            }
            Command::Writable(reply) => {
                complete_write(
                    &mut connection,
                    &mut failure,
                    &failures,
                    reply,
                    |connection| {
                        connection
                            .transaction_with_behavior(TransactionBehavior::Immediate)?
                            .rollback()
                            .context("Check storage writer availability")
                    },
                );
            }
            Command::Flush(reply) => {
                complete_write(
                    &mut connection,
                    &mut failure,
                    &failures,
                    reply,
                    |connection| {
                        let (busy, written, checkpointed): (i64, i64, i64) =
                            connection.query_row("PRAGMA wal_checkpoint(FULL)", [], |row| {
                                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                            })?;
                        if busy != 0 || written != checkpointed {
                            bail!("Workspace checkpoint is blocked by another database connection");
                        }
                        Ok(())
                    },
                );
            }
        }
    }
    // Each successful commit is durable independently of shutdown/checkpoint success.
    // Connection drops before the worker's instance lock, including after queued writes.
}

fn complete_write(
    connection: &mut Connection,
    failure: &mut Option<String>,
    failures: &Sender<String>,
    reply: Reply<()>,
    operation: impl FnOnce(&mut Connection) -> Result<()>,
) {
    let result = if let Some(reason) = failure.as_ref() {
        Err(StoreError::WriteDisabled(reason.clone()).into())
    } else {
        operation(connection)
    };
    if let Err(error) = &result {
        if !matches!(
            error.downcast_ref::<StoreError>(),
            Some(StoreError::InvalidData(_))
        ) {
            remember_failure(failure, failures, error);
        }
    }
    let _ = reply.try_send(result);
}

fn remember_failure(
    failure: &mut Option<String>,
    failures: &Sender<String>,
    error: &anyhow::Error,
) {
    if failure.is_none() {
        let message = format!("{error:#}");
        let _ = failures.try_send(message.clone());
        *failure = Some(message);
    }
}

fn invalid(message: impl Into<String>) -> anyhow::Error {
    StoreError::InvalidData(message.into()).into()
}

fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|error| invalid(format!("Cannot encode JSON: {error}")))
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| invalid(format!("Path is not valid UTF-8: {}", path.display())))
}

fn require_project(connection: &Connection, id: Uuid) -> Result<()> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [id.to_string()],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(invalid(format!("Project {id} does not exist")));
    }
    Ok(())
}

fn save_project(connection: &mut Connection, project: Project) -> Result<()> {
    if !project.root.is_absolute() {
        return Err(invalid(
            "Project root must be an absolute working-tree path",
        ));
    }
    let root = path_text(&project.root)?;
    let defaults = encode(&project.defaults)?;
    let id = project.id.to_string();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let duplicate: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE root = ?1 AND id != ?2)",
        params![root, id],
        |row| row.get(0),
    )?;
    if duplicate {
        return Err(invalid(format!(
            "A project already owns working-tree root {root}"
        )));
    }
    transaction.execute(
        "INSERT INTO projects(id, name, root, archived, defaults_json) VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(id) DO UPDATE SET name = excluded.name, root = excluded.root,
        archived = excluded.archived, defaults_json = excluded.defaults_json",
        params![id, project.name, root, project.archived, defaults],
    )?;
    transaction.commit().context("Persist project")
}

fn save_thread(connection: &mut Connection, thread: Thread) -> Result<()> {
    let attachments = encode(&thread.attachments)?;
    let model = thread.model.as_ref().map(encode).transpose()?;
    let submission = thread.submission.as_ref().map(encode).transpose()?;
    let id = thread.id.to_string();
    let project_id = thread.project_id.to_string();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_project(&transaction, thread.project_id)?;
    let previous: Option<String> = transaction
        .query_row(
            "SELECT project_id FROM threads WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )
        .optional()?;
    if previous
        .as_ref()
        .is_some_and(|parent| parent != &project_id)
    {
        return Err(invalid(
            "A conversation cannot be reassigned to another project",
        ));
    }
    transaction.execute("INSERT INTO threads(id, project_id, title, harness, session_ref, model_json, draft,
        attachments_json, submission_json, archived, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(id) DO UPDATE SET title = excluded.title, harness = excluded.harness,
        session_ref = excluded.session_ref, model_json = excluded.model_json, draft = excluded.draft,
        attachments_json = excluded.attachments_json, submission_json = excluded.submission_json,
        archived = excluded.archived, updated_at = excluded.updated_at",
        params![id, project_id, thread.title, thread.harness, thread.session_ref, model, thread.draft,
            attachments, submission, thread.archived, thread.created_at, thread.updated_at])?;
    transaction
        .commit()
        .context("Persist conversation and draft")
}

fn save_launch(connection: &mut Connection, launch: FactoryLaunch) -> Result<()> {
    let descriptor = encode(&launch.descriptor)?;
    let event_path = launch.event_path.as_deref().map(path_text).transpose()?;
    let id = launch.id.to_string();
    let project_id = launch.project_id.to_string();
    let thread_id = launch.thread_id.map(|id| id.to_string());
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_project(&transaction, launch.project_id)?;
    if let Some(thread_id) = &thread_id {
        let parent: Option<String> = transaction
            .query_row(
                "SELECT project_id FROM threads WHERE id = ?1",
                [thread_id],
                |row| row.get(0),
            )
            .optional()?;
        if parent.as_ref() != Some(&project_id) {
            return Err(invalid(
                "Factory launch conversation must exist in the launch's project",
            ));
        }
    }
    let previous: Option<(String, Option<String>)> = transaction
        .query_row(
            "SELECT project_id, thread_id FROM factory_launches WHERE id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if previous
        .as_ref()
        .is_some_and(|(project, thread)| project != &project_id || thread != &thread_id)
    {
        return Err(invalid(
            "A Factory launch cannot be reassigned to a different project or conversation",
        ));
    }
    if let Some(event_path) = event_path {
        let duplicate: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM factory_launches WHERE event_path = ?1 AND id != ?2)",
            params![event_path, id],
            |row| row.get(0),
        )?;
        if duplicate {
            return Err(invalid(
                "An event path is already associated with another Factory launch",
            ));
        }
    }
    transaction.execute("INSERT INTO factory_launches(id, project_id, thread_id, descriptor_json,
        event_path, work_item_id, attempt_id, process_exit, launch_error, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        ON CONFLICT(id) DO UPDATE SET descriptor_json = excluded.descriptor_json,
        event_path = excluded.event_path, work_item_id = excluded.work_item_id, attempt_id = excluded.attempt_id,
        process_exit = excluded.process_exit, launch_error = excluded.launch_error",
        params![id, project_id, thread_id, descriptor, event_path, launch.work_item_id,
            launch.attempt_id, launch.process_exit, launch.launch_error, launch.created_at])?;
    transaction.commit().context("Persist Factory launch")
}

fn decode<T: DeserializeOwned>(row: &Row<'_>, column: usize) -> rusqlite::Result<T> {
    let json: String = row.get(column)?;
    serde_json::from_str(&json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
    })
}

fn decode_optional<T: DeserializeOwned>(
    row: &Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<T>> {
    let json: Option<String> = row.get(column)?;
    json.map(|json| {
        serde_json::from_str(&json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
        })
    })
    .transpose()
}

fn uuid(row: &Row<'_>, column: usize) -> rusqlite::Result<Uuid> {
    let text: String = row.get(column)?;
    Uuid::parse_str(&text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
    })
}

fn optional_uuid(row: &Row<'_>, column: usize) -> rusqlite::Result<Option<Uuid>> {
    let text: Option<String> = row.get(column)?;
    text.map(|text| {
        Uuid::parse_str(&text).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
        })
    })
    .transpose()
}

fn archived(row: &Row<'_>, column: usize) -> rusqlite::Result<bool> {
    match row.get::<_, i64>(column)? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(rusqlite::Error::FromSqlConversionFailure(
            column,
            Type::Integer,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid archive flag: {value}"),
            )),
        )),
    }
}

fn read_snapshot(connection: &Connection) -> Result<WorkspaceSnapshot> {
    let projects = connection
        .prepare("SELECT id, name, root, archived, defaults_json FROM projects ORDER BY name, id")?
        .query_map([], |row| {
            Ok(Project {
                id: uuid(row, 0)?,
                name: row.get(1)?,
                root: PathBuf::from(row.get::<_, String>(2)?),
                archived: archived(row, 3)?,
                defaults: decode(row, 4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let threads = connection.prepare("SELECT id, project_id, title, harness, session_ref, model_json, draft,
        attachments_json, submission_json, archived, created_at, updated_at FROM threads ORDER BY created_at, id")?
        .query_map([], |row| Ok(Thread {
            id: uuid(row, 0)?, project_id: uuid(row, 1)?, title: row.get(2)?, harness: row.get(3)?,
            session_ref: row.get(4)?, model: decode_optional(row, 5)?, draft: row.get(6)?,
            attachments: decode(row, 7)?, submission: decode_optional(row, 8)?, archived: archived(row, 9)?,
            created_at: row.get(10)?, updated_at: row.get(11)?,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let launches = connection.prepare("SELECT id, project_id, thread_id, descriptor_json, event_path, work_item_id,
        attempt_id, process_exit, launch_error, created_at FROM factory_launches ORDER BY created_at, id")?
        .query_map([], |row| Ok(FactoryLaunch {
            id: uuid(row, 0)?, project_id: uuid(row, 1)?, thread_id: optional_uuid(row, 2)?,
            descriptor: decode(row, 3)?, event_path: row.get::<_, Option<String>>(4)?.map(PathBuf::from),
            work_item_id: row.get(5)?, attempt_id: row.get(6)?, process_exit: row.get(7)?,
            launch_error: row.get(8)?, created_at: row.get(9)?,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let settings = connection
        .prepare("SELECT key, value_json FROM settings ORDER BY key")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, decode::<Value>(row, 1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let project_ids: HashSet<_> = projects.iter().map(|project| project.id).collect();
    let thread_projects: HashMap<_, _> = threads
        .iter()
        .map(|thread| (thread.id, thread.project_id))
        .collect();
    if projects.iter().any(|project| !project.root.is_absolute()) {
        bail!("Stored project root is not absolute");
    }
    if threads
        .iter()
        .any(|thread| !project_ids.contains(&thread.project_id))
        || launches.iter().any(|launch| {
            !project_ids.contains(&launch.project_id)
                || launch
                    .thread_id
                    .is_some_and(|thread| thread_projects.get(&thread) != Some(&launch.project_id))
        })
    {
        return Err(anyhow!(
            "Stored project/conversation/Factory associations are inconsistent"
        ));
    }
    Ok(WorkspaceSnapshot {
        projects,
        threads,
        launches,
        settings,
    })
}
