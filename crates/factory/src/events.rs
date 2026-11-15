use super::{
    EventEntry, FactoryLaunchSpec, FactorySnapshot, Liveness, ReportedState, TaskSnapshot,
};
use anyhow::{Context, Result, bail};
use drostje_model::{FactoryLaunch, LaunchId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    ops::Bound,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

const RECENT_ENTRIES: usize = 200;
const PAGE_BYTES: usize = 256 * 1024;
const READ_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPage {
    pub path: PathBuf,
    pub text: String,
    pub start_offset: u64,
    pub end_offset: u64,
    pub has_older: bool,
}

pub async fn read_text(path: PathBuf, before: Option<u64>) -> Result<TextPage> {
    smol::unblock(move || text_page(path, before)).await
}

fn text_page(path: PathBuf, before: Option<u64>) -> Result<TextPage> {
    let mut file =
        File::open(&path).with_context(|| format!("Text unavailable: {}", path.display()))?;
    let length = file.metadata()?.len();
    if before.is_some_and(|offset| offset > length) {
        bail!(
            "Text changed or was truncated: {}; reload its tail",
            path.display()
        );
    }
    let requested_end = before.unwrap_or(length);
    let window_start = requested_end.saturating_sub(PAGE_BYTES as u64);
    file.seek(SeekFrom::Start(window_start))?;
    let mut bytes = vec![0; (requested_end - window_start) as usize];
    file.read_exact(&mut bytes)?;
    let end = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    if end == 0 && requested_end != 0 {
        bail!(
            "No complete text line within the {}-byte page at {}",
            PAGE_BYTES,
            path.display()
        );
    }
    let mut start = 0;
    let mut lines = 0;
    for index in (0..end).rev() {
        if bytes[index] == b'\n' {
            lines += 1;
            if lines > RECENT_ENTRIES {
                start = index + 1;
                break;
            }
        }
    }
    if start == 0 && window_start != 0 {
        file.seek(SeekFrom::Start(window_start - 1))?;
        let mut previous = [0];
        file.read_exact(&mut previous)?;
        if previous[0] != b'\n' {
            start = bytes[..end].iter().position(|byte| *byte == b'\n').unwrap() + 1;
        }
    }
    if start == end && end != 0 {
        bail!(
            "Complete text line exceeds the {}-byte page at {}",
            PAGE_BYTES,
            path.display()
        );
    }
    Ok(TextPage {
        path,
        text: String::from_utf8_lossy(&bytes[start..end]).into_owned(),
        start_offset: window_start + start as u64,
        end_offset: window_start + end as u64,
        has_older: window_start + start as u64 != 0,
    })
}

#[derive(Clone)]
struct Projection {
    state: ReportedState,
    title: Option<String>,
    story_id: Option<i32>,
    attempt_id: Option<String>,
    log_paths: BTreeSet<PathBuf>,
    tasks: BTreeMap<String, TaskSnapshot>,
    notes: VecDeque<EventEntry>,
    artifacts: BTreeSet<PathBuf>,
    transcripts: BTreeSet<PathBuf>,
    last_seen: Option<String>,
    diagnostics: VecDeque<String>,
    details: Map<String, Value>,
}

impl Default for Projection {
    fn default() -> Self {
        Self {
            state: ReportedState::Pending,
            title: None,
            story_id: None,
            attempt_id: None,
            log_paths: BTreeSet::new(),
            tasks: BTreeMap::new(),
            notes: VecDeque::new(),
            artifacts: BTreeSet::new(),
            transcripts: BTreeSet::new(),
            last_seen: None,
            diagnostics: VecDeque::new(),
            details: Map::new(),
        }
    }
}

fn bounded_push<T>(items: &mut VecDeque<T>, value: T) {
    if items.len() == RECENT_ENTRIES {
        items.pop_front();
    }
    items.push_back(value);
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

fn task_id(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_i64().map(|id| id.to_string())
}

fn task<'a>(tasks: &'a mut BTreeMap<String, TaskSnapshot>, id: &str) -> &'a mut TaskSnapshot {
    tasks.entry(id.to_owned()).or_insert_with(|| TaskSnapshot {
        id: id.to_owned(),
        title: None,
        state: "Unknown".into(),
        dependencies: Vec::new(),
        branch: None,
        step: None,
        diagnostic: None,
    })
}

