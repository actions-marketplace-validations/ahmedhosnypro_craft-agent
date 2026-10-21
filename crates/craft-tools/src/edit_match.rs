//! The exact-match engine behind edit_file: non-overlapping byte
//! matches and the splice that rebuilds the file around them.
//! Knuth-Morris-Pratt, so hostile partial matches cannot make the scan
//! quadratic.

/// Non-overlapping match positions of `needle` in `hay`, ascending.
pub(crate) fn find_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return Vec::new();
    }
    scan_matches(hay, needle, &failure_table(needle))
}

pub(crate) fn splice(bytes: &[u8], old: &[u8], new: &[u8], positions: &[usize]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut last = 0usize;
    for pos in positions {
        out.extend_from_slice(&bytes[last..*pos]);
        out.extend_from_slice(new);
        last = pos + old.len();
    }
    out.extend_from_slice(&bytes[last..]);
    out
}

/// The KMP failure links: fail[i] is the length of the longest proper
/// prefix of `needle[..=i]` that is also its suffix.
fn failure_table(needle: &[u8]) -> Vec<usize> {
    let mut fail = vec![0usize; needle.len()];
    let mut matched = 0usize;
    for i in 1..needle.len() {
        matched = failure_link(needle, &fail, i, matched);
        if let Some(slot) = fail.get_mut(i) {
            *slot = matched;
        }
    }
    fail
}

/// Fall back through the table on a mismatch, then extend on a match.
fn failure_link(needle: &[u8], fail: &[usize], i: usize, mut matched: usize) -> usize {
    while matched > 0 && needle.get(i) != needle.get(matched) {
        matched = fail.get(matched - 1).copied().unwrap_or(0);
    }
    if needle.get(i) == needle.get(matched) {
        matched + 1
    } else {
        matched
    }
}

fn scan_matches(hay: &[u8], needle: &[u8], fail: &[usize]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut matched = 0usize;
    let mut cursor = 0usize;
    while let Some(byte) = hay.get(cursor) {
        if matched == needle.len() {
            out.push(cursor - needle.len());
            matched = 0;
            continue;
        }
        if Some(byte) == needle.get(matched) {
            matched += 1;
            cursor += 1;
        } else if matched > 0 {
            matched = fail.get(matched - 1).copied().unwrap_or(0);
        } else {
            cursor += 1;
        }
    }
    if matched == needle.len() {
        out.push(hay.len() - needle.len());
    }
    out
}
