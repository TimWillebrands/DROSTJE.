use anyhow::{Context, Result, ensure};
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const PACKAGE: &str = "@earendil-works/pi-coding-agent";
const MINIMUM_NODE: (u64, u64, u64) = (22, 19, 0);

pub(super) async fn install(data_dir: PathBuf) -> Result<PathBuf> {
    smol::unblock(move || {
        let npm = crate::platform::resolve_executable("npm", None)
            .context("Pi installation requires npm; install Node.js 22.19 or newer, then retry")?;
        let node = crate::platform::resolve_executable("node", None)
            .context("Pi installation requires Node.js 22.19 or newer, then retry")?;
        install_with(&npm, &node, &data_dir.join("tools/pi"))
    })
    .await
}

fn install_with(npm: &Path, node: &Path, destination: &Path) -> Result<PathBuf> {
    let version = Command::new(node)
        .arg("--version")
        .output()
        .with_context(|| format!("Check Node.js at {}", node.display()))?;
    ensure!(
        version.status.success(),
        "Node.js version check failed: {}",
        output_text(&version)
    );
    let version_text = String::from_utf8_lossy(&version.stdout);
    ensure!(
        node_version(&version_text).is_some_and(|version| version >= MINIMUM_NODE),
        "Pi requires Node.js 22.19 or newer; found {}",
        version_text.trim()
    );

    let parent = destination
        .parent()
        .context("Pi installation directory has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Create Pi tools directory at {}", parent.display()))?;
    let staging = parent.join(format!("pi-install-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&staging)
        .with_context(|| format!("Create Pi staging directory at {}", staging.display()))?;
    let result = (|| {
        let path = path_with(node.parent().context("Node.js executable has no parent")?)?;
        let output = Command::new(npm)
            .args([
                OsStr::new("install"),
                OsStr::new("--global"),
                OsStr::new("--ignore-scripts"),
                OsStr::new("--no-fund"),
                OsStr::new("--no-audit"),
                OsStr::new("--progress=false"),
                OsStr::new("--loglevel=error"),
                OsStr::new("--prefix"),
            ])
            .arg(&staging)
            .arg(PACKAGE)
            .env("PATH", &path)
            .output()
            .with_context(|| format!("Start npm at {}", npm.display()))?;
        ensure!(
            output.status.success(),
            "Pi installation failed: {}",
            output_text(&output)
        );

        let executable = staging.join("bin/pi");
        ensure!(
            fs::metadata(&executable).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            }),
            "npm completed without installing an executable Pi at {}",
            executable.display()
        );
        let smoke = Command::new(&executable)
            .arg("--version")
            .env("PATH", path)
            .output()
            .with_context(|| format!("Start installed Pi at {}", executable.display()))?;
        ensure!(
            smoke.status.success() && !smoke.stdout.is_empty(),
            "Installed Pi did not start: {}",
            output_text(&smoke)
        );

        if destination.exists() {
            fs::remove_dir_all(destination).with_context(|| {
                format!(
                    "Replace managed Pi installation at {}",
                    destination.display()
                )
            })?;
        }
        fs::rename(&staging, destination).with_context(|| {
            format!(
                "Move verified Pi installation from {} to {}",
                staging.display(),
                destination.display()
            )
        })?;
        fs::canonicalize(destination.join("bin/pi")).context("Resolve installed Pi executable")
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn path_with(directory: &Path) -> Result<OsString> {
    let mut paths = vec![directory.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(paths).context("Build Pi installation PATH")
}

fn node_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().strip_prefix('v')?.split('.');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

fn output_text(output: &Output) -> String {
    let text = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    String::from_utf8_lossy(text)
        .trim()
        .chars()
        .take(4096)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("drostje-pi-install-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn executable(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn managed_install_runs_official_npm_package_without_lifecycle_scripts() {
        let temp = Temp::new();
        let node = temp.0.join("node");
        executable(&node, "#!/bin/sh\nprintf 'v22.19.0\\n'\n");
        let npm = temp.0.join("npm");
        executable(
            &npm,
            r#"#!/bin/sh
printf '%s\n' "$@" > "$(dirname "$0")/npm-args"
while [ "$1" != "--prefix" ]; do shift; done
prefix=$2
mkdir -p "$prefix/bin" "$prefix/lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle"
printf '#!/bin/sh\nprintf "0.85.1\\n"\n' > "$prefix/lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js"
chmod +x "$prefix/lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js"
ln -s ../lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js "$prefix/bin/pi"
"#,
        );

        let installed = install_with(&npm, &node, &temp.0.join("managed/pi")).unwrap();
        assert!(
            installed
                .ends_with("lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js")
        );
        let args = fs::read_to_string(temp.0.join("npm-args")).unwrap();
        assert!(args.contains("--global\n"));
        assert!(args.contains("--ignore-scripts\n"));
        assert!(args.contains("@earendil-works/pi-coding-agent\n"));
    }

    #[test]
    fn managed_install_rejects_old_node_before_running_npm() {
        let temp = Temp::new();
        let node = temp.0.join("node");
        executable(&node, "#!/bin/sh\nprintf 'v22.18.0\\n'\n");
        let npm = temp.0.join("npm");
        executable(&npm, "#!/bin/sh\ntouch \"$(dirname \"$0\")/ran\"\n");

        let error = install_with(&npm, &node, &temp.0.join("managed/pi")).unwrap_err();
        assert!(format!("{error:#}").contains("requires Node.js 22.19"));
        assert!(!temp.0.join("ran").exists());
    }
}
