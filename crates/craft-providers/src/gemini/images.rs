//! Image attachments: base64 payloads ride as inlineData parts; the
//! wire has no inline form for a url image.

use craft_core::error::ProviderError;
use craft_core::types::ImagePart;
use serde_json::Value;

/// The parts of one user content: the text part, then one part per
/// attached image. No images keeps the plain single-text-part shape;
/// with images the text part rides only when non-empty.
pub(super) fn parts(text: &str, images: &[ImagePart]) -> Result<Vec<Value>, ProviderError> {
    if images.is_empty() {
        return Ok(vec![serde_json::json!({"text": text})]);
    }
    let mut parts = Vec::with_capacity(images.len() + 1);
    if !text.is_empty() {
        parts.push(serde_json::json!({"text": text}));
    }
    for image in images {
        parts.push(inline(image)?);
    }
    Ok(parts)
}

/// The bare image parts of a tool result, riding after its
/// functionResponse part.
pub(super) fn inline_parts(images: &[ImagePart]) -> Result<Vec<Value>, ProviderError> {
    images.iter().map(inline).collect()
}
fn inline(image: &ImagePart) -> Result<Value, ProviderError> {
    match image {
        ImagePart::Base64 { media_type, data } => Ok(serde_json::json!({
            "inlineData": {"mimeType": media_type, "data": data}
        })),
        ImagePart::Url { url } => Err(ProviderError::InvalidRequest {
            detail: format!("the generateContent wire has no inline form for a url image: {url}"),
        }),
    }
}
