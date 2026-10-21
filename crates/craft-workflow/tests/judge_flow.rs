//! Judge flow corpus: the engine-side verdict paths over a scripted
//! executor — the pass settle, the bounded feedback repair loop, the
//! exhausted budget, the panel seats, and warn-policy steps.
#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use craft_workflow::{
    Journal, RunConclusion, StepConclusion, StepExecutor, StepInputs, StepOutcome, StepStatus,
    StepUsage, WorkflowError, WorkflowPlan, run_workflow,
};

mod common;

use common::{journal_path, no_args, temp_dir};

/// One scripted attempt: the step it lands on, its conclusion, its message.
struct Attempt {
    id: String,
    conclusion: StepConclusion,
    message: String,
}

impl Attempt {
    fn new(id: &str, conclusion: StepConclusion, message: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            conclusion,
            message: message.into(),
        }
    }
}

#[derive(Default)]
struct Scripted {
    outcomes: Mutex<BTreeMap<String, VecDeque<(StepConclusion, String)>>>,
    calls: Mutex<Vec<String>>,
    prompts: Mutex<BTreeMap<String, Vec<String>>>,
}

impl Scripted {
    fn new() -> Self {
        Self::default()
    }

    fn queue(&self, attempt: Attempt) {
        self.outcomes
            .lock()
            .unwrap()
            .entry(attempt.id)
            .or_default()
            .push_back((attempt.conclusion, attempt.message));
    }

    fn succeed(&self, id: &str) {
        self.queue(Attempt::new(
            id,
            StepConclusion::Success,
            format!("did {id}"),
        ));
    }

