//! Manager corpus, part two: failure and crash-law rows. Connect
//! failures never kill the run; mid-run deaths map to typed tool
//! errors the model reads and the run continues.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{call_tool, connect, fixture_command, mcp_on, one_server_workspace, statuses};
use common::{fresh_workspace, write_mcp_json};
use craft_mcp::child::{self, StderrTail};
use craft_mcp::client::CallError;
use craft_mcp::events::{McpEvent, McpStatus};
use serde_json::json;

/// Connects expecting fail-soft: zero tools and a Failed status whose
/// cause is returned for substring asserts.
// Corpus helpers: the loud-test idiom for panics.
#[allow(clippy::expect_used, clippy::panic)]
async fn failed_server_cause(workspace: &Path) -> String {
    let outcome = connect(workspace, &mcp_on())
        .await
        .expect("connect failures are fail-soft");
    assert!(outcome.tools.is_empty());
    let statuses = statuses(&outcome);
    let McpEvent::McpServerStatus {
        status: McpStatus::Failed,
        error: Some(cause),
        ..
    } = statuses[0]
    else {
        panic!("the server fails with a status event: {statuses:?}");
    };
    cause.to_string()
}

/// Spawns one server, calls its echo tool, and returns the typed
/// error text after shutdown.
// Corpus helper: the loud-test idiom for panics.
#[allow(clippy::expect_used)]
async fn echo_call_error(args: &[&str]) -> String {
    let workspace = one_server_workspace(args);
    let outcome = connect(&workspace, &mcp_on()).await.expect("connects");
    let err = call_tool(&outcome, "mcp__srv__echo", json!({}))
        .await
        .expect_err("the call fails typed");
    outcome.manager.shutdown().await;
    err
}

#[tokio::test]
async fn a_slow_start_times_out_and_the_run_continues() {
    let workspace = one_server_workspace(&["--slow-start"]);
    let cause = failed_server_cause(&workspace).await;
    assert!(
        cause.contains("timeout") || cause.contains("exceeded"),
        "{cause}"
    );
}

#[tokio::test]
async fn dying_after_discovery_fails_the_server_status() {
    let workspace = one_server_workspace(&["--die-after-init"]);
    let cause = failed_server_cause(&workspace).await;
    assert!(cause.contains("transport failure"), "{cause}");
}

#[tokio::test]
async fn a_non_object_schema_fails_the_whole_server() {
    let workspace = one_server_workspace(&["--bad-schema"]);
    let cause = failed_server_cause(&workspace).await;
    assert!(cause.contains("non-object inputSchema"), "{cause}");
}

#[tokio::test]
async fn an_empty_catalog_connects_with_zero_tools() {
    let workspace = one_server_workspace(&["--many-tools", "0"]);
    let outcome = connect(&workspace, &mcp_on())
        .await
        .expect("an empty catalog connects");
    assert!(outcome.tools.is_empty());
    let McpEvent::McpServerStatus {
        status: McpStatus::Connected,
        tool_count: Some(0),
        ..
    } = statuses(&outcome)[0]
    else {
        panic!(
            "an empty catalog is a healthy server: {:?}",
            statuses(&outcome)
        );
    };
    assert!(
        outcome
            .events
            .iter()
            .any(|event| matches!(event, McpEvent::McpToolsDiscovered { .. })),
        "the empty discovery record still emits"
    );
    outcome.manager.shutdown().await;
}

#[tokio::test]
async fn a_mid_run_death_maps_to_a_typed_error_after_the_ladder() {
    let workspace = one_server_workspace(&["--die-on-call"]);
    let outcome = connect(&workspace, &mcp_on())
        .await
        .expect("the server connects first");
    assert_eq!(
        outcome.tools.len(),
        2,
        "the tools register before the death"
    );
    let err = call_tool(&outcome, "mcp__srv__echo", json!({ "msg": "x" }))
        .await
        .expect_err("the dying server turns into a typed error");
    assert!(err.contains("mcp server `srv` failed"), "{err}");
    assert!(
        err.contains("after reconnect"),
        "the combined error names both causes: {err}"
    );
    let again = call_tool(&outcome, "mcp__srv__echo", json!({ "msg": "x" }))
        .await
        .expect_err("the dead name keeps its typed error");
    assert!(again.contains("mcp server `srv` failed"), "{again}");
    outcome.manager.shutdown().await;
}

