//! Unified diff text, as `platform::working_tree_diff` produces it, split into display rows.
//!
//! Pure and lossless for reading: every line of the patch lands in exactly one row, except the
//! per-file Git headers (`diff --git`, `index`, `---`, `+++`, mode and rename lines), which fold
//! into that file's [`RowKind::File`] row. Text Git did not write in a recognised shape stays a
//! [`RowKind::Note`] so nothing is dropped.

#[cfg(feature = "desktop")]
use gpui_kit::SharedString;
#[cfg(not(feature = "desktop"))]
type SharedString = std::sync::Arc<str>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    pub rows: Vec<Row>,
    /// The same rows paired for a side-by-side view; every row index appears exactly once,
    /// in order.
    pub split: Vec<SplitRow>,
}

/// One line of the side-by-side view, by index into [`Patch::rows`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitRow {
    /// A file, hunk or note row spanning both sides.
    Full(usize),
    /// The old side and the new side of one line. A context line is both sides; a removed line
    /// pairs with the added line at the same offset of the change it belongs to.
    Pair {
        old: Option<usize>,
        new: Option<usize>,
    },
}

impl SplitRow {
    /// The first row this line shows, which stands for it when switching layouts.
    pub fn first(self) -> usize {
        match self {
            Self::Full(ix) => ix,
            Self::Pair { old, new } => match (old, new) {
                (Some(old), Some(new)) => old.min(new),
                (Some(ix), None) | (None, Some(ix)) => ix,
                // `split` never builds an empty pair.
                (None, None) => 0,
            },
        }
    }

    /// Whether this line shows row `ix`.
    pub fn contains(self, ix: usize) -> bool {
        match self {
            Self::Full(row) => row == ix,
            Self::Pair { old, new } => old == Some(ix) || new == Some(ix),
        }
    }
}

/// Where a row lands in the new version of its file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// The new path, as the file row names it.
    pub path: String,
    /// 1-based line in the new file.
    pub line: u32,
}

/// Why a row has no place in the working tree.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LocationError {
    #[error("this row belongs to no file")]
    NoFile,
    #[error("{0} was deleted")]
    Deleted(String),
}

