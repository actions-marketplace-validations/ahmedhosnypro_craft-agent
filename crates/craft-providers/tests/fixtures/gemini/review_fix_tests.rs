//! Review-driven regression rows: signature capture feeds native replay,
//! parallel results merge into one user content, id-less results pair
//! positionally, and mid-stream error envelopes map onto the taxonomy.

use crate::mock::{CannedResponse, MockServer};
use crate::{TestError, detail_of, folded, provider, run, sample_request, surfaced, tool_call};
use craft_core::error::ProviderError;
use craft_core::events::ModelEvent;
use craft_core::types::{ChatMessage, ModelRequest, NativeDialect, ToolCall};

async fn serve(body: &str) -> Result<MockServer, TestError> {
    Ok(MockServer::start(vec![CannedResponse::sse(body.as_bytes())], "").await?)
}

async fn serve_stream() -> Result<MockServer, TestError> {
    serve(&crate::wire::text_stream()).await
}

/// The captured request body as JSON: one round-trip, one request.
fn request_body(server: &MockServer) -> Result<serde_json::Value, TestError> {
    let raw = &server.request(0).ok_or("a request")?.body;
    Ok(serde_json::from_str(raw)?)
}

/// An assistant tool-call turn followed by its tool results, as
/// (call_id, content, is_error) rows.
fn results_request(calls: Vec<ToolCall>, results: Vec<(&str, &str, bool)>) -> ModelRequest {
    let mut messages = vec![std::sync::Arc::new(ChatMessage::Assistant {
        text: String::new(),
        tool_calls: calls,
        native_blocks: None,
    })];
    for (call_id, content, is_error) in results {
        messages.push(std::sync::Arc::new(ChatMessage::Tool {
            call_id: String::from(call_id),
            content: String::from(content),
            is_error,
            images: Vec::new(),
        }));
    }
    let mut request = sample_request();
    request.messages = messages;
    request
}

/// Drives the request over the sample stream and yields the JSON body the
/// adapter sent.
async fn round_trip_body(request: ModelRequest) -> Result<serde_json::Value, TestError> {
    let server = serve_stream().await?;
    let events = run(&provider(&server.base_url)?, request).await;
    assert!(!events.is_empty());
    request_body(&server)
}

#[tokio::test]
async fn thought_and_signature_parts_fold_into_native_replay_blocks() -> Result<(), TestError> {
    let chunk_a = r#"data: {"candidates":[{"content":{"parts":[{"text":"pondering","thought":true},{"functionCall":{"id":"call_a","name":"bash","args":{"cmd":"ls"}},"thoughtSignature":"c2ln"},{"thoughtSignature":"YmFyZQ"}],"role":"model"},"index":0}],"modelVersion":"gemini-2.5-flash"}"#;
    let chunk_b = r#"data: {"candidates":[{"content":{"parts":[{"text":"Done."}],"role":"model"},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":2,"totalTokenCount":11}}"#;
    let body = format!("{chunk_a}\n\n{chunk_b}\n\n");
    let server = serve(&body).await?;
    let events = run(&provider(&server.base_url)?, sample_request()).await;
    let turn = folded(&events)?;
    assert_eq!(turn.text, "Done.");
    assert_eq!(
        turn.tool_calls,
        vec![tool_call("call_a", "bash", "{\"cmd\":\"ls\"}")]
    );
    let blocks = turn
        .native_blocks
        .clone()
        .ok_or("signature and thought parts fold into native blocks")?;
    assert_eq!(blocks.dialect, NativeDialect::Gemini);
    assert_eq!(
        blocks.blocks,
        serde_json::json!([
            {"text": "pondering", "thought": true},
            {"thoughtSignature": "c2ln"},
            {"thoughtSignature": "YmFyZQ"}
        ])
    );
    let mut request = sample_request();
    request.messages = vec![std::sync::Arc::new(ChatMessage::Assistant {
        text: turn.text.clone(),
        tool_calls: turn.tool_calls.clone(),
        native_blocks: turn.native_blocks.clone(),
    })];
    let replay = serve_stream().await?;
    let events = run(&provider(&replay.base_url)?, request).await;
    assert!(!events.is_empty());
    let body: serde_json::Value =
        serde_json::from_str(&replay.request(0).ok_or("a request")?.body)?;
    assert_eq!(
        body["contents"][0]["parts"],
        serde_json::json!([
            {"text": "pondering", "thought": true},
            {"thoughtSignature": "c2ln"},
            {"thoughtSignature": "YmFyZQ"},
            {"text": "Done."},
            {"functionCall": {"id": "call_a", "name": "bash", "args": {"cmd": "ls"}}}
        ]),
        "the folded native blocks replay verbatim ahead of the turn"
    );
    Ok(())
}

