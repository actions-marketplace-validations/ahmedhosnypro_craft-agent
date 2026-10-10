//! The model registry: every documented row pinned to its documented
//! numbers, longest-prefix resolution, and the three known-model gates
//! on the generateContent wire.

use std::sync::Arc;

use crate::mock::{CannedResponse, MockServer};
use crate::wire;
use crate::{KEY, TestError, run, sample_request, tool_call};
use craft_core::error::ProviderError;
use craft_core::types::{ChatMessage, ImagePart, ModelRequest, Thinking};
use craft_providers::gemini::GeminiProvider;
use craft_providers::gemini::registry::{self, ModelInfo};
use serde_json::Value;

/// The documented rows: prefix, input limit, output limit, thinking,
/// image input, one row per model page.
const OFFICIAL: &[(&str, u32, u32, bool, bool)] = &[
    ("gemini-2.0-flash", 1_048_576, 8_192, false, true),
    ("gemini-2.0-flash-lite", 1_048_576, 8_192, false, true),
    ("gemini-2.5-flash", 1_048_576, 65_536, true, true),
    ("gemini-2.5-flash-preview-tts", 8_192, 16_384, false, false),
    ("gemini-2.5-flash-lite", 1_048_576, 65_536, true, true),
    ("gemini-2.5-pro", 1_048_576, 65_536, true, true),
    ("gemini-3-flash", 1_048_576, 65_536, true, true),
    ("gemini-3.1-flash-lite", 1_048_576, 65_536, true, true),
    ("gemini-3.1-pro", 1_048_576, 65_536, true, true),
    ("gemini-3.5-flash-lite", 1_048_576, 65_536, true, true),
    ("gemini-3.6-flash", 1_048_576, 65_536, true, true),
    ("gemini-3.8-flash", 1_048_576, 65_536, true, true),
    ("gemini-3.8-flash-tts", 8_192, 16_384, false, false),
];

fn adapter(model: &str, base_url: &str) -> Result<GeminiProvider, TestError> {
    GeminiProvider::new(base_url, model, KEY).map_err(|err| Box::new(err) as TestError)
}

/// The encoded body of the one request a turn sends, served a plain
/// text stream, with the model under test.
async fn body_for(model: &str, request: ModelRequest) -> Result<Value, TestError> {
    let server = MockServer::start(
        vec![CannedResponse::sse(wire::text_stream().as_bytes())],
        "",
    )
    .await?;
    let events = run(&adapter(model, &server.base_url)?, request).await;
    assert!(!events.is_empty(), "the served stream must produce events");
    let sent = server
        .request(0)
        .ok_or("the adapter must send a request")?
        .body;
    Ok(serde_json::from_str(&sent)?)
}

/// A request whose one user message carries a base64 image.
fn image_request() -> ModelRequest {
    let mut request = sample_request();
    request.messages[0] = Arc::new(ChatMessage::User {
        text: String::from("What is in this picture?"),
        images: vec![ImagePart::Base64 {
            media_type: String::from("image/png"),
            data: String::from("aGVsbG8="),
        }],
    });
    request
}

#[test]
fn every_documented_row_carries_the_documented_numbers() {
    let rows = registry::families();
    assert_eq!(
        rows.len(),
        OFFICIAL.len(),
        "the registry and the pinned table must cover the same rows"
    );
    for (prefix, context_window_tokens, max_output_tokens, thinking, multimodal) in OFFICIAL {
        let expected = ModelInfo {
            context_window_tokens: *context_window_tokens,
            max_output_tokens: *max_output_tokens,
            thinking: *thinking,
            multimodal: *multimodal,
        };
        assert_eq!(registry::lookup(prefix), Some(expected), "{prefix}");
    }
}

#[test]
fn versioned_ids_resolve_by_longest_prefix() {
    // the text-only variants outrank the flash prefixes they extend
    assert_ne!(
        registry::lookup("gemini-2.5-flash-preview-tts"),
        registry::lookup("gemini-2.5-flash"),
        "a text-only id must not ride the flash row"
    );
    assert_ne!(
        registry::lookup("gemini-3.8-flash-tts"),
        registry::lookup("gemini-3.8-flash"),
        "a text-only id must not ride the flash row"
    );
    assert_eq!(
        registry::lookup("gemini-2.5-flash-preview-09-2025"),
        registry::lookup("gemini-2.5-flash"),
        "a dated preview id rides the family row"
    );
    assert_eq!(
        registry::lookup("gemini-3.1-pro-preview-customtools"),
        registry::lookup("gemini-3.1-pro")
    );
    assert_eq!(
        registry::lookup("gemini-2.0-flash-001"),
        registry::lookup("gemini-2.0-flash")
    );
}

#[test]
fn unknown_ids_resolve_to_nothing() {
    for id in [
        "",
        "gemini",
        "gemini-1.5-pro",
        "gemini-9.9-pro",
        "not-a-model",
    ] {
        assert_eq!(registry::lookup(id), None, "{id} must stay unknown");
    }
}

