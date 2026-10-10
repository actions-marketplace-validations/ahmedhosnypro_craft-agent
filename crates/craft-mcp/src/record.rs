//! The discovery record's identity: a sha256 over key-sorted canonical
//! JSON of the record's own fields, so an identical rediscovery is
//! detectable and emits nothing.

use std::collections::BTreeSet;

use serde_json::Value;
use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The sha256 of the key-sorted canonical form of the record fields.
/// serde_json objects are BTreeMaps, so serialization order is key
/// order and the enabled names arrive through a BTreeSet pre-sorted.
pub fn discovery_hash(
    tools: &Value,
    enabled_names: &BTreeSet<String>,
    collisions: &Value,
) -> String {
    let canonical = serde_json::json!({
        "tools": tools,
        "enabled_names": enabled_names,
        "collisions": collisions,
    });
    let Ok(text) = serde_json::to_string(&canonical) else {
        return String::new();
    };
    hex(&Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::discovery_hash;
    use std::collections::BTreeSet;

    #[test]
    fn the_hash_is_key_sorted_and_stable() {
        let tools = serde_json::json!([
            { "name": "echo", "inputSchema": { "type": "object" } },
            { "name": "report", "inputSchema": { "type": "object" } },
        ]);
        let names: BTreeSet<String> = ["mcp__srv__report", "mcp__srv__echo"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let a = discovery_hash(&tools, &names, &serde_json::json!([]));
        let b = discovery_hash(&tools, &names, &serde_json::json!([]));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn any_field_change_changes_the_hash() {
        let tools = serde_json::json!([{ "name": "echo" }]);
        let empty: BTreeSet<String> = BTreeSet::new();
        let base = discovery_hash(&tools, &empty, &serde_json::json!([]));
        assert_ne!(
            base,
            discovery_hash(&serde_json::json!([]), &empty, &serde_json::json!([]))
        );
        let mut one = empty.clone();
        one.insert(String::from("mcp__srv__echo"));
        assert_ne!(base, discovery_hash(&tools, &one, &serde_json::json!([])));
        assert_ne!(
            base,
            discovery_hash(&tools, &empty, &serde_json::json!([{ "tool": "x" }]))
        );
    }

    #[test]
    fn the_hash_of_empty_input_is_defined() {
        let empty: BTreeSet<String> = BTreeSet::new();
        let hash = discovery_hash(&serde_json::json!([]), &empty, &serde_json::json!([]));
        assert_eq!(hash.len(), 64);
        assert_ne!(hash, "");
    }
}
