//! Settlement: what the engine does with one completed attempt — charge
//! it to the pool, requeue under the retry cap, or settle it exactly once
//! into the journal and release its dependants.

use crate::engine::{Completion, StepOutcome};
use crate::error::WorkflowError;
use crate::journal::{Journal, JournalEvent};
use crate::plan::{OnFailure, PREVIEW_CHARS, StepSpec, WorkflowPlan};
use crate::state::{EngineState, Slot};
use crate::status::{StepConclusion, StepStatus, StepUsage};
use crate::truncate_chars;

/// A passed verdict settles success and releases dependants.
pub(crate) fn settle_verdict_pass(
    state: &mut EngineState,
    journal: &mut Journal,
    spec: &StepSpec,
    done: &Completion,
) -> Result<(), WorkflowError> {
    let settled = settled_completion(done, StepConclusion::Success);
    settle_done(state, journal, spec, &settled)
}

/// A verdict fail with the feedback budget spent settles failed under
/// the judge's on-failure policy.
pub(crate) fn settle_verdict_failed(
    state: &mut EngineState,
    journal: &mut Journal,
    spec: &StepSpec,
    done: &Completion,
) -> Result<(), WorkflowError> {
    let settled = settled_completion(done, StepConclusion::Error);
    settle_done(state, journal, spec, &settled)
}

/// The completion as it enters the journal, with the verdict's
/// conclusion stamped on the outcome.
fn settled_completion(done: &Completion, conclusion: StepConclusion) -> Completion {
    Completion {
        id: done.id.clone(),
        outcome: StepOutcome {
            conclusion,
            usage: done.outcome.usage,
            final_message: done.outcome.final_message.clone(),
        },
        usage: done.usage,
    }
}

/// A failed attempt requeues the step while retries and pool budget
/// remain; false means the outcome must settle instead.
pub(crate) fn queue_retry(
    plan: &WorkflowPlan,
    state: &mut EngineState,
    journal: &mut Journal,
    done: &Completion,
) -> Result<bool, WorkflowError> {
    let attempt = *state.attempts.get(&done.id).unwrap_or(&0);
    let Some(spec) = plan.step(&done.id) else {
        return Ok(false);
    };
    let max_attempts = 1u32 + spec.retry.min(plan.limits.max_retries_per_step);
    let retries_left = attempt + 1 < max_attempts;
    let failed = !done.outcome.conclusion.is_success();
    let allowed = failed && retries_left && !state.stopping && state.pool.check(spec).is_ok();
    if allowed {
        state.attempts.insert(done.id.clone(), attempt + 1);
        journal.record(JournalEvent::StepRetried {
            seq: journal.next_seq(),
            id: done.id.clone(),
            attempt: attempt + 1,
        })?;
        state.slots.insert(done.id.clone(), Slot::Pending);
        state.ready.push_back(done.id.clone());
    }
    Ok(allowed)
}

/// Every attempt is charged to the pool as it lands; the settled row
/// carries the step's total across attempts.
pub(crate) fn charge_attempt(
    state: &mut EngineState,
    spec: &StepSpec,
    done: &Completion,
) -> StepUsage {
    state.total_usage = state.total_usage.saturating_add(done.outcome.usage);
    state.pool.release(u64::from(spec.max_turns));
    state.pool.settle(done.outcome.usage);
    let entry = state.attempt_usage.entry(done.id.clone()).or_default();
    *entry = entry.saturating_add(done.outcome.usage);
    *entry
}

pub(crate) fn settle_done(
    state: &mut EngineState,
    journal: &mut Journal,
    spec: &StepSpec,
    done: &Completion,
) -> Result<(), WorkflowError> {
    let preview = truncate_chars(&done.outcome.final_message, PREVIEW_CHARS);
    let Some(input_hash) = state.input_hashes.get(&done.id) else {
        return Err(WorkflowError::Internal {
            detail: format!("step `{}` settled without an input hash", done.id),
        });
    };
    journal.record(JournalEvent::StepFinished {
        seq: journal.next_seq(),
        id: done.id.clone(),
        input_hash: input_hash.clone(),
        conclusion: done.outcome.conclusion,
        usage: done.usage,
        final_message: preview.clone(),
    })?;
    state.attempt_usage.remove(&done.id);
    state.slots.insert(
        done.id.clone(),
        Slot::Done(StepStatus::Settled {
            conclusion: done.outcome.conclusion,
            usage: done.usage,
            final_message: preview,
        }),
    );
    if done.outcome.conclusion.is_success() {
        unlock_dependants(state, spec);
    } else {
        note_failure(state, &done.id, spec, done.outcome.conclusion);
    }
    Ok(())
}

/// A successful settle releases the dependants whose needs are now met.
fn unlock_dependants(state: &mut EngineState, spec: &StepSpec) {
    for dependent in &spec.dependants {
        let Some(unmet) = state.unmet.get_mut(dependent) else {
            continue;
        };
        *unmet = unmet.saturating_sub(1);
        let pending = matches!(state.slots.get(dependent), Some(Slot::Pending));
        if *unmet == 0 && pending {
            state.ready.push_back(dependent.clone());
        }
    }
}

fn note_failure(state: &mut EngineState, id: &str, spec: &StepSpec, conclusion: StepConclusion) {
    if spec.on_failure != OnFailure::Warn && state.first_failure.is_none() {
        state.first_failure = Some((id.to_string(), conclusion));
    }
    if spec.on_failure == OnFailure::Stop {
        state.stopping = true;
    }
}
