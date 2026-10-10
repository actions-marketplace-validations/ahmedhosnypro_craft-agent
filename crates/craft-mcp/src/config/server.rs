//! One server entry's validation: the name rule, the stdio-only
//! shape, the fenced `cwd`, the env overlay, and the timeout bounds.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use craft_tools::envscrub::is_provider_key;

use super::ServerConfig;
use crate::TIMEOUT_SECS_MAX;
use crate::error::McpError;

const MAX_NAME_LEN: usize = 32;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EntryShape {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, Value>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    #[serde(rename = "enabledTools")]
    enabled_tools: Vec<String>,
    #[serde(default)]
    #[serde(rename = "disabledTools")]
    disabled_tools: Vec<String>,
    #[serde(default)]
    #[serde(rename = "startupTimeoutSecs")]
    startup_timeout_secs: Option<u64>,
    #[serde(default)]
    #[serde(rename = "toolTimeoutSecs")]
    tool_timeout_secs: Option<u64>,
    #[serde(default)]
    url: Option<Value>,
    #[serde(default)]
    r#type: Option<String>,
}

fn default_true() -> bool {
    true
}

fn valid_server_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'));
    rest_ok && name.len() <= MAX_NAME_LEN
}

/// Fold an absolute `cwd` lexically; it must land inside the root.
fn fold_within_root(root: &Path, input: &str) -> Result<PathBuf, ()> {
    let mut folded = PathBuf::from("/");
    for comp in Path::new(input).components() {
        match comp {
            Component::Normal(part) => folded.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if folded.starts_with(root) {
        Ok(folded)
    } else {
        Err(())
    }
}

pub(super) fn build_server(
    path: &Path,
    boundary_root: &Path,
    name: &str,
    entry: EntryShape,
) -> Result<ServerConfig, McpError> {
    if !valid_server_name(name) {
        return Err(rule_err(
            path,
            name,
            format!(
                "name must match ^[a-z][a-z0-9_-]{{0,{}}}$",
                MAX_NAME_LEN - 1
            ),
        ));
    }
    check_shape(path, name, &entry)?;
    let cwd = resolve_cwd(boundary_root, entry.cwd.as_deref())
        .map_err(|rule| rule_err(path, name, rule))?;
    let env = env_overlay(&entry.env).map_err(|rule| rule_err(path, name, rule))?;
    check_timeouts(&entry).map_err(|rule| rule_err(path, name, rule))?;
    Ok(ServerConfig {
        name: name.to_string(),
        command: entry.command,
        args: entry.args,
        env,
        cwd,
        enabled: entry.enabled,
        enabled_tools: entry.enabled_tools,
        disabled_tools: entry.disabled_tools,
        startup_timeout_secs: entry.startup_timeout_secs,
        tool_timeout_secs: entry.tool_timeout_secs,
    })
}

fn rule_err(path: &Path, name: &str, rule: String) -> McpError {
    McpError::Config {
        path: path.display().to_string(),
        rule: format!("server `{name}`: {rule}"),
    }
}

fn check_shape(path: &Path, name: &str, entry: &EntryShape) -> Result<(), McpError> {
    if entry.command.is_empty() {
        return Err(rule_err(
            path,
            name,
            String::from("command must not be empty"),
        ));
    }
    if entry.url.is_some() {
        return Err(rule_err(
            path,
            name,
            String::from(
                "url servers need the HTTP transport, which this version does not speak; configure a stdio command",
            ),
        ));
    }
    if let Some(kind) = entry.r#type.as_deref()
        && kind != "stdio"
    {
        return Err(rule_err(
            path,
            name,
            format!("type must be \"stdio\", got \"{kind}\""),
        ));
    }
    Ok(())
}

fn resolve_cwd(boundary_root: &Path, raw: Option<&str>) -> Result<Option<PathBuf>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if !raw.starts_with('/') {
        return Err(String::from(
            "cwd must be an absolute path inside the workspace",
        ));
    }
    fold_within_root(boundary_root, raw)
        .map(Some)
        .map_err(|()| format!("cwd `{raw}` escapes its layer's root"))
}

fn env_overlay(env: &BTreeMap<String, Value>) -> Result<Vec<(String, String)>, String> {
    let mut overlay = Vec::new();
    for (key, value) in env {
        if is_provider_key(key) {
            return Err(format!(
                "env key `{key}` is a provider key and never reaches servers"
            ));
        }
        let Some(value) = value.as_str() else {
            return Err(format!("env value for `{key}` must be a string literal"));
        };
        overlay.push((key.clone(), value.to_string()));
    }
    Ok(overlay)
}

fn check_timeouts(entry: &EntryShape) -> Result<(), String> {
    for (field, secs) in [
        ("startupTimeoutSecs", entry.startup_timeout_secs),
        ("toolTimeoutSecs", entry.tool_timeout_secs),
    ] {
        if let Some(secs) = secs
            && !(1..=TIMEOUT_SECS_MAX).contains(&secs)
        {
            return Err(format!("{field} must be in 1..={TIMEOUT_SECS_MAX}"));
        }
    }
    Ok(())
}
