//! The gh write-restriction gate: `CRAFT_AGENT_WRITE_REPO` declares the
//! single repository gh commands may write to. Write-class subcommands
//! must carry an explicit -R/--repo target (or a repos/<owner>/<repo>
//! API path) naming it; reads always pass, whatever repo they name. A
//! non-empty knob that is not `owner/name` denies writes outright.
//! `gh auth setup-git` is denied while the knob is set: the git
//! credential helper is host-wide, so allowing it would let plain
//! `git push` reach the token for any repository.

use crate::error::ToolError;

pub const KNOB: &str = "CRAFT_AGENT_WRITE_REPO";

const MAX_KNOB_BYTES: usize = 128;
const API_WRITE_METHODS: &[&str] = &["POST", "PUT", "PATCH", "DELETE"];
const BODY_FLAGS: &[&str] = &["-f", "--field", "-F", "--raw-field", "--input"];

/// The repository declared by the knob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteRepo {
    /// A valid lowercased `owner/name`.
    Target(String),
    /// A non-empty but malformed knob: reads pass, writes and
    /// `auth setup-git` deny.
    Malformed,
}

/// Parse the knob: None means the gate is off, Malformed means the value
/// was set but is not a bare `owner/name`.
pub fn write_repo(knob: Option<&str>) -> Option<WriteRepo> {
    let value = knob?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > MAX_KNOB_BYTES {
        return Some(WriteRepo::Malformed);
    }
    match repo_target(trimmed) {
        Some(target) => Some(WriteRepo::Target(target)),
        None => Some(WriteRepo::Malformed),
    }
}

/// A bare `owner/name` target, lowercased; anything else is not one.
fn repo_target(token: &str) -> Option<String> {
    let (owner, name) = token.split_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    if owner.chars().any(char::is_whitespace) || name.chars().any(char::is_whitespace) {
        return None;
    }
    if name.contains('/') {
        return None;
    }
    Some(format!("{}/{}", owner.to_lowercase(), name.to_lowercase()))
}

/// The argv of one gh invocation: the input the gate decides on.
pub struct Argv(Vec<String>);

enum Verb {
    Read,
    Write,
    SetupGit,
}

/// The -R/--repo handling for one argv slot: a flag consumes its value
/// (two slots), anything else gets the repos/ path scan.
enum Explicit {
    Flag(usize),
    Plain,
}

impl Argv {
    pub fn from_vec(items: Vec<String>) -> Self {
        Self(items)
    }

