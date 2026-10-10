//! Run-summary caps corpus: hostile and oversized content degrades to
//! bounded output, never a panic.

mod summary_common;

use craft_app::summary::{self, DELIVERABLES_CAP, FINAL_MESSAGE_CAP, TAIL_CAP};
use craft_app::{CancelCategory, ExitStatus};
use craft_core::types::ChatMessage;
use summary_common::{assistant_text, facts, outcome, tool_result, user};

#[test]
fn final_message_caps_on_a_character_boundary() {
    let long = "é".repeat(FINAL_MESSAGE_CAP + 500);
    let session = vec![assistant_text(&long)];
    let capped = summary::final_message(&session).expect("final message present");
    assert_eq!(capped.chars().count(), FINAL_MESSAGE_CAP);
    assert!(long.starts_with(capped));
    let outcome = outcome(CancelCategory::Completed, ExitStatus::Ok, "run completed");
    let rendered = summary::render_run(&outcome, facts(), Some(&session));
    let quoted: usize = rendered
        .markdown
        .lines()
        .skip_while(|line| !line.contains("### Final message"))
        .skip(2)
        .take_while(|line| line.starts_with("> "))
        .map(|line| line.chars().filter(|c| *c == 'é').count())
        .sum();
    assert!(
        quoted <= FINAL_MESSAGE_CAP,
        "the quoted final message is capped"
    );
    assert!(rendered.markdown.contains(&"é".repeat(FINAL_MESSAGE_CAP)));
}

#[test]
fn transcript_tail_caps_and_keeps_the_newest_messages() {
    let old = "x".repeat(TAIL_CAP + 2_000);
    let messages = vec![
        user(&old),
        tool_result("early noise"),
        tool_result("the newest marker"),
    ];
    let tail = summary::transcript_tail(&messages, TAIL_CAP).expect("tail present");
    assert!(tail.chars().count() <= TAIL_CAP);
    assert!(tail.contains("the newest marker"), "{tail}");
    assert!(tail.contains("early noise"));
    assert!(
        tail.starts_with("[user] "),
        "the oldest message is the truncated one"
    );
    assert!(tail.matches('x').count() < old.chars().count());
    assert_eq!(summary::transcript_tail(&[], TAIL_CAP), None);
}

#[test]
fn deliverables_dedupe_and_cap_at_eight() {
    let urls: String = (1..=12)
        .map(|n| format!("see https://github.com/octo/app/pull/{n} "))
        .collect();
    let messages = vec![
        tool_result(
            "To https://github.com/octo/app.git\n \
             9f7c1a2..3b8d4e5  main -> main\n \
             * [new branch]      feat/x -> feat/x\n \
             - [deleted]         gone -> gone",
        ),
        tool_result(&urls),
        tool_result("again https://github.com/octo/app/pull/1"),
    ];
    let found = summary::deliverables(&messages);
    assert_eq!(found.len(), DELIVERABLES_CAP);
    assert_eq!(found.first(), Some(&"- pushed `octo/app@main`".to_string()));
    assert!(
        found.contains(&"- pushed `octo/app@feat/x`".to_string()),
        "the [new branch] row must surface: {found:?}"
    );
    assert_eq!(
        found.iter().collect::<std::collections::HashSet<_>>().len(),
        found.len()
    );
    assert!(
        found.contains(&"- [PR #1 (octo/app)](https://github.com/octo/app/pull/1)".to_string()),
        "the PR scan fills the remaining slots: {found:?}"
    );
    assert!(!found.iter().any(|item| item.contains("deleted")));
}

#[test]
fn deliverables_reject_malformed_urls_and_push_rows() {
    let messages = vec![tool_result(
        "https://github.com/octo//pull/1 https://github.com/octo/app/pull/ \
         https://github.com/octo/app/pull/123456789012 \
         https://github.com/octo/app/pull/12abc \
         To https://github.com/octo/app.git\n \
         not_a_range..row  main -> main\n \
         1234..5678  tiny..range",
    )];
    assert!(summary::deliverables(&messages).is_empty());
}

#[test]
fn deliverables_from_assistant_text_and_multibyte_noise() {
    let messages: Vec<ChatMessage> = vec![
        assistant_text(
            "PR ready: https://github.com/öwnér/app/pull/5 and https://github.com/ok/app/pull/6.",
        ),
        tool_result("nothing here"),
    ];
    let found = summary::deliverables(&messages);
    assert_eq!(
        found,
        vec!["- [PR #6 (ok/app)](https://github.com/ok/app/pull/6)".to_string()]
    );
}
