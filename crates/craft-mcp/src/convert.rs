//! Server output is untrusted model input. Every content-block kind
//! gets a bounded, visible rendering — nothing is lost silently — and
//! the converted text rides the same 256 KiB cap as bash output.
//! inputSchema bounds guard the definitions the model reads.

use serde_json::Value;

use craft_tools::TRUNCATION_MARKER;

/// Converted tool-time output, capped.
pub const RESULT_CAP: usize = 256 * 1024;
const DESC_CAP: usize = 2048;
/// One tool's serialized inputSchema.
pub const SCHEMA_TOOL_CAP: usize = 64 * 1024;
/// All of one server's accepted schemas.
pub const SCHEMA_SERVER_CAP: usize = 256 * 1024;
/// Composition-keyword nesting the definitions may carry.
pub const MAX_SCHEMA_DEPTH: u32 = 8;
/// Absolute recursion bound for the depth walk.
const WALK_BUDGET: u32 = 64;

/// A tools/call result after conversion: model-visible text, plus the
/// server's own error flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Converted {
    Ok(String),
    IsError(String),
}

fn dropped(kind: &str) -> String {
    format!("[mcp content dropped: {kind} (unsupported in v1)]")
}

fn cap_bytes(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut cut = cap;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n{}", &text[..cut], TRUNCATION_MARKER)
}

/// A tool description: capped at 2 KiB with a truncation marker.
pub fn cap_description(description: &str) -> String {
    if description.len() <= DESC_CAP {
        return description.to_string();
    }
    let mut cut = DESC_CAP - TRUNCATION_MARKER.len() - 1;
    while !description.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n{}", &description[..cut], TRUNCATION_MARKER)
}

fn render_block(block: &Value) -> Result<String, String> {
    let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "text" => block
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| String::from("a text block carried no text")),
        "image" => Ok(dropped("image")),
        "audio" => Ok(dropped("audio")),
        "resource_link" => {
            let field = |name: &str| {
                block
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            Ok(format!(
                "{} {} {}",
                field("uri"),
                field("name"),
                field("description")
            ))
        }
        "resource" | "embedded" => Ok(format!(
            "[mcp resource] {}",
            serde_json::to_string(block).unwrap_or_else(|_| String::from("{}"))
        )),
        "" => Ok(dropped("unnamed content block")),
        other => Ok(dropped(other)),
    }
}

/// The optional structuredContent appendix: serialized, and only
/// when it is not byte-identical to a text block.
fn structured_appendix(
    object: &serde_json::Map<String, Value>,
    text_blocks: &[String],
) -> Option<String> {
    let structured = object.get("structuredContent")?;
    let serialized = serde_json::to_string(structured).ok()?;
    (!text_blocks.contains(&serialized)).then_some(serialized)
}

/// Converts one tools/call result: every content block rendered,
/// structuredContent appended only when it is not byte-identical to a
/// text block, and the whole capped.
pub fn convert_result(result: &Value) -> Result<Converted, String> {
    let Some(object) = result.as_object() else {
        return Err(String::from("the tools/call result is not a JSON object"));
    };
    let Some(blocks) = object.get("content").and_then(Value::as_array) else {
        return Err(String::from("the tools/call result named no content array"));
    };
    let (mut pieces, text_blocks) = render_blocks(blocks)?;
    if let Some(serialized) = structured_appendix(object, &text_blocks) {
        pieces.push(serialized);
    }
    let joined = pieces.join("\n");
    let capped = cap_bytes(&joined, RESULT_CAP);
    if object.get("isError").and_then(Value::as_bool) == Some(true) {
        Ok(Converted::IsError(capped))
    } else {
        Ok(Converted::Ok(capped))
    }
}

/// Every block rendered in order; the text blocks also collected for
/// the structuredContent identity check.
fn render_blocks(blocks: &[Value]) -> Result<(Vec<String>, Vec<String>), String> {
    let mut pieces = Vec::new();
    let mut text_blocks = Vec::new();
    for block in blocks {
        let rendered = render_block(block)?;
        if block.get("type").and_then(Value::as_str) == Some("text") {
            text_blocks.push(rendered.clone());
        }
        pieces.push(rendered);
    }
    Ok((pieces, text_blocks))
}

/// The composition depth of a schema: `$ref` plus nesting through
/// `anyOf`/`oneOf`/`allOf`/`$defs`/`if`/`then`/`else`. Plain property
/// nesting does not count. `None` means the walk hit its absolute
/// recursion bound — a hostile definition.
pub fn composition_depth(schema: &Value) -> Option<u32> {
    let mut max_seen = 0;
    walk(schema, 0, &mut max_seen)?;
    Some(max_seen)
}

fn walk(value: &Value, depth: u32, max_seen: &mut u32) -> Option<()> {
    if depth > WALK_BUDGET {
        return None;
    }
    if depth > *max_seen {
        *max_seen = depth;
    }
    let Some(object) = value.as_object() else {
        return Some(());
    };
    for (key, child) in object {
        match key.as_str() {
            "$ref" => {
                if depth + 1 > *max_seen {
                    *max_seen = depth + 1;
                }
            }
            "anyOf" | "oneOf" | "allOf" | "$defs" | "if" | "then" | "else" => {
                walk_composition(key, child, depth, max_seen)?;
            }
            _ => walk(child, depth, max_seen)?,
        }
    }
    Some(())
}

/// One composition keyword's children, each one level deeper.
fn walk_composition(key: &str, child: &Value, depth: u32, max_seen: &mut u32) -> Option<()> {
    let items: Vec<&Value> = match child {
        Value::Array(items) => items.iter().collect(),
        Value::Object(map) if key == "$defs" => map.values().collect(),
        other => vec![other],
    };
    for item in items {
        walk(item, depth + 1, max_seen)?;
    }
    Some(())
}

/// True when the schema is a JSON object — checked by shape, never by
/// trusting a `type` field.
pub fn schema_is_object(schema: &Value) -> bool {
    schema.is_object()
}

/// The serialized size of one schema, for the per-tool and per-server
/// caps.
pub fn schema_bytes(schema: &Value) -> Option<usize> {
    serde_json::to_string(schema).ok().map(|text| text.len())
}
