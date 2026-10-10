//! The spawn-request types: the single and batch argument shapes with
//! their hostile-input bounds. Shape validation only — the spawn hub
//! resolves the profile, admits the spawn, and owns the fallback
//! defaults.

use serde::Deserialize;

/// Profile names share the agent-file name space: at most 64 bytes.
pub const AGENT_NAME_MAX_BYTES: usize = 64;
/// The child's whole context: 1..=65,536 bytes.
pub const PROMPT_MAX_BYTES: usize = 65_536;
/// Turn budget: 1..=10,000.
pub const MAX_TURNS_CEILING: u32 = 10_000;
/// Fallback turn budget when neither the spawn arg nor the profile
/// names one.
pub const DEFAULT_MAX_TURNS: u32 = 25;
/// Wall-clock ceiling in seconds; zero means explicitly no timeout.
pub const TIMEOUT_SECS_CEILING: u64 = 21_600;
/// Batch members per call: at least two.
pub const BATCH_MIN_ITEMS: usize = 2;
/// Batch members per call: at most thirty-two.
pub const BATCH_MAX_ITEMS: usize = 32;
/// Sum of batch item bytes per call.
pub const BATCH_MAX_TOTAL_BYTES: usize = 1_048_576;
/// The exact placeholder a batch prompt template carries.
pub const ITEM_PLACEHOLDER: &str = "{{item}}";

/// Fail-closed validation of a spawn request; every error names the
/// refusing argument and its bound.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubagentError {
    #[error("`agent` must be a non-empty profile name")]
    EmptyAgent,
    #[error("`agent` is {bytes} bytes, over {max}")]
    AgentNameSize { bytes: usize, max: usize },
    #[error("`prompt` must be 1..={max} bytes, got {bytes}")]
    PromptSize { bytes: usize, max: usize },
    #[error("`maxTurns` must be 1..={max}, got {value}")]
    MaxTurns { value: u32, max: u32 },
    #[error("`maxTokens` must be at least 1, got {value}")]
    MaxTokens { value: u64 },
    #[error("`timeoutSecs` must be at most {max}, got {value}")]
    TimeoutSecs { value: u64, max: u64 },
    #[error("`promptTemplate` must contain the {{{{item}}}} placeholder")]
    MissingPlaceholder,
    #[error("`promptTemplate` is {bytes} bytes, over {max}")]
    TemplateSize { bytes: usize, max: usize },
    #[error("`items` must be {min}..={max} entries, got {count}")]
    BatchItems {
        count: usize,
        min: usize,
        max: usize,
    },
    #[error("`items` entry {index} is {bytes} bytes, over {max}")]
    ItemSize {
        index: usize,
        bytes: usize,
        max: usize,
    },
    #[error("`items` total {bytes} bytes, over {max}")]
    BatchBytes { bytes: usize, max: usize },
    #[error("duplicate filled prompts at items {indices:?}")]
    DuplicatePrompts { indices: Vec<usize> },
    #[error("the filled prompt for item {index} is {bytes} bytes, over {max}")]
    FilledPromptSize {
        index: usize,
        bytes: usize,
        max: usize,
    },
}

/// One validated spawn request. `prompt` is the child's whole context:
/// besides the profile name, the only parent-to-child payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpawnSpec {
    pub agent: String,
    pub prompt: String,
    /// Turn budget; None falls back to the profile, then the default.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// The child's token slice; None means the parent's remaining budget.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// None falls back to the profile's timeout; Some(0) means none.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// The budget arguments shared by both spawn forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpawnBudget {
    /// Turn budget; None falls back to the profile, then the default.
    pub max_turns: Option<u32>,
    /// The child's token slice; None means the parent's remaining budget.
    pub max_tokens: Option<u64>,
    /// None falls back to the profile's timeout; Some(0) means none.
    pub timeout_secs: Option<u64>,
}

impl SpawnSpec {
    /// Validates the argument shape; nothing launches until this passes.
    pub fn new(agent: &str, prompt: &str, budget: SpawnBudget) -> Result<Self, SubagentError> {
        let spec = Self {
            agent: agent.to_string(),
            prompt: prompt.to_string(),
            max_turns: budget.max_turns,
            max_tokens: budget.max_tokens,
            timeout_secs: budget.timeout_secs,
        };
        check_spec(&spec)?;
        Ok(spec)
    }

    /// Re-validates a spec that arrived by deserialization.
    pub fn validate(self) -> Result<Self, SubagentError> {
        check_spec(&self)?;
        Ok(self)
    }
}

