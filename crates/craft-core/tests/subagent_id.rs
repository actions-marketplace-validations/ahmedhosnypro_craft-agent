//! Subagent id law: 1-based allocation order, the exact `agent-N`
//! rendering, and the wire shape — plus the fixed firewall overlay
//! every child receives.

use craft_core::subagent::{SUBAGENT_OVERLAY, SubagentId};

#[test]
fn ids_render_and_advance_in_allocation_order() {
    assert_eq!(SubagentId::FIRST.to_string(), "agent-1");
    assert_eq!(SubagentId::FIRST.index(), 1);
    assert_eq!(SubagentId::FIRST.next().to_string(), "agent-2");
    assert_eq!(SubagentId::FIRST.next().next().index(), 3);
}

#[test]
fn ids_parse_only_the_agent_n_shape() {
    assert_eq!(SubagentId::parse("agent-7").map(SubagentId::index), Some(7));
    assert_eq!(SubagentId::parse("agent-0"), None);
    assert_eq!(SubagentId::parse("agent-x"), None);
    assert_eq!(SubagentId::parse("agent--1"), None);
    assert_eq!(SubagentId::parse("agent-4294967296"), None);
    assert_eq!(SubagentId::parse("agent-1x"), None);
    assert_eq!(SubagentId::parse("7"), None);
    assert_eq!(SubagentId::parse(""), None);
}

#[test]
fn ids_serialize_as_the_agent_n_string() {
    let id = SubagentId::parse("agent-3").expect("valid id");
    let wire = serde_json::to_string(&id).expect("serializes");
    assert_eq!(wire, "\"agent-3\"");
    let back: SubagentId = serde_json::from_str("\"agent-3\"").expect("round trip");
    assert_eq!(back, id);
    assert!(serde_json::from_str::<SubagentId>("\"3\"").is_err());
    assert!(serde_json::from_str::<SubagentId>("\"agent-0\"").is_err());
    assert!(serde_json::from_str::<SubagentId>("\"7\"").is_err());
}

#[test]
fn the_overlay_states_the_context_firewall() {
    assert!(SUBAGENT_OVERLAY.contains("only your final message"));
    assert!(SUBAGENT_OVERLAY.contains("handoff"));
    assert!(SUBAGENT_OVERLAY.contains("not the end user"));
}
