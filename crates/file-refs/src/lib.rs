use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FileReference {
    pub path: String,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
}

impl FileReference {
    pub fn label(&self) -> String {
        match (self.line, self.column) {
            (Some(line), Some(column)) => format!("{}:{line}:{column}", self.path),
            (Some(line), None) => format!("{}:{line}", self.path),
            _ => self.path.clone(),
        }
    }

    pub fn resolve(&self, base: &Path, home: Option<&Path>) -> PathBuf {
        if let Some(rest) = self.path.strip_prefix("~/")
            && let Some(home) = home
        {
            return home.join(rest);
        }
        let path = Path::new(&self.path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path.strip_prefix("./").unwrap_or(path))
        }
    }
}

const BARE_EXTENSIONS: &[&str] = &[
    "astro",
    "bash",
    "bat",
    "c",
    "cc",
    "cfg",
    "cjs",
    "clj",
    "cmake",
    "conf",
    "cpp",
    "cs",
    "css",
    "csv",
    "cts",
    "dart",
    "diff",
    "dockerfile",
    "el",
    "elm",
    "env",
    "erl",
    "ex",
    "exs",
    "fish",
    "fs",
    "gitignore",
    "go",
    "gradle",
    "graphql",
    "h",
    "hpp",
    "hs",
    "htm",
    "html",
    "ini",
    "java",
    "jl",
    "js",
    "json",
    "json5",
    "jsonc",
    "jsx",
    "kt",
    "kts",
    "less",
    "lock",
    "log",
    "lua",
    "m",
    "md",
    "mdx",
    "mjs",
    "ml",
    "mli",
    "mm",
    "mts",
    "nix",
    "php",
    "pl",
    "plist",
    "proto",
    "ps1",
    "py",
    "r",
    "rb",
    "rs",
    "sass",
    "scala",
    "scss",
    "sh",
    "sql",
    "svelte",
    "swift",
    "tex",
    "tf",
    "toml",
    "ts",
    "tsv",
    "tsx",
    "txt",
    "vim",
    "vue",
    "xml",
    "yaml",
    "yml",
    "zig",
    "zsh",
];

const BARE_NAMES: &[&str] = &[
    "AGENTS.md",
    "Brewfile",
    "CHANGELOG",
    "CODEOWNERS",
    "Dockerfile",
    "Gemfile",
    "Justfile",
    "LICENSE",
    "Makefile",
    "Procfile",
    "README",
    "Rakefile",
    "Vagrantfile",
    "justfile",
    "makefile",
];

pub fn parse(text: &str) -> Option<FileReference> {
    let text = text.trim();
    if text.is_empty() || text.len() > 1024 || text.chars().any(char::is_whitespace) {
        return None;
    }
    if text.starts_with('-') || text.contains("://") {
        return None;
    }
    let (path, line, column) = split_position(text);
    valid_path(path).then(|| FileReference {
        path: path.to_owned(),
        line,
        column,
    })
}

