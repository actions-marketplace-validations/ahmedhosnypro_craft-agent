//! One judge panel: run the seats concurrently and merge their verdicts.

use serde_json::Value;

use crate::assemble::StepInputs;
use crate::engine::{StepExecutor, StepOutcome};
use crate::error::WorkflowError;
use crate::plan::{JudgeMerge, JudgeSpec};
use crate::status::StepConclusion;
use crate::truncate_chars;

pub const MAX_ISSUES: usize = 16;
pub(super) const MAX_ISSUE_CHARS: usize = 2_000;
const MAX_SCAN_CHARS: usize = 16_384;

/// One seat's parsed verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatVerdict {
    pub pass: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedVerdict {
    pub pass: bool,
    pub issues: Vec<String>,
    pub passes: u32,
    pub fails: u32,
}

pub(super) fn json_object(text: &str) -> Option<&str> {
    let window = &text[..text.len().min(MAX_SCAN_CHARS)];
    let start = window.find('{')?;
    let end = window.rfind('}')?;
    window.get(start..=end)
}
pub fn parse_seat(step: &str, text: &str) -> Result<SeatVerdict, WorkflowError> {
    let bad = |detail: String| WorkflowError::JudgeGate {
        step: step.to_string(),
        detail,
    };
    let raw =
        json_object(text).ok_or_else(|| bad("seat verdict has no JSON object".to_string()))?;
    let value: Value =
        serde_json::from_str(raw).map_err(|err| bad(format!("seat verdict is not JSON: {err}")))?;
    let Some(verdict) = value.get("verdict").and_then(Value::as_str) else {
        return Err(bad("seat verdict lacks a `verdict` string".to_string()));
    };
    let pass = match verdict {
        "pass" => true,
        "fail" => false,
        other => return Err(bad(format!("`verdict` is `{other}`, expected pass|fail"))),
    };
    let mut issues = Vec::new();
    if let Some(list) = value.get("issues") {
        let Some(list) = list.as_array() else {
            return Err(bad("`issues` is not a list".to_string()));
        };
        for item in list.iter().take(MAX_ISSUES) {
            let Some(text) = item.as_str() else {
                return Err(bad("every issue is a string".to_string()));
            };
            issues.push(truncate_chars(text, MAX_ISSUE_CHARS));
        }
    }
    Ok(SeatVerdict { pass, issues })
}

/// One seat's in-flight run, boxed so a panel of seats can settle
/// together under one lifetime.
type SeatFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<StepOutcome, WorkflowError>> + Send + 'a>,
>;

/// The rounds a seat gets to emit a parseable verdict: the seat re-runs
/// until the parser accepts it, and this cap is the fail-closed backstop.
pub const MAX_SEAT_REPAIRS: u32 = 8;

/// The corrective prompt for one seat: the parser's own complaint, so the
/// model can fix exactly what broke.
fn repair_marker(detail: &str) -> String {
    format!(
        "\n\n## Verdict format repair\n\nThe gate's parser rejected your previous final \
         message: {detail}\nReply with ONLY this JSON object, quoted keys and all:\n\
         {{\"verdict\":\"pass\"|\"fail\",\"issues\":[\"...\"]}}\nNo prose, no markdown, nothing else."
    )
}

/// One panel of seats run concurrently, merged into the step outcome the
/// engine settles: pass verdict is a success conclusion, fail verdict an
/// error conclusion, a seat that failed to run (or a malformed verdict)
/// is a typed judge failure carrying the seat detail.
pub async fn run_panel<E: StepExecutor + Sync>(
    executor: &E,
    base: StepInputs,
    spec: &JudgeSpec,
) -> Result<StepOutcome, WorkflowError> {
    let total = spec.panel.max(1);
    let mut outcomes = run_seats(executor, &base, total, (0..total).collect(), None).await;
    repair_seats(executor, &base, total, &mut outcomes).await;
    let mut parsed: Vec<SeatVerdict> = Vec::with_capacity(outcomes.len());
    let mut total_usage = crate::status::StepUsage::default();
    let mut broken: Option<String> = None;
    for outcome in outcomes {
        let outcome = outcome?;
        total_usage = total_usage.saturating_add(outcome.usage);
        if !outcome.conclusion.is_success() {
            broken = Some(outcome.final_message);
            break;
        }
        parsed.push(parse_seat(&base.id, &outcome.final_message)?);
    }
    if let Some(detail) = broken {
        return Ok(StepOutcome {
            conclusion: StepConclusion::Error,
            usage: total_usage,
            final_message: detail,
        });
    }
    let merged = merge_seats(&base.id, spec.merge, &parsed)?;
    let conclusion = if merged.pass {
        StepConclusion::Success
    } else {
        StepConclusion::Error
    };
    Ok(StepOutcome {
        conclusion,
        usage: total_usage,
        final_message: merged_json(&merged),
    })
}