/// The shared argument checks; every model-facing number is hostile.
fn check_spec(spec: &SpawnSpec) -> Result<(), SubagentError> {
    check_agent(&spec.agent)?;
    check_prompt(&spec.prompt)?;
    check_budget(SpawnBudget {
        max_turns: spec.max_turns,
        max_tokens: spec.max_tokens,
        timeout_secs: spec.timeout_secs,
    })
}

fn check_agent(agent: &str) -> Result<(), SubagentError> {
    if agent.is_empty() {
        return Err(SubagentError::EmptyAgent);
    }
    if agent.len() > AGENT_NAME_MAX_BYTES {
        return Err(SubagentError::AgentNameSize {
            bytes: agent.len(),
            max: AGENT_NAME_MAX_BYTES,
        });
    }
    Ok(())
}

fn check_prompt(prompt: &str) -> Result<(), SubagentError> {
    let bytes = prompt.len();
    if bytes == 0 || bytes > PROMPT_MAX_BYTES {
        return Err(SubagentError::PromptSize {
            bytes,
            max: PROMPT_MAX_BYTES,
        });
    }
    Ok(())
}

fn check_budget(budget: SpawnBudget) -> Result<(), SubagentError> {
    if let Some(value) = budget.max_turns
        && bad_turns(value)
    {
        return Err(SubagentError::MaxTurns {
            value,
            max: MAX_TURNS_CEILING,
        });
    }
    if budget.max_tokens == Some(0) {
        return Err(SubagentError::MaxTokens { value: 0 });
    }
    if let Some(value) = budget.timeout_secs
        && value > TIMEOUT_SECS_CEILING
    {
        return Err(SubagentError::TimeoutSecs {
            value,
            max: TIMEOUT_SECS_CEILING,
        });
    }
    Ok(())
}

fn bad_turns(value: u32) -> bool {
    value == 0 || value > MAX_TURNS_CEILING
}

/// The batch form: one shared profile and budget, one prompt template
/// filled once per item.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpawnBatch {
    pub agent: String,
    pub prompt_template: String,
    pub items: Vec<String>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl SpawnBatch {
    /// Fills the template per item and validates every member up front:
    /// all members launch or none do.
    pub fn expand(&self) -> Result<Vec<SpawnSpec>, SubagentError> {
        let count = self.items.len();
        if !(BATCH_MIN_ITEMS..=BATCH_MAX_ITEMS).contains(&count) {
            return Err(SubagentError::BatchItems {
                count,
                min: BATCH_MIN_ITEMS,
                max: BATCH_MAX_ITEMS,
            });
        }
        if !self.prompt_template.contains(ITEM_PLACEHOLDER) {
            return Err(SubagentError::MissingPlaceholder);
        }
        let template_bytes = self.prompt_template.len();
        if template_bytes > PROMPT_MAX_BYTES {
            return Err(SubagentError::TemplateSize {
                bytes: template_bytes,
                max: PROMPT_MAX_BYTES,
            });
        }
        let mut total = 0usize;
        for (index, item) in self.items.iter().enumerate() {
            let bytes = item.len();
            if bytes > PROMPT_MAX_BYTES {
                return Err(SubagentError::ItemSize {
                    index: index + 1,
                    bytes,
                    max: PROMPT_MAX_BYTES,
                });
            }
            total = total.saturating_add(bytes);
        }
        if total > BATCH_MAX_TOTAL_BYTES {
            return Err(SubagentError::BatchBytes {
                bytes: total,
                max: BATCH_MAX_TOTAL_BYTES,
            });
        }
        self.fill()
    }

    /// Duplicate scan and per-member prompt validation, in item order.
    /// The refusal names every member of a duplicate group.
    fn fill(&self) -> Result<Vec<SpawnSpec>, SubagentError> {
        let mut filled: Vec<String> = Vec::with_capacity(self.items.len());
        for item in &self.items {
            filled.push(self.prompt_template.replace(ITEM_PLACEHOLDER, item));
        }
        let dups: Vec<usize> = filled
            .iter()
            .enumerate()
            .filter(|(_, text)| filled.iter().filter(|other| other == text).count() > 1)
            .map(|(index, _)| index + 1)
            .collect();
        if !dups.is_empty() {
            return Err(SubagentError::DuplicatePrompts { indices: dups });
        }
        let budget = SpawnBudget {
            max_turns: self.max_turns,
            max_tokens: self.max_tokens,
            timeout_secs: self.timeout_secs,
        };
        let mut specs = Vec::with_capacity(filled.len());
        for (index, text) in filled.iter().enumerate() {
            let spec = SpawnSpec::new(&self.agent, text, budget).map_err(|err| match err {
                SubagentError::PromptSize { bytes, max } => SubagentError::FilledPromptSize {
                    index: index + 1,
                    bytes,
                    max,
                },
                other => other,
            })?;
            specs.push(spec);
        }
        Ok(specs)
    }
}