impl Patch {
    /// The line in the working tree file that row `ix` shows or, for a removed line, where it
    /// was removed: the next surviving line of its hunk, else the line after the last one.
    pub fn location(&self, ix: usize) -> Result<Location, LocationError> {
        if ix >= self.rows.len() {
            return Err(LocationError::NoFile);
        }
        let Some(file_ix) = self.rows[..=ix]
            .iter()
            .rposition(|row| matches!(row.kind, RowKind::File(_)))
        else {
            return Err(LocationError::NoFile);
        };
        let file = &self.rows[file_ix];
        let path = file.text.to_string();
        if let RowKind::File(FileHeader {
            change: FileChange::Deleted,
            ..
        }) = file.kind
        {
            return Err(LocationError::Deleted(path));
        }
        // This row and the rest of its file.
        let after = || {
            self.rows[ix..]
                .iter()
                .enumerate()
                .take_while(|(offset, row)| *offset == 0 || !matches!(row.kind, RowKind::File(_)))
                .map(|(_, row)| row)
        };
        let row = &self.rows[ix];
        let line = match row.kind {
            RowKind::Added | RowKind::Context => row.new_line,
            RowKind::Hunk => Some(hunk_starts(&row.text).1),
            RowKind::File(_) => after().find_map(|row| row.new_line),
            RowKind::Removed | RowKind::Note => after()
                .skip(1)
                .take_while(|row| row.kind != RowKind::Hunk)
                .find_map(|row| row.new_line)
                .or_else(|| {
                    self.rows[file_ix + 1..ix]
                        .iter()
                        .rev()
                        .find_map(|row| match row.kind {
                            RowKind::Hunk => Some(hunk_starts(&row.text).1),
                            _ => row.new_line.map(|line| line + 1),
                        })
                }),
        };
        Ok(Location {
            path,
            line: line.unwrap_or(1).max(1),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    /// Line content without the leading `+`, `-` or space; tabs expanded to four spaces.
    pub text: SharedString,
    /// 1-based line in the old file for removed and context lines.
    pub old_line: Option<u32>,
    /// 1-based line in the new file for added and context lines.
    pub new_line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// The start of one file's changes; `text` is the new path (the old one when deleted).
    File(FileHeader),
    /// An `@@ … @@` hunk header; `text` is the whole header line.
    Hunk,
    Added,
    Removed,
    Context,
    /// Anything else: `\ No newline at end of file`, omitted-content notices, truncation.
    Note,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHeader {
    pub change: FileChange,
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChange {
    Added,
    Deleted,
    Modified,
    Renamed { from: String },
}

pub fn parse(text: &str) -> Patch {
    let mut rows = Vec::new();
    // Index of the current file row, and whether the parser is inside its hunks yet.
    let mut file: Option<usize> = None;
    let mut in_hunk = false;
    let (mut old, mut new) = (0u32, 0u32);
    for line in text.lines() {
        if let Some(paths) = line.strip_prefix("diff --git ") {
            file = Some(rows.len());
            in_hunk = false;
            rows.push(Row {
                kind: RowKind::File(FileHeader {
                    change: FileChange::Modified,
                    additions: 0,
                    deletions: 0,
                    binary: false,
                }),
                text: git_paths(paths).into(),
                old_line: None,
                new_line: None,
            });
            continue;
        }
        if let Some(header) = line.strip_prefix("@@ ")
            && file.is_some()
        {
            in_hunk = true;
            (old, new) = hunk_starts(header);
            rows.push(row(RowKind::Hunk, line, None, None));
            continue;
        }
        let Some(file_ix) = file else {
            if !line.trim().is_empty() {
                rows.push(row(RowKind::Note, line, None, None));
            }
            continue;
        };
        if !in_hunk {
            if !fold_header(&mut rows[file_ix], line) {
                rows.push(row(RowKind::Note, line, None, None));
            }
            continue;
        }
        let (kind, content) = match line.as_bytes().first() {
            Some(b'+') => (RowKind::Added, &line[1..]),
            Some(b'-') => (RowKind::Removed, &line[1..]),
            Some(b' ') => (RowKind::Context, &line[1..]),
            // Git writes an empty context line as a lone space, so a blank line ends the
            // patch proper; what follows (the truncation notice) belongs to no file.
            None => {
                file = None;
                in_hunk = false;
                continue;
            }
            Some(_) => (RowKind::Note, line),
        };
        let (old_line, new_line) = match kind {
            RowKind::Added => (None, Some(next(&mut new))),
            RowKind::Removed => (Some(next(&mut old)), None),
            RowKind::Context => (Some(next(&mut old)), Some(next(&mut new))),
            _ => (None, None),
        };
        if let RowKind::File(header) = &mut rows[file_ix].kind {
            match kind {
                RowKind::Added => header.additions += 1,
                RowKind::Removed => header.deletions += 1,
                _ => {}
            }
        }
        rows.push(row(kind, content, old_line, new_line));
    }
    let split = split(&rows);
    Patch { rows, split }
}

/// Pairs each run of removed lines with the added lines that follow it, line by line.
fn split(rows: &[Row]) -> Vec<SplitRow> {
    let mut split = Vec::with_capacity(rows.len());
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    let flush = |split: &mut Vec<SplitRow>, removed: &mut Vec<usize>, added: &mut Vec<usize>| {
        for offset in 0..removed.len().max(added.len()) {
            split.push(SplitRow::Pair {
                old: removed.get(offset).copied(),
                new: added.get(offset).copied(),
            });
        }
        removed.clear();
        added.clear();
    };
    for (ix, row) in rows.iter().enumerate() {
        match row.kind {
            RowKind::Removed => {
                // A removal after additions starts a new change.
                if !added.is_empty() {
                    flush(&mut split, &mut removed, &mut added);
                }
                removed.push(ix);
            }
            RowKind::Added => added.push(ix),
            RowKind::Context => {
                flush(&mut split, &mut removed, &mut added);
                split.push(SplitRow::Pair {
                    old: Some(ix),
                    new: Some(ix),
                });
            }
            RowKind::File(_) | RowKind::Hunk | RowKind::Note => {
                flush(&mut split, &mut removed, &mut added);
                split.push(SplitRow::Full(ix));
            }
        }
    }
    flush(&mut split, &mut removed, &mut added);
    split
}

fn row(kind: RowKind, text: &str, old_line: Option<u32>, new_line: Option<u32>) -> Row {
    Row {
        kind,
        text: text.replace('\t', "    ").into(),
        old_line,
        new_line,
    }
}

fn next(counter: &mut u32) -> u32 {
    let line = *counter;
    *counter += 1;
    line
}

/// The new path from `a/<old> b/<new>`; Git quotes paths with unusual characters, kept as is.
fn git_paths(paths: &str) -> String {
    match paths.rfind(" b/") {
        Some(ix) => paths[ix + 3..].to_owned(),
        None => paths.to_owned(),
    }
}

/// Start lines from `-a,b +c,d @@ …`; a malformed header numbers from 1.
fn hunk_starts(header: &str) -> (u32, u32) {
    let start = |sign: char| {
        header
            .split_whitespace()
            .find_map(|part| part.strip_prefix(sign))
            .and_then(|range| range.split(',').next())
            .and_then(|line| line.parse::<u32>().ok())
            .unwrap_or(1)
    };
    let old = start('-');
    let new = start('+');
    (old, new)
}

/// Git's extended header lines, which say nothing a file row does not show.
const QUIET_HEADERS: [&str; 9] = [
    "index ",
    "old mode ",
    "new mode ",
    "similarity index ",
    "dissimilarity index ",
    "copy from ",
    "copy to ",
    "--- ",
    "+++ ",
];

/// Folds a Git header line into the file row; `false` when the line is not one.
fn fold_header(file: &mut Row, line: &str) -> bool {
    let RowKind::File(header) = &mut file.kind else {
        return false;
    };
    if line.starts_with("new file mode") {
        header.change = FileChange::Added;
    } else if line.starts_with("deleted file mode") {
        header.change = FileChange::Deleted;
    } else if let Some(from) = line.strip_prefix("rename from ") {
        header.change = FileChange::Renamed { from: from.into() };
    } else if let Some(to) = line.strip_prefix("rename to ") {
        file.text = to.to_owned().into();
    } else if let Some(path) = line.strip_prefix("+++ b/") {
        file.text = path.to_owned().into();
    } else if let Some(path) = line.strip_prefix("--- a/") {
        // A deletion has no new path; the old one names the file.
        if header.change == FileChange::Deleted {
            file.text = path.to_owned().into();
        }
    } else if line.starts_with("Binary files ") || line.starts_with("Binary untracked file") {
        header.binary = true;
    } else {
        return QUIET_HEADERS.iter().any(|prefix| line.starts_with(prefix));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(patch: &Patch) -> Vec<&'static str> {
        patch
            .rows
            .iter()
            .map(|row| match row.kind {
                RowKind::File(_) => "file",
                RowKind::Hunk => "hunk",
                RowKind::Added => "+",
                RowKind::Removed => "-",
                RowKind::Context => " ",
                RowKind::Note => "note",
            })
            .collect()
    }

    fn header(row: &Row) -> &FileHeader {
        match &row.kind {
            RowKind::File(header) => header,
            other => panic!("expected a file row, got {other:?}"),
        }
    }

    #[test]
    fn modified_file_numbers_lines_and_counts_changes() {
        let patch = parse(
            "diff --git a/src/lib.rs b/src/lib.rs\nindex 1..2 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,3 +10,3 @@ fn main() {\n keep\n-\tbefore\n+after\n\\ No newline at end of file",
        );
        assert_eq!(
            kinds(&patch),
            ["file", "hunk", " ", "-", "+", "note"],
            "headers fold into the file row"
        );
        let file = &patch.rows[0];
        assert_eq!(file.text.as_ref(), "src/lib.rs");
        assert_eq!(header(file).change, FileChange::Modified);
        assert_eq!((header(file).additions, header(file).deletions), (1, 1));
        assert_eq!(patch.rows[1].text.as_ref(), "@@ -10,3 +10,3 @@ fn main() {");
        assert_eq!(
            (patch.rows[2].old_line, patch.rows[2].new_line),
            (Some(10), Some(10))
        );
        assert_eq!(patch.rows[3].text.as_ref(), "    before");
        assert_eq!(
            (patch.rows[3].old_line, patch.rows[3].new_line),
            (Some(11), None)
        );
        assert_eq!(
            (patch.rows[4].old_line, patch.rows[4].new_line),
            (None, Some(11))
        );
        assert_eq!(patch.rows[5].text.as_ref(), "\\ No newline at end of file");
    }

    #[test]
    fn added_deleted_renamed_and_binary_files_are_named() {
        let patch = parse(concat!(
            "diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,1 @@\n+hello\n",
            "diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\n--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n",
            "diff --git a/old name.rs b/new name.rs\nsimilarity index 100%\nrename from old name.rs\nrename to new name.rs\n",
            "diff --git a/logo.png b/logo.png\nnew file mode 100644\n--- /dev/null\n+++ b/logo.png\nBinary untracked file; content omitted.\n",
        ));
        let files: Vec<_> = patch
            .rows
            .iter()
            .filter(|row| matches!(row.kind, RowKind::File(_)))
            .collect();
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].text.as_ref(), "new.txt");
        assert_eq!(header(files[0]).change, FileChange::Added);
        assert_eq!(header(files[0]).additions, 1);
        assert_eq!(files[1].text.as_ref(), "gone.txt");
        assert_eq!(header(files[1]).change, FileChange::Deleted);
        assert_eq!(header(files[1]).deletions, 1);
        assert_eq!(files[2].text.as_ref(), "new name.rs");
        assert_eq!(
            header(files[2]).change,
            FileChange::Renamed {
                from: "old name.rs".into()
            }
        );
        assert_eq!(files[3].text.as_ref(), "logo.png");
        assert!(header(files[3]).binary);
    }

    #[test]
    fn text_outside_files_is_kept_as_notes() {
        let patch = parse(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n\nDiff truncated at 512 KiB.",
        );
        assert_eq!(kinds(&patch), ["file", "hunk", "-", "+", "note"]);
        assert_eq!(
            patch.rows.last().unwrap().text.as_ref(),
            "Diff truncated at 512 KiB."
        );
        assert_eq!(kinds(&parse("unexpected preamble\n")), ["note"]);
        let large = parse(
            "diff --git a/big.log b/big.log\nnew file mode 100644\n--- /dev/null\n+++ b/big.log\nUntracked file is larger than 128 KiB; content omitted.\n",
        );
        assert_eq!(kinds(&large), ["file", "note"]);
        assert!(parse("").rows.is_empty());
    }

    #[test]
    fn side_by_side_pairs_each_removal_with_the_addition_at_its_offset() {
        let patch = parse(concat!(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,6 +1,6 @@\n",
            " keep\n-old 1\n-old 2\n-old 3\n+new 1\n+new 2\n same\n+only new\n-only old\n",
        ));
        assert_eq!(
            kinds(&patch),
            ["file", "hunk", " ", "-", "-", "-", "+", "+", " ", "+", "-"]
        );
        let pair = |old: Option<usize>, new: Option<usize>| SplitRow::Pair { old, new };
        assert_eq!(
            patch.split,
            [
                SplitRow::Full(0),
                SplitRow::Full(1),
                pair(Some(2), Some(2)),
                pair(Some(3), Some(6)),
                pair(Some(4), Some(7)),
                pair(Some(5), None),
                pair(Some(8), Some(8)),
                // An addition before a removal is a change of its own.
                pair(None, Some(9)),
                pair(Some(10), None),
            ]
        );
        let mut shown: Vec<_> = patch
            .split
            .iter()
            .flat_map(|row| match *row {
                SplitRow::Full(ix) => vec![ix],
                SplitRow::Pair { old, new } if old == new => old.into_iter().collect(),
                SplitRow::Pair { old, new } => old.into_iter().chain(new).collect(),
            })
            .collect();
        shown.sort_unstable();
        assert_eq!(
            shown,
            (0..patch.rows.len()).collect::<Vec<_>>(),
            "every row once"
        );
        assert_eq!(patch.split[3].first(), 3);
        assert!(patch.split[3].contains(6));
        assert!(!patch.split[3].contains(5));
    }

    #[test]
    fn every_row_locates_a_line_in_the_working_tree() {
        let patch = parse(concat!(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n",
            "@@ -10,4 +10,3 @@ fn main() {\n keep\n-gone\n+came\n-also gone\n same\n",
            "@@ -40,2 +39,1 @@\n tail\n-last\n",
            "diff --git a/b.rs b/b.rs\ndeleted file mode 100644\n--- a/b.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n",
            "diff --git a/c.rs b/c.rs\n--- a/c.rs\n+++ b/c.rs\n@@ -1 +0,0 @@\n-only\n",
        ));
        let at = |ix: usize| {
            patch
                .location(ix)
                .map(|location| (location.path, location.line))
        };
        let file = |line: u32| Ok(("src/a.rs".to_owned(), line));
        assert_eq!(at(0), file(10), "a file opens at its first changed hunk");
        assert_eq!(at(1), file(10), "a hunk opens at its new start");
        assert_eq!(at(2), file(10));
        assert_eq!(
            at(3),
            file(11),
            "a removal opens where the next line survives"
        );
        assert_eq!(at(4), file(11));
        assert_eq!(at(5), file(12));
        assert_eq!(at(6), file(12));
        assert_eq!(at(7), file(39));
        assert_eq!(
            at(9),
            file(40),
            "a removal at a hunk's end opens after its last line"
        );
        assert_eq!(at(10), Err(LocationError::Deleted("b.rs".into())));
        assert_eq!(at(12), Err(LocationError::Deleted("b.rs".into())));
        assert_eq!(
            at(15),
            Ok(("c.rs".to_owned(), 1)),
            "emptied files open at the top"
        );
        assert_eq!(at(99), Err(LocationError::NoFile));
        assert_eq!(parse("preamble\n").location(0), Err(LocationError::NoFile));
    }
}
