// Shared TEST_BYOK_* loader for the live corpora; each corpus binary mounts
// this file via #[path]. Names are printable; values never are. A corpus
// binary uses only its own trio, so the other loader rides dead.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use futures::StreamExt;

use craft_core::events::{FoldedTurn, TurnCaps, TurnFold};
use craft_core::provider::Provider;
use craft_core::types::ModelRequest;

pub struct TestKeys {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

// Streams one turn end to end, folding events as they arrive; every
// handshake, stream, or fold failure is a loud test failure.
pub async fn live_turn<P: Provider>(
    provider: &P,
    request: ModelRequest,
) -> Result<FoldedTurn, Box<dyn std::error::Error + Send + Sync>> {
    live_turn_observed(provider, request, |_| ()).await
}

// The same turn with an observer on each raw event; corpora that count
// wire fragments ride this instead of re-streaming.
pub async fn live_turn_observed<P: Provider>(
    provider: &P,
    request: ModelRequest,
    mut observe: impl FnMut(&craft_core::events::ModelEvent),
) -> Result<FoldedTurn, Box<dyn std::error::Error + Send + Sync>> {
    let mut stream = provider.stream(request).await?;
    let mut fold = TurnFold::new(TurnCaps::default());
    while let Some(item) = stream.next().await {
        let event = item?;
        observe(&event);
        fold.push(event)?;
    }
    Ok(fold.finish()?)
}

// Walk up at most 3 levels from the crate manifest to find a repo .env.
fn env_file_candidates(manifest: &Path) -> Vec<PathBuf> {
    let mut out = vec![manifest.join(".env")];
    let mut dir = Some(manifest);
    for _ in 0..3 {
        let Some(parent) = dir.and_then(Path::parent) else {
            break;
        };
        out.push(parent.join(".env"));
        dir = Some(parent);
    }
    out
}

// Minimal KEY=VALUE lines: trims, skips blanks and #-comments, strips quotes.
fn parse_env_file(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() {
            continue;
        }
        let unquoted = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')));
        let value = unquoted.unwrap_or(value);
        out.push((key.to_string(), value.to_string()));
    }
    out
}

fn file_value(var: &str) -> Option<String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for path in env_file_candidates(&manifest) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (key, value) in parse_env_file(&text) {
            if key == var {
                return Some(value);
            }
        }
    }
    None
}

// Process env wins; a set value (even empty, even non-UTF-8) is final.
fn read_var(var: &str) -> Option<String> {
    read_var_resolved(std::env::var(var), var)
}

fn read_var_resolved(found: Result<String, std::env::VarError>, var: &str) -> Option<String> {
    match found {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => file_value(var),
        Err(std::env::VarError::NotUnicode(_)) => {
            eprintln!("SKIP: {var} is set but not valid UTF-8; the .env value is not used");
            None
        }
    }
}

// The full trio, or None when any of the three is missing or empty.
pub fn test_keys() -> Option<TestKeys> {
    let base_url = read_var("TEST_BYOK_BASE_URL").filter(|v| !v.is_empty())?;
    let api_key = read_var("TEST_BYOK_API_KEY").filter(|v| !v.is_empty())?;
    let model = read_var("TEST_BYOK_MODEL").filter(|v| !v.is_empty())?;
    Some(TestKeys {
        base_url,
        api_key,
        model,
    })
}

// One knob: the value, or the knob's name when missing or empty.
fn knob(var: &'static str) -> Result<String, &'static str> {
    read_var(var).filter(|value| !value.is_empty()).ok_or(var)
}

// The Anthropic trio, or the name of the first missing or empty knob.
pub fn anthropic_keys() -> Result<TestKeys, &'static str> {
    let base_url = knob("TEST_BYOK_ANTHROPIC_BASE_URL")?;
    let api_key = knob("TEST_BYOK_ANTHROPIC_API_KEY")?;
    let model = knob("TEST_BYOK_ANTHROPIC_MODEL")?;
    Ok(TestKeys {
        base_url,
        api_key,
        model,
    })
}

// The Gemini trio: the key and model are required; the base URL is
// optional and defaults to the production Gemini host, so a set value
// can point the corpus at a loopback gateway.
pub fn gemini_keys() -> Result<TestKeys, &'static str> {
    let api_key = knob("TEST_BYOK_GEMINI_API_KEY")?;
    let model = knob("TEST_BYOK_GEMINI_MODEL")?;
    let base_url = gemini_base_url(read_var("TEST_BYOK_GEMINI_BASE_URL"));
    Ok(TestKeys {
        base_url,
        api_key,
        model,
    })
}

// The corpus base URL: a non-empty override, else the production host.
fn gemini_base_url(found: Option<String>) -> String {
    found
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| String::from(craft_providers::gemini::DEFAULT_BASE_URL))
}

// Yields TestKeys, or prints the SKIP line and early-returns Ok from the test.
#[macro_export]
macro_rules! require_keys {
    ($corpus:expr) => {
        match $crate::byok_env::test_keys() {
            Some(keys) => keys,
            None => {
                eprintln!(
                    "SKIP: TEST_BYOK_* are not set (no .env) — the live {} corpus is off",
                    $corpus
                );
                return Ok(());
            }
        }
    };
}

// Yields the Anthropic TestKeys, or names the missing knob (never its
// value) in the SKIP line and early-returns Ok from the test.
#[macro_export]
macro_rules! require_anthropic_keys {
    ($corpus:expr) => {
        match $crate::byok_env::anthropic_keys() {
            Ok(keys) => keys,
            Err(var) => {
                eprintln!(
                    "SKIP: {var} is not set (no .env) — the live {} corpus is off",
                    $corpus
                );
                return Ok(());
            }
        }
    };
}

// Yields the Gemini TestKeys, or names the missing knob (never its
// value) in the SKIP line and early-returns Ok from the test.
#[macro_export]
macro_rules! require_gemini_keys {
    ($corpus:expr) => {
        match $crate::byok_env::gemini_keys() {
            Ok(keys) => keys,
            Err(var) => {
                eprintln!(
                    "SKIP: {var} is not set (no .env) — the live {} corpus is off",
                    $corpus
                );
                return Ok(());
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::{gemini_base_url, read_var_resolved};

    fn non_unicode() -> Result<String, std::env::VarError> {
        Err(std::env::VarError::NotUnicode(std::ffi::OsString::new()))
    }

    // The resolved-lookup seam runs the NotUnicode arm without touching
    // the live process environment or the repo .env.
    #[test]
    fn a_non_utf8_process_value_never_falls_back_to_the_file() {
        let found = read_var_resolved(non_unicode(), "TEST_BYOK_BASE_URL");
        assert_eq!(found, None);
    }

    // The process env is final once set: a malformed value never rides the
    // repo .env that byok_env would otherwise load.
    #[test]
    fn a_set_process_value_wins_even_when_malformed() {
        let found = read_var_resolved(Ok(String::from("not a url")), "TEST_BYOK_BASE_URL");
        assert_eq!(found, Some(String::from("not a url")));
    }

    // The gemini base URL knob is optional: absent or empty falls back to
    // the production host, a set value rides verbatim.
    #[test]
    fn an_absent_or_empty_gemini_base_url_defaults_to_the_production_host() {
        assert_eq!(
            gemini_base_url(None),
            craft_providers::gemini::DEFAULT_BASE_URL
        );
        assert_eq!(
            gemini_base_url(Some(String::new())),
            craft_providers::gemini::DEFAULT_BASE_URL
        );
        assert_eq!(
            gemini_base_url(Some(String::from("http://127.0.0.1:9"))),
            "http://127.0.0.1:9"
        );
    }
}
