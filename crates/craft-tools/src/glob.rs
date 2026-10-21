//! The glob tool: bounded, sorted file listing via globset over the
//! deterministic walker.

use globset;
use serde_json::Value;

use crate::error::ToolError;
use crate::event::{self, ToolStream};
use crate::pathguard::{PathCtx, resolve_read, sensitive_reason};
use crate::schema::{Prop, Schema};
use crate::tool::{Tool, ToolCtx, ToolLimits};
use crate::util;

pub const GLOB_PROPS: &[Prop] = &[
    Prop {
        name: "pattern",
        ty: Schema::Str,
        required: true,
        desc: "Glob pattern matched against paths relative to the search root (`*`, `**`, `?`).",
    },
    Prop {
        name: "path",
        ty: Schema::Str,
        required: false,
        desc: "Directory to search instead of the workspace root; an absolute configured read root also works. Workspace results are root-relative; read-root results are absolute.",
    },
];

pub struct GlobTool {
    limits: ToolLimits,
}

impl GlobTool {
    pub fn new(limits: ToolLimits) -> Self {
        Self { limits }
    }

    fn resolve_scope(&self, ctx: &ToolCtx, input: &str) -> Result<std::path::PathBuf, ToolError> {
        let scope = resolve_read(
            &PathCtx::new(
                "glob",
                ctx.workspace_root(),
                ctx.read_roots(),
                self.limits.max_path_bytes,
            ),
            input,
        )?;
        if let Some(reason) = sensitive_reason(&scope) {
            return Err(ToolError::SensitivePath {
                path: display(&scope),
                reason,
            });
        }
        Ok(scope)
    }

    fn try_execute(&self, args: &Value, ctx: &ToolCtx) -> Result<ToolStream, ToolError> {
        let input = parse_args(args, &self.limits)?;
        let scope = self.resolve_scope(ctx, &input.scope)?;
        let matcher = globset::GlobBuilder::new(&input.pattern)
            .literal_separator(true)
            .build()
            .map_err(|e| ToolError::InvalidArgs {
                tool: "glob".to_string(),
                msg: e.to_string(),
            })?
            .compile_matcher();

        let target = Target {
            scope,
            root: ctx.workspace_root().to_path_buf(),
        };
        let (mut hits, outcome) = self.collect(ctx, &target, &matcher)?;
        hits.sort_unstable();
        Ok(event::result_stream(render(
            &hits,
            &outcome,
            self.limits.glob_max_results,
        )))
    }

    /// The matches under `scope`: a lone file is tested against the
    /// pattern itself, a directory is walked. Denied paths are never
    /// visited.
    fn collect(
        &self,
        ctx: &ToolCtx,
        target: &Target,
        matcher: &globset::GlobMatcher,
    ) -> Result<(Vec<String>, crate::walk::WalkOutcome), ToolError> {
        let scope = &target.scope;
        let scope_meta = std::fs::metadata(scope).map_err(|_| ToolError::NotFound {
            path: display(scope),
        })?;
        let mut hits: Vec<String> = Vec::new();
        if scope_meta.is_file() {
            let outcome = crate::walk::WalkOutcome::default();
            if let Some(glob) = ctx.denies_read(scope) {
                return Err(crate::scope::denial("glob", &display(scope), &glob, "read"));
            }
            let base = scope.parent().unwrap_or(scope);
            if let Some(rel) = rel_to(scope, base)
                && matcher.is_match(&rel)
            {
                hits.push(display_rel(scope, &target.root));
            }
            return Ok((hits, outcome));
        }
        let limits = self.limits;
        let admit = |path: &std::path::Path| ctx.denies_read(path).is_none();
        let outcome = crate::walk::walk_filtered(
            scope,
            limits.walk_max_entries,
            limits.walk_max_depth,
            &mut |path| {
                if let Some(rel) = rel_to(&path, scope)
                    && matcher.is_match(&rel)
                {
                    hits.push(display_rel(&path, &target.root));
                }
                hits.len() < limits.glob_max_results
            },
            &admit,
        )?;
        Ok((hits, outcome))
    }
}

/// The searched directory and the workspace its results are reported against.
struct Target {
    scope: std::path::PathBuf,
    root: std::path::PathBuf,
}

struct GlobArgs {
    pattern: String,
    scope: String,
}

fn parse_args(args: &Value, limits: &ToolLimits) -> Result<GlobArgs, ToolError> {
    let args = util::ToolArgs::new(args, "glob");
    args.reject_unknown_keys(&["pattern", "path"])?;
    let pattern = args.arg_str_bounded("pattern", limits.max_pattern_bytes)?;
    if pattern.is_empty() {
        return Err(ToolError::InvalidArgs {
            tool: "glob".to_string(),
            msg: "pattern must not be empty".to_string(),
        });
    }
    let scope = args
        .arg_opt_str("path", limits.max_path_bytes)?
        .unwrap_or_else(|| ".".to_string());
    Ok(GlobArgs { pattern, scope })
}

fn render(hits: &[String], outcome: &crate::walk::WalkOutcome, max_results: usize) -> String {
    let mut out = hits.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    if outcome.skipped_sensitive > 0 {
        out.push_str(&format!(
            "[skipped {} sensitive files]\n",
            outcome.skipped_sensitive
        ));
    }
    if hits.len() >= max_results {
        out.push_str(&format!("[results capped at {max_results}]\n"));
    }
    out
}

fn rel_to(path: &std::path::Path, base: &std::path::Path) -> Option<String> {
    path.strip_prefix(base)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

fn display_rel(path: &std::path::Path, root: &std::path::Path) -> String {
    rel_to(path, root).unwrap_or_else(|| display(path))
}

fn display(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }

    fn description(&self) -> &'static str {
        "Lists workspace files matching a glob pattern, sorted, with a bounded result count; a configured read root is searchable too."
    }

    fn params_schema(&self) -> Schema {
        Schema::object("Lists files by glob.", GLOB_PROPS)
    }

    fn execute(&self, args: Value, ctx: &ToolCtx) -> ToolStream {
        match self.try_execute(&args, ctx) {
            Ok(stream) => stream,
            Err(err) => event::error_stream(err),
        }
    }
}
