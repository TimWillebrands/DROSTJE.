use super::setup::executable_diagnostics;
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Bootstrap {
    pub data_dir: PathBuf,
    pub store: Store,
    pub snapshot: WorkspaceSnapshot,
    pub diagnostics: BTreeMap<String, String>,
    pub repositories: BTreeMap<ProjectId, Result<platform::RepositoryInfo, String>>,
    pub browser_tools: Option<crate::browser_tools::BrowserToolServer>,
}

impl Bootstrap {
    /// Runs before GPUI starts; subsequent filesystem, Git and database work is asynchronous.
    pub fn load() -> Result<Self> {
        let data_dir = platform::data_dir()?;
        let store = Store::open(data_dir.clone())?;
        let mut snapshot = smol::block_on(store.snapshot())?;
        if !snapshot.settings.contains_key("executables") {
            let value = json!({
                "nvim": "", "omp": "", "pi": "", "claude": "", "codex": "", "opencode": "",
                "llama": "", "factory": ""
            });
            smol::block_on(store.set_setting("executables".into(), value.clone()))?;
            snapshot.settings.insert("executables".into(), value);
        }
        if !snapshot.settings.contains_key("factory_data_root") {
            let root = platform::factory_data_root()?;
            let value = json!(root);
            smol::block_on(store.set_setting("factory_data_root".into(), value.clone()))?;
            snapshot.settings.insert("factory_data_root".into(), value);
        }
        let ((diagnostics, browser_tools), repositories) =
            inspect_repositories(&snapshot.projects, || {
                let mut diagnostics = executable_diagnostics(&snapshot.settings);
                let browser_tools = match crate::browser_tools::BrowserToolServer::start() {
                    Ok(server) => Some(server),
                    Err(error) => {
                        diagnostics
                            .insert("browser tools".into(), format!("Unavailable: {error:#}"));
                        None
                    }
                };
                (diagnostics, browser_tools)
            });
        Ok(Self {
            data_dir,
            store,
            snapshot,
            diagnostics,
            repositories,
            browser_tools,
        })
    }
}

/// At most this many helper threads inspect repositories at startup.
const INSPECTION_THREADS: usize = 8;

/// Every project's repository as `platform::inspect_repository` reads it, keyed by project.
/// Each inspection runs several Git processes and the window waits for all of them, so they
/// run on scoped helper threads while `alongside` runs on the calling thread, which then
/// helps with what is left.
fn inspect_repositories<T>(
    projects: &[Project],
    alongside: impl FnOnce() -> T,
) -> (
    T,
    BTreeMap<ProjectId, Result<platform::RepositoryInfo, String>>,
) {
    let next = AtomicUsize::new(0);
    let inspect = || {
        let mut found = Vec::new();
        while let Some(project) = projects.get(next.fetch_add(1, Ordering::Relaxed)) {
            let info = platform::inspect_repository(&project.root).map_err(|e| format!("{e:#}"));
            found.push((project.id, info));
        }
        found
    };
    std::thread::scope(|scope| {
        // A helper that cannot start leaves its share to the others and this thread.
        let helpers: Vec<_> = (0..projects.len().min(INSPECTION_THREADS))
            .filter_map(|_| {
                std::thread::Builder::new()
                    .name("repository-inspection".into())
                    .spawn_scoped(scope, inspect)
                    .ok()
            })
            .collect();
        let value = alongside();
        let mut repositories: BTreeMap<_, _> = inspect().into_iter().collect();
        for helper in helpers {
            match helper.join() {
                Ok(found) => repositories.extend(found),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        (value, repositories)
    })
}

#[cfg(test)]
mod tests {
    use super::{INSPECTION_THREADS, inspect_repositories};
    use crate::model::{Project, ProjectDefaults};
    use crate::platform;
    use std::{fs, path::PathBuf, process::Command};
    use uuid::Uuid;

    struct TempDirectory(PathBuf);
    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn concurrent_inspection_matches_one_by_one_inspection() {
        let dir =
            TempDirectory(std::env::temp_dir().join(format!("drostje-inspect-{}", Uuid::new_v4())));
        let repository = dir.0.join("repository");
        let plain = dir.0.join("plain");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&plain).unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repository)
            .status()
            .unwrap();
        assert!(status.success());
        // More projects than helpers, so the queue is shared.
        let projects: Vec<_> = (0..INSPECTION_THREADS * 2 + 3)
            .map(|n| Project {
                id: Uuid::from_u128(n as u128 + 1),
                name: format!("Project {n}"),
                root: match n % 3 {
                    0 => repository.clone(),
                    1 => plain.clone(),
                    _ => dir.0.join("missing"),
                },
                archived: false,
                defaults: ProjectDefaults::default(),
            })
            .collect();

        let (alongside, repositories) = inspect_repositories(&projects, || "diagnostics");

        assert_eq!(alongside, "diagnostics");
        assert_eq!(repositories.len(), projects.len());
        for project in &projects {
            let expected =
                platform::inspect_repository(&project.root).map_err(|e| format!("{e:#}"));
            assert_eq!(
                format!("{:?}", repositories[&project.id]),
                format!("{expected:?}"),
                "{}",
                project.name
            );
        }
        assert!(repositories[&projects[0].id].is_ok());
        assert!(repositories[&projects[1].id].is_err());
        assert!(repositories[&projects[2].id].is_err());
        assert!(inspect_repositories(&[], || ()).1.is_empty());
    }
}
