//! Catalog assembly: enabledTools filters, name qualification,
//! per-tool schema bounds, and the caps. Every drop is a visible
//! reject with a reason — never a silent loss.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::config::ServerConfig;
use crate::convert;
use crate::naming::qualified_name;

/// Tools accepted from one server.
pub const PER_SERVER_TOOLS: usize = 64;
/// The longest raw tool name accepted.
pub const MAX_NAME_CHARS: usize = 128;
/// `tools/list` pages followed per server.
pub const LIST_PAGES: usize = 16;

/// One accepted tool: its registered identity plus the raw definition
/// for the discovery record.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolEntry {
    pub qualified: String,
    pub raw_name: String,
    pub description: String,
    pub schema: Value,
    pub def: Value,
}

/// A tool that will not register, with the reason and — for real
/// collisions — the qualified name that holds the slot.
#[derive(Debug, Clone, PartialEq)]
pub struct Reject {
    pub server: String,
    pub tool: String,
    pub reason: String,
    pub winner: Option<String>,
}

fn reject(server: &str, tool: &str, reason: String, winner: Option<String>) -> Reject {
    Reject {
        server: server.to_string(),
        tool: tool.to_string(),
        reason,
        winner,
    }
}

/// True when no accepted def violates the object-schema rule; the
/// error names the first offender. A violation fails the whole
/// server, so it is checked before assembly.
pub fn defs_have_object_schemas(defs: &[Value]) -> Result<(), String> {
    for def in defs {
        let Some(name) = def.get("name").and_then(Value::as_str) else {
            return Err(String::from("a tool definition named no name"));
        };
        let schema = def.get("inputSchema").unwrap_or(&Value::Null);
        if !convert::schema_is_object(schema) {
            return Err(format!("tool `{name}` has a non-object inputSchema"));
        }
    }
    Ok(())
}

/// Why a definition did not become an entry: a visible reject, or a
/// silent drop from the enabled/disabled filters.
enum Step {
    Skip,
    Reject(Reject),
}

/// Per-server mutable state threaded through the accept pipeline.
struct Acceptor<'a> {
    server: &'a ServerConfig,
    allow: Option<&'a [String]>,
    seen_raw: BTreeSet<String>,
    owned: &'a mut BTreeSet<String>,
    schema_budget: &'a mut usize,
    entry_count: usize,
}

/// Assembles one server's accepted tools from its raw definitions.
/// `owned` grows with every accepted qualified name so cross-server
/// collisions fail closed; `schema_budget` bounds the server's
/// accepted schemas at [`convert::SCHEMA_SERVER_CAP`].
pub fn assemble_server(
    server: &ServerConfig,
    defs: &[Value],
    owned: &mut BTreeSet<String>,
    schema_budget: &mut usize,
) -> (Vec<ToolEntry>, Vec<Reject>) {
    let mut acceptor = Acceptor {
        server,
        allow: (!server.enabled_tools.is_empty()).then_some(server.enabled_tools.as_slice()),
        seen_raw: BTreeSet::new(),
        owned,
        schema_budget,
        entry_count: 0,
    };
    let mut entries = Vec::new();
    let mut rejects = Vec::new();
    for def in defs {
        match accept_def(&mut acceptor, def) {
            Ok(entry) => entries.push(entry),
            Err(Step::Skip) => {}
            Err(Step::Reject(reject)) => rejects.push(reject),
        }
    }
    (entries, rejects)
}

fn accept_def(acceptor: &mut Acceptor<'_>, def: &Value) -> Result<ToolEntry, Step> {
    let raw_name = raw_name_of(acceptor.server, def).map_err(Step::Reject)?;
    if is_filtered(acceptor.server, acceptor.allow, raw_name) {
        return Err(Step::Skip);
    }
    let qualified = qualified_name(&acceptor.server.name, raw_name);
    reserve_seen(acceptor, raw_name, &qualified).map_err(Step::Reject)?;
    ensure_server_cap(acceptor, raw_name).map_err(Step::Reject)?;
    let (schema, size) = bounded_schema(acceptor, raw_name, def).map_err(Step::Reject)?;
    ensure_depth(acceptor, raw_name, &schema).map_err(Step::Reject)?;
    ensure_owned(acceptor, raw_name, &qualified).map_err(Step::Reject)?;
    let description = cap_description_of(def);
    *acceptor.schema_budget = acceptor.schema_budget.saturating_sub(size);
    acceptor.owned.insert(qualified.clone());
    acceptor.entry_count += 1;
    let def = def_with_description(def, &description);
    Ok(ToolEntry {
        qualified,
        raw_name: raw_name.to_string(),
        description,
        schema,
        def,
    })
}

