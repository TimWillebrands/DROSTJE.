#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

struct ScratchDirectory(PathBuf);

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn successful(output: Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation}: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn run_native_regression(modes: &[(&str, Option<&str>)]) {
    let temporary = Command::new("mktemp")
        .args(["-d", "-t", "drost-process-activity.XXXXXXXX"])
        .output()
        .expect("create isolated native regression directory");
    assert!(temporary.status.success(), "mktemp failed: {temporary:?}");
    let scratch = ScratchDirectory(PathBuf::from(
        String::from_utf8(temporary.stdout)
            .expect("temporary path is UTF-8")
            .trim_end(),
    ));
    let executable = scratch.0.join("process-activity");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/process_activity.c");
    successful(
        Command::new("cc")
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .expect("compile native regression harness with system cc"),
        "compile native process regression",
    );
    for &(scenario, argument) in modes {
        let directory = scratch.0.join(scenario);
        fs::create_dir(&directory).expect("create isolated shell scenario directory");
        successful(
            Command::new(&executable)
                .args(argument)
                .current_dir(&directory)
                .output()
                .expect("run native regression harness"),
            scenario,
        );
    }
}

#[test]
fn unintegrated_shell_process_activity() {
    run_native_regression(&[("unintegrated-shell", None)]);
}

#[cfg(target_os = "macos")]
#[test]
fn login_wrapper_shell_process_activity() {
    run_native_regression(&[("macos-login-wrapper", Some("--login-wrapper"))]);
}
