use anyhow::{Context, Result, bail, ensure};
use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

const APPLICATION_ICON: &[u8] = include_bytes!("../assets/drost-app-icon.png");

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
    absolute(match env::var_os("DROST_DATA_DIR") {
        Some(p) => p.into(),
        None => home()?.join("Library/Application Support/Drost"),
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
    use super::APPLICATION_ICON;

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
        let filled_pixels = icon
            .pixels()
            .filter(|pixel| pixel.0[3] >= 250)
            .count();
        assert!(filled_pixels * 100 / (1024 * 1024) >= 80);
    }
}
