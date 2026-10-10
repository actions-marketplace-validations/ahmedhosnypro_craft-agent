//! Layer 2: token-shape rules. Regexes marked gitleaks are ported
//! from the gitleaks default config (MIT; see NOTICE); rules marked
//! local are authored here. Deviations from the gitleaks source are
//! noted at the rule.

use regex::Regex;

/// A compiled rule: the redaction token, and whether a hit is a raw
/// token shape (bumps `raw_secret_hits`).
pub struct Pattern {
    pub regex: Regex,
    pub marker: &'static str,
    pub raw: bool,
}

type Rule = (&'static str, &'static str, bool);

fn pattern((source, marker, raw): &Rule) -> Option<Pattern> {
    let regex = Regex::new(source).ok()?;
    Some(Pattern {
        regex,
        marker,
        raw: *raw,
    })
}

/// Every rule that compiles; a rule that fails to compile is dropped
/// rather than failing the run, and the rule tests keep the set
/// honest.
pub fn rules() -> Vec<Pattern> {
    RULES.iter().filter_map(pattern).collect()
}

const RULES: &[Rule] = &[
    // gitleaks github-pat, verbatim
    (r"ghp_[0-9a-zA-Z]{36}", "[REDACTED:github-pat]", true),
    // gitleaks github-oauth, verbatim
    (r"gho_[0-9a-zA-Z]{36}", "[REDACTED:github-oauth]", true),
    // gitleaks github-app-token, verbatim
    (
        r"(?:ghu|ghs)_[0-9a-zA-Z]{36}",
        "[REDACTED:github-app-token]",
        true,
    ),
    // gitleaks github-refresh-token, verbatim
    (
        r"ghr_[0-9a-zA-Z]{36}",
        "[REDACTED:github-refresh-token]",
        true,
    ),
    // gitleaks github-fine-grained-pat, verbatim
    (
        r"github_pat_\w{82}",
        "[REDACTED:github-fine-grained-pat]",
        true,
    ),
    // gitleaks aws-access-token, verbatim
    (
        r"\b(?:A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Z2-7]{16}\b",
        "[REDACTED:aws-access-token]",
        true,
    ),
    // gitleaks gcp-api-key; the trailing-terminator guard is dropped
    // because the redactor replaces the whole match and the
    // terminator must survive
    (r"\bAIza[\w-]{35}", "[REDACTED:gcp-api-key]", false),
    // local: GCP OAuth access token; current gitleaks master carries
    // no ya29 rule
    (
        r"\bya29\.[A-Za-z0-9_-]{20,}",
        "[REDACTED:gcp-oauth-token]",
        true,
    ),
    // gitleaks jwt, trailing-terminator guard dropped
    (
        r"\bey[a-zA-Z0-9]{17,}\.ey[a-zA-Z0-9/\\_-]{17,}\.(?:[a-zA-Z0-9/\\_-]{10,}={0,2})?",
        "[REDACTED:jwt]",
        false,
    ),
    // gitleaks openai-api-key, verbatim
    (
        r"\bsk-(?:proj|svcacct|admin)-(?:[A-Za-z0-9_-]{74}|[A-Za-z0-9_-]{58})T3BlbkFJ(?:[A-Za-z0-9_-]{74}|[A-Za-z0-9_-]{58})\b",
        "[REDACTED:openai-api-key]",
        true,
    ),
    // openai sk-proj shape; the gitleaks length anchors miss
    // truncated or freshly shaped keys in captured logs
    (
        r"\bsk-(?:proj|svcacct|admin)-[A-Za-z0-9_-]{20,}",
        "[REDACTED:openai-api-key]",
        true,
    ),
    // gitleaks openai-api-key second alternative, length-relaxed
    (
        r"\bsk-[a-zA-Z0-9]{20}T3BlbkFJ[a-zA-Z0-9]{20}",
        "[REDACTED:openai-api-key]",
        true,
    ),
    // gitleaks anthropic-api-key; the 93-char-plus-AA length anchor
    // is relaxed to a prefix-plus-body shape for log redaction
    (
        r"\bsk-ant-[A-Za-z0-9_-]{24,}",
        "[REDACTED:anthropic-api-key]",
        true,
    ),
    // gitleaks npm-access-token, trailing-terminator guard dropped
    (
        r"(?i)\bnpm_[a-z0-9]{36}",
        "[REDACTED:npm-access-token]",
        true,
    ),
    // gitleaks slack-user-token, verbatim
    (
        r"xox[pe](?:-[0-9]{10,13}){3}-[a-zA-Z0-9-]{28,34}",
        "[REDACTED:slack-user-token]",
        false,
    ),
    // gitleaks slack-bot-token; the b prefix rides a one-char class so
    // the rule source itself stays scanner-clean
    (
        r"xox[b]-[0-9]{10,13}-[0-9]{10,13}[a-zA-Z0-9-]*",
        "[REDACTED:slack-bot-token]",
        false,
    ),
    // gitleaks slack-legacy-workspace-token, verbatim
    (
        r"xox[ar]-(?:\d-)?[0-9a-zA-Z]{8,48}",
        "[REDACTED:slack-legacy-workspace-token]",
        false,
    ),
    // local: PEM private key headers, any algorithm
    (
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
        "[REDACTED:pem-private-key]",
        false,
    ),
    // local: Authorization: Basic header, base64 credentials
    (
        r"(?i)authorization[ \t]*:[ \t]*basic[ \t]+[a-z0-9+/=_-]{8,}",
        "[REDACTED:authorization-basic]",
        false,
    ),
    // local: Authorization: Bearer header
    (
        r"(?i)authorization[ \t]*:[ \t]*bearer[ \t]+[a-z0-9._~+/=-]{8,}",
        "[REDACTED:authorization-bearer]",
        false,
    ),
    // local: URL-embedded credentials, the whole authority userinfo
    (
        r"[a-z][a-z0-9+.-]*://[^\s/:@]+:[^\s/@]+@[^\s]*",
        "[REDACTED:url-credentials]",
        false,
    ),
    // local: bare Bearer token without header context; the length
    // floor keeps prose like "bearer of bad news" out
    (
        r"(?i)\bbearer[ \t]+[a-z0-9._~+/=-]{16,}",
        "[REDACTED:authorization-bearer]",
        false,
    ),
    // local: x-access-token in git remote style
    (
        r"(?i)x-access-token:[^\s]{8,}",
        "[REDACTED:x-access-token]",
        false,
    ),
];

