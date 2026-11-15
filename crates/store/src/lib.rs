use anyhow::{Context, Result, bail};
use async_channel::{Receiver, Sender};
use drostje_model::{
    FactoryLaunch, FactoryThreadConfig, Project, Thread, ThreadKind, WorkspaceSnapshot, Worktree,
    WorktreeId,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params, types::Type,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions, TryLockError},
    io::ErrorKind,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 8;
const UPGRADABLE_VERSIONS: [i64; 6] = [2, 3, 4, 5, 6, 7];
macro_rules! worktrees_table {
    () => {
        "CREATE TABLE worktrees(
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    path TEXT UNIQUE NOT NULL, codename TEXT NOT NULL, branch TEXT NOT NULL, base TEXT NOT NULL,
    named INTEGER NOT NULL DEFAULT 0, created_by TEXT NOT NULL,
    created_at TEXT NOT NULL, removed_at TEXT
);"
    };
}
const SCHEMA: &str = concat!(
    "
CREATE TABLE settings(key TEXT PRIMARY KEY, value_json TEXT NOT NULL);
CREATE TABLE projects(
    id TEXT PRIMARY KEY, name TEXT NOT NULL, root TEXT UNIQUE NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0, defaults_json TEXT NOT NULL
);
",
    worktrees_table!(),
    "
CREATE TABLE threads(
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    title TEXT NOT NULL, harness TEXT NOT NULL, session_ref TEXT, model_json TEXT,
    draft TEXT NOT NULL DEFAULT '', attachments_json TEXT NOT NULL DEFAULT '[]',
    submission_json TEXT, archived INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'conversation', factory_config_json TEXT, effort TEXT,
    permission_mode TEXT, queued_json TEXT NOT NULL DEFAULT '[]',
    worktree_id TEXT REFERENCES worktrees(id), pull_request_json TEXT,
    remote_json TEXT, execution_started INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE factory_launches(
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    factory_thread_id TEXT NOT NULL REFERENCES threads(id), descriptor_json TEXT NOT NULL,
    event_path TEXT UNIQUE, work_item_id INTEGER, attempt_id TEXT,
    process_exit INTEGER, launch_error TEXT, created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX factory_launches_factory_thread ON factory_launches(factory_thread_id);"
);

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Another DROSTJE. instance owns {0}; close it before reopening this workspace")]
    InstanceInUse(PathBuf),
    #[error(
        "Database schema {found} is unsupported (this DROSTJE. supports {supported}); the database was not changed"
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
    SaveThread(Box<Thread>, Reply<()>),
    SaveWorktree(Worktree, Reply<()>),
    SaveLaunch(FactoryLaunch, Reply<()>),
    SetSetting(String, Value, Reply<()>),
    SetEncodedSetting(String, String, Reply<()>),
    Writable(Reply<()>),
    Flush(Reply<()>),
}

#[derive(Clone)]
pub struct Store {
    commands: Sender<Command>,
    pub failures: Receiver<String>,
}

impl Store {
    pub fn open(data_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&data_dir)
            .with_context(|| format!("Create application data directory {}", data_dir.display()))?;
        let data_dir = fs::canonicalize(&data_dir).context("Resolve application data directory")?;
        let lock = acquire_lock(&data_dir)?;
        let database = data_dir.join("drostje.sqlite3");
        let (commands, receiver) = async_channel::bounded(64);
        let (failure_sender, failures) = async_channel::bounded(1);
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("drostje-storage".into())
            .spawn(move || {
                let _lock = lock;
                match open_database(&database) {
                    Ok((connection, decoded)) => {
                        if ready_sender.send(Ok(())).is_ok() {
                            serve(connection, decoded, receiver, failure_sender);
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
        self.request(|reply| Command::SaveThread(Box::new(thread), reply))
            .await
    }

    pub async fn save_worktree(&self, worktree: Worktree) -> Result<()> {
        self.request(|reply| Command::SaveWorktree(worktree, reply))
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

    pub async fn set_setting_encoded<T: Serialize + ?Sized>(
        &self,
        key: String,
        value: &T,
    ) -> Result<()> {
        let json = encode(value)?;
        self.request(|reply| Command::SetEncodedSetting(key, json, reply))
            .await
    }

    pub async fn writable(&self) -> Result<()> {
        self.request(Command::Writable).await
    }

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
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(TryLockError::WouldBlock) => {
                return Err(StoreError::InstanceInUse(data_dir.to_path_buf()).into());
            }
            Err(TryLockError::Error(error)) if error.kind() == ErrorKind::Interrupted => continue,
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("Lock {}", path.display()));
            }
        }
    }
}

fn open_database(path: &Path) -> Result<(Connection, Option<WorkspaceSnapshot>)> {
    let (version, mut decoded) = if path.try_exists().context("Inspect database path")? {
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
        (0, None)
    };
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("Open writable database {}", path.display()))?;
    connection.busy_timeout(Duration::from_millis(500))?;
    connection.set_prepared_statement_cache_capacity(32);
    connection.execute_batch(
        "PRAGMA foreign_keys = ON; PRAGMA trusted_schema = OFF;
        PRAGMA synchronous = FULL; PRAGMA fullfsync = ON; PRAGMA checkpoint_fullfsync = ON;",
    )?;
    let prepare = (|| -> Result<()> {
        if version == 0 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure_empty(&transaction)?;
            transaction.execute_batch(SCHEMA)?;
            validate_schema(&transaction, SCHEMA_VERSION)?;
            decoded = Some(read_snapshot(&transaction)?);
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            transaction
                .commit()
                .context("Commit initial workspace schema")?;
        } else if UPGRADABLE_VERSIONS.contains(&version) {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if version < 3 {
                transaction.execute_batch("ALTER TABLE threads ADD COLUMN effort TEXT")?;
            }
            if version < 4 {
                transaction.execute_batch("ALTER TABLE threads ADD COLUMN permission_mode TEXT")?;
            }
            if version < 5 {
                transaction.execute_batch(
                    "ALTER TABLE threads ADD COLUMN queued_json TEXT NOT NULL DEFAULT '[]'",
                )?;
            }
            if version < 6 {
                transaction.execute_batch(worktrees_table!())?;
                transaction.execute_batch(
                    "ALTER TABLE threads ADD COLUMN worktree_id TEXT REFERENCES worktrees(id)",
                )?;
            }
            if version < 7 {
                transaction
                    .execute_batch("ALTER TABLE threads ADD COLUMN pull_request_json TEXT")?;
            }
            if version < 8 {
                transaction.execute_batch(
                    "ALTER TABLE threads ADD COLUMN remote_json TEXT;
                     ALTER TABLE threads ADD COLUMN execution_started INTEGER NOT NULL DEFAULT 0;
                     UPDATE threads SET execution_started = 1 WHERE session_ref IS NOT NULL OR submission_json IS NOT NULL;",
                )?;
            }
            validate_schema(&transaction, SCHEMA_VERSION)?;
            decoded = Some(read_snapshot(&transaction)?);
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            transaction.commit().with_context(|| {
                format!("Commit workspace schema upgrade v{version} to v{SCHEMA_VERSION}")
            })?;
        }
        Ok(())
    })();
    prepare.map_err(|error| StoreError::RecoveryRequired {
        path: path.to_path_buf(),
        reason: format!("{error:#}"),
    })?;
    let journal: String =
        connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !journal.eq_ignore_ascii_case("wal") {
        bail!("SQLite could not enable WAL journaling: {journal}");
    }
    Ok((connection, decoded))
}

