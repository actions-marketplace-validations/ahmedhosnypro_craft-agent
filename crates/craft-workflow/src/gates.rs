//! Judge and scope field conversion plus the cross-field gate checks
//! that make them safe: type-shape parsing, caps, and the feedback
//! preconditions a judge repairs under.

mod convert;

pub(crate) use convert::Converted;

use crate::error::WorkflowError;
use crate::plan::StepSpec;

/// Per-step gate checks that need only the step's own shape: the judge
/// repair loop binds to exactly one producing step.
pub(crate) fn check_gates(
    file: &str,
    id: &str,
    converted: &Converted,
    needs: &[String],
) -> Result<(), WorkflowError> {
    if let Some(judge) = &converted.judge {
        if needs.is_empty() {
            return Err(WorkflowError::Parse {
                file: file.to_string(),
                detail: format!("step `{id}`: a judge step needs at least one upstream step"),
            });
        }
        if judge.max_feedback > 0 && needs.len() != 1 {
            return Err(WorkflowError::Parse {
                file: file.to_string(),
                detail: format!(
                    "step `{id}`: judge with max-feedback > 0 needs exactly one producing step"
                ),
            });
        }
    }
    Ok(())
}

/// Cross-step gate checks run over the linked plan: judge feedback must
/// not target another judge (repair targets produce artifacts, not
/// verdicts).
pub(crate) fn cross_step_gate_checks(file: &str, specs: &[StepSpec]) -> Result<(), WorkflowError> {
    for spec in specs {
        let Some(judge) = &spec.judge else {
            continue;
        };
        if judge.max_feedback == 0 {
            continue;
        }
        let producer = &spec.needs[0];
        let Some(target) = specs.iter().find(|other| &other.id == producer) else {
            continue;
        };
        if target.is_judge() {
            return Err(WorkflowError::Parse {
                file: file.to_string(),
                detail: format!(
                    "step `{}`: judge feedback cannot target judge `{producer}`",
                    spec.id
                ),
            });
        }
    }
    Ok(())
}
