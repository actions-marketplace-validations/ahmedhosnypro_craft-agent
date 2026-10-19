//! Batch and hostile-argument corpus: the joint pre-flight (a batch
//! launches only when every member's slice fits), the concurrency
//! cap with FIFO queueing in completion order, the typed rejections
//! for hostile arguments with nothing launched, and the child's
//! filtered tool list at the request seam.

mod subagent_common;

use std::time::Duration;

use craft_runtime::ExitStatus;

use subagent_common::{delayed, text_reply, tool_reply, usage};

fn batch_args(items: &[&str], extra: &str) -> String {
    let list = items
        .iter()
        .map(|item| format!("\"{item}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\"agent\": \"repo-scout\", \"promptTemplate\": \"report on {{{{item}}}}\", \"items\": [{list}]{extra}}}"
    )
}

/// One refusal row: hostile arguments produce a typed is_error
/// result, zero launches, and a run that still completes.
async fn refusal_row(name: &str, tool: &str, args: &str, fragment: &str) {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply("c1", tool, args, usage(10, 5)),
        text_reply("parent done", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory(name, 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider, "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{name}: {:?}", outcome.detail);
    let results = subagent_common::tool_results(&events);
    assert!(results[0].0, "{name}: the refusal is an is_error result");
    assert!(
        results[0].1.contains(fragment),
        "{name}: expected `{fragment}` in: {}",
        results[0].1
    );
    assert!(
        subagent_common::started(&events).is_empty(),
        "{name}: nothing launched"
    );
}

#[tokio::test]
async fn batch_pre_flight_refuses_when_the_joint_slices_do_not_fit() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agents",
            &batch_args(&["one", "two"], ", \"maxTokens\": 600"),
            usage(10, 5),
        ),
        text_reply("parent done", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("batch-pre-flight", 1_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider.clone(), "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    assert!(
        subagent_common::started(&events).is_empty(),
        "nothing launched"
    );
    assert_eq!(provider.requests().len(), 2, "zero child streams");
    let results = subagent_common::tool_results(&events);
    assert!(results[0].0);
    assert!(results[0].1.contains("token budget:"));
}

#[tokio::test]
async fn concurrency_cap_four_fifo_in_completion_order() {
    let mut replies = vec![
        tool_reply(
            "c1",
            "spawn_agents",
            &batch_args(&["one", "two", "three", "four", "five", "six"], ""),
            usage(10, 5),
        ),
        delayed(
            text_reply("slowest member", usage(1, 1)),
            Duration::from_millis(60),
        ),
    ];
    for _ in 2..=6 {
        replies.push(delayed(
            text_reply("quick member", usage(1, 1)),
            Duration::from_millis(10),
        ));
    }
    replies.push(text_reply("parent done", usage(10, 5)));
    let provider = subagent_common::FakeProvider::new(replies);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("concurrency", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 40);
    let (outcome, events) =
        subagent_common::run(config, registry, provider.clone(), "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    assert_eq!(
        provider.max_concurrency(),
        4,
        "at most four child streams live at once"
    );
    let finished = subagent_common::finished(&events);
    assert_eq!(finished.len(), 6, "all members settle");
    let ids: Vec<&str> = finished.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids.last(),
        Some(&"agent-1"),
        "the slow member settles last: {ids:?}"
    );
    assert_eq!(
        ids.iter().filter(|id| **id != "agent-1").count(),
        5,
        "the queued members resume in completion order: {ids:?}"
    );
    let results = subagent_common::tool_results(&events);
    assert!(
        !results[0].0,
        "per-member outcomes never make the batch an error"
    );
    assert!(
        results[0]
            .1
            .contains("tally: completed: 6, failed: 0, timed_out: 0"),
        "the aggregate ends with the explicit tally: {}",
        results[0].1
    );
}

