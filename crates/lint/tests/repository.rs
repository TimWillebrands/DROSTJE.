use std::path::{Path, PathBuf};
use std::process::Command;

use drostje_lint::{Language, violations};

const MARKDOWN: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "CODING_STANDARDS.md",
    "CONTEXT.md",
    "README.md",
    "vendor/README.md",
    "docs/agents/ui.md",
    "assets/artifact-kit/REFERENCE.md",
    "assets/fonts/README.md",
    ".agents/skills/gpui-kit-ui/SKILL.md",
    ".agents/skills/gpui-kit-ui/COMPONENTS.md",
    ".agents/skills/gpui-kit-ui/CLEANUP.md",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files() -> Vec<String> {
    let output = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(root())
        .output()
        .expect("git ls-files runs in the workspace checkout");
    assert!(output.status.success(), "git ls-files failed");
    String::from_utf8(output.stdout)
        .expect("git prints UTF-8 paths")
        .lines()
        .filter(|path| !path.starts_with("vendor/") && !path.contains("/vendor/"))
        .filter(|path| root().join(path).is_file())
        .map(str::to_owned)
        .collect()
}

#[test]
fn source_has_no_comments() {
    let mut report = Vec::new();
    for path in files() {
        let Some(language) = Language::of(&path) else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(root().join(&path)) else {
            continue;
        };
        for violation in violations(language, &source) {
            report.push(format!("{path}:{}: {}", violation.line, violation.text));
        }
    }
    assert!(
        report.is_empty(),
        "\n{}\n\nComments are not allowed in this repository: they go stale, reviewers skip them \
         and future agents copy the shortcuts they excuse. Delete each comment above. If it held \
         something a reader needs, move that into the code: a clearer name, a type, an \
         `expect(\"why this holds\")` message, an `#[expect(lint, reason = \"..\")]`, a test, or \
         an error message. The one exception is a `// SAFETY:` comment directly above `unsafe`.\n",
        report.join("\n")
    );
}

fn markdown_allowed(path: &str) -> bool {
    if MARKDOWN.contains(&path) {
        return true;
    }
    let Some(name) = path.strip_prefix("docs/adr/") else {
        return false;
    };
    let (number, rest) = name.split_at(name.len().min(5));
    number.len() == 5
        && number[..4].bytes().all(|byte| byte.is_ascii_digit())
        && number.ends_with('-')
        && rest.ends_with(".md")
        && !rest.contains('/')
}

#[test]
fn markdown_is_only_the_listed_documents() {
    let unlisted: Vec<String> = files()
        .into_iter()
        .filter(|path| path.to_ascii_lowercase().ends_with(".md"))
        .filter(|path| !markdown_allowed(path))
        .collect();
    assert!(
        unlisted.is_empty(),
        "\n{}\n\nThese Markdown files are not part of the documentation set. Do not leave notes, \
         plans, reports or summaries in the repository: put the result in code, a test, an issue \
         or the conversation. A decision the user settled goes in `docs/adr/NNNN-title.md`; a new \
         standing document needs the user's approval and an entry in `MARKDOWN` in \
         `crates/lint/tests/repository.rs`.\n",
        unlisted.join("\n")
    );
}

#[test]
fn listed_markdown_exists() {
    let missing: Vec<&str> = MARKDOWN
        .iter()
        .copied()
        .filter(|path| !root().join(path).is_file())
        .collect();
    assert!(missing.is_empty(), "listed but missing: {missing:?}");
}
