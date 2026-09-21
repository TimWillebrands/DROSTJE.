use super::{FactoryLaunchSpec, FactorySnapshot, FactorySource, Liveness, events::EventTracker};
use crate::{
    model::{FactoryLaunch, LaunchId, ProjectId, ThreadId},
    platform,
    store::Store,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use async_channel::{Receiver, Sender};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const SCAN_INTERVAL: Duration = Duration::from_millis(500);
const ERROR_LIMIT: usize = 16 * 1024;
type Reply<T> = Sender<Result<T>>;

enum Request {
    Dispatch(ThreadId, FactoryLaunchSpec, Reply<LaunchId>),
    Prepared(LaunchId, Result<String>),
    Refresh(LaunchId, Reply<FactorySnapshot>),
    List(ProjectId, Sender<Vec<FactorySnapshot>>),
    Rescan(Reply<()>),
}

struct Handle {
    commands: Sender<Request>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Readiness workers must not keep a closed UI's actor accepting launches.
        self.commands.close();
    }
}

#[derive(Clone)]
pub struct FactoryManager {
    handle: Arc<Handle>,
    pub events: Receiver<FactorySnapshot>,
}

impl FactoryManager {
    /// Starts only the observer. Existing descriptors are never submitted again.
    pub fn open(store: Store, data_dir: PathBuf, launches: Vec<FactoryLaunch>) -> Result<Self> {
        let (commands, receiver) = async_channel::bounded(64);
        let (updates, events) = async_channel::unbounded();
        let worker_commands = commands.clone();
        thread::Builder::new()
            .name("drostje-factory".into())
            .spawn(move || {
                Actor::new(store, data_dir, launches, updates).run(receiver, worker_commands);
            })
            .context("Start Factory observer")?;
        Ok(Self {
            handle: Arc::new(Handle { commands }),
            events,
        })
    }

    pub async fn dispatch(
        &self,
        factory_thread_id: ThreadId,
        spec: FactoryLaunchSpec,
    ) -> Result<LaunchId> {
        let (reply, response) = async_channel::bounded(1);
        self.send(Request::Dispatch(factory_thread_id, spec, reply))
            .await?;
        response
            .recv()
            .await
            .context("Factory observer stopped before dispatch completed")?
    }

    pub async fn refresh(&self, id: LaunchId) -> Result<FactorySnapshot> {
        let (reply, response) = async_channel::bounded(1);
        self.send(Request::Refresh(id, reply)).await?;
        response
            .recv()
            .await
            .context("Factory observer stopped during refresh")?
    }

    pub async fn list(&self, project_id: ProjectId) -> Vec<FactorySnapshot> {
        let (reply, response) = async_channel::bounded(1);
        if self.send(Request::List(project_id, reply)).await.is_err() {
            return Vec::new();
        }
        response.recv().await.unwrap_or_default()
    }

    pub async fn rescan(&self) -> Result<()> {
        let (reply, response) = async_channel::bounded(1);
        self.send(Request::Rescan(reply)).await?;
        response
            .recv()
            .await
            .context("Factory observer stopped during rescan")?
    }

    async fn send(&self, request: Request) -> Result<()> {
        self.handle
            .commands
            .send(request)
            .await
            .map_err(|_| anyhow!("Factory observer is unavailable; no work was submitted"))
    }
}

struct Pending {
    spec: FactoryLaunchSpec,
    reply: Reply<LaunchId>,
}

struct OwnedChild {
    process: Child,
    _idle_sleep: Option<platform::IdleSleepAssertion>,
}

impl OwnedChild {
    fn wait(mut self) {
        let _ = self.process.wait();
    }
}

struct Actor {
    store: Store,
    data_dir: PathBuf,
    launches: BTreeMap<LaunchId, FactoryLaunch>,
    snapshots: BTreeMap<LaunchId, FactorySnapshot>,
    liveness: BTreeMap<LaunchId, Liveness>,
    pending: BTreeMap<LaunchId, Pending>,
    children: BTreeMap<LaunchId, OwnedChild>,
    wait_errors: BTreeMap<LaunchId, String>,
    power_errors: BTreeMap<LaunchId, String>,
    storage_failure: Option<String>,
    tracker: EventTracker,
    updates: Sender<FactorySnapshot>,
}

