//! Import the user's login-shell environment at startup.
//!
//! An app launched from Finder, the Dock or a desktop launcher inherits the session manager's
//! minimal environment (on macOS `PATH=/usr/bin:/bin:/usr/sbin:/sbin`), so agents and Factory
//! runs could not find tools the user reaches from a terminal (Homebrew's `zig`, `cargo`, …).
//! [`import_login_shell_environment`] asks the user's shell once, before any thread starts,
//! and merges the result into this process so every child inherits it.

use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashMap,
    env,
    ffi::{CStr, OsStr, OsString},
    io::Read,
    os::unix::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

const MARKER: &str = "__DROSTJE_SHELL_ENV__";
const TIMEOUT: Duration = Duration::from_secs(5);
/// Shell bookkeeping that describes the probe shell, not the user's environment.
const IGNORED: &[&str] = &["_", "PWD", "OLDPWD", "SHLVL"];

/// Merge the login shell's environment into this process.
///
/// Variables already set win, so explicit overrides (`DROSTJE_DATA_DIR=… cargo run`) survive.
/// `PATH` takes the shell's order, followed by inherited entries the shell lacks.
///
/// Must run before any other thread exists: it mutates the process environment.
pub fn import_login_shell_environment() -> Result<()> {
    let shell = login_shell()?;
    let captured = probe(&shell, TIMEOUT)?;
    let current: HashMap<OsString, OsString> = env::vars_os().collect();
    for (key, value) in merge(&current, captured) {
        // SAFETY: called from `run` before GPUI, the store or any executor starts a thread, and
        // the probe's reader thread has been joined, so no thread can read the environment
        // concurrently.
        unsafe { env::set_var(key, value) };
    }
    Ok(())
}

fn login_shell() -> Result<PathBuf> {
    if let Some(shell) = env::var_os("SHELL").filter(|s| !s.is_empty()) {
        return Ok(shell.into());
    }
    // SAFETY: `getpwuid` returns null or a pointer to static storage that stays valid until the
    // next passwd lookup; the shell path is copied out before returning, and startup is
    // single-threaded.
    let shell = unsafe {
        let entry = libc::getpwuid(libc::getuid());
        ensure!(
            !entry.is_null() && !(*entry).pw_shell.is_null(),
            "The login shell is unknown"
        );
        OsStr::from_bytes(CStr::from_ptr((*entry).pw_shell).to_bytes()).to_os_string()
    };
    Ok(shell.into())
}

/// Run `shell` as an interactive login shell and return the environment it exports.
///
/// Interactive, because many users extend `PATH` in `.zshrc`/`.bashrc` rather than the profile.
/// The markers skip anything the rc files print. A shell that does not finish within `timeout`
/// (a prompt, an `exec tmux`) is killed with its process group.
fn probe(shell: &Path, timeout: Duration) -> Result<Vec<(OsString, OsString)>> {
    // The script is a constant: the one place DROSTJE. hands a string to a shell, because
    // reading the shell's own environment is the point.
    let script = format!("printf '%s' {MARKER}; command env -0; printf '%s' {MARKER}");
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("Start login shell {}", shell.display()))?;
    let mut stdout = child.stdout.take().context("Login shell stdout")?;
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout.read_to_end(&mut output).map(|_| output);
        let _ = sender.send(());
        result
    });
    let finished = receiver.recv_timeout(timeout).is_ok();
    if !finished {
        // SAFETY: the child leads its own process group (`process_group(0)`), so this signals
        // only the probe shell and whatever its rc files started.
        unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
    }
    let output = reader.join();
    let status = child.wait().context("Wait for login shell")?;
    if !finished {
        bail!(
            "Login shell {} did not finish within {}s",
            shell.display(),
            timeout.as_secs()
        );
    }
    let output = match output {
        Ok(output) => output.context("Read login shell output")?,
        Err(_) => bail!("Login shell reader panicked"),
    };
    ensure!(
        status.success(),
        "Login shell {} exited with {status}",
        shell.display()
    );
    parse(&output).with_context(|| format!("Parse environment from {}", shell.display()))
}

