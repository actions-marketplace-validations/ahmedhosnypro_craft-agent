//! The edit_file tool: prior-read required, exact-match replacement on
//! bytes (so binary-ish files are never mangled), changed-on-disk
//! detection via a content stamp, and a pure-CRLF view model: CRLF
//! files match on a collapsed LF view and write back CRLF, while
//! mixed-ending files are refused.

use serde_json::Value;

use crate::error::ToolError;
use crate::event::{self, ToolStream};
use crate::lineend::{self, Endings};
use crate::pathguard::{GuardedOpen, PathCtx, open_guarded, resolve_within, sensitive_reason};
use crate::schema::{Prop, Schema};
use crate::tool::{Tool, ToolCtx, ToolLimits};
use crate::util;

pub const EDIT_PROPS: &[Prop] = &[
    Prop {
        name: "path",
        ty: Schema::Str,
        required: true,
        desc: "File to edit; an absolute path must land inside the workspace root; must have been read this session first.",
    },
    Prop {
        name: "old_string",
        ty: Schema::Str,
        required: true,
        desc: "The exact text to replace. Pure CRLF files are matched on an LF view, so LF or CRLF spellings both match.",
    },
    Prop {
        name: "new_string",
        ty: Schema::Str,
        required: true,
        desc: "The replacement. Pure CRLF files are written back with CRLF, and a CR is never doubled.",
    },
    Prop {
        name: "replace_all",
        ty: Schema::Bool,
        required: false,
        desc: "Replace every occurrence instead of requiring a unique match.",
    },
];

pub struct EditFileTool {
    limits: ToolLimits,
}

impl EditFileTool {
    pub fn new(limits: ToolLimits) -> Self {
        Self { limits }
    }

    fn try_execute(&self, args: &Value, ctx: &ToolCtx) -> Result<ToolStream, ToolError> {
        let input = parse_args(args, &self.limits)?;
        let (abs, stamp) = self.resolve_target(ctx, &input.path)?;
        let bytes = read_guarded(&abs, ctx)?;
        if bytes.len() > self.limits.write_cap {
            return Err(ToolError::FileTooLarge {
                path: display(&abs),
                size: bytes.len() as u64,
                limit: self.limits.write_cap,
            });
        }
        if util::content_hash(&bytes) != stamp {
            return Err(ToolError::ChangedOnDisk {
                path: display(&abs),
            });
        }
        let style = lineend::classify(&bytes);
        if style == Endings::Mixed {
            return Err(ToolError::EditMixedEndings {
                path: display(&abs),
            });
        }
        let views = Views::of(&bytes, &input, style);
        let positions = self.locate(&abs, &views, input.replace_all)?;
        let spliced = self.checked_splice(&abs, &views, &positions)?;
        let edited = lineend::materialize(&spliced, style);
        if edited.len() > self.limits.write_cap {
            return Err(ToolError::FileTooLarge {
                path: display(&abs),
                size: edited.len() as u64,
                limit: self.limits.write_cap,
            });
        }
        write_guarded(&abs, ctx, &edited)?;
        ctx.mark_read(abs.clone(), util::content_hash(&edited));
        let note = format!(
            "replaced {} occurrence(s) in {}",
            positions.len(),
            input.path
        );
        Ok(event::result_stream(note))
    }

