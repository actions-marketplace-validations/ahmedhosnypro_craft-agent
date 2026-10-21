//! Budget corpus: max-turns and max-total-tokens aborts, both fail-closed
//! on unverifiable usage. Spent means the sum of the disjoint usage
//! buckets — input + cache-read + cache-write + output + reasoning —
//! and the guard runs after every turn, the final one included.

mod common;

use common::*;
use craft_core::types::{StopReason, Usage};
use craft_runtime::{CancelCategory, RunConfig, RunOutcome, Runner, RuntimeEvent};

fn limited(max_turns: u32, max_total_tokens: u64) -> RunConfig {
    let mut cfg = RunConfig::new(std::path::PathBuf::from("."));
    cfg.max_turns = max_turns;
    cfg.max_total_tokens = Some(max_total_tokens);
    cfg
}

/// Runs one scripted budget row to completion.
async fn run_row(
    script: Vec<Turn>,
    max_turns: u32,
    max_total_tokens: u64,
    prompt: &str,
) -> (RunOutcome, Vec<RuntimeEvent>) {
    let provider = FakeProvider::new(script);
    let runner = Runner::new(
        provider,
        registry_with_ok("scripted", "out"),
        limited(max_turns, max_total_tokens),
    );
    run_collect(&runner, prompt).await
}

/// Asserts the budget conclusion with its exact spend line.
fn assert_budget_exit(outcome: &RunOutcome, spent: u64, allowed: u64) {
    assert_eq!(outcome.exit_code(), 3);
    assert_eq!(outcome.category, CancelCategory::Budget);
    let line = format!("token budget exceeded: {spent} tokens spent, {allowed} allowed");
    assert!(outcome.detail.contains(&line), "{outcome:?}");
}

/// Asserts the fail-closed conclusion for unverifiable usage.
fn assert_incomplete_exit(outcome: &RunOutcome) {
    assert_eq!(outcome.exit_code(), 3);
    assert_eq!(outcome.category, CancelCategory::Budget);
    assert!(outcome.detail.contains("incomplete"));
}

/// Asserts the max-turns conclusion and its terminal event.
fn assert_max_turns_exit(outcome: &RunOutcome, events: &[RuntimeEvent]) {
    assert_eq!(outcome.exit_code(), 2);
    assert_eq!(outcome.category, CancelCategory::MaxTurns);
    assert_eq!(outcome.turns, 2);
    assert!(outcome.detail.contains("max turns"));
    assert!(matches!(
        events.last(),
        Some(RuntimeEvent::RunFinished { exit_code, category, .. })
        if *exit_code == 2 && *category == CancelCategory::MaxTurns
    ));
}

#[tokio::test]
async fn max_turns_abort_is_exit_two_with_category() {
    let (outcome, events) = run_row(
        vec![tool_turn("c1", "scripted", "{}", usage(1, 1))],
        2,
        u64::MAX,
        "loop forever",
    )
    .await;

    assert_max_turns_exit(&outcome, &events);
}

#[tokio::test]
async fn max_total_tokens_abort_is_exit_three_before_more_tools() {
    let (outcome, events) = run_row(
        vec![tool_turn("c1", "scripted", "{}", usage(15, 10))],
        40,
        20,
        "spend",
    )
    .await;

    // The guard fires at the bucket sum, before the next tool round
    // could execute.
    assert_budget_exit(&outcome, 25, 20);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ToolResult { .. }))
    );
}

#[tokio::test]
async fn cache_read_tokens_count_toward_spend() {
    // A turn at 10 uncached in + 5 out with a 1M-token cached prefix:
    // the prefix is real context the provider processed, so the turn
    // spends 1,000,015 tokens and the guard fires at turn one.
    let used = bucketed_usage(10, 5, 1_000_000, 0);
    let (outcome, _) = run_row(
        vec![tool_turn("c1", "scripted", "{}", used)],
        40,
        100,
        "cached prefix",
    )
    .await;

    assert_budget_exit(&outcome, 1_000_015, 100);
    assert_eq!(
        outcome.usage.cache_read_tokens, 1_000_000,
        "the cache bucket still accumulates for reporting"
    );
}

