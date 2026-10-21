//! Refusal corpus: a tool call on a banned path returns to the model
//! as an error result naming the reason or the allowed roots, and the
//! run continues to completion.

mod common;

use common::*;
use craft_core::events::ModelEvent;
use craft_core::types::{ChatMessage, StopReason};
use craft_runtime::{CancelCategory, RunConfig, Runner};
use std::path::PathBuf;

fn config() -> RunConfig {
    RunConfig::new(std::path::PathBuf::from("."))
}

fn config_with_session(path: PathBuf) -> RunConfig {
    let mut cfg = config();
    cfg.session_path = Some(path);
    cfg
}

/// Run one tool call through the real runner over the builtin registry
/// and return the `(is_error, content)` the model saw for it.
async fn run_one_call(tool: &str, arguments: &str) -> (bool, String) {
    let provider = FakeProvider::new(vec![
        tool_turn("c1", tool, arguments, usage(1, 1)),
        text_turn("done", StopReason::EndTurn, usage(1, 1)),
    ]);
    let runner = Runner::new(provider, builtin_registry(), config());
    let (outcome, events) = run_collect(&runner, "run").await;
    assert_eq!(outcome.exit_code(), 0);
    assert_eq!(outcome.category, CancelCategory::Completed);
    result_of(&events, "c1")
}

#[tokio::test]
async fn sensitive_path_refusal_round_trips_and_the_run_continues() {
    let (is_error, content) = run_one_call("read_file", r#"{"path":".env"}"#).await;

    assert!(is_error);
    assert!(content.contains("path rejected"));
    assert!(content.contains(".env"));
}

/// The collected tool result of one call id: (is_error, content).
#[allow(clippy::panic)] // loud failure idiom outside a test fn
fn result_of(events: &[craft_runtime::RuntimeEvent], call_id: &str) -> (bool, String) {
    events
        .iter()
        .find_map(|event| match event {
            craft_runtime::RuntimeEvent::ToolResult {
                call_id: id,
                is_error,
                content,
                ..
            } if id == call_id => Some((*is_error, content.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no tool result for {call_id}"))
}

#[tokio::test]
async fn workspace_escape_refusal_round_trips_and_the_run_continues() {
    let (is_error, content) = run_one_call("read_file", r#"{"path":"../outside.txt"}"#).await;

    assert!(is_error);
    assert!(content.contains("escapes the allowed roots"));
}

#[tokio::test]
async fn an_absolute_write_outside_the_root_is_refused_and_the_run_continues() {
    let (is_error, content) = run_one_call(
        "write_file",
        r#"{"path":"/craft-agent-refusal-outside/x.md","content":"x"}"#,
    )
    .await;

    assert!(is_error);
    assert!(content.contains("escapes the allowed roots"));
    assert!(!std::path::Path::new("/craft-agent-refusal-outside/x.md").exists());
}

#[tokio::test]
async fn a_refusal_answers_the_denied_call_in_the_record_and_the_run_continues() {
    let path = session_path("denial-record");
    let mixed_turn = Turn::Events(vec![
        Ok(ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(String::from("c1")),
            name: Some(String::from("read_file")),
            arguments: String::from(r#"{"path":"Cargo.toml"}"#),
        }),
        Ok(ModelEvent::ToolCallDelta {
            index: 1,
            id: Some(String::from("c2")),
            name: Some(String::from("read_file")),
            arguments: String::from(r#"{"path":".env"}"#),
        }),
        Ok(ModelEvent::TurnFinished {
            stop_reason: StopReason::ToolUse,
            usage: usage(1, 1),
        }),
    ]);
    let provider = FakeProvider::new(vec![
        mixed_turn,
        text_turn("done", StopReason::EndTurn, usage(1, 1)),
    ]);
    let runner = Runner::new(
        provider,
        builtin_registry(),
        config_with_session(path.clone()),
    );
    let (outcome, _) = run_collect(&runner, "mixed batch").await;
    assert_eq!(outcome.exit_code(), 0);

    let record: Vec<ChatMessage> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let c1 = record.iter().find_map(|message| match message {
        ChatMessage::Tool {
            call_id, is_error, ..
        } if call_id == "c1" => Some(*is_error),
        _ => None,
    });
    let c2 = record.iter().find_map(|message| match message {
        ChatMessage::Tool {
            call_id,
            is_error,
            content,
            ..
        } if call_id == "c2" => Some((*is_error, content.clone())),
        _ => None,
    });
    assert_eq!(c1, Some(false));
    let (is_error, content) = c2.expect("c2 answered");
    assert!(is_error);
    assert!(content.contains("path rejected"));
    assert!(matches!(record.last(), Some(ChatMessage::Assistant { .. })));
}
