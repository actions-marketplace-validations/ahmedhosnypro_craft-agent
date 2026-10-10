//! Catalog corpus: filters, caps, and collisions without a server —
//! every drop is a reject naming the reason and, for collisions, the
//! winner.
// Corpus helpers: the loud-test idiom for panics.
#![allow(clippy::panic)]

mod common;

use std::collections::BTreeSet;

use craft_mcp::catalog::{self, PER_SERVER_TOOLS, assemble_server, defs_have_object_schemas};
use craft_mcp::config::ServerConfig;
use serde_json::{Value, json};

fn server(name: &str, enabled: &[&str], disabled: &[&str]) -> ServerConfig {
    ServerConfig {
        name: name.to_string(),
        command: String::from("unused"),
        args: Vec::new(),
        env: Vec::new(),
        cwd: None,
        enabled: true,
        enabled_tools: enabled.iter().map(|t| t.to_string()).collect(),
        disabled_tools: disabled.iter().map(|t| t.to_string()).collect(),
        startup_timeout_secs: None,
        tool_timeout_secs: None,
    }
}

fn def(name: &str, schema: Value) -> Value {
    json!({ "name": name, "description": "a tool", "inputSchema": schema })
}

fn simple(name: &str) -> Value {
    def(name, json!({ "type": "object", "properties": {} }))
}

fn big_budget() -> usize {
    usize::MAX
}

#[test]
fn the_object_schema_rule_names_the_offender_and_fails_the_server() {
    let defs = vec![simple("ok"), def("broken", Value::Null)];
    let rule = defs_have_object_schemas(&defs).expect_err("a null schema fails the server");
    assert!(rule.contains("tool `broken`"), "{rule}");
    assert!(defs_have_object_schemas(&[simple("ok")]).is_ok());
    let missing = json!({ "name": "anon" });
    let rule = defs_have_object_schemas(&[missing]).expect_err("a missing schema fails");
    assert!(rule.contains("anon"), "{rule}");
}

#[test]
fn enabled_tools_win_over_disabled_tools() {
    let config = server("srv", &["keep"], &[]);
    let defs = vec![simple("keep"), simple("drop")];
    let (entries, rejects) =
        assemble_server(&config, &defs, &mut BTreeSet::new(), &mut big_budget());
    assert!(
        rejects.is_empty(),
        "filtered tools are intentional, not rejects"
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].raw_name, "keep");
    let config = server("srv", &[], &["drop"]);
    let (entries, _) = assemble_server(&config, &defs, &mut BTreeSet::new(), &mut big_budget());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].raw_name, "keep");
}

#[test]
fn duplicate_raw_names_drop_the_loser_with_the_winner_named() {
    let config = server("srv", &[], &[]);
    let defs = vec![simple("echo"), simple("echo")];
    let (entries, rejects) =
        assemble_server(&config, &defs, &mut BTreeSet::new(), &mut big_budget());
    assert_eq!(entries.len(), 1);
    assert_eq!(rejects.len(), 1);
    assert_eq!(rejects[0].winner.as_deref(), Some("mcp__srv__echo"));
    assert!(
        rejects[0].reason.contains("duplicate"),
        "{}",
        rejects[0].reason
    );
}

#[test]
fn cross_server_qualified_collisions_drop_the_loser() {
    // `a__b` sanitizes to `a_b`, so its tool collides with server a_b's.
    let mut owned = BTreeSet::new();
    let first = server("a_b", &[], &[]);
    let (entries, _) = assemble_server(&first, &[simple("echo")], &mut owned, &mut big_budget());
    assert_eq!(entries.len(), 1);
    let second = server("a__b", &[], &[]);
    let (entries, rejects) =
        assemble_server(&second, &[simple("echo")], &mut owned, &mut big_budget());
    assert!(entries.is_empty(), "the loser registers nothing");
    assert_eq!(rejects.len(), 1);
    assert_eq!(rejects[0].winner.as_deref(), Some("mcp__a_b__echo"));
}

