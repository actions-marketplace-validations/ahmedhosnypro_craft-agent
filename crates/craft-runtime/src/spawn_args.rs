//! Spawn-argument planning: hostile-input validation and resolution
//! for both tools, over the shared bounds in `craft_core::subagent`.
//! Numbers are bounded, prompts byte-capped, the template fill uses
//! the exact placeholder, and duplicate filled prompts are rejected
//! naming their 1-based indices. A validated request carries the
//! resolved profile, per-member prompts, turn cap, token slice, and
//! wall clock.

use craft_core::agents::AgentFile;
use craft_core::subagent::spec::{
    DEFAULT_MAX_TURNS, MAX_TURNS_CEILING, PROMPT_MAX_BYTES, SpawnBatch, SubagentError,
    TIMEOUT_SECS_CEILING,
};
use serde_json::Value;

use crate::subagent::{SubagentHub, SubagentWiring};

/// One validated spawn request, shared by both tools.
pub(crate) struct SpawnRequest {
    pub(crate) profile: AgentFile,
    pub(crate) members: Vec<String>,
    pub(crate) max_turns: u32,
    pub(crate) slice: u64,
    pub(crate) timeout_secs: Option<u64>,
    pub(crate) batch: bool,
}

/// A bounded, printable rendering of a hostile argument for error
/// messages.
fn preview(value: &Value) -> String {
    let text = value.to_string();
    let mut cut = text.chars().take(120).collect::<String>();
    if text.chars().count() > 120 {
        cut.push('…');
    }
    cut
}

fn expect_object(args: &Value) -> Result<&serde_json::Map<String, Value>, String> {
    match args {
        Value::Object(map) => Ok(map),
        other => Err(format!(
            "arguments must be a JSON object; got `{}`",
            preview(other)
        )),
    }
}

fn req_str(map: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    match map.get(key) {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text.clone()),
        Some(other) => Err(format!(
            "{key} must be a non-empty string; got `{}`",
            preview(other)
        )),
        None => Err(format!("{key} is required")),
    }
}

fn opt_u64(
    map: &serde_json::Map<String, Value>,
    key: &str,
    min: u64,
    max: u64,
) -> Result<Option<u64>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(value) if value >= min && value <= max => Ok(Some(value)),
            Some(value) => Err(format!(
                "{key} must be a whole number between {min} and {max}; got {value}"
            )),
            None => Err(format!(
                "{key} must be a whole number between {min} and {max}"
            )),
        },
        Some(other) => Err(format!(
            "{key} must be a whole number between {min} and {max}; got `{}`",
            preview(other)
        )),
    }
}

/// Resolves the profile's default turn cap: the spawn arg, else the
/// profile's `maxTurns`, else the step default — always under the
/// ceiling.
fn resolve_turns(arg: Option<u64>, profile: &AgentFile) -> u32 {
    arg.map_or_else(
        || {
            profile
                .max_turns
                .unwrap_or(DEFAULT_MAX_TURNS)
                .min(MAX_TURNS_CEILING)
        },
        |turns| u32::try_from(turns).unwrap_or(MAX_TURNS_CEILING),
    )
}

/// Resolves the per-member token slice: the spawn arg, else the
/// remaining run budget shared evenly by a batch.
fn resolve_slice(arg: Option<u64>, hub: &SubagentHub, count: u32) -> u64 {
    arg.unwrap_or_else(|| hub.default_slice(count))
}

/// Resolves the wall-clock cap: an explicit 0 disables it, an absent
/// arg inherits the profile's `timeoutSecs`.
fn resolve_timeout(arg: Option<u64>, profile: &AgentFile) -> Option<u64> {
    match arg {
        Some(0) => None,
        Some(secs) => Some(secs),
        None => profile.timeout_secs,
    }
}

