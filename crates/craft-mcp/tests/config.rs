//! Config corpus: every mcp.json rule fires on its bad fixture and
//! never on the good one — the validate-agents selftest law, plus the
//! layered merge and the knob-off leniency.
// Corpus helpers: the loud-test idiom for panics.
#![allow(clippy::panic)]

mod common;

use common::{fresh_workspace, write_mcp_json};
use craft_mcp::config::load_config;
use craft_mcp::error::McpError;

fn load(root: &std::path::Path) -> Result<craft_mcp::config::Loaded, McpError> {
    load_config(root, root, None)
}

fn err_rule(body: &str) -> String {
    let root = fresh_workspace("config");
    write_mcp_json(&root, body);
    match load(&root) {
        Ok(_) => panic!("this config must fail closed: {body}"),
        Err(McpError::Config { path, rule }) => {
            assert!(
                path.ends_with("mcp.json"),
                "the error names the file: {path}"
            );
            rule
        }
        Err(other) => panic!("config violations are Config errors, got {other:?}"),
    }
}

#[test]
fn bad_json_names_the_file_and_rule() {
    let rule = err_rule("{ not json");
    assert!(rule.contains("not valid JSON"), "{rule}");
}

#[test]
fn unknown_root_and_entry_keys_fail() {
    let rule = err_rule("{\"mcpServers\": {\"s\": {\"command\": \"x\", \"typo\": 1}}}");
    assert!(rule.contains("unknown field"), "{rule}");
    let rule = err_rule("{\"mcpServers\": {}, \"extra\": 1}");
    assert!(rule.contains("unknown field"), "{rule}");
}

#[test]
fn missing_mcpservers_fails_closed() {
    let rule = err_rule("{}");
    assert!(rule.contains("missing field `mcpServers`"), "{rule}");
}

#[test]
fn url_entries_are_the_typed_v2_later_error() {
    let rule =
        err_rule("{\"mcpServers\": {\"s\": {\"command\": \"x\", \"url\": \"https://srv\"}}}");
    assert!(rule.contains("url servers"), "{rule}");
    assert!(rule.contains("stdio"), "{rule}");
}

#[test]
fn non_stdio_type_fails() {
    let rule = err_rule("{\"mcpServers\": {\"s\": {\"command\": \"x\", \"type\": \"http\"}}}");
    assert!(rule.contains("type must be \"stdio\""), "{rule}");
}

#[test]
fn missing_or_empty_command_fails() {
    let rule = err_rule("{\"mcpServers\": {\"s\": {}}}");
    assert!(rule.contains("missing field `command`"), "{rule}");
    let rule = err_rule("{\"mcpServers\": {\"s\": {\"command\": \"\"}}}");
    assert!(rule.contains("command must not be empty"), "{rule}");
}

#[test]
fn non_absolute_cwd_fails() {
    let rule = err_rule("{\"mcpServers\": {\"s\": {\"command\": \"x\", \"cwd\": \"reports\"}}}");
    assert!(rule.contains("cwd must be an absolute path"), "{rule}");
}

#[test]
fn escaping_cwd_fails() {
    for cwd in ["/etc", "/"] {
        let rule = err_rule(&format!(
            "{{\"mcpServers\": {{\"s\": {{\"command\": \"x\", \"cwd\": {cwd:?}}}}}}}"
        ));
        assert!(rule.contains("escapes its layer's root"), "{rule}");
    }
}

#[test]
fn oversize_files_fail() {
    let root = fresh_workspace("config");
    let filler = "x".repeat(65_537);
    write_mcp_json(
        &root,
        &format!("{{\"mcpServers\": {{\"s\": {{\"command\": \"{filler}\"}}}}}}"),
    );
    match load(&root) {
        Err(McpError::Config { rule, .. }) => assert!(rule.contains("cap 65536"), "{rule}"),
        other => panic!("an oversize file must fail, got {other:?}"),
    }
}

#[test]
fn nine_servers_fail() {
    let mut servers = Vec::new();
    for index in 0..9 {
        servers.push(format!("\"s{index}\": {{\"command\": \"x\"}}"));
    }
    let rule = err_rule(&format!("{{\"mcpServers\": {{{}}}}}", servers.join(",")));
    assert!(rule.contains("cap 8"), "{rule}");
}

#[test]
fn bad_server_names_fail() {
    for name in ["Upper", "1s", "s p a c e", "s/dash", &"s".repeat(33)] {
        let rule = err_rule(&format!(
            "{{\"mcpServers\": {{\"{name}\": {{\"command\": \"x\"}}}}}}"
        ));
        assert!(rule.contains("name must match"), "{rule}");
    }
}

#[test]
fn provider_key_overlays_never_reach_servers() {
    let rule = err_rule(
        "{\"mcpServers\": {\"s\": {\"command\": \"x\", \"env\": {\"OPENAI_API_KEY\": \"k\"}}}}",
    );
    assert!(rule.contains("provider key"), "{rule}");
    let rule = err_rule("{\"mcpServers\": {\"s\": {\"command\": \"x\", \"env\": {\"N\": 3}}}}");
    assert!(rule.contains("string literal"), "{rule}");
}

