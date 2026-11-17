use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    Script,
    C,
    Css,
    Lua,
    Toml,
    Shell,
    Html,
}

impl Language {
    pub fn of(path: &str) -> Option<Self> {
        let name = path.rsplit('/').next().unwrap_or(path);
        if name.starts_with("Dockerfile") {
            return Some(Self::Shell);
        }
        let extension = name.rsplit_once('.')?.1;
        Some(match extension {
            "rs" => Self::Rust,
            "js" | "mjs" | "ts" => Self::Script,
            "c" | "h" | "m" | "java" | "kts" => Self::C,
            "css" => Self::Css,
            "lua" => Self::Lua,
            "toml" => Self::Toml,
            "sh" => Self::Shell,
            "html" => Self::Html,
            _ => return None,
        })
    }
}

pub fn comments(language: Language, source: &str) -> Vec<Range<usize>> {
    let mut scanner = Scanner::new(source);
    match language {
        Language::Rust => scanner.rust(),
        Language::Script => scanner.c_like(true, true, false),
        Language::C => scanner.c_like(true, false, false),
        Language::Css => scanner.c_like(false, false, false),
        Language::Lua => scanner.lua(),
        Language::Toml => scanner.toml(),
        Language::Shell => scanner.shell(),
        Language::Html => scanner.html(),
    }
    scanner.found
}

#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub line: usize,
    pub text: String,
}

pub fn disallowed(language: Language, source: &str) -> Vec<Range<usize>> {
    let found = comments(language, source);
    let allowed = match language {
        Language::Rust => safety_comments(source, &found),
        _ => Vec::new(),
    };
    found
        .into_iter()
        .filter(|span| !allowed.contains(span))
        .collect()
}

pub fn violations(language: Language, source: &str) -> Vec<Violation> {
    disallowed(language, source)
        .into_iter()
        .map(|span| Violation {
            line: line_of(source, span.start),
            text: source[span]
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned(),
        })
        .collect()
}

pub fn line_of(source: &str, offset: usize) -> usize {
    source[..offset]
        .bytes()
        .filter(|&byte| byte == b'\n')
        .count()
        + 1
}

fn safety_comments(source: &str, found: &[Range<usize>]) -> Vec<Range<usize>> {
    let text = |ix: usize| &source[found[ix].clone()];
    let mut allowed = Vec::new();
    let mut ix = 0;
    while ix < found.len() {
        let marker = if text(ix).starts_with("// SAFETY:") {
            "// "
        } else if text(ix) == "/// # Safety" {
            "///"
        } else {
            ix += 1;
            continue;
        };
        let mut end = ix;
        while end + 1 < found.len()
            && (text(end + 1).starts_with(marker) || text(end + 1) == marker.trim_end())
            && source[found[end].end..found[end + 1].start]
                .trim()
                .is_empty()
        {
            end += 1;
        }
        let statement: String = source[found[end].end..]
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(2)
            .collect();
        if statement.contains("unsafe") {
            allowed.extend(found[ix..=end].iter().cloned());
        }
        ix = end + 1;
    }
    allowed
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
    found: Vec<Range<usize>>,
}

