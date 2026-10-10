//! The `tools`/`disallowedTools` field rules: a comma or space
//! separated list over the builtin ids plus the MCP tokens `mcp` and
//! `mcp:<server>`; brackets must balance and no id may be empty.

use std::path::Path;

use super::AgentFileError;
use super::TOOL_IDS;
use super::parse::{Parsed, entry};

/// TOOLS01: a comma or space separated list over the builtin ids;
/// brackets must balance and no id may be empty.
pub(crate) fn check_tools(
    path: &Path,
    parsed: &Parsed,
    field: &str,
) -> Result<Option<Vec<String>>, AgentFileError> {
    let Some(found) = entry(parsed, field) else {
        return Ok(None);
    };
    let raw = &found.value;
    let err = |message: String| AgentFileError::Tools {
        path: path.to_path_buf(),
        line: found.line,
        message,
    };
    let Some(inner) = bracket_inner(raw) else {
        return Err(err(format!("unbalanced bracket list in {field}")));
    };
    if empty_tool_id(inner) {
        return Err(err(format!("empty tool id in {field}")));
    }
    let tokens = split_tokens(inner);
    if tokens.is_empty() {
        return Err(err(format!("empty tool list in {field}")));
    }
    for token in &tokens {
        if !known_tool(token) {
            return Err(err(format!("unknown tool id '{token}' in {field}")));
        }
    }
    Ok(Some(tokens))
}

/// The list payload between balanced brackets, or the raw value when
/// unbracketed; None when the brackets do not balance.
fn bracket_inner(raw: &str) -> Option<&str> {
    let starts = raw.starts_with('[');
    let ends = raw.ends_with(']');
    if starts && ends {
        return Some(raw.get(1..raw.len().saturating_sub(1)).unwrap_or(""));
    }
    if starts || ends {
        return None;
    }
    Some(raw)
}

fn split_tokens(inner: &str) -> Vec<String> {
    inner
        .replace(',', " ")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// A builtin id, `mcp`, or an `mcp:<server>` token.
fn known_tool(token: &str) -> bool {
    TOOL_IDS.contains(&token) || is_mcp_token(token)
}

/// The run-time tool tokens for the MCP surface: `mcp` (every
/// discovered tool) and `mcp:<server>` (one configured server).
fn is_mcp_token(token: &str) -> bool {
    if token == "mcp" {
        return true;
    }
    let Some(server) = token.strip_prefix("mcp:") else {
        return false;
    };
    !server.is_empty()
        && server.len() <= 32
        && server
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
        && server
            .chars()
            .skip(1)
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

/// Leading, trailing, or doubled commas (with any spaces between).
fn empty_tool_id(inner: &str) -> bool {
    if inner.trim_start_matches(' ').starts_with(',') || inner.trim_end_matches(' ').ends_with(',')
    {
        return true;
    }
    let mut after_comma = false;
    for char in inner.chars() {
        if char == ',' {
            if after_comma {
                return true;
            }
            after_comma = true;
        } else if char != ' ' {
            after_comma = false;
        }
    }
    false
}
