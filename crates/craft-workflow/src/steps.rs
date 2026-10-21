//! Step conversion: frontmatter step entries into validated StepSpecs.

use std::collections::BTreeSet;

use crate::error::WorkflowError;
use crate::gates::{self, check_gates};
use crate::plan::{STEP_HARD_CAP, StepSpec, valid_key};
use crate::yaml::YValue;

const MAX_BODY_CHARS: usize = 16_384;
/// Cap shared by the list-valued step fields (`needs`, `outputs`).
const MAX_LIST_PER_STEP: usize = 16;

fn string_list(
    file: &str,
    step: &str,
    field: &str,
    value: Option<&YValue>,
) -> Result<Vec<String>, WorkflowError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value.as_seq().ok_or_else(|| WorkflowError::Parse {
        file: file.to_string(),
        detail: format!("step `{step}`: `{field}` must be a list"),
    })?;
    if items.len() > MAX_LIST_PER_STEP {
        return Err(WorkflowError::Parse {
            file: file.to_string(),
            detail: format!("step `{step}`: `{field}` exceeds {MAX_LIST_PER_STEP} entries"),
        });
    }
    let mut out = Vec::new();
    for item in items {
        let text = item.as_str().ok_or_else(|| WorkflowError::Parse {
            file: file.to_string(),
            detail: format!("step `{step}`: `{field}` entries must be strings"),
        })?;
        if !valid_key(text) {
            return Err(WorkflowError::Parse {
                file: file.to_string(),
                detail: format!("step `{step}`: `{field}` entry `{text}` is not a valid name"),
            });
        }
        out.push(text.to_string());
    }
    Ok(out)
}

fn convert_step(
    file: &str,
    index: usize,
    fields: &[(String, YValue)],
) -> Result<StepSpec, WorkflowError> {
    for (key, _) in fields {
        if !matches!(
            key.as_str(),
            "id" | "agent"
                | "model"
                | "kind"
                | "judge"
                | "needs"
                | "max-turns"
                | "retry"
                | "on-failure"
                | "deny-read"
                | "deny-write"
                | "outputs"
                | "body"
        ) {
            return Err(WorkflowError::UnknownField {
                file: file.to_string(),
                context: format!("step {index}"),
                field: key.clone(),
            });
        }
    }
    let id = match fields.iter().find(|(k, _)| k == "id") {
        Some((_, value)) => value.as_str().ok_or_else(|| WorkflowError::Parse {
            file: file.to_string(),
            detail: format!("step {index}: `id` must be a string"),
        })?,
        None => {
            return Err(WorkflowError::MissingStepId {
                file: file.to_string(),
                index,
            });
        }
    };
    if !valid_key(id) {
        return Err(WorkflowError::BadId {
            file: file.to_string(),
            raw: id.to_string(),
        });
    }
    let agent = match fields.iter().find(|(k, _)| k == "agent") {
        Some((_, value)) => value.as_str().ok_or_else(|| WorkflowError::Parse {
            file: file.to_string(),
            detail: format!("step `{id}`: `agent` must be a string"),
        })?,
        None => "",
    };
    if agent.is_empty() {
        return Err(WorkflowError::EmptyAgent {
            file: file.to_string(),
            step: id.to_string(),
        });
    }
    if agent.len() > 64 {
        return Err(WorkflowError::Parse {
            file: file.to_string(),
            detail: format!("step `{id}`: `agent` exceeds 64 chars"),
        });
    }
    let model = match fields.iter().find(|(k, _)| k == "model") {
        Some((_, value)) => {
            let text = value.as_str().ok_or_else(|| WorkflowError::Parse {
                file: file.to_string(),
                detail: format!("step `{id}`: `model` must be a string"),
            })?;
            if text.is_empty() || text.len() > 64 {
                return Err(WorkflowError::Parse {
                    file: file.to_string(),
                    detail: format!("step `{id}`: `model` must be 1..=64 chars"),
                });
            }
            Some(text.to_string())
        }
        None => None,
    };
    step_rest(file, id, agent, model, fields)
}

fn step_rest(
    file: &str,
    id: &str,
    agent: &str,
    model: Option<String>,
    fields: &[(String, YValue)],
) -> Result<StepSpec, WorkflowError> {
    let field = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let needs = string_list(file, id, "needs", field("needs"))?;
    let mut seen = BTreeSet::new();
    for need in &needs {
        if !seen.insert(need.clone()) {
            return Err(WorkflowError::DuplicateNeed {
                file: file.to_string(),
                step: id.to_string(),
                need: need.clone(),
            });
        }
    }
    let converted = gates::Converted::parse(file, id, fields)?;
    let outputs = string_list(file, id, "outputs", field("outputs"))?;
    let body = match field("body") {
        Some(value) => value.as_str().ok_or_else(|| WorkflowError::Parse {
            file: file.to_string(),
            detail: format!("step `{id}`: `body` must be a text block"),
        })?,
        None => {
            return Err(WorkflowError::MissingBody {
                file: file.to_string(),
                step: id.to_string(),
            });
        }
    };
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(WorkflowError::Parse {
            file: file.to_string(),
            detail: format!("step `{id}`: `body` exceeds {MAX_BODY_CHARS} chars"),
        });
    }
    check_gates(file, id, &converted, &needs)?;
    Ok(StepSpec {
        id: id.to_string(),
        agent: agent.to_string(),
        model,
        kind: converted.kind,
        judge: converted.judge,
        needs,
        max_turns: converted.max_turns,
        retry: converted.retry,
        on_failure: converted.on_failure,
        deny_read: converted.deny_read,
        deny_write: converted.deny_write,
        outputs,
        body: body.to_string(),
        dependants: Vec::new(),
    })
}

pub(crate) fn convert_steps(
    file: &str,
    value: Option<&YValue>,
) -> Result<Vec<StepSpec>, WorkflowError> {
    let Some(value) = value else {
        return Err(WorkflowError::MissingSteps {
            file: file.to_string(),
        });
    };
    let items = value.as_seq().ok_or_else(|| WorkflowError::Parse {
        file: file.to_string(),
        detail: "`steps` must be a list".to_string(),
    })?;
    if items.is_empty() {
        return Err(WorkflowError::MissingSteps {
            file: file.to_string(),
        });
    }
    if items.len() > STEP_HARD_CAP {
        return Err(WorkflowError::StepCountCap {
            file: file.to_string(),
            count: items.len(),
            cap: STEP_HARD_CAP,
        });
    }
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let fields = item.as_map().ok_or_else(|| WorkflowError::Parse {
            file: file.to_string(),
            detail: format!("step {index} must be a map"),
        })?;
        let spec = convert_step(file, index, fields)?;
        if out.iter().any(|other: &StepSpec| other.id == spec.id) {
            return Err(WorkflowError::DuplicateStepId {
                file: file.to_string(),
                id: spec.id,
            });
        }
        out.push(spec);
    }
    Ok(out)
}
