//! Scope-denial rows: the per-step `deny-read`/`deny-write` globs
//! enforced at the tool boundary. Reads and writes each fail closed on
//! their own list, edit carries both, bash is refused on command text,
//! and glob/grep silently exclude denied paths.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{assert_clean, fast_limits, run, terminal, tmp_ws, write};
use craft_tools::scope::ScopeDenies;
use craft_tools::{Registry, ToolCtx, ToolError, ToolEvent};
use serde_json::{Value, json};
use std::path::Path;

type Events = Vec<Result<ToolEvent, ToolError>>;

const DENY_READ: [&str; 1] = ["tests/**"];
const DENY_WRITE: [&str; 1] = ["secrets/**"];

fn reg() -> Registry {
    Registry::with_builtins(fast_limits())
}

fn ctx(ws: &Path, deny_read: &[&str], deny_write: &[&str]) -> ToolCtx {
    ToolCtx::with_scopes(
        ws.to_path_buf(),
        ScopeDenies::new(
            deny_read.iter().map(|g| g.to_string()).collect(),
            deny_write.iter().map(|g| g.to_string()).collect(),
        ),
    )
}

async fn call(reg: &Registry, ctx: &ToolCtx, tool: &str, args: Value) -> Events {
    run(reg.execute(tool, args, ctx)).await
}

fn scope_error(events: &Events) -> String {
    let error = events
        .iter()
        .find_map(|e| e.as_ref().err())
        .expect("a scope error event");
    assert!(
        matches!(error, ToolError::ScopeDenied { .. }),
        "expected ScopeDenied, got {error:?}"
    );
    format!("{error}")
}

/// One denied-read case: the fixture, the tool call, and the fragments
/// the denial message must carry.
struct ReadDenyCase {
    fixture: (&'static str, &'static str),
    tool: &'static str,
    args: Value,
    expects: (&'static str, &'static str),
}

async fn read_denies(case: ReadDenyCase) {
    let ws = tmp_ws("scope");
    write(&ws, case.fixture.0, case.fixture.1);
    let events = call(&reg(), &ctx(&ws, &DENY_READ, &[]), case.tool, case.args).await;
    let message = scope_error(&events);
    assert!(message.contains(case.expects.0), "{message}");
    assert!(message.contains(case.expects.1), "{message}");
}

#[tokio::test]
async fn the_read_deny_list_fails_closed_on_the_denied_path() {
    read_denies(ReadDenyCase {
        fixture: ("tests/calc_spec.py", "def test_x():\n    pass\n"),
        tool: "read_file",
        args: json!({ "path": "tests/calc_spec.py" }),
        expects: ("tests/**", "read"),
    })
    .await;
}

#[tokio::test]
async fn an_undenied_read_still_succeeds() {
    let ws = tmp_ws("scope");
    write(&ws, "src/main.rs", "fn main() {}\n");
    let events = call(
        &reg(),
        &ctx(&ws, &DENY_READ, &[]),
        "read_file",
        json!({ "path": "src/main.rs" }),
    )
    .await;
    assert_clean(&events);
    assert!(terminal(&events).contains("fn main"));
}

#[tokio::test]
async fn the_write_deny_list_fails_closed_on_the_denied_path() {
    let ws = tmp_ws("scope");
    let events = call(
        &reg(),
        &ctx(&ws, &[], &DENY_WRITE),
        "write_file",
        json!({ "path": "secrets/keys.env", "content": "k=v\n" }),
    )
    .await;
    let message = scope_error(&events);
    assert!(message.contains("secrets/**"), "{message}");
    assert!(message.contains("write"), "{message}");
    assert!(!ws.join("secrets").join("keys.env").exists());
}

#[tokio::test]
async fn edit_file_checks_both_lists() {
    let ws = tmp_ws("scope");
    write(&ws, "tests/a_spec.py", "x = 1\n");
    write(&ws, "secrets/keys.env", "k=old\n");
    let ctx = ctx(&ws, &DENY_READ, &DENY_WRITE);
    let read_denied = call(
        &reg(),
        &ctx,
        "edit_file",
        json!({ "path": "tests/a_spec.py", "old_string": "x = 1", "new_string": "x = 2" }),
    )
    .await;
    assert!(scope_error(&read_denied).contains("read"));
    let read_keys = call(
        &reg(),
        &ctx,
        "read_file",
        json!({ "path": "secrets/keys.env" }),
    )
    .await;
    assert_clean(&read_keys);
    let write_denied = call(
        &reg(),
        &ctx,
        "edit_file",
        json!({ "path": "secrets/keys.env", "old_string": "k=old", "new_string": "k=new" }),
    )
    .await;
    assert!(scope_error(&write_denied).contains("write"));
    assert!(
        terminal(
            &call(
                &reg(),
                &ctx,
                "read_file",
                json!({ "path": "secrets/keys.env" }),
            )
            .await
        )
        .contains("k=old")
    );
}

#[tokio::test]
async fn bash_refuses_a_command_that_reads_the_denied_path() {
    read_denies(ReadDenyCase {
        fixture: ("tests/a_spec.py", "x = 1\n"),
        tool: "bash",
        args: json!({ "command": "cat tests/a_spec.py" }),
        expects: ("bash", "tests/**"),
    })
    .await;
}

#[tokio::test]
async fn glob_and_grep_exclude_the_denied_paths() {
    let ws = tmp_ws("scope");
    write(&ws, "tests/a_spec.py", "needle\n");
    write(&ws, "src/lib.rs", "needle\n");
    let ctx = ctx(&ws, &DENY_READ, &[]);
    let globbed = call(&reg(), &ctx, "glob", json!({ "pattern": "**/*" })).await;
    let listed = terminal(&globbed);
    assert!(listed.contains("src/lib.rs"), "{listed}");
    assert!(!listed.contains("a_spec.py"), "{listed}");
    let grepped = call(
        &reg(),
        &ctx,
        "grep",
        json!({ "pattern": "needle", "path": "." }),
    )
    .await;
    let hits = terminal(&grepped);
    assert!(hits.contains("src/lib.rs"), "{hits}");
    assert!(!hits.contains("a_spec.py"), "{hits}");
}

#[test]
fn an_empty_deny_list_denies_nothing() {
    let ws = std::env::temp_dir();
    let denies = ScopeDenies::default();
    assert!(denies.denies_read(&ws, &ws.join("tests/a.py")).is_none());
    assert!(denies.denies_write(&ws, &ws.join("tests/a.py")).is_none());
    assert!(denies.command_mentions_denied("cat tests/a.py").is_none());
}

#[test]
fn a_hostile_glob_that_reaches_enforcement_matches_nothing() {
    let ws = std::env::temp_dir();
    let denies = ScopeDenies::new(
        vec!["../secrets/**".to_string()],
        vec!["/etc/**".to_string()],
    );
    let probe = ws.join("secrets").join("a_key");
    assert!(denies.denies_read(&ws, &probe).is_none());
    assert!(denies.denies_write(&ws, &probe).is_none());
    assert!(
        denies
            .command_mentions_denied("cat ../secrets/a_key")
            .is_some(),
        "the command screen fails closed on the glob's literal"
    );
}
