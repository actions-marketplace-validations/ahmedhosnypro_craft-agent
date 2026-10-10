//! The grep tool: Rust-native content search — the regex crate over the
//! bounded deterministic walker. No external search binary is invoked.

use globset;
use regex::Regex;
use serde_json::Value;

use crate::error::ToolError;
use crate::event::{self, ToolStream};
use crate::pathguard::{PathCtx, resolve_read, sensitive_reason};
use crate::schema::{Prop, Schema};
use crate::tool::{Tool, ToolCtx, ToolLimits};
use crate::util;
use crate::walk::walk_files;

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

const BINARY_PROBE: usize = 8 * 1024;

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
        let mut report = Report::new(self.limits);
        let meta = std::fs::metadata(&scope).map_err(|_| ToolError::NotFound {
            path: display(&scope),
        })?;
        let needle = Needle {
            matcher: &pattern.matcher,
            filter: &filter,
        };
        if meta.is_file() {
            let base = scope.parent().unwrap_or(&scope);
            report.search_file(&scope, &Paths { root: &root, base }, &needle);
        } else {
            let outcome = walk_files(
                &scope,
                self.limits.walk_max_entries,
                self.limits.walk_max_depth,
                &mut |path| {
                    report.search_file(
                        &path,
                        &Paths {
                            root: &root,
                            base: &scope,
                        },
                        &needle,
                    );
                    !report.capped()
                },
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

/// The search context of one file: `root` prefixes display paths, and
/// `base` is the directory its filter-relative path is taken against.
struct Paths<'a> {
    root: &'a std::path::Path,
    base: &'a std::path::Path,
}

struct Needle<'a> {
    matcher: &'a Regex,
    filter: &'a Option<globset::GlobMatcher>,
}

struct Report {
    rows: Vec<(String, usize, String)>,
    limits: ToolLimits,
    skipped_sensitive: usize,
    skipped_oversize: usize,
    skipped_binary: usize,
    skipped_unreadable: usize,
    capped: bool,
}

impl Report {
    fn new(limits: ToolLimits) -> Self {
        Self {
            rows: Vec::new(),
            limits,
            skipped_sensitive: 0,
            skipped_oversize: 0,
            skipped_binary: 0,
            skipped_unreadable: 0,
            capped: false,
        }
    }

    fn capped(&self) -> bool {
        self.capped
    }

    /// Search one file: oversize, binary, and unreadable files are
    /// skipped and counted; matching lines become `path:line: excerpt` rows.
    fn search_file(&mut self, path: &std::path::Path, paths: &Paths<'_>, needle: &Needle<'_>) {
        if !self.passes_filter(path, paths.base, needle.filter) {
            return;
        }
        let Some(text) = self.read_text(path) else {
            return;
        };
        let display = display_path(path, paths.root);
        self.append_matches(needle.matcher, &text, &display);
    }

    fn passes_filter(
        &self,
        path: &std::path::Path,
        base: &std::path::Path,
        filter: &Option<globset::GlobMatcher>,
    ) -> bool {
        let Some(f) = filter else {
            return true;
        };
        match path.strip_prefix(base) {
            Ok(rel) => f.is_match(rel),
            Err(_) => false,
        }
    }

    /// The file's text when it is searchable; None counts the skip reason.
    fn read_text(&mut self, path: &std::path::Path) -> Option<String> {
        let Ok(meta) = std::fs::metadata(path) else {
            self.skipped_unreadable = self.skipped_unreadable.saturating_add(1);
            return None;
        };
        if !meta.is_file() || meta.len() > self.limits.grep_file_cap as u64 {
            self.skipped_oversize = self.skipped_oversize.saturating_add(1);
            return None;
        }
        let Ok(bytes) = std::fs::read(path) else {
            self.skipped_unreadable = self.skipped_unreadable.saturating_add(1);
            return None;
        };
        if !self.is_plaintext(&bytes) {
            self.skipped_binary = self.skipped_binary.saturating_add(1);
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn is_plaintext(&self, bytes: &[u8]) -> bool {
        bytes.len() <= self.limits.grep_file_cap
            && !bytes[..bytes.len().min(BINARY_PROBE)].contains(&0)
    }

    fn append_matches(&mut self, matcher: &Regex, text: &str, display: &str) {
        for (idx, line) in text.split_inclusive('\n').enumerate() {
            let body = line.strip_suffix('\n').unwrap_or(line);
            let Some(found) = matcher.find(body) else {
                continue;
            };
            let start = found.start().min(body.len());
            let tail = util::truncate_char_boundary(&body[start..], self.limits.grep_line_cap);
            self.rows
                .push((display.to_string(), idx + 1, tail.to_string()));
            if self.rows.len() >= self.limits.grep_max_results {
                self.capped = true;
                return;
            }
        }
    }

    fn render(&mut self, pattern: &str) -> String {
        self.rows.sort_unstable();
        let mut out = String::new();
        for (path, line, tail) in &self.rows {
            out.push_str(&format!("{path}:{line}: {tail}\n"));
        }
        let skips = [
            ("oversized", self.skipped_oversize),
            ("binary", self.skipped_binary),
            ("unreadable", self.skipped_unreadable),
            ("sensitive", self.skipped_sensitive),
        ];
        for (kind, count) in skips {
            if count > 0 {
                out.push_str(&format!("[skipped {count} {kind} files]\n"));
            }
        }
        if self.capped {
            out.push_str(&format!(
                "[results capped at {}]\n",
                self.limits.grep_max_results
            ));
        }
        if self.rows.is_empty() {
            out.push_str(&format!("no matches for `{pattern}`\n"));
        }
        out
    }
}

fn display(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

fn display_path(path: &std::path::Path, root: &std::path::Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
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