pub fn parse_link(url: &str) -> Option<FileReference> {
    if let Some(rest) = url.strip_prefix("file://") {
        let (path, fragment) = rest.split_once('#').unwrap_or((rest, ""));
        let path = percent_decode(path)?;
        if !path.starts_with('/') {
            return None;
        }
        let mut reference = parse(&path).or_else(|| {
            (!path.ends_with('/')).then(|| FileReference {
                path: path.clone(),
                line: None,
                column: None,
            })
        })?;
        if let Some(line) = fragment_line(fragment) {
            reference.line = Some(line);
            reference.column = None;
        }
        return Some(reference);
    }
    if url.contains(':')
        && !url.contains('/')
        && url
            .split(':')
            .nth(1)
            .is_some_and(|rest| !rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
    {
        return None;
    }
    parse(&percent_decode(url)?)
}

pub fn tool_reference(name: &str, input: &Value) -> Option<FileReference> {
    let field = |key: &str| input[key].as_str().filter(|value| !value.trim().is_empty());
    let path = match name.to_ascii_lowercase().as_str() {
        "read" | "write" => field("path").or_else(|| field("file_path")),
        "edit" | "multiedit" => field("input")
            .and_then(|patch| patch_paths(patch).into_iter().next())
            .or_else(|| field("path"))
            .or_else(|| field("file_path")),
        _ => None,
    }?;
    let line = (name.eq_ignore_ascii_case("read"))
        .then(|| input["offset"].as_u64())
        .flatten()
        .and_then(|offset| u32::try_from(offset).ok())
        .filter(|offset| *offset > 0);
    Some(FileReference {
        path: path.to_owned(),
        line,
        column: None,
    })
}

pub fn patch_paths(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .filter(|line| line.starts_with('[') && line.ends_with(']'))
        .map(|line| {
            let inner = &line[1..line.len() - 1];
            inner.rsplit_once('#').map_or(inner, |(path, _)| path)
        })
        .filter(|path| !path.is_empty())
        .collect()
}

pub fn inline_code_references(text: &str) -> Vec<FileReference> {
    let mut references = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find('`') {
            let ticks = rest[start..].chars().take_while(|c| *c == '`').count();
            let after = &rest[start + ticks..];
            let fence = &rest[start..start + ticks];
            let Some(end) = after.find(fence) else {
                break;
            };
            if let Some(reference) = parse(&after[..end]) {
                references.push(reference);
            }
            rest = &after[end + ticks..];
        }
    }
    references
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InspectError {
    #[error("{0} does not exist")]
    Missing(String),
    #[error("{0} is a directory, not a file")]
    Directory(String),
    #[error("{0} is not a text file")]
    Binary(String),
    #[error("{path} could not be read: {error}")]
    Unreadable { path: String, error: String },
}

pub fn inspect(path: &Path) -> Result<(), InspectError> {
    let shown = path.display().to_string();
    let unreadable = |error: std::io::Error| InspectError::Unreadable {
        path: shown.clone(),
        error: error.to_string(),
    };
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(InspectError::Missing(shown));
        }
        Err(error) => return Err(unreadable(error)),
    };
    if metadata.is_dir() {
        return Err(InspectError::Directory(shown));
    }
    let mut head = Vec::with_capacity(8192);
    fs::File::open(path)
        .and_then(|file| file.take(8192).read_to_end(&mut head))
        .map_err(unreadable)?;
    if head.contains(&0) {
        return Err(InspectError::Binary(shown));
    }
    match std::str::from_utf8(&head) {
        Ok(_) => Ok(()),
        Err(error) if error.error_len().is_none() => Ok(()),
        Err(_) => Err(InspectError::Binary(shown)),
    }
}

fn split_position(text: &str) -> (&str, Option<u32>, Option<u32>) {
    if let Some((path, fragment)) = text.rsplit_once('#')
        && let Some(line) = fragment_line(fragment)
    {
        return (path, Some(line), None);
    }
    let number = |value: &str| {
        (!value.is_empty() && value.chars().all(|c| c.is_ascii_digit()))
            .then(|| value.parse::<u32>().ok())
            .flatten()
            .filter(|value| *value > 0)
    };
    let Some((head, last)) = text.rsplit_once(':') else {
        return (text, None, None);
    };
    if let Some((start, end)) = last.split_once('-')
        && let (Some(line), Some(_)) = (number(start), number(end))
    {
        return (head, Some(line), None);
    }
    let Some(last) = number(last) else {
        return (text, None, None);
    };
    match head.rsplit_once(':') {
        Some((path, line)) if number(line).is_some() => (path, number(line), Some(last)),
        _ => (head, Some(last), None),
    }
}

fn fragment_line(fragment: &str) -> Option<u32> {
    let digits: String = fragment
        .strip_prefix('L')?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|line| *line > 0)
}

