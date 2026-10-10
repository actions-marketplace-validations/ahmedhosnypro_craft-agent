//! Run-summary env corpus: the knob switch, the best-effort append
//! seam, and the no-env-values canary.

mod summary_common;

use std::collections::HashMap;
use std::path::PathBuf;

use craft_app::cli::OutputFormat;
use craft_app::config::EnvMap;
use craft_app::summary::{self, SummaryFacts};
use craft_app::{CancelCategory, ExitStatus};
use summary_common::{
    assistant_text, env_with, facts, outcome, report, run_session, scratch, session_file,
    session_file_at, user,
};

#[tokio::test]
async fn unset_or_empty_env_is_a_no_op() {
    let unset: EnvMap = HashMap::new();
    assert_eq!(summary::summary_target(&unset), None);
    let mut blank: EnvMap = HashMap::new();
    blank.insert(String::from("GITHUB_STEP_SUMMARY"), String::new());
    assert_eq!(summary::summary_target(&blank), None);
    let mut named: EnvMap = HashMap::new();
    named.insert(
        String::from("GITHUB_STEP_SUMMARY"),
        String::from("/tmp/s.md"),
    );
    assert_eq!(
        summary::summary_target(&named),
        Some(PathBuf::from("/tmp/s.md"))
    );

    let dir = scratch("noop");
    let session_path = session_file("noop", &[user("task")])
        .await
        .expect("fixture");
    let mut outcome = outcome(CancelCategory::Completed, ExitStatus::Ok, "run completed");
    outcome.session_path = Some(session_path);
    summary::emit_run_summary(&unset, OutputFormat::Plain, &outcome, facts());
    summary::emit_run_summary(&blank, OutputFormat::Plain, &outcome, facts());
    let entries: Vec<String> = std::fs::read_dir(dir)
        .expect("dir readable")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(entries, vec!["session.jsonl".to_string()]);
}

#[tokio::test]
async fn env_values_never_reach_the_summary_content() {
    let dir = scratch("canary");
    let target = dir.join("summary.md");
    let messages = vec![user("task"), assistant_text("all done, nothing sensitive")];
    let path = session_file("canary", &messages).await.expect("fixture");
    let mut outcome = outcome(CancelCategory::Completed, ExitStatus::Ok, "run completed");
    outcome.session_path = Some(path);
    summary::emit_run_summary(&env_with(&target), OutputFormat::Plain, &outcome, facts());
    let written = std::fs::read_to_string(&target).expect("summary written");
    assert!(written.contains("## craft-agent — completed (exit 0)"));
    assert!(!written.contains("sk-sentinel-never-in-summaries"));
}

#[test]
fn the_append_seam_appends_and_fails_closed_on_bad_paths() {
    let dir = scratch("append");
    let target = dir.join("summary.md");
    summary::append_summary(&target, "first block\n").expect("first append");
    summary::append_summary(&target, "second block\n").expect("second append");
    let written = std::fs::read_to_string(&target).expect("summary written");
    assert_eq!(written, "first block\nsecond block\n");
    let missing = dir.join("no-such-dir").join("summary.md");
    assert!(summary::append_summary(&missing, "x").is_err());
}

#[tokio::test]
async fn emit_workflow_summary_pulls_content_from_step_sessions() {
    let dir = scratch("workflow-emit");
    let target = dir.join("summary.md");
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).expect("sessions dir");
    let scan = run_session();
    session_file_at(&sessions.join("scan.jsonl"), &scan)
        .await
        .expect("scan fixture");
    session_file_at(
        &sessions.join("report.jsonl"),
        &[user("report task"), assistant_text("report done")],
    )
    .await
    .expect("report fixture");
    summary::emit_workflow_summary(
        &env_with(&target),
        OutputFormat::StreamJson,
        &report(),
        SummaryFacts {
            mission: "workflow",
            model: String::from("test-model"),
            provider: String::from("openai"),
            turn_cap: None,
            token_budget: Some(500_000),
            rows: Vec::new(),
        },
        &sessions,
    );
    let written = std::fs::read_to_string(&target).expect("summary written");
    assert!(written.contains("## craft-agent — failed (exit 2)"));
    assert!(
        written.contains("- [PR #7 (octo/app)](https://github.com/octo/app/pull/7)"),
        "deliverables come from the step sessions: {written}"
    );
    assert!(written.contains("### Final message"));
    assert!(written.contains("Transcript tail"));
    assert!(!written.contains("sk-sentinel-never-in-summaries"));
}
