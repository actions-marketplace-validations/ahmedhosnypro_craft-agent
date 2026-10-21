//! Workspace-relative scope globs: validation and a bounded matcher.
//!
//! A glob is a slash-separated path pattern; `*` matches within one
//! segment, a whole `**` segment matches any number of segments.

use crate::error::WorkflowError;

pub const MAX_SCOPE_GLOBS: usize = 16;
pub const MAX_GLOB_CHARS: usize = 128;
const MAX_SEGMENTS: usize = 32;

/// A validated deny list: parse-time guarantee, match-time speed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    globs: Vec<String>,
}

impl Scope {
    /// Parses a validated glob list; empty input is an empty scope.
    pub fn parse(globs: &[String]) -> Result<Self, WorkflowError> {
        if globs.len() > MAX_SCOPE_GLOBS {
            return Err(WorkflowError::Parse {
                file: String::new(),
                detail: format!("a scope list exceeds {MAX_SCOPE_GLOBS} globs"),
            });
        }
        for glob in globs {
            if !valid_glob(glob) {
                return Err(WorkflowError::Parse {
                    file: String::new(),
                    detail: format!("`{glob}` is not a workspace-relative glob"),
                });
            }
        }
        Ok(Self {
            globs: globs.to_vec(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.globs.is_empty()
    }

    /// Whether a workspace-relative path matches any glob.
    pub fn denies(&self, path: &str) -> bool {
        for glob in &self.globs {
            if glob_matches(glob, path) {
                return true;
            }
        }
        false
    }
}

/// 1..=MAX_GLOB_CHARS chars of [a-zA-Z0-9._-*], `//`-free, `..`-free,
/// `**` only as a whole segment.
pub fn valid_glob(glob: &str) -> bool {
    if glob.starts_with('/') || glob.len() > MAX_GLOB_CHARS {
        return false;
    }
    let segments: Vec<&str> = glob.split('/').collect();
    if segments.len() > MAX_SEGMENTS {
        return false;
    }
    segments.into_iter().all(valid_segment)
}

/// `**` passes as a whole segment; every other segment is non-empty,
/// `..`-free, and holds only glob bytes.
fn valid_segment(segment: &str) -> bool {
    if segment.is_empty() || segment == ".." {
        return false;
    }
    if segment == "**" {
        return true;
    }
    if !segment.bytes().all(glob_byte) {
        return false;
    }
    !segment.contains("..")
}

/// The bytes a glob segment may hold: ASCII alphanumerics plus `._-*`.
fn glob_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'*')
}

/// Segment-wise glob match; `**` bridges any segment count. Paths arrive
/// workspace-relative or absolute inside the workspace, so matching
/// always runs over the relative segments.
pub fn glob_matches(glob: &str, path: &str) -> bool {
    let trimmed = path.trim_start_matches('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in trimmed.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let tokens: Vec<&[u8]> = glob.split('/').map(str::as_bytes).collect();
    let segments: Vec<&[u8]> = trimmed.split('/').map(str::as_bytes).collect();
    Walk {
        tokens: &tokens,
        segments: &segments,
    }
    .matches()
}

/// The reachability walk over a glob's tokens and a path's segments:
/// `reaches[at]` holds that the tokens consumed so far match the first
/// `at` segments, so each token consumes the whole remainder (`**`) or
/// advances one segment.
struct Walk<'a> {
    tokens: &'a [&'a [u8]],
    segments: &'a [&'a [u8]],
}

impl<'a> Walk<'a> {
    fn matches(&self) -> bool {
        let width = self.segments.len().saturating_add(1);
        let mut reaches = vec![false; width];
        reaches[0] = true;
        for at in 0..self.tokens.len() {
            reaches = self.step(at, &reaches);
        }
        reaches.get(self.segments.len()) == Some(&true)
    }

    /// One token of the walk.
    fn step(&self, at: usize, reaches: &[bool]) -> Vec<bool> {
        let Some(token) = self.tokens.get(at) else {
            return vec![false; reaches.len()];
        };
        if *token == b"**" {
            return self.bridge(reaches);
        }
        let mut next = vec![false; reaches.len()];
        for (index, text) in self.segments.iter().enumerate() {
            let reached = reaches.get(index) == Some(&true) && star_match(token, text);
            if let (true, Some(slot)) = (reached, next.get_mut(index + 1)) {
                *slot = true;
            }
        }
        next
    }

    /// `**` bridges any segment count, so one token turns every reachable
    /// position into a whole reachable suffix.
    fn bridge(&self, reaches: &[bool]) -> Vec<bool> {
        let mut suffix = vec![false; reaches.len()];
        let mut reach = false;
        for (at, reached) in reaches.iter().enumerate() {
            reach = reach || *reached;
            suffix[at] = reach;
        }
        suffix
    }
}

/// One-segment wildcard match: `*` bridges any run of chars and the
/// other bytes stay literal, anchored at both ends.
fn star_match(pattern: &[u8], text: &[u8]) -> bool {
    let mut pieces = pattern.split(|byte| *byte == b'*').peekable();
    let Some(head) = pieces.next() else {
        return true;
    };
    if pieces.peek().is_none() {
        return text == head;
    }
    if !text.starts_with(head) {
        return false;
    }
    let mut rest = &text[head.len()..];
    while let Some(piece) = pieces.next() {
        if pieces.peek().is_none() {
            return rest.ends_with(piece);
        }
        if piece.is_empty() {
            continue;
        }
        let Some(at) = windows_find(rest, piece) else {
            return false;
        };
        rest = &rest[at + piece.len()..];
    }
    true
}

/// Byte index of `piece` inside `text`, or None.
fn windows_find(text: &[u8], piece: &[u8]) -> Option<usize> {
    text.windows(piece.len()).position(|part| part == piece)
}
