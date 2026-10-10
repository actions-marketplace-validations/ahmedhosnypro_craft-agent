//! `.craft-agent/mcp.json`: closest-wins discovery, a 64 KiB read
//! cap, a closed key set, and one actionable line per violation naming
//! the file, the server, and the rule. The optional host file merges
//! first; the project file wins per server name, wholesale.

mod layer;
mod server;

use std::collections::BTreeMap;
use std::path::Path;

use craft_core::agents::discover_file;

use crate::error::McpError;

pub const MCP_FILE: &str = ".craft-agent/mcp.json";
/// Hostile-input bound: one config file may not exceed this size.
pub const FILE_BYTE_CAP: u64 = 65_536;
/// The small-fixed-roster bound on configured servers.
pub const MAX_SERVERS: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<std::path::PathBuf>,
    pub enabled: bool,
    pub enabled_tools: Vec<String>,
    pub disabled_tools: Vec<String>,
    pub startup_timeout_secs: Option<u64>,
    pub tool_timeout_secs: Option<u64>,
}

/// The merged roster, sorted by server name.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct McpConfig {
    pub servers: Vec<ServerConfig>,
}

/// What discovery found, so the caller can tell "no file anywhere"
/// from "a file with zero servers".
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Loaded {
    pub config: McpConfig,
    pub host_present: bool,
    pub project_present: bool,
}

/// Loads the merged config: the optional host file first, then the
/// closest project file, which wins per server name wholesale. A
/// missing file everywhere is an empty config, not an error.
pub fn load_config(
    start: &Path,
    workspace_root: &Path,
    host_home: Option<&Path>,
) -> Result<Loaded, McpError> {
    let mut merged: BTreeMap<String, ServerConfig> = BTreeMap::new();
    let mut loaded = Loaded::default();
    if let Some(home) = host_home {
        load_host_layer(home, &mut merged, &mut loaded)?;
    }
    if let Some(project_file) = discover_file(start, MCP_FILE) {
        loaded.project_present = true;
        merge_layer(&project_file, workspace_root, &mut merged)?;
    }
    loaded.config = McpConfig {
        servers: merged.into_values().collect(),
    };
    Ok(loaded)
}

/// Merges the host file's layer when one exists.
fn load_host_layer(
    home: &Path,
    merged: &mut BTreeMap<String, ServerConfig>,
    loaded: &mut Loaded,
) -> Result<(), McpError> {
    let host_file = home.join(MCP_FILE);
    if !host_file.is_file() {
        return Ok(());
    }
    loaded.host_present = true;
    merge_layer(&host_file, home, merged)
}

/// One layer's servers into `merged`; later layers win per name.
fn merge_layer(
    path: &Path,
    boundary_root: &Path,
    merged: &mut BTreeMap<String, ServerConfig>,
) -> Result<(), McpError> {
    for (name, server) in layer::parse_layer(path, boundary_root)? {
        merged.insert(name, server);
    }
    Ok(())
}
