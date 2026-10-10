//! Knob parsing and the crate's public surface constants. The trust
//! grant defaults off end to end; every knob value that cannot be
//! honored is a typed error naming the knob, never a silent fallback.

pub mod catalog;
pub mod child;
pub mod client;
pub mod config;
pub mod convert;
pub mod error;
pub mod events;
pub mod manager;
pub mod naming;
pub mod record;
pub mod state;
pub mod version;

mod register;

pub use catalog::{PER_SERVER_TOOLS, ToolEntry};
pub use config::{MCP_FILE, McpConfig, ServerConfig};
pub use error::McpError;
pub use events::McpEvent;
pub use manager::{ConnectOutcome, Manager, connect_all};
pub use naming::{is_mcp_tool_name, qualified_name};

/// The trust grant: `1` connects configured servers, `0` or unset or
/// empty connects nothing. Anything else is a typed error.
pub const MCP_KNOB: &str = "CRAFT_AGENT_MCP";
/// Total tool cap across all servers; 0 accepts nothing.
pub const MAX_TOOLS_KNOB: &str = "CRAFT_AGENT_MCP_MAX_TOOLS";
pub const DEFAULT_MAX_TOOLS: usize = 128;
/// The count above which discovery emits a warning event.
pub const TOOL_WARN_COUNT: usize = 64;
pub const STARTUP_TIMEOUT_KNOB: &str = "CRAFT_AGENT_MCP_STARTUP_TIMEOUT_SECS";
pub const TOOL_TIMEOUT_KNOB: &str = "CRAFT_AGENT_MCP_TOOL_TIMEOUT_SECS";
pub const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 30;
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 120;
/// Both timeout knobs accept this range; config fields share it.
pub const TIMEOUT_SECS_MAX: u64 = 86_400;

/// One env value into the trust grant.
pub fn knob_enabled(value: Option<&str>) -> Result<bool, McpError> {
    match value.map(str::trim) {
        None | Some("") | Some("0") => Ok(false),
        Some("1") => Ok(true),
        Some(raw) => Err(McpError::Knob {
            value: raw.to_string(),
        }),
    }
}

/// One env value into the total tool cap; unset or empty keeps the
/// default.
pub fn max_tools_from(value: Option<&str>) -> Result<usize, McpError> {
    match value.map(str::trim) {
        None | Some("") => Ok(DEFAULT_MAX_TOOLS),
        Some(raw) => raw.parse::<usize>().map_err(|_| McpError::MaxToolsKnob {
            value: raw.to_string(),
        }),
    }
}

/// One env value into a timeout; unset or empty keeps the default. A
/// per-server config field naming the same timeout wins over the knob.
pub fn timeout_secs_from(
    value: Option<&str>,
    knob: &'static str,
    default: u64,
) -> Result<u64, McpError> {
    match value.map(str::trim) {
        None | Some("") => Ok(default),
        Some(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|secs| (1..=TIMEOUT_SECS_MAX).contains(secs))
            .ok_or(McpError::TimeoutKnob {
                knob,
                value: raw.to_string(),
            }),
    }
}