/// Re-runs, until the verdict parses or the cap is spent, every seat whose
/// run succeeded but whose final message the verdict parser rejected. Each
/// round carries the parser's complaint; unparseable after the last round
/// is the typed JudgeGate failure.
async fn repair_seats<E: StepExecutor + Sync>(
    executor: &E,
    base: &StepInputs,
    total: u32,
    outcomes: &mut [Result<StepOutcome, WorkflowError>],
) {
    for _ in 0..MAX_SEAT_REPAIRS {
        let unparsed: Vec<(usize, String)> = outcomes
            .iter()
            .enumerate()
            .filter_map(|(index, outcome)| match outcome {
                Ok(ok) if ok.conclusion.is_success() => parse_seat(&base.id, &ok.final_message)
                    .err()
                    .map(|err| (index, err.to_string())),
                _ => None,
            })
            .collect();
        if unparsed.is_empty() {
            return;
        }
        let indices: Vec<u32> = unparsed.iter().map(|(index, _)| *index as u32).collect();
        let details: Vec<String> = unparsed.iter().map(|(_, detail)| detail.clone()).collect();
        let slots: Vec<usize> = unparsed.iter().map(|(index, _)| *index).collect();
        let repaired = run_seats(executor, base, total, indices, Some(&details)).await;
        for (slot, mut outcome) in slots.into_iter().zip(repaired) {
            if let Ok(second) = &mut outcome
                && let Ok(first) = &outcomes[slot]
            {
                second.usage = second.usage.saturating_add(first.usage);
            }
            outcomes[slot] = outcome;
        }
    }
}

/// Runs the named seat indices concurrently; `repairs[i]` appends that
/// parser complaint's corrective prompt to seat `i`'s prompt.
async fn run_seats<E: StepExecutor + Sync>(
    executor: &E,
    base: &StepInputs,
    total: u32,
    indices: Vec<u32>,
    repairs: Option<&[String]>,
) -> Vec<Result<StepOutcome, WorkflowError>> {
    let mut seats: Vec<SeatFuture<'_>> = Vec::new();
    for (position, index) in indices.into_iter().enumerate() {
        let mut inputs = base.clone();
        inputs.prompt.push_str(&seat_marker(index + 1, total));
        if let Some(details) = repairs {
            inputs.prompt.push_str(&repair_marker(&details[position]));
        }
        seats.push(Box::pin(executor.execute(inputs)));
    }
    futures::future::join_all(seats).await
}

fn seat_marker(index: u32, total: u32) -> String {
    format!(
        "\n\n[seat {index} of {total}] Judge independently: your verdict stands \
         alone and must cite concrete evidence from the artifact you judge."
    )
}

/// Majority passes on strictly more pass votes (a tie fails); unanimous
/// passes only when every seat passed.
pub fn merge_seats(
    step: &str,
    merge: JudgeMerge,
    seats: &[SeatVerdict],
) -> Result<MergedVerdict, WorkflowError> {
    if seats.is_empty() {
        return Err(WorkflowError::JudgeGate {
            step: step.to_string(),
            detail: "judge panel ran no seats".to_string(),
        });
    }
    let passes = seats.iter().filter(|seat| seat.pass).count() as u32;
    let fails = seats.len() as u32 - passes;
    let pass = match merge {
        JudgeMerge::Majority => passes > fails,
        JudgeMerge::Unanimous => fails == 0,
    };
    let mut issues: Vec<String> = Vec::new();
    for seat in seats.iter().filter(|seat| !seat.pass) {
        for issue in &seat.issues {
            if !issues.contains(issue) {
                issues.push(issue.clone());
            }
        }
    }
    issues.truncate(MAX_ISSUES);
    Ok(MergedVerdict {
        pass,
        issues,
        passes,
        fails,
    })
}

/// The canonical settled text: verdict, vote counts, and the issues a
/// failed gate found.
pub fn merged_json(verdict: &MergedVerdict) -> String {
    let head = format!(
        "{{\"verdict\":\"{}\",\"votes\":{{\"pass\":{},\"fail\":{}}},\"issues\":[",
        if verdict.pass { "pass" } else { "fail" },
        verdict.passes,
        verdict.fails,
    );
    let mut out = head;
    for (index, issue) in verdict.issues.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(issue).unwrap_or_default());
    }
    out.push_str("]}");
    out
}
