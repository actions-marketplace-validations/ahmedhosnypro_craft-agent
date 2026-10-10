//! Live-server registration: catalog assembly, the discovery record,
//! the total tool cap, and the DynamicTool closures that carry the
//! reconnect-once ladder.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use futures::stream::{self, StreamExt as _};
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use craft_tools::event::{ToolEvent, ToolResult, ToolStream};
use craft_tools::{DynamicTool, ToolError};

use crate::TOOL_WARN_COUNT;
use crate::catalog::{self, ToolEntry};
use crate::config::ServerConfig;
use crate::convert::{self, Converted};
use crate::events::McpEvent;
use crate::record::discovery_hash;
use crate::state::{self, ServerState};

pub(crate) struct Connected {
    pub(crate) conn: crate::client::Conn,
    pub(crate) defs: Vec<Value>,
    pub(crate) tail: crate::client::StderrTail,
}

fn failure_note(server: &ServerConfig, cause: &str, tail: &[u8]) -> String {
    let tail = String::from_utf8_lossy(tail);
    if tail.is_empty() {
        format!("server `{}`: {cause}", server.name)
    } else {
        format!("server `{}`: {cause}; stderr tail: {tail}", server.name)
    }
}

/// Builds one registered tool around its server state: the call
/// ladder runs inside the stream future, on the runtime that polls it.
fn build_tool(state: Arc<ServerState>, entry: ToolEntry) -> DynamicTool {
    let raw_name = entry.raw_name;
    let qualified = entry.qualified.clone();
    let server = state.config.name.clone();
    DynamicTool {
        name: entry.qualified,
        description: entry.description,
        schema: entry.schema,
        execute: Arc::new(move |args, ctx| {
            let state = Arc::clone(&state);
            let raw_name = raw_name.clone();
            let qualified = qualified.clone();
            let server = server.clone();
            let child_env = state::child_env_with_overlay(ctx, &state.config);
            let call = async move {
                let result =
                    state::raw_call(state.as_ref(), child_env, &raw_name, &qualified, args).await;
                match result {
                    Ok(result) => match convert::convert_result(&result) {
                        Ok(Converted::Ok(text)) => {
                            Ok(ToolEvent::Terminal(ToolResult { output: text }))
                        }
                        Ok(Converted::IsError(text)) => Err(ToolError::McpToolIsError {
                            message: format!("mcp server `{server}` reported an error: {text}"),
                        }),
                        Err(detail) => Err(ToolError::McpMalformedResult { server, detail }),
                    },
                    Err(err) => Err(err),
                }
            };
            stream::once(call).boxed() as ToolStream
        }),
    }
}

/// Accumulates the run's MCP surface: events in config order, one
/// registered tool per accepted entry, one live state per server.
pub(crate) struct Assembly {
    events: Vec<McpEvent>,
    tools: Vec<DynamicTool>,
    servers: Vec<Arc<ServerState>>,
    owned: BTreeSet<String>,
    total: usize,
    max_tools: usize,
    startup_knob: u64,
    tool_knob: u64,
}

impl Assembly {
    pub(crate) fn new(max_tools: usize, startup_knob: u64, tool_knob: u64) -> Self {
        Self {
            events: Vec::new(),
            tools: Vec::new(),
            servers: Vec::new(),
            owned: BTreeSet::new(),
            total: 0,
            max_tools,
            startup_knob,
            tool_knob,
        }
    }

    pub(crate) fn push_disabled_in_config(&mut self, server: &ServerConfig) {
        self.events.push(McpEvent::disabled(
            &server.name,
            String::from("disabled by `enabled: false` in mcp.json"),
        ));
    }

    pub(crate) fn push_failed(&mut self, server: &ServerConfig, cause: &str, tail: &[u8]) {
        self.events.push(McpEvent::failed(
            &server.name,
            failure_note(server, cause, tail),
        ));
    }

    pub(crate) fn warn_if_crowded(&mut self) {
        if self.total > TOOL_WARN_COUNT {
            self.events.push(McpEvent::note(format!(
                "mcp tool definitions total {} across all servers, above the {TOOL_WARN_COUNT} comfort threshold",
                self.total
            )));
        }
    }

    pub(crate) fn finish(self) -> (Vec<McpEvent>, Vec<DynamicTool>, Vec<Arc<ServerState>>) {
        (self.events, self.tools, self.servers)
    }
}

