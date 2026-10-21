//! Engine-enforced path scopes: workspace-relative deny globs checked
//! per tool call, plus bash command screening.
//!
//! Globs are validated at plan-parse time in craft-workflow; this module
//! matches them against resolved paths and screens command text. The
//! matcher is bounded and char-boundary safe; `*` never crosses a `/`
//! boundary, a whole `**` segment bridges any depth.

use std::path::Path;

use crate::error::ToolError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeDenies {
    deny_read: Vec<String>,
    deny_write: Vec<String>,
}

impl ScopeDenies {
    pub fn new(deny_read: Vec<String>, deny_write: Vec<String>) -> Self {
        Self {
            deny_read,
            deny_write,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.deny_read.is_empty() && self.deny_write.is_empty()
    }

    /// The first read-deny glob matching `path`, relative to `workspace`.
    /// Paths outside the workspace (configured read roots) are not
    /// workspace-scoped and pass.
    pub fn denies_read(&self, workspace: &Path, path: &Path) -> Option<String> {
        match_any(&self.deny_read, workspace, path)
    }

    pub fn denies_write(&self, workspace: &Path, path: &Path) -> Option<String> {
        match_any(&self.deny_write, workspace, path)
    }

    /// A denied literal fragment appearing in a command line; the
    /// screen is best-effort and documented as such in the docs.
    pub fn command_mentions_denied(&self, command: &str) -> Option<String> {
        for glob in self.deny_read.iter().chain(self.deny_write.iter()) {
            for fragment in literal_fragments(glob) {
                if fragment.len() >= 3 && command.contains(fragment.as_str()) {
                    return Some(glob.clone());
                }
            }
        }
        None
    }
}

fn match_any(globs: &[String], workspace: &Path, path: &Path) -> Option<String> {
    if globs.is_empty() {
        return None;
    }
    let Ok(rel) = path.strip_prefix(workspace) else {
        return None;
    };
    let text = rel.to_string_lossy().replace('\\', "/");
    globs.iter().find(|glob| glob_matches(glob, &text)).cloned()
}

/// Literal fragments of a glob: the whole glob with wildcards removed
/// plus every wildcard-free segment, dropping fragments under three
/// chars so common substrings do not false-positive.
fn literal_fragments(glob: &str) -> Vec<String> {
    let mut out = vec![glob.replace("**", "").replace('*', "")];
    for segment in glob.split('/') {
        if segment == "**" || segment == "*" {
            continue;
        }
        let literal = segment.replace('*', "");
        if literal.len() >= 3 {
            out.push(literal);
        }
    }
    out.retain(|fragment| fragment.len() >= 3);
    out
}

fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

pub fn glob_matches(glob: &str, path: &str) -> bool {
    let glob: Vec<&str> = glob.split('/').collect();
    let normalized = normalize(path);
    let path: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    segment_match(&glob, 0, &path, 0)
}

fn segment_match(glob: &[&str], gi: usize, path: &[&str], pi: usize) -> bool {
    let mut gi = gi;
    let mut pi = pi;
    loop {
        let Some(segment) = glob.get(gi) else {
            return pi == path.len();
        };
        if *segment == "**" {
            return bridge_double_star(glob, gi, path, pi);
        }
        let Some(head) = path.get(pi) else {
            return false;
        };
        if !star_match(segment.as_bytes(), 0, head.as_bytes(), 0) {
            return false;
        }
        gi += 1;
        pi += 1;
    }
}

/// A `**` segment bridges any depth, so the globs after it may start at
/// any path offset; a trailing `**` matches the rest.
fn bridge_double_star(glob: &[&str], gi: usize, path: &[&str], pi: usize) -> bool {
    if gi + 1 == glob.len() {
        return true;
    }
    (pi..=path.len()).any(|at| segment_match(glob, gi + 1, path, at))
}

fn star_match(pattern: &[u8], mut pi: usize, text: &[u8], mut ti: usize) -> bool {
    let mut star: Option<usize> = None;
    let mut star_text = 0usize;
    loop {
        let Some(&byte) = pattern.get(pi) else {
            return ti == text.len();
        };
        if byte == b'*' {
            star = Some(pi);
            star_text = ti;
            pi += 1;
            continue;
        }
        if text.get(ti) != Some(&byte) {
            let Some(at) = star else {
                return false;
            };
            if star_text == text.len() {
                return false;
            }
            star_text += 1;
            pi = at + 1;
            ti = star_text;
            continue;
        }
        pi += 1;
        ti += 1;
    }
}

/// The typed denial a tool returns, with the glob that fired.
pub fn denial(tool: &str, path: &str, glob: &str, mode: &'static str) -> ToolError {
    ToolError::ScopeDenied {
        tool: tool.to_string(),
        path: path.to_string(),
        glob: glob.to_string(),
        mode,
    }
}
