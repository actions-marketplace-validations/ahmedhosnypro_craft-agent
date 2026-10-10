//! A compiled stdio MCP server for the offline corpus: newline JSON-RPC
//! 2.0 on stdin/stdout. Modes by argv:
//!
//!   (default)          initialize → tools/list (2 tools, one paginated
//!                      via nextCursor) → tools/call echo round-trip
//!   --slow-start       sleep 60 s before serving (startup-timeout path)
//!   --die-after-init   exit right after answering tools/list
//!   --die-on-call      exit during tools/call (mid-run death)
//!   --noisy-stderr     flood stderr continuously (bounded-tail proof)
//!   --bad-schema       tools/list with a null inputSchema
//!   --error-code       tools/call answers JSON-RPC -32602
//!   --is-error         tools/call answers isError:true
//!   --many-tools N     N tools (0 for an empty catalog)
//!   --oversized-result 5 MiB text result (tool-time cap proof)
//!   --garbage-on-call  answer tools/call with a non-JSON stdout line
//!                      (framing crash-law row)
//!
//! Flags combine, e.g. --noisy-stderr --die-on-call.

use std::io::{BufRead, Write};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

#[derive(Default)]
struct Modes {
    slow_start: bool,
    die_after_init: bool,
    die_on_call: bool,
    noisy_stderr: bool,
    bad_schema: bool,
    error_code: bool,
    is_error: bool,
    oversized_result: bool,
    garbage_on_call: bool,
    many_tools: Option<usize>,
}

fn parse_args(args: &[String]) -> Result<Modes, String> {
    let mut modes = Modes::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--slow-start" => modes.slow_start = true,
            "--die-after-init" => modes.die_after_init = true,
            "--die-on-call" => modes.die_on_call = true,
            "--noisy-stderr" => modes.noisy_stderr = true,
            "--bad-schema" => modes.bad_schema = true,
            "--error-code" => modes.error_code = true,
            "--is-error" => modes.is_error = true,
            "--oversized-result" => modes.oversized_result = true,
            "--garbage-on-call" => modes.garbage_on_call = true,
            "--many-tools" => {
                index += 1;
                let count = args
                    .get(index)
                    .ok_or_else(|| String::from("--many-tools needs a count"))?;
                let count: usize = count
                    .parse()
                    .map_err(|_| format!("--many-tools needs a number, got `{count}`"))?;
                modes.many_tools = Some(count);
            }
            other => return Err(format!("unknown mode `{other}`")),
        }
        index += 1;
    }
    Ok(modes)
}

fn write_line(value: &Value) {
    let mut stdout = std::io::stdout().lock();
    let mut line = serde_json::to_string(value).unwrap_or_else(|_| String::from("{}"));
    line.push('\n');
    if stdout.write_all(line.as_bytes()).is_ok() {
        let _ = stdout.flush();
    }
}

fn respond(id: &Value, result: Value) {
    write_line(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn respond_error(id: &Value, code: i64, message: &str) {
    write_line(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }));
}

fn echo_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "msg": { "type": "string", "description": "text to echo" } },
        "required": ["msg"],
        "additionalProperties": false,
    })
}

fn tool_def(name: &str, description: &str) -> Value {
    json!({ "name": name, "description": description, "inputSchema": echo_schema() })
}

fn list_page(modes: &Modes, cursor: Option<&str>) -> Value {
    if modes.bad_schema {
        return json!({
            "tools": [{ "name": "broken", "description": "non-object schema", "inputSchema": null }],
        });
    }
    if let Some(count) = modes.many_tools {
        let tools: Vec<Value> = (0..count)
            .map(|index| {
                let name = format!("tool-{:04}", index + 1);
                tool_def(&name, "a numbered fixture tool")
            })
            .collect();
        return json!({ "tools": tools });
    }
    match cursor {
        None => json!({
            "tools": [tool_def("echo", "echoes its arguments back as text")],
            "nextCursor": "page-2",
        }),
        Some("page-2") => json!({
            "tools": [tool_def("report", "a fixed status line")],
        }),
        Some(_) => json!({ "tools": [], "error": "unknown cursor" }),
    }
}

fn call_result(modes: &Modes, id: &Value, name: &str, arguments: &Value) {
    if modes.die_on_call {
        std::process::exit(0);
    }
    if modes.garbage_on_call {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(b"this line is not json\n");
        let _ = stdout.flush();
        std::thread::sleep(Duration::from_millis(250));
        std::process::exit(0);
    }
    let error = |message: String| respond_error(id, -32602, &message);
    if modes.error_code {
        error(String::from(
            "Invalid params: the fixture answers --error-code",
        ));
        return;
    }
    if modes.is_error {
        respond(
            id,
            json!({
                "content": [{ "type": "text", "text": "the fixture tool failed deliberately" }],
                "isError": true,
            }),
        );
        return;
    }
    if modes.oversized_result {
        respond(
            id,
            json!({
                "content": [{ "type": "text", "text": "a".repeat(5 * 1024 * 1024) }],
            }),
        );
        return;
    }
    match name {
        "echo" => respond(
            id,
            json!({
                "content": [{ "type": "text", "text": format!("echo: {arguments}") }],
            }),
        ),
        "report" => respond(
            id,
            json!({ "content": [{ "type": "text", "text": "fixture report: all good" }] }),
        ),
        other => error(format!("Invalid params: unknown tool `{other}`")),
    }
}

fn serve(modes: Modes) {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        match message.get("method").and_then(Value::as_str).unwrap_or("") {
            "initialize" => respond(
                &id,
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "mcp-fixture", "version": "0.1.0" },
                }),
            ),
            "notifications/initialized" => {}
            "tools/list" => {
                let cursor = message
                    .get("params")
                    .and_then(|params| params.get("cursor"))
                    .and_then(Value::as_str);
                respond(&id, list_page(&modes, cursor));
                if modes.die_after_init {
                    std::process::exit(0);
                }
            }
            "tools/call" => {
                let name = message
                    .get("params")
                    .and_then(|params| params.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let arguments = message
                    .get("params")
                    .and_then(|params| params.get("arguments"))
                    .cloned()
                    .unwrap_or(json!({}));
                call_result(&modes, &id, name, &arguments);
            }
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let modes = match parse_args(&args) {
        Ok(modes) => modes,
        Err(problem) => {
            eprintln!("mcp-fixture: {problem}");
            std::process::exit(2);
        }
    };
    if modes.slow_start {
        thread::sleep(Duration::from_secs(60));
    }
    if modes.noisy_stderr {
        for counter in 1..=64 {
            eprintln!("mcp-fixture stderr noise line {counter}");
        }
        thread::spawn(|| {
            let mut counter = 64;
            loop {
                counter += 1;
                eprintln!("mcp-fixture stderr noise line {counter}");
                thread::sleep(Duration::from_millis(2));
            }
        });
    }
    serve(modes);
}
