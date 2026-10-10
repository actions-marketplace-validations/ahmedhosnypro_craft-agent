//! Knob corpus: the trust grant defaults off, every hostile value is a
//! typed error naming the knob, and the helpers never silently fall
//! back.
// Corpus helpers: the loud-test idiom for panics.
#![allow(clippy::panic)]

use craft_mcp::{
    DEFAULT_MAX_TOOLS, DEFAULT_STARTUP_TIMEOUT_SECS, DEFAULT_TOOL_TIMEOUT_SECS, McpError,
    knob_enabled, max_tools_from, timeout_secs_from,
};

#[test]
fn the_trust_grant_defaults_off() {
    assert_eq!(knob_enabled(None), Ok(false));
    assert_eq!(knob_enabled(Some("")), Ok(false));
    assert_eq!(knob_enabled(Some("  ")), Ok(false));
    assert_eq!(knob_enabled(Some("0")), Ok(false));
    assert_eq!(knob_enabled(Some("1")), Ok(true));
}

#[test]
fn a_typoed_grant_never_silently_decides() {
    for raw in ["true", "yes", "on", "2", "0x1", "enabled"] {
        match knob_enabled(Some(raw)) {
            Err(McpError::Knob { value }) => {
                assert!(value.contains(raw.trim()), "{value}");
            }
            other => panic!("`{raw}` must be a typed Knob error, got {other:?}"),
        }
    }
    assert_eq!(
        knob_enabled(Some(" 1 ")),
        Ok(true),
        "surrounding whitespace trims"
    );
    let err = knob_enabled(Some("garbage")).expect_err("names the knob");
    assert!(err.to_string().contains("CRAFT_AGENT_MCP"), "{err}");
}

#[test]
fn the_total_cap_knob_parses_like_the_window_knobs() {
    assert_eq!(max_tools_from(None), Ok(DEFAULT_MAX_TOOLS));
    assert_eq!(max_tools_from(Some("")), Ok(128));
    assert_eq!(max_tools_from(Some(" 64 ")), Ok(64));
    assert_eq!(max_tools_from(Some("0")), Ok(0), "zero accepts nothing");
    for raw in ["-1", "1.5", "many", "99999999999999999999999"] {
        let err = max_tools_from(Some(raw)).expect_err("hostile values are typed errors");
        assert!(
            err.to_string().contains("CRAFT_AGENT_MCP_MAX_TOOLS"),
            "{err}"
        );
    }
}

#[test]
fn timeout_knobs_keep_their_defaults_and_bounds() {
    assert_eq!(
        timeout_secs_from(None, "STARTUP", DEFAULT_STARTUP_TIMEOUT_SECS),
        Ok(30)
    );
    assert_eq!(
        timeout_secs_from(Some(""), "TOOL", DEFAULT_TOOL_TIMEOUT_SECS),
        Ok(120)
    );
    assert_eq!(timeout_secs_from(Some(" 1 "), "STARTUP", 30), Ok(1));
    assert_eq!(timeout_secs_from(Some("86400"), "TOOL", 120), Ok(86_400));
    for raw in ["0", "86401", "soon", "-5"] {
        let err = timeout_secs_from(Some(raw), "STARTUP", 30).expect_err("out-of-range is typed");
        let text = err.to_string();
        assert!(
            text.contains("CRAFT_AGENT_MCP_STARTUP_TIMEOUT_SECS"),
            "{text}"
        );
        assert!(text.contains("1..=86400"), "{text}");
    }
}
