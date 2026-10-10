//! Workspace path containment and the sensitive-path predicate.
//!
//! Reads may also reach the extra read roots named by
//! `CRAFT_AGENT_READ_ROOTS` — the workspace is always tried first,
//! then roots in declaration order, and symlink components resolve
//! against the matched root, so a link may not stitch roots. Sensitive
//! paths are denied on every call: the guard is a pure
//! function of the resolved path, so nothing is cached across calls.
//! Ban patterns match case-insensitively; the public exemptions
//! (`.env.example`/`.env.sample`/`.env.template`, `*.pub` keys) require
//! the exact documented spellings. Symlink components resolve against
//! the root: a link that leaves the root, dangles, or names a
//! sensitive target is denied.

use std::path::{Component, Path, PathBuf};

use crate::error::ToolError;

/// The path-resolution inputs: the tool label for errors, the
/// workspace root, the extra read roots a READ path may also fold
/// against, and the path byte cap.
pub struct PathCtx<'a> {
    tool: &'a str,
    workspace: &'a Path,
    read_roots: &'a [PathBuf],
    max_path_bytes: usize,
}

impl<'a> PathCtx<'a> {
    pub fn new(
        tool: &'a str,
        workspace: &'a Path,
        read_roots: &'a [PathBuf],
        max_path_bytes: usize,
    ) -> Self {
        Self {
            tool,
            workspace,
            read_roots,
            max_path_bytes,
        }
    }

    fn arg_err(&self, msg: String) -> ToolError {
        ToolError::InvalidArgs {
            tool: self.tool.to_string(),
            msg,
        }
    }

    fn escapes(&self, path: &str) -> ToolError {
        let mut roots = Vec::with_capacity(1 + self.read_roots.len());
        roots.push(self.workspace.to_path_buf());
        roots.extend(self.read_roots.iter().cloned());
        ToolError::EscapesWorkspace {
            path: path.to_string(),
            allowed: roots_label(&roots),
        }
    }
}