impl Actor {
    fn new(
        store: Store,
        data_dir: PathBuf,
        launches: Vec<FactoryLaunch>,
        updates: Sender<FactorySnapshot>,
    ) -> Self {
        let liveness = launches
            .iter()
            .map(|launch| {
                (
                    launch.id,
                    launch
                        .process_exit
                        .map(Liveness::Exited)
                        .unwrap_or(Liveness::Unverified),
                )
            })
            .collect();
        Self {
            store,
            data_dir,
            liveness,
            launches: launches
                .into_iter()
                .map(|launch| (launch.id, launch))
                .collect(),
            snapshots: BTreeMap::new(),
            pending: BTreeMap::new(),
            children: BTreeMap::new(),
            wait_errors: BTreeMap::new(),
            power_errors: BTreeMap::new(),
            storage_failure: None,
            tracker: EventTracker::new(),
            updates,
        }
    }

    fn run(mut self, receiver: Receiver<Request>, commands: Sender<Request>) {
        let _ = self.scan();
        let mut next_scan = Instant::now() + SCAN_INTERVAL;
        loop {
            if receiver.is_closed() {
                break;
            }
            if Instant::now() >= next_scan {
                self.wait_children();
                let _ = self.scan();
                next_scan = Instant::now() + SCAN_INTERVAL;
            }
            let request = smol::block_on(smol::future::race(
                async { receiver.recv().await.ok() },
                async {
                    smol::Timer::at(next_scan).await;
                    None
                },
            ));
            if receiver.is_closed() {
                break;
            }
            let Some(request) = request else {
                continue;
            };
            match request {
                Request::Dispatch(factory_thread_id, spec, reply) => {
                    if let Err(error) =
                        self.begin(factory_thread_id, spec, reply.clone(), &commands)
                    {
                        let _ = reply.try_send(Err(error));
                    }
                }
                Request::Prepared(id, result) => self.prepared(id, result),
                Request::Refresh(id, reply) => {
                    self.tracker.refresh();
                    self.wait_children();
                    let _ = self.scan();
                    let result = self
                        .snapshots
                        .get(&id)
                        .cloned()
                        .with_context(|| format!("Unknown Factory launch {id}"));
                    let _ = reply.try_send(result);
                }
                Request::List(project_id, reply) => {
                    let mut snapshots: Vec<_> = self
                        .snapshots
                        .values()
                        .filter(|snapshot| snapshot.launch.project_id == project_id)
                        .cloned()
                        .collect();
                    snapshots.sort_by(|left, right| {
                        right
                            .launch
                            .created_at
                            .cmp(&left.launch.created_at)
                            .then_with(|| right.launch.id.cmp(&left.launch.id))
                    });
                    let _ = reply.try_send(snapshots);
                }
                Request::Rescan(reply) => {
                    self.tracker.refresh();
                    self.wait_children();
                    let _ = reply.try_send(self.scan());
                }
            }
        }
        // Pending work has not spawned. On an abrupt process exit these descriptors
        // instead remain unknown, which is deliberately not a replay instruction.
        let pending_ids: Vec<_> = self.pending.keys().copied().collect();
        for id in pending_ids {
            self.fail_pending(
                id,
                anyhow!("Factory observer closed before the process was spawned"),
            );
        }
        // The closed observer's actor becomes the reaper. Release Store and
        // observation state, but retain each child's assertion until it is reaped.
        let children = std::mem::take(&mut self.children);
        drop(self);
        for (_, child) in children {
            child.wait();
        }
    }

    fn begin(
        &mut self,
        factory_thread_id: ThreadId,
        spec: FactoryLaunchSpec,
        reply: Reply<LaunchId>,
        commands: &Sender<Request>,
    ) -> Result<()> {
        self.writable()?;
        ensure!(
            !self.launches.contains_key(&spec.id),
            "Factory launch {} already exists; a re-dispatch needs a new launch ID",
            spec.id
        );
        let data_dir =
            fs::canonicalize(&self.data_dir).context("Resolve DROSTJE. data directory")?;
        let spec = normalize_spec(spec, &data_dir)?;
        self.wait_children();
        self.scan()?;
        self.guard_duplicate(&spec)?;
        let launch = FactoryLaunch {
            id: spec.id,
            project_id: spec.project_id,
            factory_thread_id,
            descriptor: serde_json::to_value(&spec).context("Encode Factory launch descriptor")?,
            event_path: None,
            work_item_id: match &spec.source {
                FactorySource::AzureWorkItem(id) => Some(*id),
                _ => None,
            },
            attempt_id: None,
            process_exit: None,
            launch_error: None,
            created_at: platform::now(),
        };
        // A successful fully synchronous Store commit is required before even
        // scheduling prerequisites. This pending record also closes duplicate clicks.
        smol::block_on(self.store.save_launch(launch.clone()))
            .context("Persist pending Factory launch; no process was spawned")?;
        let id = spec.id;
        self.launches.insert(id, launch);
        self.liveness.insert(id, Liveness::Unverified);
        self.pending.insert(
            id,
            Pending {
                spec: spec.clone(),
                reply,
            },
        );
        if let Err(error) = prepare_launch_files(&data_dir, &spec) {
            self.fail_pending(id, error);
            return Ok(());
        }
        let _ = self.scan();
        // Doctor can involve Docker/provider probes. It never blocks event polling
        // and is the only external process scheduled before the final spawn gate.
        let sender = commands.clone();
        let started = thread::Builder::new()
            .name("drostje-factory-readiness".into())
            .spawn(move || {
                let result = doctor(
                    spec.executable.as_deref().expect("normalized executable"),
                    &spec.target_repo,
                    &spec.harness,
                    Some(&spec),
                );
                let _ = sender.send_blocking(Request::Prepared(id, result));
            });
        if let Err(error) = started {
            self.fail_pending(id, error.into());
        }
        Ok(())
    }

