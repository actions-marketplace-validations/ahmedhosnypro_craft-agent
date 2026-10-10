//! Terminal-row verification: a finished journal must replay to the same
//! conclusion the folded step table implies, or the resume diverges.

use crate::error::WorkflowError;
use crate::plan::WorkflowPlan;
use crate::resume::Folded;
use crate::state::{EngineState, Slot};
use crate::status::{RunConclusion, SkipReason, StepReport, StepStatus, StepUsage, WorkflowReport};

/// The terminal journal row a resume must agree with.
pub(crate) struct FinishedRun<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) conclusion: &'a RunConclusion,
    pub(crate) failed_step: &'a Option<String>,
    pub(crate) total_usage: &'a StepUsage,
}

pub(crate) fn verify_finished(
    plan: &WorkflowPlan,
    state: &EngineState,
    path: &str,
    run: &FinishedRun<'_>,
) -> Result<(String, Folded), WorkflowError> {
    verify_settled(plan, state, path)?;
    check_conclusion(state, run)?;
    check_failed_step(state, run)?;
    check_total_usage(state, run)?;
    let report = WorkflowReport {
        run_id: run.run_id.to_string(),
        name: plan.name.clone(),
        conclusion: run.conclusion.clone(),
        exit_code: run.conclusion.exit_code(),
        failed_step: run.failed_step.clone(),
        steps: step_reports(plan, state),
        total_usage: *run.total_usage,
    };
    Ok((run.run_id.to_string(), Folded::Finished(report)))
}

/// Every step must have settled before the terminal row.
fn verify_settled(
    plan: &WorkflowPlan,
    state: &EngineState,
    path: &str,
) -> Result<(), WorkflowError> {
    for spec in &plan.steps {
        if !matches!(state.slots.get(&spec.id), Some(Slot::Done(_))) {
            return Err(WorkflowError::JournalParse {
                path: path.to_string(),
                line: 0,
                detail: format!("run_finished before step `{}` settled", spec.id),
            });
        }
    }
    Ok(())
}

fn check_conclusion(state: &EngineState, run: &FinishedRun<'_>) -> Result<(), WorkflowError> {
    let recomputed = fold_conclusion(state);
    if &recomputed != run.conclusion {
        return Err(WorkflowError::Divergence {
            detail: format!(
                "run_finished conclusion does not match the folded steps \
                 (journal `{:?}`, folded `{recomputed:?}`)",
                run.conclusion
            ),
        });
    }
    Ok(())
}

fn check_failed_step(state: &EngineState, run: &FinishedRun<'_>) -> Result<(), WorkflowError> {
    let expected = state.first_failure.as_ref().map(|(step, _)| step.clone());
    if run.failed_step != &expected {
        return Err(WorkflowError::Divergence {
            detail: "run_finished failed_step does not match the folded steps".to_string(),
        });
    }
    Ok(())
}

fn check_total_usage(state: &EngineState, run: &FinishedRun<'_>) -> Result<(), WorkflowError> {
    if run.total_usage != &state.total_usage {
        return Err(WorkflowError::Divergence {
            detail: "run_finished total_usage does not match the folded steps".to_string(),
        });
    }
    Ok(())
}

fn fold_conclusion(state: &EngineState) -> RunConclusion {
    let has_skip = state
        .slots
        .values()
        .any(|slot| matches!(slot, Slot::Done(StepStatus::Skipped { .. })));
    match &state.first_failure {
        Some((step, failed)) => RunConclusion::Failed {
            step: step.clone(),
            conclusion: *failed,
        },
        None if has_skip => RunConclusion::BudgetExhausted,
        None => RunConclusion::Completed,
    }
}

pub(crate) fn step_reports(plan: &WorkflowPlan, state: &EngineState) -> Vec<StepReport> {
    plan.steps
        .iter()
        .map(|spec| StepReport {
            id: spec.id.clone(),
            status: match state.slots.get(&spec.id) {
                Some(Slot::Done(status)) => status.clone(),
                _ => StepStatus::Skipped {
                    reason: SkipReason::Unreachable,
                },
            },
        })
        .collect()
}