fn valid_path(path: &str) -> bool {
    if path.is_empty() || path.ends_with('/') || path.contains("//") {
        return false;
    }
    let allowed =
        |c: char| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '@' | '+');
    if !path.chars().all(allowed) {
        return false;
    }
    if path.contains('~') && !path.starts_with("~/") {
        return false;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.is_empty() || name.chars().all(|c| c == '.') {
        return false;
    }
    if BARE_NAMES.contains(&name) {
        return true;
    }
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    if extension.is_empty() || !extension.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    if !extension.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let extension = extension.to_ascii_lowercase();
    if path.contains('/') {
        return !stem.is_empty() || name.len() > 1;
    }
    BARE_EXTENSIONS.contains(&extension.as_str()) && (!stem.is_empty() || name.starts_with('.'))
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reference(path: &str, line: Option<u32>, column: Option<u32>) -> Option<FileReference> {
        Some(FileReference {
            path: path.into(),
            line,
            column,
        })
    }

    #[test]
    fn inline_code_paths_parse_with_their_positions() {
        assert_eq!(
            parse("src/browser_tools.rs:149-210"),
            reference("src/browser_tools.rs", Some(149), None)
        );
        assert_eq!(
            parse("assets/browser-tools/tools.json"),
            reference("assets/browser-tools/tools.json", None, None)
        );
        assert_eq!(
            parse("src/app.rs:42"),
            reference("src/app.rs", Some(42), None)
        );
        assert_eq!(
            parse("src/app.rs:42:7"),
            reference("src/app.rs", Some(42), Some(7))
        );
        assert_eq!(
            parse("src/app.rs#L10-L20"),
            reference("src/app.rs", Some(10), None)
        );
        assert_eq!(parse("Cargo.toml"), reference("Cargo.toml", None, None));
        assert_eq!(parse("Makefile:3"), reference("Makefile", Some(3), None));
        assert_eq!(parse(".gitignore"), reference(".gitignore", None, None));
        assert_eq!(
            parse("/Users/me/notes.txt"),
            reference("/Users/me/notes.txt", None, None)
        );
        assert_eq!(parse("~/.zshrc.zsh"), reference("~/.zshrc.zsh", None, None));
        assert_eq!(parse("./build.rs"), reference("./build.rs", None, None));
    }

    #[test]
    fn code_that_is_not_a_path_stays_code() {
        for text in [
            "cx.notify",
            "self.state.read",
            "origin/main",
            "src/ui",
            "src/ui/",
            "std::fmt",
            "foo()",
            "Vec<T>",
            "cargo test",
            "v1.2",
            "0.6.6",
            "https://example.com/a.rs",
            "--release",
            "a.rs:0",
            "",
            "..",
            "a//b.rs",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn link_targets_parse_files_and_ignore_other_schemes() {
        assert_eq!(
            parse_link("file:///tmp/My%20Notes/a.txt#L4"),
            reference("/tmp/My Notes/a.txt", Some(4), None)
        );
        assert_eq!(
            parse_link("file:///etc/hosts"),
            reference("/etc/hosts", None, None)
        );
        assert_eq!(
            parse_link("src/app.rs#L12"),
            reference("src/app.rs", Some(12), None)
        );
        assert_eq!(parse_link("mailto:me@example.com"), None);
        assert_eq!(parse_link("https://example.com/x.rs"), None);
        assert_eq!(parse_link("file://relative/a.txt"), None);
    }

    #[test]
    fn tool_calls_name_the_file_they_touched() {
        assert_eq!(
            tool_reference("Read", &json!({"file_path": "/p/src/a.rs", "offset": 120})),
            reference("/p/src/a.rs", Some(120), None)
        );
        assert_eq!(
            tool_reference("write", &json!({"path": "notes.md"})),
            reference("notes.md", None, None)
        );
        assert_eq!(
            tool_reference("edit", &json!({"input": "[src/lib.rs#69AE]\n-old\n+new"})),
            reference("src/lib.rs", None, None)
        );
        assert_eq!(
            tool_reference("bash", &json!({"command": "cat a.rs"})),
            None
        );
        assert_eq!(tool_reference("read", &json!({"path": " "})), None);
    }

    #[test]
    fn inline_code_scan_skips_fences_and_non_paths() {
        let text = "See `src/a.rs:3` and ``b/c.toml``, not `cx.notify`.\n```\n`src/in_fence.rs`\n```\nThen `README.md`.";
        let found: Vec<_> = inline_code_references(text)
            .into_iter()
            .map(|reference| reference.label())
            .collect();
        assert_eq!(found, ["src/a.rs:3", "b/c.toml", "README.md"]);
    }

    #[test]
    fn references_resolve_against_the_working_tree() {
        let base = Path::new("/work/tree");
        let home = Path::new("/home/me");
        let resolve = |path: &str| {
            FileReference {
                path: path.into(),
                line: None,
                column: None,
            }
            .resolve(base, Some(home))
        };
        assert_eq!(resolve("src/a.rs"), Path::new("/work/tree/src/a.rs"));
        assert_eq!(resolve("./src/a.rs"), Path::new("/work/tree/src/a.rs"));
        assert_eq!(resolve("/etc/hosts"), Path::new("/etc/hosts"));
        assert_eq!(resolve("~/.zshrc"), Path::new("/home/me/.zshrc"));
    }

    #[test]
    fn inspect_accepts_text_and_rejects_binaries_directories_and_missing_files() {
        let dir = std::env::temp_dir().join(format!("drostje-file-refs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let text = dir.join("a.rs");
        fs::write(&text, "fn main() {}\n").unwrap();
        assert_eq!(inspect(&text), Ok(()));
        let binary = dir.join("a.bin");
        fs::write(&binary, [0x7f, b'E', b'L', b'F', 0, 1]).unwrap();
        assert!(matches!(inspect(&binary), Err(InspectError::Binary(_))));
        assert!(matches!(inspect(&dir), Err(InspectError::Directory(_))));
        assert!(matches!(
            inspect(&dir.join("missing.rs")),
            Err(InspectError::Missing(_))
        ));
        let split = dir.join("split.txt");
        let mut bytes = vec![b'a'; 8191];
        bytes.extend("é".as_bytes());
        fs::write(&split, bytes).unwrap();
        assert_eq!(inspect(&split), Ok(()));
        fs::remove_dir_all(&dir).unwrap();
    }
}
