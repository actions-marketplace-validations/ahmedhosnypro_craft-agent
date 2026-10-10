//! Grouping and ranking: overlap merge, cross-root filter, capped output.

use std::collections::HashMap;

use crate::Truncation;
use crate::dups::{Candidate, MAX_CANDIDATES, Occurrence};
use crate::dups_scan::{FileRef, Window};

/// A run of kept-line indices within one file: window starts before merging,
/// merged runs, and grouped spans alike.
struct Span {
    file: usize,
    first: usize,
    last: usize,
}

/// Walk spans sorted by (file, first, last) and merge each span that
/// overlaps or touches the open region into it; otherwise the region closes.
fn merge_spans(spans: &[Span]) -> Vec<(usize, usize, usize)> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|id| (spans[*id].file, spans[*id].first, spans[*id].last));
    let mut regions: Vec<(usize, usize, usize)> = Vec::new();
    for id in order {
        let Some(span) = spans.get(id) else {
            continue;
        };
        let overlap = matches!(regions.last(), Some(region)
            if region.0 == span.file && span.first <= region.2.saturating_add(1));
        match regions.last_mut() {
            Some(region) if overlap => {
                region.2 = region.2.max(span.last);
            }
            _ => regions.push((span.file, span.first, span.last)),
        }
    }
    regions
}

struct Group {
    total: usize,
    first_key: (String, String, usize),
    candidate: Candidate,
}

fn find(parent: &mut [usize], id: usize) -> usize {
    let mut current = id;
    loop {
        let Some(&up) = parent.get(current) else {
            return current;
        };
        if up == current {
            return current;
        }
        let Some(&up_up) = parent.get(up) else {
            return current;
        };
        parent[current] = up_up;
        current = up_up;
    }
}

fn union(parent: &mut [usize], left: usize, right: usize) {
    let a = find(parent, left);
    let b = find(parent, right);
    if a != b {
        parent[a.max(b)] = a.min(b);
    }
}

/// One span per merged run of a qualifying group; runs of one group are
/// unioned so overlapping windows of the same block collapse later.
/// Within one hash bucket, windows with different text never group together.
fn collect_spans(windows: &HashMap<u64, Vec<Window>>, min_lines: usize) -> (Vec<Span>, Vec<usize>) {
    let mut spans: Vec<Span> = Vec::new();
    let mut parent: Vec<usize> = Vec::new();
    for bucket in windows.values() {
        let mut hits: Vec<(&Window, usize)> = bucket
            .iter()
            .flat_map(|window| window.starts.iter().map(move |start| (window, *start)))
            .collect();
        hits.sort_by_key(|(window, start)| (window.file, *start));
        let mut runs: Vec<Span> = Vec::new();
        let mut offset = 0;
        while offset < hits.len() {
            let (window, _) = &hits[offset];
            let lines = &window.lines;
            let mut end = offset;
            while end < hits.len() && hits[end].0.lines == *lines {
                end += 1;
            }
            let mut text_runs: Vec<Span> = Vec::new();
            for (window, start) in &hits[offset..end] {
                let extends = matches!(text_runs.last(), Some(run)
                    if run.file == window.file && *start <= run.last.saturating_add(1));
                if extends {
                    if let Some(run) = text_runs.last_mut() {
                        run.last = run.last.max(*start);
                    }
                } else {
                    text_runs.push(Span {
                        file: window.file,
                        first: *start,
                        last: *start,
                    });
                }
            }
            let mut distinct = 0;
            let mut previous: Option<usize> = None;
            for run in &text_runs {
                if previous != Some(run.file) {
                    distinct += 1;
                    previous = Some(run.file);
                }
            }
            if distinct >= 2 || text_runs.len() >= 2 {
                runs.append(&mut text_runs);
            }
            offset = end;
        }
        if runs.len() >= 2 {
            push_group_spans(&runs, min_lines, &mut spans, &mut parent);
        }
    }
    (spans, parent)
}

