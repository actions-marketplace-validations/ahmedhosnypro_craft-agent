//! The plan file: bounded bytes to validated frontmatter. The reader is
//! the only place a plan touches the filesystem; the frontmatter shape
//! checks name and description before the step graph is built.

use std::io::Read;
use std::path::Path;

use crate::error::WorkflowError;
use crate::plan::{MAX_FILE_BYTES, WorkflowPlan};
use crate::yaml::YValue;

const FRONTMATTER_FIELDS: &[&str] = &["name", "description", "args", "limits", "steps"];

impl WorkflowPlan {
    /// Reads one plan file fail-closed: bounded bytes, valid UTF-8, then
    /// the text parse.
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
}

/// The frontmatter root with its file name, for error context.
pub(crate) struct Frontmatter<'a> {
    pub(crate) file: &'a str,
    pub(crate) root: &'a YValue,
}

impl Frontmatter<'_> {
    /// The root must be a map holding only known fields.
    pub(crate) fn check_fields(&self) -> Result<(), WorkflowError> {
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

    pub(crate) fn name(&self) -> Result<String, WorkflowError> {
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
        if !crate::plan::valid_key(name) {
            return Err(WorkflowError::Parse {
                file: self.file.to_string(),
                detail: format!(
                    "`name` `{name}` is not 1..={} chars of a-z, 0-9, dot, dash, underscore",
                    crate::plan::MAX_NAME_CHARS
                ),
            });
        }
        Ok(name.to_string())
    }

    pub(crate) fn description(&self) -> Result<Option<String>, WorkflowError> {
        let Some(value) = self.root.get("description") else {
            return Ok(None);
        };
        let text = value.as_str().ok_or_else(|| WorkflowError::Parse {
            file: self.file.to_string(),
            detail: "`description` must be a string".to_string(),
        })?;
        if text.chars().count() > crate::plan::MAX_DESCRIPTION_CHARS {
            return Err(WorkflowError::Parse {
                file: self.file.to_string(),
                detail: format!(
                    "`description` exceeds {} chars",
                    crate::plan::MAX_DESCRIPTION_CHARS
                ),
            });
        }
        Ok(Some(text.to_string()))
    }
}

/// Splits the text after the opening fence into frontmatter and body at
/// the next bare `---` line.
pub(crate) fn split_frontmatter(rest: &str) -> Result<(&str, &str), String> {
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