fn parse(output: &[u8]) -> Result<Vec<(OsString, OsString)>> {
    let marker = MARKER.as_bytes();
    let find = |from: usize| {
        output[from..]
            .windows(marker.len())
            .position(|w| w == marker)
            .map(|ix| from + ix)
    };
    let start = find(0).context("Missing start marker")? + marker.len();
    let end = find(start).context("Missing end marker")?;
    Ok(output[start..end]
        .split(|&b| b == 0)
        .filter_map(|entry| {
            let eq_ix = entry.iter().position(|&b| b == b'=')?;
            let key = &entry[..eq_ix];
            (!key.is_empty()).then(|| {
                (
                    OsStr::from_bytes(key).to_os_string(),
                    OsStr::from_bytes(&entry[eq_ix + 1..]).to_os_string(),
                )
            })
        })
        .collect())
}

/// The variables to set so the process environment matches the login shell's.
fn merge(
    current: &HashMap<OsString, OsString>,
    captured: Vec<(OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    captured
        .into_iter()
        .filter(|(key, _)| !IGNORED.iter().any(|ignored| key == ignored))
        .filter_map(|(key, value)| {
            if key == "PATH" {
                let mut paths: Vec<PathBuf> = env::split_paths(&value).collect();
                if let Some(inherited) = current.get(&key) {
                    for path in env::split_paths(inherited) {
                        if !paths.contains(&path) {
                            paths.push(path);
                        }
                    }
                }
                let joined = env::join_paths(paths).ok()?;
                (current.get(&key) != Some(&joined)).then_some((key, joined))
            } else {
                (!current.contains_key(&key)).then_some((key, value))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, time::Instant};

    fn os(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[test]
    fn parse_skips_rc_noise_around_markers() {
        let output =
            format!("Welcome!\n{MARKER}PATH=/opt/homebrew/bin:/usr/bin\0A=x=y\0\0{MARKER}bye\n");
        assert_eq!(
            parse(output.as_bytes()).unwrap(),
            os(&[("PATH", "/opt/homebrew/bin:/usr/bin"), ("A", "x=y")])
        );
        assert!(parse(b"no markers").is_err());
    }

    #[test]
    fn merge_prefers_shell_path_order_and_keeps_existing_variables() {
        let current: HashMap<_, _> = os(&[
            ("PATH", "/usr/bin:/bin:/custom"),
            ("DROSTJE_DATA_DIR", "/tmp/scratch"),
        ])
        .into_iter()
        .collect();
        let captured = os(&[
            ("PATH", "/opt/homebrew/bin:/usr/bin:/bin"),
            ("DROSTJE_DATA_DIR", "/elsewhere"),
            ("CARGO_HOME", "/Users/u/.cargo"),
            ("PWD", "/Users/u"),
            ("SHLVL", "2"),
        ]);
        assert_eq!(
            merge(&current, captured),
            os(&[
                ("PATH", "/opt/homebrew/bin:/usr/bin:/bin:/custom"),
                ("CARGO_HOME", "/Users/u/.cargo"),
            ])
        );
    }

    #[test]
    fn probe_reads_a_real_shell_environment() {
        let captured = probe(Path::new("/bin/sh"), TIMEOUT).unwrap();
        assert!(captured.iter().any(|(key, _)| key == "PATH"));
    }

    #[test]
    fn probe_kills_a_shell_that_never_finishes() {
        let shell = env::temp_dir().join(format!("drostje-hanging-shell-{}", std::process::id()));
        fs::write(&shell, "#!/bin/sh\nsleep 30\n").unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let error = probe(&shell, Duration::from_millis(200)).unwrap_err();
        fs::remove_file(&shell).unwrap();
        assert!(error.to_string().contains("did not finish"), "{error:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
