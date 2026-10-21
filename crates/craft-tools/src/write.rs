//! The write_file tool: guarded writes with parent-directory creation.
//! A write does not satisfy edit_file's prior-read rule — only a read
//! does.

use serde_json::Value;

use crate::error::ToolError;
use crate::event::{self, ToolStream};
use crate::pathguard::{GuardedOpen, PathCtx, open_guarded, resolve_within, sensitive_reason};
use crate::schema::{Prop, Schema};
use crate::tool::{Tool, ToolCtx, ToolLimits};
use crate::util;

pub const WRITE_PROPS: &[Prop] = &[
    Prop {
        name: "path",
        ty: Schema::Str,
        required: true,
        desc: "Target file path inside the workspace; an absolute path must land inside the workspace root; missing parents are created.",
    },
    Prop {
        name: "content",
        ty: Schema::Str,
        required: true,
        desc: "The full file content to write, or the bytes to append in append mode.",
    },
    Prop {
        name: "mode",
        ty: Schema::Str,
        required: false,
        desc: "\"overwrite\" (default) replaces the file; \"append\" adds the bytes at EOF without a trailing newline.",
    },
];

pub struct WriteFileTool {
    limits: ToolLimits,
}

impl WriteFileTool {
    pub fn new(limits: ToolLimits) -> Self {
        Self { limits }
    }

    /// Resolve the target, refusing a sensitive path or a scope-denied one.
    fn resolve_target(&self, ctx: &ToolCtx, input: &str) -> Result<std::path::PathBuf, ToolError> {
        let abs = resolve_within(
            &PathCtx::new(
                "write_file",
                ctx.workspace_root(),
                &[],
                self.limits.max_path_bytes,
            ),
            input,
        )?;
        if let Some(reason) = sensitive_reason(&abs) {
            return Err(ToolError::SensitivePath {
                path: display(&abs),
                reason,
            });
        }
        if let Some(glob) = ctx.denies_write(&abs) {
            return Err(crate::scope::denial(
                "write_file",
                &display(&abs),
                &glob,
                "write",
            ));
        }
        Ok(abs)
    }

    fn try_execute(&self, args: &Value, ctx: &ToolCtx) -> Result<ToolStream, ToolError> {
        let input = parse_args(args, &self.limits)?;
        let abs = self.resolve_target(ctx, &input.path)?;
        guard_parent(&abs, ctx)?;
        let note = if input.append {
            write_bytes(&abs, ctx, input.content.as_bytes(), &open_plan(true))?;
            format!("appended {} bytes to {}", input.content.len(), input.path)
        } else {
            write_bytes(&abs, ctx, input.content.as_bytes(), &open_plan(false))?;
            format!("wrote {} bytes to {}", input.content.len(), input.path)
        };
        Ok(event::result_stream(note))
    }
}

struct WriteArgs {
    path: String,
    content: String,
    append: bool,
}

fn parse_args(args: &Value, limits: &ToolLimits) -> Result<WriteArgs, ToolError> {
    let args = util::ToolArgs::new(args, "write_file");
    args.reject_unknown_keys(&["path", "content", "mode"])?;
    let path = args.arg_str_bounded("path", limits.max_path_bytes)?;
    let content = args.arg_str("content")?;
    let append = match args.arg_opt_str("mode", 32)?.as_deref() {
        None => false,
        Some("overwrite") => false,
        Some("append") => true,
        Some(_) => {
            return Err(ToolError::InvalidArgs {
                tool: "write_file".to_string(),
                msg: "mode must be \"overwrite\" or \"append\"".to_string(),
            });
        }
    };
    if content.len() > limits.write_cap {
        return Err(ToolError::WriteTooLarge {
            size: content.len(),
            limit: limits.write_cap,
        });
    }
    Ok(WriteArgs {
        path,
        content,
        append,
    })
}

/// Create the missing parents, then refuse a target whose canonical
/// parent escapes the workspace root.
fn guard_parent(abs: &std::path::Path, ctx: &ToolCtx) -> Result<(), ToolError> {
    let Some(parent) = abs.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|e| ToolError::Io {
        op: "create directories",
        msg: e.to_string(),
    })?;
    let canon = std::fs::canonicalize(parent).map_err(|e| ToolError::Io {
        op: "create directories",
        msg: e.to_string(),
    })?;
    let canon_root = std::fs::canonicalize(ctx.workspace_root())
        .unwrap_or_else(|_| ctx.workspace_root().to_path_buf());
    if !canon.starts_with(&canon_root) {
        return Err(ToolError::EscapesWorkspace {
            path: display(abs),
            allowed: display(ctx.workspace_root()),
        });
    }
    Ok(())
}

/// The guarded-open mode and the op labels for one write flavor.
struct OpenPlan {
    mode: GuardedOpen,
    op_open: &'static str,
    op_write: &'static str,
}

fn open_plan(append: bool) -> OpenPlan {
    if append {
        OpenPlan {
            mode: GuardedOpen::Append,
            op_open: "open for append",
            op_write: "append",
        }
    } else {
        OpenPlan {
            mode: GuardedOpen::Truncate,
            op_open: "write",
            op_write: "write",
        }
    }
}

/// Write `bytes` exactly: never reads the existing content, never adds
/// a newline, creates a missing file. The open is guarded, so a
/// symlink swapped in after the path-guard walk is refused.
fn write_bytes(
    abs: &std::path::Path,
    ctx: &ToolCtx,
    bytes: &[u8],
    plan: &OpenPlan,
) -> Result<(), ToolError> {
    use std::io::Write as _;
    let roots = [ctx.workspace_root().to_path_buf()];
    let mut file = open_guarded(abs, &roots, plan.mode, plan.op_open)?;
    file.write_all(bytes).map_err(|e| ToolError::Io {
        op: plan.op_write,
        msg: e.to_string(),
    })
}

fn display(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Writes a full file inside the workspace; missing parent directories are created. mode=append adds the bytes at EOF without a trailing newline."
    }

    fn params_schema(&self) -> Schema {
        Schema::object("Writes one file.", WRITE_PROPS)
    }

    fn execute(&self, args: Value, ctx: &ToolCtx) -> ToolStream {
        match self.try_execute(&args, ctx) {
            Ok(stream) => stream,
            Err(err) => event::error_stream(err),
        }
    }
}