    fn guard_duplicate(&self, spec: &FactoryLaunchSpec) -> Result<()> {
        for launch in self.launches.values() {
            let Ok(previous) =
                serde_json::from_value::<FactoryLaunchSpec>(launch.descriptor.clone())
            else {
                continue;
            };
            if !same_source(&previous, spec) {
                continue;
            }
            ensure!(
                !self.pending.contains_key(&launch.id) && !self.children.contains_key(&launch.id),
                "Factory launch {} for this source is pending or owned and running",
                launch.id
            );
            let terminal = self
                .snapshots
                .get(&launch.id)
                .is_some_and(|snapshot| snapshot.reported_state.terminal());
            if terminal || launch.launch_error.is_some() {
                continue;
            }
            ensure!(
                spec.confirm_unknown,
                "Factory launch {} has an unresolved outcome. View its state and explicitly confirm re-dispatch with a fresh launch ID; the earlier process may still be running",
                launch.id
            );
        }
        Ok(())
    }

    fn prepared(&mut self, id: LaunchId, result: Result<String>) {
        let Some(pending) = self.pending.get(&id) else {
            return;
        };
        if let Err(error) = result {
            self.fail_pending(id, error);
            return;
        }
        let spec = pending.spec.clone();
        let result = (|| {
            // Recheck the selected local inputs after a potentially long doctor.
            platform::validate_branch(&spec.target_repo, &spec.target_branch)?;
            if let FactorySource::LocalFolder(folder) = &spec.source {
                read_prd(folder)?;
            }
            let stdout = append_log(&launch_dir(&spec).join("stdout.log"))?;
            let stderr = append_log(&launch_dir(&spec).join("stderr.log"))?;
            // Prerequisites succeeding cannot override a subsequent Store failure.
            self.writable()?;
            let idle_sleep = match platform::IdleSleepAssertion::acquire("DROSTJE. Factory run") {
                Ok(assertion) => Some(assertion),
                Err(error) => {
                    self.power_errors.insert(
                        id,
                        format!(
                            "Cannot prevent idle system sleep during this Factory run: {error:#}. Keep your Mac awake manually until the run exits."
                        ),
                    );
                    None
                }
            };
            Ok(OwnedChild {
                process: spawn_run(&spec, stdout, stderr)?,
                _idle_sleep: idle_sleep,
            })
        })();
        match result {
            Ok(child) => {
                self.children.insert(id, child);
                self.liveness.insert(id, Liveness::OwnedRunning);
                if let Some(pending) = self.pending.remove(&id) {
                    let _ = pending.reply.try_send(Ok(id));
                }
                let _ = self.scan();
            }
            Err(error) => self.fail_pending(id, error),
        }
    }

    fn fail_pending(&mut self, id: LaunchId, error: anyhow::Error) {
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        let message = format!("{error:#}");
        if let Ok(mut log) = append_log(&launch_dir(&pending.spec).join("stderr.log")) {
            let _ = writeln!(log, "DROSTJE.: process not spawned: {message}");
            let _ = log.sync_all();
        }
        let mut launch = self.launches[&id].clone();
        launch.launch_error = Some(bounded_error(&message));
        let saved = self.save_observation(launch);
        let _ = self.scan();
        let error = match saved {
            Ok(()) => error,
            Err(storage) => {
                anyhow!("{message}\nThe launch refusal could not be persisted: {storage:#}")
            }
        };
        let _ = pending.reply.try_send(Err(error));
    }

