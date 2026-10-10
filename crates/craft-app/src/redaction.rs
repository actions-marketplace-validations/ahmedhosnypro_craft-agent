//! The Actions-log belt: register known secret values with the
//! runner's mask machinery before anything can print them, and the
//! summary-row shape for redaction counters.

use crate::config::EnvMap;

/// `::add-mask::` every registered value so the rendered job log
/// masks them too; harmless outside Actions, where the runner ignores
/// workflow commands.
pub(crate) fn emit_add_mask(redactor: &craft_redact::Redactor, env: &EnvMap) {
    if !env.contains_key("GITHUB_ACTIONS") {
        return;
    }
    for value in redactor.mask_values() {
        eprintln!("::add-mask::{value}");
    }
}

/// One summary row with the run's redaction counters; empty when the
/// redactor is off.
pub(crate) fn summary_rows(redactor: &craft_redact::Redactor) -> Vec<(&'static str, String)> {
    if !redactor.is_enabled() {
        return Vec::new();
    }
    vec![(
        "redactions",
        format!(
            "{} (raw {})",
            redactor.redactions(),
            redactor.raw_secret_hits()
        ),
    )]
}

/// Terminal-line detail for failed runs: redacted the same way tool
/// output is.
pub(crate) fn terminal_detail(
    error: &impl std::fmt::Display,
    redactor: &craft_redact::Redactor,
) -> String {
    redactor.redact(&error.to_string()).into_owned()
}
