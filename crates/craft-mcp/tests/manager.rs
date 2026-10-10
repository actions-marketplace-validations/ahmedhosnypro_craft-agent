//! Manager corpus, part one: the healthy path and the trust gate.
//! Status and discovery events per server, pagination, and the knob
//! rows. Failure and crash-law rows live in manager_failures.rs.

mod common;

use common::{
    connect, fresh_workspace, mcp_on, one_server_workspace, statuses, three_server_workspace,
    write_mcp_json,
};
use craft_mcp::events::{McpEvent, McpStatus};
use serde_json::json;

#[test]
fn the_discovery_record_def_carries_the_capped_description() {
    let server = craft_mcp::ServerConfig {
        name: String::from("srv"),
        command: String::from("x"),
        args: Vec::new(),
        env: Vec::new(),
        cwd: None,
        enabled: true,
        enabled_tools: Vec::new(),
        disabled_tools: Vec::new(),
        startup_timeout_secs: None,
        tool_timeout_secs: None,
    };
    let defs = vec![json!({
        "name": "echo",
        "description": "d".repeat(64 * 1024),
        "inputSchema": { "type": "object" },
    })];
    let mut owned = std::collections::BTreeSet::new();
    let mut budget = craft_mcp::convert::SCHEMA_SERVER_CAP;
    let (entries, rejects) =
        craft_mcp::catalog::assemble_server(&server, &defs, &mut owned, &mut budget);
    assert!(rejects.is_empty(), "{rejects:?}");
    let description = entries[0]
        .def
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        description.len() <= 2048,
        "the record's def is capped: {}",
        description.len()
    );
    assert!(description.ends_with("[output truncated]"), "{description}");
}

#[tokio::test]
async fn the_default_fixture_round_trips_two_paginated_tools() {
    let workspace = one_server_workspace(&[]);
    let outcome = connect(&workspace, &mcp_on())
        .await
        .expect("a valid config connects");
    assert_eq!(outcome.tools.len(), 2);
    let names: Vec<&str> = outcome.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        ["mcp__srv__echo", "mcp__srv__report"],
        "pagination fetched page 2"
    );
    let McpEvent::McpServerStatus {
        status: McpStatus::Connected,
        tool_count: Some(2),
        ..
    } = statuses(&outcome)[0]
    else {
        panic!(
            "the server connects with both tools: {:?}",
            statuses(&outcome)
        );
    };
    let discovered: Vec<_> = outcome
        .events
        .iter()
        .filter(|event| matches!(event, McpEvent::McpToolsDiscovered { .. }))
        .collect();
    assert_eq!(discovered.len(), 1, "one discovery record");
    match &discovered[0] {
        McpEvent::McpToolsDiscovered {
            hash,
            enabled_names,
            ..
        } => {
            assert_eq!(hash.len(), 64, "the record carries a sha256 hex hash");
            assert_eq!(
                enabled_names,
                &[
                    String::from("mcp__srv__echo"),
                    String::from("mcp__srv__report")
                ],
                "the enabled names are the sorted qualified names"
            );
        }
        _ => panic!("the discovery record has the wrong shape"),
    }
    let echoed = common::call_tool(&outcome, "mcp__srv__echo", json!({ "msg": "pong" }))
        .await
        .expect("the echo tool round-trips");
    assert!(echoed.contains("echo: {\"msg\":\"pong\"}"), "{echoed}");
    outcome.manager.shutdown().await;
}

#[tokio::test]
async fn the_knob_off_run_spawns_nothing_and_reports_disabled() {
    let workspace = one_server_workspace(&[]);
    let outcome = connect(&workspace, &[])
        .await
        .expect("knob off is not an error");
    assert!(outcome.tools.is_empty());
    let statuses = statuses(&outcome);
    assert_eq!(statuses.len(), 1);
    let McpEvent::McpServerStatus {
        status: McpStatus::Disabled,
        note: Some(note),
        ..
    } = statuses[0]
    else {
        panic!("the server reports disabled: {statuses:?}");
    };
    assert!(
        note.contains("CRAFT_AGENT_MCP"),
        "the note names the knob: {note}"
    );
}

#[tokio::test]
async fn knob_off_makes_an_invalid_config_inert() {
    let workspace = fresh_workspace("manager");
    write_mcp_json(&workspace, "{ not json");
    let outcome = connect(&workspace, &[])
        .await
        .expect("knob off never fails the run");
    assert!(outcome.tools.is_empty());
    let notes: Vec<_> = outcome
        .events
        .iter()
        .filter_map(|event| match event {
            McpEvent::McpNote { note } => Some(note),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("not valid JSON"), "{notes:?}");
    assert!(notes[0].contains("ignored"), "{notes:?}");
}

#[tokio::test]
async fn knob_on_fails_closed_on_an_invalid_config() {
    let workspace = fresh_workspace("manager");
    write_mcp_json(&workspace, "{\"mcpServers\": {\"s\": {\"command\": \"\"}}}");
    match connect(&workspace, &mcp_on()).await {
        Ok(_) => panic!("an invalid config must fail the run closed"),
        Err(err) => assert!(
            err.to_string().contains("command must not be empty"),
            "{err}"
        ),
    }
}

#[tokio::test]
async fn no_file_and_knob_on_is_a_note_and_zero_overhead() {
    let workspace = fresh_workspace("manager");
    let outcome = connect(&workspace, &mcp_on())
        .await
        .expect("no file is not an error");
    assert!(outcome.tools.is_empty());
    assert_eq!(outcome.events.len(), 1);
    assert!(
        matches!(
            &outcome.events[0],
            McpEvent::McpNote { note } if note.contains("no mcp.json discovered")
        ),
        "{:?}",
        outcome.events
    );
}

#[tokio::test]
async fn caps_and_warnings_apply_across_servers() {
    let workspace = three_server_workspace("\"--many-tools\", \"64\"");
    let outcome = connect(&workspace, &mcp_on()).await.expect("connects");
    let names: Vec<&str> = outcome.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names.len(),
        128,
        "the 128 default total cap holds: {names:?}"
    );
    assert!(
        names
            .iter()
            .all(|name| name.starts_with("mcp__alpha__") || name.starts_with("mcp__beta__"))
    );
    let collisions = outcome
        .events
        .iter()
        .filter(|event| matches!(event, McpEvent::McpNameCollision { .. }))
        .count();
    assert_eq!(
        collisions, 64,
        "the surplus drops with collision-style events"
    );
    let notes: Vec<_> = outcome
        .events
        .iter()
        .filter_map(|event| match event {
            McpEvent::McpNote { note } => Some(note),
            _ => None,
        })
        .collect();
    assert!(
        notes
            .iter()
            .any(|note| note.contains("above the 64 comfort threshold")),
        "the >64 warning fires: {notes:?}"
    );
    let statuses = statuses(&outcome);
    assert_eq!(statuses.len(), 4);
    let disabled = statuses
        .iter()
        .rev()
        .find(|event| {
            matches!(
                event,
                McpEvent::McpServerStatus {
                    status: McpStatus::Disabled,
                    ..
                }
            )
        })
        .expect("the enabled:false server reports disabled");
    let note = match disabled {
        McpEvent::McpServerStatus { note, .. } => note.clone(),
        _ => None,
    }
    .unwrap_or_default();
    assert!(note.contains("enabled: false"), "{note}");
    outcome.manager.shutdown().await;
}