    pub fn into_vec(self) -> Vec<String> {
        self.0
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// The repos/<owner>/<repo> API path target, when `path` is the tail
    /// after `repos/`.
    fn repo_from_path(&self, path: &str) -> Option<String> {
        let mut parts = path.split('/');
        let owner = parts.next()?;
        let name = parts.next()?;
        if owner.is_empty() || name.is_empty() {
            return None;
        }
        Some(format!("{}/{}", owner.to_lowercase(), name.to_lowercase()))
    }

    /// The `owner/name` positional of `gh repo <verb> <owner/name>`.
    fn positional_repo_target(&self) -> Option<String> {
        if self.0.first().is_some_and(|a| a != "repo") {
            return None;
        }
        self.0.get(2).and_then(|positional| repo_target(positional))
    }

    /// Every explicit repo target in the argv, lowercased: -R/--repo
    /// values (separate, `=`, or attached) and repos/<owner>/<repo>
    /// API paths.
    fn repo_targets(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut idx = 0;
        while let Some(arg) = self.0.get(idx) {
            let consumed = match self.explicit_target(idx, &mut out) {
                Explicit::Flag(consumed) => consumed,
                Explicit::Plain => {
                    if let Some(at) = arg.find("repos/")
                        && let Some(repo) = self.repo_from_path(&arg[at + "repos/".len()..])
                    {
                        out.push(repo);
                    }
                    1
                }
            };
            idx += consumed;
        }
        out
    }

    /// The -R/--repo handling for one argv slot: a flag consumes its
    /// value (two slots), anything else gets the repos/ path scan.
    fn explicit_target(&self, idx: usize, out: &mut Vec<String>) -> Explicit {
        let Some(arg) = self.0.get(idx).map(String::as_str) else {
            return Explicit::Plain;
        };
        if arg != "-R" && arg != "--repo" {
            if let Some(value) = arg
                .strip_prefix("--repo=")
                .or_else(|| arg.strip_prefix("-R="))
            {
                out.push(value.to_lowercase());
                return Explicit::Flag(1);
            }
            return Explicit::Plain;
        }
        if let Some(value) = self.0.get(idx + 1) {
            out.push(value.to_lowercase());
        }
        Explicit::Flag(2)
    }

    /// A write needs at least one explicit target and every target naming
    /// the declared repo; a malformed knob denies writes outright.
    fn authorize_write(&self, repo: &WriteRepo) -> Result<(), ToolError> {
        let WriteRepo::Target(valid) = repo else {
            return Err(ToolError::GhWriteRepoMalformed);
        };
        let mut targets = self.repo_targets();
        if let Some(target) = self.positional_repo_target() {
            targets.push(target);
        }
        if targets.is_empty() {
            return Err(ToolError::GhWriteUntargeted);
        }
        for target in targets {
            if target != *valid {
                return Err(ToolError::GhWriteDenied { target });
            }
        }
        Ok(())
    }

    /// Classify one gh argv. Known repo-scoped families are writes
    /// except for their documented read subcommands; unrecognized
    /// subcommands inside those families default to write, and
    /// unrecognized first-level commands (browse, search, ...) stay
    /// passable.
    fn classify(&self) -> Verb {
        let Some(first) = self.0.first() else {
            return Verb::Read;
        };
        match first.as_str() {
            "pr" => self.sub(&["list", "view", "diff", "status", "checks"]),
            "issue" => self.sub(&["list", "view", "status"]),
            "repo" => self.sub(&["list", "view", "clone"]),
            "release" => self.sub(&["list", "view", "download"]),
            "secret" => self.sub(&["list"]),
            "variable" => self.sub(&["list"]),
            "label" => self.sub(&["list"]),
            "workflow" => self.sub(&["list", "view"]),
            "run" => self.sub(&["list", "view", "watch", "download"]),
            "push" => Verb::Write,
            "api" => self.api_verb(),
            "auth" if self.0.get(1).is_some_and(|s| s == "setup-git") => Verb::SetupGit,
            _ => Verb::Read,
        }
    }

    /// The read subcommand test for a repo-scoped family.
    fn sub(&self, reads: &[&str]) -> Verb {
        match self.0.get(1).map(String::as_str) {
            Some(name) if reads.contains(&name) => Verb::Read,
            _ => Verb::Write,
        }
    }

    /// `gh api` is a write when the method flag names one of the mutating
    /// methods or the argv carries request-body flags.
    fn api_verb(&self) -> Verb {
        let mut write = false;
        let mut idx = 1;
        while let Some(arg) = self.0.get(idx) {
            let consumed = self.method_arg(idx, &mut write);
            write = write || BODY_FLAGS.contains(&arg.as_str()) || arg.starts_with("--field=");
            idx += consumed;
        }
        if write { Verb::Write } else { Verb::Read }
    }

    /// How many argv slots the method flag at `idx` spans; a named write
    /// method or a valueless -X/--method flag marks the call a write.
    fn method_arg(&self, idx: usize, write: &mut bool) -> usize {
        let Some(arg) = self.0.get(idx).map(String::as_str) else {
            return 1;
        };
        let (value, consumed) = if arg == "-X" || arg == "--method" {
            match self.0.get(idx + 1) {
                Some(value) => (Some(value.as_str()), 2),
                None => {
                    *write = true;
                    (None, 1)
                }
            }
        } else {
            (
                arg.strip_prefix("--method=")
                    .or_else(|| arg.strip_prefix("-X")),
                1,
            )
        };
        if let Some(value) = value {
            *write = *write || API_WRITE_METHODS.contains(&value.to_uppercase().as_str());
        }
        consumed
    }
}

/// Gate one gh argv under the declared write repo.
pub fn check(argv: &Argv, repo: &WriteRepo) -> Result<(), ToolError> {
    match argv.classify() {
        Verb::Read => Ok(()),
        Verb::SetupGit => Err(ToolError::GhSetupGitDenied),
        Verb::Write => argv.authorize_write(repo),
    }
}

#[cfg(test)]
mod tests {
    use super::{Argv, KNOB, WriteRepo, check, write_repo};

    fn argv(items: &[&str]) -> Argv {
        Argv::from_vec(items.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn knob_parsing() {
        assert!(write_repo(None).is_none());
        assert!(write_repo(Some("")).is_none());
        assert!(write_repo(Some("  ")).is_none());
        assert_eq!(
            write_repo(Some("Acme/Widgets")),
            Some(WriteRepo::Target(String::from("acme/widgets")))
        );
        for bad in ["acme", "a/b/c", " /widgets", "owner/na me"] {
            assert_eq!(write_repo(Some(bad)), Some(WriteRepo::Malformed), "{bad}");
        }
    }

    #[test]
    fn api_path_shapes() {
        let empty = Argv::from_vec(Vec::new());
        assert_eq!(
            empty.repo_from_path("acme/widgets/pulls").as_deref(),
            Some("acme/widgets")
        );
        assert!(empty.repo_from_path("acme").is_none());
        assert!(empty.repo_from_path("").is_none());
    }

    #[test]
    fn gate_decisions() {
        let repo = WriteRepo::Target(String::from("acme/widgets"));
        assert!(check(&argv(&["pr", "view", "-R", "x/y"]), &repo).is_ok());
        assert!(check(&argv(&["pr", "create", "-R", "acme/widgets"]), &repo).is_ok());
        assert!(check(&argv(&["pr", "create", "-R", "x/y"]), &repo).is_err());
        assert!(check(&argv(&["pr", "create"]), &repo).is_err());
        assert!(check(&argv(&["auth", "setup-git"]), &repo).is_err());
        assert!(KNOB == "CRAFT_AGENT_WRITE_REPO");
    }
}
