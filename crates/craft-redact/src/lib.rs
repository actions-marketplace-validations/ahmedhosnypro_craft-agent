//! Secret redaction for tool output captured into public logs and
//! transcripts. Layer 1 removes known values from the environment, in
//! every encoding the incident showed up in; layer 2 removes
//! well-known token shapes via regex rules.

mod patterns;
mod registry;
mod variants;

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

pub use registry::{MAX_REGISTRY_VALUES, MAX_VALUE_BYTES};

const KNOWN_SECRET: &str = "[REDACTED:known-secret]";

/// Shared redaction state: registry needles, pattern rules, counters.
/// Needle automata are None only when a build exceeds a library
/// limit; the layer then contributes no matches.
pub struct Redactor {
    enabled: bool,
    needles: Option<aho_corasick::AhoCorasick>,
    values: Vec<String>,
    values_set: HashSet<String>,
    patterns: Vec<patterns::Pattern>,
    redactions: AtomicU64,
    raw_hits: AtomicU64,
}

impl Redactor {
    /// Registry and pattern layers from the process environment plus
    /// CRAFT_REDACT_EXTRA; `CRAFT_REDACT=off` disables the redactor.
    pub fn from_env() -> Self {
        let enabled = !registry::switch_off();
        let values = registry::collect_env();
        Self::assemble(values, patterns::rules(), enabled)
    }

    /// Explicit construction: the values land in the registry in all
    /// their encodings, and `patterns` switches layer 2 on. The
    /// off-switch never applies here.
    pub fn from_parts(values: Vec<String>, patterns: bool) -> Self {
        let pattern_rules = if patterns {
            patterns::rules()
        } else {
            Vec::new()
        };
        Self::assemble(values, pattern_rules, true)
    }

    /// False when the off-switch was read: text passes through
    /// untouched and counters stay at zero.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Layer 1 over known values first, then layer 2 over the result.
    /// Layer 2 matches only token-shaped spans, so it never restores
    /// redacted content.
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if !self.enabled {
            return Cow::Borrowed(text);
        }
        let after_l1 = self.strip_known(text);
        let after_l2 = self.strip_patterns(&after_l1);
        match after_l2 {
            Cow::Borrowed(_) => after_l1,
            Cow::Owned(s) => Cow::Owned(s),
        }
    }

    /// Total replacements made so far across all calls and threads.
    pub fn redactions(&self) -> u64 {
        self.redactions.load(Ordering::Relaxed)
    }

    /// Replacements where the match was a raw registered value or a
    /// raw token-shaped pattern hit (not an encoded variant).
    pub fn raw_secret_hits(&self) -> u64 {
        self.raw_hits.load(Ordering::Relaxed)
    }

    /// The registered values in raw form, for `::add-mask::` emission
    /// by the caller.
    pub fn mask_values(&self) -> Vec<String> {
        self.values.clone()
    }
}

impl Redactor {
    fn assemble(values: Vec<String>, patterns: Vec<patterns::Pattern>, enabled: bool) -> Self {
        let values = registry::cap(registry::dedupe(values));
        let values_set: HashSet<String> = values.iter().cloned().collect();
        let variants: Vec<String> = values
            .iter()
            .flat_map(|value| variants::of(value))
            .collect();
        Self {
            enabled,
            needles: build_automaton(&variants),
            values,
            values_set,
            patterns,
            redactions: AtomicU64::new(0),
            raw_hits: AtomicU64::new(0),
        }
    }

    /// Replace every known-value match with the known-secret token,
    /// counting matches on a raw registered value as raw hits.
    fn strip_known<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let Some(needles) = &self.needles else {
            return Cow::Borrowed(text);
        };
        if text.is_empty() {
            return Cow::Borrowed(text);
        }
        let matches: Vec<_> = needles.find_iter(text).collect();
        if matches.is_empty() {
            return Cow::Borrowed(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        let mut redactions = 0u64;
        let mut raw_hits = 0u64;
        for m in &matches {
            if self.values_set.contains(&text[m.start()..m.end()]) {
                raw_hits += 1;
            }
            out.push_str(&text[last..m.start()]);
            out.push_str(KNOWN_SECRET);
            last = m.end();
            redactions += 1;
        }
        out.push_str(&text[last..]);
        self.redactions.fetch_add(redactions, Ordering::Relaxed);
        self.raw_hits.fetch_add(raw_hits, Ordering::Relaxed);
        Cow::Owned(out)
    }

    /// Replace every pattern-rule match, counting raw-token-shaped
    /// rules as raw hits. Pattern rules are token-shaped by design, so
    /// a match can never reveal a redaction token's surroundings.
    fn strip_patterns<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if text.is_empty() || self.patterns.is_empty() {
            return Cow::Borrowed(text);
        }
        let mut out = Cow::Borrowed(text);
        for rule in &self.patterns {
            if !rule.regex.is_match(&out) {
                continue;
            }
            let hits = rule.regex.find_iter(&out).count();
            out = Cow::Owned(rule.regex.replace_all(&out, rule.marker).into_owned());
            self.redactions.fetch_add(hits as u64, Ordering::Relaxed);
            if rule.raw {
                self.raw_hits.fetch_add(hits as u64, Ordering::Relaxed);
            }
        }
        out
    }
}

/// Leftmost-longest automaton over the needles; None when the build
/// exceeds a library limit, which the layer treats as no needles.
fn build_automaton(needles: &[String]) -> Option<aho_corasick::AhoCorasick> {
    aho_corasick::AhoCorasick::builder()
        .match_kind(aho_corasick::MatchKind::LeftmostLongest)
        .build(needles)
        .ok()
}

/// Counts and switch state only; needle text never reaches Debug.
impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor")
            .field("enabled", &self.enabled)
            .field("values", &self.values.len())
            .field("rules", &self.patterns.len())
            .field("redactions", &self.redactions())
            .field("raw_hits", &self.raw_secret_hits())
            .finish()
    }
}
