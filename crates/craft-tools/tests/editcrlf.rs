//! edit_file's line-ending model: pure-CRLF files match on an LF view
//! and write back CRLF without doubling CRs; mixed or lone-CR files are
//! refused with a remedy hint and stay byte-exact.

mod common;

use common::{assert_clean, ctx_for, error, fast_limits, run, tmp_ws, write_bytes};
use craft_tools::{Registry, ToolCtx, ToolError, ToolEvent};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

fn reg() -> Registry {
    Registry::with_builtins(fast_limits())
}

async fn edit(ctx: &ToolCtx, args: serde_json::Value) -> Vec<Result<ToolEvent, ToolError>> {
    run(reg().execute("edit_file", args, ctx)).await
}

fn edit_args(old: &str, new: &str, all: bool) -> serde_json::Value {
    json!({ "path": "f.txt", "old_string": old, "new_string": new, "replace_all": all })
}

// Every row seeds f.txt, reads it, and hands back the workspace; the
// shared name stays unique per call through tmp_ws's sequence counter.
async fn seeded(bytes: &[u8]) -> (PathBuf, ToolCtx) {
    let ws = tmp_ws("crlf-seed");
    let ctx = ctx_for(&ws);
    write_bytes(&ws, "f.txt", bytes);
    let events = run(reg().execute("read_file", json!({ "path": "f.txt" }), &ctx)).await;
    assert_clean(&events);
    (ws, ctx)
}

async fn edit_ok(ws: &Path, ctx: &ToolCtx, args: serde_json::Value, expect: &[u8]) {
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

async fn refuse_mixed(seed: &[u8], old: &str, new: &str, expect: &[u8]) {
    let (ws, ctx) = seeded(seed).await;
    let events = edit(&ctx, edit_args(old, new, false)).await;
    let err = error(&events);
    assert!(matches!(err, ToolError::EditMixedEndings { .. }), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("mixed"), "{text}");
    assert!(text.contains("write_file"), "{text}");
    assert_eq!(file_bytes(&ws), expect);
}

fn file_bytes(ws: &Path) -> Vec<u8> {
    fs::read(ws.join("f.txt")).unwrap_or_default()
}

#[tokio::test]
async fn crlf_file_matches_lf_view_and_writes_crlf() {
    replace(b"alpha\r\nbeta\r\n", "beta", "BETA", b"alpha\r\nBETA\r\n").await;
}

#[tokio::test]
async fn crlf_old_string_with_pairs_also_matches() {
    replace(
        b"alpha\r\nbeta\r\n",
        "alpha\r\nbeta",
        "alpha\r\ngamma",
        b"alpha\r\ngamma\r\n",
    )
    .await;
}

#[tokio::test]
async fn crlf_new_string_never_doubles_carriage_returns() {
    let (ws, ctx) = seeded(b"a\r\nb\r\n").await;
    let events = edit(&ctx, edit_args("b", "x\r\ny", false)).await;
    assert_clean(&events);
    let bytes = file_bytes(&ws);
    assert_eq!(bytes, b"a\r\nx\r\ny\r\n");
    assert!(
        !bytes.windows(2).any(|w| w == b"\r\r"),
        "a CR must never double: {bytes:?}"
    );
}

#[tokio::test]
async fn crlf_replace_all_stays_pure_crlf() {
    replace_all(b"a\r\na\r\na\r\n", "a", "b", b"b\r\nb\r\nb\r\n").await;
}

#[tokio::test]
async fn consecutive_crlf_edits_hold_the_stamp() {
    let (ws, ctx) = seeded(b"one\r\ntwo\r\n").await;
    edit_ok(&ws, &ctx, edit_args("one", "ONE", false), b"ONE\r\ntwo\r\n").await;
    edit_ok(&ws, &ctx, edit_args("two", "TWO", false), b"ONE\r\nTWO\r\n").await;
}

#[tokio::test]
async fn lf_files_stay_byte_exact() {
    replace(b"a\nb\n", "b", "B", b"a\nB\n").await;
}

#[tokio::test]
async fn crlf_file_with_unterminated_last_line_is_still_crlf() {
    replace(b"a\r\nb", "b", "B", b"a\r\nB").await;
}

#[tokio::test]
async fn mixed_endings_file_is_refused_with_a_hint() {
    refuse_mixed(b"a\r\nb\nc\n", "b", "B", b"a\r\nb\nc\n").await;
}

#[tokio::test]
async fn lone_cr_file_is_refused_with_a_hint() {
    refuse_mixed(b"a\rb\n", "a", "A", b"a\rb\n").await;
}

#[tokio::test]
async fn mixed_refusal_loses_to_the_prior_read_rule() {
    let ws = tmp_ws("crlf-mixed-noread");
    let ctx = ctx_for(&ws);
    write_bytes(&ws, "f.txt", b"a\r\nb\nc\n");
    let events = edit(&ctx, edit_args("b", "B", false)).await;
    assert!(matches!(error(&events), ToolError::NotReadFirst { .. }));
    assert_eq!(file_bytes(&ws), b"a\r\nb\nc\n");
}

#[tokio::test]
async fn lf_only_strings_do_not_match_a_mixed_file() {
    let (ws, ctx) = seeded(b"a\r\nb\nc\n").await;
    let events = edit(&ctx, edit_args("a\nb", "z", false)).await;
    let err = error(&events);
    assert!(
        matches!(err, ToolError::EditMixedEndings { .. }),
        "mixed files are refused before matching: {err:?}"
    );
    assert_eq!(file_bytes(&ws), b"a\r\nb\nc\n");
}
