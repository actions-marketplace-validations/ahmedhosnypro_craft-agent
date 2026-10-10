//! Summary markdown assembly: one builder per mission over shared,
//! capped sections.

use std::path::Path;

use craft_core::types::{ChatMessage, Usage};
use craft_workflow::status::{
    RunConclusion, SkipReason, StepConclusion, StepStatus, WorkflowReport,
};

use super::extract;
use super::{SummaryFacts, TAIL_CAP};
use crate::{CancelCategory, RunOutcome};

/// The finished summary and its one-line annotation.
pub struct Rendered {
    pub markdown: String,
    pub notice: String,
}

/// The workflow mission's extracted step-session content.
pub struct StepContent {
    pub deliverables: Vec<String>,
    pub final_message: Option<String>,
    pub tail: Option<String>,
}

/// The conclusion word the summary heading and annotation carry.
pub fn conclusion_word(category: CancelCategory) -> &'static str {
    match category {
        CancelCategory::Completed => "completed",
        CancelCategory::Error => "error",
        CancelCategory::MaxTurns => "max-turns",
        CancelCategory::Stationarity => "stationarity",
        CancelCategory::Budget => "budget",
    }
}

/// The run mission's summary from the terminal record and the session.
pub fn render_run(
    outcome: &RunOutcome,
    mut facts: SummaryFacts,
    session: Option<&[ChatMessage]>,
) -> Rendered {
    let conclusion = conclusion_word(outcome.category);
    if let Some(path) = &outcome.session_path {
        facts.rows.push(("Session", path.display().to_string()));
    }
    let final_message = session.and_then(extract::final_message);
    let deliverables = session.map(extract::deliverables);
    let tail = session.and_then(|messages| extract::transcript_tail(messages, TAIL_CAP));
    let notice_tail = final_message.unwrap_or(outcome.detail.as_str());
    let markdown = document(
        &heading(conclusion, outcome.exit_code()),
        &blockquote(&outcome.detail),
        &facts_table(&facts, Some(u64::from(outcome.turns)), Some(&outcome.usage)),
        &[
            &deliverables_section(deliverables),
            &final_message_section(final_message),
            &tail_section(tail),
        ],
    );
    Rendered {
        markdown,
        notice: notice_line(conclusion, outcome.exit_code(), notice_tail),
    }
}

/// The workflow mission's summary: the report's step table plus the
/// tail-step session's extracted content.
pub fn render_workflow(
    report: &WorkflowReport,
    mut facts: SummaryFacts,
    content: StepContent,
) -> Rendered {
    let conclusion = workflow_conclusion(report);
    if let Some(step) = &report.failed_step {
        facts.rows.push(("Failed step", step.clone()));
    }
    facts
        .rows
        .push(("Plan", format!("{} ({})", report.name, report.run_id)));
    let usage = Usage {
        input_tokens: report.total_usage.input_tokens,
        output_tokens: report.total_usage.output_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: report.total_usage.reasoning_tokens,
        incomplete: false,
    };
    let lead = workflow_lead(report);
    let markdown = document(
        &heading(conclusion, report.exit_code),
        &blockquote(&lead),
        &facts_table(&facts, Some(report.total_usage.turns), Some(&usage)),
        &[
            &step_table(report),
            &deliverables_section(Some(content.deliverables)),
            &final_message_section(content.final_message.as_deref()),
            &tail_section(content.tail),
        ],
    );
    let notice_tail = content.final_message.as_deref().unwrap_or(&lead);
    Rendered {
        markdown,
        notice: notice_line(conclusion, report.exit_code, notice_tail),
    }
}

/// The digest mission's summary: the facts plus the digest path.
pub fn render_digest(mut facts: SummaryFacts, digest_path: Option<&Path>, turns: u32) -> Rendered {
    let (digest_cell, lead) = match digest_path {
        Some(path) => (
            path.display().to_string(),
            format!("digest written to {}", path.display()),
        ),
        None => (
            String::from("(dry-run)"),
            String::from("digest dry-run complete"),
        ),
    };
    facts.rows.push(("Digest", digest_cell));
    let markdown = document(
        &heading("completed", 0),
        &blockquote(&lead),
        &facts_table(&facts, Some(u64::from(turns)), None),
        &[],
    );
    Rendered {
        markdown,
        notice: notice_line("completed", 0, &lead),
    }
}

/// The annotation: one line, first line of the tail only.
pub fn notice_line(conclusion: &str, exit_code: i32, tail: &str) -> String {
    let first = tail.lines().next().unwrap_or_default();
    format!("::notice::{conclusion} — exit {exit_code} — {first}")
}