/// Validates and resolves `spawn_agent`'s arguments against the
/// catalog and the hub's budget view.
pub(crate) fn plan_single(wiring: &SubagentWiring, args: &Value) -> Result<SpawnRequest, String> {
    let map = expect_object(args)?;
    let agent = req_str(map, "agent")?;
    let prompt = req_str(map, "prompt")?;
    if prompt.len() > PROMPT_MAX_BYTES {
        return Err(format!(
            "`prompt` must be 1..={} bytes, got {}",
            PROMPT_MAX_BYTES,
            prompt.len()
        ));
    }
    let max_turns = opt_u64(map, "maxTurns", 1, u64::from(MAX_TURNS_CEILING))?;
    let max_tokens = opt_u64(map, "maxTokens", 1, u64::MAX)?;
    let timeout_secs = opt_u64(map, "timeoutSecs", 0, TIMEOUT_SECS_CEILING)?;
    let profile = wiring.hub.profile(&agent)?;
    Ok(SpawnRequest {
        max_turns: resolve_turns(max_turns, &profile),
        slice: resolve_slice(max_tokens, &wiring.hub, 1),
        timeout_secs: resolve_timeout(timeout_secs, &profile),
        members: vec![prompt],
        profile,
        batch: false,
    })
}

/// Validates and resolves `spawn_agents`' arguments: the shared
/// `SpawnBatch::expand` owns the shape law (item bounds, the exact
/// placeholder, the duplicate scan naming every group member); this
/// layer reads the raw value fields and resolves the profile, slice,
/// and clock.
pub(crate) fn plan_batch(wiring: &SubagentWiring, args: &Value) -> Result<SpawnRequest, String> {
    let map = expect_object(args)?;
    let agent = req_str(map, "agent")?;
    let template = req_str(map, "promptTemplate")?;
    let items = expect_items(map)?;
    let batch = SpawnBatch {
        agent,
        prompt_template: template,
        items,
        max_turns: None,
        max_tokens: None,
        timeout_secs: None,
    };
    let specs = batch.expand().map_err(subagent_refusal)?;
    let max_turns = opt_u64(map, "maxTurns", 1, u64::from(MAX_TURNS_CEILING))?;
    let max_tokens = opt_u64(map, "maxTokens", 1, u64::MAX)?;
    let timeout_secs = opt_u64(map, "timeoutSecs", 0, TIMEOUT_SECS_CEILING)?;
    let profile = wiring.hub.profile(&batch.agent)?;
    let members: Vec<String> = specs.into_iter().map(|spec| spec.prompt).collect();
    Ok(SpawnRequest {
        max_turns: resolve_turns(max_turns, &profile),
        slice: resolve_slice(max_tokens, &wiring.hub, members.len() as u32),
        timeout_secs: resolve_timeout(timeout_secs, &profile),
        members,
        profile,
        batch: true,
    })
}

/// The shared batch refusal rendered for the tool caller. The agent
/// name is validated up front (non-empty string; the catalog lookup is
/// the authority on known names), and empty items are covered by the
/// filled-prompt bounds — `expand` can only surface the batch law.
fn subagent_refusal(error: SubagentError) -> String {
    match error {
        SubagentError::MissingPlaceholder => {
            String::from("`promptTemplate` must contain the {{item}} placeholder")
        }
        SubagentError::TemplateSize { bytes, max } => {
            format!("`promptTemplate` is {bytes} bytes, over {max}")
        }
        SubagentError::BatchItems { count, min, max } => {
            format!("`items` must be {min}..={max} entries, got {count}")
        }
        SubagentError::ItemSize { index, bytes, max } => {
            format!("`items` entry {index} is {bytes} bytes, over {max}")
        }
        SubagentError::BatchBytes { bytes, max } => {
            format!("`items` total {bytes} bytes, over {max}")
        }
        SubagentError::FilledPromptSize { index, bytes, max } => {
            if bytes == 0 {
                format!("the filled prompt for item {index} must be non-empty")
            } else {
                format!("the filled prompt for item {index} is {bytes} bytes, over {max}")
            }
        }
        SubagentError::DuplicatePrompts { indices } => {
            format!("duplicate filled prompts at items {indices:?}")
        }
        other => format!("spawn batch refused: {other}"),
    }
}

fn expect_items(map: &serde_json::Map<String, Value>) -> Result<Vec<String>, String> {
    let Some(items) = map.get("items") else {
        return Err(String::from("items is required"));
    };
    let Value::Array(list) = items else {
        return Err(format!(
            "items must be an array of strings; got `{}`",
            preview(items)
        ));
    };
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let Value::String(text) = item else {
            return Err(format!(
                "every item must be a string; got `{}`",
                preview(item)
            ));
        };
        out.push(text.clone());
    }
    Ok(out)
}