#[test]
fn the_per_server_cap_drops_surplus_tools() {
    let config = server("srv", &[], &[]);
    let defs: Vec<Value> = (0..PER_SERVER_TOOLS + 6)
        .map(|index| simple(&format!("t{index}")))
        .collect();
    let (entries, rejects) =
        assemble_server(&config, &defs, &mut BTreeSet::new(), &mut big_budget());
    assert_eq!(entries.len(), PER_SERVER_TOOLS);
    assert_eq!(rejects.len(), 6);
    assert!(
        rejects
            .iter()
            .all(|r| r.reason.contains("per-server tool cap"))
    );
}

#[test]
fn long_names_and_unserializable_defs_are_rejected() {
    let config = server("srv", &[], &[]);
    let long = "n".repeat(catalog::MAX_NAME_CHARS + 1);
    let defs = vec![simple(&long), json!({ "inputSchema": {} })];
    let (entries, rejects) =
        assemble_server(&config, &defs, &mut BTreeSet::new(), &mut big_budget());
    assert!(entries.is_empty());
    assert_eq!(rejects.len(), 2);
    assert!(
        rejects[0].reason.contains("128 characters"),
        "{}",
        rejects[0].reason
    );
    assert!(
        rejects[1].reason.contains("names no tool"),
        "{}",
        rejects[1].reason
    );
}

#[test]
fn oversized_schemas_reject_the_tool_then_exhaust_the_budget() {
    let config = server("srv", &[], &[]);
    let big_schema = json!({
        "type": "object",
        "properties": { "blob": { "type": "string", "description": "x".repeat(70_000) } },
    });
    let over = def("over", big_schema.clone());
    let (entries, rejects) =
        assemble_server(&config, &[over], &mut BTreeSet::new(), &mut big_budget());
    assert!(entries.is_empty());
    assert!(
        rejects[0].reason.contains("64 KiB"),
        "{}",
        rejects[0].reason
    );

    let fitting = json!({ "type": "object", "properties": { "b": { "type": "string" } } });
    let defs: Vec<Value> = (0..5)
        .map(|i| def(&format!("t{i}"), fitting.clone()))
        .collect();
    let mut budget = 100;
    let (entries, rejects) = assemble_server(&config, &defs, &mut BTreeSet::new(), &mut budget);
    assert_eq!(
        entries.len(),
        1,
        "the first tool fits, the rest exhaust the budget"
    );
    assert_eq!(rejects.len(), 4);
    assert!(
        rejects.iter().all(|r| r.reason.contains("256 KiB")),
        "{rejects:?}"
    );
}

#[test]
fn composition_depth_bounds_reject_deep_nesting() {
    let config = server("srv", &[], &[]);
    let mut deep = json!({ "type": "object", "properties": { "leaf": { "type": "string" } } });
    for _ in 0..9 {
        deep = json!({ "allOf": [deep] });
    }
    let (entries, rejects) = assemble_server(
        &config,
        &[def("deep", deep)],
        &mut BTreeSet::new(),
        &mut big_budget(),
    );
    assert!(entries.is_empty(), "nine composition levels are rejected");
    assert!(
        rejects[0].reason.contains("composition keywords"),
        "{}",
        rejects[0].reason
    );

    let mut plain = json!({ "type": "object", "properties": { "leaf": { "type": "string" } } });
    for _ in 0..30 {
        plain = json!({ "type": "object", "properties": { "next": plain } });
    }
    let (entries, _) = assemble_server(
        &config,
        &[def("plain", plain)],
        &mut BTreeSet::new(),
        &mut big_budget(),
    );
    assert_eq!(entries.len(), 1, "plain property nesting does not count");

    let hostile: Value = json!({});
    let mut walker = hostile;
    for _ in 0..100 {
        walker = json!({ "allOf": [walker] });
    }
    let (entries, rejects) = assemble_server(
        &config,
        &[def("hostile", walker)],
        &mut BTreeSet::new(),
        &mut big_budget(),
    );
    assert!(entries.is_empty());
    assert!(
        rejects[0].reason.contains("depth-walk bound"),
        "{}",
        rejects[0].reason
    );
}
