//! RED corpus: env resolution — fixture env maps, never a live `.env`.

use std::collections::HashMap;

use craft_app::AppError;
use craft_app::config::{EnvConfig, EnvMap, ProviderKind, mount_backend, resolve_env_config};
use craft_providers::anthropic::AuthScheme;

fn env(pairs: &[(&str, &str)]) -> EnvMap {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect::<HashMap<String, String>>()
}

fn resolve_pairs(pairs: &[(&str, &str)]) -> Result<EnvConfig, AppError> {
    resolve_env_config(&env(pairs))
}

fn assert_missing_var(err: &AppError, var: &str) {
    assert!(
        matches!(err, AppError::MissingEnv { var: named } if *named == var),
        "expected a missing-env error for {var}, got {err}"
    );
    assert!(err.to_string().contains(var));
}

fn assert_invalid_provider_error(err: &AppError, expected: &str) {
    assert!(matches!(err, AppError::InvalidProvider { value } if value == expected));
    let text = err.to_string();
    for known in ["anthropic", "openai", "gemini", "vertex"] {
        assert!(text.contains(known), "the error names {known}: {text}");
    }
}

fn assert_provider_identity(cfg: &EnvConfig) {
    assert_eq!(cfg.provider, ProviderKind::OpenAi);
    assert_eq!(cfg.model, "gpt-4o-mini");
    assert_eq!(cfg.api_key, "sk-test");
    assert_eq!(cfg.gh_token, None);
}

fn assert_openai_base_url(cfg: &EnvConfig) {
    assert_eq!(cfg.base_url, "https://api.openai.com/v1");
}

fn assert_anthropic_identity(cfg: &EnvConfig) {
    assert_eq!(cfg.provider, ProviderKind::Anthropic);
    assert_eq!(cfg.api_key, "ak-test");
    assert_eq!(cfg.anthropic_auth, AuthScheme::ApiKey);
}

fn assert_anthropic_base_url(cfg: &EnvConfig) {
    assert_eq!(cfg.base_url, "https://api.anthropic.com");
}

fn assert_gemini_identity(cfg: &EnvConfig) {
    assert_eq!(cfg.provider, ProviderKind::Gemini);
    assert_eq!(cfg.api_key, "gk-test");
    assert_eq!(cfg.base_url, "https://generativelanguage.googleapis.com");
}

fn assert_vertex_identity(cfg: &EnvConfig) {
    assert_eq!(cfg.provider, ProviderKind::Vertex);
    assert_eq!(cfg.model, "gemini-2.5-flash");
    assert_eq!(cfg.vertex_project, "fixture-project");
    assert_eq!(cfg.vertex_location, "us-central1");
}

fn assert_vertex_keyless_mount(cfg: &EnvConfig) {
    assert_eq!(cfg.api_key, "", "the vertex mount is keyless");
    assert_eq!(cfg.base_url, "", "the endpoint comes from the location");
}

fn resolve_provider(
    provider: &'static str,
    model: &'static str,
    key_var: &'static str,
    key: &'static str,
    extra: &[(&'static str, &'static str)],
) -> Result<EnvConfig, AppError> {
    let mut pairs = vec![
        ("CRAFT_AGENT_PROVIDER", provider),
        ("CRAFT_AGENT_MODEL", model),
        (key_var, key),
    ];
    pairs.extend_from_slice(extra);
    resolve_pairs(&pairs)
}

fn base_url_with_extra(
    provider: &'static str,
    key_var: &'static str,
    url_var: &'static str,
    url: &'static str,
) -> Result<String, AppError> {
    let cfg = resolve_provider(provider, "m", key_var, "k", &[(url_var, url)])?;
    Ok(cfg.base_url)
}

#[test]
fn missing_provider_is_a_named_error() {
    let err =
        resolve_pairs(&[("CRAFT_AGENT_MODEL", "gpt-4o")]).expect_err("provider must be named");
    assert_missing_var(&err, "CRAFT_AGENT_PROVIDER");
}

#[test]
fn invalid_provider_value_is_rejected() {
    let err = resolve_pairs(&[
        ("CRAFT_AGENT_PROVIDER", "claude"),
        ("CRAFT_AGENT_MODEL", "m"),
    ])
    .expect_err("unknown provider must fail");
    assert_invalid_provider_error(&err, "claude");
}

#[test]
fn missing_model_is_a_named_error() {
    let err = resolve_pairs(&[("CRAFT_AGENT_PROVIDER", "openai"), ("OPENAI_API_KEY", "k")])
        .expect_err("model must be required");
    assert_missing_var(&err, "CRAFT_AGENT_MODEL");
}

#[test]
fn openai_resolves_with_default_base_url() {
    let cfg = resolve_provider("openai", "gpt-4o-mini", "OPENAI_API_KEY", "sk-test", &[])
        .expect("openai resolves");
    assert_provider_identity(&cfg);
    assert_openai_base_url(&cfg);
}

#[test]
fn base_url_env_overrides_the_provider_default() {
    let cases = [
        (
            "openai",
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "http://localhost:8080/v1",
        ),
        (
            "anthropic",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "http://localhost:8080",
        ),
        (
            "gemini",
            "GOOGLE_API_KEY",
            "GEMINI_BASE_URL",
            "http://localhost:8080/v1beta",
        ),
    ];
    for (provider, key_var, url_var, url) in cases {
        let base_url =
            base_url_with_extra(provider, key_var, url_var, url).expect("custom base url resolves");
        assert_eq!(base_url, url, "provider {provider}");
    }
}

