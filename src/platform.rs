use anyhow::{Context, Result, bail, ensure};
use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

const APPLICATION_ICON: &[u8] = include_bytes!("../assets/drostje-app-icon.png");

#[cfg(target_os = "macos")]
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

#[cfg(not(target_os = "macos"))]
pub fn set_application_icon() -> Result<()> {
    Ok(())
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
        None => home()?.join("Library/Application Support/Drostje"),
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
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryDiff {
    pub patch: String,
    pub changed_files: usize,
    pub additions: usize,
    pub deletions: usize,
    pub truncated: bool,
}

const MAX_DIFF_BYTES: usize = 512 * 1024;
const MAX_UNTRACKED_FILE_BYTES: usize = 128 * 1024;

pub fn working_tree_diff(root: &Path) -> Result<RepositoryDiff> {
    let status = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let entries = status_entries(&status);
    let tracked = git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--unified=3",
            "HEAD",
            "--",
        ],
    );
    let mut patch = match tracked {
        Ok(patch) => patch,
        Err(error) if git(root, &["rev-parse", "--verify", "HEAD"]).is_ok() => {
            return Err(error);
        }
        Err(_) => {
            let staged = git(
                root,
                &[
                    "diff",
                    "--cached",
                    "--no-ext-diff",
                    "--no-color",
                    "--unified=3",
                    "--",
                ],
            )?;
            let unstaged = git(
                root,
                &["diff", "--no-ext-diff", "--no-color", "--unified=3", "--"],
            )?;
            [staged, unstaged]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    for (status, path) in &entries {
        if *status == "??" {
            append_untracked_patch(root, path, &mut patch)?;
        }
    }
    let mut truncated = false;
    if patch.len() > MAX_DIFF_BYTES {
        let mut end = MAX_DIFF_BYTES;
        while !patch.is_char_boundary(end) {
            end -= 1;
        }
        patch.truncate(end);
        patch.push_str("\n\nDiff truncated at 512 KiB.");
        truncated = true;
    }
    let additions = patch
        .lines()
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .count();
    let deletions = patch
        .lines()
        .filter(|line| line.starts_with('-') && !line.starts_with("---"))
        .count();
    Ok(RepositoryDiff {
        patch,
        changed_files: entries.len(),
        additions,
        deletions,
        truncated,
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

fn append_untracked_patch(root: &Path, path: &str, patch: &mut String) -> Result<()> {
    let relative = Path::new(path);
    ensure!(
        !relative.is_absolute()
            && !relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir)),
        "Git returned an invalid untracked path"
    );
    let bytes = match fs::read(root.join(relative)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Read untracked file {}", relative.display()));
        }
    };
    if !patch.is_empty() {
        patch.push('\n');
    }
    patch.push_str(&format!(
        "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n"
    ));
    if bytes.len() > MAX_UNTRACKED_FILE_BYTES {
        patch.push_str("Untracked file is larger than 128 KiB; content omitted.\n");
    } else if let Ok(text) = std::str::from_utf8(&bytes) {
        let lines = text.lines().count();
        patch.push_str(&format!("@@ -0,0 +1,{lines} @@\n"));
        for line in text.lines() {
            patch.push('+');
            patch.push_str(line);
            patch.push('\n');
        }
    } else {
        patch.push_str("Binary untracked file; content omitted.\n");
    }
    Ok(())
}
pub fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .context("Run Git")?;
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
pub fn inspect_repository(path: &Path) -> Result<RepositoryInfo> {
    let path =
        fs::canonicalize(path).with_context(|| format!("Read project {}", path.display()))?;
    ensure!(path.is_dir(), "Project is not a directory");
    ensure!(
        git(&path, &["rev-parse", "--is-bare-repository"])? == "false",
        "Bare repositories cannot be registered as projects"
    );
    let root = fs::canonicalize(git(&path, &["rev-parse", "--show-toplevel"])?)?;
    fs::read_dir(&root).context("Project root is unreadable")?;
    let branch = git(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let branches = git(
        &root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?
    .lines()
    .map(str::to_owned)
    .collect();
    let dirty = !git(&root, &["status", "--porcelain"])?.is_empty();
    Ok(RepositoryInfo {
        root,
        branch,
        branches,
        dirty,
    })
}
pub fn validate_branch(root: &Path, branch: &str) -> Result<()> {
    ensure!(!branch.trim().is_empty(), "Choose a target branch");
    git(root, &["check-ref-format", "--branch", branch])?;
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
    )?;
    Ok(())
}
pub fn now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // gmtime_r writes only the provided tm and uses no process-global time buffer.
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
    use super::{APPLICATION_ICON, working_tree_diff};
    use std::{fs, path::Path, process::Command};

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
}