    /// The guarded target and its prior-read stamp; sensitive paths and
    /// files not read this session are refused.
    fn resolve_target(
        &self,
        ctx: &ToolCtx,
        input: &str,
    ) -> Result<(std::path::PathBuf, u64), ToolError> {
        let abs = resolve_within(
            &PathCtx::new(
                "edit_file",
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
        if let Some(glob) = ctx.denies_read(&abs) {
            return Err(crate::scope::denial(
                "edit_file",
                &display(&abs),
                &glob,
                "read",
            ));
        }
        if let Some(glob) = ctx.denies_write(&abs) {
            return Err(crate::scope::denial(
                "edit_file",
                &display(&abs),
                &glob,
                "write",
            ));
        }
        let Some(stamp) = ctx.read_stamp(&abs) else {
            return Err(ToolError::NotReadFirst {
                path: display(&abs),
            });
        };
        Ok((abs, stamp))
    }

    fn locate(
        &self,
        abs: &std::path::Path,
        views: &Views<'_>,
        replace_all: bool,
    ) -> Result<Vec<usize>, ToolError> {
        let positions = crate::edit_match::find_all(&views.file, &views.old);
        if positions.is_empty() {
            return Err(ToolError::EditNoMatch { path: display(abs) });
        }
        if positions.len() > 1 && !replace_all {
            return Err(ToolError::EditAmbiguous {
                path: display(abs),
                count: positions.len(),
            });
        }
        Ok(positions)
    }

    /// Splice the replacements in, rejecting a result that would exceed
    /// the write cap before anything is allocated or written.
    fn checked_splice(
        &self,
        abs: &std::path::Path,
        views: &Views<'_>,
        positions: &[usize],
    ) -> Result<Vec<u8>, ToolError> {
        let count = positions.len();
        let out_len = views
            .file
            .len()
            .saturating_sub(count.saturating_mul(views.old.len()))
            .saturating_add(count.saturating_mul(views.new.len()));
        if out_len > self.limits.write_cap {
            return Err(ToolError::FileTooLarge {
                path: display(abs),
                size: out_len as u64,
                limit: self.limits.write_cap,
            });
        }
        Ok(crate::edit_match::splice(
            &views.file,
            &views.old,
            &views.new,
            positions,
        ))
    }
}

struct EditArgs {
    path: String,
    old: String,
    new: String,
    replace_all: bool,
}

fn invalid(msg: &str) -> ToolError {
    ToolError::InvalidArgs {
        tool: "edit_file".to_string(),
        msg: msg.to_string(),
    }
}

fn parse_args(args: &Value, limits: &ToolLimits) -> Result<EditArgs, ToolError> {
    let args = util::ToolArgs::new(args, "edit_file");
    args.reject_unknown_keys(&["path", "old_string", "new_string", "replace_all"])?;
    let path = args.arg_str_bounded("path", limits.max_path_bytes)?;
    let old = args.arg_str("old_string")?;
    let new = args.arg_str("new_string")?;
    let replace_all = args.arg_opt_bool("replace_all")?.unwrap_or(false);
    if old.is_empty() {
        return Err(invalid("old_string must not be empty"));
    }
    if old == new {
        return Err(invalid("old_string and new_string are exactly the same"));
    }
    Ok(EditArgs {
        path,
        old,
        new,
        replace_all,
    })
}

/// The line-ending views of the file and both edit strings.
struct Views<'a> {
    file: std::borrow::Cow<'a, [u8]>,
    old: std::borrow::Cow<'a, [u8]>,
    new: std::borrow::Cow<'a, [u8]>,
}

impl<'a> Views<'a> {
    fn of(bytes: &'a [u8], input: &'a EditArgs, style: Endings) -> Self {
        Self {
            file: lineend::view(bytes, style),
            old: lineend::view(input.old.as_bytes(), style),
            new: lineend::view(input.new.as_bytes(), style),
        }
    }
}

fn display(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

// Guarded I/O: the path was guard-approved; the guarded open resolves
// symlinks and opens in one pass, so a post-guard swap is refused.
fn read_guarded(abs: &std::path::Path, ctx: &ToolCtx) -> Result<Vec<u8>, ToolError> {
    let roots = [ctx.workspace_root().to_path_buf()];
    let mut file = open_guarded(abs, &roots, GuardedOpen::Read, "read")?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).map_err(|e| ToolError::Io {
        op: "read",
        msg: e.to_string(),
    })?;
    Ok(bytes)
}

fn write_guarded(abs: &std::path::Path, ctx: &ToolCtx, bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Write as _;
    let roots = [ctx.workspace_root().to_path_buf()];
    let mut file = open_guarded(abs, &roots, GuardedOpen::Truncate, "write")?;
    file.write_all(bytes).map_err(|e| ToolError::Io {
        op: "write",
        msg: e.to_string(),
    })
}

impl Tool for EditFileTool {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Replaces an exact match in a previously read file; pure CRLF files match on an LF view and write back CRLF; files with mixed line endings are refused; aborts if the file changed on disk."
    }

    fn params_schema(&self) -> Schema {
        Schema::object("Applies one exact-match edit.", EDIT_PROPS)
    }

    fn execute(&self, args: Value, ctx: &ToolCtx) -> ToolStream {
        match self.try_execute(&args, ctx) {
            Ok(stream) => stream,
            Err(err) => event::error_stream(err),
        }
    }
}