#[tokio::test]
async fn reasoning_counts_toward_spend() {
    // Two turns at 10 in + 20 visible out + 20 reasoning: each turn
    // spends 50, so a 61 budget funds exactly one round.
    let used = bucketed_usage(10, 20, 0, 20);
    let (outcome, _) = run_row(
        vec![
            tool_turn("c1", "scripted", "{}", used),
            tool_turn("c2", "scripted", "{}", used),
        ],
        40,
        61,
        "reasoning",
    )
    .await;

    assert_budget_exit(&outcome, 100, 61);
    assert_eq!(outcome.turns, 2);
    assert_eq!(outcome.usage.reasoning_tokens, 40);
}

#[tokio::test]
async fn the_guard_fires_on_the_full_bucket_sum() {
    // 15 uncached in + 5 000 cached + 10 visible out + 23 reasoning:
    // the detail pins the spend at the bucket sum.
    let used = bucketed_usage(15, 10, 5_000, 23);
    let (outcome, _) = run_row(
        vec![tool_turn("c1", "scripted", "{}", used)],
        40,
        20,
        "spend",
    )
    .await;

    assert_budget_exit(&outcome, 5_048, 20);
}

#[tokio::test]
async fn finishing_exactly_at_the_cap_is_exit_three() {
    // The cap is inclusive: a run that spends exactly its allowance
    // has reached the budget, and the guard runs before completion.
    let (outcome, _) = run_row(
        vec![text_turn("done", StopReason::EndTurn, usage(10, 0))],
        40,
        10,
        "finish",
    )
    .await;
    assert_budget_exit(&outcome, 10, 10);
}

#[tokio::test]
async fn a_finishing_turn_that_crosses_the_cap_aborts_with_exit_three() {
    // The final turn is charged before the run may conclude: crossing
    // the cap while delivering the answer is a budget conclusion, not
    // a silent overshoot.
    let (outcome, _) = run_row(
        vec![
            tool_turn("c1", "scripted", "{}", usage(60, 0)),
            text_turn("done", StopReason::EndTurn, usage(30, 15)),
        ],
        40,
        100,
        "cross on the last turn",
    )
    .await;

    assert_budget_exit(&outcome, 105, 100);
    assert_eq!(outcome.turns, 2);
}

#[tokio::test]
async fn a_cache_heavy_run_crosses_on_the_finishing_turn() {
    // A mostly-cached run: turn one spends 3 134 and funds its tool
    // round; the final turn pushes the bucket sum to 9 014, so the
    // run ends budgeted even though the answer arrived.
    let (outcome, events) = run_row(
        vec![
            tool_turn("c1", "scripted", "{}", bucketed_usage(969, 18, 1_920, 227)),
            text_turn(
                "done",
                StopReason::EndTurn,
                bucketed_usage(1_867, 1_199, 2_560, 254),
            ),
        ],
        40,
        4_000,
        "mostly cache",
    )
    .await;

    assert_budget_exit(&outcome, 9_014, 4_000);
    assert_eq!(outcome.turns, 2);
    assert_eq!(outcome.usage.input_tokens, 2_836);
    assert_eq!(outcome.usage.output_tokens, 1_217);
    assert_eq!(outcome.usage.cache_read_tokens, 4_480);
    assert_eq!(outcome.usage.reasoning_tokens, 481);
    // Turn one's tool round ran; only the conclusion changed.
    assert!(events.iter().any(|e| matches!(
        e,
        RuntimeEvent::ToolResult { is_error, .. } if !*is_error
    )));
}

#[tokio::test]
async fn incomplete_usage_never_silently_passes_the_budget_check() {
    let (outcome, _) = run_row(
        vec![tool_turn("c1", "scripted", "{}", Usage::omitted())],
        40,
        4_000_000,
        "unverifiable",
    )
    .await;

    assert_incomplete_exit(&outcome);
}

#[tokio::test]
async fn incomplete_usage_on_a_finishing_turn_fails_closed() {
    let (outcome, _) = run_row(
        vec![text_turn("done", StopReason::EndTurn, Usage::omitted())],
        40,
        4_000_000,
        "finish",
    )
    .await;

    assert_incomplete_exit(&outcome);
    assert!(outcome.usage.incomplete);
}
