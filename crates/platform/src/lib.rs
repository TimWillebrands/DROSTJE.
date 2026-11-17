use anyhow::{Context, Result, bail, ensure};
use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

mod shell_env;
pub use shell_env::import_login_shell_environment;
#[cfg(test)]
mod git_tests;
mod unix;
pub use unix::{
    PeerCredentials, WakePipe, WriteReadiness, connect_unix_nonblocking, effective_uid,
    kill_process_group, new_session, peer_credentials, poll_writable, session_id, set_nonblocking,
    terminate_process_group, wait_until_ready,
};

#[cfg(any(feature = "desktop", test))]
#[cfg_attr(
    not(any(target_os = "macos", test)),
    expect(
        dead_code,
        reason = "only AppKit consumes the icon at runtime; tests decode it everywhere"
    )
)]
const APPLICATION_ICON: &[u8] = include_bytes!("../../../assets/drostje-app-icon.png");

#[cfg(all(target_os = "macos", feature = "desktop"))]
pub fn set_application_icon() -> Result<()> {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let main_thread =
        MainThreadMarker::new().context("Set the application icon on the main thread")?;
    let data = NSData::with_bytes(APPLICATION_ICON);
    let icon = NSImage::initWithData(NSImage::alloc(), &data)
        .context("Decode the bundled application icon")?;
    let application = NSApplication::sharedApplication(main_thread);
    // SAFETY: AppKit accepts a valid, non-null NSImage here and retains it for the application.
    unsafe { application.setApplicationIconImage(Some(&icon)) };
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "desktop")))]
pub fn set_application_icon() -> Result<()> {
    Ok(())
}

pub struct IdleSleepAssertion {
    #[cfg(target_os = "macos")]
    id: u32,
}

impl IdleSleepAssertion {
    pub fn acquire(reason: &str) -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                id: idle_sleep::acquire(reason)?,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = reason;
            Ok(Self {})
        }
    }
}

impl Drop for IdleSleepAssertion {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        idle_sleep::release(self.id);
    }
}

#[cfg(target_os = "macos")]
mod idle_sleep {
    use anyhow::{Context, Result, ensure};
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: CFTypeRef,
            level: u32,
            name: CFTypeRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            allocator: CFTypeRef,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external: u8,
        ) -> CFTypeRef;
        fn CFRelease(value: CFTypeRef);
    }

    struct CFString(CFTypeRef);

    impl CFString {
        fn new(value: &str) -> Result<Self> {
            let length = value
                .len()
                .try_into()
                .context("Power assertion name too long")?;
            // SAFETY: bytes are valid UTF-8 and live for this copying call; NULL selects
            // the default allocator. 0x08000100 is kCFStringEncodingUTF8.
            let string = unsafe {
                CFStringCreateWithBytes(std::ptr::null(), value.as_ptr(), length, 0x08000100, 0)
            };
            ensure!(!string.is_null(), "Allocate power assertion string");
            Ok(Self(string))
        }
    }

    impl Drop for CFString {
        fn drop(&mut self) {
            // SAFETY: this is the non-null, owned reference returned by Create.
            unsafe { CFRelease(self.0) };
        }
    }

    pub(super) fn acquire(reason: &str) -> Result<u32> {
        let kind = CFString::new("PreventUserIdleSystemSleep")?;
        let name = CFString::new(reason)?;
        let mut id = 0;
        // SAFETY: both CFStrings remain alive for the call, and id is writable.
        // 255 is kIOPMAssertionLevelOn; IOKit owns copies of the strings on success.
        let result = unsafe { IOPMAssertionCreateWithName(kind.0, 255, name.0, &mut id) };
        ensure!(
            result == 0,
            "Prevent macOS idle sleep: IOKit error {result:#x}"
        );
        Ok(id)
    }

    pub(super) fn release(id: u32) {
        // SAFETY: id is owned by one non-cloneable IdleSleepAssertion and released once.
        let result = unsafe { IOPMAssertionRelease(id) };
        if result != 0 {
            eprintln!("Release macOS idle sleep protection: IOKit error {result:#x}");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::IdleSleepAssertion;

        unsafe extern "C" {
            fn IOPMAssertionCopyProperties(id: u32) -> CFTypeRef;
            fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
            fn CFEqual(left: CFTypeRef, right: CFTypeRef) -> u8;
        }

        fn prevents_idle_system_sleep(id: u32) -> bool {
            // SAFETY: IOKit accepts any assertion ID and returns NULL for released IDs.
            let properties = unsafe { IOPMAssertionCopyProperties(id) };
            if properties.is_null() {
                return false;
            }
            let key = CFString::new("AssertType").unwrap();
            let expected = CFString::new("PreventUserIdleSystemSleep").unwrap();
            // SAFETY: properties is an owned dictionary; key and expected are live
            // CFStrings. The borrowed dictionary value is compared before release.
            unsafe {
                let kind = CFDictionaryGetValue(properties, key.0);
                let matches = !kind.is_null() && CFEqual(kind, expected.0) != 0;
                CFRelease(properties);
                matches
            }
        }

        #[test]
        fn overlapping_work_releases_only_its_own_sleep_assertion() {
            let first = IdleSleepAssertion::acquire("DROSTJE. power lifecycle test").unwrap();
            let second = IdleSleepAssertion::acquire("DROSTJE. power lifecycle test").unwrap();
            let first_id = first.id;
            let second_id = second.id;
            assert!(prevents_idle_system_sleep(first_id));
            assert!(prevents_idle_system_sleep(second_id));
            drop(first);
            assert!(!prevents_idle_system_sleep(first_id));
            assert!(prevents_idle_system_sleep(second_id));
            drop(second);
            assert!(!prevents_idle_system_sleep(second_id));
        }
    }
}