fn push_group_spans(
    runs: &[Span],
    min_lines: usize,
    spans: &mut Vec<Span>,
    parent: &mut Vec<usize>,
) {
    let mut group: Vec<usize> = Vec::with_capacity(runs.len());
    for run in runs {
        let id = spans.len();
        spans.push(Span {
            file: run.file,
            first: run.first,
            last: run.last.saturating_add(min_lines - 1),
        });
        parent.push(id);
        group.push(id);
    }
    let mut linked = group.iter().copied();
    if let Some(first) = linked.next() {
        for id in linked {
            union(parent, first, id);
        }
    }
}

/// Union spans that overlap or touch inside one file; the region extent
/// grows as the sorted walk advances so chains and nests both merge.
fn link_overlaps(spans: &[Span], parent: &mut [usize]) {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|id| (spans[*id].file, spans[*id].first, spans[*id].last));
    let mut region: Option<(usize, usize, usize)> = None;
    for id in order {
        let Some(span) = spans.get(id) else {
            continue;
        };
        let overlap = matches!(region, Some((file, extent, _))
            if file == span.file && span.first <= extent.saturating_add(1));
        match region {
            Some((file, extent, rep)) if overlap => {
                union(parent, rep, id);
                region = Some((file, extent.max(span.last), rep));
            }
            _ => region = Some((span.file, span.last, id)),
        }
    }
}

/// Every component becomes one candidate; per file, its spans chain into
/// regions and the widest region fixes the candidate's line count.
pub(crate) fn rank_groups(
    files: &[FileRef],
    windows: &HashMap<u64, Vec<Window>>,
    min_lines: usize,
    max_candidates: usize,
    cross_only: bool,
    truncated: &mut Vec<Truncation>,
) -> Vec<Candidate> {
    let (spans, mut parent) = collect_spans(windows, min_lines);
    link_overlaps(&spans, &mut parent);
    let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
    for id in 0..spans.len() {
        let root = find(&mut parent, id);
        components.entry(root).or_default().push(id);
    }
    let mut groups: Vec<Group> = Vec::new();
    for ids in components.values() {
        if let Some(group) = build_group(files, &spans, ids) {
            groups.push(group);
        }
    }
    if cross_only {
        groups.retain(|group| distinct_roots(&group.candidate) >= 2);
    }
    groups.sort_by(|a, b| {
        b.total
            .cmp(&a.total)
            .then_with(|| a.first_key.cmp(&b.first_key))
    });
    let cap = max_candidates.min(MAX_CANDIDATES);
    if groups.len() > cap {
        truncated.push(Truncation::new(
            "candidates",
            cap,
            "more duplicate groups exist; raise --max-candidates (ceiling 500)",
        ));
    }
    groups
        .into_iter()
        .take(cap)
        .map(|group| group.candidate)
        .collect()
}

fn distinct_roots(candidate: &Candidate) -> usize {
    let mut seen: Vec<&str> = Vec::new();
    for occurrence in &candidate.occurrences {
        let root = occurrence.root.as_str();
        if !seen.contains(&root) {
            seen.push(root);
        }
    }
    seen.len()
}

fn build_group(files: &[FileRef], spans: &[Span], ids: &[usize]) -> Option<Group> {
    let group_spans: Vec<Span> = ids
        .iter()
        .filter_map(|id| spans.get(*id))
        .map(|span| Span {
            file: span.file,
            first: span.first,
            last: span.last,
        })
        .collect();
    let regions = merge_spans(&group_spans);
    let lines = regions.iter().map(|region| region.2 - region.1 + 1).max()?;
    let mut occurrences: Vec<Occurrence> = Vec::with_capacity(regions.len());
    for (file, first, _) in &regions {
        let Some(record) = files.get(*file) else {
            continue;
        };
        let Some(kept) = record.file.kept.get(*first) else {
            continue;
        };
        occurrences.push(Occurrence {
            root: record.root.clone(),
            file: record.file.rel.clone(),
            line: kept.line,
        });
    }
    if occurrences.is_empty() {
        return None;
    }
    occurrences.sort_by(|a, b| (&a.root, &a.file, a.line).cmp(&(&b.root, &b.file, b.line)));
    let total = lines.saturating_mul(occurrences.len());
    let first = &occurrences[0];
    Some(Group {
        total,
        first_key: (first.root.clone(), first.file.clone(), first.line),
        candidate: Candidate { lines, occurrences },
    })
}