#[tokio::test]
async fn output_caps_clamp_to_the_known_row() -> Result<(), TestError> {
    for (model, cap) in [
        ("gemini-2.5-flash", 65_536),
        ("gemini-2.0-flash", 8_192),
        ("gemini-2.5-flash-preview-tts", 16_384),
    ] {
        let mut request = sample_request();
        request.max_tokens = u32::MAX;
        let body = body_for(model, request).await?;
        assert_eq!(
            body["generationConfig"]["maxOutputTokens"], cap,
            "{model} clamps to its documented cap"
        );
    }
    Ok(())
}

#[tokio::test]
async fn requests_below_the_cap_ride_unchanged() -> Result<(), TestError> {
    let mut request = sample_request();
    request.max_tokens = 128;
    let body = body_for("gemini-2.5-flash", request).await?;
    assert_eq!(body["generationConfig"]["maxOutputTokens"], 128);
    Ok(())
}

#[tokio::test]
async fn thinking_config_is_omitted_for_known_non_thinking_models() -> Result<(), TestError> {
    for model in [
        "gemini-2.0-flash",
        "gemini-2.0-flash-lite",
        "gemini-3.8-flash-tts",
    ] {
        let mut request = sample_request();
        request.thinking = Some(Thinking {
            budget_tokens: 2048,
            include_thoughts: true,
        });
        let body = body_for(model, request).await?;
        assert!(
            body["generationConfig"].get("thinkingConfig").is_none(),
            "{model} must not carry a thinking request"
        );
    }
    Ok(())
}

#[tokio::test]
async fn thinking_config_rides_for_known_thinking_models() -> Result<(), TestError> {
    let mut request = sample_request();
    request.thinking = Some(Thinking {
        budget_tokens: 2048,
        include_thoughts: false,
    });
    let body = body_for("gemini-2.5-pro", request).await?;
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        serde_json::json!({"thinkingBudget": 2048})
    );
    Ok(())
}

#[tokio::test]
async fn images_on_known_non_multimodal_models_fail_typed_before_the_wire() -> Result<(), TestError>
{
    for model in ["gemini-2.5-flash-preview-tts", "gemini-3.8-flash-tts"] {
        let server = MockServer::start(Vec::new(), "").await?;
        let events = run(&adapter(model, &server.base_url)?, image_request()).await;
        assert_eq!(
            server.request_count(),
            0,
            "{model}: no request leaves the adapter"
        );
        assert!(
            matches!(
                events.first(),
                Some(Err(ProviderError::InvalidRequest { .. }))
            ),
            "{model}: an image must fail typed: {events:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn tool_result_images_gate_the_same_way() -> Result<(), TestError> {
    let server = MockServer::start(Vec::new(), "").await?;
    let mut request = sample_request();
    request.messages = vec![
        Arc::new(ChatMessage::Assistant {
            text: String::new(),
            tool_calls: vec![tool_call("call_a", "inspect", "{}")],
            native_blocks: None,
        }),
        Arc::new(ChatMessage::Tool {
            call_id: String::from("call_a"),
            content: String::from("photo"),
            is_error: false,
            images: vec![ImagePart::Base64 {
                media_type: String::from("image/png"),
                data: String::from("aGVsbG8="),
            }],
        }),
    ];
    let events = run(
        &adapter("gemini-2.5-flash-preview-tts", &server.base_url)?,
        request,
    )
    .await;
    assert_eq!(server.request_count(), 0, "no request leaves the adapter");
    assert!(
        matches!(
            events.first(),
            Some(Err(ProviderError::InvalidRequest { .. }))
        ),
        "a tool-result image must fail typed: {events:?}"
    );
    Ok(())
}

#[tokio::test]
async fn unknown_ids_keep_every_gate_off() -> Result<(), TestError> {
    let mut request = sample_request();
    request.max_tokens = u32::MAX;
    request.thinking = Some(Thinking {
        budget_tokens: 512,
        include_thoughts: true,
    });
    let body = body_for("gemini-9.9-pro", request).await?;
    assert_eq!(
        body["generationConfig"]["maxOutputTokens"], 4_294_967_295u64,
        "no cap is applied without a known row"
    );
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        serde_json::json!({"thinkingBudget": 512, "includeThoughts": true}),
        "the thinking request rides without a known row"
    );
    Ok(())
}

#[tokio::test]
async fn images_on_unknown_ids_pass_through() -> Result<(), TestError> {
    let server = MockServer::start(
        vec![CannedResponse::sse(wire::text_stream().as_bytes())],
        "",
    )
    .await?;
    let events = run(
        &adapter("gemini-9.9-pro", &server.base_url)?,
        image_request(),
    )
    .await;
    assert!(!events.is_empty(), "the served stream must produce events");
    let body = serde_json::from_str::<Value>(&server.request(0).ok_or("a request")?.body)?;
    assert!(
        body["contents"][0]["parts"][1]["inlineData"].is_object(),
        "an unknown id rides the image through ungated"
    );
    Ok(())
}
