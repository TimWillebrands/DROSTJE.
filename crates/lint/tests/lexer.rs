use drostje_lint::{Language, comments, violations};

fn found(language: Language, source: &str) -> Vec<&str> {
    comments(language, source)
        .into_iter()
        .map(|span| &source[span])
        .collect()
}

#[test]
fn rust_finds_line_doc_and_nested_block_comments() {
    let source = "//! crate\n/// item\nfn f() {} // tail\n/* a /* b */ c */ let x = 1;\n";
    assert_eq!(
        found(Language::Rust, source),
        ["//! crate", "/// item", "// tail", "/* a /* b */ c */"]
    );
}

#[test]
fn rust_ignores_comment_markers_inside_literals() {
    let source = concat!(
        "let a = \"// not\";\n",
        "let b = r##\"/* \"# not */\"##;\n",
        "let c = b\"//\";\n",
        "let d = '/';\n",
        "let e = '\\'';\n",
        "fn g<'a>(x: &'a str) -> &'a str { x }\n",
        "let f = \"http://x\\\" //\";\n",
        "let r#type = 1; // real\n",
    );
    assert_eq!(found(Language::Rust, source), ["// real"]);
}

#[test]
fn rust_allows_safety_comment_runs_directly_above_unsafe() {
    let source = concat!(
        "// SAFETY: the pointer outlives the call\n",
        "// and is never aliased.\n",
        "let v = unsafe { f(p) };\n",
        "// SAFETY: not above unsafe\n",
        "let w = 1;\n",
        "// SAFETY: the descriptor is live.\n",
        "let result =\n",
        "    unsafe { poll(fd) };\n",
        "/// Prose is not allowed.\n",
        "/// # Safety\n",
        "///\n",
        "/// `read` writes at most `capacity` bytes.\n",
        "pub(crate) unsafe fn capture() {}\n",
    );
    let lines: Vec<usize> = violations(Language::Rust, source)
        .into_iter()
        .map(|violation| violation.line)
        .collect();
    assert_eq!(lines, [4, 9]);
}

#[test]
fn script_skips_strings_templates_and_regexes() {
    let source = concat!(
        "const a = 'http://x'; // one\n",
        "const b = `${c // two\n}/* not */`;\n",
        "const r = /\\/\\/[/*]/g; /* three */\n",
        "const q = x / 2 / y; // four\n",
    );
    assert_eq!(
        found(Language::Script, source),
        ["// one", "// two", "/* three */", "// four"]
    );
}

#[test]
fn css_has_only_block_comments() {
    let source = "a { background: url(http://x); } /* one */\n";
    assert_eq!(found(Language::Css, source), ["/* one */"]);
}

#[test]
fn lua_finds_short_and_long_comments_outside_strings() {
    let source = "local s = '--' -- one\nlocal t = [[--]]\n--[==[ two ]==]\n";
    assert_eq!(found(Language::Lua, source), ["-- one", "--[==[ two ]==]"]);
}

#[test]
fn toml_ignores_hashes_in_strings() {
    let source = "a = \"#x\" # one\nb = '#'\nc = \"\"\"\n# no\n\"\"\"\n# two\n";
    assert_eq!(found(Language::Toml, source), ["# one", "# two"]);
}

#[test]
fn shell_keeps_shebang_and_flags_full_line_comments() {
    let source = "#!/bin/sh\n# one\necho \"$#\" ${#x}\n  # two\n";
    assert_eq!(found(Language::Shell, source), ["# one", "# two"]);
}

#[test]
fn html_finds_markup_script_and_style_comments() {
    let source = "<!-- one --><style>/* two */</style><script>// three\n</script>";
    assert_eq!(
        found(Language::Html, source),
        ["<!-- one -->", "/* two */", "// three"]
    );
}

#[test]
fn language_follows_file_name() {
    assert_eq!(Language::of("crates/a/src/lib.rs"), Some(Language::Rust));
    assert_eq!(Language::of("Dockerfile.host"), Some(Language::Shell));
    assert_eq!(Language::of("assets/x.mjs"), Some(Language::Script));
    assert_eq!(
        Language::of("mobile/android/app/src/main/java/app/drostje/phone/ScanActivity.java"),
        Some(Language::C)
    );
    assert_eq!(
        Language::of("mobile/android/build.gradle.kts"),
        Some(Language::C)
    );
    assert_eq!(Language::of("README.md"), None);
}
