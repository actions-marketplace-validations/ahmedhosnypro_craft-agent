//! The stdio transport: one child process per server speaking
//! newline-delimited JSON-RPC 2.0 on stdin/stdout, stderr drained
//! into a bounded tail, and the shutdown ladder close-stdin → wait →
//! SIGTERM → SIGKILL.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::version;

/// Bound on one protocol line; a longer line is a framing error, so a
/// hostile server cannot balloon the client's memory. Sits above the
/// oversized-result corpus payload so the 256 KiB tool-time cap is
/// what trims it, not the wire.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
/// Why a protocol exchange failed; `Transport` failures are the only
/// ones the reconnect ladder retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    Transport(String),
    Timeout { ms: u64 },
    Malformed(String),
    Rpc { code: i64, message: String },
    ArgsTooLarge { bytes: usize },
    VersionRejected { answered: String },
}

impl CallError {
    /// The one-line cause failure messages and combined errors quote.
    pub fn message(&self) -> String {
        match self {
            CallError::Transport(reason) => format!("transport failure: {reason}"),
            CallError::Timeout { ms } => format!("the call exceeded its {ms}ms timeout"),
            CallError::Malformed(detail) => format!("malformed protocol output: {detail}"),
            CallError::Rpc { code, message } => {
                format!("JSON-RPC error {code}: {message}")
            }
            CallError::ArgsTooLarge { bytes } => {
                format!("the arguments serialize to {bytes} bytes")
            }
            CallError::VersionRejected { answered } => {
                format!("the server speaks protocol version {answered}")
            }
        }
    }
}

pub struct Conn {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    pub(crate) reader: BufReader<ChildStdout>,
    pub(crate) next_id: u64,
}

/// Keeps the last [`STDERR_TAIL_CAP`] bytes of a child's stderr.
pub type StderrTail = Arc<Mutex<Vec<u8>>>;

async fn write_message(stdin: &mut ChildStdin, message: &Value) -> Result<(), CallError> {
    let Ok(mut line) = serde_json::to_string(message) else {
        return Err(CallError::Malformed(String::from(
            "cannot serialize the request",
        )));
    };
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|source| CallError::Transport(format!("stdin write failed: {source}")))?;
    stdin
        .flush()
        .await
        .map_err(|source| CallError::Transport(format!("stdin flush failed: {source}")))
}

/// Reads one line, bounded: an oversized line, invalid UTF-8, or a
/// clean close are typed framing errors, never a skip.
async fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Value, CallError> {
    let mut buf = Vec::new();
    loop {
        if let Some(line) = fill_line(reader, &mut buf).await? {
            return parse_line(line);
        }
    }
}

/// Accumulates bytes until a newline: `None` when more input is needed.
async fn fill_line(
    reader: &mut BufReader<ChildStdout>,
    buf: &mut Vec<u8>,
) -> Result<Option<Vec<u8>>, CallError> {
    let available = reader
        .fill_buf()
        .await
        .map_err(|source| CallError::Transport(format!("stdout read failed: {source}")))?;
    if available.is_empty() {
        return Err(closed_or_partial(buf));
    }
    let Some(nl) = available.iter().position(|byte| *byte == b'\n') else {
        buf.extend_from_slice(available);
        let read = available.len();
        reader.consume(read);
        return check_line_bound(buf).map(|_| None);
    };
    buf.extend_from_slice(&available[..nl]);
    reader.consume(nl + 1);
    check_line_bound(buf)?;
    Ok(Some(std::mem::take(buf)))
}

fn closed_or_partial(buf: &[u8]) -> CallError {
    if buf.is_empty() {
        CallError::Transport(String::from("the server closed its stdout"))
    } else {
        CallError::Malformed(String::from("the server ended mid-message"))
    }
}

