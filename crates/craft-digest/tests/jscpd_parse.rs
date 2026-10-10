// Corpus fixtures: unwrap() is the loud-test idiom here.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use craft_digest::DigestError;
use craft_digest::bundle::Bundle;
use craft_digest::jscpd::{
    JSCPD_REPORT_CAP_BYTES, MAX_DUPLICATE_CANDIDATES, parse_jscpd_report, read_jscpd_report,
};
use craft_digest::jscpd_runner::{JscpdRunner, run_jscpd};
use craft_digest::window::{Window, WindowSource};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jscpd-report.json")
}

fn tmp_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/tmp/craft-digest-corpus")
        .join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn synthetic_pair_report(count: i64, tokens: impl Fn(i64) -> i64) -> String {
    let entries: Vec<String> = (0..count)
        .map(|i| {
            let tokens = tokens(i);
            format!(
                r#"{{"firstFile":{{"name":"r{i}/a.rs","start":{i}}},"secondFile":{{"name":"r{i}/b.rs","start":{i}}},"tokens":{tokens},"lines":5}}"#
            )
        })
        .collect();
    format!(r#"{{"duplicates":[{}]}}"#, entries.join(","))
}

#[test]
fn recorded_report_parses_and_sorts_by_tokens() {
    let candidates = read_jscpd_report(&fixture_path()).unwrap();
    let tokens: Vec<u64> = candidates.iter().map(|c| c.tokens).collect();
    assert_eq!(tokens, vec![105, 79, 73]);
    let first = &candidates[0];
    assert_eq!(first.first.repo, "app-a");
    assert_eq!(first.first.file, "src/lib.rs");
    assert_eq!(first.first.line, 3);
    assert_eq!(first.second.repo, "app-b");
    assert_eq!(first.second.file, "src/lib.rs");
    assert_eq!(first.second.line, 2);
    assert_eq!(first.lines, 15);
}

#[test]
fn synthetic_pairs_are_capped_and_sorted_desc() {
    let json = synthetic_pair_report(25, |i| (i * 7) % 25 + 1);
    let candidates = parse_jscpd_report(&json).unwrap();
    assert_eq!(candidates.len(), MAX_DUPLICATE_CANDIDATES);
    assert_eq!(MAX_DUPLICATE_CANDIDATES, 20);
    let tokens: Vec<u64> = candidates.iter().map(|c| c.tokens).collect();
    assert_eq!(tokens, (6..=25).rev().collect::<Vec<u64>>());
}

#[test]
fn hostile_site_names_are_skipped_not_fatal() {
    let json = r#"{"duplicates":[
 {"firstFile":{"name":"app-a/src/lib.rs","start":1},"secondFile":{"name":"app-b/src/lib.rs","start":2},"tokens":50},
 {"firstFile":{"name":"app-a/x.rs\" \"hover text","start":1},"secondFile":{"name":"app-b/x.rs","start":2},"tokens":90},
 {"firstFile":{"name":"app-a/y.rs\n## injected heading","start":1},"secondFile":{"name":"app-b/x.rs","start":2},"tokens":80},
 {"firstFile":{"name":"app-a/(x).rs","start":1},"secondFile":{"name":"app-b/x.rs","start":2},"tokens":70},
 {"firstFile":{"name":"app-a/","start":1},"secondFile":{"name":"app-b/x.rs","start":2},"tokens":60}
]}"#;
    let candidates = parse_jscpd_report(json).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].first.file, "src/lib.rs");
}

#[test]
fn site_names_are_sanitized_before_the_safety_check() {
    let json = r#"{"duplicates":[
 {"firstFile":{"name":"app-a/x&#32;space.rs","start":1},"secondFile":{"name":"app-b/x.rs","start":2},"tokens":90},
 {"firstFile":{"name":"app-a/x&#8234;bidi.rs","start":1},"secondFile":{"name":"app-b/x.rs","start":2},"tokens":80}
]}"#;
    let candidates = parse_jscpd_report(json).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].first.file, "xbidi.rs");
}

