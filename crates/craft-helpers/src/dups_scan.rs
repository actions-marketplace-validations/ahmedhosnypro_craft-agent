//! Window hashing: the scan half of duplicate detection.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::Truncation;
use crate::dups::{
    DupsQuery, DupsReport, MAX_CANDIDATES, MAX_MIN_LINES, MAX_WINDOWS, ScanFile, collect_files,
};
use crate::dups_rank::rank_groups;
use crate::error::HelperError;
use crate::error::invalid_bound;

pub(crate) struct FileRef {
    pub(crate) root: String,
    pub(crate) file: ScanFile,
}

/// A hashed window of kept lines; the text is kept so equal hashes must
/// still compare equal line-for-line before hits group together.
#[derive(Clone)]
pub(crate) struct Window {
    pub(crate) file: usize,
    pub(crate) starts: Vec<usize>,
    pub(crate) lines: Vec<String>,
}

/// Hash every window of `min_lines` kept lines; the caller reports groups
/// that repeat across files or disjointly inside one file.
pub fn scan_dups(query: &DupsQuery) -> Result<DupsReport, HelperError> {
    if !(2..=MAX_MIN_LINES).contains(&query.min_lines) {
        return Err(invalid_bound(
            "min-lines",
            &query.min_lines.to_string(),
            2,
            MAX_MIN_LINES,
        ));
    }
    if query.max_candidates == 0 || query.max_candidates > MAX_CANDIDATES {
        return Err(invalid_bound(
            "max-candidates",
            &query.max_candidates.to_string(),
            1,
            MAX_CANDIDATES,
        ));
    }
    let mut skipped: Vec<String> = Vec::new();
    let mut truncated: Vec<Truncation> = Vec::new();
    let mut files: Vec<FileRef> = Vec::new();
    for root in &query.roots {
        let label = root.to_string_lossy().to_string();
        for file in collect_files(root, &mut skipped, &mut truncated) {
            files.push(FileRef {
                root: label.clone(),
                file,
            });
        }
    }
    let mut windows: HashMap<u64, Vec<Window>> = HashMap::new();
    if let Some(root) = hash_windows(&files, query.min_lines, &mut windows) {
        skipped.push(format!(
            "window cap ({MAX_WINDOWS}) reached during root {root}; remaining files skipped"
        ));
        truncated.push(Truncation::new(
            "windows",
            MAX_WINDOWS,
            &format!("root {root} filled the window cap; scan narrower roots"),
        ));
    }
    let candidates = rank_groups(
        &files,
        &windows,
        query.min_lines,
        query.max_candidates,
        query.cross_only,
        &mut truncated,
    );
    Ok(DupsReport {
        candidates,
        skipped,
        truncated,
    })
}

/// Hash every window; returns the root whose scan was in progress when the
/// window cap fired, so the skip note can name it.
fn hash_windows(
    files: &[FileRef],
    min_lines: usize,
    windows: &mut HashMap<u64, Vec<Window>>,
) -> Option<String> {
    let mut total: usize = 0;
    for (file_index, record) in files.iter().enumerate() {
        let kept = &record.file.kept;
        if kept.len() < min_lines {
            continue;
        }
        for start in 0..=kept.len() - min_lines {
            if total >= MAX_WINDOWS {
                return Some(record.root.clone());
            }
            let mut hasher = DefaultHasher::new();
            let Some(slice) = kept.get(start..start + min_lines) else {
                continue;
            };
            for line in slice {
                line.text.hash(&mut hasher);
            }
            let hash = hasher.finish();
            let lines: Vec<String> = slice.iter().map(|line| line.text.clone()).collect();
            push_window(windows, hash, file_index, start, &lines);
            total += 1;
        }
    }
    None
}

/// Record one window occurrence; hash-equal windows with different text stay
/// in separate `Window` rows of the bucket, so a hash collision never merges
/// two different blocks into one group.
pub(crate) fn push_window(
    windows: &mut HashMap<u64, Vec<Window>>,
    hash: u64,
    file: usize,
    start: usize,
    lines: &[String],
) {
    let bucket = windows.entry(hash).or_default();
    match bucket
        .iter_mut()
        .find(|window| window.file == file && window.lines == lines)
    {
        Some(window) => window.starts.push(start),
        None => bucket.push(Window {
            file,
            starts: vec![start],
            lines: lines.to_vec(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Window, push_window};

    #[test]
    fn hash_collision_never_groups_different_blocks() {
        let mut buckets: HashMap<u64, Vec<Window>> = HashMap::new();
        for text in ["aaa bbb", "zzz yyy"] {
            let lines: Vec<String> = (0..4).map(|_| text.to_string()).collect();
            for file in 0..2 {
                for start in 0..3 {
                    push_window(&mut buckets, 7, file, start, &lines);
                }
            }
        }
        let bucket = buckets.get(&7).unwrap();
        assert_eq!(bucket.len(), 4, "two texts x two files stay separate");
        for window in bucket {
            assert_eq!(
                window.starts.len(),
                3,
                "identical windows in one file still group"
            );
        }
    }
}