fn inspect_existing(path: &Path) -> Result<(i64, Option<WorkspaceSnapshot>)> {
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("Read existing database without modifying it")?;
    connection.busy_timeout(Duration::from_millis(500))?;
    let transaction = connection.transaction()?;
    let version: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != 0 && version != SCHEMA_VERSION && !UPGRADABLE_VERSIONS.contains(&version) {
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
    let mut decoded = None;
    if version == 0 {
        ensure_empty(&transaction)?;
    } else {
        validate_schema(&transaction, version)?;
        validate_foreign_keys(&transaction)?;
        if version == SCHEMA_VERSION {
            decoded =
                Some(read_snapshot(&transaction).context("Decode existing workspace records")?);
        }
    }
    transaction.commit()?;
    Ok((version, decoded))
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

fn validate_schema(connection: &Connection, version: i64) -> Result<()> {
    type Column<'a> = (&'a str, &'a str, bool, Option<&'a str>, bool);
    let all_tables: &[(&str, &[Column<'_>])] = &[
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
                ("kind", "TEXT", true, Some("'conversation'"), false),
                ("factory_config_json", "TEXT", false, None, false),
                ("effort", "TEXT", false, None, false),
                ("permission_mode", "TEXT", false, None, false),
                ("queued_json", "TEXT", true, Some("'[]'"), false),
                ("worktree_id", "TEXT", false, None, false),
                ("pull_request_json", "TEXT", false, None, false),
                ("remote_json", "TEXT", false, None, false),
                ("execution_started", "INTEGER", true, Some("0"), false),
            ],
        ),
        (
            "worktrees",
            &[
                ("id", "TEXT", false, None, true),
                ("project_id", "TEXT", true, None, false),
                ("path", "TEXT", true, None, false),
                ("codename", "TEXT", true, None, false),
                ("branch", "TEXT", true, None, false),
                ("base", "TEXT", true, None, false),
                ("named", "INTEGER", true, Some("0"), false),
                ("created_by", "TEXT", true, None, false),
                ("created_at", "TEXT", true, None, false),
                ("removed_at", "TEXT", false, None, false),
            ],
        ),
        (
            "factory_launches",
            &[
                ("id", "TEXT", false, None, true),
                ("project_id", "TEXT", true, None, false),
                ("factory_thread_id", "TEXT", true, None, false),
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
    let tables: Vec<_> = all_tables
        .iter()
        .filter(|(table, _)| version >= 6 || *table != "worktrees")
        .collect();
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
        bail!("Database objects do not match workspace schema v{version}");
    }
    for (table, expected) in tables {
        let missing = match (*table, version) {
            ("threads", 2) => 7,
            ("threads", 3) => 6,
            ("threads", 4) => 5,
            ("threads", 5) => 4,
            ("threads", 6) => 3,
            ("threads", 7) => 2,
            _ => 0,
        };
        let expected: &[_] = &expected[..expected.len() - missing];
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
            || columns.iter().zip(expected).any(|(actual, expected)| {
                actual.0 != expected.0
                    || !actual.1.eq_ignore_ascii_case(expected.1)
                    || actual.2 != expected.2
                    || actual.3.as_deref().map(|s| s.trim_matches(['(', ')', ' '])) != expected.3
                    || actual.4 != expected.4
            })
        {
            bail!("Table {table} does not match workspace schema v{version}");
        }
        let key = if *table == "settings" { "key" } else { "id" };
        require_unique(connection, table, key)?;
    }
    require_unique(connection, "projects", "root")?;
    require_unique(connection, "factory_launches", "event_path")?;
    require_unique(connection, "factory_launches", "factory_thread_id")?;
    if version >= 6 {
        require_unique(connection, "worktrees", "path")?;
    }
    let mut thread_keys = vec![("project_id", "projects")];
    if version >= 6 {
        thread_keys.push(("worktree_id", "worktrees"));
    }
    let mut foreign_keys = vec![
        ("settings", Vec::new()),
        ("projects", Vec::new()),
        ("threads", thread_keys),
        (
            "factory_launches",
            vec![("project_id", "projects"), ("factory_thread_id", "threads")],
        ),
    ];
    if version >= 6 {
        foreign_keys.push(("worktrees", vec![("project_id", "projects")]));
    }
    for (table, expected) in foreign_keys {
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

fn validate_foreign_keys(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    if statement.query([])?.next()?.is_some() {
        bail!("Stored records have broken foreign-key associations");
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

fn serve(
    mut connection: Connection,
    mut decoded: Option<WorkspaceSnapshot>,
    commands: Receiver<Command>,
    failures: Sender<String>,
) {
    let mut failure = None;
    while let Ok(command) = commands.recv_blocking() {
        let decoded = decoded.take();
        match command {
            Command::Snapshot(reply) => {
                if let Some(snapshot) = decoded {
                    let _ = reply.try_send(Ok(snapshot));
                    continue;
                }
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
                    |connection| save_thread(connection, *thread),
                );
            }
            Command::SaveWorktree(worktree, reply) => {
                complete_write(
                    &mut connection,
                    &mut failure,
                    &failures,
                    reply,
                    |connection| save_worktree(connection, worktree),
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
                    |connection| save_setting(connection, &key, &encode(&value)?),
                );
            }
            Command::SetEncodedSetting(key, json, reply) => {
                complete_write(
                    &mut connection,
                    &mut failure,
                    &failures,
                    reply,
                    |connection| save_setting(connection, &key, &json),
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
    if let Err(error) = &result
        && !matches!(
            error.downcast_ref::<StoreError>(),
            Some(StoreError::InvalidData(_))
        )
    {
        remember_failure(failure, failures, error);
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

fn encode<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|error| invalid(format!("Cannot encode JSON: {error}")))
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| invalid(format!("Path is not valid UTF-8: {}", path.display())))
}

fn require_project(connection: &Connection, id: Uuid) -> Result<()> {
    let exists: bool = connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)")?
        .query_row([id.to_string()], |row| row.get(0))?;
    if !exists {
        return Err(invalid(format!("Project {id} does not exist")));
    }
    Ok(())
}

fn require_worktree(connection: &Connection, id: WorktreeId, project: Uuid) -> Result<()> {
    let owner: Option<Uuid> = connection
        .prepare_cached("SELECT project_id FROM worktrees WHERE id = ?1")?
        .query_row([id.to_string()], |row| uuid(row, 0))
        .optional()?;
    if owner != Some(project) {
        return Err(invalid(format!(
            "Worktree {id} does not belong to project {project}"
        )));
    }
    Ok(())
}

fn save_setting(connection: &mut Connection, key: &str, json: &str) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction
        .prepare_cached(
            "INSERT INTO settings(key, value_json) VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        )?
        .execute(params![key, json])?;
    transaction.commit().context("Persist setting")
}

fn save_worktree(connection: &mut Connection, worktree: Worktree) -> Result<()> {
    if !worktree.path.is_absolute() {
        return Err(invalid("Worktree path must be an absolute directory"));
    }
    let path = path_text(&worktree.path)?;
    let id = worktree.id.to_string();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_project(&transaction, worktree.project_id)?;
    let previous = transaction
        .prepare_cached(
            "SELECT project_id, path, codename, base, created_by FROM worktrees WHERE id = ?1",
        )?
        .query_row([&id], |row| {
            Ok((
                uuid(row, 0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                uuid(row, 4)?,
            ))
        })
        .optional()?;
    if previous
        .as_ref()
        .is_some_and(|(project, previous_path, codename, base, creator)| {
            *project != worktree.project_id
                || previous_path != path
                || *codename != worktree.codename
                || *base != worktree.base
                || *creator != worktree.created_by
        })
    {
        return Err(invalid(
            "A worktree's project, directory, codename, base and creator cannot be reassigned",
        ));
    }
    let duplicate: bool = transaction
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM worktrees WHERE path = ?1 AND id != ?2)")?
        .query_row(params![path, id], |row| row.get(0))?;
    if duplicate {
        return Err(invalid(format!("A worktree already owns directory {path}")));
    }
    transaction
        .prepare_cached(
            "INSERT INTO worktrees(id, project_id, path, codename, branch, base, named, created_by,
         created_at, removed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET branch = excluded.branch, named = excluded.named,
         removed_at = excluded.removed_at",
        )?
        .execute(params![
            id,
            worktree.project_id.to_string(),
            path,
            worktree.codename,
            worktree.branch,
            worktree.base,
            worktree.named,
            worktree.created_by.to_string(),
            worktree.created_at,
            worktree.removed_at
        ])?;
    transaction.commit().context("Persist worktree")
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
    let duplicate: bool = transaction
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM projects WHERE root = ?1 AND id != ?2)")?
        .query_row(params![root, id], |row| row.get(0))?;
    if duplicate {
        return Err(invalid(format!(
            "A project already owns working-tree root {root}"
        )));
    }
    transaction.prepare_cached("INSERT INTO projects(id, name, root, archived, defaults_json) VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(id) DO UPDATE SET name = excluded.name, root = excluded.root,
        archived = excluded.archived, defaults_json = excluded.defaults_json")?.execute(
        params![id, project.name, root, project.archived, defaults],
    )?;
    transaction.commit().context("Persist project")
}

fn save_thread(connection: &mut Connection, thread: Thread) -> Result<()> {
    validate_thread_shape(&thread)?;
    let id = thread.id.to_string();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_project(&transaction, thread.project_id)?;
    if let Some(worktree) = thread.worktree_id {
        require_worktree(&transaction, worktree, thread.project_id)?;
    }
    let previous = transaction
        .prepare_cached("SELECT project_id, kind, factory_config_json, worktree_id, remote_json, execution_started FROM threads WHERE id = ?1")?.query_row(
            [&id],
            |row| {
                Ok((
                    uuid(row, 0)?,
                    thread_kind(row, 1)?,
                    decode_optional::<FactoryThreadConfig>(row, 2)?,
                    uuid_optional(row, 3)?,
                    decode_optional::<drostje_model::RemoteLocation>(row, 4)?,
                    row.get::<_, bool>(5)?,
                ))
            },
        )
        .optional()?;
    if previous
        .as_ref()
        .is_some_and(|(project, kind, _, worktree, _, _)| {
            *project != thread.project_id || kind != &thread.kind || *worktree != thread.worktree_id
        })
    {
        return Err(invalid(
            "A thread's project, kind and worktree cannot be reassigned",
        ));
    }
    if previous
        .as_ref()
        .is_some_and(|(_, _, _, _, remote, started)| {
            *started && (remote != &thread.remote || !thread.execution_started)
        })
    {
        return Err(invalid(
            "A started Conversation's execution location is immutable",
        ));
    }
    let owns_launch: bool = transaction
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM factory_launches WHERE factory_thread_id = ?1)",
        )?
        .query_row([&id], |row| row.get(0))?;
    if owns_launch
        && previous
            .as_ref()
            .is_some_and(|(_, _, config, _, _, _)| config != &thread.factory)
    {
        return Err(invalid(
            "A launched Factory thread's configuration is read-only",
        ));
    }
    let attachments = encode(&thread.attachments)?;
    let model = thread.model.as_ref().map(encode).transpose()?;
    let submission = thread.submission.as_ref().map(encode).transpose()?;
    let factory = thread.factory.as_ref().map(encode).transpose()?;
    let queued = encode(&thread.queued)?;
    let pull_request = thread.pull_request.as_ref().map(encode).transpose()?;
    let remote = thread.remote.as_ref().map(encode).transpose()?;
    transaction.prepare_cached("INSERT INTO threads(id, project_id, title, harness, session_ref, model_json, draft,
        attachments_json, submission_json, archived, created_at, updated_at, kind, factory_config_json, effort,
        permission_mode, queued_json, worktree_id, pull_request_json, remote_json, execution_started)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
        ON CONFLICT(id) DO UPDATE SET title = excluded.title, harness = excluded.harness,
        session_ref = excluded.session_ref, model_json = excluded.model_json, draft = excluded.draft,
        attachments_json = excluded.attachments_json, submission_json = excluded.submission_json,
        archived = excluded.archived, updated_at = excluded.updated_at,
        factory_config_json = excluded.factory_config_json, effort = excluded.effort,
        permission_mode = excluded.permission_mode, queued_json = excluded.queued_json,
        pull_request_json = excluded.pull_request_json, remote_json = excluded.remote_json,
        execution_started = excluded.execution_started")?.execute(
        params![id, thread.project_id.to_string(), thread.title, thread.harness, thread.session_ref,
            model, thread.draft, attachments, submission, thread.archived, thread.created_at,
            thread.updated_at, kind_text(&thread.kind), factory, thread.effort, thread.permission_mode,
            queued, thread.worktree_id.map(|id| id.to_string()), pull_request, remote, thread.execution_started])?;
    transaction.commit().context("Persist thread and draft")
}

fn save_launch(connection: &mut Connection, launch: FactoryLaunch) -> Result<()> {
    let event_path = launch.event_path.as_deref().map(path_text).transpose()?;
    let id = launch.id.to_string();
    let factory_thread_id = launch.factory_thread_id.to_string();
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_project(&transaction, launch.project_id)?;
    let owner = transaction
        .prepare_cached("SELECT project_id, kind, factory_config_json FROM threads WHERE id = ?1")?
        .query_row([&factory_thread_id], |row| {
            Ok((
                uuid(row, 0)?,
                thread_kind(row, 1)?,
                decode_optional::<FactoryThreadConfig>(row, 2)?,
            ))
        })
        .optional()?;
    if !owner.as_ref().is_some_and(|(project, kind, config)| {
        *project == launch.project_id && *kind == ThreadKind::Factory && config.is_some()
    }) {
        return Err(invalid(
            "Factory launch owner must be a Factory thread in the launch's project",
        ));
    }
    let previous = transaction
        .prepare_cached(
            "SELECT project_id, factory_thread_id, descriptor_json
             FROM factory_launches WHERE id = ?1",
        )?
        .query_row([&id], |row| {
            Ok((uuid(row, 0)?, uuid(row, 1)?, decode::<Value>(row, 2)?))
        })
        .optional()?;
    if let Some((project, owner, descriptor)) = &previous {
        if *project != launch.project_id || *owner != launch.factory_thread_id {
            return Err(invalid(
                "A Factory launch's project and owner cannot be reassigned",
            ));
        }
        if descriptor != &launch.descriptor {
            return Err(invalid("A Factory launch's descriptor is read-only"));
        }
    }
    let duplicate_owner: bool = transaction.prepare_cached("SELECT EXISTS(SELECT 1 FROM factory_launches WHERE factory_thread_id = ?1 AND id != ?2)")?.query_row(
        params![factory_thread_id, id],
        |row| row.get(0),
    )?;
    if duplicate_owner {
        return Err(invalid(
            "A Factory thread can own only one launch; create a new Factory thread",
        ));
    }
    if let Some(event_path) = event_path {
        let duplicate: bool = transaction
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM factory_launches WHERE event_path = ?1 AND id != ?2)",
            )?
            .query_row(params![event_path, id], |row| row.get(0))?;
        if duplicate {
            return Err(invalid(
                "An event path is already associated with another Factory launch",
            ));
        }
    }
    if previous.is_some() {
        transaction
            .prepare_cached(
                "UPDATE factory_launches SET event_path = ?2, work_item_id = ?3, attempt_id = ?4,
             process_exit = ?5, launch_error = ?6 WHERE id = ?1",
            )?
            .execute(params![
                id,
                event_path,
                launch.work_item_id,
                launch.attempt_id,
                launch.process_exit,
                launch.launch_error
            ])?;
    } else {
        transaction.prepare_cached("INSERT INTO factory_launches(id, project_id, factory_thread_id,
             descriptor_json, event_path, work_item_id, attempt_id, process_exit, launch_error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)")?.execute(
            params![id, launch.project_id.to_string(), factory_thread_id, encode(&launch.descriptor)?,
                event_path, launch.work_item_id, launch.attempt_id, launch.process_exit,
                launch.launch_error, launch.created_at],
        )?;
    }
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

fn uuid_optional(row: &Row<'_>, column: usize) -> rusqlite::Result<Option<Uuid>> {
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

fn read_projects(connection: &Connection) -> Result<Vec<Project>> {
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
    if projects.iter().any(|project| !project.root.is_absolute()) {
        bail!("Stored project root is not absolute");
    }
    if projects
        .iter()
        .map(|project| project.id)
        .collect::<HashSet<_>>()
        .len()
        != projects.len()
    {
        bail!("Stored projects contain duplicate UUID identities");
    }
    Ok(projects)
}

fn read_worktrees(connection: &Connection) -> Result<Vec<Worktree>> {
    let worktrees = connection
        .prepare(
            "SELECT id, project_id, path, codename, branch, base, named, created_by, created_at,
             removed_at FROM worktrees ORDER BY created_at, id",
        )?
        .query_map([], |row| {
            Ok(Worktree {
                id: uuid(row, 0)?,
                project_id: uuid(row, 1)?,
                path: PathBuf::from(row.get::<_, String>(2)?),
                codename: row.get(3)?,
                branch: row.get(4)?,
                base: row.get(5)?,
                named: archived(row, 6)?,
                created_by: uuid(row, 7)?,
                created_at: row.get(8)?,
                removed_at: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if worktrees
        .iter()
        .any(|worktree| !worktree.path.is_absolute())
    {
        bail!("Stored worktree directory is not absolute");
    }
    Ok(worktrees)
}

fn read_settings(connection: &Connection) -> Result<std::collections::BTreeMap<String, Value>> {
    Ok(connection
        .prepare("SELECT key, value_json FROM settings ORDER BY key")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, decode::<Value>(row, 1)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn kind_text(kind: &ThreadKind) -> &'static str {
    match kind {
        ThreadKind::Conversation => "conversation",
        ThreadKind::Factory => "factory",
    }
}

fn thread_kind(row: &Row<'_>, column: usize) -> rusqlite::Result<ThreadKind> {
    match row.get::<_, String>(column)?.as_str() {
        "conversation" => Ok(ThreadKind::Conversation),
        "factory" => Ok(ThreadKind::Factory),
        kind => Err(rusqlite::Error::FromSqlConversionFailure(
            column,
            Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid thread kind: {kind}"),
            )),
        )),
    }
}

fn validate_thread_shape(thread: &Thread) -> Result<()> {
    if let Some(remote) = &thread.remote {
        if remote.host.trim().is_empty() || remote.project.trim().is_empty() {
            return Err(invalid("Remote execution requires a Host and project key"));
        }
        if thread.worktree_id.is_some() {
            return Err(invalid("Remote Conversations cannot use desktop worktrees"));
        }
    }
    match thread.kind {
        ThreadKind::Conversation => {
            if thread.factory.is_some() {
                return Err(invalid(
                    "Conversation threads cannot have Factory configuration",
                ));
            }
            if thread.pull_request.is_some() && thread.worktree_id.is_none() {
                return Err(invalid(
                    "Only worktree Conversations can be linked to a pull request",
                ));
            }
        }
        ThreadKind::Factory => {
            if !thread.harness.is_empty()
                || !thread.draft.is_empty()
                || thread.session_ref.is_some()
                || thread.model.is_some()
                || thread.submission.is_some()
                || !thread.attachments.is_empty()
                || !thread.queued.is_empty()
                || thread.pull_request.is_some()
                || thread.remote.is_some()
                || thread.execution_started
            {
                return Err(invalid(
                    "Factory threads cannot contain interactive conversation state",
                ));
            }
            if thread.factory.is_none() {
                return Err(invalid("Factory threads require configuration"));
            }
        }
    }
    Ok(())
}

fn read_snapshot(connection: &Connection) -> Result<WorkspaceSnapshot> {
    let projects = read_projects(connection)?;
    let threads = connection.prepare("SELECT id, project_id, title, harness, session_ref, model_json, draft,
        attachments_json, submission_json, archived, created_at, updated_at, kind, factory_config_json, effort,
        permission_mode, queued_json, worktree_id, pull_request_json, remote_json, execution_started FROM threads ORDER BY created_at, id")?
        .query_map([], |row| Ok(Thread {
            id: uuid(row, 0)?, project_id: uuid(row, 1)?, title: row.get(2)?, harness: row.get(3)?,
            session_ref: row.get(4)?, model: decode_optional(row, 5)?, draft: row.get(6)?,
            attachments: decode(row, 7)?, submission: decode_optional(row, 8)?, archived: archived(row, 9)?,
            created_at: row.get(10)?, updated_at: row.get(11)?,
            kind: thread_kind(row, 12)?, factory: decode_optional(row, 13)?, effort: row.get(14)?,
            permission_mode: row.get(15)?, queued: decode(row, 16)?, worktree_id: uuid_optional(row, 17)?,
            pull_request: decode_optional(row, 18)?,
            remote: decode_optional(row, 19)?, execution_started: row.get(20)?,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let worktrees = read_worktrees(connection)?;
    let launches = connection
        .prepare(
            "SELECT id, project_id, factory_thread_id, descriptor_json, event_path,
        work_item_id, attempt_id, process_exit, launch_error, created_at
        FROM factory_launches ORDER BY created_at, id",
        )?
        .query_map([], |row| {
            Ok(FactoryLaunch {
                id: uuid(row, 0)?,
                project_id: uuid(row, 1)?,
                factory_thread_id: uuid(row, 2)?,
                descriptor: decode(row, 3)?,
                event_path: row.get::<_, Option<String>>(4)?.map(PathBuf::from),
                work_item_id: row.get(5)?,
                attempt_id: row.get(6)?,
                process_exit: row.get(7)?,
                launch_error: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let settings = read_settings(connection)?;
    let project_ids: HashSet<_> = projects.iter().map(|project| project.id).collect();
    let thread_by_id: HashMap<_, _> = threads.iter().map(|thread| (thread.id, thread)).collect();
    if thread_by_id.len() != threads.len()
        || launches
            .iter()
            .map(|launch| launch.id)
            .collect::<HashSet<_>>()
            .len()
            != launches.len()
    {
        bail!("Stored threads or launches contain duplicate UUID identities");
    }
    let mut owned_threads = HashSet::new();
    for launch in &launches {
        let owner = thread_by_id.get(&launch.factory_thread_id);
        if !project_ids.contains(&launch.project_id)
            || !owner.is_some_and(|thread| {
                thread.kind == ThreadKind::Factory && thread.project_id == launch.project_id
            })
        {
            bail!("Stored project/thread/Factory associations are inconsistent");
        }
        if !owned_threads.insert(launch.factory_thread_id) {
            bail!("A stored Factory thread owns more than one launch");
        }
    }
    let worktree_by_id: HashMap<_, _> = worktrees
        .iter()
        .map(|worktree| (worktree.id, worktree))
        .collect();
    if worktree_by_id.len() != worktrees.len() {
        bail!("Stored worktrees contain duplicate UUID identities");
    }
    if worktrees
        .iter()
        .any(|worktree| !project_ids.contains(&worktree.project_id))
    {
        bail!("Stored worktree belongs to a missing project");
    }
    for thread in &threads {
        if !project_ids.contains(&thread.project_id) {
            bail!("Stored thread belongs to a missing project");
        }
        if let Some(id) = thread.worktree_id
            && !worktree_by_id
                .get(&id)
                .is_some_and(|worktree| worktree.project_id == thread.project_id)
        {
            bail!("Stored thread references a worktree outside its project");
        }
        validate_thread_shape(thread)?;
    }
    Ok(WorkspaceSnapshot {
        projects,
        threads,
        worktrees,
        launches,
        settings,
    })
}
