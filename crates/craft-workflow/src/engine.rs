//! The superstep scheduler: admission under the pool caps, execution via
//! the StepExecutor seam, settlement into the journal, and the exactly-once
//! terminal settle.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use futures::stream::{FuturesUnordered, StreamExt};

use crate::assemble::{assemble_step, previews_for};
use crate::error::WorkflowError;
use crate::journal::{Journal, JournalEvent};
use crate::judge;
use crate::plan::{ArgValue, StepSpec, WorkflowPlan};
use crate::status::{
    RunConclusion, SkipReason, StepConclusion, StepStatus, StepUsage, WorkflowReport,
};
use crate::truncate_chars;

use crate::state::{EngineState, Slot};
use crate::verify::step_reports;

pub const SCHEDULER_ITERATION_CAP: usize = 1_024;

/// One step is one full agent run; the engine owns no provider or runtime.
pub trait StepExecutor {
    fn execute(
        &self,
        step: crate::assemble::StepInputs,
    ) -> impl Future<Output = Result<StepOutcome, WorkflowError>> + Send;
}

/// What one step attempt returned: terminal conclusion, usage, final message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    pub conclusion: StepConclusion,
    pub usage: StepUsage,
    pub final_message: String,
}

type Flights<'a> = FuturesUnordered<
    Pin<Box<dyn Future<Output = (String, Result<StepOutcome, WorkflowError>)> + Send + 'a>>,
>;

/// Runs the loop to terminal settle: admit ready steps under the caps,
/// await completions in arrival order (no wave barrier), settle each into
/// the journal, and finish exactly once.
pub(crate) async fn drive<E: StepExecutor + Sync>(
    plan: &WorkflowPlan,
    args: &BTreeMap<String, ArgValue>,
    run_id: &str,
    executor: &E,
    journal: &mut Journal,
    mut state: EngineState,
) -> Result<WorkflowReport, WorkflowError> {
    let mut in_flight: Flights<'_> = FuturesUnordered::new();
    let mut iterations = 0usize;
    loop {
        iterations += 1;
        if iterations > SCHEDULER_ITERATION_CAP {
            return Err(WorkflowError::SchedulerCap {
                cap: SCHEDULER_ITERATION_CAP,
            });
        }
        admit(plan, args, &mut state, journal, executor, &mut in_flight)?;
        if in_flight.is_empty() {
            break;
        }
        let Some((id, result)) = in_flight.next().await else {
            break;
        };
        let outcome = result.map_err(|err| WorkflowError::StepLaunch {
            step: id.clone(),
            detail: truncate_chars(&err.to_string(), 400),
        })?;
        let done = Completion {
            id,
            outcome,
            usage: StepUsage::default(),
        };
        let spec = spec_of(plan, &done.id)?;
        settle(SettleStep { plan, spec }, &mut state, journal, done)?;
    }
    finish(plan, &mut state, journal, run_id)
}

fn spec_of<'a>(
    plan: &'a WorkflowPlan,
    id: &str,
) -> Result<&'a crate::plan::StepSpec, WorkflowError> {
    plan.step(id).ok_or_else(|| WorkflowError::Internal {
        detail: format!("settling step `{id}` is not in the plan"),
    })
}

fn admit<'a, E: StepExecutor + Sync>(
    plan: &'a WorkflowPlan,
    args: &'a BTreeMap<String, ArgValue>,
    state: &mut EngineState,
    journal: &mut Journal,
    executor: &'a E,
    in_flight: &mut Flights<'a>,
) -> Result<(), WorkflowError> {
    let slots = plan.limits.max_concurrent_steps as usize;
    while !state.stopping && in_flight.len() < slots {
        let Some(id) = state.ready.pop_front() else {
            break;
        };
        let Some(spec) = plan.step(&id) else {
            return Err(WorkflowError::Internal {
                detail: format!("ready step `{id}` is not in the plan"),
            });
        };
        if state.pool.check(spec).is_err() {
            state.budget_refused = true;
            state.stopping = true;
            continue;
        }
        let previews = previews_for(spec, |need| state.settled_preview(need));
        let inputs = assemble_step(plan, args, spec, &previews, &state.feedback_text(&id));
        journal.record(JournalEvent::StepStarted {
            seq: journal.next_seq(),
            id: id.clone(),
        })?;
        if state.counted.insert(id.clone()) {
            state.pool.start();
        }
        state.pool.reserve(u64::from(spec.max_turns));
        state.input_hashes.insert(id.clone(), inputs.input_hash());
        state.slots.insert(id.clone(), Slot::Running);
        let task = id.clone();
        let call: Pin<Box<dyn Future<Output = Result<StepOutcome, WorkflowError>> + Send + 'a>> =
            match &spec.judge {
                Some(judge) if spec.is_judge() => {
                    Box::pin(judge::run_panel(executor, inputs, judge))
                }
                _ => Box::pin(executor.execute(inputs)),
            };
        in_flight.push(Box::pin(async move {
            let result = call.await;
            (task, result)
        }));
    }
    Ok(())
}