pub fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not configured")
}
fn absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(env::current_dir()?.join(path))
    }
}
pub fn data_dir() -> Result<PathBuf> {
    absolute(match env::var_os("DROSTJE_DATA_DIR") {
        Some(p) => p.into(),
        None => default_data_dir()?,
    })
}
#[cfg(target_os = "macos")]
fn default_data_dir() -> Result<PathBuf> {
    Ok(home()?.join("Library/Application Support/Drostje"))
}
#[cfg(not(target_os = "macos"))]
fn default_data_dir() -> Result<PathBuf> {
    Ok(match env::var_os("XDG_DATA_HOME") {
        Some(p) => PathBuf::from(p).join("drostje"),
        None => home()?.join(".local/share/drostje"),
    })
}
pub fn factory_data_root() -> Result<PathBuf> {
    absolute(if let Some(p) = env::var_os("FACTORY_DATA_ROOT") {
        p.into()
    } else if let Some(p) = env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(p).join("compliance-software-factory")
    } else {
        home()?.join(".cache/compliance-software-factory")
    })
}
fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
pub fn resolve_executable(name: &str, explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(value) = explicit.filter(|s| !s.trim().is_empty()) {
        let path = if let Some(rest) = value.strip_prefix("~/") {
            home()?.join(rest)
        } else {
            absolute(PathBuf::from(value))?
        };
        ensure!(
            executable(&path),
            "Configured {name} executable is missing or not executable: {}",
            path.display()
        );
        return fs::canonicalize(&path).with_context(|| format!("Resolve {}", path.display()));
    }
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|p| env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home()?.join(".local/bin"),
    ]);
    for dir in dirs {
        let path = dir.join(name);
        if executable(&path) {
            return fs::canonicalize(path).context("Resolve executable");
        }
    }
    bail!(
        "{name} was not found in PATH or standard installation locations; configure it in Settings"
    )
}
#[derive(Clone, Debug)]
pub struct RepositoryInfo {
    pub root: PathBuf,
    pub branch: Option<String>,
    pub branches: Vec<String>,
    pub dirty: bool,
    verified_branches: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryDiff {
    pub head_branch: Option<String>,
    pub patch: String,
    pub changed_files: usize,
    pub additions: usize,
    pub deletions: usize,
    pub truncated: bool,
    pub rows: std::sync::Arc<drostje_diff::Patch>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkingTreePatch {
    pub head_branch: Option<String>,
    pub patch: String,
    pub changed_files: usize,
    pub additions: usize,
    pub deletions: usize,
    pub truncated: bool,
}

const MAX_DIFF_BYTES: usize = 512 * 1024;
const MAX_UNTRACKED_FILE_BYTES: usize = 128 * 1024;

pub fn working_tree_diff(root: &Path) -> Result<RepositoryDiff> {
    let WorkingTreePatch {
        head_branch,
        patch,
        changed_files,
        additions,
        deletions,
        truncated,
    } = working_tree_patch(root)?;
    let rows = std::sync::Arc::new(drostje_diff::parse(&patch));
    Ok(RepositoryDiff {
        head_branch,
        rows,
        patch,
        changed_files,
        additions,
        deletions,
        truncated,
    })
}

pub fn working_tree_patch(root: &Path) -> Result<WorkingTreePatch> {
    let (status, status_truncated) = git_bounded(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        MAX_DIFF_BYTES,
    )?;
    ensure!(
        !status_truncated,
        "Git status exceeds 512 KiB; narrow the checkout before reading Changes"
    );
    let entries = status_entries(&status);
    let tracked = git_bounded(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--unified=3",
            "HEAD",
            "--",
        ],
        MAX_DIFF_BYTES,
    );
    let (mut patch, mut truncated) = match tracked {
        Ok(patch) => patch,
        Err(error) if git_bounded(root, &["rev-parse", "--verify", "HEAD"], 4096).is_ok() => {
            return Err(error);
        }
        Err(_) => {
            let (mut staged, mut truncated) = git_bounded(
                root,
                &[
                    "diff",
                    "--cached",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    "--unified=3",
                    "--",
                ],
                MAX_DIFF_BYTES,
            )?;
            if !truncated {
                if !staged.is_empty() && staged.len() < MAX_DIFF_BYTES {
                    staged.push('\n');
                }
                let (unstaged, cut) = git_bounded(
                    root,
                    &[
                        "diff",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--no-color",
                        "--unified=3",
                        "--",
                    ],
                    MAX_DIFF_BYTES.saturating_sub(staged.len()),
                )?;
                staged.push_str(&unstaged);
                truncated = cut;
            }
            (staged, truncated)
        }
    };
    for (status, path) in &entries {
        if truncated {
            break;
        }
        if *status == "??" {
            truncated = append_untracked_patch(root, path, &mut patch)?;
        }
    }
    if truncated {
        patch.push_str("\n\nDiff truncated at 512 KiB.");
    }
    let (additions, deletions) = count_changed_lines(&patch);
    let head_branch = git_bounded(root, &["symbolic-ref", "--quiet", "--short", "HEAD"], 4096)
        .ok()
        .filter(|(_, truncated)| !truncated)
        .map(|(branch, _)| branch.trim_end_matches(['\n', '\r']).to_owned());
    Ok(WorkingTreePatch {
        head_branch,
        patch,
        changed_files: entries.len(),
        additions,
        deletions,
        truncated,
    })
}

fn count_changed_lines(patch: &str) -> (usize, usize) {
    patch.lines().fold((0, 0), |(additions, deletions), line| {
        if line.starts_with('+') && !line.starts_with("+++") {
            (additions + 1, deletions)
        } else if line.starts_with('-') && !line.starts_with("---") {
            (additions, deletions + 1)
        } else {
            (additions, deletions)
        }
    })
}

fn status_entries(status: &str) -> Vec<(&str, &str)> {
    let mut fields = status.split('\0').filter(|field| !field.is_empty());
    let mut entries = Vec::new();
    while let Some(field) = fields.next() {
        if field.len() < 3 {
            continue;
        }
        let state = &field[..2];
        let path = &field[3..];
        entries.push((state, path));
        if state.contains('R') || state.contains('C') {
            let _ = fields.next();
        }
    }
    entries
}

fn append_untracked_patch(root: &Path, path: &str, patch: &mut String) -> Result<bool> {
    use std::io::Read;
    let relative = Path::new(path);
    ensure!(
        !relative.is_absolute()
            && !relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir)),
        "Git returned an invalid untracked path"
    );
    let full = root.join(relative);
    let metadata = match fs::symlink_metadata(&full) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Read untracked file {}", relative.display()));
        }
    };
    let root = fs::canonicalize(root)?;
    let parent = fs::canonicalize(full.parent().context("Untracked file has no parent")?)?;
    ensure!(
        parent.starts_with(&root),
        "Untracked file parent is outside the checkout"
    );
    let symlink = metadata.file_type().is_symlink();
    let mode = if symlink { "120000" } else { "100644" };
    let header = format!(
        "\ndiff --git a/{path} b/{path}\nnew file mode {mode}\n--- /dev/null\n+++ b/{path}\n"
    );
    if append_diff_text(patch, &header) {
        return Ok(true);
    }
    if !symlink && !metadata.is_file() {
        return Ok(append_diff_text(
            patch,
            "Special untracked file; content omitted.\n",
        ));
    }
    if !symlink && metadata.len() > MAX_UNTRACKED_FILE_BYTES as u64 {
        return Ok(append_diff_text(
            patch,
            "Untracked file is larger than 128 KiB; content omitted.\n",
        ));
    }
    let bytes = if symlink {
        use std::os::unix::ffi::OsStrExt;
        fs::read_link(&full)?.as_os_str().as_bytes().to_vec()
    } else {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&full)?;
        let mut bytes = Vec::new();
        file.take((MAX_UNTRACKED_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        bytes
    };
    if bytes.len() > MAX_UNTRACKED_FILE_BYTES {
        return Ok(append_diff_text(
            patch,
            "Untracked file is larger than 128 KiB; content omitted.\n",
        ));
    }
    if let Ok(text) = std::str::from_utf8(&bytes) {
        if append_diff_text(patch, &format!("@@ -0,0 +1,{} @@\n", text.lines().count())) {
            return Ok(true);
        }
        for line in text.lines() {
            if patch.len() + line.len() + 2 > MAX_DIFF_BYTES {
                return Ok(true);
            }
            patch.push('+');
            patch.push_str(line);
            patch.push('\n');
        }
        Ok(false)
    } else {
        Ok(append_diff_text(
            patch,
            "Binary untracked file; content omitted.\n",
        ))
    }
}

