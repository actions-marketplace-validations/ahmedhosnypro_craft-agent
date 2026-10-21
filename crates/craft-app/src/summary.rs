//! Run summaries: when `GITHUB_STEP_SUMMARY` names a file, each mission
//! seam appends a markdown record of the finished run and prints one
//! `::notice::` annotation. Best-effort by contract — a summary never
//! changes the run's exit code, and an unset or empty variable changes
//! nothing at all.

mod extract;
mod render;

use std::io::Write as _;
use std::path::{Path, PathBuf};

use craft_core::types::ChatMessage;
use craft_runtime::session::Session;
use craft_workflow::status::{StepReport, StepStatus, WorkflowReport};

use crate::RunOutcome;
use crate::cli::OutputFormat;
use crate::config::{EnvMap, ProviderKind};

pub use extract::{cap_chars, deliverables, final_message, transcript_tail};
pub use render::{
    Rendered, StepContent, conclusion_word, notice_line, render_digest, render_run, render_workflow,
};

pub const SUMMARY_ENV: &str = "GITHUB_STEP_SUMMARY";
pub const FINAL_MESSAGE_CAP: usize = 2_000;
pub const TAIL_CAP: usize = 4_000;
pub const DELIVERABLES_CAP: usize = 8;

/// Operator-visible facts for the summary table: typed counts and
/// configured names only. The builder never reads the environment for
/// content.
pub struct SummaryFacts {
    pub mission: &'static str,
    pub model: String,
    pub provider: String,
    pub turn_cap: Option<u32>,
    pub token_budget: Option<u64>,
    pub rows: Vec<(&'static str, String)>,
}

/// The configured provider's id, as the summary table shows it.
pub fn provider_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OpenAi => "openai",
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Gemini => "gemini",
        ProviderKind::Vertex => "vertex",
    }
}

/// The summary file the environment names; absent or empty is a no-op.
pub fn summary_target(env: &EnvMap) -> Option<PathBuf> {
    let value = env.get(SUMMARY_ENV)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// The run mission seam: after the writer drain, append the finished
/// run's summary when the knob is set.
pub fn emit_run_summary(
    env: &EnvMap,
    format: OutputFormat,
    outcome: &RunOutcome,
    facts: SummaryFacts,
) {
    let Some(target) = summary_target(env) else {
        return;
    };
    let session = load_session(outcome.session_path.as_deref());
    publish(
        &target,
        format,
        &render_run(outcome, facts, session.as_deref()),
    );
}

/// The workflow run being summarized: its report and the directory its
/// step sessions live in.
pub struct WorkflowRun<'a> {
    pub report: &'a WorkflowReport,
    pub sessions_dir: &'a Path,
}

/// The workflow mission seam: same shape plus the per-step table, with
/// deliverables drawn from every settled step's session.
pub fn emit_workflow_summary(
    env: &EnvMap,
    format: OutputFormat,
    run: WorkflowRun<'_>,
    facts: SummaryFacts,
) {
    let Some(target) = summary_target(env) else {
        return;
    };
    let content = step_content(run.report, run.sessions_dir);
    publish(
        &target,
        format,
        &render_workflow(run.report, facts, content),
    );
}

/// The digest mission seam: the summary links the digest markdown path.
pub fn emit_digest_summary(
    env: &EnvMap,
    digest_path: Option<&Path>,
    turns: u32,
    facts: SummaryFacts,
) {
    let Some(target) = summary_target(env) else {
        return;
    };
    publish(
        &target,
        OutputFormat::Plain,
        &render_digest(facts, digest_path, turns),
    );
}

fn step_content(report: &WorkflowReport, sessions_dir: &Path) -> StepContent {
    let mut content = StepContent {
        deliverables: Vec::new(),
        final_message: None,
        tail: None,
    };
    let settled = settled_steps(report);
    for step in &settled {
        gather_step_deliverables(step, sessions_dir, &mut content.deliverables);
    }
    let Some(step) = tail_step(report, &settled) else {
        return content;
    };
    let path = sessions_dir.join(format!("{}.jsonl", step.id));
    if let Some(messages) = load_session(Some(&path)) {
        content.tail = transcript_tail(&messages, TAIL_CAP);
    }
    if let StepStatus::Settled { final_message, .. } = &step.status {
        content.final_message = Some(cap_chars(final_message, FINAL_MESSAGE_CAP).to_string());
    }
    content
}

/// Every settled step, in report order.
fn settled_steps(report: &WorkflowReport) -> Vec<&StepReport> {
    report
        .steps
        .iter()
        .filter(|step| matches!(step.status, StepStatus::Settled { .. }))
        .collect()
}

/// One settled step's deliverables, appended under the cap, deduped.
fn gather_step_deliverables(step: &StepReport, sessions_dir: &Path, into: &mut Vec<String>) {
    let path = sessions_dir.join(format!("{}.jsonl", step.id));
    let Some(messages) = load_session(Some(&path)) else {
        return;
    };
    for item in deliverables(&messages) {
        if into.len() >= DELIVERABLES_CAP {
            break;
        }
        if !into.contains(&item) {
            into.push(item);
        }
    }
}

/// The step whose session shows in the tail: the failed one when the
/// report names it, else the last settled step.
fn tail_step<'a>(report: &'a WorkflowReport, settled: &[&'a StepReport]) -> Option<&'a StepReport> {
    report
        .steps
        .iter()
        .rev()
        .find(|step| Some(step.id.as_str()) == report.failed_step.as_deref())
        .or_else(|| settled.last().copied())
}

fn load_session(path: Option<&Path>) -> Option<Vec<ChatMessage>> {
    let path = path?;
    match Session::load(path) {
        Ok(messages) => Some(messages),
        Err(error) => {
            eprintln!(
                "craft-agent: summary session read {} failed: {error}",
                path.display()
            );
            None
        }
    }
}

/// Append the markdown and print the annotation; both are best-effort —
/// the run is already over and its exit code never moves.
fn publish(target: &Path, format: OutputFormat, rendered: &Rendered) {
    if let Err(error) = append_summary(target, &rendered.markdown) {
        eprintln!(
            "craft-agent: summary write to {} failed: {error}",
            target.display()
        );
    }
    // stream-json stdout is the documented NDJSON contract, so the
    // annotation rides stderr there; both land in the Actions log.
    match format {
        OutputFormat::Plain => {
            let _ = writeln!(std::io::stdout().lock(), "{}", rendered.notice);
        }
        OutputFormat::StreamJson => {
            let _ = writeln!(std::io::stderr().lock(), "{}", rendered.notice);
        }
    }
}

/// Appends one summary block; the file is created when absent.
pub fn append_summary(path: &Path, markdown: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(markdown.as_bytes())
}