#[test]
fn garbage_reports_are_typed_errors() {
    for bad in ["not json", "{}", "[]", r#"{"duplicates":null}"#, ""] {
        let err = parse_jscpd_report(bad).unwrap_err();
        assert!(matches!(err, DigestError::Jscpd(_)), "took {bad:?}");
    }
}

#[test]
fn malformed_entries_are_skipped_not_fatal() {
    let json = r#"{"duplicates":[
 {"firstFile":{"name":"app-a/src/lib.rs","start":1},"secondFile":{"name":"app-b/src/lib.rs","start":2},"tokens":50},
 {"firstFile":{"start":1},"secondFile":{"name":"app-b/x.rs","start":2},"tokens":60},
 {"firstFile":{"name":"no-slash.rs","start":1},"secondFile":{"name":"app-b/x.rs","start":2},"tokens":70},
 {"firstFile":{"name":"app-a/y.rs","start":1},"secondFile":{"name":"app-b/x.rs","start":2}},
 {"firstFile":{"name":"app-a/y.rs","start":1},"secondFile":{"name":"app-b/x.rs","start":2},"tokens":"lots"},
 {"firstFile":{"name":"app-a/y.rs","start":1},"secondFile":{"name":"app-b/x.rs","start":2},"tokens":90}
]}"#;
    let candidates = parse_jscpd_report(json).unwrap();
    let tokens: Vec<u64> = candidates.iter().map(|c| c.tokens).collect();
    assert_eq!(tokens, vec![90, 50]);
}

#[test]
fn oversized_report_is_a_typed_error() {
    let huge = "x".repeat(JSCPD_REPORT_CAP_BYTES + 1);
    assert!(matches!(
        parse_jscpd_report(&huge).unwrap_err(),
        DigestError::JscpdTooLarge(_)
    ));
}

#[test]
fn top_pairs_come_from_the_whole_report_not_the_first_thousand() {
    let json = synthetic_pair_report(1_100, |i| i);
    let candidates = parse_jscpd_report(&json).unwrap();
    assert_eq!(candidates.len(), MAX_DUPLICATE_CANDIDATES);
    let tokens: Vec<u64> = candidates.iter().map(|c| c.tokens).collect();
    assert_eq!(tokens, (1080..=1099).rev().collect::<Vec<u64>>());
}

#[test]
fn oversized_report_file_is_a_typed_error() {
    let dir = tmp_dir("jscpd-oversized");
    let path = dir.join("jscpd-report.json");
    fs::write(&path, "x".repeat(JSCPD_REPORT_CAP_BYTES + 1)).unwrap();
    assert!(matches!(
        read_jscpd_report(&path).unwrap_err(),
        DigestError::JscpdTooLarge(_)
    ));
}

#[test]
fn missing_report_file_is_a_typed_error() {
    let err = read_jscpd_report(Path::new("/nonexistent/jscpd-report.json")).unwrap_err();
    assert!(matches!(err, DigestError::Jscpd(_)));
}

#[test]
fn a_hung_jscpd_is_killed_at_its_deadline() {
    let dir = tmp_dir("jscpd-deadline");
    let program = dir.join("jscpd");
    fs::write(&program, "#!/bin/sh\nsleep 30\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let runner = JscpdRunner::with_program(&program).with_deadline(Duration::from_millis(200));
    let mut bundle = Bundle::new(
        "storytold".to_string(),
        Window {
            since: "2026-10-01".to_string(),
            until: "2026-10-11".to_string(),
            source: WindowSource::Marker,
        },
    );
    let started = Instant::now();
    let candidates = run_jscpd(&runner, &dir, &dir.join("report"), &mut bundle).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "child not killed"
    );
    assert!(candidates.is_empty());
    assert_eq!(bundle.errors.len(), 1);
    assert_eq!(bundle.errors[0].repo, "bundle");
    assert_eq!(bundle.errors[0].stage, "jscpd");
    assert!(bundle.errors[0].message.contains("deadline"));
    let _ = fs::remove_dir_all(&dir);
}
