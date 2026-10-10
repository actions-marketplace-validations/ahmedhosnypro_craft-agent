//! Secret-redaction corpus: a registered value never leaves the runtime
//! in raw or encoded form, the transcript-side belt scrubs the text
//! fields events carry, and explicitly constructed redactors stay on.

mod common;

use common::*;
use craft_core::types::StopReason;
use craft_runtime::{RunConfig, Runner, RuntimeEvent};

/// The redaction marker layer 1 replaces a known value with.
const KNOWN_MARKER: &str = "[REDACTED:known-secret]";

/// A classic-shaped fake token, assembled so the source file never
/// carries the assembled literal.
fn fake_token() -> String {
    format!("ghp_{}", "A".repeat(36))
}

#[tokio::test]
async fn encoded_secrets_never_leave_the_runtime() {
    let token = fake_token();
    let b64_composite =
        "eC1hY2Nlc3MtdG9rZW46Z2hwX0FBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
    let b64_token = "Z2hwX0FBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
    let redactor = std::sync::Arc::new(craft_redact::Redactor::from_parts(
        vec![token.clone()],
        true,
    ));
    let provider = FakeProvider::new(vec![
        tool_turn(
            "c1",
            "leaky",
            &format!("{{\"inline\":\"{token}\"}}"),
            usage(1, 1),
        ),
        text_turn("done", StopReason::EndTurn, usage(1, 1)),
    ]);
    let progress_line = format!("Authorization: Basic {b64_composite}");
    let output_line = format!("pushed with {token}");
    let registry = registry_with_progress("leaky", &[&progress_line], &output_line);
    let mut config = RunConfig::new(std::path::PathBuf::from("."));
    config.redactor = redactor.clone();
    let runner = Runner::new(provider, registry, config);
    let (_outcome, events) = run_collect(&runner, "ship the branch").await;

    let mut saw_marker = false;
    for event in &events {
        let line = serde_json::to_string(event).expect("event serializes");
        assert!(!line.contains(&token), "raw token in event: {line}");
        assert!(!line.contains(b64_composite), "composite in event: {line}");
        assert!(!line.contains(b64_token), "encoded token in event: {line}");
        if line.contains(KNOWN_MARKER) {
            saw_marker = true;
        }
    }
    assert!(saw_marker, "no event carries the redaction marker");
    assert!(
        redactor.redactions() >= 2,
        "seam redactions: {}",
        redactor.redactions()
    );
    assert!(
        redactor.raw_secret_hits() >= 1,
        "raw hits: {}",
        redactor.raw_secret_hits()
    );
}

#[test]
fn belt_redacts_event_text_fields() {
    let token = fake_token();
    let redactor = craft_redact::Redactor::from_parts(vec![token.clone()], false);

    let mut retried = RuntimeEvent::TurnRetried {
        turn: 1,
        attempt: 1,
        reason: format!("stream died near {token}"),
    };
    craft_runtime::redact_event(&mut retried, &redactor);
    let RuntimeEvent::TurnRetried { reason, .. } = &retried else {
        panic!("event kept its variant");
    };
    assert!(reason.contains(KNOWN_MARKER), "{reason}");
    assert!(!reason.contains(&token), "{reason}");

    let mut finished = RuntimeEvent::SubagentFinished {
        turn: 2,
        id: String::from("sub-1"),
        profile: String::from("proofer"),
        stop_reason: String::from("completed"),
        detail: format!("child echoed {token}"),
        usage: usage(0, 0),
        turns: 1,
        session_path: None,
    };
    craft_runtime::redact_event(&mut finished, &redactor);
    let RuntimeEvent::SubagentFinished { detail, .. } = &finished else {
        panic!("event kept its variant");
    };
    assert!(detail.contains(KNOWN_MARKER), "{detail}");
    assert!(!detail.contains(&token), "{detail}");
}

#[test]
fn from_parts_redaction_is_always_on() {
    let token = fake_token();
    let redactor = craft_redact::Redactor::from_parts(vec![token.clone()], false);
    assert!(redactor.is_enabled());
    let text = format!("echo {token}");
    let scrubbed = redactor.redact(&text).into_owned();
    assert!(scrubbed.contains(KNOWN_MARKER), "{scrubbed}");
    assert!(!scrubbed.contains(&token), "{scrubbed}");
}

#[test]
fn debug_output_carries_no_needle_text() {
    let token = fake_token();
    let redactor = craft_redact::Redactor::from_parts(vec![token.clone()], false);
    let rendered = format!("{redactor:?}");
    assert!(!rendered.contains(&token), "{rendered}");
    assert!(rendered.contains("values: 1"), "{rendered}");
    assert!(rendered.contains("enabled: true"), "{rendered}");
}