    fn writable(&mut self) -> Result<()> {
        if let Some(error) = &self.storage_failure {
            bail!("Factory persistence is disabled: {error}");
        }
        if let Err(error) = smol::block_on(self.store.writable()) {
            self.storage_failure = Some(format!("{error:#}"));
            return Err(error.context("Factory dispatch requires writable durable storage"));
        }
        Ok(())
    }

    fn save_observation(&mut self, launch: FactoryLaunch) -> Result<()> {
        let result = smol::block_on(self.store.save_launch(launch.clone()));
        if let Err(error) = &result {
            self.storage_failure = Some(format!("{error:#}"));
        }
        // An observed exit/refusal remains true even if its durable write failed.
        // Its snapshot explicitly warns that this observation was not persisted.
        self.launches.insert(launch.id, launch);
        result
    }

    fn wait_children(&mut self) {
        let mut exited = Vec::new();
        for (&id, child) in &mut self.children {
            match child.process.try_wait() {
                Ok(Some(status)) => exited.push((
                    id,
                    status
                        .code()
                        .unwrap_or_else(|| -status.signal().unwrap_or(1)),
                )),
                Ok(None) => {}
                Err(error) => {
                    self.liveness.insert(id, Liveness::Unverified);
                    self.wait_errors.insert(
                        id,
                        format!("Cannot observe the owned child's exit: {error}"),
                    );
                }
            }
        }
        for (id, code) in exited {
            self.children.remove(&id);
            self.wait_errors.remove(&id);
            self.liveness.insert(id, Liveness::Exited(code));
            let mut launch = self.launches[&id].clone();
            launch.process_exit = Some(code);
            let _ = self.save_observation(launch);
        }
    }

    fn scan(&mut self) -> Result<()> {
        let launches: Vec<_> = self.launches.values().cloned().collect();
        let mut projections = self.tracker.scan(&launches, &self.liveness);
        for snapshot in &mut projections {
            let Some(current) = self.launches.get(&snapshot.launch.id) else {
                continue;
            };
            let mut linked = current.clone();
            linked.event_path = snapshot.launch.event_path.clone();
            linked.work_item_id = snapshot.launch.work_item_id;
            linked.attempt_id = snapshot.launch.attempt_id.clone();
            if linked != *current && self.storage_failure.is_none() {
                match smol::block_on(self.store.save_launch(linked.clone())) {
                    Ok(()) => {
                        self.launches.insert(linked.id, linked);
                    }
                    Err(error) => {
                        self.storage_failure =
                            Some(format!("Cannot persist Factory event linkage: {error:#}"));
                    }
                }
            }
            // The projection supplies linkage only, never overwrites the actual
            // process outcome or errors collected by this actor.
            snapshot.launch = self.launches[&snapshot.launch.id].clone();
        }
        for mut snapshot in projections {
            if let Some(error) = &self.storage_failure {
                snapshot.diagnostics.push(format!("Factory persistence failed; observations may not survive restart and new dispatch is disabled: {error}"));
            }
            if self.pending.contains_key(&snapshot.launch.id) {
                snapshot.diagnostics.push("Pending dispatch: checking prerequisites; no Factory run process has been spawned.".into());
            }
            if let Some(error) = self.wait_errors.get(&snapshot.launch.id) {
                snapshot.diagnostics.push(error.clone());
            }
            if let Some(error) = self.power_errors.get(&snapshot.launch.id) {
                snapshot.diagnostics.push(error.clone());
            }
            if self.snapshots.get(&snapshot.launch.id) != Some(&snapshot) {
                self.snapshots.insert(snapshot.launch.id, snapshot.clone());
                let _ = self.updates.try_send(snapshot);
            }
        }
        match &self.storage_failure {
            Some(error) => Err(anyhow!("Factory persistence failed: {error}")),
            None => Ok(()),
        }
    }
}

pub async fn check_readiness(
    executable: Option<PathBuf>,
    cwd: PathBuf,
    harness: String,
) -> Result<String> {
    smol::unblock(move || {
        validate_harness(&harness)?;
        let executable = resolve_factory(executable.as_deref())?;
        let cwd = fs::canonicalize(cwd).context("Resolve Factory readiness working directory")?;
        doctor(&executable, &cwd, &harness, None)
    })
    .await
}

