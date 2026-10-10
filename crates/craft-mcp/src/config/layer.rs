//! One config layer: the closed key set, the size-bounded read, and
//! the per-server parse into validated entries.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::server::{EntryShape, build_server};
use super::{FILE_BYTE_CAP, MAX_SERVERS, ServerConfig};
use crate::error::McpError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileShape {
    #[serde(rename = "mcpServers")]
    mcp_servers: BTreeMap<String, Value>,
}

fn read_bounded(path: &Path) -> Result<String, McpError> {
    use std::io::Read as _;
    let io_err = |rule: String| McpError::Config {
        path: path.display().to_string(),
        rule,
    };
    let file = std::fs::File::open(path)
        .map_err(|source| io_err(format!("cannot read the file: {source}")))?;
    let mut bytes = Vec::new();
    file.take(FILE_BYTE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_err(format!("cannot read the file: {source}")))?;
    if bytes.len() as u64 > FILE_BYTE_CAP {
        return Err(io_err(format!(
            "file is {} bytes (cap {FILE_BYTE_CAP})",
            bytes.len()
        )));
    }
    String::from_utf8(bytes).map_err(|_| io_err(String::from("file is not valid UTF-8")))
}

/// Reads and validates one config layer; every violation is one
/// typed config error, so a committed file that cannot be honored
/// fails the run instead of being silently dropped. `boundary_root`
/// fences absolute `cwd` values: the workspace for a project file,
/// the host home for the host file.
pub(super) fn parse_layer(
    path: &Path,
    boundary_root: &Path,
) -> Result<Vec<(String, ServerConfig)>, McpError> {
    let text = read_bounded(path)?;
    let value: Value = serde_json::from_str(&text).map_err(|source| McpError::Config {
        path: path.display().to_string(),
        rule: format!("not valid JSON: {source}"),
    })?;
    let file: FileShape = serde_json::from_value(value).map_err(|source| McpError::Config {
        path: path.display().to_string(),
        rule: source.to_string(),
    })?;
    if file.mcp_servers.len() > MAX_SERVERS {
        return Err(McpError::Config {
            path: path.display().to_string(),
            rule: format!(
                "the file configures {} servers (cap {MAX_SERVERS})",
                file.mcp_servers.len()
            ),
        });
    }
    let mut servers = Vec::new();
    for (name, entry) in &file.mcp_servers {
        let entry: EntryShape =
            serde_json::from_value(entry.clone()).map_err(|source| McpError::Config {
                path: path.display().to_string(),
                rule: format!("server `{name}`: {source}"),
            })?;
        servers.push((
            name.clone(),
            build_server(path, boundary_root, name, entry)?,
        ));
    }
    Ok(servers)
}
