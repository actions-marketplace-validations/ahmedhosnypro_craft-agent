//! Judge verdicts: the seat contract and the panel merge.
//!
//! A seat's final message carries JSON `{"verdict":"pass"|"fail",
//! "issues":[...]}`. The merged verdict is the judge step's settled
//! final message, so downstream judges read it as a normal preview.

mod panel;

pub use panel::{
    MAX_ISSUES, MergedVerdict, SeatVerdict, merge_seats, merged_json, parse_seat, run_panel,
};

use panel::{MAX_ISSUE_CHARS, json_object};
use serde_json::Value;

use crate::error::WorkflowError;
use crate::truncate_chars;

/// A judge message that parses as a verdict JSON object — the engine
/// reads it to decide the repair path.
pub fn is_verdict_message(text: &str) -> bool {
    parse_merged(text).is_ok()
}

/// The verdict side of a settled judge message.
pub struct MergedVerdictRead {
    pub pass: bool,
    pub issues: Vec<String>,
}

pub fn parse_merged(text: &str) -> Result<MergedVerdictRead, WorkflowError> {
    let raw = json_object(text).ok_or_else(|| WorkflowError::JudgeGate {
        step: String::new(),
        detail: "judge verdict has no JSON object".to_string(),
    })?;
    let value: Value = serde_json::from_str(raw).map_err(|err| WorkflowError::JudgeGate {
        step: String::new(),
        detail: format!("judge verdict is not JSON: {err}"),
    })?;
    let verdict = value
        .get("verdict")
        .and_then(Value::as_str)
        .and_then(|text| match text {
            "pass" => Some(true),
            "fail" => Some(false),
            _ => None,
        })
        .ok_or_else(|| WorkflowError::JudgeGate {
            step: String::new(),
            detail: "judge verdict lacks a pass|fail `verdict` field".to_string(),
        })?;
    let mut issues = Vec::new();
    if let Some(list) = value.get("issues")
        && let Some(items) = list.as_array()
    {
        for item in items.iter().take(MAX_ISSUES) {
            if let Some(text) = item.as_str() {
                issues.push(truncate_chars(text, MAX_ISSUE_CHARS));
            }
        }
    }
    Ok(MergedVerdictRead {
        pass: verdict,
        issues,
    })
}

/// A settled judge verdict: a pass settles the step; a fail either
/// requeues the producing step with the issues (a bounded repair loop)
/// or settles the judge failed under its own on-failure policy once the
/// feedback budget is spent.
pub(crate) fn settle_verdict(
    state: &mut crate::state::EngineState,
    journal: &mut crate::journal::Journal,
    spec: &crate::plan::StepSpec,
    done: &crate::engine::Completion,
) -> Result<(), WorkflowError> {
    let merged = parse_merged(&done.outcome.final_message)?;
    if merged.pass {
        return crate::settle::settle_verdict_pass(state, journal, spec, done);
    }
    let Some(judge) = &spec.judge else {
        return Err(WorkflowError::Internal {
            detail: format!("judge step `{}` lost its judge block", spec.id),
        });
    };
    let producer = &spec.needs[0];
    let spent = state.feedback_attempts.get(producer).copied().unwrap_or(0);
    if judge.max_feedback > spent {
        state
            .feedback
            .insert(producer.clone(), merged.issues.clone());
        state.feedback_attempts.insert(producer.clone(), spent + 1);
        journal.record(crate::journal::JournalEvent::FeedbackRetried {
            seq: journal.next_seq(),
            id: producer.clone(),
            judge: spec.id.clone(),
            attempt: spent + 1,
            issues: merged.issues.clone(),
            usage: done.outcome.usage,
        })?;
        state.attempt_usage.remove(spec.id.as_str());
        state
            .slots
            .insert(producer.clone(), crate::state::Slot::Pending);
        state.ready.push_back(producer.clone());
        state
            .slots
            .insert(spec.id.clone(), crate::state::Slot::Pending);
        state.unmet.insert(spec.id.clone(), spec.needs.len() as u32);
        Ok(())
    } else {
        crate::settle::settle_verdict_failed(state, journal, spec, done)
    }
}