fn append_diff_text(patch: &mut String, text: &str) -> bool {
    let available = MAX_DIFF_BYTES.saturating_sub(patch.len());
    let mut end = text.len().min(available);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    patch.push_str(&text[..end]);
    end != text.len()
}

fn git_bounded(root: &Path, args: &[&str], limit: usize) -> Result<(String, bool)> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = git_command(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Run Git")?;
    let stdout = child.stdout.take().context("Git stdout missing")?;
    let stderr = child.stderr.take().context("Git stderr missing")?;
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let mut bytes = Vec::new();
    let read = stdout.take((limit + 1) as u64).read_to_end(&mut bytes);
    let truncated = bytes.len() > limit;
    if truncated || read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().context("Wait for Git")?;
    let errors = errors
        .join()
        .map_err(|_| anyhow::anyhow!("Git stderr reader stopped"))??;
    read.context("Read Git output")?;
    ensure!(
        truncated || status.success(),
        "Git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&errors).trim()
    );
    if truncated {
        bytes.truncate(limit);
        if let Err(error) = std::str::from_utf8(&bytes)
            && error.error_len().is_none()
        {
            bytes.truncate(error.valid_up_to());
        }
    }
    Ok((
        String::from_utf8(bytes).context("Git returned non-UTF-8 text")?,
        truncated,
    ))
}
#[cfg(test)]
thread_local! {
    static GIT_SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn git_command(root: &Path) -> Command {
    #[cfg(test)]
    GIT_SPAWNS.with(|spawns| spawns.set(spawns.get() + 1));
    let mut command = Command::new("git");
    command.arg("-C").arg(root);
    command
}

fn git_output(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    git_command(root).args(args).output().context("Run Git")
}

pub fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = git_output(root, args)?;
    ensure!(
        output.status.success(),
        "Git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)
        .context("Git returned non-UTF-8 text")?
        .trim_end_matches(['\n', '\r'])
        .to_owned())
}
fn git_prints_anything(root: &Path, args: &[&str]) -> Result<bool> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = git_command(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Run Git")?;
    let mut stdout = child.stdout.take().context("Run Git")?;
    let mut stderr = child.stderr.take().context("Run Git")?;
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let mut first = [0; 1];
    let read = loop {
        match stdout.read(&mut first) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            read => break read,
        }
    };
    drop(stdout);
    let status = child.wait().context("Run Git")?;
    let errors = errors
        .join()
        .map_err(|_| anyhow::anyhow!("Git stderr reader stopped"))?
        .context("Run Git")?;
    let printed = read.context("Run Git")? > 0;
    ensure!(
        status.success() || (printed && status.code().is_none()),
        "Git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&errors).trim()
    );
    Ok(printed)
}

