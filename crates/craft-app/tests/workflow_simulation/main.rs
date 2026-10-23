//! The judged-workflow simulation: the same 16-step architecture as the
//! real adoption scan — collect, parallel clone and classify waves,
//! per-PR grandchildren, panel gates with a repaired seat, appeals,
//! draft waves, committers, rollup — driven end to end over a scripted
//! provider with local fixtures and no network. This runs in seconds
//! and asserts the same five proofs the sim's assert-workflow.sh checks
//! on a real run: spawn tools live, parallelism reached, every child
//! settled, gates settled, and the run concluded.
#![allow(clippy::expect_used)]

mod rig;
mod scripted;

use std::sync::Arc;

use craft_app::workflow_cmd::run_or_resume;
use craft_app::workflow_exec::{StepMount, WorkflowExecutor};
use craft_workflow::plan;
use craft_workflow::{Journal, WorkflowPlan};

use rig::{Rig, rig as rig_for};
use scripted::SimProvider;

/// The full 16-step simulation over the driver, returning the report.
fn run_simulation(rig: &Rig) -> Result<craft_workflow::WorkflowReport, Box<dyn std::error::Error>> {
    let plan_spec = WorkflowPlan::parse_file(
        &rig.workspace
            .join(".craft-agent")
            .join("workflows")
            .join("sim-adopt.md"),
    )?;
    let args = plan::resolve_args(&plan_spec, &std::collections::BTreeMap::new())?;
    let mount: StepMount<SimProvider> = {
        let provider = rig.provider.clone();
        Arc::new(move |_model| Ok(provider.clone()))
    };
    let mut executor = WorkflowExecutor::new(
        mount,
        "sim-model".to_string(),
        rig.workspace.clone(),
        rig.run_dir.join("sessions"),
        Some(1_000_000),
        None,
        Some(3),
        rig.agents.clone(),
    );
    executor = executor
        .with_subagents(Some(rig.hub.clone()))
        .with_sink(rig.sink.clone());
    let mut journal = Journal::open(&rig.run_dir.join("journal.jsonl"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let report = runtime.block_on(run_or_resume(&plan_spec, &args, &executor, &mut journal))?;
    Ok(report)
}

#[test]
fn the_judged_workflow_simulation_settles_every_step() {
    let rig = rig_for("full");
    let report = run_simulation(&rig).expect("the simulated workflow settles");
    assert_eq!(report.exit_code, 0, "the simulation concludes success");
    assert_eq!(report.steps.len(), 16, "all sixteen steps settle");
    for step in &report.steps {
        match &step.status {
            craft_workflow::status::StepStatus::Settled { conclusion, .. } => {
                assert!(conclusion.is_success(), "step {} succeeds", step.id)
            }
            skipped => panic!("step {} skips: {skipped:?}", step.id),
        }
    }
}

#[test]
fn the_simulation_proves_spawn_tools_live_and_parallel() {
    let rig = rig_for("spawns");
    let report = run_simulation(&rig).expect("the simulated workflow settles");
    // Every step and every child made model calls through the provider.
    assert!(
        rig.provider.calls() > 16,
        "every step and every child made model calls: {}",
        rig.provider.calls()
    );
    // The run's event stream carries one launch and one settle per child.
    let (started, finished, peak) = spawn_counts(&rig.events);
    assert!(
        started >= 19,
        "the waves launch every child: clones + batches + grandchildren +\
         drafts + rollup = {started}"
    );
    assert_eq!(started, finished, "no child is left dangling");
    assert!(
        peak >= 3,
        "children overlap: the peak concurrent fan-out was {peak}"
    );
    // A judge seat answered in prose, forcing the engine's repair loop.
    assert!(
        rig.provider.malformed() >= 1,
        "a judge seat answered in prose, forcing the repair loop"
    );
    // The local artifacts the waves produce exist.
    assert!(
        rig.workspace
            .join("run-ledger/clone/app-x/outcome.md")
            .is_file()
    );
    assert!(
        rig.workspace
            .join("run-ledger/rollup/state-body.md")
            .is_file()
    );
    assert_eq!(report.exit_code, 0);
}

#[test]
fn a_seat_repair_loop_lands_the_verdict() {
    // The simulation's seat 1 answers in prose; the engine must repair it
    // until the verdict parses and the gate still settles.
    let rig = rig_for("repair");
    let report = run_simulation(&rig).expect("the repaired gate settles");
    assert_eq!(report.exit_code, 0);
    assert!(rig.provider.malformed() >= 1);
    assert!(
        report.failed_step.is_none(),
        "no step fails despite the malformed seat"
    );
}

/// Reads the drained event stream: (children launched, children settled,
/// peak concurrent children) over the whole run.
fn spawn_counts(
    events: &std::sync::mpsc::Receiver<Option<craft_runtime::RuntimeEvent>>,
) -> (usize, usize, usize) {
    let mut started = 0usize;
    let mut finished = 0usize;
    let mut alive: usize = 0;
    let mut peak = 0usize;
    while let Ok(event) = events.try_recv() {
        match event {
            Some(craft_runtime::RuntimeEvent::SubagentStarted { .. }) => {
                started += 1;
                alive += 1;
                if alive > peak {
                    peak = alive;
                }
            }
            Some(craft_runtime::RuntimeEvent::SubagentFinished { .. }) => {
                finished += 1;
                alive = alive.saturating_sub(1);
            }
            _ => {}
        }
    }
    (started, finished, peak)
}

#[test]
fn the_simulation_fails_closed_when_the_hub_cannot_mount() {
    // A hostile hub setting must abort the run rather than silently
    // degrade to serial work. The cap parser is the seam that refuses,
    // so the test asserts its typed error (no env mutation needed).
    let hostile = craft_runtime::SubagentCaps::resolve(
        Some(String::from("4")),
        Some(String::from("banana")),
        Some(String::from("4")),
    );
    assert!(
        hostile.is_err(),
        "a hostile cap knob is a typed error, never a silent default"
    );
    let detail = hostile.expect_err("hostile knob errors");
    assert!(
        detail.contains("banana"),
        "the error names the hostile value: {detail}"
    );
}
