//! Detached-spawn corpus: the call returns at admission, the bounded
//! pool runs the members under the concurrency cap, and `poll_agents`
//! hands the envelopes back by id or in bulk — including the bounded
//! wait, the unknown-id refusal, the fail-closed admission refusals,
//! and the wall-clock reclaim that keeps a stuck child from hanging
//! anything.

mod subagent_common;

use std::time::Duration;

use craft_runtime::ExitStatus;
use subagent_common::provider::Reply;

use subagent_common::{delayed, text_reply, tool_reply, usage};

/// One detached batch call.
fn detached_args(items: &[&str], extra: &str) -> String {
    let list = items
        .iter()
        .map(|item| format!("\"{item}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\"agent\": \"repo-scout\", \"promptTemplate\": \"report on {{{{item}}}}\", \"items\": [{list}]{extra}}}"
    )
}

/// The reply queue for a detached round trip. The in-memory rig polls
/// the parent task without yielding, so the parent's second turn takes
/// the entry right after the spawn: that entry is the poll, and the
/// members consume the entries behind it during the poll's bounded
/// wait. Ordering is therefore deterministic.
fn round_trip(
    count: usize,
    spawn_args: &str,
    poll_args: &str,
    child_delay: Duration,
) -> Vec<Reply> {
    let mut replies = vec![
        tool_reply("c1", "spawn_agents", spawn_args, usage(10, 5)),
        tool_reply("c2", "poll_agents", poll_args, usage(10, 5)),
    ];
    for index in 0..count {
        replies.push(delayed(
            text_reply(&format!("scout {index} done"), usage(4, 2)),
            child_delay,
        ));
    }
    replies.push(text_reply("parent done", usage(10, 5)));
    replies
}

#[tokio::test]
async fn detached_returns_at_admission_and_polls_back_every_envelope() {
    let spawn_args = detached_args(&["alpha", "beta", "gamma"], ", \"detached\": true");
    let provider = subagent_common::FakeProvider::new(round_trip(
        3,
        &spawn_args,
        "{\"waitSecs\": 10}",
        Duration::ZERO,
    ));
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("detached-roundtrip", 100_000).caps(
            craft_runtime::SubagentCaps {
                max_depth: 2,
                max_total: 32,
                max_concurrency: 8,
            },
        ),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider.clone(), "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);

    // The spawn terminal named the accepted ids without awaiting the
    // members; the poll then collected every envelope.
    let results = subagent_common::tool_results(&events);
    let accepted = &results[0].1;
    assert!(accepted.contains("detached: 3 accepted"), "{accepted}");
    for id in ["agent-1", "agent-2", "agent-3"] {
        assert!(accepted.contains(id), "{accepted}");
    }
    let polled = &results[1].1;
    for id in ["agent-1", "agent-2", "agent-3"] {
        assert!(polled.contains(id), "{polled}");
    }
    assert!(polled.contains("status: completed"), "{polled}");
    assert!(polled.contains("tally: completed: 3"), "{polled}");
    assert!(polled.contains("outstanding: none"), "{polled}");

    // Every child streamed, and every settle is evidenced by an event.
    let started = subagent_common::started(&events);
    let finished = subagent_common::finished(&events);
    assert_eq!(started.len(), 3, "three children launched");
    assert_eq!(finished.len(), 3, "three children settled");
    for (id, _profile) in &started {
        assert!(
            finished.iter().any(|(finished_id, _, _)| finished_id == id),
            "{id} never settled"
        );
    }
}

#[tokio::test]
async fn detached_wait_bounded_collects_as_each_child_lands() {
    let spawn_args = detached_args(
        &["alpha", "beta"],
        ", \"detached\": true, \"timeoutSecs\": 60",
    );
    let provider = subagent_common::FakeProvider::new(round_trip(
        2,
        &spawn_args,
        "{\"waitSecs\": 10}",
        Duration::ZERO,
    ));
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("detached-wait", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider.clone(), "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);

    let results = subagent_common::tool_results(&events);
    let polled = &results[1].1;
    assert!(polled.contains("tally: completed: 2"), "{polled}");
    assert!(polled.contains("outstanding: none"), "{polled}");
    assert_eq!(subagent_common::finished(&events).len(), 2);
}

#[tokio::test]
async fn poll_of_an_unknown_id_is_a_typed_error() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "poll_agents",
            "{\"ids\": [\"agent-9\"]}",
            usage(10, 5),
        ),
        text_reply("parent done", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("poll-unknown", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider.clone(), "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    let results = subagent_common::tool_results(&events);
    assert!(results[0].0, "the refusal is an is_error result");
    assert!(
        results[0].1.contains("unknown agent ids"),
        "{}",
        results[0].1
    );
}

#[tokio::test]
async fn detached_pre_flight_refuses_when_the_batch_does_not_fit() {
    let spawn_args = detached_args(
        &["alpha", "beta"],
        ", \"detached\": true, \"maxTokens\": 600",
    );
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply("c1", "spawn_agents", &spawn_args, usage(10, 5)),
        text_reply("parent done", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("detached-pre-flight", 1_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider.clone(), "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    let results = subagent_common::tool_results(&events);
    assert!(results[0].0, "the refusal is an is_error result");
    assert!(results[0].1.contains("token budget:"), "{}", results[0].1);
    assert!(
        subagent_common::started(&events).is_empty(),
        "nothing launched"
    );
    assert_eq!(provider.requests().len(), 2, "zero child streams");
}

#[tokio::test]
async fn detached_concurrency_cap_queues_the_rest() {
    let spawn_args = detached_args(&["alpha", "beta", "gamma"], ", \"detached\": true");
    let provider = subagent_common::FakeProvider::new(round_trip(
        3,
        &spawn_args,
        "{\"waitSecs\": 10}",
        Duration::ZERO,
    ));
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("detached-pool-cap", 100_000).caps(
            craft_runtime::SubagentCaps {
                max_depth: 2,
                max_total: 32,
                max_concurrency: 2,
            },
        ),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider.clone(), "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    assert!(
        provider.max_concurrency() <= 2,
        "the pool never exceeded the concurrency cap; saw {}",
        provider.max_concurrency()
    );
    let results = subagent_common::tool_results(&events);
    assert!(
        results[1].1.contains("tally: completed: 3"),
        "{}",
        results[1].1
    );
    assert_eq!(subagent_common::finished(&events).len(), 3);
}

#[tokio::test]
async fn detached_child_past_its_wall_clock_settles_timed_out() {
    let spawn_args = detached_args(
        &["alpha", "beta"],
        ", \"detached\": true, \"timeoutSecs\": 1",
    );
    let provider = subagent_common::FakeProvider::new(round_trip(
        2,
        &spawn_args,
        "{\"waitSecs\": 10}",
        Duration::from_secs(3),
    ));
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("detached-timeout", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider.clone(), "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);

    let results = subagent_common::tool_results(&events);
    let polled = &results[1].1;
    assert!(polled.contains("stop_reason: timed_out"), "{polled}");
    assert!(
        polled.contains("tally: completed: 0, failed: 0, timed_out: 2"),
        "{polled}"
    );
    assert!(polled.contains("outstanding: none"), "{polled}");
    let finished = subagent_common::finished(&events);
    assert_eq!(finished.len(), 2, "both clock-reclaimed children settled");
    for (_id, stop, _turns) in &finished {
        assert_eq!(stop, "timed_out", "{:?}", finished);
    }
}

#[tokio::test]
async fn detached_single_child_returns_at_admission() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agent",
            "{\"agent\": \"repo-scout\", \"prompt\": \"report on alpha\", \"detached\": true}",
            usage(10, 5),
        ),
        tool_reply("c2", "poll_agents", "{\"waitSecs\": 10}", usage(10, 5)),
        text_reply("scout done", usage(4, 2)),
        text_reply("parent done", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::memory("detached-single", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider.clone(), "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    let results = subagent_common::tool_results(&events);
    assert!(
        results[0].1.contains("detached: 1 accepted"),
        "{}",
        results[0].1
    );
    let polled = &results[1].1;
    assert!(polled.contains("agent-1"), "{polled}");
    assert!(polled.contains("status: completed"), "{polled}");
    assert!(polled.contains("outstanding: none"), "{polled}");
}