fn has_changes(root: &Path) -> Result<bool> {
    git_prints_anything(root, &["status", "--porcelain"])
}

pub fn inspect_repository(path: &Path) -> Result<RepositoryInfo> {
    let path =
        fs::canonicalize(path).with_context(|| format!("Read project {}", path.display()))?;
    ensure!(path.is_dir(), "Project is not a directory");
    let root = fs::canonicalize(work_tree_root(&path)?)?;
    fs::read_dir(&root).context("Project root is unreadable")?;
    let listing = local_branches(&root)?;
    let branch = match listing.head {
        Some(branch) => Some(branch),
        None => git(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok(),
    };
    let dirty = has_changes(&root)?;
    Ok(RepositoryInfo {
        root,
        branch,
        branches: listing.branches,
        dirty,
        verified_branches: listing.verified,
    })
}

fn work_tree_root(path: &Path) -> Result<String> {
    if let Ok(output) = git_output(
        path,
        &["rev-parse", "--is-bare-repository", "--show-toplevel"],
    ) && output.status.success()
        && let Some(toplevel) = output.stdout.strip_prefix(b"false\n")
        && let Ok(toplevel) = std::str::from_utf8(toplevel)
    {
        return Ok(toplevel.trim_end_matches(['\n', '\r']).to_owned());
    }
    ensure!(
        git(path, &["rev-parse", "--is-bare-repository"])? == "false",
        "Bare repositories cannot be registered as projects"
    );
    git(path, &["rev-parse", "--show-toplevel"])
}

struct BranchListing {
    branches: Vec<String>,
    head: Option<String>,
    verified: Vec<String>,
}

fn local_branches(root: &Path) -> Result<BranchListing> {
    const FORMAT: &str =
        "--format=%(refname:short)%00%(HEAD)%00%(symref)%00%(refname:lstrip=2)%00%(objecttype)";
    if let Ok(listing) = git(root, &["for-each-ref", FORMAT, "refs/heads"])
        && let Some(listing) = parse_branch_listing(&listing)
    {
        return Ok(listing);
    }
    let branches = git(
        root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?
    .lines()
    .map(str::to_owned)
    .collect();
    Ok(BranchListing {
        branches,
        head: None,
        verified: Vec::new(),
    })
}

fn parse_branch_listing(listing: &str) -> Option<BranchListing> {
    let mut branches = Vec::new();
    let mut verified = Vec::new();
    let mut head = None;
    let mut any_symref = false;
    for line in listing.lines() {
        let mut fields = line.split('\0');
        let (Some(short), Some(marker), Some(symref), Some(name), Some(kind), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return None;
        };
        any_symref |= !symref.is_empty();
        if marker == "*" && short == name {
            head = Some(name.to_owned());
        }
        if kind == "commit" && obviously_valid_branch_name(name) {
            verified.push(name.to_owned());
        }
        branches.push(short.to_owned());
    }
    if any_symref {
        head = None;
    }
    Some(BranchListing {
        branches,
        head,
        verified,
    })
}

impl RepositoryInfo {
    pub fn verified_branch(&self, branch: &str) -> bool {
        self.verified_branches.iter().any(|name| name == branch)
    }
}

fn obviously_valid_branch_name(name: &str) -> bool {
    name != "HEAD"
        && !name.starts_with('-')
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
        && name.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with('.')
                && !part.ends_with(".lock")
        })
}

