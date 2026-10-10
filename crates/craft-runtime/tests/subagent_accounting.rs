//! Accounting and contract corpus: parent-only turn counting with
//! child tokens folded in, the envelope golden over the real seam,
//! the additive event contract (drain after the tool batch,
//! `run_finished` last), session layout for in-memory parents, and
//! registry filtering at the request seam.

mod subagent_common;

use craft_runtime::ExitStatus;

use subagent_common::{text_reply, tool_reply, usage};

/// The serialized type tag of every event, in order.
fn event_tags(events: &[craft_runtime::RuntimeEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            serde_json::to_value(event)
                .ok()
                .and_then(|value| value.get("type").cloned())
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .collect()
}

/// The additive contract: the batch drains between the tool result and
/// the next turn, and run_finished stays last.
fn assert_event_drain_order(tags: &[String]) {
    let tool_result = tags.iter().position(|name| name == "tool_result");
    let started = tags.iter().position(|name| name == "subagent_started");
    let finished = tags.iter().position(|name| name == "subagent_finished");
    let turn_two = tags.iter().rposition(|name| name == "turn_started");
    assert!(
        matches!(
            (tool_result, started, finished, turn_two),
            (Some(a), Some(b), Some(c), Some(d)) if a < b && b < c && c < d
        ),
        "the batch drains between the tool result and the next turn: {tags:?}"
    );
    assert_eq!(
        tags.last().map(String::as_str),
        Some("run_finished"),
        "run_finished stays last"
    );
}

/// The started event serializes exactly its documented fields.
fn assert_started_shape(events: &[craft_runtime::RuntimeEvent]) {
    let started = events
        .iter()
        .find(|event| matches!(event, craft_runtime::RuntimeEvent::SubagentStarted { .. }));
    let keys = started
        .and_then(|event| serde_json::to_value(event).ok())
        .and_then(|value| {
            value
                .as_object()
                .map(|object| object.keys().cloned().collect::<Vec<String>>())
        });
    assert_eq!(
        keys,
        Some(vec![
            String::from("id"),
            String::from("max_tokens"),
            String::from("max_turns"),
            String::from("profile"),
            String::from("timeout_secs"),
            String::from("turn"),
            String::from("type"),
        ]),
        "the started event serializes exactly its documented fields"
    );
}

#[tokio::test]
async fn parent_turns_stay_parent_only_while_usage_carries_children() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agent",
            "{\"agent\": \"repo-scout\", \"prompt\": \"two turns\"}",
            usage(10, 5),
        ),
        tool_reply("k1", "glob", "{}", usage(100, 10)),
        text_reply("child done", usage(100, 10)),
        text_reply("parent handoff", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::logged("turn-accounting", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider, "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    assert_eq!(
        outcome.turns, 2,
        "child turns never advance the parent counter"
    );
    assert_eq!(
        outcome.usage.input_tokens, 220,
        "the child's spend folds in"
    );
    let finished = subagent_common::finished(&events);
    assert_eq!(
        finished[0].2, 2,
        "the envelope carries the child's own turns"
    );
    let run_turn_starts = events
        .iter()
        .filter(|event| matches!(event, craft_runtime::RuntimeEvent::TurnStarted { .. }))
        .count();
    assert_eq!(
        run_turn_starts, 2,
        "child per-turn events never reach the parent"
    );
}

#[tokio::test]
async fn envelope_golden_over_the_real_seam() {
    let spawn_turn = usage(10, 5);
    let child_turn = usage(50, 10);
    let parent_last = usage(10, 5);
    let run_token_cap = 1_000;
    let child_spend = child_turn.input_tokens + child_turn.output_tokens;
    let parent_spend = spawn_turn.input_tokens + spawn_turn.output_tokens;
    let budget_remaining = run_token_cap - parent_spend - child_spend;
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agent",
            "{\"agent\": \"repo-scout\", \"prompt\": \"scan\"}",
            spawn_turn,
        ),
        text_reply("scout handoff text", child_turn),
        text_reply("parent done", parent_last),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::logged("envelope-golden", run_token_cap),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider, "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    let content = subagent_common::tool_results(&events)[0].1.clone();
    let expected_tail = rig
        .workspace
        .join("sessions")
        .join("parent")
        .join("agents")
        .join("agent-1")
        .join("session.jsonl")
        .display()
        .to_string();
    let expected = format!(
        "agent_id: agent-1\nprofile: repo-scout\nstatus: completed\n\
         stop_reason: completed\nturns: 1  usage: {child_spend} tokens  budget_remaining: {budget_remaining}\n\
         transcript: {expected_tail}\n[summary]\nscout handoff text"
    );
    assert_eq!(content, expected, "the single envelope is the golden block");
}

#[tokio::test]
async fn in_memory_parents_keep_children_in_memory() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agent",
            "{\"agent\": \"repo-scout\", \"prompt\": \"scan\"}",
            usage(10, 5),
        ),
        text_reply("child done", usage(50, 10)),
        text_reply("parent done", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("memory-layout", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider, "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    let finished_event = events
        .iter()
        .find_map(|event| match event {
            craft_runtime::RuntimeEvent::SubagentFinished { session_path, .. } => {
                Some(session_path.clone())
            }
            _ => None,
        })
        .expect("a finished event");
    assert_eq!(finished_event, None, "in-memory parents omit the pointer");
    let content = subagent_common::tool_results(&events)[0].1.clone();
    assert!(!content.contains("transcript:"), "{content}");
}

#[tokio::test]
async fn event_contract_drains_between_the_tool_result_and_the_next_turn() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agent",
            "{\"agent\": \"repo-scout\", \"prompt\": \"scan\"}",
            usage(10, 5),
        ),
        text_reply("child done", usage(50, 10)),
        text_reply("parent done", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::logged("event-order", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider, "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    let tags = event_tags(&events);
    assert_event_drain_order(&tags);
    assert_started_shape(&events);
}

#[tokio::test]
async fn settled_child_spend_over_the_cap_ends_the_run_before_the_next_request() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agent",
            "{\"agent\": \"repo-scout\", \"prompt\": \"spend\"}",
            usage(10, 5),
        ),
        text_reply("child spent its slice", usage(40, 10)),
        text_reply("never sent", usage(1, 1)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::logged("budget-settle", 50),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, _) = subagent_common::run(config, registry, provider.clone(), "Drive.").await;
    assert_eq!(outcome.exit, ExitStatus::Budget, "{:?}", outcome.detail);
    assert_eq!(
        provider.requests().len(),
        2,
        "the post-settle cap check ends the run before a third paid turn"
    );
    assert_eq!(outcome.usage.input_tokens, 50);
}
