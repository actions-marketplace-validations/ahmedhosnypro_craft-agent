//! The additive MCP events: one server status before the first turn,
//! one discovery record per connected server, name-collision drops,
//! and operator notes. Field shapes are what the run bridge forwards
//! onto the NDJSON stream.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStatus {
    Connected,
    Failed,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpEvent {
    /// `{"type":"mcp_server_status","name":...,"status":...,
    /// "transport":"stdio","tool_count":...,"error":...,"note":...}`
    McpServerStatus {
        name: String,
        status: McpStatus,
        transport: String,
        tool_count: Option<u32>,
        error: Option<String>,
        note: Option<String>,
    },
    /// `{"type":"mcp_tools_discovered","server":...,"hash":...,
    /// "tools":[...],"enabled_names":[...]}`
    McpToolsDiscovered {
        server: String,
        hash: String,
        tools: Value,
        enabled_names: Vec<String>,
    },
    /// A tool that will not register: the loser of a name collision or
    /// a cap-bound reject. `winner` names the qualified name holding
    /// the slot, when there is one.
    McpNameCollision {
        server: String,
        tool: String,
        winner: Option<String>,
        reason: String,
    },
    /// Operator-facing notes: no file discovered, knob-off parse
    /// failures, the over-64 tool warning.
    McpNote { note: String },
}

impl McpEvent {
    /// A failed server: the error string, no operator note.
    pub fn failed(name: &str, error: String) -> Self {
        McpEvent::McpServerStatus {
            name: name.to_string(),
            status: McpStatus::Failed,
            transport: String::from("stdio"),
            tool_count: None,
            error: Some(error),
            note: None,
        }
    }

    /// A disabled server: the operator note, no error.
    pub fn disabled(name: &str, note: String) -> Self {
        McpEvent::McpServerStatus {
            name: name.to_string(),
            status: McpStatus::Disabled,
            transport: String::from("stdio"),
            tool_count: None,
            error: None,
            note: Some(note),
        }
    }

    /// A connected server and its registered tool count.
    pub fn connected(name: &str, tool_count: u32) -> Self {
        McpEvent::McpServerStatus {
            name: name.to_string(),
            status: McpStatus::Connected,
            transport: String::from("stdio"),
            tool_count: Some(tool_count),
            error: None,
            note: None,
        }
    }

    pub fn note(note: String) -> Self {
        McpEvent::McpNote { note }
    }
}
