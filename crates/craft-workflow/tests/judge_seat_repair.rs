//! Seat verdict-repair corpus: one corrective round for a seat whose
//! final message the verdict parser rejected, then fail-closed.
#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::sync::Mutex;

use craft_workflow::{
    Journal, RunConclusion, StepConclusion, StepExecutor, StepInputs, StepOutcome, StepStatus,
    StepUsage, WorkflowError, WorkflowPlan, judge::MAX_SEAT_REPAIRS, run_workflow,
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
    outcomes: Mutex<std::collections::BTreeMap<String, VecDeque<(StepConclusion, String)>>>,
    calls: Mutex<Vec<String>>,
    prompts: Mutex<std::collections::BTreeMap<String, Vec<String>>>,
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

    fn call_count(&self, id: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| *seen == id)
            .count()
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
                turns: 1,
                input_tokens: 10,
                output_tokens: 5,
                reasoning_tokens: 0,
            },
            final_message: message,
        })
    }
}

const JUDGE_PLAN: &str = r#"---
name: repair
steps:
  - id: produce
    agent: worker
    body: |
      do the work
  - id: gate
    kind: judge
    agent: reviewer
    needs: [produce]
    judge:
      panel: 1
      merge: unanimous
      max-feedback: 0
    body: |
      judge the work
---"#;

fn plan(text: &str) -> WorkflowPlan {
    WorkflowPlan::parse("t.md", text).unwrap()
}

async fn run(text: &str, fake: &Scripted) -> craft_workflow::WorkflowReport {
    let dir = temp_dir("seat-repair");
    let mut journal = Journal::open(&journal_path(&dir)).unwrap();
    run_workflow(&plan(text), &no_args(), fake, &mut journal)
        .await
        .unwrap()
}

async fn run_err(text: &str, fake: &Scripted) -> WorkflowError {
    let dir = temp_dir("seat-repair-err");
    let mut journal = Journal::open(&journal_path(&dir)).unwrap();
    run_workflow(&plan(text), &no_args(), fake, &mut journal)
        .await
        .unwrap_err()
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
async fn a_seat_repairs_until_the_verdict_parses() {
    let fake = Scripted::new();
    fake.succeed("produce");
    for _ in 0..3 {
        fake.queue(Attempt::new(
            "gate",
            StepConclusion::Success,
            r#"{verdict: "pass", issues: []}"#,
        ));
    }
    fake.queue(Attempt::new(
        "gate",
        StepConclusion::Success,
        r#"{"verdict":"pass","issues":[]}"#,
    ));
    let report = run(JUDGE_PLAN, &fake).await;
    assert!(matches!(report.conclusion, RunConclusion::Completed));
    assert_eq!(fake.call_count("gate"), 4, "repairs until it parses");
    let prompts = fake.prompts_for("gate");
    assert!(prompts[1].contains("Verdict format repair"));
    assert!(
        prompts[1].contains("parser rejected") && prompts[1].contains("key must be a string"),
        "the repair prompt carries the parser's complaint: {}",
        &prompts[1][prompts[1].len().saturating_sub(200)..]
    );
}

#[tokio::test]
async fn a_seat_that_never_repairs_still_fails_closed() {
    let fake = Scripted::new();
    fake.succeed("produce");
    for _ in 0..=MAX_SEAT_REPAIRS {
        fake.queue(Attempt::new(
            "gate",
            StepConclusion::Success,
            "I judged the work and it looks fine to me.",
        ));
    }
    let error = run_err(JUDGE_PLAN, &fake).await;
    assert!(
        error.to_string().contains("no JSON object"),
        "the typed judge failure survives: {error}"
    );
    assert_eq!(
        fake.call_count("gate"),
        1 + MAX_SEAT_REPAIRS as usize,
        "bounded by the repair cap"
    );
}

#[tokio::test]
async fn only_the_unparsed_seats_of_a_panel_repair() {
    let plan = JUDGE_PLAN.replace("      panel: 1", "      panel: 3");
    let fake = Scripted::new();
    fake.succeed("produce");
    for index in 0..3 {
        fake.queue(Attempt::new(
            "gate",
            StepConclusion::Success,
            if index == 1 {
                r#"{verdict: "fail", issues: ["bad key quoting"]}"#
            } else {
                r#"{"verdict":"pass","issues":[]}"#
            },
        ));
    }
    fake.queue(Attempt::new(
        "gate",
        StepConclusion::Success,
        r#"{"verdict":"pass","issues":[]}"#,
    ));
    let report = run(&plan, &fake).await;
    assert!(matches!(report.conclusion, RunConclusion::Completed));
    assert_eq!(fake.call_count("gate"), 4, "one seat repairs, two do not");
}

#[tokio::test]
async fn a_seat_that_errored_is_not_repaired() {
    let fake = Scripted::new();
    fake.succeed("produce");
    fake.queue(Attempt::new(
        "gate",
        StepConclusion::MaxTurns,
        "timed out mid-verdict",
    ));
    let report = run(JUDGE_PLAN, &fake).await;
    assert!(matches!(report.conclusion, RunConclusion::Failed { .. }));
    assert_eq!(report.exit_code, 1);
    assert_eq!(
        fake.call_count("gate"),
        1,
        "no repair round for a dead seat"
    );
    let message = settled_message(&report, "gate");
    assert!(message.contains("timed out mid-verdict"), "{message}");
}