/// One attempt handed back by a flight: the step, its terminal outcome,
/// and the step's usage total across attempts so far.
pub(crate) struct Completion {
    pub(crate) id: String,
    pub(crate) outcome: StepOutcome,
    pub(crate) usage: StepUsage,
}

/// One step in its plan: what an attempt settles against.
pub(crate) struct SettleStep<'a> {
    pub(crate) plan: &'a WorkflowPlan,
    pub(crate) spec: &'a StepSpec,
}

/// A passed verdict settles success and releases dependants; a failed
/// verdict settles under the judge's policy. Both live in `settle`.
pub(crate) fn settle(
    step: SettleStep<'_>,
    state: &mut EngineState,
    journal: &mut Journal,
    mut done: Completion,
) -> Result<(), WorkflowError> {
    done.usage = crate::settle::charge_attempt(state, step.spec, &done);
    if step.spec.is_judge() && judge::is_verdict_message(&done.outcome.final_message) {
        return judge::settle_verdict(state, journal, step.spec, &done);
    }
    if crate::settle::queue_retry(step.plan, state, journal, &done)? {
        Ok(())
    } else {
        crate::settle::settle_done(state, journal, step.spec, &done)
    }
}

fn finish(
    plan: &WorkflowPlan,
    state: &mut EngineState,
    journal: &mut Journal,
    run_id: &str,
) -> Result<WorkflowReport, WorkflowError> {
    for spec in &plan.steps {
        match state.slots.get(&spec.id) {
            Some(Slot::Pending) | None => {
                let reason = skip_reason(state, spec);
                journal.record(JournalEvent::StepSkipped {
                    seq: journal.next_seq(),
                    id: spec.id.clone(),
                    reason: reason.clone(),
                })?;
                state
                    .slots
                    .insert(spec.id.clone(), Slot::Done(StepStatus::Skipped { reason }));
            }
            Some(Slot::Running) => {
                return Err(WorkflowError::Internal {
                    detail: format!("step `{}` is still running at the terminal settle", spec.id),
                });
            }
            Some(Slot::Done(_)) => {}
        }
    }
    let (conclusion, failed_step) = match &state.first_failure {
        Some((step, failed)) => (
            RunConclusion::Failed {
                step: step.clone(),
                conclusion: *failed,
            },
            Some(step.clone()),
        ),
        None if state.budget_refused => (RunConclusion::BudgetExhausted, None),
        None => (RunConclusion::Completed, None),
    };
    let exit_code = conclusion.exit_code();
    journal.record(JournalEvent::RunFinished {
        seq: journal.next_seq(),
        conclusion: conclusion.clone(),
        failed_step: failed_step.clone(),
        total_usage: state.total_usage,
    })?;
    let steps = step_reports(plan, state);
    Ok(WorkflowReport {
        run_id: run_id.to_string(),
        name: plan.name.clone(),
        conclusion,
        exit_code,
        failed_step,
        steps,
        total_usage: state.total_usage,
    })
}

/// Names the dependency that broke, so a skip is visible, never silent.
fn skip_reason(state: &EngineState, spec: &StepSpec) -> SkipReason {
    for need in &spec.needs {
        let broken = match state.slots.get(need) {
            Some(Slot::Done(StepStatus::Settled { conclusion, .. })) => !conclusion.is_success(),
            Some(Slot::Done(StepStatus::Skipped { .. })) => true,
            _ => false,
        };
        if broken {
            return SkipReason::DependencyFailed { dep: need.clone() };
        }
    }
    SkipReason::Unreachable
}
