//! Connect-before-run: resolve the knobs, load the merged config,
//! connect every enabled server in parallel, and register the
//! discovered tools. Config violations fail the run closed; per-server
//! connect failures never do — they ride status events and the run
//! proceeds.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::task::{Id, JoinError, JoinSet};

use craft_tools::{DynamicTool, ToolCtx};

use crate::client::{Conn, StderrTail};
use crate::config::{Loaded, ServerConfig, load_config};
use crate::error::McpError;
use crate::events::McpEvent;
use crate::register::{Assembly, Connected};
use crate::state::{self, ServerState};
use crate::{
    DEFAULT_STARTUP_TIMEOUT_SECS, DEFAULT_TOOL_TIMEOUT_SECS, MAX_TOOLS_KNOB, MCP_KNOB,
    STARTUP_TIMEOUT_KNOB, TOOL_TIMEOUT_KNOB, knob_enabled, max_tools_from, timeout_secs_from,
};

pub struct ConnectOutcome {
    pub events: Vec<McpEvent>,
    pub tools: Vec<DynamicTool>,
    pub manager: Manager,
}

pub struct Manager {
    servers: Vec<Arc<ServerState>>,
}

impl Manager {
    /// The shutdown ladder for every live server: close stdin, wait,
    /// SIGTERM, then SIGKILL.
    pub async fn shutdown(self) {
        for server in self.servers {
            let conn = server.conn.lock().await.take();
            if let Some(conn) = conn {
                conn.shutdown().await;
            }
        }
    }
}

enum ServerOutcome {
    NotEnabled,
    Failed { cause: String, tail: Vec<u8> },
    Live(Box<Connected>),
}

/// The knob values one connect run honors, resolved up front.
struct Knobs {
    enabled: bool,
    max_tools: usize,
    startup: u64,
    tool: u64,
}

impl Knobs {
    fn resolve(ctx: &ToolCtx) -> Result<Self, McpError> {
        Ok(Self {
            enabled: knob_enabled(ctx.env_value(MCP_KNOB).as_deref())?,
            max_tools: max_tools_from(ctx.env_value(MAX_TOOLS_KNOB).as_deref())?,
            startup: timeout_secs_from(
                ctx.env_value(STARTUP_TIMEOUT_KNOB).as_deref(),
                "STARTUP",
                DEFAULT_STARTUP_TIMEOUT_SECS,
            )?,
            tool: timeout_secs_from(
                ctx.env_value(TOOL_TIMEOUT_KNOB).as_deref(),
                "TOOL",
                DEFAULT_TOOL_TIMEOUT_SECS,
            )?,
        })
    }
}

/// One spawned connect task's payload: its config index, its stderr
/// tail, and the spawn result.
type ConnectTask = (usize, StderrTail, Result<(Conn, Vec<Value>), String>);

/// The per-server connects run in parallel and collect-all: one bad
/// server never blocks its siblings. Every configured server gets one
/// status event; every connected server one discovery record; every
/// dropped tool one collision event.
pub async fn connect_all(
    ctx: &ToolCtx,
    host_home: Option<&Path>,
) -> Result<ConnectOutcome, McpError> {
    let knobs = Knobs::resolve(ctx)?;
    let workspace_root = ctx.workspace_root();
    let loaded = match load_config(workspace_root, workspace_root, host_home) {
        Ok(loaded) => loaded,
        Err(err) if !knobs.enabled => {
            return Ok(outcome_with_events(vec![McpEvent::note(format!(
                "{err}; ignored because {MCP_KNOB} is not 1"
            ))]));
        }
        Err(err) => return Err(err),
    };
    if !knobs.enabled {
        return Ok(disabled_outcome(&loaded));
    }
    if !loaded.project_present && !loaded.host_present {
        return Ok(outcome_with_events(vec![McpEvent::note(String::from(
            "no mcp.json discovered",
        ))]));
    }
    connect_enabled(ctx, &loaded, &knobs).await
}

fn outcome_with_events(events: Vec<McpEvent>) -> ConnectOutcome {
    ConnectOutcome {
        events,
        tools: Vec::new(),
        manager: Manager {
            servers: Vec::new(),
        },
    }
}