#[tokio::test]
async fn parallel_tool_results_merge_into_one_user_content() -> Result<(), TestError> {
    let request = results_request(
        vec![
            tool_call("call_a", "get_weather", "{\"city\":\"Oslo\"}"),
            tool_call("call_b", "bash", "{\"cmd\":\"ls\"}"),
        ],
        vec![("call_a", "3C", false), ("call_b", "0 files", true)],
    );
    let body = round_trip_body(request).await?;
    let contents = body["contents"].as_array().ok_or("contents array")?;
    assert_eq!(
        contents.len(),
        2,
        "consecutive results ride one user content"
    );
    assert_eq!(
        contents[1],
        serde_json::json!({
            "role": "user",
            "parts": [
                {"functionResponse": {"id": "call_a", "name": "get_weather", "response": {"output": "3C"}}},
                {"functionResponse": {"id": "call_b", "name": "bash", "response": {"error": "0 files"}}}
            ]
        })
    );
    Ok(())
}

#[tokio::test]
async fn id_less_parallel_results_pair_positionally() -> Result<(), TestError> {
    let request = results_request(
        vec![
            tool_call("", "get_weather", "{\"city\":\"Oslo\"}"),
            tool_call("", "bash", "{\"cmd\":\"ls\"}"),
        ],
        vec![("", "3C", false), ("", "0 files", false)],
    );
    let body = round_trip_body(request).await?;
    assert_eq!(
        body["contents"][1],
        serde_json::json!({
            "role": "user",
            "parts": [
                {"functionResponse": {"name": "get_weather", "response": {"output": "3C"}}},
                {"functionResponse": {"name": "bash", "response": {"output": "0 files"}}}
            ]
        }),
        "each result pairs with its own call, in call order"
    );
    Ok(())
}

async fn mid_stream_error(status: &str, message: &str) -> Result<ProviderError, TestError> {
    let body = format!(
        "data: {{\"candidates\":[{{\"content\":{{\"parts\":[{{\"text\":\"Hel\"}}],\"role\":\"model\"}},\"index\":0}}]}}\n\ndata: {{\"error\":{{\"code\":500,\"message\":\"{message}\",\"status\":\"{status}\"}}}}\n\n"
    );
    let server = serve(&body).await?;
    let events = run(&provider(&server.base_url)?, sample_request()).await;
    assert!(
        matches!(events.first(), Some(Ok(ModelEvent::TextDelta { .. }))),
        "the content before the error still streams: {events:?}"
    );
    surfaced(&events)
}

#[tokio::test]
async fn mid_stream_error_envelopes_map_onto_the_taxonomy() -> Result<(), TestError> {
    let err = mid_stream_error("INTERNAL", "backend exploded").await?;
    assert!(matches!(err, ProviderError::Transient { .. }), "{err:?}");
    assert!(detail_of(&err).contains("backend exploded"));
    let err = mid_stream_error("RESOURCE_EXHAUSTED", "slow down").await?;
    assert!(matches!(err, ProviderError::RateLimit { .. }), "{err:?}");
    let err = mid_stream_error("UNAUTHENTICATED", "no key").await?;
    assert!(matches!(err, ProviderError::Auth { .. }), "{err:?}");
    let err = mid_stream_error("MYSTERY", "what").await?;
    assert!(matches!(err, ProviderError::Transport { .. }), "{err:?}");
    Ok(())
}
