//! Workflow plan location: the flags, the env knobs, and the
//! discovery-dir rules that turn a CLI selection into one plan file.

use std::path::{Path, PathBuf};

use craft_workflow::WorkflowPlan;

use crate::cli::WorkflowCli;
use crate::config::EnvMap;
use crate::error::AppError;

/// Project-scope workflows directory, relative to any directory from
/// the start up to the git root.
pub const WORKFLOWS_DIR: &str = ".craft-agent/workflows";
/// Levels walked up from the start directory, inclusive.
const DISCOVERY_DEPTH: usize = 8;
pub const WORKFLOW_FILE_KNOB: &str = "CRAFT_AGENT_WORKFLOW_FILE";
pub const WORKFLOWS_DIR_KNOB: &str = "CRAFT_AGENT_WORKFLOWS_DIR";

/// The workflow plan's path: the explicit file flag, else the named
/// plan in the workflows directory, else the env-knob file.
pub fn locate_plan(
    cli: &WorkflowCli,
    env: &EnvMap,
    workspace_root: &Path,
) -> Result<PathBuf, AppError> {
    if let Some(path) = &cli.workflow_file {
        return Ok(path.clone());
    }
    if let Some(name) = &cli.workflow {
        let dir = workflows_dir(env, workspace_root)?;
        return find_named(&dir, name);
    }
    let Some(raw) = env
        .get(WORKFLOW_FILE_KNOB)
        .filter(|value| !value.is_empty())
    else {
        return Err(AppError::WorkflowNotSelected);
    };
    let path = PathBuf::from(raw);
    if !path.is_file() {
        return Err(AppError::WorkflowEnvFileMissing { path });
    }
    Ok(path)
}

/// The directory `--workflow <name>` resolves in: the env knob when
/// set, else the closest project-scope directory walking up.
fn workflows_dir(env: &EnvMap, workspace_root: &Path) -> Result<PathBuf, AppError> {
    if let Some(raw) = env
        .get(WORKFLOWS_DIR_KNOB)
        .filter(|value| !value.is_empty())
    {
        let dir = PathBuf::from(raw);
        if !dir.is_dir() {
            return Err(AppError::WorkflowDirEnvMissing { dir });
        }
        return Ok(dir);
    }
    discover_workflows_dir(workspace_root).ok_or_else(|| AppError::WorkflowDirMissing {
        dir: workspace_root.join(WORKFLOWS_DIR),
    })
}

/// The closest workflows directory from `start` walking up, at most
/// [`DISCOVERY_DEPTH`] levels; the git root is the last candidate.
fn discover_workflows_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    for _ in 0..DISCOVERY_DEPTH {
        let candidate = dir.join(WORKFLOWS_DIR);
        if candidate.is_dir() {
            return Some(candidate);
        }
        if dir.join(".git").exists() {
            return None;
        }
        if !dir.pop() {
            return None;
        }
    }
    None
}

/// Looks a plan up by its frontmatter `name`; invalid files are noted
/// and skipped, fail-soft, so one bad file never hides the rest.
fn find_named(dir: &Path, name: &str) -> Result<PathBuf, AppError> {
    let shown = dir.to_path_buf();
    let reader = std::fs::read_dir(dir).map_err(|source| AppError::WorkflowDirRead {
        dir: shown.clone(),
        source,
    })?;
    let mut files = Vec::new();
    for entry in reader {
        let entry = entry.map_err(|source| AppError::WorkflowDirRead {
            dir: shown.clone(),
            source,
        })?;
        let path = entry.path();
        let is_file = entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false);
        if is_file && path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    files.sort();
    let mut names = Vec::new();
    for path in &files {
        match WorkflowPlan::parse_file(path) {
            Ok(parsed) => {
                names.push(parsed.name.clone());
                if parsed.name == name {
                    return Ok(path.clone());
                }
            }
            Err(err) => eprintln!(
                "craft-agent: skipped workflow file `{}`: {err}",
                path.display()
            ),
        }
    }
    Err(AppError::WorkflowUnknown {
        name: name.to_string(),
        dir: shown,
        available: listing(names.iter().cloned()),
    })
}

pub fn listing(names: impl IntoIterator<Item = String>) -> String {
    let names: Vec<String> = names.into_iter().collect();
    if names.is_empty() {
        String::from("(none found)")
    } else {
        names.join(", ")
    }
}