fn diagnostic_text(payload: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| string(payload, key))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

fn valid_payload(event_type: &str, payload: &Value) -> bool {
    let text = |key| payload.get(key).is_some_and(Value::is_string);
    let integer = |key| {
        payload
            .get(key)
            .and_then(Value::as_i64)
            .is_some_and(|id| i32::try_from(id).is_ok())
    };
    match event_type {
        "run-started" => integer("userStoryId") && text("logPath"),
        "story-loaded" => integer("userStoryId") && text("title") && text("state"),
        "attempt-ready" => {
            integer("userStoryId")
                && text("attemptId")
                && text("targetBranch")
                && text("baseSha")
                && text("mode")
        }
        "agent-selected" => text("harness") && text("model"),
        "feature-branch-ready" => text("featureBranch"),
        "task-dag-loaded" => {
            payload
                .get("tasks")
                .and_then(Value::as_array)
                .is_some_and(|tasks| {
                    tasks.iter().all(|task| {
                        task.get("id").and_then(Value::as_i64).is_some()
                            && task.get("state").is_some_and(Value::is_string)
                    })
                })
                && payload
                    .get("dependencyLinks")
                    .and_then(Value::as_array)
                    .is_some_and(|links| {
                        links.iter().all(|link| {
                            link.get("predecessorId").and_then(Value::as_i64).is_some()
                                && link.get("successorId").and_then(Value::as_i64).is_some()
                        })
                    })
        }
        "slice-started" => integer("taskId") && text("sliceBranch"),
        "slice-step-started" => integer("taskId") && text("slug"),
        "slice-step-completed" => integer("taskId") && text("slug") && text("outcome"),
        "slice-merged" => integer("taskId") && text("text"),
        "slice-failed" => {
            integer("taskId") && text("role") && text("status") && text("stopFailure")
        }
        "slice-parked" => integer("taskId") && text("slug") && text("reason"),
        "active-sandbox" => {
            integer("taskId") && text("identity") && text("role") && text("sliceBranch")
        }
        "run-completed" => text("text"),
        "run-aborted" => text("reason") && text("text"),
        "run-paused" => integer("taskId") && text("reason") && text("text"),
        _ => true,
    }
}

