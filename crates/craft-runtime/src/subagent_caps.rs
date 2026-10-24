//! The run-wide spawn caps and their environment knobs: three bounded
//! numbers resolved from the process environment, every hostile value a
//! typed error naming its knob and range. The hub itself lives in
//! `subagent`.

/// Depth of subagents a run may admit; 0 disables spawning entirely.
pub const ENV_MAX_DEPTH: &str = "CRAFT_AGENT_SUBAGENT_MAX_DEPTH";
/// Total subagents one run may admit (the id-churn bound).
pub const ENV_MAX_TOTAL: &str = "CRAFT_AGENT_SUBAGENT_MAX_TOTAL";
/// Subagents that may run at once, tree-wide.
pub const ENV_MAX_CONCURRENCY: &str = "CRAFT_AGENT_SUBAGENT_MAX_CONCURRENCY";

/// One cap knob: its environment name plus its default and bounds.
pub(crate) struct CapSpec {
    pub(crate) knob: &'static str,
    pub(crate) default: u64,
    pub(crate) min: u64,
    pub(crate) max: u64,
}

impl CapSpec {
    pub(crate) const DEPTH: Self = Self {
        knob: ENV_MAX_DEPTH,
        default: 2,
        min: 0,
        max: 4,
    };
    pub(crate) const TOTAL: Self = Self {
        knob: ENV_MAX_TOTAL,
        default: 32,
        min: 1,
        max: 1024,
    };
    pub(crate) const CONCURRENCY: Self = Self {
        knob: ENV_MAX_CONCURRENCY,
        default: 4,
        min: 1,
        max: 1024,
    };

    pub(crate) fn range_error(&self) -> String {
        format!(
            "{} must be a whole number between {} and {}",
            self.knob, self.min, self.max
        )
    }
}

/// One knob's resolved value: a missing knob keeps its default, an
/// empty or out-of-range one is a typed error.
pub(crate) fn parse_cap(spec: CapSpec, raw: Option<String>) -> Result<u32, String> {
    let Some(raw) = raw else {
        return Ok(u32::try_from(spec.default).unwrap_or_default());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(spec.range_error());
    }
    let value: u64 = trimmed
        .parse()
        .map_err(|_| format!("{}; got `{trimmed}`", spec.range_error()))?;
    if value < spec.min || value > spec.max {
        return Err(format!("{}; got `{trimmed}`", spec.range_error()));
    }
    Ok(u32::try_from(value).unwrap_or_default())
}