impl<'a> Scanner<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            bytes: source.as_bytes(),
            pos: 0,
            found: Vec::new(),
        }
    }

    fn at(&self, offset: usize) -> u8 {
        self.bytes.get(self.pos + offset).copied().unwrap_or(0)
    }

    fn starts_with(&self, text: &str) -> bool {
        self.bytes
            .get(self.pos..)
            .is_some_and(|rest| rest.starts_with(text.as_bytes()))
    }

    fn done(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn line_comment(&mut self) {
        let start = self.pos;
        while !self.done() && self.at(0) != b'\n' {
            self.pos += 1;
        }
        self.found.push(start..self.pos);
    }

    fn block_comment(&mut self, open: &str, close: &str, nested: bool) {
        let start = self.pos;
        self.pos += open.len();
        let mut depth = 1;
        while !self.done() && depth > 0 {
            if nested && self.starts_with(open) {
                depth += 1;
                self.pos += open.len();
            } else if self.starts_with(close) {
                depth -= 1;
                self.pos += close.len();
            } else {
                self.pos += 1;
            }
        }
        self.found.push(start..self.pos);
    }

    fn quoted(&mut self, quote: u8, stop_at_newline: bool) {
        self.pos += 1;
        while !self.done() {
            match self.at(0) {
                b'\\' => self.pos += 2,
                b'\n' if stop_at_newline => return,
                byte if byte == quote => {
                    self.pos += 1;
                    return;
                }
                _ => self.pos += 1,
            }
        }
    }

    fn until(&mut self, close: &str) {
        while !self.done() && !self.starts_with(close) {
            self.pos += 1;
        }
        self.pos = (self.pos + close.len()).min(self.bytes.len());
    }

    fn rust(&mut self) {
        while !self.done() {
            let byte = self.at(0);
            if self.starts_with("//") {
                self.line_comment();
            } else if self.starts_with("/*") {
                self.block_comment("/*", "*/", true);
            } else if byte == b'"' {
                self.quoted(b'"', false);
            } else if byte == b'\'' {
                self.rust_quote();
            } else if byte == b'_' || byte.is_ascii_alphabetic() {
                self.rust_word();
            } else {
                self.pos += 1;
            }
        }
    }

    fn rust_word(&mut self) {
        let start = self.pos;
        while self.at(0) == b'_' || self.at(0).is_ascii_alphanumeric() {
            self.pos += 1;
        }
        let word = &self.bytes[start..self.pos];
        let raw = matches!(word, b"r" | b"br" | b"cr");
        if raw && matches!(self.at(0), b'#' | b'"') {
            let mut hashes = 0;
            while self.at(hashes) == b'#' {
                hashes += 1;
            }
            if self.at(hashes) != b'"' {
                return;
            }
            self.pos += hashes + 1;
            let close = format!("\"{}", "#".repeat(hashes));
            self.until(&close);
        } else if matches!(word, b"b" | b"c") && self.at(0) == b'"' {
            self.quoted(b'"', false);
        } else if word == b"b" && self.at(0) == b'\'' {
            self.quoted(b'\'', false);
        }
    }

    fn rust_quote(&mut self) {
        if self.at(1) == b'\\' {
            self.pos += 3;
            while !self.done() && self.at(0) != b'\'' {
                self.pos += 1;
            }
            self.pos += 1;
            return;
        }
        let rest = self.bytes.get(self.pos + 1..).unwrap_or_default();
        let width = std::str::from_utf8(&rest[..rest.len().min(4)])
            .or_else(|error| std::str::from_utf8(&rest[..error.valid_up_to()]))
            .ok()
            .and_then(|text| text.chars().next())
            .map_or(1, char::len_utf8);
        if self.at(1 + width) == b'\'' {
            self.pos += width + 2;
        } else {
            self.pos += 1;
        }
    }

    fn c_like(&mut self, line_comments: bool, script: bool, in_template: bool) {
        let mut regex_allowed = true;
        let mut braces = 0usize;
        while !self.done() {
            let byte = self.at(0);
            if line_comments && self.starts_with("//") {
                self.line_comment();
                continue;
            }
            if self.starts_with("/*") {
                self.block_comment("/*", "*/", false);
                continue;
            }
            match byte {
                b'"' | b'\'' => {
                    self.quoted(byte, script);
                    regex_allowed = false;
                }
                b'`' if script => {
                    self.template();
                    regex_allowed = false;
                }
                b'/' if script && regex_allowed => {
                    self.regex();
                    regex_allowed = false;
                }
                b'{' => {
                    braces += 1;
                    self.pos += 1;
                    regex_allowed = true;
                }
                b'}' if in_template && braces == 0 => {
                    self.pos += 1;
                    return;
                }
                b'}' => {
                    braces = braces.saturating_sub(1);
                    self.pos += 1;
                    regex_allowed = true;
                }
                _ if byte == b'_' || byte == b'$' || byte.is_ascii_alphanumeric() => {
                    let start = self.pos;
                    while matches!(self.at(0), b'_' | b'$') || self.at(0).is_ascii_alphanumeric() {
                        self.pos += 1;
                    }
                    let word = &self.bytes[start..self.pos];
                    regex_allowed = matches!(
                        word,
                        b"return"
                            | b"typeof"
                            | b"case"
                            | b"in"
                            | b"of"
                            | b"delete"
                            | b"void"
                            | b"throw"
                            | b"new"
                            | b"else"
                            | b"do"
                            | b"yield"
                            | b"await"
                    );
                }
                _ if byte.is_ascii_whitespace() => self.pos += 1,
                b')' | b']' => {
                    self.pos += 1;
                    regex_allowed = false;
                }
                _ => {
                    self.pos += 1;
                    regex_allowed = true;
                }
            }
        }
    }

    fn template(&mut self) {
        self.pos += 1;
        while !self.done() {
            match self.at(0) {
                b'\\' => self.pos += 2,
                b'`' => {
                    self.pos += 1;
                    return;
                }
                b'$' if self.at(1) == b'{' => {
                    self.pos += 2;
                    self.c_like(true, true, true);
                }
                _ => self.pos += 1,
            }
        }
    }

    fn regex(&mut self) {
        self.pos += 1;
        let mut class = false;
        while !self.done() {
            match self.at(0) {
                b'\\' => self.pos += 2,
                b'\n' => return,
                b'[' => {
                    class = true;
                    self.pos += 1;
                }
                b']' => {
                    class = false;
                    self.pos += 1;
                }
                b'/' if !class => {
                    self.pos += 1;
                    return;
                }
                _ => self.pos += 1,
            }
        }
    }

    fn lua_long_bracket(&self, offset: usize) -> Option<usize> {
        if self.at(offset) != b'[' {
            return None;
        }
        let mut level = 0;
        while self.at(offset + 1 + level) == b'=' {
            level += 1;
        }
        (self.at(offset + 1 + level) == b'[').then_some(level)
    }

    fn lua(&mut self) {
        while !self.done() {
            let byte = self.at(0);
            if self.starts_with("--") {
                if let Some(level) = self.lua_long_bracket(2) {
                    let start = self.pos;
                    self.pos += level + 4;
                    self.until(&format!("]{}]", "=".repeat(level)));
                    self.found.push(start..self.pos);
                } else {
                    self.line_comment();
                }
            } else if let Some(level) = self.lua_long_bracket(0) {
                self.pos += level + 2;
                self.until(&format!("]{}]", "=".repeat(level)));
            } else if byte == b'"' || byte == b'\'' {
                self.quoted(byte, true);
            } else {
                self.pos += 1;
            }
        }
    }

    fn toml(&mut self) {
        while !self.done() {
            if self.starts_with("\"\"\"") {
                self.pos += 3;
                self.until("\"\"\"");
            } else if self.starts_with("'''") {
                self.pos += 3;
                self.until("'''");
            } else if self.at(0) == b'"' {
                self.quoted(b'"', true);
            } else if self.at(0) == b'\'' {
                self.pos += 1;
                self.until("'");
            } else if self.at(0) == b'#' {
                self.line_comment();
            } else {
                self.pos += 1;
            }
        }
    }

    fn shell(&mut self) {
        let mut first = true;
        while !self.done() {
            let line_start = self.pos;
            while matches!(self.at(0), b' ' | b'\t') {
                self.pos += 1;
            }
            let shebang = first && line_start == 0 && self.starts_with("#!");
            first = false;
            if self.at(0) == b'#' && !shebang {
                self.line_comment();
            }
            while !self.done() && self.at(0) != b'\n' {
                self.pos += 1;
            }
            self.pos += 1;
        }
        self.pos = self.bytes.len();
    }

    fn html(&mut self) {
        while !self.done() {
            if self.starts_with("<!--") {
                self.block_comment("<!--", "-->", false);
            } else if self.starts_with("<script") {
                self.until(">");
                self.embedded("</script", true);
            } else if self.starts_with("<style") {
                self.until(">");
                self.embedded("</style", false);
            } else {
                self.pos += 1;
            }
        }
    }

    fn embedded(&mut self, close: &str, script: bool) {
        let start = self.pos;
        let mut end = start;
        while end < self.bytes.len() && !self.bytes[end..].starts_with(close.as_bytes()) {
            end += 1;
        }
        let mut inner = Scanner {
            bytes: &self.bytes[..end],
            pos: start,
            found: Vec::new(),
        };
        inner.c_like(script, script, false);
        self.found.append(&mut inner.found);
        self.pos = end;
    }
}
