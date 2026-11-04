#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{fs, path::PathBuf, process::Command};

#[test]
fn committed_native_text_is_typing_not_bracketed_paste() {
    let temporary = Command::new("mktemp")
        .args(["-d", "-t", "drost-native-input.XXXXXXXX"])
        .output()
        .expect("create isolated native input regression directory");
    assert!(temporary.status.success(), "mktemp failed: {temporary:?}");
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(PathBuf::from(
        String::from_utf8(temporary.stdout)
            .expect("temporary path is UTF-8")
            .trim_end(),
    ));
    let executable = scratch.0.join("native-input");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/native_input.c");
    let compile = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("compile native input regression harness");
    assert!(compile.status.success(), "{compile:?}");
    let result = Command::new(executable)
        .output()
        .expect("run native input regression harness");
    assert!(result.status.success(), "{result:?}");
}
