//! Image attachments: base64 images ride as inlineData parts, a url
//! source is a typed rejection before any wire bytes, and message
//! shapes without images stay plain.

use crate::mock::{CannedResponse, MockServer};
use crate::wire;
use crate::{TestError, provider, run, sample_request, tool_call};
use craft_core::error::ProviderError;
use craft_core::types::{ChatMessage, ImagePart, ModelRequest};
use serde_json::Value;
use std::sync::Arc;

fn png(data: &str) -> ImagePart {
    ImagePart::Base64 {
        media_type: String::from("image/png"),
        data: String::from(data),
    }
}

/// The encoded body of the one request a turn sends, served a plain
/// text stream.
async fn body_for(request: ModelRequest) -> Result<Value, TestError> {
    let server = MockServer::start(
        vec![CannedResponse::sse(wire::text_stream().as_bytes())],
        "",
    )
    .await?;
    let events = run(&provider(&server.base_url)?, request).await;
    assert!(!events.is_empty(), "the served stream must produce events");
    let sent = server
        .request(0)
        .ok_or("the adapter must send a request")?
        .body;
    Ok(serde_json::from_str(&sent)?)
}
#[tokio::test]
async fn base64_images_ride_inline_beside_the_text() -> Result<(), TestError> {
    let mut request = sample_request();
    request.messages = vec![Arc::new(ChatMessage::User {
        text: String::from("Describe this."),
        images: vec![
            png("aGk="),
            ImagePart::Base64 {
                media_type: String::from("image/webp"),
                data: String::from("d2Vi"),
            },
        ],
    })];
    let body = body_for(request).await?;
    assert_eq!(
        body["contents"][0],
        serde_json::json!({
            "role": "user",
            "parts": [
                {"text": "Describe this."},
                {"inlineData": {"mimeType": "image/png", "data": "aGk="}},
                {"inlineData": {"mimeType": "image/webp", "data": "d2Vi"}},
            ]
        })
    );
    let mut request = sample_request();
    request.messages = vec![Arc::new(ChatMessage::User {
        text: String::new(),
        images: vec![png("aGk=")],
    })];
    let body = body_for(request).await?;
    assert_eq!(
        body["contents"][0]["parts"],
        serde_json::json!([{"inlineData": {"mimeType": "image/png", "data": "aGk="}}]),
        "empty text rides no part when images carry the content"
    );
    Ok(())
}
#[tokio::test]
async fn url_images_are_rejected_before_the_wire() -> Result<(), TestError> {
    let server = MockServer::start(Vec::new(), "").await?;
    let mut request = sample_request();
    request.messages = vec![Arc::new(ChatMessage::User {
        text: String::from("look"),
        images: vec![ImagePart::Url {
            url: String::from("https://example.test/shot.png"),
        }],
    })];
    let events = run(&provider(&server.base_url)?, request).await;
    assert_eq!(server.request_count(), 0, "no request leaves the adapter");
    assert!(
        matches!(
            events.first(),
            Some(Err(ProviderError::InvalidRequest { .. }))
        ),
        "a url image has no inline form: {events:?}"
    );
    Ok(())
}

#[tokio::test]
async fn messages_without_images_keep_the_plain_parts_shape() -> Result<(), TestError> {
    let body = body_for(sample_request()).await?;
    assert_eq!(
        body["contents"][0]["parts"],
        serde_json::json!([{"text": "Summarize the merged PRs."}]),
        "no images, no inlineData parts"
    );
    Ok(())
}
#[tokio::test]
async fn tool_result_images_ride_after_the_function_response() -> Result<(), TestError> {
    let mut request = sample_request();
    request.messages = vec![
        Arc::new(ChatMessage::Assistant {
            text: String::new(),
            tool_calls: vec![tool_call("call_a", "bash", "{\"cmd\":\"ls\"}")],
            native_blocks: None,
        }),
        Arc::new(ChatMessage::Tool {
            call_id: String::from("call_a"),
            content: String::from("{\"output\": \"0 files\"}"),
            is_error: false,
            images: vec![png("aGk=")],
        }),
    ];
    let body = body_for(request).await?;
    assert_eq!(
        body["contents"][1],
        serde_json::json!({
            "role": "user",
            "parts": [
                {"functionResponse": {
                    "id": "call_a",
                    "name": "bash",
                    "response": {"output": "0 files"},
                }},
                {"inlineData": {"mimeType": "image/png", "data": "aGk="}},
            ]
        }),
        "attached images ride after the answered call"
    );
    Ok(())
}
