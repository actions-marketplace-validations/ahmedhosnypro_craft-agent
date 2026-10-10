//! Layer 1 registry: env scan, eligibility gates, caps, off-switch.

use std::collections::HashSet;
use std::env;

/// A candidate value larger than this is never registered.
pub const MAX_VALUE_BYTES: usize = 512;
/// The registry never holds more values than this.
pub const MAX_REGISTRY_VALUES: usize = 10_000;

const SECRETISH: &str = "(?i)(token|key|secret|password|passwd|pwd|credential|auth)";
const URL_CREDS: &str = r"^[a-z][a-z0-9+.-]*://[^/@\s]+:[^@\s]+@";
const EXTRA_VAR: &str = "CRAFT_REDACT_EXTRA";
const OFF_VAR: &str = "CRAFT_REDACT";

/// Minimum bytes for a secret-ish env value to be registered.
const MIN_SECRET_VALUE_BYTES: usize = 16;
/// Minimum bytes for a URL-embedded password to be registered.
const MIN_URL_PASSWORD_BYTES: usize = 8;

/// Registry values from the process environment plus CRAFT_REDACT_EXTRA.
pub fn collect_env() -> Vec<String> {
    let vars = env::vars_os().filter_map(|(name, value)| {
        match (name.into_string(), value.into_string()) {
            (Ok(name), Ok(value)) => Some((name, value)),
            _ => None,
        }
    });
    collect_from(vars, env::var(EXTRA_VAR).ok())
}

/// The registry from any (name, value) source; the seam for tests.
/// Secret-ish names register their value; URL values register their
/// embedded password; CRAFT_REDACT_EXTRA adds comma-separated values.
pub fn collect_from(
    vars: impl Iterator<Item = (String, String)>,
    extra: Option<String>,
) -> Vec<String> {
    let secretish = regex::Regex::new(SECRETISH).ok();
    let url_creds = regex::Regex::new(URL_CREDS).ok();
    let mut values = Vec::new();
    for (name, value) in vars {
        if let Some(re) = &secretish
            && re.is_match(&name)
            && eligible(&value, MIN_SECRET_VALUE_BYTES)
        {
            values.push(value.clone());
        }
        if let Some(re) = &url_creds
            && let Some(password) = url_password(&value, re)
        {
            values.push(password);
        }
    }
    for value in extra.iter().flat_map(|extra| extra.split(',')) {
        if eligible(value, MIN_SECRET_VALUE_BYTES) {
            values.push(value.to_string());
        }
    }
    cap(dedupe(values))
}

/// Whether the process-level off-switch disables the redactor.
pub fn switch_off() -> bool {
    switch_off_from(env::var(OFF_VAR).ok())
}

/// The off-switch decision for any CRAFT_REDACT value; the seam for
/// tests. Only off/0/false (case-insensitive) disables.
pub fn switch_off_from(value: Option<String>) -> bool {
    match value {
        Some(value) => {
            matches!(value.to_ascii_lowercase().as_str(), "off" | "0" | "false")
        }
        None => false,
    }
}

/// The password segment of a URL value with authority credentials:
/// between the last ':' before the '@' and the '@'.
fn url_password(value: &str, url_creds: &regex::Regex) -> Option<String> {
    if !url_creds.is_match(value) {
        return None;
    }
    let rest = value.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let at = authority.rfind('@')?;
    let password = authority[..at].rsplit(':').next()?;
    (password.len() >= MIN_URL_PASSWORD_BYTES).then(|| password.to_string())
}

/// Value shape gate: at least `min` bytes, not all ASCII digits, no
/// whitespace.
fn eligible(value: &str, min: usize) -> bool {
    value.len() >= min
        && !value.bytes().all(|b| b.is_ascii_digit())
        && !value.bytes().any(|b| b.is_ascii_whitespace())
}

