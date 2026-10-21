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
    /// Files and subtrees excluded by the caller's admission test.
    pub skipped_scope: usize,
}

impl WalkOutcome {
    fn count(&mut self, skip: Skip) {
        match skip {
            Skip::Symlink => self.skipped_symlinks = self.skipped_symlinks.saturating_add(1),
            Skip::Sensitive => self.skipped_sensitive = self.skipped_sensitive.saturating_add(1),
            Skip::Scope => self.skipped_scope = self.skipped_scope.saturating_add(1),
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
    Scope,
}

/// A per-path admission test the caller supplies (engine scope denial).
type Admit<'a> = dyn Fn(&Path) -> bool + 'a;

/// One listed directory entry and its position in the walk.
struct Entry {
    path: PathBuf,
    file_type: Option<std::fs::FileType>,
    depth: usize,
    max_depth: usize,
}

fn plan_visit(entry: Entry, admit: &Admit<'_>) -> Visit {
    if entry.file_type.is_some_and(|t| t.is_symlink()) {
        return Visit::Skip(Skip::Symlink);
    }
    if entry.file_type.is_some_and(|t| t.is_dir()) {
        return plan_descend(entry.path, entry.depth, entry.max_depth, admit);
    }
    if !admit(&entry.path) {
        return Visit::Skip(Skip::Scope);
    }
    if sensitive_reason(&entry.path).is_some() {
        return Visit::Skip(Skip::Sensitive);
    }
    Visit::File(entry.path)
}

fn plan_descend(path: PathBuf, depth: usize, max_depth: usize, admit: &Admit<'_>) -> Visit {
    if depth >= max_depth {
        return Visit::Drop;
    }
    if is_skipped_dir(&path) {
        return Visit::Drop;
    }
    if !admit(&path) {
        return Visit::Drop;
    }
    if sensitive_reason(&path).is_some() {
        return Visit::Drop;
    }
    Visit::Descend(path)
}

/// The walk state: the outcome counters, the pending-directory stack,
/// the bounds, the visitor, and the admission test.
struct Walk<'a> {
    outcome: WalkOutcome,
    stack: Vec<(PathBuf, usize)>,
    max_entries: usize,
    max_depth: usize,
    visit: &'a mut dyn FnMut(PathBuf) -> bool,
    admit: &'a Admit<'a>,
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
        match plan_visit(
            Entry {
                path,
                file_type,
                depth,
                max_depth: self.max_depth,
            },
            self.admit,
        ) {
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
/// visitor returns false to stop early (result caps). Sensitive files,
/// denied paths, and symlinks are skipped and counted, never visited.
pub fn walk_files(
    root: &Path,
    max_entries: usize,
    max_depth: usize,
    visit: &mut dyn FnMut(PathBuf) -> bool,
) -> Result<WalkOutcome, ToolError> {
    walk_filtered(root, max_entries, max_depth, visit, &|_| true)
}

/// The walk with a caller admission test: a path the test rejects is
/// never visited and never descended into.
pub fn walk_filtered(
    root: &Path,
    max_entries: usize,
    max_depth: usize,
    visit: &mut dyn FnMut(PathBuf) -> bool,
    admit: &Admit<'_>,
) -> Result<WalkOutcome, ToolError> {
    let mut walk = Walk {
        outcome: WalkOutcome::default(),
        stack: vec![(root.to_path_buf(), 0)],
        max_entries,
        max_depth,
        visit,
        admit,
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