#[test]
fn out_of_range_timeouts_fail() {
    for field in ["startupTimeoutSecs", "toolTimeoutSecs"] {
        for value in [0, 86_401] {
            let rule = err_rule(&format!(
                "{{\"mcpServers\": {{\"s\": {{\"command\": \"x\", \"{field}\": {value}}}}}}}"
            ));
            assert!(rule.contains(field), "{rule}");
            assert!(rule.contains("1..=86400"), "{rule}");
        }
    }
}

#[test]
fn good_configs_load_with_defaults() {
    let root = fresh_workspace("config");
    write_mcp_json(
        &root,
        &format!(
            "{{\"mcpServers\": {{\"repo-stats\": {{\"command\": \"/bin/true\", \
             \"args\": [\"--mode\", \"read-only\"], \
             \"env\": {{\"REPO_STATS_MODE\": \"pr\"}}, \"cwd\": {root:?}, \
             \"enabledTools\": [\"pr_summary\"], \"disabledTools\": [], \
             \"startupTimeoutSecs\": 30, \"toolTimeoutSecs\": 120}}}}}}"
        ),
    );
    let loaded = load(&root).expect("a well-formed config loads");
    assert!(loaded.project_present);
    assert_eq!(loaded.config.servers.len(), 1);
    let server = &loaded.config.servers[0];
    assert_eq!(server.name, "repo-stats");
    assert_eq!(server.args, ["--mode", "read-only"]);
    assert_eq!(
        server.env,
        [("REPO_STATS_MODE".to_string(), "pr".to_string())]
    );
    assert_eq!(server.cwd.as_deref(), Some(root.as_path()));
    assert!(server.enabled);
    assert_eq!(server.startup_timeout_secs, Some(30));
}

#[test]
fn no_file_anywhere_is_an_empty_config() {
    let root = fresh_workspace("config");
    let loaded = load(&root).expect("no file is not an error");
    assert!(!loaded.project_present && !loaded.host_present);
    assert!(loaded.config.servers.is_empty());
}

#[test]
fn layered_merge_replaces_per_name_wholesale() {
    let root = fresh_workspace("config");
    write_mcp_json(
        &root,
        "{\"mcpServers\": {\"shared\": {\"command\": \"/project\"}, \"only-project\": {\"command\": \"/p\"}}}",
    );
    let host = fresh_workspace("config-host");
    std::fs::create_dir_all(host.join(".craft-agent")).expect("host dir");
    std::fs::write(
        host.join(".craft-agent").join("mcp.json"),
        "{\"mcpServers\": {\"shared\": {\"command\": \"/host\", \"args\": [\"--host\"]}, \"only-host\": {\"command\": \"/h\"}}}",
    )
    .expect("host mcp.json");
    let loaded = load_config(&root, &root, Some(&host)).expect("layers merge");
    let names: Vec<&str> = loaded
        .config
        .servers
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["only-host", "only-project", "shared"]);
    let shared = loaded
        .config
        .servers
        .iter()
        .find(|s| s.name == "shared")
        .expect("the merged server exists");
    assert_eq!(shared.command, "/project");
    assert!(
        shared.args.is_empty(),
        "the project entry replaces wholesale"
    );
}

#[test]
fn each_layer_fences_cwd_against_its_own_root() {
    let root = fresh_workspace("config");
    write_mcp_json(&root, "{\"mcpServers\": {}}");
    let host = fresh_workspace("config-host");
    let host_cwd = host.join("mcp-data");
    let body =
        format!("{{\"mcpServers\": {{\"s\": {{\"command\": \"x\", \"cwd\": {host_cwd:?}}}}}}}");
    std::fs::create_dir_all(host.join(".craft-agent")).expect("host dir");
    std::fs::write(host.join(".craft-agent").join("mcp.json"), &body).expect("host mcp.json");
    let loaded =
        load_config(&root, &root, Some(&host)).expect("a host cwd inside the host home loads");
    let server = &loaded.config.servers[0];
    assert_eq!(server.cwd.as_deref(), Some(host_cwd.as_path()));

    let body = format!("{{\"mcpServers\": {{\"s\": {{\"command\": \"x\", \"cwd\": {root:?}}}}}}}");
    std::fs::write(host.join(".craft-agent").join("mcp.json"), &body).expect("host mcp.json");
    match load_config(&root, &root, Some(&host)) {
        Err(McpError::Config { rule, .. }) => {
            assert!(rule.contains("escapes its layer's root"), "{rule}")
        }
        other => panic!("a host cwd outside the host home fails, got {other:?}"),
    }
}

#[test]
fn an_invalid_host_layer_fails_closed_too() {
    let root = fresh_workspace("config");
    write_mcp_json(&root, "{\"mcpServers\": {}}");
    let host = fresh_workspace("config-host");
    std::fs::create_dir_all(host.join(".craft-agent")).expect("host dir");
    std::fs::write(host.join(".craft-agent").join("mcp.json"), "{ nope").expect("host mcp.json");
    match load_config(&root, &root, Some(&host)) {
        Err(McpError::Config { path, .. }) => assert!(path.contains("config-host"), "{path}"),
        other => panic!("an invalid host layer must fail, got {other:?}"),
    }
}
