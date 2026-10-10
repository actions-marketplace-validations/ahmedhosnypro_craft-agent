//! Event-shape corpus: the NDJSON pins. The two spec lines must
//! serialize exactly as written; nothing else may move.
// Corpus helpers: the loud-test idiom for panics.
#![allow(clippy::panic)]

use craft_mcp::events::{McpEvent, McpStatus};
use serde_json::json;

#[test]
fn the_status_line_matches_the_spec_shape() {
    let event = McpEvent::McpServerStatus {
        name: String::from("repo-stats"),
        status: McpStatus::Connected,
        transport: String::from("stdio"),
        tool_count: Some(3),
        error: None,
        note: None,
    };
    let line = serde_json::to_value(&event).expect("serializes");
    assert_eq!(
        line,
        json!({
            "type": "mcp_server_status",
            "name": "repo-stats",
            "status": "connected",
            "transport": "stdio",
            "tool_count": 3,
            "error": null,
            "note": null,
        })
    );
    let text = serde_json::to_string(&event).expect("serializes");
    assert!(text.contains("\"type\":\"mcp_server_status\""), "{text}");
}

#[test]
fn the_discovery_line_matches_the_spec_shape() {
    let event = McpEvent::McpToolsDiscovered {
        server: String::from("repo-stats"),
        hash: String::from("deadbeef"),
        tools: json!([{ "name": "echo" }]),
        enabled_names: vec![String::from("mcp__repo-stats__echo")],
    };
    let line = serde_json::to_value(&event).expect("serializes");
    assert_eq!(
        line,
        json!({
            "type": "mcp_tools_discovered",
            "server": "repo-stats",
            "hash": "deadbeef",
            "tools": [{ "name": "echo" }],
            "enabled_names": ["mcp__repo-stats__echo"],
        })
    );
}

#[test]
fn failed_and_disabled_statuses_round_trip() {
    let failed = McpEvent::failed("srv", String::from("handshake failed"));
    let text = serde_json::to_string(&failed).expect("serializes");
    assert!(text.contains("\"status\":\"failed\""), "{text}");
    let back: McpEvent = serde_json::from_str(&text).expect("round trips");
    assert_eq!(back, failed);

    let disabled = McpEvent::disabled("srv", String::from("disabled: CRAFT_AGENT_MCP is not 1"));
    let text = serde_json::to_string(&disabled).expect("serializes");
    assert!(text.contains("\"status\":\"disabled\""), "{text}");
    assert!(text.contains("CRAFT_AGENT_MCP"), "{text}");
}

#[test]
fn collision_and_note_events_carry_their_fields() {
    let collision = McpEvent::McpNameCollision {
        server: String::from("a__b"),
        tool: String::from("echo"),
        winner: Some(String::from("mcp__a_b__echo")),
        reason: String::from("another server already owns the qualified name"),
    };
    let line = serde_json::to_value(&collision).expect("serializes");
    assert_eq!(
        line,
        json!({
            "type": "mcp_name_collision",
            "server": "a__b",
            "tool": "echo",
            "winner": "mcp__a_b__echo",
            "reason": "another server already owns the qualified name",
        })
    );
    let note = McpEvent::note(String::from("no mcp.json discovered"));
    let text = serde_json::to_string(&note).expect("serializes");
    assert!(text.contains("\"type\":\"mcp_note\""), "{text}");
    assert!(text.contains("no mcp.json discovered"), "{text}");
}
