//! Core corpus: zero-context firewall, the budget sum rule, admission
//! refusal before any spend, and child-denial isolation — the
//! never-crash regression (a hostile child ending is data for the
//! parent, never a parent abort).

mod subagent_common;

use craft_runtime::ExitStatus;
use craft_runtime::session::Session;

use subagent_common::{text_reply, tool_reply, usage};

const PARENT_PROMPT: &str = "PARENT-SENTINEL orchestrate the child and report.";
const CHILD_PROMPT: &str = "Inspect the workspace and report readiness.";

fn spawn_args(prompt: &str, extra: &str) -> String {
    format!("{{\"agent\": \"repo-scout\", \"prompt\": \"{prompt}\"{extra}}}")
}

#[tokio::test]
async fn zero_context_pin_the_child_sees_only_the_task_prompt() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agent",
            &spawn_args(CHILD_PROMPT, ""),
            usage(10, 5),
        ),
        text_reply("scout ready", usage(50, 10)),
        text_reply("parent handoff", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::logged("zero-context", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) = subagent_common::run(config, registry, provider, PARENT_PROMPT).await;
    assert_eq!(outcome.exit, ExitStatus::Ok, "{:?}", outcome.detail);
    let child = rig
        .workspace
        .join("sessions")
        .join("parent")
        .join("agents")
        .join("agent-1")
        .join("session.jsonl");
    let messages = Session::load(&child).expect("the child transcript loads");
    assert_eq!(
        messages.first().map(|first| match first {
            craft_core::types::ChatMessage::User { text, .. } => text.clone(),
            _ => String::new(),
        }),
        Some(CHILD_PROMPT.to_string()),
        "the child's first message is exactly the task prompt"
    );
    let transcript = std::fs::read_to_string(&child).expect("child transcript");
    assert!(
        !transcript.contains("PARENT-SENTINEL"),
        "no parent context ever reaches the child session"
    );
    let parent = std::fs::read_to_string(rig.session.as_ref().expect("file-backed"))
        .expect("parent transcript");
    assert!(parent.contains("PARENT-SENTINEL"));
    assert_eq!(
        subagent_common::started(&events),
        vec![("agent-1".into(), "repo-scout".into())]
    );
}

#[tokio::test]
async fn sum_rule_child_overspend_then_parent_cap_fires_per_run() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agent",
            "{\"agent\": \"repo-scout\", \"prompt\": \"p\", \"maxTokens\": 1000}",
            usage(10, 5),
        ),
        text_reply("child halfway", usage(1200, 0)),
        text_reply("parent final", usage(200, 100)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::logged("sum-rule", 1_500),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider, "Drive the run.").await;
    assert_eq!(outcome.exit, ExitStatus::Budget, "{:?}", outcome.detail);
    assert_eq!(
        outcome.turns, 2,
        "the parent needed its envelope-reading turn"
    );
    assert_eq!(
        outcome.usage.input_tokens, 1_410,
        "parent and child spend sum"
    );
    let finished = subagent_common::finished(&events);
    assert_eq!(finished.len(), 1);
    assert_eq!(
        finished[0].1, "budget",
        "the child died exit 3 on its own slice"
    );
    let results = subagent_common::tool_results(&events);
    assert!(results[0].0, "a failed child is an is_error result");
    assert!(
        results[0].1.contains("budget_remaining: 285"),
        "the honest post-child remaining budget: {}",
        results[0].1
    );
}

#[tokio::test]
async fn admission_refusal_makes_zero_provider_calls() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agent",
            "{\"agent\": \"repo-scout\", \"prompt\": \"p\", \"maxTokens\": 5000}",
            usage(10, 5),
        ),
        text_reply("parent final", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::logged("admission", 1_000),
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
        results[0].1.contains("token budget:"),
        "the refusing dimension is named: {}",
        results[0].1
    );
    assert!(
        subagent_common::started(&events).is_empty(),
        "nothing launched"
    );
    assert_eq!(
        provider.requests().len(),
        2,
        "zero child streams: only the two parent turns called the provider"
    );
}

#[tokio::test]
async fn a_childs_refused_path_is_data_and_the_child_recovers() {
    let provider = subagent_common::FakeProvider::new(vec![
        tool_reply(
            "c1",
            "spawn_agent",
            "{\"agent\": \"repo-scout\", \"prompt\": \"read the env file\"}",
            usage(10, 5),
        ),
        tool_reply("k1", "read_file", "{\"path\": \".env\"}", usage(5, 5)),
        text_reply("child fine", usage(5, 5)),
        text_reply("parent handoff", usage(10, 5)),
    ]);
    let rig = subagent_common::rig(
        subagent_common::RigSpec::logged("denial", 100_000),
        provider.clone(),
        vec![subagent_common::scout()],
    );
    let (config, registry) = subagent_common::parent_run(&rig, 20);
    let (outcome, events) =
        subagent_common::run(config, registry, provider, "Read the secrets.").await;
    assert_eq!(
        outcome.exit,
        ExitStatus::Ok,
        "the parent run survives a refused child call: {:?}",
        outcome.detail
    );
    let finished = subagent_common::finished(&events);
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].1, "completed", "the child recovered");
    let results = subagent_common::tool_results(&events);
    assert!(!results[0].0);
    assert!(results[0].1.contains("child fine"));
    let child = rig
        .workspace
        .join("sessions")
        .join("parent")
        .join("agents")
        .join("agent-1")
        .join("session.jsonl");
    let messages = Session::load(&child).expect("the child transcript loads");
    let refused = messages.iter().any(|message| {
        matches!(
            message,
            craft_core::types::ChatMessage::Tool {
                call_id,
                is_error: true,
                content,
                ..
            } if call_id == "k1" && content.contains("path rejected")
        )
    });
    assert!(
        refused,
        "the refusal is an error result in the child record"
    );
}
