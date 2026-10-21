//! The grep tool: Rust-native content search — the regex crate over the
//! bounded deterministic walker. No external search binary is invoked.
//! The per-file search and the rendered rows live in `grep_report`.

use globset;
use regex::Regex;
use serde_json::Value;

use crate::error::ToolError;
use crate::event::{self, ToolStream};
use crate::pathguard::{PathCtx, resolve_read, sensitive_reason};
use crate::schema::{Prop, Schema};
use crate::tool::{Tool, ToolCtx, ToolLimits};
use crate::util;

pub const GREP_PROPS: &[Prop] = &[
    Prop {
        name: "pattern",
        ty: Schema::Str,
        required: true,
        desc: "Text to find; a regex unless `literal` is true.",
    },
    Prop {
        name: "path",
        ty: Schema::Str,
        required: false,
        desc: "File or directory to search instead of the workspace root; an absolute configured read root also works.",
    },
    Prop {
        name: "glob",
        ty: Schema::Str,
        required: false,
        desc: "Only files matching this glob pattern are searched.",
    },
    Prop {
        name: "literal",
        ty: Schema::Bool,
        required: false,
        desc: "Treat the pattern as literal text instead of a regex.",
    },
];

pub struct GrepTool {
    limits: ToolLimits,
}
impl GrepTool {
    pub fn new(limits: ToolLimits) -> Self {
        Self { limits }
    }

    fn resolve_scope(&self, ctx: &ToolCtx, input: &str) -> Result<std::path::PathBuf, ToolError> {
        let scope = resolve_read(
            &PathCtx::new(
                "grep",
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
        let pattern = parse_pattern(args, &self.limits)?;
        let args = util::ToolArgs::new(args, "grep");
        let filter = build_filter(
            args.arg_opt_str("glob", self.limits.max_pattern_bytes)?
                .as_deref(),
        )?;
        let scope_input = args
            .arg_opt_str("path", self.limits.max_path_bytes)?
            .unwrap_or_else(|| ".".to_string());
        let root = ctx.workspace_root().to_path_buf();
        let scope = self.resolve_scope(ctx, &scope_input)?;
        let mut report = crate::grep_report::Report::new(self.limits);
        let meta = std::fs::metadata(&scope).map_err(|_| ToolError::NotFound {
            path: display(&scope),
        })?;
        let needle = crate::grep_report::Needle {
            matcher: &pattern.matcher,
            filter: &filter,
        };
        if meta.is_file() {
            if let Some(glob) = ctx.denies_read(&scope) {
                return Err(crate::scope::denial(
                    "grep",
                    &display(&scope),
                    &glob,
                    "read",
                ));
            }
            report.search_file(
                &scope,
                &crate::grep_report::Paths {
                    root: &root,
                    base: scope.parent().unwrap_or(&scope),
                },
                &needle,
            );
        } else {
            let admit = |path: &std::path::Path| ctx.denies_read(path).is_none();
            let outcome = crate::walk::walk_filtered(
                &scope,
                self.limits.walk_max_entries,
                self.limits.walk_max_depth,
                &mut |path| {
                    report.search_file(
                        &path,
                        &crate::grep_report::Paths {
                            root: &root,
                            base: &scope,
                        },
                        &needle,
                    );
                    !report.capped()
                },
                &admit,
            )?;
            report.skipped_sensitive = outcome.skipped_sensitive;
        }
        Ok(event::result_stream(report.render(&pattern.raw)))
    }
}

/// The raw pattern and its compiled matcher.
struct Pattern {
    raw: String,
    matcher: Regex,
}

fn parse_pattern(args: &Value, limits: &ToolLimits) -> Result<Pattern, ToolError> {
    let args = util::ToolArgs::new(args, "grep");
    args.reject_unknown_keys(&["pattern", "path", "glob", "literal"])?;
    let raw = args.arg_str_bounded("pattern", limits.max_pattern_bytes)?;
    if raw.is_empty() {
        return Err(ToolError::InvalidArgs {
            tool: "grep".to_string(),
            msg: "pattern must not be empty".to_string(),
        });
    }
    let literal = args.arg_opt_bool("literal")?.unwrap_or(false);
    let needle = if literal {
        regex::escape(&raw)
    } else {
        raw.clone()
    };
    let matcher = Regex::new(&needle).map_err(|e| ToolError::BadPattern {
        msg: util::truncate_char_boundary(&e.to_string(), 512).to_string(),
    })?;
    Ok(Pattern { raw, matcher })
}

fn build_filter(raw: Option<&str>) -> Result<Option<globset::GlobMatcher>, ToolError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let glob = globset::GlobBuilder::new(raw)
        .literal_separator(true)
        .build()
        .map_err(|e| ToolError::InvalidArgs {
            tool: "grep".to_string(),
            msg: e.to_string(),
        })?;
    Ok(Some(glob.compile_matcher()))
}

fn display(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Searches file contents with a regex or literal pattern across the workspace and any configured read roots, bounded and sorted."
    }

    fn params_schema(&self) -> Schema {
        Schema::object("Searches file contents.", GREP_PROPS)
    }

    fn execute(&self, args: Value, ctx: &ToolCtx) -> ToolStream {
        match self.try_execute(&args, ctx) {
            Ok(stream) => stream,
            Err(err) => event::error_stream(err),
        }
    }
}
