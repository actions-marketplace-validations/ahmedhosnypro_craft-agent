//! Shallow clones of the suite repositories into a scratch dir, run
//! directly via std::process::Command; the program is injectable so the
//! corpora drive fixture scripts.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::bundle::Bundle;
use crate::error::{DigestError, bounded};

/// Wall-clock bound for one shallow clone; expiry becomes an error row.
pub const CLONE_DEADLINE: Duration = Duration::from_secs(600);
const WAIT_POLL: Duration = Duration::from_millis(50);
/// Per-pipe byte cap on captured child output.
const CHILD_OUTPUT_CAP_BYTES: u64 = 1024 * 1024;

pub struct CloneRunner {
    program: PathBuf,
    deadline: Duration,
}

impl Default for CloneRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl CloneRunner {
    pub fn new() -> Self {
        CloneRunner {
            program: PathBuf::from("git"),
            deadline: CLONE_DEADLINE,
        }
    }

    /// Tests point this at a fake git script.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        CloneRunner {
            program: program.into(),
            deadline: CLONE_DEADLINE,
        }
    }

    /// Tests shorten the bound to exercise expiry.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }
}

/// The inputs a clone run shares.
pub struct CloneCtx<'a> {
    pub runner: &'a CloneRunner,
    pub org: &'a str,
    pub repos: &'a [String],
    pub scratch_root: &'a Path,
}

/// Shallow-clone every repo under `<scratch_root>/<repo>`; a failing repo
/// becomes an error row and the run continues.
pub fn clone_all(ctx: &CloneCtx<'_>, bundle: &mut Bundle) -> Result<(), DigestError> {
    fs::create_dir_all(ctx.scratch_root)
        .map_err(|err| DigestError::Io(bounded(&err.to_string())))?;
    for repo in ctx.repos {
        clone_repo(ctx, repo, bundle);
    }
    Ok(())
}

fn clone_repo(ctx: &CloneCtx<'_>, repo: &str, bundle: &mut Bundle) {
    let dir = ctx.scratch_root.join(file_slug(repo));
    let url = format!("https://github.com/{}/{repo}.git", ctx.org);
    let mut command = Command::new(&ctx.runner.program);
    command
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg(&url)
        .arg(&dir);
    match run_with_deadline(&mut command, ctx.runner.deadline) {
        Ok(BoundedRun::Completed { status, .. }) if status.success() => {}
        Ok(BoundedRun::Completed { stderr, .. }) => {
            let stderr = String::from_utf8_lossy(&stderr);
            bundle.push_error(repo, "clone", &stderr);
        }
        Ok(BoundedRun::Expired) => {
            let message = format!("clone exceeded its {:?} deadline", ctx.runner.deadline);
            bundle.push_error(repo, "clone", &message);
        }
        Err(err) => bundle.push_error(repo, "clone", &err.to_string()),
    }
}

/// A repo name is a file-name component, never a path.
pub(crate) fn file_slug(repo: &str) -> String {
    let slug: String = repo
        .chars()
        .map(|c| if is_slug_char(c) { c } else { '-' })
        .collect();
    if slug != repo {
        format!("repo-{slug}")
    } else {
        slug
    }
}

fn is_slug_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// A child run to completion, or killed at its deadline.
pub(crate) enum BoundedRun {
    Completed {
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    Expired,
}

/// Run a child to completion or kill it at `deadline`; the pipes are
/// drained concurrently so a full output page cannot false-expire the wait.
pub(crate) fn run_with_deadline(
    command: &mut Command,
    deadline: Duration,
) -> std::io::Result<BoundedRun> {
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let readers = drain(&mut child);
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (stdout, stderr) = join_readers(readers);
                return Ok(BoundedRun::Completed {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) if start.elapsed() < deadline => std::thread::sleep(WAIT_POLL),
            Ok(None) => {
                kill_and_reap(&mut child);
                return Ok(BoundedRun::Expired);
            }
            Err(err) => {
                kill_and_reap(&mut child);
                return Err(err);
            }
        }
    }
}

fn drain(child: &mut Child) -> (JoinHandle<Vec<u8>>, JoinHandle<Vec<u8>>) {
    (
        spawn_reader(child.stdout.take()),
        spawn_reader(child.stderr.take()),
    )
}

fn spawn_reader<R: Read + Send + 'static>(handle: Option<R>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(handle) = handle {
            let mut limited = handle.take(CHILD_OUTPUT_CAP_BYTES);
            let _ = limited.read_to_end(&mut buffer);
        }
        buffer
    })
}

fn join_readers(readers: (JoinHandle<Vec<u8>>, JoinHandle<Vec<u8>>)) -> (Vec<u8>, Vec<u8>) {
    let (out, err) = readers;
    (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    )
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A fake program that writes 4 MiB to stderr then exits; the captured
    /// stderr must be capped, not grown to the whole output.
    #[test]
    fn child_output_reader_is_capped() {
        let dir =
            std::env::temp_dir().join(format!("craft-digest-output-cap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("spew");
        {
            let mut f = fs::File::create(&script).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, "yes | head -c 4194304 1>&2").unwrap();
        }
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let mut command = Command::new(&script);
        let run = run_with_deadline(&mut command, Duration::from_secs(30)).unwrap();
        let BoundedRun::Completed { stderr, .. } = run else {
            panic!("expected the spew script to finish");
        };
        assert_eq!(stderr.len() as u64, CHILD_OUTPUT_CAP_BYTES);
        let _ = fs::remove_dir_all(&dir);
    }
}
