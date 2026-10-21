//! Live MCP corpus: one round trip and one error path against a real
//! third-party stdio server. Without TEST_BYOK_MCP_STDIO_CMD in the
//! repo-root .env the corpus prints its SKIP reason and passes.
//! Key names are printable; values never are.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use craft_mcp::events::{McpEvent, McpStatus};
use craft_tools::ToolCtx;
use futures::StreamExt;
use serde_json::json;

type TestError = Box<dyn std::error::Error + Send + Sync>;

// Process env wins; a set value (even empty) is final.
fn read_var(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => file_value(var),
        Err(_) => {
            eprintln!("SKIP: {var} is set but not valid UTF-8; the .env value is not used");
            None
        }
    }
}

fn file_value(var: &str) -> Option<String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut dir = Some(manifest.as_path());
    for _ in 0..4 {
        let Some(current) = dir else { break };
        if let Ok(text) = std::fs::read_to_string(current.join(".env"))
            && let Some(value) = env_file_entry(&text, var)
        {
            return Some(value);
        }
        dir = current.parent();
    }
    None
}

fn env_file_entry(text: &str, var: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == var {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

fn fixture_setup() -> Result<Option<(String, Vec<String>)>, TestError> {
    let Some(command) = read_var("TEST_BYOK_MCP_STDIO_CMD").filter(|c| !c.is_empty()) else {
        eprintln!(
            "SKIP: TEST_BYOK_MCP_STDIO_CMD is not set (no .env) — the live mcp corpus is off"
        );
        return Ok(None);
    };
    let args = match read_var("TEST_BYOK_MCP_STDIO_ARGS") {
        Some(raw) if !raw.is_empty() => serde_json::from_str::<Vec<String>>(&raw).map_err(|_| {
            "TEST_BYOK_MCP_STDIO_ARGS must be a JSON array of strings; the live mcp corpus is off"
                .to_string()
        })?,
        _ => Vec::new(),
    };
    Ok(Some((command, args)))
}

fn fresh_workspace() -> Result<PathBuf, TestError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!("craft-mcp-live-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|err| format!("temp workspace: {err}"))?;
    Ok(root)
}

async fn live_connect(
    command: &str,
    args: &[String],
) -> Result<craft_mcp::ConnectOutcome, TestError> {
    let workspace = fresh_workspace()?;
    std::fs::create_dir_all(workspace.join(".craft-agent"))
        .map_err(|err| format!("config dir: {err}"))?;
    let config = json!({
        "mcpServers": {
            "live": {
                "command": command,
                "args": args,
                "cwd": workspace,
                "startupTimeoutSecs": 30,
                "toolTimeoutSecs": 120,
            }
        }
    });
    std::fs::write(
        workspace.join(".craft-agent").join("mcp.json"),
        serde_json::to_string(&config)?,
    )
    .map_err(|err| format!("mcp.json: {err}"))?;
    let ctx = ToolCtx::with_env_scopes(
        workspace.clone(),
        vec![("CRAFT_AGENT_MCP".to_string(), "1".to_string())],
        craft_tools::scope::ScopeDenies::default(),
    );
    craft_mcp::connect_all(&ctx, None)
        .await
        .map_err(|err| format!("live connect failed: {err}").into())
}

async fn drive(tool: &craft_mcp::ConnectOutcome, name: &str) -> Result<String, String> {
    let tool = tool
        .tools
        .iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| format!("the tool {name} is not registered"))?;
    let ctx = ToolCtx::new(std::env::temp_dir());
    let mut stream = (tool.execute)(json!({}), &ctx);
    match stream.next().await {
        Some(Ok(craft_tools::event::ToolEvent::Terminal(result))) => Ok(result.output),
        Some(Ok(_)) => Err(String::from("unexpected progress event")),
        Some(Err(err)) => Err(err.to_string()),
        None => Err(String::from("the stream ended without a terminal")),
    }
}

/// The error path every real server answers: tools/call for a name it
/// does not serve is a JSON-RPC error, and the client surfaces it as a
/// typed protocol failure.
async fn live_error_path(command: &str, args: &[String]) -> Result<(), TestError> {
    use craft_mcp::child;
    let tail: child::StderrTail = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut conn = child::spawn(
        command,
        args,
        None,
        Vec::new(),
        std::sync::Arc::clone(&tail),
    )
    .map_err(|err| format!("spawn: {err}"))?;
    conn.connect(
        std::time::Instant::now() + std::time::Duration::from_secs(30),
        30_000,
    )
    .await
    .map_err(|err| format!("handshake: {}", err.message()))?;
    let outcome = conn
        .call_tool(
            "definitely-not-a-tool",
            &json!({}),
            std::time::Duration::from_secs(30),
        )
        .await;
    let text = match outcome {
        Err(err) => err.message(),
        Ok(_) => String::from("an unknown tool name unexpectedly succeeded"),
    };
    assert!(
        text.contains("JSON-RPC error"),
        "the error path is typed: {text}"
    );
    conn.shutdown().await;
    Ok(())
}

#[tokio::test]
#[ignore = "live corpus: requires TEST_BYOK_MCP_STDIO_CMD in .env"]
async fn integration_mcp() -> Result<(), TestError> {
    let Some((command, args)) = fixture_setup()? else {
        return Ok(());
    };
    let outcome = live_connect(&command, &args).await?;
    let connected = outcome
        .events
        .iter()
        .filter(|event| {
            matches!(
                event,
                McpEvent::McpServerStatus {
                    status: McpStatus::Connected,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        connected, 1,
        "the live server connects: {:?}",
        outcome.events
    );
    assert!(!outcome.tools.is_empty(), "at least one tool registers");
    let name = outcome.tools[0].name.clone();
    let result = drive(&outcome, &name).await;
    assert!(
        result.is_ok() || result.as_ref().is_err_and(|err| err.contains("mcp server")),
        "the round trip is a typed outcome either way: {result:?}"
    );
    outcome.manager.shutdown().await;
    live_error_path(&command, &args).await
}
