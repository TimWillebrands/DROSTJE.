//! Per-run directory for Servo's on-disk state: IndexedDB, Cache Storage and
//! anything else Servo keys off `Opts::config_dir`.
//!
//! Without a config directory Servo 0.7 drops its own temporary directory
//! before use and writes IndexedDB below the process's working directory, so
//! the engine always supplies one. Nothing in it outlives the engine: the
//! directory is removed when the engine stops, and a later start removes the
//! directories of runs that did not stop cleanly. A run holds a lock for its
//! whole life, so concurrent DROSTJE processes never remove each other's state.
use std::{
    env,
    fs::{self, File},
    io,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::Duration,
};

use uuid::Uuid;

const LOCK: &str = "run.lock";
/// A run creates its directory and takes its lock within microseconds; never
/// sweep a directory young enough to be between those two steps.
const STALE_AFTER: Duration = Duration::from_secs(60);

pub(super) struct EngineProfile {
    path: PathBuf,
    _lock: File,
}

impl EngineProfile {
    pub(super) fn create() -> io::Result<Self> {
        // The runtime dir is per user and private; the temp dir is per user on macOS.
        let root = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(env::temp_dir)
            .join("drostje-servo");
        Self::create_in(&root, STALE_AFTER)
    }

    fn create_in(root: &Path, stale_after: Duration) -> io::Result<Self> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        if fs::symlink_metadata(root)?.file_type().is_symlink() {
            return Err(io::Error::other(format!(
                "{} is a symbolic link",
                root.display()
            )));
        }
        sweep(root, stale_after);
        let path = root.join(Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        let lock = File::create_new(path.join(LOCK))?;
        lock.try_lock().map_err(io::Error::other)?;
        Ok(Self { path, _lock: lock })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for EngineProfile {
    fn drop(&mut self) {
        // Storage threads may still be exiting; anything left is swept next start.
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn sweep(root: &Path, stale_after: Duration) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let old_enough = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= stale_after);
        if !old_enough || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let abandoned = match File::open(path.join(LOCK)) {
            Ok(lock) => lock.try_lock().is_ok(),
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        };
        if abandoned {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let root = env::temp_dir().join(format!("drostje-profile-test-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn a_run_owns_its_directory_until_dropped() {
        let root = scratch();
        let run = EngineProfile::create_in(&root, Duration::ZERO).unwrap();
        fs::write(run.path().join("indexeddb.sqlite"), b"data").unwrap();
        // Another engine starting meanwhile must not sweep a live run.
        let other = EngineProfile::create_in(&root, Duration::ZERO).unwrap();
        assert!(run.path().join("indexeddb.sqlite").exists());
        let path = run.path().to_path_buf();
        drop(run);
        assert!(!path.exists());
        assert!(other.path().exists());
        drop(other);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_start_sweeps_runs_that_did_not_stop_cleanly() {
        let root = scratch();
        let crashed = root.join("crashed");
        fs::create_dir_all(crashed.join("clientstorage")).unwrap();
        File::create(crashed.join(LOCK)).unwrap();
        let without_lock = root.join("without-lock");
        fs::create_dir(&without_lock).unwrap();
        let young = EngineProfile::create_in(&root, Duration::from_secs(3600)).unwrap();
        assert!(crashed.exists(), "young directories may still be starting");
        let run = EngineProfile::create_in(&root, Duration::ZERO).unwrap();
        assert!(!crashed.exists());
        assert!(!without_lock.exists());
        assert!(young.path().exists());
        drop((young, run));
        fs::remove_dir_all(root).unwrap();
    }
}
