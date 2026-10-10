//! Pool admission corpus: the fail-closed step/turn/token checks that
//! refuse a step before it starts.
#![allow(clippy::unwrap_used)]

mod common;

use common::{FakeExecutor, journal_path, no_args, plan_texts, temp_dir};
use craft_workflow::{RunConclusion, StepStatus, WorkflowPlan, run_workflow};

fn plan(text: &str) -> WorkflowPlan {
    WorkflowPlan::parse("t.md", text).unwrap()
}

fn journal_open(dir: &std::path::Path) -> craft_workflow::Journal {
    craft_workflow::Journal::open(&journal_path(dir)).unwrap()
}

fn report_status(report: &craft_workflow::WorkflowReport, id: &str) -> StepStatus {
    report
        .steps
        .iter()
        .find(|s| s.id == id)
        .unwrap()
        .status
        .clone()
}

#[tokio::test]
async fn concurrent_wave_cannot_overdraw_the_turn_budget() {
    let text = r#"---
name: wave
limits:
  max-total-turns: 3
  max-concurrent-steps: 2
steps:
  - id: a
    agent: w
    max-turns: 2
    body: |
      a
  - id: b
    agent: w
    max-turns: 2
    body: |
      b
---
prelude
"#;
    let dir = temp_dir("wave");
    let mut journal = journal_open(&dir);
    let fake = FakeExecutor::new();
    let report = run_workflow(&plan(text), &no_args(), &fake, &mut journal)
        .await
        .unwrap();
    assert_eq!(fake.calls(), ["a"]);
    assert_eq!(fake.max_in_flight(), 1);
    assert!(matches!(report.conclusion, RunConclusion::BudgetExhausted));
    assert_eq!(report.exit_code, 3);
}

#[tokio::test]
async fn reservations_release_on_settle_and_admit_the_next_step() {
    let text = r#"---
name: release
limits:
  max-total-turns: 8
  max-concurrent-steps: 2
steps:
  - id: a
    agent: w
    max-turns: 2
    body: |
      a
  - id: b
    agent: w
    max-turns: 2
    body: |
      b
  - id: c
    agent: w
    max-turns: 2
    body: |
      c
  - id: d
    agent: w
    max-turns: 2
    body: |
      d
  - id: e
    agent: w
    max-turns: 2
    body: |
      e
---
prelude
"#;
    let dir = temp_dir("release");
    let mut journal = journal_open(&dir);
    let fake = FakeExecutor::new();
    let report = run_workflow(&plan(text), &no_args(), &fake, &mut journal)
        .await
        .unwrap();
    assert_eq!(fake.max_in_flight(), 2);
    assert_eq!(fake.calls().len(), 4);
    assert!(fake.calls().contains(&"d".to_string()));
    assert!(matches!(
        report_status(&report, "e"),
        StepStatus::Skipped { .. }
    ));
    assert!(matches!(report.conclusion, RunConclusion::BudgetExhausted));
}

#[tokio::test]
async fn step_cap_refuses_admission_and_exhausts_budget() {
    let text = plan_texts::FOUR_INDEPENDENT.replace("max-steps: 4", "max-steps: 2");
    let dir = temp_dir("stepcap");
    let mut journal = journal_open(&dir);
    let fake = FakeExecutor::new();
    let report = run_workflow(&plan(&text), &no_args(), &fake, &mut journal)
        .await
        .unwrap();
    assert_eq!(report.exit_code, 3);
    assert_eq!(report.failed_step, None);
    assert!(matches!(report.conclusion, RunConclusion::BudgetExhausted));
    assert_eq!(fake.calls(), ["w", "x"]);
    match report_status(&report, "z") {
        StepStatus::Skipped { reason } => {
            assert_eq!(reason, craft_workflow::SkipReason::Unreachable)
        }
        other => panic!("expected a visible skip, got {other:?}"),
    }
}

#[tokio::test]
async fn turn_reservation_refuses_admission_before_the_next_start() {
    let text = r#"---
name: turns
limits:
  max-total-turns: 3
steps:
  - id: a
    agent: w
    max-turns: 2
    body: |
      a
  - id: b
    agent: w
    max-turns: 2
    body: |
      b
---
prelude
"#;
    let dir = temp_dir("turns");
    let mut journal = journal_open(&dir);
    let fake = FakeExecutor::new();
    let report = run_workflow(&plan(text), &no_args(), &fake, &mut journal)
        .await
        .unwrap();
    assert_eq!(fake.calls(), ["a"]);
    assert!(matches!(report.conclusion, RunConclusion::BudgetExhausted));
    assert_eq!(report.exit_code, 3);
    match report_status(&report, "b") {
        StepStatus::Skipped { reason } => {
            assert_eq!(reason, craft_workflow::SkipReason::Unreachable)
        }
        other => panic!("expected a visible skip, got {other:?}"),
    }
}

async fn run_two_step_token_pool(
    max_total_tokens: u64,
) -> (craft_workflow::WorkflowReport, FakeExecutor) {
    let text = format!(
        r#"---
name: tokens
limits:
  max-total-tokens: {max_total_tokens}
steps:
  - id: a
    agent: w
    body: |
      a
  - id: b
    agent: w
    body: |
      b
---
prelude
"#
    );
    let dir = temp_dir("tokens");
    let mut journal = journal_open(&dir);
    let fake = FakeExecutor::new();
    let report = run_workflow(&plan(&text), &no_args(), &fake, &mut journal)
        .await
        .unwrap();
    (report, fake)
}

#[tokio::test]
async fn token_pool_exhaustion_refuses_later_admissions() {
    let (report, fake) = run_two_step_token_pool(50).await;
    assert_eq!(fake.calls(), ["a"]);
    assert!(matches!(report.conclusion, RunConclusion::BudgetExhausted));
}

#[tokio::test]
async fn completion_before_budget_lets_the_last_step_finish() {
    let (report, fake) = run_two_step_token_pool(150).await;
    assert_eq!(fake.calls(), ["a", "b"]);
    assert!(report.is_success());
    assert!(report.total_usage.total_tokens() > 150);
}
