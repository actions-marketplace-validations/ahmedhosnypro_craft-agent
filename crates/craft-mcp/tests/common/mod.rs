//! Shared helpers for the craft-mcp corpora: a fresh workspace per
//! test and an mcp.json writer. Each test binary compiles this module
//! and uses only part of it, so unused helpers ride dead.
#![allow(dead_code)]
// Corpus helpers and fixtures: the loud-test idiom for panics.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// pid + sequence keeps concurrent same-tick callers apart.
static WORKSPACE_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn fresh_workspace(tag: &str) -> PathBuf {
    let seq = WORKSPACE_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!(
        "craft-mcp-{tag}-{}-{nanos}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("the temp workspace was created");
    root
}

/// Writes `<workspace>/.craft-agent/mcp.json`.
pub fn write_mcp_json(workspace: &Path, body: &str) {
    let dir = workspace.join(".craft-agent");
    std::fs::create_dir_all(&dir).expect("the config dir was created");
    std::fs::write(dir.join("mcp.json"), body).expect("mcp.json was written");
}

pub fn fixture_command() -> String {
    env!("CARGO_BIN_EXE_mcp-fixture").to_string()
}

/// One server entry with defaults trimmed to what the corpus needs.
pub fn server_json(command: &str, extra: &str) -> String {
    if extra.is_empty() {
        format!("{{\"command\": {command:?}}}")
    } else {
        format!("{{\"command\": {command:?}, {extra}}}")
    }
}

use craft_mcp::events::McpEvent;
use craft_tools::ToolCtx;
use craft_tools::event::ToolEvent;
use futures::StreamExt;

pub async fn connect(
    workspace: &Path,
    env: &[(&str, &str)],
) -> Result<craft_mcp::ConnectOutcome, craft_mcp::McpError> {
    let pairs: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let ctx = ToolCtx::with_env(workspace.to_path_buf(), pairs);
    craft_mcp::connect_all(&ctx, None).await
}

pub fn mcp_on() -> [(&'static str, &'static str); 1] {
    [("CRAFT_AGENT_MCP", "1")]
}

pub fn statuses(outcome: &craft_mcp::ConnectOutcome) -> Vec<&McpEvent> {
    outcome
        .events
        .iter()
        .filter(|event| matches!(event, McpEvent::McpServerStatus { .. }))
        .collect()
}

/// Drives one dynamic tool to its single stream item.
pub async fn call_tool(
    outcome: &craft_mcp::ConnectOutcome,
    name: &str,
    args: serde_json::Value,
) -> Result<String, String> {
    let tool = outcome
        .tools
        .iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("the tool {name} is registered"));
    let workspace = fresh_workspace("call-scratch");
    let ctx = ToolCtx::new(workspace);
    let mut stream = (tool.execute)(args, &ctx);
    match stream.next().await {
        Some(Ok(ToolEvent::Terminal(result))) => Ok(result.output),
        Some(Ok(_)) => Err(String::from("progress from a terminal-only stream")),
        Some(Err(err)) => Err(err.to_string()),
        None => Err(String::from("the stream ended without an item")),
    }
}

pub fn one_server_workspace(args: &[&str]) -> PathBuf {
    let workspace = fresh_workspace("manager");
    let mut extra = format!("\"args\": {:?}", args);
    extra.push_str(", \"startupTimeoutSecs\": 2, \"toolTimeoutSecs\": 2");
    write_mcp_json(
        &workspace,
        &format!(
            "{{\"mcpServers\": {{\"srv\": {}}}}}",
            server_json(&fixture_command(), &extra)
        ),
    );
    workspace
}

pub fn three_server_workspace(tool_args: &str) -> PathBuf {
    let workspace = fresh_workspace("manager");
    let fixture = fixture_command();
    let entry =
        format!("{{\"command\": {fixture:?}, \"args\": [{tool_args}], \"startupTimeoutSecs\": 2}}");
    write_mcp_json(
        &workspace,
        &format!(
            "{{\"mcpServers\": {{\"alpha\": {entry}, \"beta\": {entry}, \"gamma\": {entry}, \"off\": {{\"command\": {fixture:?}, \"enabled\": false}}}}}}"
        ),
    );
    workspace
}