impl Projection {
    fn line(&mut self, bytes: &[u8], offset: u64, path: &Path) {
        let parsed = serde_json::from_slice::<Value>(bytes);
        let value = match parsed {
            Ok(value) => value,
            Err(error) => {
                bounded_push(
                    &mut self.diagnostics,
                    format!(
                        "Malformed event at {} byte {offset}: {error}",
                        path.display()
                    ),
                );
                return;
            }
        };
        let event_type = string(&value, "eventType");
        let timestamp = string(&value, "timestampUtc");
        let payload = value.get("payload");
        if event_type.is_none() || timestamp.is_none() || !payload.is_some_and(Value::is_object) {
            bounded_push(
                &mut self.diagnostics,
                format!(
                    "Malformed event envelope at {} byte {offset}",
                    path.display()
                ),
            );
            return;
        }
        let entry = EventEntry {
            offset,
            timestamp,
            event_type: event_type.unwrap(),
            payload: payload.unwrap().clone(),
            raw: String::from_utf8_lossy(bytes).into_owned(),
        };
        if value.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
            bounded_push(
                &mut self.diagnostics,
                format!(
                    "Unsupported event schema {} at {} byte {offset}; event not applied",
                    value.get("schemaVersion").unwrap_or(&Value::Null),
                    path.display()
                ),
            );
            bounded_push(&mut self.notes, entry);
            return;
        }
        if !valid_payload(&entry.event_type, &entry.payload) {
            bounded_push(
                &mut self.diagnostics,
                format!(
                    "Malformed {} payload at {} byte {offset}; event not applied",
                    entry.event_type,
                    path.display()
                ),
            );
            bounded_push(&mut self.notes, entry);
            return;
        }
        self.last_seen = entry.timestamp.clone();
        self.fold(&entry);
        bounded_push(&mut self.notes, entry);
    }

    fn fold(&mut self, event: &EventEntry) {
        let payload = &event.payload;
        for key in ["artifactRef", "artifactPath", "logPath"] {
            if let Some(path) = string(payload, key).filter(|path| !path.is_empty()) {
                self.artifacts.insert(PathBuf::from(path));
            }
        }
        for key in ["transcriptPath", "transcript"] {
            if let Some(path) = string(payload, key).filter(|path| !path.is_empty()) {
                self.transcripts.insert(PathBuf::from(path));
            }
        }
        if event.event_type == "agent-note"
            && let Some(paths) = payload.get("paths").and_then(Value::as_array)
        {
            self.artifacts.extend(
                paths
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from),
            );
        }
        match event.event_type.as_str() {
            "run-started" => {
                if !self.state.terminal() {
                    self.state = ReportedState::Running;
                }
                self.story_id = payload
                    .get("userStoryId")
                    .and_then(Value::as_i64)
                    .and_then(|id| i32::try_from(id).ok());
                if let Some(path) = string(payload, "logPath").filter(|path| !path.is_empty()) {
                    self.log_paths.insert(PathBuf::from(path));
                }
            }
            "story-loaded" => {
                self.title = string(payload, "title");
                self.details
                    .insert(event.event_type.clone(), payload.clone());
            }
            "attempt-ready" => {
                self.attempt_id = string(payload, "attemptId");
                self.details
                    .insert(event.event_type.clone(), payload.clone());
            }
            "agent-selected"
            | "feature-branch-ready"
            | "target-branch-resolved"
            | "active-sandbox" => {
                self.details
                    .insert(event.event_type.clone(), payload.clone());
            }
            "task-dag-loaded" => {
                self.tasks.clear();
                if let Some(tasks) = payload.get("tasks").and_then(Value::as_array) {
                    for entry in tasks {
                        if let Some(id) = task_id(entry, "id") {
                            let task = task(&mut self.tasks, &id);
                            if let Some(state) = string(entry, "state") {
                                task.state = state;
                            }
                            task.title = string(entry, "title");
                            task.dependencies.clear();
                        }
                    }
                }
                if let Some(links) = payload.get("dependencyLinks").and_then(Value::as_array) {
                    for link in links {
                        if let (Some(predecessor), Some(successor)) =
                            (task_id(link, "predecessorId"), task_id(link, "successorId"))
                        {
                            let dependencies = &mut task(&mut self.tasks, &successor).dependencies;
                            if !dependencies.contains(&predecessor) {
                                dependencies.push(predecessor);
                            }
                        }
                    }
                }
            }
            "slice-started"
            | "slice-step-started"
            | "slice-step-completed"
            | "slice-merged"
            | "slice-failed"
            | "slice-parked" => {
                if let Some(id) = task_id(payload, "taskId") {
                    let task = task(&mut self.tasks, &id);
                    match event.event_type.as_str() {
                        "slice-started" => {
                            task.state = "Running".into();
                            task.branch = string(payload, "sliceBranch");
                            task.step = None;
                            task.diagnostic = None;
                        }
                        "slice-step-started" => {
                            task.state = "Running".into();
                            task.step = string(payload, "slug");
                        }
                        "slice-step-completed" => {
                            task.step = string(payload, "slug");
                            task.diagnostic = string(payload, "outcome");
                        }
                        "slice-merged" => {
                            task.state = "Merged".into();
                            task.diagnostic = string(payload, "text");
                        }
                        "slice-failed" => {
                            task.state = "Failed".into();
                            task.diagnostic = Some(diagnostic_text(
                                payload,
                                &[
                                    "role",
                                    "status",
                                    "stopReason",
                                    "stopFailure",
                                    "failureDetail",
                                ],
                            ));
                        }
                        "slice-parked" => {
                            task.state = "Paused".into();
                            task.step = string(payload, "slug");
                            task.diagnostic = string(payload, "reason");
                        }
                        _ => unreachable!(),
                    }
                }
            }
            "run-completed" | "run-aborted" | "run-paused" => {
                self.state = match event.event_type.as_str() {
                    "run-completed" => ReportedState::Completed,
                    "run-aborted" => ReportedState::Failed,
                    _ => ReportedState::Paused,
                };
                self.details
                    .insert(event.event_type.clone(), payload.clone());
                let diagnostic = diagnostic_text(payload, &["reason", "text"]);
                if !diagnostic.is_empty() {
                    bounded_push(&mut self.diagnostics, diagnostic);
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    identity: (u64, u64),
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Stamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            identity: (metadata.dev(), metadata.ino()),
            len: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    fn settled(&self) -> bool {
        const SETTLE_SECONDS: i64 = 3;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        now - self.modified.0.max(self.changed.0) >= SETTLE_SECONDS
    }
}

#[derive(Default)]
struct EventFile {
    identity: Option<(u64, u64)>,
    modified: Option<SystemTime>,
    changed: Option<(i64, i64)>,
    offset: u64,
    partial: Vec<u8>,
    checkpoint: Vec<u8>,
    projection: Projection,
    replay: bool,
    unavailable: Option<String>,
    log_paths: Vec<PathBuf>,
    changed_at: u64,
}

impl EventFile {
    fn update(&mut self, path: &Path, generation: u64) {
        if !self.replay && self.unavailable.is_none() && self.unchanged_on_disk(path) {
            return;
        }
        let opened = File::open(path)
            .map_err(anyhow::Error::from)
            .and_then(|file| {
                let metadata = file.metadata()?;
                if !metadata.is_file() {
                    bail!("not a regular file");
                }
                Ok((file, metadata))
            });
        let (read, error) = match opened {
            Ok((file, metadata)) => match self.read(file, &metadata, path) {
                Ok(read) => (read, None),
                Err(error) => (true, Some(error)),
            },
            Err(error) => (false, Some(error)),
        };
        let unavailable =
            error.map(|error| format!("Event history unavailable: {}: {error:#}", path.display()));
        if read || unavailable != self.unavailable {
            self.changed_at = generation;
            self.log_paths = self
                .projection
                .log_paths
                .iter()
                .filter_map(|path| normalized(path))
                .collect();
        }
        self.unavailable = unavailable;
    }

    fn unchanged_on_disk(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|metadata| {
            let stamp = Stamp::of(&metadata);
            metadata.is_file()
                && self.identity == Some(stamp.identity)
                && stamp.len == self.offset
                && self.modified == metadata.modified().ok()
                && self.changed == Some(stamp.changed)
        })
    }

    fn read(&mut self, mut file: File, metadata: &fs::Metadata, path: &Path) -> Result<bool> {
        let stamp = Stamp::of(metadata);
        let identity = stamp.identity;
        let modified = metadata.modified().ok();
        self.changed = Some(stamp.changed);
        let mut replaced =
            self.identity != Some(identity) || metadata.len() < self.offset || self.replay;
        if !replaced && metadata.len() == self.offset && modified == self.modified {
            return Ok(false);
        }
        if !replaced && modified != self.modified {
            if metadata.len() == self.offset {
                replaced = true;
            } else if !self.checkpoint.is_empty() {
                file.seek(SeekFrom::Start(self.offset - self.checkpoint.len() as u64))?;
                let mut checkpoint = [0; 256];
                file.read_exact(&mut checkpoint[..self.checkpoint.len()])?;
                replaced = checkpoint[..self.checkpoint.len()] != self.checkpoint;
            }
        }
        if replaced {
            self.offset = 0;
            self.partial.clear();
            self.checkpoint.clear();
            self.projection = Projection::default();
        }
        self.identity = Some(identity);
        self.modified = modified;
        self.replay = false;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = [0; READ_BYTES];
        loop {
            let count = file.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            let buffer_offset = self.offset - self.partial.len() as u64;
            let previous_len = self.partial.len();
            self.offset += count as u64;
            self.partial.extend_from_slice(&bytes[..count]);
            let mut start = 0;
            for (index, byte) in self.partial.iter().enumerate().skip(previous_len) {
                if *byte == b'\n' {
                    let mut end = index;
                    if end > start && self.partial[end - 1] == b'\r' {
                        end -= 1;
                    }
                    self.projection.line(
                        &self.partial[start..end],
                        buffer_offset + start as u64,
                        path,
                    );
                    start = index + 1;
                }
            }
            self.partial.drain(..start);
        }
        let checkpoint_size = self.offset.min(256) as usize;
        self.checkpoint.resize(checkpoint_size, 0);
        file.seek(SeekFrom::Start(self.offset - checkpoint_size as u64))?;
        file.read_exact(&mut self.checkpoint)?;
        Ok(true)
    }
}

fn normalized(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    if let Ok(path) = fs::canonicalize(path) {
        return Some(path);
    }
    let mut ancestor = path;
    let mut suffix = Vec::new();
    while let Some(parent) = ancestor.parent() {
        if let Ok(canonical) = fs::canonicalize(ancestor) {
            let mut joined = canonical;
            for component in suffix.iter().rev() {
                joined.push(component);
            }
            return Some(lexical(&joined));
        }
        suffix.push(ancestor.file_name()?.to_owned());
        ancestor = parent;
    }
    Some(lexical(path))
}

fn lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

enum Listed {
    Directory(PathBuf),
    File(PathBuf),
}

struct Listing {
    stamp: Stamp,
    entries: Vec<Listed>,
}

struct Root {
    files: BTreeSet<PathBuf>,
    diagnostics: Vec<String>,
    changed_at: u64,
}

struct Evaluation {
    generation: u64,
    launch: FactoryLaunch,
    liveness: Liveness,
    root: Option<PathBuf>,
    stored: Option<PathBuf>,
}

const NO_FILES: &BTreeSet<PathBuf> = &BTreeSet::new();

pub(super) fn liveness_of(
    launch: &FactoryLaunch,
    liveness: &BTreeMap<LaunchId, Liveness>,
) -> Liveness {
    liveness.get(&launch.id).cloned().unwrap_or_else(|| {
        launch
            .process_exit
            .map(Liveness::Exited)
            .unwrap_or(Liveness::Unverified)
    })
}

fn candidates<'a>(
    root: &'a BTreeSet<PathBuf>,
    stored: Option<&'a PathBuf>,
) -> impl Iterator<Item = &'a PathBuf> {
    let extra = stored.filter(|path| !root.contains(*path));
    let before = match extra {
        Some(path) => root.range::<PathBuf, _>((Bound::Unbounded, Bound::Excluded(path))),
        None => root.range::<PathBuf, _>(..),
    };
    let after =
        extra.map(|path| root.range::<PathBuf, _>((Bound::Excluded(path), Bound::Unbounded)));
    before.chain(extra).chain(after.into_iter().flatten())
}

