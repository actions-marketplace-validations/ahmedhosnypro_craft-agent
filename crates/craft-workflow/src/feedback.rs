//! The judge-feedback replay fold: a `feedback_retried` journal row puts
//! both the producing step and its judge back to pending, charging the
//! usage the verdict attempt consumed.

use crate::error::WorkflowError;
use crate::state::{EngineState, Slot};
use crate::status::{StepStatus, StepUsage};

use crate::resume::FoldSite;

/// One `feedback_retried` row being replayed: the requeue it records and
/// the usage the verdict attempt charged.
pub(crate) struct FeedbackRow<'a> {
    pub(crate) producer: &'a str,
    pub(crate) judge: &'a str,
    pub(crate) attempt: u32,
    pub(crate) issues: &'a [String],
    pub(crate) usage: &'a StepUsage,
}

pub(crate) fn fold_feedback(
    site: &FoldSite<'_>,
    state: &mut EngineState,
    row: FeedbackRow<'_>,
) -> Result<(), WorkflowError> {
    site.require_step(row.producer)?;
    site.require_step(row.judge)?;
    let settled = matches!(
        state.slots.get(row.producer),
        Some(Slot::Done(StepStatus::Settled { conclusion, .. })) if conclusion.is_success()
    );
    if !settled {
        return Err(site.corrupt(format!(
            "feedback requeued step `{}` before it settled",
            row.producer
        )));
    }
    if !matches!(state.slots.get(row.judge), Some(Slot::Running)) {
        return Err(site.corrupt(format!(
            "judge `{}` requeued `{}` while not running",
            row.judge, row.producer
        )));
    }
    state.total_usage = state.total_usage.saturating_add(*row.usage);
    state.pool.settle(*row.usage);
    state
        .feedback
        .insert(row.producer.to_string(), row.issues.to_vec());
    let entry = state
        .feedback_attempts
        .entry(row.producer.to_string())
        .or_insert(0);
    if row.attempt <= *entry {
        return Err(site.corrupt(format!(
            "step `{}` feedback attempt regresses",
            row.producer
        )));
    }
    *entry = row.attempt;
    state.slots.insert(row.producer.to_string(), Slot::Pending);
    state.slots.insert(row.judge.to_string(), Slot::Pending);
    state.unmet.insert(
        row.judge.to_string(),
        site.plan
            .step(row.judge)
            .map(|spec| spec.needs.len() as u32)
            .unwrap_or(0),
    );
    Ok(())
}
