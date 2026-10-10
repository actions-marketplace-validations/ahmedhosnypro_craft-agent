//! Run-summary format corpus: the per-conclusion shape for every
//! mission, straight from the terminal record.

mod summary_common;

use std::path::Path;

use craft_app::summary::{self, SummaryFacts};
use craft_app::{CancelCategory, ExitStatus};
use summary_common::{assistant_call, assistant_text, facts, outcome, report, run_session, user};

#[test]
fn run_summary_renders_heading_facts_and_sections_per_conclusion() {
    let cases = [
        (CancelCategory::Completed, ExitStatus::Ok, "completed", 0),
        (
            CancelCategory::MaxTurns,
            ExitStatus::MaxTurns,
            "max-turns",
            2,
        ),
        (CancelCategory::Budget, ExitStatus::Budget, "budget", 3),
        (CancelCategory::Error, ExitStatus::Error, "error", 1),
    ];
    for (category, exit, word, code) in cases {
        let detail = format!("{word} detail line");
        let outcome = outcome(category, exit, &detail);
        let session = run_session();
        let rendered = summary::render_run(&outcome, facts(), Some(&session));
        assert!(
            rendered
                .markdown
                .contains(&format!("## craft-agent — {word} (exit {code})")),
            "{word} heading: {}",
            rendered.markdown
        );
        assert!(
            rendered.markdown.contains(&format!("> {detail}\n")),
            "{word} lead: {}",
            rendered.markdown
        );
        assert!(rendered.markdown.contains("| Mission | run |"), "{word}");
        assert!(
            rendered
                .markdown
                .contains("| Model | test-model (openai) |"),
            "{word}"
        );
        assert!(rendered.markdown.contains("| Turns | 2 of 40 |"), "{word}");
        assert!(
            rendered.markdown.contains(
                "| Tokens | in 100 · out 50 · cache-read 10 · reasoning 5 (of 4000000) |"
            ),
            "{word}"
        );
        assert!(
            rendered
                .markdown
                .contains("| Session | /tmp/sessions/record.jsonl |"),
            "{word}"
        );
        assert!(
            rendered
                .markdown
                .contains("- [PR #7 (octo/app)](https://github.com/octo/app/pull/7)"),
            "{word} deliverable"
        );
        assert!(
            rendered.markdown.contains("- pushed `octo/app@main`"),
            "{word} push"
        );
        assert!(rendered.markdown.contains("### Final message"), "{word}");
        assert!(
            rendered
                .markdown
                .contains("<details><summary>Transcript tail</summary>"),
            "{word} tail"
        );
        assert_eq!(
            rendered.notice,
            format!(
                "::notice::{word} — exit {code} — done — see https://github.com/octo/app/pull/7 for the PR"
            ),
            "{word} notice"
        );
    }
}

#[test]
fn a_run_without_a_final_message_falls_back_to_the_detail_line() {
    let outcome = outcome(
        CancelCategory::MaxTurns,
        ExitStatus::MaxTurns,
        "run aborted after max turns (1)",
    );
    let session = vec![user("task"), assistant_call("bash")];
    let rendered = summary::render_run(&outcome, facts(), Some(&session));
    assert_eq!(
        rendered.notice,
        "::notice::max-turns — exit 2 — run aborted after max turns (1)"
    );
    assert!(!rendered.markdown.contains("### Final message"));
    assert!(rendered.markdown.contains("### Deliverables"));
    assert!(
        rendered
            .markdown
            .contains("- none surfaced in this run's transcript")
    );
}

#[test]
fn a_run_without_a_session_skips_the_session_sections() {
    let mut outcome = outcome(CancelCategory::Completed, ExitStatus::Ok, "run completed");
    outcome.session_path = None;
    let rendered = summary::render_run(&outcome, facts(), None);
    assert!(
        rendered
            .markdown
            .contains("## craft-agent — completed (exit 0)")
    );
    assert!(!rendered.markdown.contains("### Deliverables"));
    assert!(!rendered.markdown.contains("### Final message"));
    assert!(!rendered.markdown.contains("Transcript tail"));
    assert!(!rendered.markdown.contains("| Session |"));
    assert!(rendered.markdown.contains("> run completed\n"));
}

#[test]
fn workflow_summary_renders_the_step_table_and_failed_step() {
    let content = summary::StepContent {
        deliverables: vec!["- [PR #3 (octo/app)](https://github.com/octo/app/pull/3)".to_string()],
        final_message: Some(String::from("scan died at the cap")),
        tail: None,
    };
    let rendered = summary::render_workflow(
        &report(),
        SummaryFacts {
            mission: "workflow",
            model: String::from("test-model"),
            provider: String::from("openai"),
            turn_cap: None,
            token_budget: Some(500_000),
            rows: Vec::new(),
        },
        content,
    );
    assert!(
        rendered
            .markdown
            .contains("## craft-agent — failed (exit 2)")
    );
    assert!(
        rendered
            .markdown
            .contains("> workflow failed at step `scan`: max-turns")
    );
    assert!(rendered.markdown.contains("| Failed step | scan |"));
    assert!(rendered.markdown.contains("| Plan | suite (r1) |"));
    assert!(rendered.markdown.contains("| Turns | 2 |"));
    assert!(rendered.markdown.contains("| scan | max-turns | 2 | 90 |"));
    assert!(
        rendered
            .markdown
            .contains("| report | skipped (scan failed) | — | — |")
    );
    assert!(
        rendered
            .markdown
            .contains("- [PR #3 (octo/app)](https://github.com/octo/app/pull/3)")
    );
    assert_eq!(
        rendered.notice,
        "::notice::failed — exit 2 — scan died at the cap"
    );
}

#[test]
fn digest_summary_links_the_digest_path_and_the_dry_run() {
    let rendered = summary::render_digest(facts(), Some(Path::new("/out/digest.md")), 3);
    assert!(
        rendered
            .markdown
            .contains("## craft-agent — completed (exit 0)")
    );
    assert!(
        rendered
            .markdown
            .contains("> digest written to /out/digest.md")
    );
    assert!(rendered.markdown.contains("| Digest | /out/digest.md |"));
    assert!(rendered.markdown.contains("| Turns | 3 of 40 |"));
    assert!(!rendered.markdown.contains("| Tokens |"));
    assert_eq!(
        rendered.notice,
        "::notice::completed — exit 0 — digest written to /out/digest.md"
    );

    let dry = summary::render_digest(facts(), None, 1);
    assert!(dry.markdown.contains("| Digest | (dry-run) |"));
    assert_eq!(
        dry.notice,
        "::notice::completed — exit 0 — digest dry-run complete"
    );
}

#[test]
fn final_message_and_tail_render_only_when_present() {
    let messages = vec![user("only a task"), assistant_text("short but final")];
    let outcome = outcome(CancelCategory::Completed, ExitStatus::Ok, "run completed");
    let rendered = summary::render_run(&outcome, facts(), Some(&messages));
    assert!(rendered.markdown.contains("> short but final"));
    assert!(rendered.markdown.contains("[user] only a task"));
    assert!(rendered.markdown.contains("[assistant] short but final"));
}
