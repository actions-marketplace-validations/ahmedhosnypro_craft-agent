//! Neutral-to-wire request encoding for the generateContent dialect.

use craft_core::error::ProviderError;
use craft_core::types::{ChatMessage, ModelRequest, NativeDialect, ToolChoice};
use serde_json::Value;
use std::sync::Arc;

use super::generation;
use super::images;
use super::registry;

/// Neutral request to documented generateContent JSON. Native replay
/// blocks from this dialect re-ride verbatim; another dialect is a typed
/// rejection, never a re-encode. Consecutive tool results ride as one
/// user content of functionResponse parts, answering the model turn. A
/// known model's registry row clamps the output cap, omits the thinking
/// request it cannot honor, and rejects image input; unknown ids pass
/// through untouched.
pub(super) fn encode(request: &ModelRequest, model: &str) -> Result<Value, ProviderError> {
    let known = registry::lookup(model);
    if let Some(info) = known
        && !info.multimodal
        && has_images(&request.messages)
    {
        return Err(ProviderError::InvalidRequest {
            detail: format!("model {model} accepts no image input"),
        });
    }
    let mut contents = Vec::new();
    let mut results = Vec::new();
    for (index, message) in request.messages.iter().enumerate() {
        match message.as_ref() {
            ChatMessage::Tool { .. } => {
                results.extend(tool_response_parts(message, &request.messages[..index])?);
            }
            _ => {
                flush_results(&mut contents, &mut results);
                contents.push(encode_message(message)?);
            }
        }
    }
    flush_results(&mut contents, &mut results);
    let mut body = serde_json::json!({
        "contents": contents,
        "generationConfig": generation::config(request, known)?,
    });
    if let Some(system) = request
        .system
        .as_deref()
        .filter(|system| !system.is_empty())
    {
        let map = object_mut(&mut body)?;
        map.insert(
            String::from("systemInstruction"),
            serde_json::json!({"parts": [{"text": system}]}),
        );
    }
    if !request.tools.is_empty() {
        let declarations: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parametersJsonSchema": tool.parameters,
                })
            })
            .collect();
        let map = object_mut(&mut body)?;
        map.insert(
            String::from("tools"),
            serde_json::json!([{"functionDeclarations": declarations}]),
        );
        if let Some(choice) = &request.tool_choice {
            map.insert(String::from("toolConfig"), tool_config(choice));
        }
    }
    Ok(body)
}

/// The functionCallingConfig form of the choice knob: AUTO, ANY, or ANY
/// narrowed to one named function.
fn tool_config(choice: &ToolChoice) -> Value {
    let mut function_calling = serde_json::Map::new();
    match choice {
        ToolChoice::Auto => {
            function_calling.insert(String::from("mode"), Value::from("AUTO"));
        }
        ToolChoice::Any => {
            function_calling.insert(String::from("mode"), Value::from("ANY"));
        }
        ToolChoice::Tool { name } => {
            function_calling.insert(String::from("mode"), Value::from("ANY"));
            function_calling.insert(
                String::from("allowedFunctionNames"),
                serde_json::json!([name]),
            );
        }
    }
    serde_json::json!({"functionCallingConfig": function_calling})
}

/// One neutral message to one wire content. Tool messages never reach
/// here: they flush through tool_response_part as a grouped content.
fn encode_message(message: &ChatMessage) -> Result<Value, ProviderError> {
    match message {
        ChatMessage::User { text, images } => Ok(serde_json::json!({
            "role": "user",
            "parts": images::parts(text, images)?
        })),
        ChatMessage::Assistant {
            text,
            tool_calls,
            native_blocks,
        } => {
            let mut parts = Vec::new();
            if let Some(blocks) = native_blocks {
                parts.extend(native_parts(blocks)?);
            }
            if !text.is_empty() {
                parts.push(serde_json::json!({"text": text}));
            }
            for call in tool_calls {
                let mut function = serde_json::Map::new();
                if !call.id.is_empty() {
                    function.insert(String::from("id"), Value::String(call.id.clone()));
                }
                function.insert(String::from("name"), Value::String(call.name.clone()));
                function.insert(String::from("args"), wire_args(&call.arguments)?);
                parts.push(serde_json::json!({"functionCall": function}));
            }
            Ok(serde_json::json!({"role": "model", "parts": parts}))
        }
        ChatMessage::Tool { .. } => Err(ProviderError::Transport {
            detail: String::from("tool messages encode through tool_response_parts"),
        }),
    }
}

