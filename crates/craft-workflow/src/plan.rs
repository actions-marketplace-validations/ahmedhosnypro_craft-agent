//! The workflow plan: frontmatter types and the parse entry. Arg specs and
//! resolution live in `args`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::args::parse_args;
pub use crate::args::resolve_args;
use crate::error::WorkflowError;
use crate::graph;
use crate::hash::sha256_hex;
use crate::steps;
use crate::truncate_chars;
use crate::validate;
use crate::yaml;

pub const MAX_FILE_BYTES: u64 = 262_144;
pub const STEP_HARD_CAP: usize = 16;
pub const MAX_NAME_CHARS: usize = 64;
pub const MAX_DESCRIPTION_CHARS: usize = 256;
pub const MAX_PRELUDE_CHARS: usize = 8_192;
pub const MAX_ARGS: usize = 16;
/// Bounded preview of a step's final message, in prompts and the journal.
pub const PREVIEW_CHARS: usize = 4_000;

pub const DEFAULT_MAX_STEPS: u32 = 16;
pub const DEFAULT_MAX_TOTAL_TURNS: u64 = 240;
pub const DEFAULT_MAX_TOTAL_TOKENS: u64 = 4_000_000;
pub const DEFAULT_MAX_RETRIES_PER_STEP: u32 = 1;
pub const DEFAULT_MAX_CONCURRENT_STEPS: u32 = 1;
pub const DEFAULT_STEP_MAX_TURNS: u32 = 25;

pub const MAX_STEPS_CEILING: u32 = 64;
pub const MAX_TOTAL_TURNS_CEILING: u64 = 10_000;
pub const MAX_TOTAL_TOKENS_CEILING: u64 = 100_000_000;
pub const MAX_RETRIES_CEILING: u32 = 4;
pub const MAX_CONCURRENT_CEILING: u32 = 16;
pub const MAX_STEP_TURNS_CEILING: u32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    Stop,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgValue {
    Str(String),
    Seq(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgKind {
    String,
    Array,
}

impl ArgKind {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "string" => Some(Self::String),
            "array" => Some(Self::Array),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgSpec {
    pub kind: ArgKind,
    pub default: Option<ArgValue>,
}

/// Workflow-level ceilings: one fail-closed pool over three dimensions
/// (step count, total turns, total tokens) plus the concurrency slots and
/// the per-step retry ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_steps: u32,
    pub max_total_turns: u64,
    pub max_total_tokens: u64,
    pub max_retries_per_step: u32,
    pub max_concurrent_steps: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_steps: DEFAULT_MAX_STEPS,
            max_total_turns: DEFAULT_MAX_TOTAL_TURNS,
            max_total_tokens: DEFAULT_MAX_TOTAL_TOKENS,
            max_retries_per_step: DEFAULT_MAX_RETRIES_PER_STEP,
            max_concurrent_steps: DEFAULT_MAX_CONCURRENT_STEPS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSpec {
    pub id: String,
    pub agent: String,
    pub model: Option<String>,
    pub needs: Vec<String>,
    pub max_turns: u32,
    pub retry: u32,
    pub on_failure: OnFailure,
    pub outputs: Vec<String>,
    pub body: String,
    #[serde(skip)]
    pub dependants: Vec<String>,
}

/// A validated plan. The identity hash covers everything except `limits`
/// (resuming with raised limits is legal) and the derived `dependants`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkflowPlan {
    pub name: String,
    pub description: Option<String>,
    pub args: BTreeMap<String, ArgSpec>,
    #[serde(skip)]
    pub limits: Limits,
    pub steps: Vec<StepSpec>,
    #[serde(skip)]
    pub plan_hash: String,
    pub shared_body: String,
}

impl WorkflowPlan {
    pub fn step(&self, id: &str) -> Option<&StepSpec> {
        self.steps.iter().find(|spec| spec.id == id)
    }
}

/// A workflow key: 1..=MAX_NAME_CHARS chars of alphanumerics, dot, dash, underscore.
pub fn valid_key(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_NAME_CHARS
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

impl WorkflowPlan {
    pub fn parse_file(path: &Path) -> Result<Self, WorkflowError> {
        let file = path.to_string_lossy().into_owned();
        let handle = std::fs::File::open(path).map_err(|err| WorkflowError::Parse {
            file: file.clone(),
            detail: err.to_string(),
        })?;
        let mut bytes = Vec::new();
        handle
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| WorkflowError::Parse {
                file: file.clone(),
                detail: err.to_string(),
            })?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(WorkflowError::Parse {
                file,
                detail: format!("larger than {MAX_FILE_BYTES} bytes"),
            });
        }
        let text = String::from_utf8(bytes).map_err(|_| WorkflowError::Parse {
            file: file.clone(),
            detail: "not valid UTF-8".to_string(),
        })?;
        Self::parse(&file, &text)
    }

    pub fn parse(file: &str, text: &str) -> Result<Self, WorkflowError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let rest = text
            .strip_prefix("---\n")
            .or_else(|| text.strip_prefix("---\r\n"))
            .ok_or_else(|| WorkflowError::Parse {
                file: file.to_string(),
                detail: "missing `---` opening the frontmatter".to_string(),
            })?;
        let (frontmatter, shared_body) =
            split_frontmatter(rest).map_err(|detail| WorkflowError::Parse {
                file: file.to_string(),
                detail,
            })?;
        let shared_body = truncate_chars(shared_body.trim(), MAX_PRELUDE_CHARS);
        let root = yaml::parse(frontmatter).map_err(|detail| WorkflowError::Parse {
            file: file.to_string(),
            detail,
        })?;
        let front = Frontmatter { file, root: &root };
        front.check_fields()?;
        let name = front.name()?;
        let description = front.description()?;
        let args = parse_args(file, front.root.get("args"))?;
        let limits = validate::convert_limits(file, front.root.get("limits"))?;
        let mut steps = steps::convert_steps(file, front.root.get("steps"))?;
        graph::finalize_graph(file, &mut steps)?;
        graph::check_arg_refs(file, &steps, &shared_body, &args)?;
        let plan = Self {
            name,
            description,
            args,
            limits,
            steps,
            plan_hash: String::new(),
            shared_body,
        };
        let hash_input = serde_json::to_string(&plan).map_err(|err| WorkflowError::Parse {
            file: file.to_string(),
            detail: err.to_string(),
        })?;
        Ok(Self {
            plan_hash: sha256_hex(&[hash_input.as_bytes()]),
            ..plan
        })
    }
}