#[tokio::test]
async fn the_child_registry_filters_at_the_request_seam() {
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
        subagent_common::RigSpec::memory("registry-filter", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, _) =
        subagent_common::run(config, registry, provider.clone(), "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    let requests = provider.requests();
    let child_tools: Vec<String> = requests[1]
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert_eq!(
        child_tools,
        vec!["glob", "grep", "read_file"],
        "the child's request lists exactly its profile's tools"
    );
}

/// Malformed tool-call JSON survives the run: the fold salvages the
/// call and the call layer refuses it as an is_error result the model
/// can act on — nothing launches, the run continues.
#[tokio::test]
async fn malformed_tool_json_survives_as_tool_error_feedback() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply("c1", "spawn_agent", "not json at all", usage(10, 5)),
        text_reply("parent done", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("hostile-json", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider, "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    assert!(
        subagent_common::started(&events).is_empty(),
        "nothing launched"
    );
    let results = subagent_common::tool_results(&events);
    assert!(results[0].0, "the refusal is an is_error result");
    assert!(
        results[0].1.contains(
            "tool arguments were not valid JSON; the call was not executed: not json at all"
        ),
        "the feedback names the raw arguments: {}",
        results[0].1
    );
}

/// The hostile single-spawn rows.
async fn single_spawn_refusals() {
    let big_prompt = "a".repeat(65_537);
    let oversize = format!("{{\"agent\": \"repo-scout\", \"prompt\": \"{big_prompt}\"}}");
    refusal_row(
        "hostile-prompt",
        "spawn_agent",
        &oversize,
        "1..=65536 bytes, got 65537",
    )
    .await;
    refusal_row(
        "hostile-max-turns",
        "spawn_agent",
        "{\"agent\": \"repo-scout\", \"prompt\": \"p\", \"maxTurns\": 0}",
        "maxTurns must be a whole number between 1 and 10000",
    )
    .await;
    refusal_row(
        "hostile-max-tokens",
        "spawn_agent",
        "{\"agent\": \"repo-scout\", \"prompt\": \"p\", \"maxTokens\": 0}",
        "maxTokens must be a whole number between 1 and",
    )
    .await;
    refusal_row(
        "hostile-timeout",
        "spawn_agent",
        "{\"agent\": \"repo-scout\", \"prompt\": \"p\", \"timeoutSecs\": 21601}",
        "timeoutSecs must be a whole number between 0 and 21600",
    )
    .await;
    refusal_row(
        "hostile-unknown-profile",
        "spawn_agent",
        "{\"agent\": \"nope\", \"prompt\": \"p\"}",
        "unknown agent profile `nope`",
    )
    .await;
}

/// The hostile batch rows.
async fn batch_refusals() {
    let many: Vec<String> = (1..=33).map(|i| format!("\"item-{i}\"")).collect();
    refusal_row(
        "hostile-placeholder",
        "spawn_agents",
        "{\"agent\": \"repo-scout\", \"promptTemplate\": \"no placeholder\", \"items\": [\"a\", \"b\"]}",
        "must contain the {{item}} placeholder",
    )
    .await;
    refusal_row(
        "hostile-few-items",
        "spawn_agents",
        &batch_args(&["one"], ""),
        "`items` must be 2..=32 entries, got 1",
    )
    .await;
    refusal_row(
        "hostile-many-items",
        "spawn_agents",
        &format!(
            "{{\"agent\": \"repo-scout\", \"promptTemplate\": \"report on {{{{item}}}}\", \"items\": [{}]}}",
            many.join(", ")
        ),
        "`items` must be 2..=32 entries, got 33",
    )
    .await;
    refusal_row(
        "hostile-duplicates",
        "spawn_agents",
        &batch_args(&["same", "same"], ""),
        "duplicate filled prompts at items [1, 2]",
    )
    .await;
}

#[tokio::test]
async fn hostile_arguments_are_typed_errors_that_launch_nothing() {
    single_spawn_refusals().await;
    batch_refusals().await;
}
