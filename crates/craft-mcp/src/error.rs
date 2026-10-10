//! The crate's closed error taxonomy. Config and knob violations fail
//! the run closed; per-server connect failures never do — they ride
//! status events and the run proceeds.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum McpError {
    /// A config file that cannot be honored: one actionable line naming
    /// the file and the rule.
    #[error("mcp.json invalid: {path}: {rule}")]
    Config { path: String, rule: String },
    /// The trust grant is 1 or 0 only; a typo'd grant never silently
    /// enables or disables servers.
    #[error("CRAFT_AGENT_MCP must be 1 or 0; got `{value}`")]
    Knob { value: String },
    #[error("CRAFT_AGENT_MCP_MAX_TOOLS must be a whole number of tools; got `{value}`")]
    MaxToolsKnob { value: String },
    #[error(
        "CRAFT_AGENT_MCP_{knob}_TIMEOUT_SECS must be a whole number of seconds in 1..=86400; got `{value}`"
    )]
    TimeoutKnob { knob: &'static str, value: String },
}