#[tokio::test]
async fn jsonrpc_errors_surface_without_a_reconnect() {
    let workspace = one_server_workspace(&["--error-code"]);
    let outcome = connect(&workspace, &mcp_on()).await.expect("connects");
    let err = call_tool(&outcome, "mcp__srv__echo", json!({}))
        .await
        .expect_err("a JSON-RPC error is a typed error");
    assert!(err.contains("JSON-RPC error -32602"), "{err}");
    outcome.manager.shutdown().await;
}

#[tokio::test]
async fn server_side_iserror_flows_as_a_typed_error_result() {
    let workspace = one_server_workspace(&["--is-error"]);
    let outcome = connect(&workspace, &mcp_on()).await.expect("connects");
    let err = call_tool(&outcome, "mcp__srv__echo", json!({}))
        .await
        .expect_err("isError:true is a typed error the model reads");
    assert!(err.contains("failed deliberately"), "{err}");
    outcome.manager.shutdown().await;
}

#[tokio::test]
async fn non_json_stdout_is_a_typed_malformed_result_never_a_skip() {
    let err = echo_call_error(&["--garbage-on-call"]).await;
    assert!(err.contains("malformed result"), "{err}");
    assert!(err.contains("not valid JSON"), "{err}");
}

#[tokio::test]
async fn the_stderr_tail_rides_failure_messages() {
    let err = echo_call_error(&["--noisy-stderr", "--die-on-call"]).await;
    assert!(err.contains("stderr tail"), "{err}");
    assert!(err.contains("stderr noise line"), "{err}");
}

#[tokio::test]
async fn a_server_that_stops_draining_stdin_cannot_hang_the_call() {
    let tail: StderrTail = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut conn = child::spawn(
        &fixture_command(),
        &[String::from("--slow-start")],
        None,
        Vec::new(),
        tail,
    )
    .expect("the sleeping fixture spawns");
    let arguments = json!({ "msg": "a".repeat(2 * 1024 * 1024) });
    let call = conn.call_tool("echo", &arguments, Duration::from_secs(1));
    let outcome = tokio::time::timeout(Duration::from_secs(30), call)
        .await
        .expect("the write half honors the deadline");
    let err = outcome.expect_err("an unanswered call is a typed timeout");
    assert!(
        matches!(err, CallError::Timeout { ms: 1_000 }),
        "the call times out typed: {err:?}"
    );
    conn.shutdown().await;
}

#[tokio::test]
async fn a_failed_reconnect_dedups_queued_calls_instead_of_respawning() {
    let workspace = fresh_workspace("manager");
    let fixture = fixture_command();
    let wrapper = format!(
        "test -e \"{}/block-reconnect\" && exit 1; exec \"{}\" --die-on-call",
        workspace.display(),
        fixture
    );
    write_mcp_json(
        &workspace,
        &format!(
            "{{\"mcpServers\": {{\"srv\": {{\"command\": \"sh\", \"args\": [\"-c\", {wrapper:?}], \"startupTimeoutSecs\": 2, \"toolTimeoutSecs\": 2}}}}}}"
        ),
    );
    let outcome = connect(&workspace, &mcp_on())
        .await
        .expect("the server connects through the wrapper");
    assert_eq!(outcome.tools.len(), 2);
    std::fs::write(workspace.join("block-reconnect"), b"")
        .expect("the reconnect blocker is in place");
    let (first, second) = tokio::join!(
        call_tool(&outcome, "mcp__srv__echo", json!({ "msg": "x" })),
        call_tool(&outcome, "mcp__srv__echo", json!({ "msg": "y" })),
    );
    let errors = [first, second]
        .into_iter()
        .map(|result| result.expect_err("both calls fail typed"))
        .collect::<Vec<_>>();
    assert!(
        errors.iter().any(|err| err.contains("reconnect failed")),
        "the lock holder attempts one reconnect: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|err| err.contains("the server is not connected")),
        "the queued call reuses the outcome instead of respawning: {errors:?}"
    );
    outcome.manager.shutdown().await;
}

#[tokio::test]
async fn oversized_results_cap_at_the_tool_time_bound() {
    let workspace = one_server_workspace(&["--oversized-result"]);
    let outcome = connect(&workspace, &mcp_on()).await.expect("connects");
    let output = call_tool(&outcome, "mcp__srv__echo", json!({}))
        .await
        .expect("the capped result is still a success");
    assert!(
        output.len() <= 256 * 1024 + 64,
        "the cap holds: {}",
        output.len()
    );
    assert!(
        output.ends_with("[output truncated]"),
        "the marker rides the cap"
    );
    outcome.manager.shutdown().await;
}
