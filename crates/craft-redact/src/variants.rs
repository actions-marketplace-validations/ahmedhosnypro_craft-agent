//! Encoded variants of a registered value: raw, percent, hex, and
//! base64 (standard/url-safe, padded/unpadded), over the value alone
//! and over the header-prefixed composites that leak into tool output.

/// Header prefixes whose composites get every encoding; the empty
/// prefix is the value alone.
const PREFIXES: [&str; 9] = [
    "",
    "x-access-token:",
    "Bearer ",
    "bearer ",
    "token ",
    "Authorization: Basic ",
    "Authorization: Bearer ",
    "api-key:",
    ":",
];

/// Every needle form of one registered value.
pub fn of(value: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(PREFIXES.len() * 8);
    for prefix in PREFIXES {
        let composite = format!("{prefix}{value}");
        out.push(composite.clone());
        out.push(percent_encode(&composite));
        out.push(hex_encode(&composite, false));
        out.push(hex_encode(&composite, true));
        out.push(base64_encode(&composite, false, true));
        out.push(base64_encode(&composite, false, false));
        out.push(base64_encode(&composite, true, true));
        out.push(base64_encode(&composite, true, false));
    }
    out
}

/// Percent-encode every byte outside the unreserved set; escapes are
/// uppercase hex, matching common URL encoders.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            push_hex_byte(&mut out, b, true);
        }
    }
    out
}

/// Whole-string hex, lower or upper.
fn hex_encode(s: &str, upper: bool) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        push_hex_byte(&mut out, b, upper);
    }
    out
}

fn push_hex_byte(out: &mut String, b: u8, upper: bool) {
    const LOWER: &[u8; 16] = b"0123456789abcdef";
    const UPPER: &[u8; 16] = b"0123456789ABCDEF";
    let table = if upper { UPPER } else { LOWER };
    out.push(table[(b >> 4) as usize] as char);
    out.push(table[(b & 0x0F) as usize] as char);
}

/// Base64 over the standard or URL-safe alphabet, with or without
/// padding.
fn base64_encode(s: &str, url_safe: bool, padded: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        push_triplet(&mut out, chunk, base64_table(url_safe));
    }
    push_tail(&mut out, bytes.len(), padded);
    out
}

fn base64_table(url_safe: bool) -> &'static [u8; 64] {
    const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    if url_safe { URL } else { STD }
}

fn push_triplet(out: &mut String, chunk: &[u8], table: &[u8; 64]) {
    let b0 = chunk[0] as u32;
    let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
    let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
    let n = (b0 << 16) | (b1 << 8) | b2;
    out.push(table[((n >> 18) & 0x3F) as usize] as char);
    out.push(table[((n >> 12) & 0x3F) as usize] as char);
    if chunk.len() > 1 {
        out.push(table[((n >> 6) & 0x3F) as usize] as char);
    }
    if chunk.len() > 2 {
        out.push(table[(n & 0x3F) as usize] as char);
    }
}

/// The `=` fill for a 1- or 2-byte tail, only when padding is on.
fn push_tail(out: &mut String, len: usize, padded: bool) {
    if !padded {
        return;
    }
    match len % 3 {
        1 => out.push_str("=="),
        2 => out.push('='),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{base64_encode, hex_encode, of, percent_encode};

    #[test]
    fn base64_matches_reference_vectors() {
        assert_eq!(
            base64_encode("hello world", false, true),
            "aGVsbG8gd29ybGQ="
        );
        assert_eq!(
            base64_encode("hello world", false, false),
            "aGVsbG8gd29ybGQ"
        );
        assert_eq!(base64_encode("hello world", true, true), "aGVsbG8gd29ybGQ=");
        assert_eq!(base64_encode("ab", false, true), "YWI=");
        assert_eq!(base64_encode("abc", false, false), "YWJj");
        assert_eq!(base64_encode("~?", false, true), "fj8=");
        // '+' vs '-' divergence shows only when the bytes produce one
        assert_eq!(base64_encode("?>", true, true), "Pz4=");
        assert_eq!(base64_encode("?>", false, true), "Pz4=");
    }

    #[test]
    fn hex_and_percent_match_reference_vectors() {
        assert_eq!(hex_encode("AB", false), "4142");
        assert_eq!(hex_encode("AB", true), "4142");
        assert_eq!(hex_encode("\u{00ff}", true), "C3BF");
        assert_eq!(percent_encode("a b:c"), "a%20b%3Ac");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(percent_encode("Bearer abc"), "Bearer%20abc");
    }

    #[test]
    fn every_variant_is_nonempty_and_the_raw_set_is_present() {
        let value = "s3cr3t-value-0123456789";
        let variants = of(value);
        assert_eq!(variants.len(), 72);
        assert!(variants.iter().all(|v| !v.is_empty()));
        assert!(variants.contains(&value.to_string()));
        assert!(variants.contains(&format!("x-access-token:{value}")));
        assert!(variants.contains(&format!("Bearer {value}")));
        assert!(variants.contains(&percent_encode(&format!("token {value}"))));
        assert!(variants.contains(&hex_encode(value, true)));
        assert!(variants.contains(&base64_encode(&format!(":{value}"), false, true)));
        assert!(variants.contains(&base64_encode(value, true, false)));
    }
}
