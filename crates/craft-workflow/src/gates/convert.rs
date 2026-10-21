//! Step and judge field conversion: the scalar YAML fields a step
//! accepts, parsed under the shared caps.

use crate::error::WorkflowError;
use crate::plan::{
    DEFAULT_STEP_MAX_TURNS, JudgeMerge, JudgeSpec, MAX_FEEDBACK, MAX_PANEL, MAX_RETRIES_CEILING,
    MAX_STEP_TURNS_CEILING, OnFailure, StepKind,
};
use crate::scope::{MAX_SCOPE_GLOBS, valid_glob};
use crate::validate::parse_u64;
use crate::yaml::YValue;

/// The keys a judge block accepts; anything else fails closed.
const JUDGE_KEYS: &[&str] = &["panel", "merge", "max-feedback"];

/// The parsed scalar fields of one step, before the shared checks.
pub(crate) struct Converted {
    pub kind: StepKind,
    pub judge: Option<JudgeSpec>,
    pub max_turns: u32,
    pub retry: u32,
    pub on_failure: OnFailure,
    pub deny_read: Vec<String>,
    pub deny_write: Vec<String>,
}

/// Where a field's parse error points: the file, the step, the field.
pub(crate) struct FieldCtx<'a> {
    file: &'a str,
    step: &'a str,
    field: &'a str,
}

impl<'a> FieldCtx<'a> {
    pub(crate) fn new(file: &'a str, step: &'a str, field: &'a str) -> Self {
        Self { file, step, field }
    }

    /// A sub-field of the one already in hand: the `judge` block's own keys.
    fn nested(&self, field: &'a str) -> Self {
        Self {
            file: self.file,
            step: self.step,
            field,
        }
    }

    fn bad_value(&self, detail: String) -> WorkflowError {
        WorkflowError::Parse {
            file: self.file.to_string(),
            detail: format!("step `{}`: {detail}", self.step),
        }
    }

    fn bad_scope(&self, detail: String) -> WorkflowError {
        WorkflowError::BadScope {
            file: self.file.to_string(),
            step: self.step.to_string(),
            detail,
        }
    }

    fn bad_turns(&self, detail: String) -> WorkflowError {
        WorkflowError::BadMaxTurns {
            file: self.file.to_string(),
            step: self.step.to_string(),
            detail,
        }
    }

    fn bad_on_failure(&self, value: String) -> WorkflowError {
        WorkflowError::BadOnFailure {
            file: self.file.to_string(),
            step: self.step.to_string(),
            value,
        }
    }

    /// `retry` reports as a retry count, every other field as a judge count.
    fn bad_count(&self, value: String, cap: u32) -> WorkflowError {
        if self.field == "retry" {
            WorkflowError::BadRetry {
                file: self.file.to_string(),
                step: self.step.to_string(),
                value,
                cap,
            }
        } else {
            WorkflowError::BadJudge {
                file: self.file.to_string(),
                step: self.step.to_string(),
                field: self.field.to_string(),
                value,
                cap,
            }
        }
    }
}

impl Converted {
    pub(crate) fn parse<'a>(
        file: &'a str,
        id: &'a str,
        fields: &[(String, YValue)],
    ) -> Result<Self, WorkflowError> {
        let field = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v);
        let ctx = |name: &'a str| FieldCtx::new(file, id, name);
        let kind = parse_kind(&ctx("kind"), field("kind"))?;
        let judge = parse_judge(&ctx("judge"), &kind, field("judge"))?;
        let max_turns = parse_turns(&ctx("max-turns"), field("max-turns"))?;
        let retry = parse_count(&ctx("retry"), field("retry"), MAX_RETRIES_CEILING, 0)?;
        let on_failure = parse_on_failure(&ctx("on-failure"), field("on-failure"))?;
        let deny_read = parse_globs(&ctx("deny-read"), field("deny-read"))?;
        let deny_write = parse_globs(&ctx("deny-write"), field("deny-write"))?;
        Ok(Self {
            kind,
            judge,
            max_turns,
            retry,
            on_failure,
            deny_read,
            deny_write,
        })
    }
}

fn parse_kind(ctx: &FieldCtx<'_>, value: Option<&YValue>) -> Result<StepKind, WorkflowError> {
    let Some(value) = value else {
        return Ok(StepKind::Agent);
    };
    match value.as_str() {
        Some("agent") => Ok(StepKind::Agent),
        Some("judge") => Ok(StepKind::Judge),
        Some(other) => Err(ctx.bad_value(format!("`kind` is `{other}`, expected agent|judge"))),
        None => Err(ctx.bad_value("`kind` must be a string".to_string())),
    }
}