fn check_branch_name(root: &Path, branch: &str) -> Result<()> {
    git(root, &["check-ref-format", "--branch", branch])?;
    Ok(())
}

pub fn validate_branch(root: &Path, branch: &str) -> Result<()> {
    ensure!(!branch.trim().is_empty(), "Choose a target branch");
    let obviously_valid = obviously_valid_branch_name(branch);
    if !obviously_valid {
        check_branch_name(root, branch)?;
    }
    if let Err(error) = git(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
    ) {
        if obviously_valid {
            check_branch_name(root, branch)?;
        }
        return Err(error);
    }
    Ok(())
}
fn git_exit(root: &Path, args: &[&str]) -> Result<i32> {
    let output = git_output(root, args)?;
    output
        .status
        .code()
        .with_context(|| format!("Git {} was terminated by a signal", args.join(" ")))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeStatus {
    pub exists: bool,
    pub clean: bool,
    pub head_branch: Option<String>,
    pub merged: bool,
}

pub fn create_worktree(
    project_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<PathBuf> {
    validate_branch(project_root, base)?;
    git(project_root, &["check-ref-format", "--branch", branch])?;
    ensure!(!path.exists(), "{} already exists", path.display());
    let parent = path
        .parent()
        .context("Worktree path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("Create {}", parent.display()))?;
    let path_arg = path.to_str().context("Worktree path is not valid UTF-8")?;
    git(project_root, &["worktree", "prune"])?;
    git(
        project_root,
        &["worktree", "add", "-b", branch, path_arg, base],
    )?;
    fs::canonicalize(path).with_context(|| format!("Read worktree {}", path.display()))
}

pub fn worktree_head_branch(path: &Path) -> Result<Option<String>> {
    let toplevel = fs::canonicalize(git(path, &["rev-parse", "--show-toplevel"])?)?;
    ensure!(
        toplevel == fs::canonicalize(path)?,
        "{} is not a working-tree root",
        path.display()
    );
    Ok(git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok())
}

pub fn branch_exists(root: &Path, branch: &str) -> Result<bool> {
    match git_exit(
        root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )? {
        0 => Ok(true),
        1 => Ok(false),
        code => bail!("Git show-ref exited with {code}"),
    }
}

pub fn branch_has_upstream(root: &Path, branch: &str) -> Result<bool> {
    match git_exit(
        root,
        &["config", "--get", &format!("branch.{branch}.remote")],
    )? {
        0 => Ok(true),
        1 => Ok(false),
        code => bail!("Git config exited with {code}"),
    }
}

pub fn branch_is_merged(root: &Path, branch: &str, base: &str) -> Result<bool> {
    let both_exist = obviously_valid_branch_name(base)
        && obviously_valid_branch_name(branch)
        && git_exit(
            root,
            &[
                "rev-parse",
                &format!("refs/heads/{base}^{{commit}}"),
                &format!("refs/heads/{branch}^{{commit}}"),
                "--",
            ],
        )
        .is_ok_and(|code| code == 0);
    if !both_exist {
        validate_branch(root, base)?;
        validate_branch(root, branch)?;
    }
    match git_exit(root, &["merge-base", "--is-ancestor", branch, base])? {
        0 => Ok(true),
        1 => Ok(false),
        code => bail!("Git merge-base exited with {code}"),
    }
}

pub fn switch_branch(root: &Path, branch: &str, create: bool) -> Result<()> {
    ensure!(
        !has_changes(root)?,
        "The working tree has uncommitted changes; commit or stash them first"
    );
    git(root, &["check-ref-format", "--branch", branch])?;
    if create {
        git(root, &["switch", "-c", branch])?;
    } else {
        git(root, &["switch", branch])?;
    }
    Ok(())
}

pub fn commit_all(root: &Path, message: &str) -> Result<String> {
    let message = message.trim();
    ensure!(!message.is_empty(), "Write a commit message first");
    ensure!(
        has_changes(root)?,
        "Nothing to commit; the working tree is clean"
    );
    git(root, &["add", "--all"])?;
    git(root, &["commit", "--quiet", "-m", message])?;
    git(root, &["rev-parse", "--short", "HEAD"])
}

pub fn rename_branch(worktree: &Path, old: &str, new: &str) -> Result<()> {
    ensure!(
        worktree_head_branch(worktree)?.as_deref() == Some(old),
        "The worktree is no longer on {old}"
    );
    ensure!(
        !branch_has_upstream(worktree, old)?,
        "Branch {old} already has an upstream"
    );
    git(worktree, &["check-ref-format", "--branch", new])?;
    git(worktree, &["branch", "-m", old, new])?;
    Ok(())
}

pub fn inspect_worktree(
    project_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<WorktreeStatus> {
    if !path.is_dir() {
        return Ok(WorktreeStatus::default());
    }
    let head_branch = worktree_head_branch(path)?;
    let clean = !has_changes(path)?;
    let merged = branch_is_merged(project_root, branch, base)?;
    Ok(WorktreeStatus {
        exists: true,
        clean,
        head_branch,
        merged,
    })
}

pub fn remove_worktree(project_root: &Path, path: &Path, force: bool) -> Result<()> {
    let path_arg = path.to_str().context("Worktree path is not valid UTF-8")?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(path_arg);
    git(project_root, &args)?;
    Ok(())
}

pub fn prune_worktrees(project_root: &Path) -> Result<()> {
    git(project_root, &["worktree", "prune"])?;
    Ok(())
}

pub fn delete_branch(project_root: &Path, branch: &str) -> Result<()> {
    git(project_root, &["branch", "-d", branch])?;
    Ok(())
}

#[cfg(unix)]
pub fn host_name() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: the pointer and length describe `buffer`, which stays alive and writable for the call.
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    let name = String::from_utf8_lossy(&buffer[..end]);
    let name = name.trim().trim_end_matches(".local").trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(not(unix))]
pub fn host_name() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
}

pub fn now() -> String {
    timestamp(std::time::SystemTime::now())
}

pub fn timestamp_before(age: std::time::Duration) -> String {
    let now = std::time::SystemTime::now();
    timestamp(now.checked_sub(age).unwrap_or(now))
}

fn timestamp(time: std::time::SystemTime) -> String {
    let seconds = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: gmtime_r writes only the provided tm and uses no process-global time buffer;
    // an all-zero libc::tm (integers and a null zone pointer) is a valid value if it fails.
    let tm = unsafe {
        libc::gmtime_r(&seconds, tm.as_mut_ptr());
        tm.assume_init()
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_name_is_a_trimmed_name_without_the_local_suffix() {
        if let Some(name) = super::host_name() {
            assert!(!name.is_empty());
            assert!(!name.ends_with(".local"));
            assert_eq!(name.trim(), name);
        }
    }

    use super::{
        APPLICATION_ICON, count_changed_lines, timestamp, timestamp_before, working_tree_diff,
        working_tree_patch,
    };
    use std::{fs, path::Path, process::Command};

    #[test]
    fn timestamps_format_utc_and_order_as_strings() {
        let day = std::time::Duration::from_secs(24 * 60 * 60);
        assert_eq!(
            timestamp(std::time::UNIX_EPOCH + day),
            "1970-01-02T00:00:00Z"
        );
        assert!(timestamp_before(day) < super::now());
    }

    struct TempRepository(std::path::PathBuf);

    impl TempRepository {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("drostje-diff-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            git(&path, &["init", "-b", "main"]);
            fs::write(path.join("tracked.txt"), "before\n").unwrap();
            git(&path, &["add", "tracked.txt"]);
            git(
                &path,
                &[
                    "-c",
                    "user.name=DROSTJE.",
                    "-c",
                    "user.email=drostje@example.invalid",
                    "commit",
                    "-m",
                    "fixture",
                ],
            );
            Self(path)
        }
    }

    impl Drop for TempRepository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn application_icon_is_a_filled_rounded_square() {
        let icon = image::load_from_memory(APPLICATION_ICON)
            .expect("application icon should decode")
            .into_rgba8();

        assert_eq!(icon.dimensions(), (1024, 1024));
        for corner in [(0, 0), (1023, 0), (0, 1023), (1023, 1023)] {
            assert_eq!(icon.get_pixel(corner.0, corner.1).0[3], 0);
        }
        assert!(icon.get_pixel(512, 512).0[3] >= 250);
        let filled_pixels = icon.pixels().filter(|pixel| pixel.0[3] >= 250).count();
        assert!(filled_pixels * 100 / (1024 * 1024) >= 80);
    }

    #[test]
    fn working_tree_diff_includes_tracked_and_untracked_content() {
        let repository = TempRepository::new();
        fs::write(repository.0.join("tracked.txt"), "after\n").unwrap();
        fs::write(repository.0.join("new.txt"), "new line\n").unwrap();

        let diff = working_tree_diff(&repository.0).unwrap();

        assert_eq!(diff.changed_files, 2);
        assert_eq!(diff.additions, 2);
        assert_eq!(diff.deletions, 1);
        assert!(diff.patch.contains("-before"));
        assert!(diff.patch.contains("+after"));
        assert!(diff.patch.contains("diff --git a/new.txt b/new.txt"));
        assert!(diff.patch.contains("+new line"));
        assert!(!diff.truncated);
    }

    #[test]
    fn working_tree_patch_is_the_diff_without_display_rows() {
        let repository = TempRepository::new();
        fs::write(repository.0.join("tracked.txt"), "after\n").unwrap();
        fs::write(repository.0.join("new.txt"), "new line\n").unwrap();

        let patch = working_tree_patch(&repository.0).unwrap();
        let diff = working_tree_diff(&repository.0).unwrap();

        assert_eq!(patch.head_branch, diff.head_branch);
        assert_eq!(patch.patch, diff.patch);
        assert_eq!(
            (patch.changed_files, patch.additions, patch.deletions),
            (diff.changed_files, diff.additions, diff.deletions)
        );
        assert_eq!(patch.truncated, diff.truncated);
        assert_eq!(*diff.rows, drostje_diff::parse(&patch.patch));
    }

    #[test]
    fn changed_lines_are_counted_in_one_pass_without_file_headers() {
        let patch =
            "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-old\n+new\n context\n-gone\n+++ also a header\n";
        assert_eq!(count_changed_lines(patch), (1, 2));
        assert_eq!(count_changed_lines(""), (0, 0));
    }
    #[cfg(unix)]
    #[test]
    fn untracked_symlinks_show_the_link_without_reading_external_content() {
        let repository = TempRepository::new();
        let external = repository.0.with_extension("secret");
        fs::write(&external, "private-host-credential\n").unwrap();
        std::os::unix::fs::symlink(&external, repository.0.join("external-link")).unwrap();
        let diff = working_tree_diff(&repository.0).unwrap();
        assert!(diff.patch.contains("new file mode 120000"));
        assert!(diff.patch.contains(&external.to_string_lossy().to_string()));
        assert!(!diff.patch.contains("private-host-credential"));
        fs::remove_file(external).unwrap();
    }

    #[test]
    fn oversized_untracked_and_tracked_diffs_are_bounded_during_collection() {
        let repository = TempRepository::new();
        fs::File::create(repository.0.join("huge.bin"))
            .unwrap()
            .set_len(1024 * 1024 * 1024)
            .unwrap();
        let diff = working_tree_diff(&repository.0).unwrap();
        assert!(diff.patch.contains("content omitted"));
        fs::write(
            repository.0.join("tracked.txt"),
            "changed line\n".repeat(100_000),
        )
        .unwrap();
        let diff = working_tree_diff(&repository.0).unwrap();
        assert!(diff.truncated);
        assert!(diff.patch.len() <= super::MAX_DIFF_BYTES + 64);
    }

    #[test]
    fn commit_all_records_tracked_and_untracked_changes() {
        use super::commit_all;
        let repository = TempRepository::new();
        git(&repository.0, &["config", "user.name", "DROSTJE."]);
        git(
            &repository.0,
            &["config", "user.email", "drostje@example.invalid"],
        );
        assert!(
            format!("{:#}", commit_all(&repository.0, "Nothing").unwrap_err())
                .contains("Nothing to commit")
        );
        fs::write(repository.0.join("tracked.txt"), "after\n").unwrap();
        fs::write(repository.0.join("new.txt"), "new line\n").unwrap();
        assert!(commit_all(&repository.0, "  \n").is_err());

        let hash = commit_all(&repository.0, "Update notes\n\nAdd new.txt").unwrap();

        assert!(!hash.is_empty());
        let diff = working_tree_diff(&repository.0).unwrap();
        assert_eq!(diff.changed_files, 0);
        assert!(diff.patch.is_empty());
        let log = std::process::Command::new("git")
            .arg("-C")
            .arg(&repository.0)
            .args(["log", "-1", "--format=%s%n%b", "--name-only"])
            .output()
            .unwrap();
        let log = String::from_utf8(log.stdout).unwrap();
        assert!(log.starts_with("Update notes\nAdd new.txt"), "{log}");
        assert!(
            log.contains("new.txt") && log.contains("tracked.txt"),
            "{log}"
        );
    }

    #[test]
    fn worktrees_are_created_renamed_inspected_and_removed() {
        use super::{
            branch_exists, branch_is_merged, create_worktree, delete_branch, inspect_worktree,
            prune_worktrees, remove_worktree, rename_branch, worktree_head_branch,
        };
        let repository = TempRepository::new();
        let data = std::env::temp_dir().join(format!("drostje-wt-{}", uuid::Uuid::new_v4()));
        let path = data.join("worktrees").join("repo").join("amber-heron");
        let created = create_worktree(&repository.0, &path, "drostje/amber-heron", "main").unwrap();
        assert_eq!(created, fs::canonicalize(&path).unwrap());
        assert!(path.join("tracked.txt").is_file());
        assert_eq!(
            worktree_head_branch(&created).unwrap().as_deref(),
            Some("drostje/amber-heron")
        );
        assert!(branch_exists(&repository.0, "drostje/amber-heron").unwrap());
        assert!(!branch_exists(&repository.0, "drostje/other").unwrap());
        assert!(branch_is_merged(&repository.0, "drostje/amber-heron", "main").unwrap());
        assert!(create_worktree(&repository.0, &path, "drostje/again", "main").is_err());
        assert!(
            create_worktree(
                &repository.0,
                &data.join("x"),
                "drostje/amber-heron",
                "main"
            )
            .is_err()
        );

        use super::switch_branch;
        switch_branch(&created, "drostje/side", true).unwrap();
        assert_eq!(
            worktree_head_branch(&created).unwrap().as_deref(),
            Some("drostje/side")
        );
        assert!(
            switch_branch(&created, "main", false).is_err(),
            "main is checked out in the root"
        );
        switch_branch(&created, "drostje/amber-heron", false).unwrap();
        fs::write(created.join("dirty.txt"), "x\n").unwrap();
        assert!(switch_branch(&created, "drostje/side", false).is_err());
        fs::remove_file(created.join("dirty.txt")).unwrap();
        delete_branch(&repository.0, "drostje/side").unwrap();

        fs::write(created.join("tracked.txt"), "worktree change\n").unwrap();
        git(&created, &["add", "tracked.txt"]);
        git(
            &created,
            &[
                "-c",
                "user.name=DROSTJE.",
                "-c",
                "user.email=drostje@example.invalid",
                "commit",
                "-m",
                "wip",
            ],
        );
        let status =
            inspect_worktree(&repository.0, &created, "drostje/amber-heron", "main").unwrap();
        assert!(status.exists && status.clean && !status.merged);
        assert_eq!(status.head_branch.as_deref(), Some("drostje/amber-heron"));
        assert!(delete_branch(&repository.0, "drostje/amber-heron").is_err());

        rename_branch(&created, "drostje/amber-heron", "drostje/fix-login").unwrap();
        assert_eq!(
            worktree_head_branch(&created).unwrap().as_deref(),
            Some("drostje/fix-login")
        );
        assert!(!branch_exists(&repository.0, "drostje/amber-heron").unwrap());
        assert!(rename_branch(&created, "drostje/amber-heron", "drostje/x").is_err());
        assert!(worktree_head_branch(&repository.0.join("..")).is_err());

        fs::write(created.join("scratch.txt"), "untracked\n").unwrap();
        let dirty = inspect_worktree(&repository.0, &created, "drostje/fix-login", "main").unwrap();
        assert!(!dirty.clean);
        assert!(remove_worktree(&repository.0, &created, false).is_err());
        git(&created, &["checkout", "--detach"]);
        assert_eq!(worktree_head_branch(&created).unwrap(), None);
        remove_worktree(&repository.0, &created, true).unwrap();
        assert!(!created.exists());
        let gone = inspect_worktree(&repository.0, &created, "drostje/fix-login", "main").unwrap();
        assert!(!gone.exists);

        let second = data.join("worktrees").join("repo").join("bold-otter");
        create_worktree(&repository.0, &second, "drostje/bold-otter", "main").unwrap();
        fs::remove_dir_all(&second).unwrap();
        prune_worktrees(&repository.0).unwrap();
        assert!(!git_output(&repository.0, &["worktree", "list"]).contains("bold-otter"));
        assert!(branch_is_merged(&repository.0, "drostje/bold-otter", "main").unwrap());
        delete_branch(&repository.0, "drostje/bold-otter").unwrap();
        let _ = fs::remove_dir_all(&data);
    }

    fn git_output(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }
}