fn doctor(
    executable: &Path,
    cwd: &Path,
    harness: &str,
    spec: Option<&FactoryLaunchSpec>,
) -> Result<String> {
    let mut command = Command::new(executable);
    command
        .args(["doctor", "--harness", harness])
        .current_dir(cwd)
        .stdin(Stdio::null());
    if let Some(spec) = spec {
        // These are the CLI's documented environment counterparts, so the exact
        // doctor argv remains unchanged while checking this launch's directories.
        command
            .env("FACTORY_DATA_ROOT", &spec.data_root)
            .env("FACTORY_LOG_DIR", &spec.log_dir)
            .env("FACTORY_TARGET_REPO", &spec.target_repo)
            .env("FACTORY_TARGET_BRANCH", &spec.target_branch);
    }
    let output = command
        .output()
        .with_context(|| format!("Run {} doctor --harness {harness}", executable.display()))?;
    let text = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // HostReadinessReport.ExitCode is nonzero exactly for core failure; Azure
    // warnings intentionally do not prevent valid local-folder work.
    ensure!(
        output.status.success(),
        "Factory readiness refused ({}):\n{text}",
        output.status
    );
    Ok(text)
}

fn validate_harness(harness: &str) -> Result<()> {
    ensure!(
        matches!(harness, "opencode" | "codex" | "claude-code"),
        "Unsupported Factory harness {harness:?}; choose opencode, codex, or claude-code"
    );
    Ok(())
}

fn resolve_factory(executable: Option<&Path>) -> Result<PathBuf> {
    let explicit = executable
        .map(|path| {
            path.to_str()
                .context("Factory executable path must be valid UTF-8")
        })
        .transpose()?;
    platform::resolve_executable("factory", explicit)
}

fn normalize_spec(mut spec: FactoryLaunchSpec, data_dir: &Path) -> Result<FactoryLaunchSpec> {
    validate_harness(&spec.harness)?;
    spec.executable = Some(resolve_factory(spec.executable.as_deref())?);
    spec.target_repo = platform::inspect_repository(&spec.target_repo)?.root;
    platform::validate_branch(&spec.target_repo, &spec.target_branch)?;
    platform::git(
        &spec.target_repo,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", spec.target_branch),
        ],
    )?;
    spec.data_root = prospective_path(&spec.data_root)?;
    if spec.data_root.exists() {
        ensure!(
            spec.data_root.is_dir(),
            "Factory data root is not a directory"
        );
    }
    let launches = data_dir.join("factory-launches");
    check_directory_if_present(&launches)?;
    let expected_log_dir = launches.join(spec.id.to_string()).join("logs");
    ensure!(
        prospective_path(&spec.log_dir)? == expected_log_dir,
        "Factory log directory must be {}",
        expected_log_dir.display()
    );
    ensure!(
        !path_exists(expected_log_dir.parent().expect("launch directory"))?,
        "Factory launch directory already exists; use a fresh launch ID"
    );
    spec.log_dir = expected_log_dir;
    spec.model = spec.model.filter(|value| !value.trim().is_empty());
    spec.thought_level = spec.thought_level.filter(|value| !value.trim().is_empty());
    match &mut spec.source {
        FactorySource::LocalFolder(folder) => {
            *folder = fs::canonicalize(&*folder)
                .with_context(|| format!("Resolve Factory folder {}", folder.display()))?;
            read_prd(folder)?;
        }
        FactorySource::AzureWorkItem(id) => {
            ensure!(*id > 0, "Azure work-item ID must be a positive integer")
        }
    }
    Ok(spec)
}

pub(crate) fn same_source(left: &FactoryLaunchSpec, right: &FactoryLaunchSpec) -> bool {
    match (&left.source, &right.source) {
        (FactorySource::LocalFolder(left), FactorySource::LocalFolder(right)) => {
            fs::canonicalize(left).unwrap_or_else(|_| left.clone())
                == fs::canonicalize(right).unwrap_or_else(|_| right.clone())
        }
        (FactorySource::AzureWorkItem(left_id), FactorySource::AzureWorkItem(right_id)) => {
            left_id == right_id
                && prospective_path(&left.data_root).is_ok_and(|left_root| {
                    prospective_path(&right.data_root)
                        .is_ok_and(|right_root| left_root == right_root)
                })
        }
        _ => false,
    }
}

