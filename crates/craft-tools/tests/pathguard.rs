//! Symlink containment rows: dangling links, sensitive aliases,
//! and outside targets are denied; plain within-root links still work.
//! The secret-name bans are proven end to end here too.

mod common;

use common::{ctx_for, error, run, tmp_ws, write};
use craft_tools::{Registry, ToolError, ToolLimits};
use serde_json::json;
use std::fs;
use std::os::unix::fs::symlink;

fn read(reg: &Registry, ctx: &craft_tools::ToolCtx, rel: &str) -> ToolError {
    let f = async { run(reg.execute("read_file", json!({ "path": rel }), ctx)).await };
    error(&futures::executor::block_on(f))
}

fn write_events(
    reg: &Registry,
    ctx: &craft_tools::ToolCtx,
    rel: &str,
    content: &str,
) -> Vec<Result<craft_tools::ToolEvent, ToolError>> {
    let f = async {
        run(reg.execute(
            "write_file",
            json!({ "path": rel, "content": content }),
            ctx,
        ))
        .await
    };
    futures::executor::block_on(f)
}

// A symlink inside the workspace pointing at a not-yet-existing file in
// a sibling directory: read and write must both be denied, and the
// denied write must not create the outside target.
#[allow(clippy::expect_used)] // fixture builder outside a test fn
fn deny_dangling(name: &str, link: &str, path: &str) {
    let ws = tmp_ws(name);
    let target_dir = ws.parent().expect("parent").join(format!("{name}-targets"));
    fs::create_dir_all(&target_dir).expect("target dir");
    let target = target_dir.join("payload.txt");
    symlink(&target, ws.join(link)).expect("dangling symlink");

    let reg = Registry::with_builtins(ToolLimits::default());
    let ctx = ctx_for(&ws);
    assert!(matches!(
        read(&reg, &ctx, path),
        ToolError::EscapesWorkspace { .. }
    ));
    let events = write_events(&reg, &ctx, path, "escaped\n");
    assert!(matches!(error(&events), ToolError::EscapesWorkspace { .. }));
    assert!(!target.exists(), "denied write must not create the target");
    let _ = fs::remove_dir_all(&target_dir);
}

#[test]
fn dangling_symlink_read_and_write_are_denied() {
    deny_dangling("pathguard-dangling", "link", "link");
}

#[test]
fn midpath_dangling_symlink_is_denied() {
    deny_dangling("pathguard-dangling-mid", "linkdir", "linkdir/payload.txt");
}

#[test]
fn symlink_alias_to_sensitive_file_is_denied() {
    let ws = tmp_ws("pathguard-alias");
    write(&ws, ".env", "SECRET=abcdef\n");
    symlink(ws.join(".env"), ws.join("notes")).expect("alias symlink");

    let reg = Registry::with_builtins(ToolLimits::default());
    let ctx = ctx_for(&ws);
    assert!(matches!(
        read(&reg, &ctx, "notes"),
        ToolError::SensitivePath { .. }
    ));
    let events = write_events(&reg, &ctx, "notes", "overwritten\n");
    assert!(matches!(error(&events), ToolError::SensitivePath { .. }));
    assert_eq!(
        fs::read_to_string(ws.join(".env")).expect("unchanged"),
        "SECRET=abcdef\n"
    );
}

#[test]
fn plain_within_root_symlink_still_resolves() {
    let ws = tmp_ws("pathguard-plain");
    write(&ws, "readme.txt", "hello\n");
    symlink(ws.join("readme.txt"), ws.join("alias.txt")).expect("plain symlink");

    let reg = Registry::with_builtins(ToolLimits::default());
    let ctx = ctx_for(&ws);
    let f = async { run(reg.execute("read_file", json!({ "path": "alias.txt" }), &ctx)).await };
    let events = futures::executor::block_on(f);
    assert!(
        !events.iter().any(|e| e.is_err()),
        "within-root symlink read should pass: {events:?}"
    );
}

// The guarded open re-resolves and opens in one pass, so a component
// swapped to an escaping symlink after the guard walk is refused. The
// swap is written before the call here — plain File::open would follow
// these links, which is the regression these rows pin.
#[test]
fn swapped_final_symlink_escape_is_denied_at_open() {
    use craft_tools::pathguard::{GuardedOpen, open_guarded};
    let ws = tmp_ws("pathguard-swap-final");
    let outside = ws
        .parent()
        .expect("parent")
        .join("craft-tools-swap-final-outside.txt");
    fs::write(&outside, "escaped\n").expect("outside write");
    symlink(&outside, ws.join("victim.txt")).expect("escaping symlink");

    let roots = [ws.clone()];
    let err = open_guarded(&ws.join("victim.txt"), &roots, GuardedOpen::Read, "read")
        .expect_err("escaping symlink must be denied");
    assert!(matches!(err, ToolError::EscapesWorkspace { .. }), "{err:?}");
    let err = open_guarded(
        &ws.join("victim.txt"),
        &roots,
        GuardedOpen::Truncate,
        "write",
    )
    .expect_err("escaping symlink must be denied");
    assert!(matches!(err, ToolError::EscapesWorkspace { .. }), "{err:?}");
    assert_eq!(
        fs::read_to_string(&outside).expect("untouched"),
        "escaped\n"
    );
}