impl Assembly {
    /// One connected server: the object-schema rule, catalog assembly
    /// with filters and caps, the collision events, the discovery
    /// record, and the live state the tools call through.
    pub(crate) fn push_live(&mut self, server: &ServerConfig, live: Connected) {
        let (state, defs) = server_state(server, live, self.startup_knob, self.tool_knob);
        if let Some(rule) = catalog::defs_have_object_schemas(&defs).err() {
            self.push_schema_rejected(server, state, rule);
            return;
        }
        let mut budget = convert::SCHEMA_SERVER_CAP;
        let (entries, rejects) =
            catalog::assemble_server(server, &defs, &mut self.owned, &mut budget);
        let kept = self.kept_within_total(server, entries);
        self.push_collision_events(server, &rejects);
        self.push_discovery(server, &kept, &rejects);
        for entry in kept {
            self.tools.push(build_tool(Arc::clone(&state), entry));
        }
        self.servers.push(state);
    }

    /// A whole-server schema violation: the state still shuts down with
    /// the run, and the event carries the rule.
    fn push_schema_rejected(
        &mut self,
        server: &ServerConfig,
        state: Arc<ServerState>,
        rule: String,
    ) {
        self.servers.push(state);
        self.events.push(McpEvent::failed(
            &server.name,
            failure_note(server, &rule, &[]),
        ));
    }

    /// The entries kept under the run-wide tool cap; the rest get
    /// collision events.
    fn kept_within_total(
        &mut self,
        server: &ServerConfig,
        entries: Vec<ToolEntry>,
    ) -> Vec<ToolEntry> {
        let mut kept = Vec::new();
        for entry in entries {
            if self.total >= self.max_tools {
                self.events.push(collision_event(
                    &server.name,
                    &entry.raw_name,
                    None,
                    format!("the total tool cap ({}) is reached", self.max_tools),
                ));
                continue;
            }
            self.total += 1;
            kept.push(entry);
        }
        kept
    }

    fn push_collision_events(&mut self, server: &ServerConfig, rejects: &[catalog::Reject]) {
        for reject in rejects {
            self.events.push(collision_event(
                &server.name,
                &reject.tool,
                reject.winner.as_deref(),
                reject.reason.clone(),
            ));
        }
    }

    /// The connected status plus the discovery record for one server.
    fn push_discovery(
        &mut self,
        server: &ServerConfig,
        kept: &[ToolEntry],
        rejects: &[catalog::Reject],
    ) {
        let defs = Value::Array(kept.iter().map(|entry| entry.def.clone()).collect());
        let names: BTreeSet<String> = kept.iter().map(|entry| entry.qualified.clone()).collect();
        let collisions = rejects_value(rejects);
        let hash = discovery_hash(&defs, &names, &collisions);
        self.events
            .push(McpEvent::connected(&server.name, kept.len() as u32));
        self.events.push(McpEvent::McpToolsDiscovered {
            server: server.name.clone(),
            hash,
            tools: defs,
            enabled_names: names.into_iter().collect(),
        });
    }
}

/// The live state one server's tools call through, plus its defs.
fn server_state(
    server: &ServerConfig,
    live: Connected,
    startup_knob: u64,
    tool_knob: u64,
) -> (Arc<ServerState>, Vec<Value>) {
    let Connected { conn, defs, tail } = live;
    let startup = Duration::from_secs(state::effective_timeout(
        server.startup_timeout_secs,
        startup_knob,
    ));
    let tool = Duration::from_secs(state::effective_timeout(
        server.tool_timeout_secs,
        tool_knob,
    ));
    let state = Arc::new(ServerState {
        config: server.clone(),
        stderr_tail: tail,
        conn: AsyncMutex::new(Some(conn)),
        generation: AtomicU64::new(0),
        startup_timeout: startup,
        tool_timeout: tool,
    });
    (state, defs)
}

fn collision_event(server: &str, tool: &str, winner: Option<&str>, reason: String) -> McpEvent {
    McpEvent::McpNameCollision {
        server: server.to_string(),
        tool: tool.to_string(),
        winner: winner.map(str::to_string),
        reason,
    }
}

fn rejects_value(rejects: &[catalog::Reject]) -> Value {
    Value::Array(
        rejects
            .iter()
            .map(|reject| {
                serde_json::json!({
                    "server": reject.server,
                    "tool": reject.tool,
                    "winner": reject.winner,
                    "reason": reject.reason,
                })
            })
            .collect(),
    )
}