fn parse_judge(
    ctx: &FieldCtx<'_>,
    kind: &StepKind,
    value: Option<&YValue>,
) -> Result<Option<JudgeSpec>, WorkflowError> {
    let Some(value) = value else {
        if kind == &StepKind::Judge {
            return Err(ctx.bad_value("`kind: judge` requires a `judge` block".to_string()));
        }
        return Ok(None);
    };
    if kind == &StepKind::Agent {
        return Err(ctx.bad_value("a `judge` block requires `kind: judge`".to_string()));
    }
    let entries = value
        .as_map()
        .ok_or_else(|| ctx.bad_value("`judge` must be a mapping".to_string()))?;
    for (key, _) in entries {
        if !JUDGE_KEYS.contains(&key.as_str()) {
            return Err(WorkflowError::UnknownField {
                file: ctx.file.to_string(),
                context: format!("step `{}`: judge", ctx.step),
                field: key.clone(),
            });
        }
    }
    judge_spec(ctx, entries).map(Some)
}

fn judge_spec(
    ctx: &FieldCtx<'_>,
    entries: &[(String, YValue)],
) -> Result<JudgeSpec, WorkflowError> {
    let panel = parse_count(
        &ctx.nested("panel"),
        map_get(entries, "panel"),
        MAX_PANEL,
        1,
    )?;
    let max_feedback = parse_count(
        &ctx.nested("max-feedback"),
        map_get(entries, "max-feedback"),
        MAX_FEEDBACK,
        0,
    )?;
    let merge = parse_merge(ctx, map_get(entries, "merge"))?;
    Ok(JudgeSpec {
        panel,
        merge,
        max_feedback,
    })
}

fn map_get<'a>(entries: &'a [(String, YValue)], name: &str) -> Option<&'a YValue> {
    entries.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

fn parse_merge(ctx: &FieldCtx<'_>, value: Option<&YValue>) -> Result<JudgeMerge, WorkflowError> {
    let Some(value) = value else {
        return Ok(JudgeMerge::Majority);
    };
    match value.as_str() {
        Some("majority") => Ok(JudgeMerge::Majority),
        Some("unanimous") => Ok(JudgeMerge::Unanimous),
        _ => Err(ctx.bad_value("`judge.merge` must be majority|unanimous".to_string())),
    }
}

fn parse_turns(ctx: &FieldCtx<'_>, value: Option<&YValue>) -> Result<u32, WorkflowError> {
    let Some(value) = value else {
        return Ok(DEFAULT_STEP_MAX_TURNS);
    };
    let text = value
        .as_str()
        .ok_or_else(|| ctx.bad_turns("must be a positive integer".to_string()))?;
    let Some(number) = parse_u64(text) else {
        return Err(ctx.bad_turns(format!("`{text}` is not a positive integer")));
    };
    if number == 0 {
        return Err(ctx.bad_turns("must be positive".to_string()));
    }
    if number > u64::from(MAX_STEP_TURNS_CEILING) {
        return Err(ctx.bad_turns(format!(
            "`{number}` exceeds the {MAX_STEP_TURNS_CEILING} ceiling"
        )));
    }
    Ok(number as u32)
}

fn parse_count(
    ctx: &FieldCtx<'_>,
    value: Option<&YValue>,
    cap: u32,
    fallback: u32,
) -> Result<u32, WorkflowError> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    let text = value
        .as_str()
        .ok_or_else(|| ctx.bad_count("(non-string)".to_string(), cap))?;
    let Some(number) = parse_u64(text) else {
        return Err(ctx.bad_count(text.to_string(), cap));
    };
    if number > u64::from(cap) {
        return Err(ctx.bad_count(text.to_string(), cap));
    }
    Ok(number as u32)
}

fn parse_on_failure(
    ctx: &FieldCtx<'_>,
    value: Option<&YValue>,
) -> Result<OnFailure, WorkflowError> {
    let Some(value) = value else {
        return Ok(OnFailure::Stop);
    };
    let text = value
        .as_str()
        .ok_or_else(|| ctx.bad_on_failure("(non-string)".to_string()))?;
    OnFailure::parse(text).ok_or_else(|| ctx.bad_on_failure(text.to_string()))
}

fn parse_globs(ctx: &FieldCtx<'_>, value: Option<&YValue>) -> Result<Vec<String>, WorkflowError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value
        .as_seq()
        .ok_or_else(|| ctx.bad_scope(format!("`{}` must be a list of globs", ctx.field)))?;
    if items.len() > MAX_SCOPE_GLOBS {
        return Err(ctx.bad_scope(format!("`{}` exceeds {MAX_SCOPE_GLOBS} globs", ctx.field)));
    }
    let mut out = Vec::new();
    for item in items {
        let text = item
            .as_str()
            .ok_or_else(|| ctx.bad_scope(format!("`{}` entries must be strings", ctx.field)))?;
        if !valid_glob(text) {
            return Err(ctx.bad_scope(format!(
                "`{}` entry `{text}` is not a workspace-relative glob",
                ctx.field
            )));
        }
        out.push(text.to_string());
    }
    Ok(out)
}