    fn fail(&self, id: &str) {
        self.queue(Attempt::new(
            id,
            StepConclusion::Error,
            format!("crashed {id}"),
        ));
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn call_count(&self, id: &str) -> usize {
        self.calls().iter().filter(|seen| *seen == id).count()
    }

    fn prompts_for(&self, id: &str) -> Vec<String> {
        self.prompts
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }
}

impl StepExecutor for Scripted {
    async fn execute(&self, step: StepInputs) -> Result<StepOutcome, WorkflowError> {
        self.calls.lock().unwrap().push(step.id.clone());
        self.prompts
            .lock()
            .unwrap()
            .entry(step.id.clone())
            .or_default()
            .push(step.prompt.clone());
        tokio::task::yield_now().await;
        let scripted = {
            let mut outcomes = self.outcomes.lock().unwrap();
            outcomes
                .get_mut(&step.id)
                .and_then(|queue| queue.pop_front())
        };
        let (conclusion, message) =
            scripted.unwrap_or((StepConclusion::Success, format!("did {}", step.id)));
        Ok(StepOutcome {
            conclusion,
            usage: StepUsage {
                turns: 2,
                input_tokens: 60,
                output_tokens: 30,
                reasoning_tokens: 10,
            },
            final_message: message,
        })
    }
}

const JUDGE_PLAN: &str = r#"---
name: tdd
steps:
  - id: write-tests
    agent: test-writer
    body: |
      write the failing tests
  - id: implement
    agent: implementer
    deny-read:
      - tests/**
    body: |
      implement against the spec
  - id: gate
    kind: judge
    agent: reviewer
    needs: [implement]
    judge:
      panel: 1
      merge: unanimous
      max-feedback: 2
    body: |
      judge the implementation
---"#;

fn plan(text: &str) -> WorkflowPlan {
    WorkflowPlan::parse("t.md", text).unwrap()
}

async fn run(text: &str, fake: &Scripted) -> craft_workflow::WorkflowReport {
    let dir = temp_dir("judge");
    let mut journal = Journal::open(&journal_path(&dir)).unwrap();
    run_workflow(&plan(text), &no_args(), fake, &mut journal)
        .await
        .unwrap()
}

fn settled_message(report: &craft_workflow::WorkflowReport, id: &str) -> String {
    let status = report
        .steps
        .iter()
        .find(|s| s.id == id)
        .unwrap()
        .status
        .clone();
    match status {
        StepStatus::Settled { final_message, .. } => final_message,
        other => format!("{other:?}"),
    }
}

#[tokio::test]
async fn a_passing_judge_settles_the_producer_chain() {
    let fake = Scripted::new();
    fake.succeed("write-tests");
    fake.succeed("implement");
    fake.queue(Attempt::new(
        "gate",
        StepConclusion::Success,
        r#"{"verdict":"pass"}"#,
    ));
    let report = run(JUDGE_PLAN, &fake).await;
    assert!(matches!(report.conclusion, RunConclusion::Completed));
    assert_eq!(report.exit_code, 0);
    assert_eq!(fake.call_count("implement"), 1, "one pass, one run");
}

#[tokio::test]
async fn a_failing_judge_requeues_the_producer_with_its_issues() {
    let fake = Scripted::new();
    fake.succeed("write-tests");
    fake.succeed("implement");
    fake.queue(Attempt::new(
        "gate",
        StepConclusion::Success,
        r#"{"verdict":"fail","issues":["the null guard is missing"]}"#,
    ));
    fake.queue(Attempt::new(
        "gate",
        StepConclusion::Success,
        r#"{"verdict":"pass"}"#,
    ));
    let report = run(JUDGE_PLAN, &fake).await;
    assert!(matches!(report.conclusion, RunConclusion::Completed));
    assert_eq!(fake.call_count("implement"), 2, "one repair round");
    let prompts = fake.prompts_for("implement");
    assert_eq!(prompts.len(), 2);
    assert!(
        prompts[1].contains("Judge feedback") && prompts[1].contains("null guard"),
        "the retry prompt carries the gate issues"
    );
}

#[tokio::test]
async fn the_feedback_budget_is_bounded_and_then_fails_closed() {
    let plan = JUDGE_PLAN.replace("max-feedback: 2", "max-feedback: 1");
    let fake = Scripted::new();
    fake.succeed("write-tests");
    fake.succeed("implement");
    fake.succeed("implement");
    fake.queue(Attempt::new(
        "gate",
        StepConclusion::Success,
        r#"{"verdict":"fail","issues":["still no null guard"]}"#,
    ));
    fake.queue(Attempt::new(
        "gate",
        StepConclusion::Success,
        r#"{"verdict":"fail","issues":["still no null guard"]}"#,
    ));
    let report = run(&plan, &fake).await;
    assert!(matches!(report.conclusion, RunConclusion::Failed { .. }));
    assert_eq!(report.exit_code, 1);
    assert_eq!(fake.call_count("implement"), 2, "one retry only");
    assert_eq!(fake.call_count("gate"), 2, "the gate runs once per round");
}

#[tokio::test]
async fn every_panel_seat_runs_with_its_own_marker() {
    let plan = JUDGE_PLAN.replace("      panel: 1", "      panel: 3");
    let fake = Scripted::new();
    fake.succeed("write-tests");
    fake.succeed("implement");
    for _ in 0..3 {
        fake.queue(Attempt::new(
            "gate",
            StepConclusion::Success,
            r#"{"verdict":"pass"}"#,
        ));
    }
    let report = run(&plan, &fake).await;
    assert!(matches!(report.conclusion, RunConclusion::Completed));
    assert_eq!(fake.call_count("gate"), 3);
    let prompts = fake.prompts_for("gate");
    assert_eq!(prompts.len(), 3);
    assert!(prompts[0].contains("[seat 1 of 3]"));
    assert!(prompts[2].contains("[seat 3 of 3]"));
}

#[tokio::test]
async fn a_warn_step_failure_lets_the_run_complete() {
    let plan = r#"---
name: w
steps:
  - id: flaky
    agent: worker
    on-failure: warn
    body: |
      may fail
  - id: next
    agent: worker
    body: |
      runs anyway
---"#;
    let fake = Scripted::new();
    fake.fail("flaky");
    let report = run(plan, &fake).await;
    assert!(matches!(report.conclusion, RunConclusion::Completed));
    assert!(fake.calls().contains(&"next".to_string()));
}

#[tokio::test]
async fn a_failed_judge_settles_the_producer_chain_failed() {
    let plan = JUDGE_PLAN.replace("max-feedback: 2", "max-feedback: 0");
    let fake = Scripted::new();
    fake.succeed("write-tests");
    fake.succeed("implement");
    fake.queue(Attempt::new(
        "gate",
        StepConclusion::Success,
        r#"{"verdict":"fail","issues":["never ran the suite"]}"#,
    ));
    let report = run(&plan, &fake).await;
    assert!(matches!(report.conclusion, RunConclusion::Failed { .. }));
    assert_eq!(fake.call_count("implement"), 1, "no retry without a budget");
    let message = settled_message(&report, "gate");
    assert!(
        message.contains("never ran the suite"),
        "the failed judge step carries the gate issues: {message}"
    );
}
