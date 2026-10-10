// The incident shape: tool stdout carried base64("x-access-token:<token>").
// Layer 1 must kill that encoded composite, not just the raw value.

use std::sync::Arc;
use std::thread;

use craft_redact::Redactor;

// base64, standard alphabet with padding — what shell `base64` emits.
fn b64(input: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (chunk.get(1).copied().unwrap_or(0) as u32) << 8
            | (chunk.get(2).copied().unwrap_or(0) as u32);
        out.push(TABLE[(n >> 18) as usize & 0x3F] as char);
        out.push(TABLE[(n >> 12) as usize & 0x3F] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(n >> 6) as usize & 0x3F] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[n as usize & 0x3F] as char);
        }
    }
    match bytes.len() % 3 {
        1 => out.push_str("=="),
        2 => out.push('='),
        _ => {}
    }
    out
}

fn incident_token() -> String {
    format!("ghp_{}", "x9QkW2mZv7TnR4sYp6LdJ8hCe5gFaUuBi3Mt")
}

#[test]
fn the_incident_fixture_is_redacted_from_stdout() {
    let token = incident_token();
    let encoded = b64(&format!("x-access-token:{token}"));
    let line = format!("+ git fetch origin\n+ {encoded}\nremote: ok\n");
    let redactor = Redactor::from_parts(vec![token.clone()], false);
    let out = redactor.redact(&line);
    assert!(out.contains("[REDACTED:known-secret]"), "{out}");
    assert!(!out.contains(&token), "{out}");
    assert!(!out.contains(&token[4..]), "{out}");
    assert!(!out.contains(&encoded), "{out}");
    for window in token.as_bytes()[4..].windows(8) {
        if let Ok(window) = std::str::from_utf8(window) {
            assert!(!out.contains(window), "{window} leaked: {out}");
        }
    }
    assert_eq!(redactor.raw_secret_hits(), 0);
    assert!(redactor.redactions() >= 1);
}

#[test]
fn a_raw_registered_value_is_redacted_and_counted_raw() {
    let token = incident_token();
    let redactor = Redactor::from_parts(vec![token.clone()], false);
    let line = format!("token was {token} in the log");
    let out = redactor.redact(&line);
    assert_eq!(out, "token was [REDACTED:known-secret] in the log");
    assert!(redactor.raw_secret_hits() >= 1);
    assert_eq!(redactor.redactions(), 1);
}

#[test]
fn bearer_and_basic_header_composites_are_redacted() {
    let value = "tok1234567890123".to_string();
    let redactor = Redactor::from_parts(vec![value.clone()], false);
    let header = format!("Authorization: Bearer {value}");
    let out = redactor.redact(&header);
    assert!(out.contains("[REDACTED:known-secret]"), "{out}");
    let basic = b64(&format!("Authorization: Basic {value}"));
    let line = format!("header {basic} end");
    let out = redactor.redact(&line);
    assert!(out.contains("[REDACTED:known-secret]"), "{out}");
}

#[test]
fn encoded_variants_of_a_value_are_redacted() {
    let redactor = Redactor::from_parts(vec!["abcdefgh12345678".to_string()], false);
    let cases: Vec<(String, &str)> = vec![
        ("61626364656667683132333435363738".to_string(), "hex lower"),
        (
            "61626364656667683132333435363738".to_ascii_uppercase(),
            "hex upper",
        ),
        ("YWJjZGVmZ2gxMjM0NTY3OA==".to_string(), "base64 padded"),
        ("YWJjZGVmZ2gxMjM0NTY3OA".to_string(), "base64 unpadded"),
    ];
    for (needle, label) in cases {
        let line = format!("found {needle} here");
        let out = redactor.redact(&line);
        assert!(out.contains("[REDACTED:known-secret]"), "{label}: {out}");
    }
    let spaced = Redactor::from_parts(vec!["secret pw 12345".to_string()], false);
    let out = spaced.redact("found secret%20pw%2012345 here");
    assert!(out.contains("[REDACTED:known-secret]"), "{out}");
}