#[test]
fn anthropic_resolves_with_its_key() {
    let cfg = resolve_provider(
        "anthropic",
        "claude-sonnet-4",
        "ANTHROPIC_API_KEY",
        "ak-test",
        &[],
    )
    .expect("anthropic resolves");
    assert_anthropic_identity(&cfg);
    assert_anthropic_base_url(&cfg);
}

#[test]
fn anthropic_auth_bearer_mounts_the_bearer_scheme() {
    let cfg = resolve_provider(
        "anthropic",
        "m",
        "ANTHROPIC_API_KEY",
        "k",
        &[("ANTHROPIC_AUTH", "bearer")],
    )
    .expect("bearer is a legal knob value");
    assert_eq!(cfg.anthropic_auth, AuthScheme::Bearer);
}

#[test]
fn anthropic_auth_x_api_key_or_unset_defaults_to_the_header_scheme() {
    for value in ["x-api-key", ""] {
        let cfg = resolve_provider(
            "anthropic",
            "m",
            "ANTHROPIC_API_KEY",
            "k",
            &[("ANTHROPIC_AUTH", value)],
        )
        .expect("x-api-key and empty keep the default");
        assert_eq!(cfg.anthropic_auth, AuthScheme::ApiKey);
    }
    let cfg = resolve_provider("anthropic", "m", "ANTHROPIC_API_KEY", "k", &[])
        .expect("unset keeps the default");
    assert_eq!(cfg.anthropic_auth, AuthScheme::ApiKey);
}

#[test]
fn anthropic_auth_unknown_value_is_a_named_error() {
    let err = resolve_provider(
        "anthropic",
        "m",
        "ANTHROPIC_API_KEY",
        "k",
        &[("ANTHROPIC_AUTH", "token")],
    )
    .expect_err("an unknown scheme must fail");
    assert!(matches!(err, AppError::InvalidAuth { ref value } if value == "token"));
    assert!(err.to_string().contains("ANTHROPIC_AUTH"));
    assert!(err.to_string().contains("token"));
}

#[test]
fn gemini_resolves_with_its_key() {
    let cfg = resolve_provider("gemini", "gemini-2", "GOOGLE_API_KEY", "gk-test", &[])
        .expect("gemini resolves");
    assert_gemini_identity(&cfg);
}

fn vertex_pairs(
    project: Option<&'static str>,
    location: Option<&'static str>,
) -> Vec<(&'static str, &'static str)> {
    let mut pairs = vec![
        ("CRAFT_AGENT_PROVIDER", "vertex"),
        ("CRAFT_AGENT_MODEL", "gemini-2.5-flash"),
    ];
    if let Some(project) = project {
        pairs.push(("GOOGLE_VERTEX_PROJECT", project));
    }
    if let Some(location) = location {
        pairs.push(("GOOGLE_VERTEX_LOCATION", location));
    }
    pairs
}

#[test]
fn vertex_resolves_with_project_and_location() {
    let cfg = resolve_pairs(&vertex_pairs(Some("fixture-project"), Some("us-central1")))
        .expect("vertex resolves");
    assert_vertex_identity(&cfg);
    assert_vertex_keyless_mount(&cfg);
}

#[test]
fn missing_vertex_project_is_a_named_error() {
    let err = resolve_pairs(&vertex_pairs(None, Some("us-central1")))
        .expect_err("the project must be required");
    assert_missing_var(&err, "GOOGLE_VERTEX_PROJECT");
}

#[test]
fn missing_vertex_location_is_a_named_error() {
    let err = resolve_pairs(&vertex_pairs(Some("fixture-project"), None))
        .expect_err("the location must be required");
    assert_missing_var(&err, "GOOGLE_VERTEX_LOCATION");
}

#[test]
fn vertex_backend_mounts_without_an_api_key() {
    let cfg = resolve_pairs(&vertex_pairs(Some("fixture-project"), Some("global")))
        .expect("vertex resolves");
    assert!(
        mount_backend(&cfg).is_ok(),
        "the vertex mount resolves the credential lazily, not at launch"
    );
}

#[test]
fn named_provider_with_missing_key_names_the_variable() {
    let err = resolve_pairs(&[
        ("CRAFT_AGENT_PROVIDER", "anthropic"),
        ("CRAFT_AGENT_MODEL", "m"),
    ])
    .expect_err("anthropic key must be required");
    assert_missing_var(&err, "ANTHROPIC_API_KEY");
}

#[test]
fn no_ambient_fallback_across_providers() {
    // provider says gemini; only OPENAI_API_KEY is set — must still name
    // GOOGLE_API_KEY, never silently adopt the other key.
    let err = resolve_pairs(&[
        ("CRAFT_AGENT_PROVIDER", "gemini"),
        ("CRAFT_AGENT_MODEL", "m"),
        ("OPENAI_API_KEY", "sk-test"),
    ])
    .expect_err("no cross-provider fallback");
    assert_missing_var(&err, "GOOGLE_API_KEY");
}

#[test]
fn gh_token_passes_through_when_present() {
    let cfg = resolve_provider(
        "openai",
        "m",
        "OPENAI_API_KEY",
        "k",
        &[("GH_TOKEN", "ghp-test")],
    )
    .expect("openai resolves");
    assert_eq!(cfg.gh_token.as_deref(), Some("ghp-test"));
}

#[test]
fn backend_mounts_for_each_resolved_provider() {
    for provider in ["openai", "anthropic", "gemini"] {
        let key_var = match provider {
            "openai" => "OPENAI_API_KEY",
            "anthropic" => "ANTHROPIC_API_KEY",
            _ => "GOOGLE_API_KEY",
        };
        let cfg = resolve_provider(provider, "some-model", key_var, "fixture-key", &[])
            .expect("provider resolves");
        assert!(
            mount_backend(&cfg).is_ok(),
            "backend mounts for provider `{provider}`"
        );
    }
}