fn workflow_conclusion(report: &WorkflowReport) -> &'static str {
    match report.conclusion {
        RunConclusion::Completed => "completed",
        RunConclusion::Failed { .. } => "failed",
        RunConclusion::BudgetExhausted => "budget",
    }
}

fn workflow_lead(report: &WorkflowReport) -> String {
    match &report.conclusion {
        RunConclusion::Completed => String::from("workflow completed"),
        RunConclusion::Failed { step, conclusion } => format!(
            "workflow failed at step `{step}`: {}",
            step_conclusion_word(*conclusion)
        ),
        RunConclusion::BudgetExhausted => String::from("workflow token budget exhausted"),
    }
}

fn step_conclusion_word(conclusion: StepConclusion) -> &'static str {
    match conclusion {
        StepConclusion::Success => "success",
        StepConclusion::Error => "error",
        StepConclusion::MaxTurns => "max-turns",
        StepConclusion::Stationarity => "stationarity",
        StepConclusion::Budget => "budget",
    }
}

fn step_table(report: &WorkflowReport) -> String {
    let mut out =
        String::from("\n### Steps\n\n| Step | Conclusion | Turns | Tokens |\n|---|---|---|---|\n");
    for step in &report.steps {
        let (conclusion, turns, tokens) = match &step.status {
            StepStatus::Settled {
                conclusion, usage, ..
            } => (
                step_conclusion_word(*conclusion).to_string(),
                usage.turns.to_string(),
                usage.total_tokens().to_string(),
            ),
            StepStatus::Skipped { reason } => (skip_note(reason), "—".to_string(), "—".to_string()),
        };
        out.push_str(&format!(
            "| {} | {conclusion} | {turns} | {tokens} |\n",
            table_cell(&step.id)
        ));
    }
    out
}

fn skip_note(reason: &SkipReason) -> String {
    match reason {
        SkipReason::DependencyFailed { dep } => format!("skipped ({dep} failed)"),
        SkipReason::Unreachable => String::from("skipped (unreachable)"),
    }
}

fn document(heading: &str, lead: &str, table: &str, sections: &[&str]) -> String {
    let mut out = format!("{heading}\n\n");
    if !lead.is_empty() {
        out.push_str(lead);
        out.push('\n');
    }
    out.push_str(table);
    for section in sections {
        if !section.is_empty() {
            out.push_str(section);
        }
    }
    out
}

fn heading(conclusion: &str, exit_code: i32) -> String {
    format!("## craft-agent — {conclusion} (exit {exit_code})")
}

fn blockquote(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn facts_table(facts: &SummaryFacts, turns: Option<u64>, usage: Option<&Usage>) -> String {
    let mut out = String::from("| | |\n|---|---|\n");
    out.push_str(&format!("| Mission | {} |\n", table_cell(facts.mission)));
    out.push_str(&format!(
        "| Model | {} |\n",
        table_cell(&format!("{} ({})", facts.model, facts.provider))
    ));
    if let Some(turns) = turns {
        let cell = match facts.turn_cap {
            Some(cap) => format!("{turns} of {cap}"),
            None => turns.to_string(),
        };
        out.push_str(&format!("| Turns | {cell} |\n"));
    }
    if let Some(usage) = usage {
        out.push_str(&format!(
            "| Tokens | in {} · out {} · cache-read {} · reasoning {}{} |\n",
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.reasoning_tokens,
            budget_suffix(facts.token_budget)
        ));
    }
    for (name, value) in &facts.rows {
        out.push_str(&format!(
            "| {} | {} |\n",
            table_cell(name),
            table_cell(value)
        ));
    }
    out
}

fn budget_suffix(budget: Option<u64>) -> String {
    match budget {
        Some(budget) => format!(" (of {budget})"),
        None => String::new(),
    }
}

fn deliverables_section(items: Option<Vec<String>>) -> String {
    let Some(items) = items else {
        return String::new();
    };
    let mut out = String::from("\n### Deliverables\n\n");
    if items.is_empty() {
        out.push_str("- none surfaced in this run's transcript\n");
        return out;
    }
    for item in items {
        out.push_str(&item);
        out.push('\n');
    }
    out
}

fn final_message_section(text: Option<&str>) -> String {
    let Some(text) = text else {
        return String::new();
    };
    let mut out = String::from("\n### Final message\n\n");
    for line in text.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn tail_section(text: Option<String>) -> String {
    let Some(text) = text else {
        return String::new();
    };
    format!(
        "\n<details><summary>Transcript tail</summary>\n\n````text\n{text}\n````\n\n</details>\n"
    )
}

/// A GFM table cell: pipes cannot break the column, newlines the row.
fn table_cell(value: &str) -> String {
    value.replace('\n', " ").replace('|', "\\|")
}
