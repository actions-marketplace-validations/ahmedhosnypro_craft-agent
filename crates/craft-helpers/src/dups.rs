//! Duplicate-code scanning: the tree walk and line normalization.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::Truncation;

/// One scanned source file; anything past this size is a skip note.
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// Directory depth; deeper trees get a skip note.
pub const MAX_DEPTH: usize = 48;
/// Source files per root; past the cap the rest of the root is skipped.
pub const MAX_FILES_PER_ROOT: usize = 4_000;
/// Total hashed windows across the scan; past the cap it truncates.
pub const MAX_WINDOWS: usize = 250_000;
/// `--min-lines` ceiling.
pub const MAX_MIN_LINES: usize = 200;
/// `--max-candidates` ceiling.
pub const MAX_CANDIDATES: usize = 500;

const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "java", "c", "h", "cc", "cpp", "hpp", "cs",
    "go", "rb", "php", "swift", "kt", "scala", "sh",
];
const IGNORED_DIRS: &[&str] = &[".git", "target", "node_modules"];

#[derive(Debug, Clone)]
pub struct DupsQuery {
    pub roots: Vec<PathBuf>,
    pub min_lines: usize,
    pub max_candidates: usize,
    pub cross_only: bool,
}

#[derive(Debug, Serialize)]
pub struct DupsReport {
    pub candidates: Vec<Candidate>,
    pub skipped: Vec<String>,
    pub truncated: Vec<Truncation>,
}

#[derive(Debug, Serialize)]
pub struct Candidate {
    pub lines: usize,
    pub occurrences: Vec<Occurrence>,
}

#[derive(Debug, Serialize)]
pub struct Occurrence {
    pub root: String,
    pub file: String,
    pub line: usize,
}

pub use crate::dups_scan::scan_dups;

/// A kept line: 1-based original line number and its normalized form.
pub(crate) struct KeptLine {
    pub line: usize,
    pub text: String,
}

/// One source file's normalized lines, root-relative path.
pub(crate) struct ScanFile {
    pub rel: String,
    pub kept: Vec<KeptLine>,
}

/// Normalize one raw line: trim, collapse inner whitespace; blank and
/// pure-comment lines are dropped.
pub(crate) fn normalize_line(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || is_comment(trimmed) {
        return None;
    }
    Some(trimmed.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn is_comment(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
}

/// Walk one root in sorted order; every anomaly is a note, never an Err.
pub(crate) fn collect_files(
    root: &Path,
    skipped: &mut Vec<String>,
    truncated: &mut Vec<Truncation>,
) -> Vec<ScanFile> {
    if !root.is_dir() {
        skipped.push(format!("root {}: not a directory, skipped", root.display()));
        return Vec::new();
    }
    let mut walk = Walk {
        root,
        count: 0,
        capped: false,
        files: Vec::new(),
        skipped: Vec::new(),
        truncated: Vec::new(),
    };
    walk.walk(root, 0);
    skipped.append(&mut walk.skipped);
    truncated.append(&mut walk.truncated);
    walk.files
}

struct Walk<'a> {
    root: &'a Path,
    count: usize,
    capped: bool,
    files: Vec<ScanFile>,
    skipped: Vec<String>,
    truncated: Vec<Truncation>,
}

impl Walk<'_> {
    fn walk(&mut self, dir: &Path, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            self.skipped.push(format!(
                "{}: unreadable directory, skipped",
                rel_of(self.root, dir)
            ));
            return;
        };
        let mut names: Vec<std::ffi::OsString> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .collect();
        names.sort();
        for name in names {
            if self.count >= MAX_FILES_PER_ROOT {
                self.record_cap();
                return;
            }
            self.visit(dir, depth, name);
        }
    }

    fn record_cap(&mut self) {
        if self.capped {
            return;
        }
        self.capped = true;
        self.skipped.push(format!(
            "root {}: file cap reached; remaining files skipped",
            self.root.display()
        ));
        self.truncated.push(Truncation::new(
            "files",
            MAX_FILES_PER_ROOT,
            &format!(
                "root {} hit the per-root file cap; scan narrower roots",
                self.root.display()
            ),
        ));
    }

    fn visit(&mut self, dir: &Path, depth: usize, name: std::ffi::OsString) {
        let path = dir.join(&name);
        let name_lossy = name.to_string_lossy();
        if path.is_dir() {
            self.visit_dir(&path, depth, &name_lossy);
        } else if path.is_file() && is_source(&name_lossy) {
            self.count += 1;
            self.scan_path(&path);
        }
    }

    fn visit_dir(&mut self, path: &Path, depth: usize, name_lossy: &str) {
        if IGNORED_DIRS.contains(&name_lossy) {
            return;
        }
        if depth >= MAX_DEPTH {
            self.skipped.push(format!(
                "{}: depth over {MAX_DEPTH}, skipped",
                rel_of(self.root, path)
            ));
            return;
        }
        self.walk(path, depth + 1);
    }

    fn scan_path(&mut self, path: &Path) {
        let rel = rel_of(self.root, path);
        let Ok(file) = std::fs::File::open(path) else {
            self.skipped.push(format!("{rel}: unreadable, skipped"));
            return;
        };
        let mut text = String::new();
        if file
            .take(MAX_FILE_BYTES + 1)
            .read_to_string(&mut text)
            .is_err()
        {
            self.skipped
                .push(format!("{rel}: not valid utf-8, skipped"));
            return;
        }
        if text.len() > MAX_FILE_BYTES as usize {
            self.skipped
                .push(format!("{rel}: size over {MAX_FILE_BYTES} bytes, skipped"));
            return;
        }
        self.files.push(ScanFile {
            rel,
            kept: kept_lines(&text),
        });
    }
}

fn kept_lines(text: &str) -> Vec<KeptLine> {
    text.lines()
        .enumerate()
        .filter_map(|(offset, raw)| {
            normalize_line(raw).map(|text| KeptLine {
                line: offset + 1,
                text,
            })
        })
        .collect()
}

fn rel_of(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().to_string(),
        Err(_) => path.to_string_lossy().to_string(),
    }
}

fn is_source(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((_, extension)) => {
            let extension = extension.to_ascii_lowercase();
            SOURCE_EXTENSIONS.contains(&extension.as_str())
        }
        None => false,
    }
}
