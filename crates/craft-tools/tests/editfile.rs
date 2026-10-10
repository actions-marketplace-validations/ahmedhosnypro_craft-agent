//! edit_file: prior-read requirement, exact-match replacement, ambiguity,
//! and changed-on-disk detection. One ToolCtx per test — the read stamp
//! lives in the ctx, so a fresh ctx per call would lose it (that
//! isolation is the runtime's guarantee, so the corpus pins it).

mod common;

use common::{
    assert_clean, ctx_for, error, fast_limits, run, terminal, tmp_ws, write, write_bytes,
};
use craft_tools::{Registry, ToolCtx, ToolError};
use serde_json::json;
use std::fs;

fn reg() -> Registry {
    Registry::with_builtins(fast_limits())
}

async fn edit(
    ctx: &ToolCtx,
    args: serde_json::Value,
) -> Vec<Result<craft_tools::ToolEvent, ToolError>> {
    run(reg().execute("edit_file", args, ctx)).await
}

// Every row seeds f.txt and reads it; the shared name stays unique per
// call through tmp_ws's sequence counter.
async fn seeded(content: &[u8]) -> (std::path::PathBuf, ToolCtx) {
    let ws = tmp_ws("edit-seed");
    let ctx = ctx_for(&ws);
    write_bytes(&ws, "f.txt", content);
    let _ = run(reg().execute("read_file", json!({ "path": "f.txt" }), &ctx)).await;
    (ws, ctx)
}

fn edit_args(old: &str, new: &str, all: bool) -> serde_json::Value {
    json!({ "path": "f.txt", "old_string": old, "new_string": new, "replace_all": all })
}

fn file_bytes(ws: &std::path::Path) -> Vec<u8> {
    fs::read(ws.join("f.txt")).unwrap_or_default()
}

async fn edit_ok(ws: &std::path::Path, ctx: &ToolCtx, args: serde_json::Value, expect: &[u8]) {
    let events = edit(ctx, args).await;
    assert_clean(&events);
    assert_eq!(file_bytes(ws), expect);
}

async fn replace(seed: &[u8], old: &str, new: &str, expect: &[u8]) {
    let (ws, ctx) = seeded(seed).await;
    edit_ok(&ws, &ctx, edit_args(old, new, false), expect).await;
}

async fn replace_all(seed: &[u8], old: &str, new: &str, expect: &[u8]) {
    let (ws, ctx) = seeded(seed).await;
    edit_ok(&ws, &ctx, edit_args(old, new, true), expect).await;
}

#[tokio::test]
async fn edit_requires_a_prior_read() {
    let ws = tmp_ws("edit-noread");
    let ctx = ctx_for(&ws);
    write(&ws, "f.txt", "alpha beta\n");
    let events = edit(&ctx, edit_args("alpha", "gamma", false)).await;
    assert!(matches!(error(&events), ToolError::NotReadFirst { .. }));
    assert_eq!(file_bytes(&ws), b"alpha beta\n");
}

#[tokio::test]
async fn writing_the_file_does_not_satisfy_the_prior_read_rule() {
    let ws = tmp_ws("edit-afterwrite");
    let ctx = ctx_for(&ws);
    let events = run(reg().execute(
        "write_file",
        json!({ "path": "f.txt", "content": "alpha\n" }),
        &ctx,
    ))
    .await;
    assert_clean(&events);
    let events = edit(&ctx, edit_args("alpha", "gamma", false)).await;
    assert!(matches!(error(&events), ToolError::NotReadFirst { .. }));
}

#[tokio::test]
async fn exact_match_edit_replaces_once() {
    replace(b"alpha beta gamma\n", "beta", "BETA", b"alpha BETA gamma\n").await;
}

#[tokio::test]
async fn consecutive_edits_after_one_read_are_allowed() {
    let (ws, ctx) = seeded(b"one two\n").await;
    edit_ok(&ws, &ctx, edit_args("one", "ONE", false), b"ONE two\n").await;
    edit_ok(&ws, &ctx, edit_args("two", "TWO", false), b"ONE TWO\n").await;
}

#[tokio::test]
async fn missing_match_is_a_typed_error() {
    let (_ws, ctx) = seeded(b"alpha\n").await;
    let events = edit(&ctx, edit_args("zeta", "eta", false)).await;
    assert!(matches!(error(&events), ToolError::EditNoMatch { .. }));
}

#[tokio::test]
async fn ambiguous_match_requires_uniqueness_or_replace_all() {
    let (ws, ctx) = seeded(b"x x x\n").await;

    let events = edit(&ctx, edit_args("x", "y", false)).await;
    match error(&events) {
        ToolError::EditAmbiguous { count, .. } => assert_eq!(count, 3),
        other => panic!("expected EditAmbiguous, got {other:?}"),
    }

    let events = edit(&ctx, edit_args("x", "y", true)).await;
    assert_clean(&events);
    assert_eq!(file_bytes(&ws), b"y y y\n");
    assert!(
        terminal(&events).contains("3"),
        "confirmation reports the count"
    );
}

#[tokio::test]
async fn empty_old_string_is_rejected() {
    let (_ws, ctx) = seeded(b"data\n").await;
    let events = edit(&ctx, edit_args("", "z", false)).await;
    assert!(matches!(error(&events), ToolError::InvalidArgs { .. }));
}

#[tokio::test]
async fn external_modification_after_read_is_detected() {
    let (ws, ctx) = seeded(b"original\n").await;
    fs::write(ws.join("f.txt"), "replaced externally\n").expect("outside write");
    let events = edit(&ctx, edit_args("replaced externally", "edited", false)).await;
    assert!(
        matches!(error(&events), ToolError::ChangedOnDisk { .. }),
        "a matching old_string must not rescue a changed file: {events:?}"
    );
    assert_eq!(file_bytes(&ws), b"replaced externally\n");
}

#[tokio::test]
async fn rereading_after_external_change_reenables_edit() {
    let (ws, ctx) = seeded(b"original\n").await;
    fs::write(ws.join("f.txt"), "fresh content\n").expect("outside write");

    let _ = run(reg().execute("read_file", json!({ "path": "f.txt" }), &ctx)).await;
    let events = edit(&ctx, edit_args("fresh", "crisp", false)).await;
    assert_clean(&events);
    assert_eq!(file_bytes(&ws), b"crisp content\n");
}

#[tokio::test]
async fn edit_result_over_the_write_cap_is_rejected_untouched() {
    let (ws, ctx) = seeded(b"aaa\n").await;
    let big = "z".repeat(2000);
    let events = edit(&ctx, edit_args("a", &big, true)).await;
    match error(&events) {
        ToolError::FileTooLarge { size, limit, .. } => {
            assert_eq!(size, 4 - 3 + 3 * 2000);
            assert_eq!(limit, 1024);
        }
        other => panic!("expected FileTooLarge, got {other:?}"),
    }
    assert_eq!(file_bytes(&ws), b"aaa\n");
}

#[tokio::test]
async fn replace_all_skips_overlapping_occurrences() {
    replace_all(b"aaaa\n", "aa", "y", b"yy\n").await;
}

#[tokio::test]
async fn replace_all_finds_matches_after_a_partial_prefix() {
    replace_all(b"aabaaab\n", "aab", "X", b"XaX\n").await;
}