#[cfg(test)]
mod tests {
    use super::{RULES, rules};

    fn redact(text: &str) -> String {
        rules().iter().fold(text.to_string(), |acc, rule| {
            rule.regex.replace_all(&acc, rule.marker).into_owned()
        })
    }

    #[test]
    fn every_rule_compiles() {
        assert_eq!(rules().len(), RULES.len());
    }

    #[test]
    fn ported_token_shapes_redact_with_their_rule_id() {
        let cases = [
            (format!("ghp_{}", "a".repeat(36)), "[REDACTED:github-pat]"),
            (format!("gho_{}", "b".repeat(36)), "[REDACTED:github-oauth]"),
            (
                format!("ghu_{}", "c".repeat(36)),
                "[REDACTED:github-app-token]",
            ),
            (
                format!("ghs_{}", "d".repeat(36)),
                "[REDACTED:github-app-token]",
            ),
            (
                format!("ghr_{}", "e".repeat(36)),
                "[REDACTED:github-refresh-token]",
            ),
            (
                format!("github_pat_{}", "F".repeat(82)),
                "[REDACTED:github-fine-grained-pat]",
            ),
            (
                format!("AKIA{}", "IOSFODNN7EXAMPLE"),
                "[REDACTED:aws-access-token]",
            ),
            (format!("AIza{}", "a".repeat(35)), "[REDACTED:gcp-api-key]"),
            (
                format!("npm_{}", "a".repeat(36)),
                "[REDACTED:npm-access-token]",
            ),
        ];
        for (text, marker) in cases {
            assert!(
                redact(&text).contains(marker),
                "{text} -> {}",
                redact(&text)
            );
        }
    }

    #[test]
    fn openai_anthropic_jwt_pem_and_header_shapes_redact() {
        let cases = [
            (
                format!("sk-proj-{}T3BlbkFJ{}", "a".repeat(58), "b".repeat(58)),
                "[REDACTED:openai-api-key]",
            ),
            (
                format!("sk-proj-{}", "c".repeat(22)),
                "[REDACTED:openai-api-key]",
            ),
            (
                format!("sk-ant-{}", "d".repeat(24)),
                "[REDACTED:anthropic-api-key]",
            ),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N0XFUP0THsR8U"
                    .to_string(),
                "[REDACTED:jwt]",
            ),
            (
                format!("-----BEGIN {}PRIVATE KEY-----", "OPENSSH "),
                "[REDACTED:pem-private-key]",
            ),
            (
                "Authorization: Basic dXNlcjpwYXNzd29yZA==".to_string(),
                "[REDACTED:authorization-basic]",
            ),
            (
                "Authorization: Bearer abcdefghij1234567890".to_string(),
                "[REDACTED:authorization-bearer]",
            ),
            (
                "Bearer abcdefghij1234567890".to_string(),
                "[REDACTED:authorization-bearer]",
            ),
            (
                format!(
                    "{}://{}:{}@{}/db",
                    "postgres", "user", "secretpw", "host.internal"
                ),
                "[REDACTED:url-credentials]",
            ),
            (
                "x-access-token:ghs_somevalue12345".to_string(),
                "[REDACTED:x-access-token]",
            ),
        ];
        for (text, marker) in cases {
            assert!(
                redact(&text).contains(marker),
                "{text} -> {}",
                redact(&text)
            );
        }
    }

    #[test]
    fn slack_shapes_redact() {
        let user = format!(
            "xox{}-{}",
            "p", "1234567890-1234567890-1234567890-abcdefghij1234567890abcdefghij"
        );
        let legacy = format!("xox{}-{}", "a", "2-1234567890123");
        assert!(
            redact(&user).contains("[REDACTED:slack-user-token]"),
            "{user}"
        );
        let bot = format!("xox{}-{}-{}", "b", "1234567890123", "1234567890123abc");
        assert!(redact(&bot).contains("[REDACTED:slack-bot-token]"), "{bot}");
        assert!(
            redact(&legacy).contains("[REDACTED:slack-legacy-workspace-token]"),
            "{legacy}"
        );
    }

    #[test]
    fn gcp_oauth_token_redacts() {
        let token = format!("ya29.{}", "a".repeat(24));
        assert!(
            redact(&token).contains("[REDACTED:gcp-oauth-token]"),
            "{token}"
        );
    }

    #[test]
    fn ordinary_text_never_matches_the_pattern_layer() {
        let cases = [
            "tokenize the corpus",
            "a monkey business",
            "background: #deadbeef;",
            "let key = value.into_key();",
            "uuid 550e8400-e29b-41d4-a716-446655440000 stays",
            "short creds abcdefghijkl not flagged",
            "Bearer of bad news",
        ];
        for text in cases {
            assert_eq!(redact(text), text, "{text}");
        }
    }
}
