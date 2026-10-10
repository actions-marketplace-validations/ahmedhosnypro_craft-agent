//! Qualified tool names: `mcp__<server>__<tool>`. Each part is
//! sanitized to `[a-zA-Z0-9_-]` with underscore runs collapsed, and the
//! assembled name is capped at 64 bytes by keeping the first 55 bytes
//! plus `_` plus 8 hex digits of FNV-1a over the full name — sanitized
//! parts are ASCII, so every byte index is a char boundary.

const NAME_CAP: usize = 64;
const KEEP_BYTES: usize = 55;
const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

pub const MCP_PREFIX: &str = "mcp__";

/// True for every tool name the MCP manager registers.
pub fn is_mcp_tool_name(name: &str) -> bool {
    name.starts_with(MCP_PREFIX)
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn sanitize_part(part: &str) -> String {
    let mut out = String::new();
    let mut pending_underscore = false;
    for ch in part.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            let is_underscore = ch == '_';
            if is_underscore && pending_underscore {
                continue;
            }
            pending_underscore = is_underscore;
            out.push(ch);
        } else if !pending_underscore {
            out.push('_');
            pending_underscore = true;
        }
    }
    out
}

/// The qualified name a discovered tool registers under.
pub fn qualified_name(server: &str, tool: &str) -> String {
    let full = format!(
        "{MCP_PREFIX}{}__{}",
        sanitize_part(server),
        sanitize_part(tool)
    );
    if full.len() <= NAME_CAP {
        return full;
    }
    let head = &full[..KEEP_BYTES];
    format!("{head}_{:08x}", fnv1a(full.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{is_mcp_tool_name, qualified_name};

    #[test]
    fn the_mcp_prefix_marks_tool_names() {
        assert!(is_mcp_tool_name("mcp__srv__echo"));
        assert!(!is_mcp_tool_name("read_file"));
        assert!(!is_mcp_tool_name("mcp_echo"));
    }

    #[test]
    fn parts_sanitize_and_collapse() {
        assert_eq!(
            qualified_name("repo-stats", "pr summary"),
            "mcp__repo-stats__pr_summary"
        );
        assert_eq!(qualified_name("s", "a__b"), "mcp__s__a_b");
        assert_eq!(
            qualified_name("s", "Ünïcode tool!"),
            "mcp__s___n_code_tool_"
        );
    }

    #[test]
    fn overlong_names_cap_with_a_stable_hash_suffix() {
        let long_tool = "t".repeat(80);
        let a = qualified_name("server-name", &long_tool);
        assert_eq!(a.len(), 64);
        assert!(a.starts_with("mcp__server-name__"));
        assert_eq!(a.as_bytes()[55], b'_');
        let b = qualified_name("server-name", &format!("{long_tool}x"));
        assert_ne!(a, b, "distinct full names keep distinct suffixes");
        let again = qualified_name("server-name", &long_tool);
        assert_eq!(a, again, "the suffix is deterministic");
    }

    #[test]
    fn builtin_collision_is_impossible_by_construction() {
        for builtin in [
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "glob",
            "grep",
            "gh",
        ] {
            assert!(is_mcp_tool_name(builtin) == builtin.starts_with("mcp__"));
        }
    }
}