pub(super) struct EventTracker {
    files: BTreeMap<PathBuf, EventFile>,
    roots: BTreeMap<PathBuf, Root>,
    listings: BTreeMap<PathBuf, Listing>,
    stored: BTreeMap<PathBuf, PathBuf>,
    evaluated: BTreeMap<LaunchId, Evaluation>,
    generation: u64,
}

impl EventTracker {
    pub(super) fn new() -> Self {
        Self {
            files: BTreeMap::new(),
            roots: BTreeMap::new(),
            listings: BTreeMap::new(),
            stored: BTreeMap::new(),
            evaluated: BTreeMap::new(),
            generation: 0,
        }
    }

    pub(super) fn refresh(&mut self) {
        for file in self.files.values_mut() {
            file.replay = true;
        }
        self.listings.clear();
        self.stored.clear();
        self.evaluated.clear();
    }

    pub(super) fn observe<'a>(&mut self, launches: impl IntoIterator<Item = &'a FactoryLaunch>) {
        self.generation += 1;
        let generation = self.generation;
        let mut visited = BTreeSet::new();
        let mut roots = BTreeSet::new();
        let mut stored = BTreeMap::new();
        let mut launch_ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for launch in launches {
            launch_ids.insert(launch.id);
            if let Ok(spec) = FactoryLaunchSpec::deserialize(&launch.descriptor) {
                let root = spec.data_root.join("events");
                if !roots.contains(&root) {
                    let mut found = BTreeSet::new();
                    let mut diagnostics = Vec::new();
                    self.discover(&root, &mut found, &mut diagnostics, &mut visited);
                    paths.extend(found.iter().cloned());
                    match self.roots.get_mut(&root) {
                        Some(state) if state.files == found && state.diagnostics == diagnostics => {
                        }
                        Some(state) => {
                            state.files = found;
                            state.diagnostics = diagnostics;
                            state.changed_at = generation;
                        }
                        None => {
                            self.roots.insert(
                                root.clone(),
                                Root {
                                    files: found,
                                    diagnostics,
                                    changed_at: generation,
                                },
                            );
                        }
                    }
                    roots.insert(root);
                }
            }
            if let Some(path) = &launch.event_path {
                let normalized = self
                    .stored
                    .get(path)
                    .cloned()
                    .unwrap_or_else(|| normalized(path).unwrap_or_else(|| path.clone()));
                paths.insert(normalized.clone());
                stored.insert(path.clone(), normalized);
            }
        }
        self.stored = stored;
        self.listings.retain(|path, _| visited.contains(path));
        self.roots.retain(|root, _| roots.contains(root));
        self.evaluated.retain(|id, _| launch_ids.contains(id));
        for path in &paths {
            self.files
                .entry(path.clone())
                .or_insert_with(|| EventFile {
                    changed_at: generation,
                    ..EventFile::default()
                })
                .update(path, generation);
        }
        self.files.retain(|path, _| paths.contains(path));
        for root in self.roots.values_mut() {
            root.changed_at = root
                .files
                .iter()
                .filter_map(|path| self.files.get(path))
                .map(|file| file.changed_at)
                .fold(root.changed_at, u64::max);
        }
    }

    fn discover(
        &mut self,
        directory: &Path,
        paths: &mut BTreeSet<PathBuf>,
        diagnostics: &mut Vec<String>,
        visited: &mut BTreeSet<PathBuf>,
    ) {
        visited.insert(directory.to_path_buf());
        let stamp = fs::metadata(directory)
            .ok()
            .map(|metadata| Stamp::of(&metadata));
        if let Some(stamp) = stamp
            && let Some(listing) = self.listings.remove(directory)
            && listing.stamp == stamp
        {
            for entry in &listing.entries {
                match entry {
                    Listed::Directory(path) => self.discover(path, paths, diagnostics, visited),
                    Listed::File(path) => {
                        paths.insert(path.clone());
                    }
                }
            }
            self.listings.insert(directory.to_path_buf(), listing);
            return;
        }
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                if diagnostics.len() < RECENT_ENTRIES {
                    diagnostics.push(format!(
                        "Event directory unavailable: {}: {error}",
                        directory.display()
                    ));
                }
                return;
            }
        };
        let mut base = None;
        let mut listed = Vec::new();
        let mut complete = true;
        for entry in entries {
            let result = entry.and_then(|entry| Ok((entry.path(), entry.file_type()?)));
            match result {
                Ok((path, kind)) if kind.is_dir() => {
                    self.discover(&path, paths, diagnostics, visited);
                    listed.push(Listed::Directory(path));
                }
                Ok((path, kind))
                    if kind.is_file()
                        && path
                            .extension()
                            .is_some_and(|extension| extension == "jsonl")
                        && !path.file_name().is_some_and(|name| {
                            name.to_string_lossy().eq_ignore_ascii_case("host.jsonl")
                        }) =>
                {
                    let base = base.get_or_insert_with(|| normalized(directory));
                    let path = match (base.as_ref(), path.file_name()) {
                        (Some(base), Some(name)) => base.join(name),
                        _ => path,
                    };
                    paths.insert(path.clone());
                    listed.push(Listed::File(path));
                }
                Ok(_) => {}
                Err(error) => {
                    complete = false;
                    if diagnostics.len() < RECENT_ENTRIES {
                        diagnostics.push(format!(
                            "Cannot discover events in {}: {error}",
                            directory.display()
                        ));
                    }
                }
            }
        }
        if let Some(stamp) = stamp
            && complete
            && stamp.settled()
        {
            self.listings.insert(
                directory.to_path_buf(),
                Listing {
                    stamp,
                    entries: listed,
                },
            );
        }
    }

    pub(super) fn unchanged(
        &self,
        launch: &FactoryLaunch,
        liveness: &BTreeMap<LaunchId, Liveness>,
    ) -> bool {
        let Some(evaluation) = self.evaluated.get(&launch.id) else {
            return false;
        };
        evaluation.launch == *launch
            && evaluation.liveness == liveness_of(launch, liveness)
            && evaluation.root.as_ref().is_none_or(|root| {
                self.roots
                    .get(root)
                    .is_some_and(|root| root.changed_at <= evaluation.generation)
            })
            && evaluation.stored.as_ref().is_none_or(|path| {
                self.files
                    .get(path)
                    .is_some_and(|file| file.changed_at <= evaluation.generation)
            })
    }

    pub(super) fn snapshot(
        &mut self,
        launch: &FactoryLaunch,
        liveness: &BTreeMap<LaunchId, Liveness>,
    ) -> FactorySnapshot {
        let spec = FactoryLaunchSpec::deserialize(&launch.descriptor);
        let stored = launch.event_path.as_ref().map(|path| {
            self.stored
                .get(path)
                .cloned()
                .unwrap_or_else(|| normalized(path).unwrap_or_else(|| path.clone()))
        });
        let root = spec.as_ref().ok().map(|spec| spec.data_root.join("events"));
        let live = liveness_of(launch, liveness);
        let snapshot = self.project(launch, live.clone(), spec, root.as_ref(), stored.as_ref());
        self.evaluated.insert(
            launch.id,
            Evaluation {
                generation: self.generation,
                launch: launch.clone(),
                liveness: live,
                root,
                stored,
            },
        );
        snapshot
    }

    fn project(
        &self,
        launch: &FactoryLaunch,
        live: Liveness,
        spec: serde_json::Result<FactoryLaunchSpec>,
        root: Option<&PathBuf>,
        stored: Option<&PathBuf>,
    ) -> FactorySnapshot {
        let mut snapshot = FactorySnapshot {
            launch: launch.clone(),
            reported_state: ReportedState::Pending,
            liveness: live,
            title: None,
            tasks: BTreeMap::new(),
            notes: Vec::new(),
            artifacts: Vec::new(),
            transcripts: Vec::new(),
            last_seen: None,
            diagnostics: Vec::new(),
            correlation_candidates: Vec::new(),
            details: Value::Object(Map::new()),
        };
        if let Some(error) = &launch.launch_error {
            snapshot.diagnostics.push(format!("Launch error: {error}"));
        }
        let spec = match spec {
            Ok(spec) if spec.id == launch.id && spec.project_id == launch.project_id => spec,
            Ok(_) => {
                snapshot.diagnostics.push(
                    "Launch descriptor identity does not match its durable record; outcome unknown"
                        .into(),
                );
                return snapshot;
            }
            Err(error) => {
                snapshot.diagnostics.push(format!(
                    "Unsupported or malformed launch descriptor; outcome unknown: {error}"
                ));
                return snapshot;
            }
        };
        let Some(log_dir) = normalized(&spec.log_dir) else {
            snapshot
                .diagnostics
                .push("Launch log directory is not absolute; correlation unavailable".into());
            return snapshot;
        };
        let (root_paths, errors) = root
            .and_then(|root| self.roots.get(root))
            .map_or((NO_FILES, &[][..]), |root| {
                (&root.files, &root.diagnostics[..])
            });
        snapshot.diagnostics.extend(errors.iter().cloned());
        let matches: Vec<_> = candidates(root_paths, stored)
            .filter(|path| {
                self.files.get(*path).is_some_and(|file| {
                    file.log_paths
                        .iter()
                        .any(|path| *path != log_dir && path.starts_with(&log_dir))
                })
            })
            .cloned()
            .collect();
        if matches.len() > 1 {
            snapshot.diagnostics.push("Ambiguous Factory event correlation: multiple run-started logPath values match this launch; no run selected".into());
            snapshot.correlation_candidates = matches;
            return snapshot;
        }
        let Some(path) = matches.first() else {
            snapshot.diagnostics.push(
                "No event log matches this launch's exact log directory; launch outcome unknown"
                    .into(),
            );
            for candidate in candidates(root_paths, stored) {
                if let Some(file) = self.files.get(candidate) {
                    for diagnostic in &file.projection.diagnostics {
                        if snapshot.diagnostics.len() >= RECENT_ENTRIES {
                            break;
                        }
                        snapshot.diagnostics.push(diagnostic.clone());
                    }
                }
            }
            if let Some(path) = stored {
                if let Some(error) = self
                    .files
                    .get(path)
                    .and_then(|file| file.unavailable.as_ref())
                {
                    snapshot.diagnostics.push(error.clone());
                } else {
                    snapshot.diagnostics.push(format!(
                        "Recorded event history cannot establish correlation: {}",
                        path.display()
                    ));
                }
            }
            return snapshot;
        };
        let file = &self.files[path];
        let projection = &file.projection;
        snapshot.launch.event_path = Some(path.clone());
        snapshot.launch.work_item_id = projection.story_id;
        snapshot.launch.attempt_id = projection.attempt_id.clone();
        snapshot.reported_state = projection.state.clone();
        snapshot.title = projection.title.clone();
        snapshot.tasks = projection.tasks.clone();
        snapshot.notes = projection.notes.iter().cloned().collect();
        snapshot.artifacts = projection.artifacts.iter().cloned().collect();
        snapshot.transcripts = projection.transcripts.iter().cloned().collect();
        snapshot.last_seen = projection.last_seen.clone();
        snapshot.details = Value::Object(projection.details.clone());
        snapshot
            .diagnostics
            .extend(projection.diagnostics.iter().cloned());
        if let Some(error) = &file.unavailable {
            snapshot.diagnostics.push(error.clone());
        }
        for transcript in &snapshot.transcripts {
            if !transcript.is_file() && snapshot.diagnostics.len() < RECENT_ENTRIES * 2 {
                snapshot
                    .diagnostics
                    .push(format!("Transcript unavailable: {}", transcript.display()));
            }
        }
        snapshot
    }
}