/// Every configured server reported disabled, nothing connected.
fn disabled_outcome(loaded: &Loaded) -> ConnectOutcome {
    let events = loaded
        .config
        .servers
        .iter()
        .map(|server| McpEvent::disabled(&server.name, format!("disabled: {MCP_KNOB} is not 1")))
        .collect();
    outcome_with_events(events)
}

async fn connect_enabled(
    ctx: &ToolCtx,
    loaded: &Loaded,
    knobs: &Knobs,
) -> Result<ConnectOutcome, McpError> {
    let configs = &loaded.config.servers;
    let (join, task_index) = spawn_connects(ctx, configs, knobs.startup);
    let outcomes = collect_outcomes(join, &task_index, configs.len()).await;
    let mut assembly = Assembly::new(knobs.max_tools, knobs.startup, knobs.tool);
    for (server, outcome) in configs.iter().zip(outcomes) {
        match outcome {
            ServerOutcome::NotEnabled => assembly.push_disabled_in_config(server),
            ServerOutcome::Failed { cause, tail } => assembly.push_failed(server, &cause, &tail),
            ServerOutcome::Live(live) => assembly.push_live(server, *live),
        }
    }
    assembly.warn_if_crowded();
    let (events, tools, servers) = assembly.finish();
    Ok(ConnectOutcome {
        events,
        tools,
        manager: Manager { servers },
    })
}

/// One enabled server per spawned task, with its config index
/// registered for join-error attribution.
fn spawn_connects(
    ctx: &ToolCtx,
    configs: &[ServerConfig],
    startup_knob: u64,
) -> (JoinSet<ConnectTask>, HashMap<Id, usize>) {
    let mut join = JoinSet::new();
    let mut task_index = HashMap::new();
    for (index, server) in configs.iter().enumerate() {
        if !server.enabled {
            continue;
        }
        let handle = join.spawn(connect_future(ctx, server, startup_knob, index));
        task_index.insert(handle.id(), index);
    }
    (join, task_index)
}

/// The owned inputs one connect task runs with, so the spawned future
/// borrows nothing from the caller.
fn connect_future(
    ctx: &ToolCtx,
    server: &ServerConfig,
    startup_knob: u64,
    index: usize,
) -> impl std::future::Future<Output = ConnectTask> + 'static {
    let config = server.clone();
    let child_env = state::child_env_with_overlay(ctx, server);
    let startup = Duration::from_secs(state::effective_timeout(
        server.startup_timeout_secs,
        startup_knob,
    ));
    async move {
        let tail: StderrTail = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let result =
            state::spawn_connected(&config, child_env, std::sync::Arc::clone(&tail), startup).await;
        (index, tail, result)
    }
}

/// Joins every connect task into its config's outcome slot.
async fn collect_outcomes(
    mut join: JoinSet<ConnectTask>,
    task_index: &HashMap<Id, usize>,
    count: usize,
) -> Vec<ServerOutcome> {
    let mut outcomes: Vec<ServerOutcome> = (0..count).map(|_| ServerOutcome::NotEnabled).collect();
    while let Some(joined) = join.join_next_with_id().await {
        match joined {
            Ok((_task_id, (index, tail, result))) => {
                outcomes[index] = outcome_of(tail, result);
            }
            Err(err) => mark_join_failed(&err, task_index, &mut outcomes),
        }
    }
    outcomes
}

fn outcome_of(tail: StderrTail, result: Result<(Conn, Vec<Value>), String>) -> ServerOutcome {
    let tail_text = tail.lock().map(|t| t.clone()).unwrap_or_default();
    match result {
        Ok((conn, defs)) => ServerOutcome::Live(Box::new(Connected { conn, defs, tail })),
        Err(cause) => ServerOutcome::Failed {
            cause,
            tail: tail_text,
        },
    }
}

fn mark_join_failed(
    err: &JoinError,
    task_index: &HashMap<Id, usize>,
    outcomes: &mut [ServerOutcome],
) {
    let Some(&index) = task_index.get(&err.id()) else {
        return;
    };
    outcomes[index] = join_failed(err);
}

fn join_failed(err: &JoinError) -> ServerOutcome {
    let cause = if err.is_panic() {
        format!("the connect task panicked: {err}")
    } else {
        format!("the connect task ended early: {err}")
    };
    ServerOutcome::Failed {
        cause,
        tail: Vec::new(),
    }
}
