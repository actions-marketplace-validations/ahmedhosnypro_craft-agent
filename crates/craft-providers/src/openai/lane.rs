//! The Chat Completions decode lane: data-line chunks fold into model
//! events, the terminal buffers while accounting frames merge usage, and
//! exactly one TurnFinished leaves the lane when the wire finishes.

use craft_core::error::ProviderError;
use craft_core::events::ModelEvent;
use craft_core::provider::ModelEventStream;
use craft_core::sse::{SseEvent, SseOptions};
use craft_core::types::StopReason;

use super::chunk::{WireChoice, WireChunk, WireToolCall};
use super::map_error_envelope;
use crate::lane_driver::{LaneCommon, LaneDialect, mount};

const DONE_MARK: &str = "[DONE]";

/// Mounts the decode lane over one connected response.
pub(super) fn lane_stream(response: reqwest::Response, sse: SseOptions) -> ModelEventStream {
    mount(response, sse, Dialect)
}

/// The Chat Completions decode state; the lane driver holds the rest.
pub(super) struct Dialect;

impl LaneDialect for Dialect {
    /// One SSE event: the terminator ends the lane, error envelopes
    /// surface typed, usage merges, and the first choice maps onto delta
    /// events. The request never asks for more than one completion, so
    /// later choices are ignored.
    fn consume(&mut self, common: &mut LaneCommon, event: SseEvent) -> Result<bool, ProviderError> {
        if event.data.trim() == DONE_MARK {
            return Ok(true);
        }
        let chunk: WireChunk = match serde_json::from_str(&event.data) {
            Ok(chunk) => chunk,
            Err(err) => return common.anomaly(format!("invalid chunk JSON: {err}")),
        };
        apply_chunk(common, chunk)
    }
}

fn apply_chunk(common: &mut LaneCommon, chunk: WireChunk) -> Result<bool, ProviderError> {
    if let Some(error) = chunk.error {
        return Err(map_error_envelope(error));
    }
    if let Some(usage) = chunk.usage {
        common.record_usage(usage.into());
    }
    if let Some(choice) = chunk.choices.into_iter().next() {
        apply_choice(common, choice)?;
    }
    Ok(false)
}

fn apply_choice(common: &mut LaneCommon, choice: WireChoice) -> Result<(), ProviderError> {
    if let Some(delta) = choice.delta {
        push_content(common, delta.content)?;
        for fragment in delta.tool_calls {
            push_tool_fragment(common, fragment)?;
        }
    }
    if let Some(reason) = choice.finish_reason {
        record_terminal(common, reason)?;
    }
    Ok(())
}

fn push_content(common: &mut LaneCommon, content: Option<String>) -> Result<(), ProviderError> {
    let Some(text) = content.filter(|text| !text.is_empty()) else {
        return Ok(());
    };
    if common.terminal().is_some() {
        return Err(ProviderError::Transport {
            detail: String::from("content delta arrived after finish_reason"),
        });
    }
    common.push_pending(ModelEvent::TextDelta { text });
    Ok(())
}

fn push_tool_fragment(
    common: &mut LaneCommon,
    fragment: WireToolCall,
) -> Result<(), ProviderError> {
    let function = fragment.function.unwrap_or_default();
    let name = function.name;
    let arguments = function.arguments.unwrap_or_default();
    if blank_fragment(fragment.id.as_deref(), name.as_deref(), &arguments) {
        return Ok(());
    }
    if common.terminal().is_some() {
        return Err(ProviderError::Transport {
            detail: String::from("tool-call delta arrived after finish_reason"),
        });
    }
    common.push_pending(ModelEvent::ToolCallDelta {
        index: fragment.index,
        id: fragment.id,
        name,
        arguments,
    });
    Ok(())
}

/// A fragment with no id, name, or arguments carries no signal.
fn blank_fragment(id: Option<&str>, name: Option<&str>, arguments: &str) -> bool {
    id.is_none() && name.is_none() && arguments.is_empty()
}

fn record_terminal(common: &mut LaneCommon, reason: String) -> Result<(), ProviderError> {
    let stop_reason = StopReason::from_openai(&reason);
    match common.terminal() {
        None => {
            common.set_terminal(stop_reason);
            Ok(())
        }
        Some(known) if known == stop_reason => Ok(()),
        Some(_) => Err(ProviderError::Transport {
            detail: format!("conflicting finish_reason: {reason}"),
        }),
    }
}
