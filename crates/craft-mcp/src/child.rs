//! The child-process side of the stdio transport: spawn with the
//! scrubbed env plus overlay, the bounded stderr tail, and the
//! shutdown ladder close-stdin -> wait -> SIGTERM -> SIGKILL.

use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;

use crate::client::Conn;

pub use crate::client::StderrTail;

/// Keeps the last [`STDERR_TAIL_CAP`] bytes of a child's stderr.
pub const STDERR_TAIL_CAP: usize = 4096;
const SIGTERM_GRACE: Duration = Duration::from_secs(5);

pub fn spawn(
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    child_env: Vec<(String, String)>,
    stderr_tail: StderrTail,
) -> Result<Conn, String> {
    let mut command_line = Command::new(command);
    command_line
        .args(args)
        .env_clear()
        .envs(child_env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command_line.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        command_line.process_group(0);
    }
    let mut child = command_line
        .spawn()
        .map_err(|source| format!("could not spawn `{command}`: {source}"))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (stdin, stdout) = match (stdin, stdout) {
        (Some(stdin), Some(stdout)) => (stdin, stdout),
        _ => return Err(format!("`{command}` closed a protocol pipe at spawn")),
    };
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            let mut stderr = tokio::io::BufReader::new(stderr);
            let mut chunk = [0u8; 4096];
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let Ok(mut tail) = stderr_tail.lock() else {
                            break;
                        };
                        if tail.len() + read > STDERR_TAIL_CAP {
                            let excess = tail.len() + read - STDERR_TAIL_CAP;
                            let drop = excess.min(tail.len());
                            tail.drain(..drop);
                        }
                        tail.extend_from_slice(&chunk[..read]);
                    }
                }
            }
        });
    }
    Ok(Conn {
        child,
        stdin,
        reader: BufReader::new(stdout),
        next_id: 1,
    })
}

impl Conn {
    /// Close stdin, wait, SIGTERM, then SIGKILL.
    pub async fn shutdown(mut self) {
        self.stdin.shutdown().await.ok();
        drop(self.stdin);
        if wait_grace(&mut self.child).await {
            return;
        }
        signal_group(&self.child, true);
        if wait_grace(&mut self.child).await {
            return;
        }
        signal_group(&self.child, false);
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

/// One grace-window wait; true when the child exited on its own.
async fn wait_grace(child: &mut tokio::process::Child) -> bool {
    tokio::time::timeout(SIGTERM_GRACE, child.wait())
        .await
        .is_ok()
}

/// Signals the child's whole process group: SIGTERM when `graceful`,
/// SIGKILL otherwise. A no-op off unix.
fn signal_group(child: &tokio::process::Child, graceful: bool) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, killpg};
        if let Some(pid) = child.id() {
            let signal = if graceful {
                Signal::SIGTERM
            } else {
                Signal::SIGKILL
            };
            let _ = killpg(nix::unistd::Pid::from_raw(pid as i32), signal);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (child, graceful);
    }
}