#[test]
fn pattern_layer_redacts_token_shaped_secrets() {
    let redactor = Redactor::from_parts(Vec::new(), true);
    let cases: Vec<(String, &str)> = vec![
        (
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N0XFUP0THsR8U"
                .to_string(),
            "[REDACTED:jwt]",
        ),
        (
            format!("-----BEGIN {}PRIVATE KEY-----", "RSA "),
            "[REDACTED:pem-private-key]",
        ),
        (
            format!("AKIA{}", "IOSFODNN7EXAMPLE"),
            "[REDACTED:aws-access-token]",
        ),
        (
            "Authorization: Basic dXNlcjpwYXNzd29yZA==".to_string(),
            "[REDACTED:authorization-basic]",
        ),
        (
            "Bearer abcdefghij1234567890".to_string(),
            "[REDACTED:authorization-bearer]",
        ),
    ];
    for (text, marker) in cases {
        let out = redactor.redact(&text);
        assert!(out.contains(marker), "{text} -> {out}");
    }
    let fine = format!("github_pat_{}", "F".repeat(82));
    let out = redactor.redact(&fine);
    assert!(out.contains("[REDACTED:github-fine-grained-pat]"), "{out}");
    assert!(
        redactor.raw_secret_hits() >= 2,
        "aws and fine-grained are raw"
    );
}

#[test]
fn a_registered_dsn_password_is_redacted() {
    let dsn = format!("{}://{}:{}@{}/db", "postgres", "user", "secretpw12", "host");
    let redactor = Redactor::from_parts(vec!["secretpw12".to_string()], false);
    let line = format!("connect {dsn} ok");
    let out = redactor.redact(&line);
    assert!(!out.contains("secretpw12"), "{out}");
    assert!(out.contains("[REDACTED:known-secret]"), "{out}");
    let with_patterns = Redactor::from_parts(vec!["secretpw12".to_string()], true);
    let out = with_patterns.redact(&line);
    assert!(!out.contains("secretpw12"), "{out}");
}

#[test]
fn ordinary_text_is_never_redacted() {
    let redactor = Redactor::from_parts(Vec::new(), true);
    let cases = [
        "tokenize the corpus",
        "a monkey business",
        "background: #deadbeef;",
        "let key = value.into_key();",
        "uuid 550e8400-e29b-41d4-a716-446655440000 stays",
        "short creds abcdefghijkl not flagged",
    ];
    for text in cases {
        assert_eq!(redactor.redact(text), text, "{text}");
    }
    assert_eq!(redactor.redactions(), 0);
}

#[test]
fn explicit_construction_is_always_enabled() {
    let redactor = Redactor::from_parts(Vec::new(), false);
    assert!(redactor.is_enabled());
    assert!(redactor.mask_values().is_empty());
}

#[test]
fn concurrent_redaction_sums_counters() {
    let value = incident_token();
    let redactor = Arc::new(Redactor::from_parts(vec![value.clone()], false));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let redactor = Arc::clone(&redactor);
            let value = value.clone();
            thread::spawn(move || {
                for _ in 0..250 {
                    let line = format!("line {value} end");
                    let out = redactor.redact(&line);
                    assert_eq!(out, "line [REDACTED:known-secret] end");
                }
            })
        })
        .collect();
    for thread in threads {
        assert!(thread.join().is_ok(), "worker failed");
    }
    assert_eq!(redactor.redactions(), 1000);
    assert_eq!(redactor.raw_secret_hits(), 1000);
}

#[test]
fn oversized_values_are_never_registered() {
    let too_big = "x".repeat(513);
    let redactor = Redactor::from_parts(vec![too_big.clone()], false);
    assert!(redactor.mask_values().is_empty());
    assert_eq!(redactor.redact(&too_big), too_big.as_str());
}

#[test]
fn the_registry_caps_at_ten_thousand_values() {
    let values: Vec<String> = (0..10_005)
        .map(|i| format!("secret-value-{i:08}-x"))
        .collect();
    let redactor = Redactor::from_parts(values, false);
    assert_eq!(
        redactor.mask_values().len(),
        craft_redact::MAX_REGISTRY_VALUES
    );
    assert!(
        redactor
            .mask_values()
            .contains(&"secret-value-00000000-x".to_string())
    );
    assert_eq!(craft_redact::MAX_VALUE_BYTES, 512);
}
