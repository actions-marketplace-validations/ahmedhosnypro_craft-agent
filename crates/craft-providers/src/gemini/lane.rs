//! The Gemini decode lane: camelCase chunk JSON folds into model
//! events, thought text and signatures fold into native replay blocks,
//! the enum-default finishReason is not a stop, the terminal buffers
//! while accounting frames merge usage, and exactly one TurnFinished
//! leaves the lane when the wire finishes.

use craft_core::error::ProviderError;
use craft_core::events::ModelEvent;
use craft_core::provider::ModelEventStream;
use craft_core::sse::{SseEvent, SseOptions};
use craft_core::types::{NativeDialect, StopReason};

use super::chunk::{WireCandidate, WireChunk, WireFunctionCall, WirePart};
use super::map_error_envelope;
use crate::lane_driver::{LaneCommon, LaneDialect, mount};

/// Mounts the decode lane over one connected response.
pub(super) fn lane_stream(response: reqwest::Response, sse: SseOptions) -> ModelEventStream {
    mount(response, sse, Dialect::default())
}

/// The Gemini decode state: the adapter-assigned tool-call index; the
/// lane driver holds the rest.
#[derive(Debug, Default)]
pub(super) struct Dialect {
    tool_index: u64,
}

impl LaneDialect for Dialect {
    /// One SSE event: the data JSON decodes permissively, a mid-stream
    /// error envelope surfaces typed, usageMetadata merges, a blocked
    /// prompt is the terminal stop, and the first candidate is consumed.
    fn consume(&mut self, common: &mut LaneCommon, event: SseEvent) -> Result<bool, ProviderError> {
        let chunk: WireChunk = match serde_json::from_str(&event.data) {
            Ok(chunk) => chunk,
            Err(err) => return common.anomaly(format!("invalid chunk JSON: {err}")),
        };
        let WireChunk {
            error,
            usage_metadata,
            prompt_feedback,
            candidates,
        } = chunk;
        if let Some(error) = error {
            return Err(map_error_envelope(error));
        }
        if let Some(usage) = usage_metadata {
            common.record_usage(usage.into());
        }
        if let Some(reason) = prompt_feedback.and_then(|feedback| feedback.block_reason) {
            record_terminal(common, StopReason::ContentFilter, &reason)?;
        }
        if let Some(candidate) = candidates.into_iter().next() {
            consume_candidate(common, self, candidate)?;
        }
        Ok(false)
    }
}

/// The first candidate's parts map onto delta events and its definitive
/// finishReason is the terminal stop. The request never asks for
/// more than one candidate, so later ones are ignored.
fn consume_candidate(
    common: &mut LaneCommon,
    dialect: &mut Dialect,
    candidate: WireCandidate,
) -> Result<(), ProviderError> {
    if let Some(content) = candidate.content {
        for part in content.parts {
            push_part(common, dialect, part)?;
        }
    }
    if let Some(reason) = candidate.finish_reason
        && !is_default_finish(&reason)
    {
        record_terminal(common, StopReason::from_gemini(&reason), &reason)?;
    }
    Ok(())
}

/// The enum-default finishReason is not a stop: the wire keeps
/// streaming and the definitive reason lands later. Both documented
/// spellings count, in any case.
fn is_default_finish(reason: &str) -> bool {
    reason.eq_ignore_ascii_case("UNSPECIFIED")
        || reason.eq_ignore_ascii_case("FINISH_REASON_UNSPECIFIED")
}

/// One candidate part: thought text and signatures fold into native
/// replay blocks, visible text streams, and a function call lands whole
/// on the next adapter-assigned index.
fn push_part(
    common: &mut LaneCommon,
    dialect: &mut Dialect,
    part: WirePart,
) -> Result<(), ProviderError> {
    let WirePart {
        thought,
        text,
        function_call,
        thought_signature,
    } = part;
    push_native(common, thought, text.as_deref(), thought_signature)?;
    if let Some(call) = function_call {
        return push_tool_call(common, dialect, call);
    }
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return Ok(());
    };
    if thought {
        return Ok(());
    }
    if common.terminal().is_some() {
        return Err(ProviderError::Transport {
            detail: String::from("content delta arrived after finishReason"),
        });
    }
    common.push_pending(ModelEvent::TextDelta { text });
    Ok(())
}

/// Thought text rides its own native part; a signature on any other
/// part rides a bare one — both replay verbatim ahead of the turn, and
/// one after the terminal is a corrupt stream.
fn push_native(
    common: &mut LaneCommon,
    thought: bool,
    text: Option<&str>,
    signature: Option<String>,
) -> Result<(), ProviderError> {
    let mut block = serde_json::Map::new();
    if thought && let Some(text) = text.filter(|text| !text.is_empty()) {
        block.insert(String::from("text"), serde_json::Value::from(text));
        block.insert(String::from("thought"), serde_json::Value::from(true));
    }
    if let Some(signature) = signature {
        block.insert(
            String::from("thoughtSignature"),
            serde_json::Value::from(signature),
        );
    }
    if block.is_empty() {
        return Ok(());
    }
    if common.terminal().is_some() {
        return Err(ProviderError::Transport {
            detail: String::from("native part arrived after finishReason"),
        });
    }
    common.push_pending(ModelEvent::NativeBlock {
        dialect: NativeDialect::Gemini,
        block: serde_json::Value::Object(block),
    });
    Ok(())
}

fn push_tool_call(
    common: &mut LaneCommon,
    dialect: &mut Dialect,
    call: WireFunctionCall,
) -> Result<(), ProviderError> {
    if common.terminal().is_some() {
        return Err(ProviderError::Transport {
            detail: String::from("functionCall part arrived after finishReason"),
        });
    }
    let arguments = serde_json::to_string(&call.args.unwrap_or_else(|| serde_json::json!({})))
        .map_err(|err| ProviderError::Transport {
            detail: format!("function args did not re-serialize: {err}"),
        })?;
    common.push_pending(ModelEvent::ToolCallDelta {
        index: dialect.tool_index,
        id: call.id,
        name: call.name,
        arguments,
    });
    dialect.tool_index = dialect.tool_index.saturating_add(1);
    Ok(())
}

fn record_terminal(
    common: &mut LaneCommon,
    stop_reason: StopReason,
    raw: &str,
) -> Result<(), ProviderError> {
    match common.terminal() {
        None => {
            common.set_terminal(stop_reason);
            Ok(())
        }
        Some(known) if known == stop_reason => Ok(()),
        Some(_) => Err(ProviderError::Transport {
            detail: format!("conflicting finishReason: {raw}"),
        }),
    }
}