fn read_prd(folder: &Path) -> Result<()> {
    ensure!(
        folder.is_dir(),
        "Factory folder is not a directory: {}",
        folder.display()
    );
    let path = folder.join("PRD.md");
    let mut file = File::open(&path)
        .with_context(|| format!("Read existing Factory PRD {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "Factory PRD is not a regular file: {}",
        path.display()
    );
    io::copy(&mut file, &mut io::sink())
        .with_context(|| format!("Read existing Factory PRD {}", path.display()))?;
    Ok(())
}

fn prospective_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if let Ok(path) = fs::canonicalize(&absolute) {
        return Ok(path);
    }
    ensure!(
        !absolute
            .components()
            .any(|component| component == Component::ParentDir),
        "A not-yet-created Factory path must not contain '..': {}",
        path.display()
    );
    let mut ancestor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                ensure!(
                    resolved.is_dir(),
                    "Factory path parent is not a directory: {}",
                    resolved.display()
                );
                for name in missing.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .context("Factory path has no existing ancestor")?
                        .to_os_string(),
                );
                ancestor = ancestor
                    .parent()
                    .context("Factory path has no existing parent")?;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Resolve Factory path {}", path.display()));
            }
        }
    }
}

fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("Inspect {}", path.display())),
    }
}

fn check_directory_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.file_type().is_dir(),
            "App-owned directory must not be a symlink or other file: {}",
            path.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Inspect app-owned directory {}", path.display()));
        }
    }
    Ok(())
}

fn create_owned_parent(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => sync_parent(path)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            check_directory_if_present(path)?
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Create app-owned directory {}", path.display()));
        }
    }
    Ok(())
}

fn launch_dir(spec: &FactoryLaunchSpec) -> &Path {
    spec.log_dir
        .parent()
        .expect("validated launch log directory")
}

fn prepare_launch_files(data_dir: &Path, spec: &FactoryLaunchSpec) -> Result<()> {
    create_owned_parent(&data_dir.join("factory-launches"))?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(launch_dir(spec))
        .context("Reserve a new Factory launch directory")?;
    sync_parent(launch_dir(spec))?;
    fs::create_dir(&spec.log_dir).context("Create Factory log directory")?;
    let descriptor =
        serde_json::to_vec_pretty(spec).context("Encode file-backed Factory launch descriptor")?;
    create_file(&launch_dir(spec).join("descriptor.json"), &descriptor)?;
    append_log(&launch_dir(spec).join("stdout.log"))?.sync_all()?;
    append_log(&launch_dir(spec).join("stderr.log"))?.sync_all()?;
    File::open(launch_dir(spec))?
        .sync_all()
        .context("Persist Factory launch directory")?;
    Ok(())
}

fn create_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| {
            format!(
                "Create new {} without overwriting existing work",
                path.display()
            )
        })?;
    file.write_all(contents)
        .with_context(|| format!("Write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("Persist {}", path.display()))
}

fn sync_parent(path: &Path) -> Result<()> {
    File::open(path.parent().context("Missing parent directory")?)?
        .sync_all()
        .context("Persist new directory entry")
}

fn append_log(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("Open append-only Factory log {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "Factory output must be a regular file: {}",
        path.display()
    );
    Ok(file)
}

fn spawn_run(spec: &FactoryLaunchSpec, stdout: File, stderr: File) -> Result<Child> {
    let executable = spec
        .executable
        .as_ref()
        .context("Factory executable was not resolved")?;
    let mut command = Command::new(executable);
    command.arg("run");
    match &spec.source {
        FactorySource::LocalFolder(folder) => {
            command.arg("--folder").arg(folder);
        }
        FactorySource::AzureWorkItem(id) => {
            command.arg(id.to_string());
        }
    }
    command
        .arg("--target-repo")
        .arg(&spec.target_repo)
        .arg("--target-branch")
        .arg(&spec.target_branch)
        .arg("--data-root")
        .arg(&spec.data_root)
        .arg("--log-dir")
        .arg(&spec.log_dir)
        .arg("--harness")
        .arg(&spec.harness);
    if let Some(model) = &spec.model {
        command.arg("--model").arg(model);
    }
    if let Some(level) = &spec.thought_level {
        command.arg("--thought-level").arg(level);
    }
    command
        .arg("--yes")
        .current_dir(&spec.target_repo)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    // Only async-signal-safe operations belong between fork and exec. A failed
    // setsid must fail spawn rather than leave Factory tied to the GUI's session.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .with_context(|| format!("Spawn detached Factory run using {}", executable.display()))
}

fn bounded_error(message: &str) -> String {
    if message.len() <= ERROR_LIMIT {
        return message.to_owned();
    }
    let mut end = ERROR_LIMIT;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[Full diagnostic is in stderr.log]", &message[..end])
}
