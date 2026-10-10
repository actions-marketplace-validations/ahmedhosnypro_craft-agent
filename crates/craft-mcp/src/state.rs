//! Per-server run state: the owned connection, the reconnect-once
//! ladder every failed call walks, and the child-env overlay servers
//! spawn with.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use craft_tools::ToolCtx;
use craft_tools::ToolError;

use crate::catalog::LIST_PAGES;
use crate::child::{self, StderrTail};
use crate::client::{CallError, Conn};
use crate::config::ServerConfig;

pub struct ServerState {
    pub config: ServerConfig,
    pub stderr_tail: StderrTail,
    pub conn: AsyncMutex<Option<Conn>>,
    pub generation: AtomicU64,
    pub startup_timeout: Duration,
    pub tool_timeout: Duration,
}

/// The scrubbed child env plus the per-server overlay, config winning.
pub fn child_env_with_overlay(ctx: &ToolCtx, config: &ServerConfig) -> Vec<(String, String)> {
    let mut env = ctx.child_env();
    for (key, value) in &config.env {
        match env.iter_mut().find(|(existing, _)| existing == key) {
            Some(slot) => slot.1 = value.clone(),
            None => env.push((key.clone(), value.clone())),
        }
    }
    env
}

/// The per-server timeout: an explicit config field wins; the env knob
/// overrides the default only. Both sources arrive pre-validated.
pub(crate) fn effective_timeout(configured: Option<u64>, from_knob: u64) -> u64 {
    configured.unwrap_or(from_knob)
}

fn to_tool_error(server: &str, tool: &str, err: CallError) -> ToolError {
    match &err {
        CallError::Malformed(detail) => ToolError::McpMalformedResult {
            server: server.to_string(),
            detail: detail.clone(),
        },
        CallError::Timeout { ms } => ToolError::McpToolTimeout {
            server: server.to_string(),
            timeout_ms: *ms,
        },
        CallError::ArgsTooLarge { bytes } => ToolError::InvalidArgs {
            tool: tool.to_string(),
            msg: format!("arguments serialize to {bytes} bytes, over the wire cap"),
        },
        CallError::Transport(_) | CallError::VersionRejected { .. } | CallError::Rpc { .. } => {
            ToolError::McpServerFailed {
                server: server.to_string(),
                reason: err.message(),
            }
        }
    }
}

/// The server's failure line with its bounded stderr tail appended.
fn with_tail(server: &str, reason: &str, tail: &StderrTail) -> ToolError {
    let tail = tail
        .lock()
        .map(|t| String::from_utf8_lossy(&t).into_owned())
        .unwrap_or_default();
    let reason = if tail.is_empty() {
        reason.to_string()
    } else {
        format!("{reason}; stderr tail: {tail}")
    };
    ToolError::McpServerFailed {
        server: server.to_string(),
        reason,
    }
}

/// Spawn and fully connect one server: handshake plus a tools/list
/// resync, bounded by the startup timeout. The definitions return for
/// the discovery record.
pub async fn spawn_connected(
    config: &ServerConfig,
    child_env: Vec<(String, String)>,
    stderr_tail: StderrTail,
    startup_timeout: Duration,
) -> Result<(Conn, Vec<Value>), String> {
    let deadline = Instant::now() + startup_timeout;
    let ms = startup_timeout.as_millis().min(u64::MAX as u128) as u64;
    let mut conn = child::spawn(
        &config.command,
        &config.args,
        config.cwd.as_deref(),
        child_env,
        stderr_tail,
    )?;
    conn.connect(deadline, ms)
        .await
        .map_err(|err| err.message())?;
    let defs = conn
        .list_tools(deadline, ms, LIST_PAGES)
        .await
        .map_err(|err| err.message())?;
    Ok((conn, defs))
}

/// The reconnect-once ladder around one tools/call: a transport
/// failure triggers at most one reconnect (deduped by generation,
/// which advances on success and on failure so queued callers do not
/// each respawn a dead server) and one retry; the combined error names
/// both causes. JSON-RPC errors, timeouts, and malformed output return
/// without a retry.
pub async fn raw_call(
    state: &ServerState,
    child_env: Vec<(String, String)>,
    raw_name: &str,
    qualified: &str,
    arguments: Value,
) -> Result<Value, ToolError> {
    let server = state.config.name.clone();
    let before = state.generation.load(Ordering::Relaxed);
    let mut guard = state.conn.lock().await;
    let first = match guard.as_mut() {
        Some(conn) => {
            conn.call_tool(raw_name, &arguments, state.tool_timeout)
                .await
        }
        None => Err(CallError::Transport(String::from(
            "the server is not connected",
        ))),
    };
    let first_reason = match first {
        Ok(value) => return Ok(value),
        Err(CallError::Transport(first_reason)) => first_reason,
        Err(other) => return Err(to_tool_error(&server, qualified, other)),
    };
    if state.generation.load(Ordering::Relaxed) != before {
        let second = match guard.as_mut() {
            Some(conn) => {
                conn.call_tool(raw_name, &arguments, state.tool_timeout)
                    .await
            }
            None => Err(CallError::Transport(String::from(
                "the server is not connected",
            ))),
        };
        return second.map_err(|err| {
            with_tail(
                &server,
                &format!(
                    "call failed: {first_reason}; after a concurrent reconnect: {}",
                    err.message()
                ),
                &state.stderr_tail,
            )
        });
    }
    let reconnect = spawn_connected(
        &state.config,
        child_env,
        Arc::clone(&state.stderr_tail),
        state.startup_timeout,
    )
    .await;
    match reconnect {
        Ok((conn, _defs)) => {
            state.generation.store(before + 1, Ordering::Relaxed);
            *guard = Some(conn);
        }
        Err(reconnect_err) => {
            state.generation.store(before + 1, Ordering::Relaxed);
            *guard = None;
            let reason = format!("{first_reason}; reconnect failed: {reconnect_err}");
            return Err(with_tail(&server, &reason, &state.stderr_tail));
        }
    }
    let second = match guard.as_mut() {
        Some(conn) => {
            conn.call_tool(raw_name, &arguments, state.tool_timeout)
                .await
        }
        None => Err(CallError::Transport(String::from(
            "the server is not connected",
        ))),
    };
    match second {
        Ok(value) => Ok(value),
        Err(err) => {
            let reason = format!(
                "call failed: {first_reason}; after reconnect: {}",
                err.message()
            );
            Err(with_tail(&server, &reason, &state.stderr_tail))
        }
    }
}
