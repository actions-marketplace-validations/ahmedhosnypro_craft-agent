//! The `denyRead`/`denyWrite` field rules: a comma or space separated
//! list of workspace-relative globs. Shape mirrors the workflow
//! scope gate so a profile glob is never silently unmatched at
//! enforcement time.

use std::path::Path;

use super::AgentFileError;
use super::parse::{Parsed, entry};

const MAX_GLOBS: usize = 16;
const MAX_GLOB_CHARS: usize = 128;
const MAX_SEGMENTS: usize = 24;

/// Field01: a comma separated list of workspace-relative globs.
pub(crate) fn check_globs(
    path: &Path,
    parsed: &Parsed,
    field: &str,
) -> Result<Vec<String>, AgentFileError> {
    let Some(found) = entry(parsed, field) else {
        return Ok(Vec::new());
    };
    let err = |message: String| AgentFileError::ValueShape {
        path: path.to_path_buf(),
        line: found.line,
        message,
    };
    let raw = found.value.trim();
    let Some(inner) = bracket_inner(raw) else {
        return Err(err(format!("unbalanced bracket list in {field}")));
    };
    let tokens = split_tokens(inner);
    if tokens.is_empty() {
        return Err(err(format!("empty glob list in {field}")));
    }
    if tokens.len() > MAX_GLOBS {
        return Err(err(format!(
            "{field} lists {} globs (cap {MAX_GLOBS})",
            tokens.len()
        )));
    }
    for token in &tokens {
        if !valid_glob(token) {
            return Err(err(format!(
                "`{token}` is not a workspace-relative glob in {field}"
            )));
        }
    }
    Ok(tokens)
}

/// The list payload between balanced brackets, or the raw value when
/// unbracketed; None when the brackets do not balance.
fn bracket_inner(raw: &str) -> Option<&str> {
    let starts = raw.starts_with('[');
    let ends = raw.ends_with(']');
    if starts && ends {
        return Some(&raw[1..raw.len() - 1]);
    }
    if starts || ends {
        return None;
    }
    Some(raw)
}

fn split_tokens(inner: &str) -> Vec<String> {
    inner
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

/// 1..=MAX_GLOB_CHARS chars of [a-zA-Z0-9._-*], `//`-free, `..`-free,
/// `**` only as a whole segment.
fn valid_glob(glob: &str) -> bool {
    if glob.starts_with('/') || glob.len() > MAX_GLOB_CHARS {
        return false;
    }
    let segments: Vec<&str> = glob.split('/').collect();
    if segments.len() > MAX_SEGMENTS {
        return false;
    }
    segments.into_iter().all(valid_segment)
}

/// `**` passes as a whole segment; every other segment holds only glob
/// bytes and neither `..` nor an inner `**`.
fn valid_segment(segment: &str) -> bool {
    if segment == "**" {
        return true;
    }
    if segment.is_empty() {
        return false;
    }
    if !segment.bytes().all(glob_byte) {
        return false;
    }
    !segment.contains("..") && !segment.contains("**")
}

fn glob_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_rules() {
        assert!(valid_glob("tests/**"));
        assert!(valid_glob("**/*.spec.ts"));
        assert!(!valid_glob("/etc/passwd"));
        assert!(!valid_glob("../secrets/*"));
        assert!(!valid_glob("a//b"));
        assert!(!valid_glob("a**b/**"));
        assert!(!valid_glob(""));
    }

    #[test]
    fn list_split() {
        assert_eq!(
            split_tokens("tests/**, src"),
            vec!["tests/**".to_string(), "src".to_string()]
        );
        assert_eq!(split_tokens("a, b"), vec!["a".to_string(), "b".to_string()]);
        assert!(split_tokens("  ").is_empty());
    }

    #[test]
    fn brackets_strip_only_when_balanced() {
        assert_eq!(bracket_inner("[a, b]"), Some("a, b"));
        assert_eq!(bracket_inner("a, b"), Some("a, b"));
        assert_eq!(bracket_inner("[a, b"), None);
        assert_eq!(bracket_inner("a, b]"), None);
    }
}