/// The comma-joined display paths of the roots a path may live in.
fn roots_label(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The env knob naming extra read-only roots for the file-read tools.
pub const READ_ROOTS_KNOB: &str = "CRAFT_AGENT_READ_ROOTS";

const MAX_KNOB_BYTES: usize = 4096;
const MAX_READ_ROOTS: usize = 4;

/// Knob value -> extra read roots. None or empty turns the feature
/// off; an oversized value grants nothing (fail closed). Entries are
/// trimmed; empty and non-absolute entries are dropped; at most
/// MAX_READ_ROOTS survive, in declaration order. A root that does not
/// exist is tolerated — reads inside it return `NotFound`.
pub fn parse_read_roots(knob: Option<&str>) -> Vec<PathBuf> {
    let Some(value) = knob else {
        return Vec::new();
    };
    if value.trim().is_empty() || value.len() > MAX_KNOB_BYTES {
        return Vec::new();
    }
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter(|entry| Path::new(entry).is_absolute())
        .map(PathBuf::from)
        .take(MAX_READ_ROOTS)
        .collect()
}

/// Resolve a tool-supplied path against the workspace root, rejecting
/// lexical escapes, symlink escapes, and dangling symlinks. The
/// returned path is contained in the root lexically, and every symlink
/// component on it resolves inside the root to a non-sensitive target.
pub fn resolve_within(paths: &PathCtx<'_>, input: &str) -> Result<PathBuf, ToolError> {
    let root = paths.workspace;
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let folded = fold_within(paths, &canon_root, input)?;
    reject_symlink_escapes(paths, &canon_root, &folded, input)?;
    Ok(folded)
}

/// Resolve a READ path: a relative input folds against the workspace
/// root only (`resolve_within`, verbatim); an absolute input may land
/// in the workspace or a configured read root, tried in that order.
/// The first containing root wins; its symlink verdict is definitive,
/// so a link may not stitch roots; a probe outside every root stays
/// the run-fatal `EscapesWorkspace` denial.
pub fn resolve_read(paths: &PathCtx<'_>, input: &str) -> Result<PathBuf, ToolError> {
    if !Path::new(input).is_absolute() {
        return resolve_within(paths, input);
    }
    let candidates =
        std::iter::once(paths.workspace).chain(paths.read_roots.iter().map(PathBuf::as_path));
    for root in candidates {
        let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let folded = match fold_within(paths, &canon_root, input) {
            Ok(folded) => folded,
            Err(ToolError::EscapesWorkspace { .. }) => continue,
            Err(other) => return Err(other),
        };
        reject_symlink_escapes(paths, &canon_root, &folded, input)?;
        return Ok(folded);
    }
    Err(paths.escapes(input))
}

/// Fold the input lexically: `.` drops, `..` pops, and an absolute
/// input must still land inside the root.
fn fold_within(paths: &PathCtx<'_>, root: &Path, input: &str) -> Result<PathBuf, ToolError> {
    if input.is_empty() {
        return Err(paths.arg_err("path must not be empty".to_string()));
    }
    if input.len() > paths.max_path_bytes {
        return Err(paths.arg_err(format!("path exceeds {} bytes", paths.max_path_bytes)));
    }
    let parts = fold_parts(input).ok_or_else(|| paths.escapes(input))?;
    let mut folded = if Path::new(input).is_absolute() {
        PathBuf::from("/")
    } else {
        root.to_path_buf()
    };
    for part in &parts {
        folded.push(part);
    }
    if !folded.starts_with(root) {
        return Err(paths.escapes(input));
    }
    Ok(folded)
}

/// The lexically folded components of the input; None when a `..` pops
/// past the start.
fn fold_parts(input: &str) -> Option<Vec<std::ffi::OsString>> {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for comp in Path::new(input).components() {
        match comp {
            Component::Normal(c) => parts.push(c.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    Some(parts)
}

/// Walk the folded path component by component. Every symlink must
/// canonicalize inside the root to a non-sensitive target; a dangling
/// symlink is denied outright, because writing through one would
/// create its target outside the root.
fn reject_symlink_escapes(
    paths: &PathCtx<'_>,
    canon_root: &Path,
    folded: &Path,
    input: &str,
) -> Result<(), ToolError> {
    let rel = match folded.strip_prefix(canon_root) {
        Ok(rel) => rel,
        Err(_) => return Err(paths.escapes(input)),
    };
    let mut resolved = canon_root.to_path_buf();
    for comp in rel.components() {
        resolved.push(comp);
        let Ok(meta) = std::fs::symlink_metadata(&resolved) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let Ok(target) = std::fs::canonicalize(&resolved) else {
            return Err(paths.escapes(input));
        };
        if !target.starts_with(canon_root) {
            return Err(paths.escapes(input));
        }
        if let Some(reason) = sensitive_reason(&target) {
            return Err(ToolError::SensitivePath {
                path: input.to_string(),
                reason,
            });
        }
        resolved = target;
    }
    Ok(())
}

/// How a guarded open uses the file: read-only, truncate-and-write, or
/// append (the write modes create a missing file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardedOpen {
    Read,
    Truncate,
    Append,
}

fn io_err(op: &'static str) -> impl Fn(std::io::Error) -> ToolError {
    move |e| ToolError::Io {
        op,
        msg: e.to_string(),
    }
}

fn no_parent() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent")
}

fn no_file_name() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name")
}

/// The fully-resolved target of `abs`: canonicalize when it exists,
/// else the canonicalized parent joined with the final name (the write
/// modes create the file there).
fn resolved_target(abs: &Path, mode: GuardedOpen) -> std::io::Result<PathBuf> {
    match std::fs::canonicalize(abs) {
        Ok(canon) => Ok(canon),
        Err(e) if mode == GuardedOpen::Read || e.kind() != std::io::ErrorKind::NotFound => Err(e),
        Err(_) => {
            let parent = abs.parent().ok_or_else(no_parent)?;
            let name = abs.file_name().ok_or_else(no_file_name)?;
            Ok(std::fs::canonicalize(parent)?.join(name))
        }
    }
}

/// Open `abs` for the callers that hold a guard verdict on it,
/// resolving symlinks and opening in one pass. The path is
/// canonicalized once (a legitimate symlink as the final component is
/// honored), the canonical path must stay inside one of `roots` and be
/// non-sensitive, and the final open is relative to the canonical
/// parent with O_NOFOLLOW, so a component swapped to an escaping
/// symlink after the guard walk is refused (ELOOP) instead of followed.
pub fn open_guarded(
    abs: &Path,
    roots: &[PathBuf],
    mode: GuardedOpen,
    op: &'static str,
) -> Result<std::fs::File, ToolError> {
    let io = io_err(op);
    let shown = || abs.to_string_lossy().into_owned();
    let canon = resolved_target(abs, mode).map_err(|e| {
        if mode == GuardedOpen::Read && e.kind() == std::io::ErrorKind::NotFound {
            ToolError::NotFound { path: shown() }
        } else {
            io(e)
        }
    })?;
    let inside = roots.iter().any(|root| {
        let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        canon.starts_with(canon_root)
    });
    if !inside {
        return Err(ToolError::EscapesWorkspace {
            path: shown(),
            allowed: roots_label(roots),
        });
    }
    if let Some(reason) = sensitive_reason(&canon) {
        return Err(ToolError::SensitivePath {
            path: shown(),
            reason,
        });
    }
    open_final(&canon, mode, &io)
}

fn open_final(
    canon: &Path,
    mode: GuardedOpen,
    io: impl Fn(std::io::Error) -> ToolError,
) -> Result<std::fs::File, ToolError> {
    use nix::fcntl::{OFlag, openat};
    use nix::sys::stat::Mode;
    use std::os::fd::AsFd as _;
    let parent = canon.parent().ok_or_else(no_parent).map_err(&io)?;
    let dir = std::fs::File::open(parent).map_err(&io)?;
    if !dir.metadata().map_err(&io)?.is_dir() {
        return Err(io(std::io::Error::from(std::io::ErrorKind::NotADirectory)));
    }
    let base = match mode {
        GuardedOpen::Read => OFlag::O_RDONLY,
        GuardedOpen::Truncate => OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
        GuardedOpen::Append => OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_APPEND,
    };
    let name = canon.file_name().ok_or_else(no_file_name).map_err(&io)?;
    let flags = base | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let fd = openat(dir.as_fd(), name, flags, Mode::from_bits_truncate(0o666))
        .map_err(|e| io(std::io::Error::from_raw_os_error(e as i32)))?;
    Ok(std::fs::File::from(fd))
}

/// The reason a resolved path is sensitive, if it is. Checked per call
/// against every path component.
pub fn sensitive_reason(path: &Path) -> Option<&'static str> {
    path.components()
        .find_map(|comp| component_reason(comp.as_os_str()))
}

fn component_reason(name: &std::ffi::OsStr) -> Option<&'static str> {
    let shown = name.to_string_lossy();
    let lower = shown.to_lowercase();
    if lower == ".aws" || lower == ".gcp" {
        return Some("cloud credentials directory");
    }
    if lower.starts_with(".env")
        && !matches!(
            shown.as_ref(),
            ".env.example" | ".env.sample" | ".env.template"
        )
    {
        return Some("environment file");
    }
    if is_key_or_token(&lower) && !shown.ends_with(".pub") {
        return Some("key or token file");
    }
    if lower.starts_with("credentials") {
        return Some("credentials file");
    }
    None
}

fn is_key_or_token(lower: &str) -> bool {
    lower.starts_with("id_rsa")
        || lower.starts_with("id_dsa")
        || lower.starts_with("id_ed25519")
        || lower.starts_with("id_ecdsa")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".netrc")
        || lower.ends_with(".npmrc")
}