/// Oversized values are dropped, then the registry is capped.
pub fn cap(values: Vec<String>) -> Vec<String> {
    let mut values: Vec<String> = values
        .into_iter()
        .filter(|value| value.len() <= MAX_VALUE_BYTES)
        .collect();
    values.truncate(MAX_REGISTRY_VALUES);
    values
}

/// Order-preserving dedupe.
pub fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_REGISTRY_VALUES, MAX_VALUE_BYTES, URL_CREDS, cap, collect_from, dedupe, eligible,
        switch_off_from, url_password,
    };

    fn url_password_of(value: &str) -> Option<String> {
        let re = regex::Regex::new(URL_CREDS).ok()?;
        url_password(value, &re)
    }

    #[test]
    fn a_secretish_name_registers_its_value() {
        let values = collect_from(
            [
                ("GH_TOKEN".to_string(), "ghp_abcdef0123456789ab".to_string()),
                ("HOME".to_string(), "/home/agent".to_string()),
                ("ALL_DIGITS".to_string(), "12345678901234567890".to_string()),
                ("SPACED_TOKEN".to_string(), "abc def0123456789".to_string()),
            ]
            .into_iter(),
            None,
        );
        assert!(values.contains(&"ghp_abcdef0123456789ab".to_string()));
        assert_eq!(values.len(), 1);
    }

    #[test]
    fn url_embedded_credentials_register_the_password() {
        let dsn = format!(
            "{}://{}:{}@{}/app",
            "postgres", "user", "secretpw12", "db.internal"
        );
        let values = collect_from(
            [("DATABASE_URL".to_string(), dsn.clone())].into_iter(),
            None,
        );
        assert_eq!(values, vec!["secretpw12".to_string()]);
        assert_eq!(url_password_of(&dsn).as_deref(), Some("secretpw12"));
        let no_password = format!("{}://{}@{}/app", "postgres", "user", "db.internal");
        assert_eq!(url_password_of(&no_password), None);
        let short = format!("{}://{}:{}@{}/app", "postgres", "user", "p", "db.internal");
        assert_eq!(url_password_of(&short), None);
    }

    #[test]
    fn the_extra_var_contributes_its_comma_separated_values() {
        let values = collect_from(
            std::iter::empty(),
            Some("aaaa111122223333,bbbb111122223333".to_string()),
        );
        assert_eq!(values.len(), 2);
        let short = collect_from(std::iter::empty(), Some("tooshort".to_string()));
        assert!(short.is_empty());
    }

    #[test]
    fn the_off_switch_accepts_only_off_zero_and_false() {
        assert!(switch_off_from(Some("off".to_string())));
        assert!(switch_off_from(Some("0".to_string())));
        assert!(switch_off_from(Some("FALSE".to_string())));
        assert!(!switch_off_from(Some("on".to_string())));
        assert!(!switch_off_from(Some(String::new())));
        assert!(!switch_off_from(None));
    }

    #[test]
    fn eligibility_requires_length_non_digits_and_no_whitespace() {
        assert!(eligible("abcdefgh12345678", 16));
        assert!(!eligible("abcdefgh1234567", 16));
        assert!(!eligible("1234567890123456", 16));
        assert!(!eligible("abcdefgh 12345678", 16));
        assert!(!eligible("", 16));
    }

    #[test]
    fn the_caps_drop_oversized_values_then_truncate_the_registry() {
        let oversized = "x".repeat(MAX_VALUE_BYTES + 1);
        let at_limit = "y".repeat(MAX_VALUE_BYTES);
        assert_eq!(cap(vec![at_limit.clone(), oversized]), vec![at_limit]);
        let many: Vec<String> = (0..MAX_REGISTRY_VALUES + 5)
            .map(|i| format!("value-{i:06}"))
            .collect();
        assert_eq!(cap(many).len(), MAX_REGISTRY_VALUES);
    }

    #[test]
    fn dedupe_keeps_the_first_occurrence_in_order() {
        let values = vec!["alpha".to_string(), "beta".to_string(), "alpha".to_string()];
        assert_eq!(
            dedupe(values),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }
}