#[test]
fn swapped_midpath_symlink_escape_is_denied_at_open() {
    use craft_tools::pathguard::{GuardedOpen, open_guarded};
    let ws = tmp_ws("pathguard-swap-mid");
    let outside = ws
        .parent()
        .expect("parent")
        .join("craft-tools-swap-mid-outside");
    fs::create_dir_all(&outside).expect("outside dir");
    symlink(&outside, ws.join("linkdir")).expect("escaping dir symlink");

    let roots = [ws.clone()];
    let err = open_guarded(
        &ws.join("linkdir/payload.txt"),
        &roots,
        GuardedOpen::Append,
        "write",
    )
    .expect_err("escaping mid-path symlink must be denied");
    assert!(matches!(err, ToolError::EscapesWorkspace { .. }), "{err:?}");
    assert!(!outside.join("payload.txt").exists(), "no write outside");
}

#[test]
fn guarded_open_honors_within_root_final_symlink_and_creates() {
    use craft_tools::pathguard::{GuardedOpen, open_guarded};
    let ws = tmp_ws("pathguard-open-ok");
    write(&ws, "real.txt", "seed\n");
    symlink(ws.join("real.txt"), ws.join("alias.txt")).expect("plain symlink");

    let roots = [ws.clone()];
    let mut via_alias =
        open_guarded(&ws.join("alias.txt"), &roots, GuardedOpen::Append, "write").expect("open");
    use std::io::Write as _;
    via_alias.write_all(b"more\n").expect("append");
    drop(via_alias);
    assert_eq!(
        fs::read_to_string(ws.join("real.txt")).expect("through alias"),
        "seed\nmore\n"
    );

    let mut fresh =
        open_guarded(&ws.join("new.txt"), &roots, GuardedOpen::Truncate, "write").expect("create");
    fresh.write_all(b"born\n").expect("write");
    drop(fresh);
    assert_eq!(
        fs::read_to_string(ws.join("new.txt")).expect("created"),
        "born\n"
    );
}

#[test]
fn secret_names_are_denied_and_pub_forms_pass() {
    let ws = tmp_ws("pathguard-secret-names");
    for name in [
        "id_dsa",
        "server.pem",
        "site.key",
        ".netrc",
        ".npmrc",
        "id_dsa.pub",
    ] {
        write(&ws, name, "fixture\n");
    }
    let reg = Registry::with_builtins(ToolLimits::default());
    let ctx = ctx_for(&ws);
    for name in ["id_dsa", "server.pem", "site.key", ".netrc", ".npmrc"] {
        assert!(
            matches!(read(&reg, &ctx, name), ToolError::SensitivePath { .. }),
            "{name} should be denied"
        );
    }
    let f = async { run(reg.execute("read_file", json!({ "path": "id_dsa.pub" }), &ctx)).await };
    let events = futures::executor::block_on(f);
    assert!(
        !events.iter().any(|e| e.is_err()),
        "id_dsa.pub should be readable: {events:?}"
    );
}

#[test]
fn parse_read_roots_rows() {
    use craft_tools::pathguard::parse_read_roots;
    fn roots(raw: &str) -> Vec<std::path::PathBuf> {
        parse_read_roots(Some(raw))
    }
    assert!(parse_read_roots(None).is_empty());
    assert!(roots("").is_empty());
    assert!(roots("   ").is_empty());
    assert!(roots(",,").is_empty());
    assert_eq!(
        roots(" /a , /b ,, sub , "),
        vec![
            std::path::PathBuf::from("/a"),
            std::path::PathBuf::from("/b")
        ]
    );
    assert_eq!(roots("/r1,/r2,/r3,/r4,/r5").len(), 4);
    assert!(roots(&format!("/{}", "x".repeat(4096))).is_empty());
}

#[test]
fn bans_match_and_exemptions_hold() {
    use craft_tools::pathguard::sensitive_reason;
    use std::path::Path;
    for banned in [
        "/ws/.env",
        "/ws/.env.production",
        "/ws/sub/id_rsa",
        "/ws/server.pem",
        "/ws/tls/site.key",
        "/ws/.netrc",
        "/ws/.aws/config",
        "/ws/credentials.json",
    ] {
        assert!(sensitive_reason(Path::new(banned)).is_some(), "{banned}");
    }
    for allowed in ["/ws/.env.example", "/ws/id_rsa.pub", "/ws/notes.txt"] {
        assert_eq!(sensitive_reason(Path::new(allowed)), None, "{allowed}");
    }
}
