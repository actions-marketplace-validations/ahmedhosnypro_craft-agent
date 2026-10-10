//! The protocol-era seam: every wire-shape decision for the legacy
//! initialize handshake lives here, so a later era slots in without
//! touching discovery or registration.

use serde_json::{Value, json};

/// The protocol version this client speaks and requires.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub fn initialize_request(id: u64) -> Value {
    request(
        id,
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "craft-agent", "version": env!("CARGO_PKG_VERSION") },
        }),
    )
}

/// Sent after the initialize response; the server never answers.
pub fn initialized_notification() -> Value {
    json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
}

pub fn tools_list_request(id: u64, cursor: Option<&str>) -> Value {
    let params = match cursor {
        Some(cursor) => json!({ "cursor": cursor }),
        None => json!({}),
    };
    request(id, "tools/list", params)
}

pub fn tools_call_request(id: u64, name: &str, arguments: &Value) -> Value {
    request(
        id,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
}

/// A server answering another version is a version rejection: this
/// client speaks one era and disconnects.
pub fn accepts_server_version(answered: &str) -> bool {
    answered == PROTOCOL_VERSION
}
