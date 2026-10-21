//! Shared drivers for the craft-tools corpora: fast tool limits,
//! event helpers, and the fixture builders re-exported from `fixtures`.

#![allow(dead_code)]
// Corpus fixtures: expect() is the loud-test idiom here.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod fixtures;

#[allow(unused_imports)]
pub use fixtures::{fake_child_env, fake_gh, tmp_ws, write, write_bytes};

use std::path::Path;
use std::time::Duration;

use craft_tools::scope::ScopeDenies;
use craft_tools::{ToolCtx, ToolError, ToolEvent, ToolLimits, ToolStream};

pub fn fast_limits() -> ToolLimits {
    ToolLimits {
        bash_default_timeout: Duration::from_millis(1200),
        bash_max_timeout: Duration::from_secs(4),
        proc_grace: Duration::from_millis(600),
        proc_out_cap: 4096,
        gh_timeout: Duration::from_secs(8),
        read_cap: 1024,
        write_cap: 1024,
        max_path_bytes: 4096,
        max_command_bytes: 4096,
        max_pattern_bytes: 256,
        max_args: 8,
        glob_max_results: 6,
        grep_max_results: 5,
        walk_max_entries: 500,
        walk_max_depth: 16,
        grep_file_cap: 1024,
        grep_line_cap: 64,
    }
}

pub fn ctx_for(ws: &Path) -> ToolCtx {
    ToolCtx::new(ws.to_path_buf())
}

pub fn ctx_env(
    ws: &Path,
    pairs: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
) -> ToolCtx {
    let env = pairs
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    ToolCtx::with_env_scopes(ws.to_path_buf(), env, ScopeDenies::default())
}

pub async fn run(stream: ToolStream) -> Vec<Result<ToolEvent, ToolError>> {
    use futures::StreamExt;
    stream.collect().await
}

/// The single terminal event, or panics with the actual error.
pub fn terminal(events: &[Result<ToolEvent, ToolError>]) -> String {
    let terminal = events.iter().find_map(|e| match e {
        Ok(ToolEvent::Terminal(t)) => Some(t.output.clone()),
        _ => None,
    });
    match terminal {
        Some(out) => out,
        None => panic!("no terminal event in {:?}", events),
    }
}

/// The first typed error, or panics if the run succeeded.
pub fn error(events: &[Result<ToolEvent, ToolError>]) -> ToolError {
    let err = events.iter().find_map(|e| match e {
        Err(e) => Some(e.clone()),
        _ => None,
    });
    match err {
        Some(e) => e,
        None => panic!("no error event in {:?}", events),
    }
}

/// Progress payloads in arrival order.
pub fn progress(events: &[Result<ToolEvent, ToolError>]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Ok(ToolEvent::Progress(p)) => Some(p.text.clone()),
            _ => None,
        })
        .collect()
}

/// Exactly one terminal and no errors, else panic.
pub fn assert_clean(events: &[Result<ToolEvent, ToolError>]) {
    let terminals = events
        .iter()
        .filter(|e| matches!(e, Ok(ToolEvent::Terminal(_))))
        .count();
    let errors = events.iter().filter(|e| e.is_err()).count();
    assert_eq!(terminals, 1, "expected one terminal: {events:?}");
    assert_eq!(errors, 0, "expected no errors: {events:?}");
}