fn check_line_bound(buf: &[u8]) -> Result<(), CallError> {
    if buf.len() > MAX_MESSAGE_BYTES {
        return Err(CallError::Malformed(format!(
            "a protocol line exceeds {MAX_MESSAGE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn parse_line(buf: Vec<u8>) -> Result<Value, CallError> {
    let text = String::from_utf8(buf)
        .map_err(|_| CallError::Malformed(String::from("a protocol line is not valid UTF-8")))?;
    serde_json::from_str(&text).map_err(|source| {
        CallError::Malformed(format!("a protocol line is not valid JSON: {source}"))
    })
}

/// Reads lines until the response with `id` arrives; notifications
/// are skipped, anything else is a framing error.
async fn read_response(reader: &mut BufReader<ChildStdout>, id: u64) -> Result<Value, CallError> {
    loop {
        let message = read_message(reader).await?;
        let Some(object) = message.as_object() else {
            return Err(CallError::Malformed(String::from(
                "a response is not a JSON object",
            )));
        };
        if object.contains_key("method") {
            continue;
        }
        match object.get("id") {
            Some(Value::Number(number)) if number.as_u64() == Some(id) => {
                if let Some(error) = object.get("error") {
                    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
                    let text = error.get("message").and_then(Value::as_str).unwrap_or("");
                    return Err(CallError::Rpc {
                        code,
                        message: text.to_string(),
                    });
                }
                return Ok(object.get("result").cloned().unwrap_or(Value::Null));
            }
            Some(Value::Number(number)) => {
                return Err(CallError::Malformed(format!(
                    "a response carried the wrong id ({number})"
                )));
            }
            _ => {
                return Err(CallError::Malformed(String::from(
                    "a response carried no id",
                )));
            }
        }
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// One bounded round trip: the deadline covers the write and the read,
/// so a server that stops draining stdin cannot hang the call.
async fn exchange(
    conn: &mut Conn,
    request: Value,
    deadline: Instant,
    timeout_ms: u64,
) -> Result<Value, CallError> {
    let write = write_message(&mut conn.stdin, &request);
    match tokio::time::timeout(remaining(deadline), write).await {
        Ok(result) => result?,
        Err(_) => return Err(CallError::Timeout { ms: timeout_ms }),
    }
    let id = request.get("id").and_then(Value::as_u64).unwrap_or(0);
    let read = read_response(&mut conn.reader, id);
    match tokio::time::timeout(remaining(deadline), read).await {
        Ok(result) => result,
        Err(_) => Err(CallError::Timeout { ms: timeout_ms }),
    }
}

fn next_id(conn: &mut Conn) -> u64 {
    let id = conn.next_id;
    conn.next_id = conn.next_id.saturating_add(1);
    id
}

impl Conn {
    /// Spawn plus the legacy handshake: initialize, version check,
    /// notifications/initialized. The deadline bounds the exchange.
    pub async fn connect(&mut self, deadline: Instant, timeout_ms: u64) -> Result<(), CallError> {
        let id = next_id(self);
        let answered =
            exchange(self, version::initialize_request(id), deadline, timeout_ms).await?;
        let negotiated = answered
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CallError::Malformed(String::from(
                    "the initialize response named no protocolVersion",
                ))
            })?;
        if !version::accepts_server_version(negotiated) {
            return Err(CallError::VersionRejected {
                answered: negotiated.to_string(),
            });
        }
        write_message(&mut self.stdin, &version::initialized_notification()).await?;
        Ok(())
    }

    /// `tools/list` followed to exhaustion, at most `max_pages` pages.
    pub async fn list_tools(
        &mut self,
        deadline: Instant,
        timeout_ms: u64,
        max_pages: usize,
    ) -> Result<Vec<Value>, CallError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..max_pages {
            let id = next_id(self);
            let page = exchange(
                self,
                version::tools_list_request(id, cursor.as_deref()),
                deadline,
                timeout_ms,
            )
            .await?;
            let page_tools = page.get("tools").and_then(Value::as_array).ok_or_else(|| {
                CallError::Malformed(String::from("the tools/list result named no tools array"))
            })?;
            tools.extend(page_tools.iter().cloned());
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err(CallError::Malformed(format!(
            "tools/list paged past the {max_pages}-page bound"
        )))
    }
}

impl Conn {
    /// One `tools/call` round trip; the result value is the caller's
    /// to convert. Oversized arguments never reach the wire.
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: &Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let id = next_id(self);
        let request = version::tools_call_request(id, name, arguments);
        let Ok(serialized) = serde_json::to_string(&request) else {
            return Err(CallError::Malformed(String::from(
                "cannot serialize the request",
            )));
        };
        if serialized.len() > MAX_MESSAGE_BYTES {
            return Err(CallError::ArgsTooLarge {
                bytes: serialized.len(),
            });
        }
        let deadline = Instant::now() + timeout;
        exchange(
            self,
            request,
            deadline,
            timeout.as_millis().min(u64::MAX as u128) as u64,
        )
        .await
    }
}
