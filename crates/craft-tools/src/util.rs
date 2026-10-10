//! Small shared helpers: bounded extraction of typed arguments from
//! untrusted JSON, char-boundary truncation, lossy bounded reads, and
//! content hashing for the changed-on-disk guard.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::Value;

use crate::error::ToolError;

/// Bounds for an array-of-strings argument: the byte cap per item and
/// the maximum item count.
pub struct ListBounds {
    pub item_cap: usize,
    pub max_items: usize,
}

/// The untrusted arguments object of one tool call, paired with the
/// tool label its typed errors carry.
pub struct ToolArgs<'a> {
    args: &'a Value,
    tool: &'a str,
}

impl<'a> ToolArgs<'a> {
    pub fn new(args: &'a Value, tool: &'a str) -> Self {
        Self { args, tool }
    }

    fn invalid(&self, msg: String) -> ToolError {
        ToolError::InvalidArgs {
            tool: self.tool.to_string(),
            msg,
        }
    }

    /// Reject keys outside the tool's documented set.
    pub fn reject_unknown_keys(&self, allowed: &[&str]) -> Result<(), ToolError> {
        let Value::Object(map) = self.args else {
            return Err(self.invalid("arguments must be an object".to_string()));
        };
        for key in map.keys() {
            if !allowed.iter().any(|k| k == key) {
                return Err(self.invalid(format!("unknown argument `{key}`")));
            }
        }
        Ok(())
    }

    /// A required string argument; empty strings are rejected by callers that
    /// need non-empty values (the default here allows them).
    pub fn arg_str(&self, key: &str) -> Result<String, ToolError> {
        let Value::Object(map) = self.args else {
            return Err(self.invalid("arguments must be an object".to_string()));
        };
        let Some(value) = map.get(key) else {
            return Err(self.invalid(format!("missing required argument `{key}`")));
        };
        Ok(self.str_value(value, key)?.to_string())
    }

    /// A required string argument with a byte-size cap.
    pub fn arg_str_bounded(&self, key: &str, cap: usize) -> Result<String, ToolError> {
        let s = self.arg_str(key)?;
        self.within_cap(&s, key, cap)?;
        Ok(s)
    }

    /// The string at `value`, or the typed must-be-a-string error.
    fn str_value<'v>(&self, value: &'v Value, key: &str) -> Result<&'v str, ToolError> {
        let Some(s) = value.as_str() else {
            return Err(self.invalid(format!("argument `{key}` must be a string")));
        };
        Ok(s)
    }

    /// Reject a string above its byte cap.
    fn within_cap(&self, s: &str, key: &str, cap: usize) -> Result<(), ToolError> {
        if s.len() > cap {
            return Err(self.invalid(format!("argument `{key}` exceeds {cap} bytes")));
        }
        Ok(())
    }

    /// The raw value of an optional key; null counts as absent.
    fn opt_value(&self, key: &str) -> Result<Option<&'a Value>, ToolError> {
        let Value::Object(map) = self.args else {
            return Err(self.invalid("arguments must be an object".to_string()));
        };
        match map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => Ok(Some(value)),
        }
    }

    /// An optional string argument with a byte-size cap; null counts as absent.
    pub fn arg_opt_str(&self, key: &str, cap: usize) -> Result<Option<String>, ToolError> {
        let Some(value) = self.opt_value(key)? else {
            return Ok(None);
        };
        let s = self.str_value(value, key)?;
        self.within_cap(s, key, cap)?;
        Ok(Some(s.to_string()))
    }

    /// An optional non-negative integer argument; floats and negatives are
    /// rejected, and values above `max` are rejected as hostile.
    pub fn arg_opt_u64(&self, key: &str, max: u64) -> Result<Option<u64>, ToolError> {
        let Some(value) = self.opt_value(key)? else {
            return Ok(None);
        };
        let Some(n) = value.as_u64() else {
            return Err(self.invalid(format!("argument `{key}` must be a non-negative integer")));
        };
        if n > max {
            return Err(self.invalid(format!("argument `{key}` exceeds {max}")));
        }
        Ok(Some(n))
    }

    /// An optional boolean argument.
    pub fn arg_opt_bool(&self, key: &str) -> Result<Option<bool>, ToolError> {
        match self.opt_value(key)? {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(self.invalid(format!("argument `{key}` must be a boolean"))),
        }
    }

    /// A required array-of-strings argument with per-item and count caps.
    pub fn arg_str_list(&self, key: &str, bounds: ListBounds) -> Result<Vec<String>, ToolError> {
        let ListBounds {
            item_cap,
            max_items,
        } = bounds;
        let Value::Object(map) = self.args else {
            return Err(self.invalid("arguments must be an object".to_string()));
        };
        let Some(Value::Array(items)) = map.get(key) else {
            return Err(self.invalid(format!("argument `{key}` must be an array of strings")));
        };
        if items.is_empty() {
            return Err(self.invalid(format!("argument `{key}` must not be empty")));
        }
        if items.len() > max_items {
            return Err(self.invalid(format!("argument `{key}` exceeds {max_items} items")));
        }
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let Some(s) = item.as_str() else {
                return Err(self.invalid(format!("argument `{key}` must contain only strings")));
            };
            if s.len() > item_cap {
                return Err(self.invalid(format!("`{key}` items must stay under {item_cap} bytes")));
            }
            out.push(s.to_string());
        }
        Ok(out)
    }
}

/// Truncate a string to a byte cap without splitting a character.
pub fn truncate_char_boundary(s: &str, cap: usize) -> &str {
    if s.len() <= cap {
        return s;
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Lossy UTF-8 conversion of at most `cap` bytes; true when truncated.
/// The cut backs up over continuation bytes so no character is split.
pub fn bounded_lossy(bytes: &[u8], cap: usize) -> (String, bool) {
    if bytes.len() <= cap {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let mut cut = cap.min(bytes.len());
    while cut > 0 && (bytes[cut] & 0xC0) == 0x80 {
        cut -= 1;
    }
    (String::from_utf8_lossy(&bytes[..cut]).into_owned(), true)
}

/// A cheap content stamp for the changed-on-disk guard; not cryptographic.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}
