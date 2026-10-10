//! Conversion corpus: every content-block kind renders, dropped media
//! carry a visible notice, structuredContent mirrors only when it
//! differs, and the whole rides the 256 KiB cap.
// Corpus helpers: the loud-test idiom for panics.
#![allow(clippy::panic)]

use craft_mcp::convert::{Converted, composition_depth, convert_result};
use serde_json::{Value, json};

fn result_of(body: Value) -> Result<Converted, String> {
    convert_result(&body)
}

#[test]
fn text_blocks_join_and_report() {
    let converted = result_of(json!({
        "content": [
            { "type": "text", "text": "first" },
            { "type": "text", "text": "second" },
        ]
    }))
    .expect("text blocks convert");
    assert_eq!(converted, Converted::Ok(String::from("first\nsecond")));
}

#[test]
fn image_and_audio_blocks_drop_with_visible_notices() {
    let converted = result_of(json!({
        "content": [
            { "type": "image", "data": "zzz", "mimeType": "image/png" },
            { "type": "audio", "data": "zzz" },
            { "type": "text", "text": "kept" },
        ]
    }))
    .expect("mixed blocks convert");
    let Converted::Ok(text) = converted else {
        panic!("no error flag means Ok");
    };
    assert!(
        text.contains("[mcp content dropped: image (unsupported in v1)]"),
        "{text}"
    );
    assert!(
        text.contains("[mcp content dropped: audio (unsupported in v1)]"),
        "{text}"
    );
    assert!(text.contains("kept"), "{text}");
}

#[test]
fn resource_links_render_one_text_line() {
    let converted = result_of(json!({
        "content": [
            { "type": "resource_link", "uri": "file:///report", "name": "report", "description": "the weekly report" },
        ]
    }))
    .expect("resource links convert");
    let Converted::Ok(text) = converted else {
        panic!("no error flag means Ok");
    };
    assert_eq!(text, "file:///report report the weekly report");
}

#[test]
fn embedded_resources_render_auditable_json() {
    let converted = result_of(json!({
        "content": [ { "type": "resource", "resource": { "uri": "file:///x" } } ],
    }))
    .expect("embedded resources convert");
    let Converted::Ok(text) = converted else {
        panic!("no error flag means Ok");
    };
    assert!(text.starts_with("[mcp resource] {"), "{text}");
    assert!(text.contains("file:///x"), "{text}");
}

#[test]
fn unknown_block_kinds_drop_with_a_notice() {
    let converted = result_of(json!({
        "content": [ { "type": "hologram" } ],
    }))
    .expect("unknown kinds convert to notices");
    let Converted::Ok(text) = converted else {
        panic!("no error flag means Ok");
    };
    assert_eq!(text, "[mcp content dropped: hologram (unsupported in v1)]");
}

#[test]
fn structured_content_mirrors_only_when_it_differs() {
    let same = result_of(json!({
        "content": [ { "type": "text", "text": "{\"ok\":true}" } ],
        "structuredContent": { "ok": true },
    }))
    .expect("converts");
    let Converted::Ok(text) = same else {
        panic!("Ok")
    };
    assert_eq!(
        text, "{\"ok\":true}",
        "the mirror is skipped when byte-identical"
    );

    let differs = result_of(json!({
        "content": [ { "type": "text", "text": "plain" } ],
        "structuredContent": { "rows": 2 },
    }))
    .expect("converts");
    let Converted::Ok(text) = differs else {
        panic!("Ok")
    };
    assert_eq!(text, "plain\n{\"rows\":2}");
}

#[test]
fn oversized_output_caps_at_256kib_with_the_marker() {
    let converted = result_of(json!({
        "content": [ { "type": "text", "text": "a".repeat(5 * 1024 * 1024) } ],
    }))
    .expect("oversized output converts");
    let Converted::Ok(text) = converted else {
        panic!("Ok")
    };
    assert!(
        text.len() < 5 * 1024 * 1024,
        "the 5 MiB payload is not passed through"
    );
    assert!(
        text.len() <= 256 * 1024 + 64,
        "the cap holds: {}",
        text.len()
    );
    assert!(
        text.ends_with("[output truncated]"),
        "the cap leaves a marker"
    );
}

#[test]
fn server_error_flags_carry_the_content() {
    let converted = result_of(json!({
        "content": [ { "type": "text", "text": "rate limited, retry later" } ],
        "isError": true,
    }))
    .expect("isError results convert");
    assert_eq!(
        converted,
        Converted::IsError(String::from("rate limited, retry later"))
    );
}

#[test]
fn hostile_result_shapes_are_typed_errors() {
    let err = result_of(json!("a string")).expect_err("a non-object result fails");
    assert!(err.contains("not a JSON object"), "{err}");
    let err =
        result_of(json!({ "content": "not an array" })).expect_err("content must be an array");
    assert!(err.contains("no content array"), "{err}");
    let err = result_of(json!({})).expect_err("missing content fails");
    assert!(err.contains("no content array"), "{err}");
}

#[test]
fn composition_depth_counts_only_composition_keywords() {
    let flat = composition_depth(&json!({ "type": "object", "properties": {} }));
    assert_eq!(flat, Some(0));
    let ref_only = composition_depth(&json!({ "$ref": "#/$defs/x" }));
    assert_eq!(ref_only, Some(1));
    let nested = composition_depth(&json!({
        "allOf": [ { "anyOf": [ { "$ref": "#/x" } ] } ]
    }));
    assert_eq!(nested, Some(3));
    let mut hostile: Value = json!({});
    for _ in 0..100 {
        hostile = json!({ "allOf": [hostile] });
    }
    assert_eq!(composition_depth(&hostile), None, "the walk budget holds");
}