/// One tool result as its functionResponse part, then any attached
/// images as inlineData parts.
fn tool_response_parts(
    message: &ChatMessage,
    prior: &[Arc<ChatMessage>],
) -> Result<Vec<Value>, ProviderError> {
    let ChatMessage::Tool {
        call_id,
        content,
        is_error,
        images: attached,
        ..
    } = message
    else {
        return Err(ProviderError::Transport {
            detail: String::from("tool_response_parts requires a tool message"),
        });
    };
    let name = resolve_call_name(prior, call_id)?;
    let mut response = serde_json::Map::new();
    if !call_id.is_empty() {
        response.insert(String::from("id"), Value::String(call_id.clone()));
    }
    response.insert(String::from("name"), Value::String(name));
    response.insert(
        String::from("response"),
        if *is_error {
            serde_json::json!({"error": content})
        } else {
            wire_response(content)
        },
    );
    let mut parts = vec![serde_json::json!({"functionResponse": response})];
    parts.extend(images::inline_parts(attached)?);
    Ok(parts)
}

/// Consecutive tool results flush as one user content of functionResponse
/// parts, immediately after the model turn they answer.
fn flush_results(contents: &mut Vec<Value>, results: &mut Vec<Value>) {
    if results.is_empty() {
        return;
    }
    let parts = std::mem::take(results);
    contents.push(serde_json::json!({"role": "user", "parts": parts}));
}

/// Native replay parts: this dialect only, array form only, verbatim.
fn native_parts(blocks: &craft_core::types::NativeBlocks) -> Result<Vec<Value>, ProviderError> {
    if blocks.dialect != NativeDialect::Gemini {
        return Err(ProviderError::InvalidRequest {
            detail: String::from("native replay blocks belong to another dialect"),
        });
    }
    let Value::Array(parts) = &blocks.blocks else {
        return Err(ProviderError::InvalidRequest {
            detail: String::from("native replay blocks must be a parts array"),
        });
    };
    Ok(parts.clone())
}

/// functionResponse requires the function name, so the result resolves
/// it from the assistant call it answers. Ids pair a result with a call
/// of the nearest earlier assistant turn; repeated or empty ids pair
/// positionally, in call and result order.
fn resolve_call_name(prior: &[Arc<ChatMessage>], call_id: &str) -> Result<String, ProviderError> {
    let mut earlier = 0usize;
    for message in prior.iter().rev() {
        match message.as_ref() {
            ChatMessage::Tool {
                call_id: result_id, ..
            } if result_id == call_id => earlier = earlier.saturating_add(1),
            ChatMessage::Assistant { tool_calls, .. } => {
                return match tool_calls
                    .iter()
                    .filter(|call| call.id == call_id)
                    .nth(earlier)
                {
                    Some(call) => Ok(call.name.clone()),
                    None => Err(ProviderError::InvalidRequest {
                        detail: format!("no assistant tool call pairs with result id {call_id}"),
                    }),
                };
            }
            _ => {}
        }
    }
    Err(ProviderError::InvalidRequest {
        detail: format!("no assistant tool call pairs with result id {call_id}"),
    })
}

/// Whether any message carries an image attachment.
fn has_images(messages: &[Arc<ChatMessage>]) -> bool {
    messages.iter().any(|message| match message.as_ref() {
        ChatMessage::User { images, .. } | ChatMessage::Tool { images, .. } => !images.is_empty(),
        ChatMessage::Assistant { .. } => false,
    })
}

fn object_mut(value: &mut Value) -> Result<&mut serde_json::Map<String, Value>, ProviderError> {
    value
        .as_object_mut()
        .ok_or_else(|| ProviderError::Transport {
            detail: String::from("the encoded body must be an object"),
        })
}

/// A functionCall's args: the neutral arguments are raw JSON-object
/// text; the wire wants the object itself. An empty text is the empty
/// object, anything unparsable or non-object is a typed rejection.
fn wire_args(arguments: &str) -> Result<Value, ProviderError> {
    let parsed = if arguments.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(arguments).map_err(|err| ProviderError::InvalidRequest {
            detail: format!("tool arguments are not valid JSON: {err}"),
        })?
    };
    if !parsed.is_object() {
        return Err(ProviderError::InvalidRequest {
            detail: String::from("tool arguments must be a JSON object"),
        });
    }
    Ok(parsed)
}

/// The documented response object: an error result rides the error key,
/// a JSON-object result rides verbatim, any other text wraps in output.
fn wire_response(content: &str) -> Value {
    if let Ok(parsed) = serde_json::from_str::<Value>(content)
        && parsed.is_object()
    {
        return parsed;
    }
    serde_json::json!({"output": content})
}
