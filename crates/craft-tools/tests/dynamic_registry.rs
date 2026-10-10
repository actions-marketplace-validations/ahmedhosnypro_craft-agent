//! DynamicTool corpus: run-time tools register beside the builtins,
//! dispatch through the same contracted path, and unknown names stay
//! typed errors. The MCP manager registers through this seam.

use std::sync::Arc;

use craft_tools::event::{ToolEvent, ToolResult, ToolStream};
use craft_tools::{DynamicTool, Registry, ToolError, ToolLimits};
use futures::stream::{self, StreamExt};

fn terminal_tool(name: &str, output: &str) -> DynamicTool {
    let output = output.to_string();
    DynamicTool {
        name: name.to_string(),
        description: String::from("a dynamic corpus tool"),
        schema: serde_json::json!({
            "type": "object",
            "properties": { "q": { "type": "string" } },
            "additionalProperties": false,
        }),
        execute: Arc::new(move |_args, _ctx| {
            let output = output.clone();
            let future = async move { Ok(ToolEvent::Terminal(ToolResult { output })) };
            stream::once(future).boxed() as ToolStream
        }),
    }
}

async fn single_item(stream: ToolStream) -> Result<ToolEvent, ToolError> {
    let items: Vec<Result<ToolEvent, ToolError>> = stream.collect().await;
    assert_eq!(
        items.len(),
        1,
        "the contract enforces exactly one terminal item"
    );
    items
        .into_iter()
        .next()
        .unwrap_or(Err(ToolError::DoubleTerminal))
}

#[tokio::test]
async fn dynamic_tools_dispatch_like_builtins() {
    let mut registry = Registry::with_builtins(ToolLimits::default());
    registry.register_dynamic(terminal_tool("mcp__srv__echo", "dynamic-ok"));
    let mut names = registry.names();
    names.retain(|name| name.starts_with("mcp__"));
    assert_eq!(names, ["mcp__srv__echo"]);
    let spec = registry.spec("mcp__srv__echo").expect("the spec is cached");
    assert_eq!(spec.description, "a dynamic corpus tool");
    assert_eq!(spec.schema["type"], "object");
    let ctx = craft_tools::ToolCtx::new(std::env::temp_dir());
    let event = single_item(registry.execute("mcp__srv__echo", serde_json::json!({}), &ctx))
        .await
        .expect("the dynamic tool executes");
    assert_eq!(
        event,
        ToolEvent::Terminal(ToolResult {
            output: String::from("dynamic-ok")
        })
    );
}

#[tokio::test]
async fn unknown_names_stay_typed_errors() {
    let registry = Registry::new(ToolLimits::default());
    let ctx = craft_tools::ToolCtx::new(std::env::temp_dir());
    let err = single_item(registry.execute("mcp__srv__nope", serde_json::json!({}), &ctx))
        .await
        .expect_err("unregistered names error");
    assert_eq!(
        err,
        ToolError::UnknownTool {
            name: String::from("mcp__srv__nope")
        }
    );
}

#[tokio::test]
async fn the_stream_contract_binds_dynamic_tools() {
    let double_terminal = DynamicTool {
        name: String::from("mcp__srv__broken"),
        description: String::new(),
        schema: serde_json::json!({ "type": "object" }),
        execute: Arc::new(|_args, _ctx| {
            let items: Vec<Result<ToolEvent, ToolError>> = vec![
                Ok(ToolEvent::Terminal(ToolResult {
                    output: String::new(),
                })),
                Ok(ToolEvent::Terminal(ToolResult {
                    output: String::new(),
                })),
            ];
            stream::iter(items).boxed() as ToolStream
        }),
    };
    let mut registry = Registry::new(ToolLimits::default());
    registry.register_dynamic(double_terminal);
    let ctx = craft_tools::ToolCtx::new(std::env::temp_dir());
    let items: Vec<Result<ToolEvent, ToolError>> = registry
        .execute("mcp__srv__broken", serde_json::json!({}), &ctx)
        .collect()
        .await;
    assert_eq!(
        items.len(),
        2,
        "the second terminal arrives as a typed error"
    );
    assert_eq!(
        items[1],
        Err(ToolError::DoubleTerminal),
        "the enforcer wraps dynamic streams too"
    );
    let _ = items[0].as_ref().map_err(|e| e.to_string()).map(|_| ());
}
