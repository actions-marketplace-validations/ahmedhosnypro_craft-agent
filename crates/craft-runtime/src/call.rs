//! The tool-call bridge: drives the registry's contracted streams into
//! model-visible tool results and run events. Tool errors round-trip as
//! `is_error` results the model can act on; nothing in the tool layer
//! ends a run.

use craft_core::types::{ChatMessage, ToolCall};
use craft_tools::{OutStream, ToolCtx, ToolEvent};
use futures::StreamExt;

use crate::events::{RuntimeEvent, ToolStreamName};
use crate::guard::{Guard, NUDGE_TEXT, Stationarity};
use crate::session::{Session, SessionError};

/// The stationarity stop detail; shared with the runner so the guard's
/// message and the run record never drift apart.
pub(crate) const STATIONARY_DETAIL: &str =
    "stationary run stopped: the same tool call repeated 8 times in a row";

/// How the flow continues after a turn's tool calls.
pub(crate) enum Flow {
    Continue,
    Stationary,
}

/// Appends one error tool result per call that will never execute, so a
/// terminal exit leaves a transcript the provider accepts on resume:
/// every logged tool_use has its tool_result.
pub(crate) async fn abandon_calls(
    session: &mut Session,
    calls: &[ToolCall],
    detail: &str,
) -> Result<(), SessionError> {
    for call in calls {
        session
            .append(ChatMessage::Tool {
                call_id: call.id.clone(),
                content: format!("tool call not completed: {detail}"),
                is_error: true,
                images: Vec::new(),
            })
            .await?;
    }
    Ok(())
}

/// Executes every call of a turn in order, appending each result to
/// the session, applying the stationarity guard, and injecting the
/// nudge before the flow continues.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_calls(
    registry: &craft_tools::Registry,
    ctx: &ToolCtx,
    redactor: &craft_redact::Redactor,
    session: &mut Session,
    calls: &[ToolCall],
    turn: u32,
    sink: &mut (dyn FnMut(RuntimeEvent) + Send),
    guard: &mut Stationarity,
) -> Result<Flow, SessionError> {
    let mut nudge = false;
    for (index, call) in calls.iter().enumerate() {
        sink(RuntimeEvent::ToolCall {
            turn,
            index: index as u64,
            id: call.id.clone(),
            name: call.name.clone(),
            // The sink sees a redacted copy; execution keeps the raw arguments.
            arguments: redactor.redact(&call.arguments).into_owned(),
        });
        let (content, is_error) = execute_one(registry, ctx, redactor, call, turn, sink).await;
        session
            .append(craft_core::types::ChatMessage::Tool {
                call_id: call.id.clone(),
                content: content.clone(),
                is_error,
                images: Vec::new(),
            })
            .await?;
        sink(RuntimeEvent::ToolResult {
            turn,
            call_id: call.id.clone(),
            name: call.name.clone(),
            is_error,
            content,
        });
        match guard.observe(call) {
            Guard::Stop => {
                abandon_calls(session, &calls[index + 1..], STATIONARY_DETAIL).await?;
                return Ok(Flow::Stationary);
            }
            Guard::Nudge => nudge = true,
            Guard::Continue => {}
        }
    }
    if nudge {
        session
            .append(craft_core::types::ChatMessage::User {
                text: String::from(NUDGE_TEXT),
                images: Vec::new(),
            })
            .await?;
    }
    Ok(Flow::Continue)
}

/// Drives one contracted tool stream to its single result: progress is
/// surfaced as it arrives, and the first terminal or typed error wins.
/// The returned pair is the model-visible content and its `is_error`.
async fn execute_one(
    registry: &craft_tools::Registry,
    ctx: &ToolCtx,
    redactor: &craft_redact::Redactor,
    call: &ToolCall,
    turn: u32,
    sink: &mut (dyn FnMut(RuntimeEvent) + Send),
) -> (String, bool) {
    let args = parse_args(call);
    let mut stream = registry.execute(&call.name, args, ctx);
    while let Some(item) = stream.next().await {
        match item {
            Ok(ToolEvent::Progress(progress)) => sink(RuntimeEvent::ToolProgress {
                turn,
                name: call.name.clone(),
                stream: stream_name(progress.stream),
                text: redactor.redact(&progress.text).into_owned(),
            }),
            Ok(ToolEvent::Terminal(result)) => {
                return (redactor.redact(&result.output).into_owned(), false);
            }
            Err(err) => return (redactor.redact(&err.to_string()).into_owned(), true),
        }
    }
    (
        String::from("tool stream ended without a terminal result"),
        true,
    )
}

fn parse_args(call: &ToolCall) -> serde_json::Value {
    let trimmed = call.arguments.trim();
    if trimmed.is_empty() {
        return serde_json::Value::Object(serde_json::Map::new());
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| {
        serde_json::Value::String(format!("invalid tool arguments JSON: {}", call.arguments))
    })
}

fn stream_name(stream: OutStream) -> ToolStreamName {
    match stream {
        OutStream::Stdout => ToolStreamName::Stdout,
        OutStream::Stderr => ToolStreamName::Stderr,
    }
}