const FRONTMATTER_FIELDS: &[&str] = &["name", "description", "args", "limits", "steps"];

/// A parsed frontmatter root, with the file name for error context.
struct Frontmatter<'a> {
    file: &'a str,
    root: &'a yaml::YValue,
}

impl Frontmatter<'_> {
    /// The root must be a map holding only known fields.
    fn check_fields(&self) -> Result<(), WorkflowError> {
        let entries = self.root.as_map().ok_or_else(|| WorkflowError::Parse {
            file: self.file.to_string(),
            detail: "frontmatter must be a mapping".to_string(),
        })?;
        for (key, _) in entries {
            if !FRONTMATTER_FIELDS.contains(&key.as_str()) {
                return Err(WorkflowError::UnknownField {
                    file: self.file.to_string(),
                    context: "frontmatter".to_string(),
                    field: key.clone(),
                });
            }
        }
        Ok(())
    }

    fn name(&self) -> Result<String, WorkflowError> {
        let name = match self.root.get("name") {
            Some(value) => value.as_str().ok_or_else(|| WorkflowError::Parse {
                file: self.file.to_string(),
                detail: "`name` must be a string".to_string(),
            })?,
            None => {
                return Err(WorkflowError::MissingName {
                    file: self.file.to_string(),
                });
            }
        };
        if !valid_key(name) {
            return Err(WorkflowError::Parse {
                file: self.file.to_string(),
                detail: format!(
                    "`name` `{name}` is not 1..={MAX_NAME_CHARS} chars of a-z, 0-9, dot, dash, underscore"
                ),
            });
        }
        Ok(name.to_string())
    }

    fn description(&self) -> Result<Option<String>, WorkflowError> {
        let Some(value) = self.root.get("description") else {
            return Ok(None);
        };
        let text = value.as_str().ok_or_else(|| WorkflowError::Parse {
            file: self.file.to_string(),
            detail: "`description` must be a string".to_string(),
        })?;
        if text.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(WorkflowError::Parse {
                file: self.file.to_string(),
                detail: "`description` exceeds 256 chars".to_string(),
            });
        }
        Ok(Some(text.to_string()))
    }
}

fn split_frontmatter(rest: &str) -> Result<(&str, &str), String> {
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare == "---" {
            return Ok((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err("frontmatter is not closed by a `---` line".to_string())
}
