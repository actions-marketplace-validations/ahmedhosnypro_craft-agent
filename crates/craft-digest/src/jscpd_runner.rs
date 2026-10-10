//! One jscpd run across the whole clone set, feeding the recorded-report
//! parser; the program is injectable so corpora drive fixture scripts.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::bundle::Bundle;
use crate::clone_runner::{BoundedRun, run_with_deadline};
use crate::error::DigestError;
use crate::jscpd::{DuplicateCandidate, read_jscpd_report};

/// jscpd's file reporters write `<output>/jscpd-report.json`.
pub const REPORT_FILE_NAME: &str = "jscpd-report.json";

/// Wall-clock bound for the jscpd run; expiry becomes an error row.
pub const JSCPD_DEADLINE: Duration = Duration::from_secs(600);

pub struct JscpdRunner {
    program: PathBuf,
    deadline: Duration,
}

impl Default for JscpdRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl JscpdRunner {
    pub fn new() -> Self {
        JscpdRunner {
            program: PathBuf::from("jscpd"),
            deadline: JSCPD_DEADLINE,
        }
    }

    /// Tests point this at a fake jscpd script.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        JscpdRunner {
            program: program.into(),
            deadline: JSCPD_DEADLINE,
        }
    }

    /// Tests shorten the bound to exercise expiry.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }
}

/// One run over the clone set: Rust mode, `**/target/**` ignored, json
/// report only, exit code passed through untouched. A missing binary, a
/// non-zero exit, a deadline expiry, or an unusable report becomes an
/// error row and no candidates — never a fatal Err.
pub fn run_jscpd(
    runner: &JscpdRunner,
    scan_root: &Path,
    report_dir: &Path,
    bundle: &mut Bundle,
) -> Result<Vec<DuplicateCandidate>, DigestError> {
    let mut command = Command::new(&runner.program);
    command
        .arg(scan_root)
        .arg("--format")
        .arg("rust")
        .arg("--ignore")
        .arg("**/target/**")
        .arg("--reporters")
        .arg("json")
        .arg("--output")
        .arg(report_dir);
    let run = match run_with_deadline(&mut command, runner.deadline) {
        Ok(run) => run,
        Err(err) => {
            bundle.push_error("bundle", "jscpd", &err.to_string());
            return Ok(Vec::new());
        }
    };
    match run {
        BoundedRun::Completed { status, stderr, .. } if !status.success() => {
            bundle.push_error("bundle", "jscpd", &String::from_utf8_lossy(&stderr));
            return Ok(Vec::new());
        }
        BoundedRun::Completed { .. } => {}
        BoundedRun::Expired => {
            let message = format!("jscpd exceeded its {:?} deadline", runner.deadline);
            bundle.push_error("bundle", "jscpd", &message);
            return Ok(Vec::new());
        }
    }
    match read_jscpd_report(&report_dir.join(REPORT_FILE_NAME)) {
        Ok(candidates) => Ok(candidates),
        Err(err) => {
            bundle.push_error("bundle", "jscpd", &err.to_string());
            Ok(Vec::new())
        }
    }
}
