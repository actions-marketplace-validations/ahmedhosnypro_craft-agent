//! The generationConfig object: the output cap, the sampling knobs, and
//! the thinking request, each riding only when the caller set it; a
//! known model clamps the cap and omits the thinking request it cannot
//! honor.

use craft_core::error::ProviderError;
use craft_core::types::{ModelRequest, Sampling, Thinking};
use serde_json::{Map, Value};

use super::registry::ModelInfo;

/// The body's generationConfig: maxOutputTokens always, clamped to a
/// known model's documented cap; temperature, topP, and stopSequences
/// only when set; thinkingConfig only when set and the known model
/// thinks.
pub(super) fn config(
    request: &ModelRequest,
    known: Option<ModelInfo>,
) -> Result<Value, ProviderError> {
    let mut config = Map::new();
    let max_output_tokens = known.map_or(request.max_tokens, |info| {
        request.max_tokens.min(info.max_output_tokens)
    });
    config.insert(
        String::from("maxOutputTokens"),
        Value::from(max_output_tokens),
    );
    if let Some(sampling) = &request.sampling {
        sampling_fields(&mut config, sampling)?;
    }
    if let Some(thinking) = &request.thinking
        && known.is_none_or(|info| info.thinking)
    {
        config.insert(String::from("thinkingConfig"), thinking_config(thinking));
    }
    Ok(Value::Object(config))
}
/// Finite temperature and topP as JSON numbers, stopSequences only when
/// set; anything unset keeps the provider default.
fn sampling_fields(
    config: &mut Map<String, Value>,
    sampling: &Sampling,
) -> Result<(), ProviderError> {
    for (field, value) in [
        ("temperature", sampling.temperature),
        ("topP", sampling.top_p),
    ] {
        let Some(value) = value else {
            continue;
        };
        if !value.is_finite() {
            return Err(ProviderError::InvalidRequest {
                detail: format!("{field} must be a finite number"),
            });
        }
        config.insert(String::from(field), serde_json::json!(value));
    }
    if !sampling.stop_sequences.is_empty() {
        config.insert(
            String::from("stopSequences"),
            serde_json::json!(sampling.stop_sequences),
        );
    }
    Ok(())
}

/// The thinking request: the budget always, includeThoughts only when
/// the caller asked for surfaced thoughts.
fn thinking_config(thinking: &Thinking) -> Value {
    let mut config = Map::new();
    config.insert(
        String::from("thinkingBudget"),
        Value::from(thinking.budget_tokens),
    );
    if thinking.include_thoughts {
        config.insert(String::from("includeThoughts"), Value::Bool(true));
    }
    Value::Object(config)
}
