//! New plan-field corpus: `kind`, the judge block, `deny-read`/
//! `deny-write`, and `on-failure: warn` — every rule fires with its
//! typed error, and the legal shapes parse.
#![allow(clippy::unwrap_used)]

use craft_workflow::{OnFailure, WorkflowError, WorkflowPlan};

fn ok(text: &str) -> WorkflowPlan {
    WorkflowPlan::parse("t.md", text).unwrap()
}

fn err(text: &str) -> WorkflowError {
    WorkflowPlan::parse("t.md", text).unwrap_err()
}

const BASE: &str =
    "---\nname: t\nsteps:\n  - id: a\n    agent: worker\n    body: |\n      do a\n---\n";

fn plan_with(step: &str) -> String {
    format!("---\nname: t\nsteps:\n{step}---\n")
}

#[test]
fn a_step_defaults_to_agent_kind() {
    let plan = ok(BASE);
    assert!(plan.steps[0].judge.is_none());
    assert!(!plan.steps[0].is_judge());
    assert!(plan.steps[0].deny_read.is_empty());
    assert!(plan.steps[0].deny_write.is_empty());
    assert_eq!(plan.steps[0].on_failure, OnFailure::Stop);
}

#[test]
fn a_judge_step_parses_its_block() {
    let plan = ok(&plan_with(
        "  - id: make\n    agent: worker\n    body: |\n      build\n  - id: gate\n    \
         kind: judge\n    agent: reviewer\n    needs: [make]\n    judge:\n      panel: 3\n      \
         merge: majority\n      max-feedback: 2\n    body: |\n      judge\n",
    ));
    let judge = plan.steps[1].judge.as_ref().unwrap();
    assert_eq!(judge.panel, 3);
    assert_eq!(judge.merge, craft_workflow::JudgeMerge::Majority);
    assert_eq!(judge.max_feedback, 2);
    assert!(plan.steps[1].is_judge());
}

#[test]
fn a_panel_over_the_cap_is_rejected() {
    let text = plan_with(
        "  - id: make\n    agent: worker\n    body: |\n      build\n  - id: gate\n    \
         kind: judge\n    agent: reviewer\n    needs: [make]\n    judge:\n      panel: 9\n    \
         body: |\n      judge\n",
    );
    assert!(matches!(err(&text), WorkflowError::BadJudge { .. }));
}

#[test]
fn a_judge_needs_at_least_one_upstream() {
    let text = plan_with(
        "  - id: gate\n    kind: judge\n    agent: reviewer\n    judge:\n      panel: 1\n    \
         body: |\n      judge\n",
    );
    assert!(matches!(err(&text), WorkflowError::Parse { .. }));
}

#[test]
fn a_judge_with_feedback_targets_exactly_one_producer() {
    let text = plan_with(
        "  - id: make\n    agent: worker\n    body: |\n      build\n  - id: shape\n    \
         agent: worker\n    body: |\n      shape\n  - id: gate\n    kind: judge\n    \
         agent: reviewer\n    needs: [make, shape]\n    judge:\n      max-feedback: 2\n    \
         body: |\n      judge\n",
    );
    assert!(matches!(err(&text), WorkflowError::Parse { .. }));
}

#[test]
fn judge_feedback_may_not_target_another_judge() {
    let text = plan_with(
        "  - id: make\n    agent: worker\n    body: |\n      build\n  - id: inner\n    \
         kind: judge\n    agent: reviewer\n    needs: [make]\n    judge:\n      max-feedback: 1\n    \
         body: |\n      judge\n  - id: outer\n    kind: judge\n    agent: reviewer\n    \
         needs: [inner]\n    judge:\n      max-feedback: 1\n    body: |\n      judge again\n",
    );
    assert!(matches!(err(&text), WorkflowError::Parse { .. }));
}

#[test]
fn deny_lists_parse_and_are_glob_checked() {
    let plan = ok(&plan_with(
        "  - id: a\n    agent: worker\n    deny-read:\n      - tests/**\n    \
         deny-write:\n      - secrets/**\n    body: |\n      do a\n",
    ));
    assert_eq!(plan.steps[0].deny_read, ["tests/**"]);
    assert_eq!(plan.steps[0].deny_write, ["secrets/**"]);
}

#[test]
fn a_deny_glob_must_be_workspace_relative() {
    let text = plan_with(
        "  - id: a\n    agent: worker\n    deny-read:\n      - /etc/passwd\n    body: |\n      do a\n",
    );
    assert!(matches!(err(&text), WorkflowError::BadScope { .. }));
    let traversal = plan_with(
        "  - id: a\n    agent: worker\n    deny-read:\n      - ../**\n    body: |\n      do a\n",
    );
    assert!(matches!(err(&traversal), WorkflowError::BadScope { .. }));
}

#[test]
fn on_failure_warn_parses_and_continues() {
    let plan = ok(&plan_with(
        "  - id: a\n    agent: worker\n    on-failure: warn\n    body: |\n      do a\n",
    ));
    assert_eq!(plan.steps[0].on_failure, OnFailure::Warn);
    assert!(plan.steps[0].on_failure.continues_on_failure());
    let bad =
        plan_with("  - id: a\n    agent: worker\n    on-failure: maybe\n    body: |\n      do a\n");
    assert!(err(&bad).to_string().contains("on-failure"));
}

#[test]
fn parses_the_sim_tdd_isolation_fixture() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../sim/workflows/tdd-isolation-01.md");
    let plan = WorkflowPlan::parse_file(&fixture).unwrap();
    assert_eq!(plan.name, "tdd-isolation-01");
    let ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["write-tests", "implement", "gate"]);
    let implement = &plan.steps[1];
    assert_eq!(implement.agent, "blind-implementer");
    assert_eq!(implement.deny_read, ["tests/**"]);
    let gate = &plan.steps[2];
    assert!(gate.is_judge());
    assert_eq!(gate.needs, ["implement"]);
    let judge = gate.judge.as_ref().unwrap();
    assert_eq!(judge.panel, 1);
    assert_eq!(judge.merge, craft_workflow::JudgeMerge::Unanimous);
    assert_eq!(judge.max_feedback, 2);
}

#[test]
fn an_unknown_judge_block_key_is_rejected() {
    let text = plan_with(
        "  - id: make\n    agent: worker\n    body: |\n      build\n  - id: gate\n    \
         kind: judge\n    agent: reviewer\n    needs: [make]\n    judge:\n      panel: 1\n      \
         on-failure: stop\n    body: |\n      judge\n",
    );
    let err = err(&text);
    assert!(
        matches!(err, WorkflowError::UnknownField { .. }),
        "expected UnknownField, got {err:?}"
    );
    assert!(err.to_string().contains("judge"), "{err}");
}