fn raw_name_of<'a>(server: &ServerConfig, def: &'a Value) -> Result<&'a str, Reject> {
    let raw_name = match def.get("name").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => name,
        _ => {
            return Err(reject(
                &server.name,
                "",
                String::from("the tool definition names no tool"),
                None,
            ));
        }
    };
    if raw_name.len() > MAX_NAME_CHARS {
        return Err(reject(
            &server.name,
            raw_name,
            format!("the tool name exceeds {MAX_NAME_CHARS} characters"),
            None,
        ));
    }
    Ok(raw_name)
}

fn is_filtered(server: &ServerConfig, allow: Option<&[String]>, raw_name: &str) -> bool {
    allow.is_some_and(|list| !list.iter().any(|tool| tool == raw_name))
        || server.disabled_tools.iter().any(|tool| tool == raw_name)
}

fn reserve_seen(
    acceptor: &mut Acceptor<'_>,
    raw_name: &str,
    qualified: &str,
) -> Result<(), Reject> {
    if acceptor.seen_raw.insert(raw_name.to_string()) {
        return Ok(());
    }
    Err(reject(
        &acceptor.server.name,
        raw_name,
        String::from("a duplicate tool name within the server"),
        Some(qualified.to_string()),
    ))
}

fn ensure_server_cap(acceptor: &Acceptor<'_>, raw_name: &str) -> Result<(), Reject> {
    if acceptor.entry_count < PER_SERVER_TOOLS {
        return Ok(());
    }
    Err(reject(
        &acceptor.server.name,
        raw_name,
        format!("the per-server tool cap ({PER_SERVER_TOOLS}) is reached"),
        None,
    ))
}

fn bounded_schema(
    acceptor: &Acceptor<'_>,
    raw_name: &str,
    def: &Value,
) -> Result<(Value, usize), Reject> {
    let schema = def.get("inputSchema").cloned().unwrap_or(Value::Null);
    let Some(size) = convert::schema_bytes(&schema) else {
        return Err(reject(
            &acceptor.server.name,
            raw_name,
            String::from("the inputSchema cannot be serialized"),
            None,
        ));
    };
    if size > convert::SCHEMA_TOOL_CAP {
        return Err(reject(
            &acceptor.server.name,
            raw_name,
            format!("the inputSchema is {size} bytes (cap 64 KiB)"),
            None,
        ));
    }
    if size > *acceptor.schema_budget {
        return Err(reject(
            &acceptor.server.name,
            raw_name,
            String::from("the server's schemas exceed the 256 KiB cap"),
            None,
        ));
    }
    Ok((schema, size))
}

fn ensure_depth(acceptor: &Acceptor<'_>, raw_name: &str, schema: &Value) -> Result<(), Reject> {
    let reject_depth = |reason: String| reject(&acceptor.server.name, raw_name, reason, None);
    match convert::composition_depth(schema) {
        Some(depth) if depth <= convert::MAX_SCHEMA_DEPTH => Ok(()),
        Some(_) => Err(reject_depth(format!(
            "the inputSchema nests composition keywords past {} levels",
            convert::MAX_SCHEMA_DEPTH
        ))),
        None => Err(reject_depth(String::from(
            "the inputSchema nests past the depth-walk bound",
        ))),
    }
}

fn ensure_owned(acceptor: &Acceptor<'_>, raw_name: &str, qualified: &str) -> Result<(), Reject> {
    match acceptor.owned.get(qualified) {
        None => Ok(()),
        Some(winner) => Err(reject(
            &acceptor.server.name,
            raw_name,
            String::from("another server already owns the qualified name"),
            Some(winner.clone()),
        )),
    }
}

fn cap_description_of(def: &Value) -> String {
    let description = def.get("description").and_then(Value::as_str).unwrap_or("");
    convert::cap_description(description)
}

fn def_with_description(def: &Value, description: &str) -> Value {
    match def {
        Value::Object(map) => {
            let mut map = map.clone();
            map.insert(
                String::from("description"),
                Value::String(description.to_string()),
            );
            Value::Object(map)
        }
        other => other.clone(),
    }
}
