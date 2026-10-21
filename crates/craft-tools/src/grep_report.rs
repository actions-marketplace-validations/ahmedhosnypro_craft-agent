//! The grep report: bounded per-file search over the walker's files,
//! skip accounting (oversize, binary, unreadable, sensitive), and the
//! rendered `path:line: excerpt` rows with their skip and cap notes.

use regex::Regex;

use crate::tool::ToolLimits;
use crate::util;

const BINARY_PROBE: usize = 8 * 1024;

/// The search context of one file: `root` prefixes display paths, and
/// `base` is the directory its filter-relative path is taken against.
pub(crate) struct Paths<'a> {
    pub(crate) root: &'a std::path::Path,
    pub(crate) base: &'a std::path::Path,
}

pub(crate) struct Needle<'a> {
    pub(crate) matcher: &'a Regex,
    pub(crate) filter: &'a Option<globset::GlobMatcher>,
}

pub(crate) struct Report {
    pub(crate) skipped_sensitive: usize,
    rows: Vec<(String, usize, String)>,
    limits: ToolLimits,
    skipped_oversize: usize,
    skipped_binary: usize,
    skipped_unreadable: usize,
    capped: bool,
}

impl Report {
    pub(crate) fn new(limits: ToolLimits) -> Self {
        Self {
            rows: Vec::new(),
            limits,
            skipped_sensitive: 0,
            skipped_oversize: 0,
            skipped_binary: 0,
            skipped_unreadable: 0,
            capped: false,
        }
    }

    pub(crate) fn capped(&self) -> bool {
        self.capped
    }

    /// Search one file: oversize, binary, and unreadable files are
    /// skipped and counted; matching lines become `path:line: excerpt` rows.
    pub(crate) fn search_file(
        &mut self,
        path: &std::path::Path,
        paths: &Paths<'_>,
        needle: &Needle<'_>,
    ) {
        if !self.passes_filter(path, paths.base, needle.filter) {
            return;
        }
        let Some(text) = self.read_text(path) else {
            return;
        };
        let display = display_path(path, paths.root);
        self.append_matches(needle.matcher, &text, &display);
    }

    fn passes_filter(
        &self,
        path: &std::path::Path,
        base: &std::path::Path,
        filter: &Option<globset::GlobMatcher>,
    ) -> bool {
        let Some(f) = filter else {
            return true;
        };
        match path.strip_prefix(base) {
            Ok(rel) => f.is_match(rel),
            Err(_) => false,
        }
    }

    /// The file's text when it is searchable; None counts the skip reason.
    fn read_text(&mut self, path: &std::path::Path) -> Option<String> {
        let Ok(meta) = std::fs::metadata(path) else {
            self.skipped_unreadable = self.skipped_unreadable.saturating_add(1);
            return None;
        };
        if !meta.is_file() || meta.len() > self.limits.grep_file_cap as u64 {
            self.skipped_oversize = self.skipped_oversize.saturating_add(1);
            return None;
        }
        let Ok(bytes) = std::fs::read(path) else {
            self.skipped_unreadable = self.skipped_unreadable.saturating_add(1);
            return None;
        };
        if !self.is_plaintext(&bytes) {
            self.skipped_binary = self.skipped_binary.saturating_add(1);
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn is_plaintext(&self, bytes: &[u8]) -> bool {
        bytes.len() <= self.limits.grep_file_cap
            && !bytes[..bytes.len().min(BINARY_PROBE)].contains(&0)
    }

    fn append_matches(&mut self, matcher: &Regex, text: &str, display: &str) {
        for (idx, line) in text.split_inclusive('\n').enumerate() {
            let body = line.strip_suffix('\n').unwrap_or(line);
            let Some(found) = matcher.find(body) else {
                continue;
            };
            let start = found.start().min(body.len());
            let tail = util::truncate_char_boundary(&body[start..], self.limits.grep_line_cap);
            self.rows
                .push((display.to_string(), idx + 1, tail.to_string()));
            if self.rows.len() >= self.limits.grep_max_results {
                self.capped = true;
                return;
            }
        }
    }

    pub(crate) fn render(&mut self, pattern: &str) -> String {
        self.rows.sort_unstable();
        let mut out = String::new();
        for (path, line, tail) in &self.rows {
            out.push_str(&format!("{path}:{line}: {tail}\n"));
        }
        let skips = [
            ("oversized", self.skipped_oversize),
            ("binary", self.skipped_binary),
            ("unreadable", self.skipped_unreadable),
            ("sensitive", self.skipped_sensitive),
        ];
        for (kind, count) in skips {
            if count > 0 {
                out.push_str(&format!("[skipped {count} {kind} files]\n"));
            }
        }
        if self.capped {
            out.push_str(&format!(
                "[results capped at {}]\n",
                self.limits.grep_max_results
            ));
        }
        if self.rows.is_empty() {
            out.push_str(&format!("no matches for `{pattern}`\n"));
        }
        out
    }
}

fn display_path(path: &std::path::Path, root: &std::path::Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}
