//! A bounded, deterministic directory walker shared by glob and grep.
//! Entries are visited in sorted name order; symlinks are never
//! followed; every loop is capped.

use std::path::{Path, PathBuf};

use crate::error::ToolError;
use crate::pathguard::sensitive_reason;

/// Directory names never traversed. Kept in one place so the skip set
/// is identical for glob and grep.
pub const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WalkOutcome {
    pub entries_seen: usize,
    pub visited_files: usize,
    pub skipped_sensitive: usize,
    pub skipped_unreadable: usize,
    pub skipped_symlinks: usize,
}

impl WalkOutcome {
    fn count(&mut self, skip: Skip) {
        match skip {
            Skip::Symlink => self.skipped_symlinks = self.skipped_symlinks.saturating_add(1),
            Skip::Sensitive => self.skipped_sensitive = self.skipped_sensitive.saturating_add(1),
        }
    }
}

/// The per-directory listing result: ready entries, an unreadable
/// directory (already counted), or the entry cap hit.
enum DirListing {
    Ready(Vec<(PathBuf, Option<std::fs::FileType>)>),
    Unreadable,
    Truncated,
}

fn sorted_entries(dir: &Path, outcome: &mut WalkOutcome, max_entries: usize) -> DirListing {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            outcome.skipped_unreadable = outcome.skipped_unreadable.saturating_add(1);
            return DirListing::Unreadable;
        }
    };
    let mut listed: Vec<(PathBuf, Option<std::fs::FileType>)> = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            outcome.skipped_unreadable = outcome.skipped_unreadable.saturating_add(1);
            continue;
        };
        outcome.entries_seen = outcome.entries_seen.saturating_add(1);
        if outcome.entries_seen > max_entries {
            return DirListing::Truncated;
        }
        listed.push((entry.path(), entry.file_type().ok()));
    }
    listed.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
    DirListing::Ready(listed)
}

/// What the walk does with one listed entry.
enum Visit {
    Descend(PathBuf),
    Skip(Skip),
    Drop,
    File(PathBuf),
}

enum Skip {
    Symlink,
    Sensitive,
}

fn plan_visit(
    path: PathBuf,
    file_type: Option<std::fs::FileType>,
    depth: usize,
    max_depth: usize,
) -> Visit {
    if file_type.is_some_and(|t| t.is_symlink()) {
        return Visit::Skip(Skip::Symlink);
    }
    if file_type.is_some_and(|t| t.is_dir()) {
        return plan_descend(path, depth, max_depth);
    }
    if sensitive_reason(&path).is_some() {
        return Visit::Skip(Skip::Sensitive);
    }
    Visit::File(path)
}

fn plan_descend(path: PathBuf, depth: usize, max_depth: usize) -> Visit {
    if depth >= max_depth {
        return Visit::Drop;
    }
    if is_skipped_dir(&path) {
        return Visit::Drop;
    }
    if sensitive_reason(&path).is_some() {
        return Visit::Drop;
    }
    Visit::Descend(path)
}

/// The walk state: the outcome counters, the pending-directory stack,
/// the bounds, and the visitor.
struct Walk<'a> {
    outcome: WalkOutcome,
    stack: Vec<(PathBuf, usize)>,
    max_entries: usize,
    max_depth: usize,
    visit: &'a mut dyn FnMut(PathBuf) -> bool,
}

impl Walk<'_> {
    /// List one directory and handle its entries; true ends the walk.
    fn step_dir(&mut self, dir: &Path, depth: usize) -> bool {
        let listed = match sorted_entries(dir, &mut self.outcome, self.max_entries) {
            DirListing::Ready(listed) => listed,
            DirListing::Unreadable => return false,
            DirListing::Truncated => return true,
        };
        for (path, file_type) in listed {
            if self.step_entry((path, file_type), depth) {
                return true;
            }
        }
        false
    }

    /// Handle one entry; true ends the walk.
    fn step_entry(&mut self, entry: (PathBuf, Option<std::fs::FileType>), depth: usize) -> bool {
        let (path, file_type) = entry;
        match plan_visit(path, file_type, depth, self.max_depth) {
            Visit::Descend(path) => self.stack.push((path, depth + 1)),
            Visit::Skip(skip) => self.outcome.count(skip),
            Visit::Drop => {}
            Visit::File(path) => {
                self.outcome.visited_files = self.outcome.visited_files.saturating_add(1);
                if !(self.visit)(path) {
                    return true;
                }
            }
        }
        false
    }
}

/// Walk `root` depth-first, visiting files in sorted name order. The
/// visitor returns false to stop early (result caps). Sensitive files
/// and symlinks are skipped and counted, never visited.
pub fn walk_files(
    root: &Path,
    max_entries: usize,
    max_depth: usize,
    visit: &mut dyn FnMut(PathBuf) -> bool,
) -> Result<WalkOutcome, ToolError> {
    let mut walk = Walk {
        outcome: WalkOutcome::default(),
        stack: vec![(root.to_path_buf(), 0)],
        max_entries,
        max_depth,
        visit,
    };
    while let Some((dir, depth)) = walk.stack.pop() {
        if walk.step_dir(&dir, depth) {
            return Ok(walk.outcome);
        }
    }
    Ok(walk.outcome)
}

fn is_skipped_dir(path: &Path) -> bool {
    path.file_name()
        .map(|n| {
            let name = n.to_string_lossy();
            SKIP_DIRS.iter().any(|skip| *skip == name)
        })
        .unwrap_or(false)
}
