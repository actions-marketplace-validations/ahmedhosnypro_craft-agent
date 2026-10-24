//! RED corpus: agent-file field rules — required identity, name
//! charset and stem, description shape and trigger, model shape, and
//! tool ids; each row mirrors a rule of the agent-file gate.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use craft_core::agents::{AgentFileError, load_file};

fn scratch(name: &str) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "craft-agents-v-{}-{name}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn write(dir: &Path, file: &str, text: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(file);
    fs::write(&path, text)?;
    Ok(path)
}

/// A profile file whose frontmatter is `frontmatter` (full lines).
fn profile(name: &str, file: &str, frontmatter: &str) -> std::io::Result<PathBuf> {
    let dir = scratch(name)?;
    write(&dir, file, &format!("---\n{frontmatter}---\nBody.\n"))
}

/// A file whose frontmatter carries `extra` after a valid identity.
fn agent(file: &str, extra: &str) -> std::io::Result<PathBuf> {
    profile(
        file,
        file,
        &format!("name: probe\ndescription: Use for probing.\n{extra}"),
    )
}

fn expect(path: &Path, fragment: &str) {
    let text = match load_file(path) {
        Err(error) => error.to_string(),
        Ok(_) => String::from("the file was accepted"),
    };
    assert!(text.contains(fragment), "expected `{fragment}` in `{text}`");
}

#[test]
fn name_is_required() {
    let path = profile("noname", "probe.md", "description: Use for probing.\n").unwrap();
    expect(&path, "missing required field: name");
}

#[test]
fn empty_name_is_rejected() {
    let path = profile(
        "emptyname",
        "probe.md",
        "name:\ndescription: Use for probing.\n",
    )
    .unwrap();
    expect(&path, "name is empty");
}

#[test]
fn description_is_required() {
    let path = profile("nodesc", "probe.md", "name: probe\n").unwrap();
    expect(&path, "missing required field: description");
}

#[test]
fn empty_description_is_rejected() {
    let path = profile("emptydesc", "probe.md", "name: probe\ndescription:\n").unwrap();
    expect(&path, "description is empty");
}

#[test]
fn uppercase_name_is_rejected() {
    let path = profile(
        "upper",
        "probe.md",
        "name: Probe\ndescription: Use for probing.\n",
    )
    .unwrap();
    expect(&path, "name must be a lowercase slug");
}

#[test]
fn overlong_name_is_rejected() {
    let long = format!("a{}.md", "b".repeat(64));
    let path = profile(
        "longname",
        &long,
        &format!("name: a{}\ndescription: Use for probing.\n", "b".repeat(64)),
    )
    .unwrap();
    expect(&path, "name must be a lowercase slug");
}

#[test]
fn stem_mismatch_is_rejected() {
    let path = profile(
        "stem",
        "probe.md",
        "name: other\ndescription: Use for probing.\n",
    )
    .unwrap();
    expect(&path, "name 'other' must equal the file stem 'probe'");
}

#[test]
fn placeholder_description_is_rejected() {
    let path = profile("placeholder", "probe.md", "name: probe\ndescription: TBD\n").unwrap();
    expect(&path, "placeholder description");
}

#[test]
fn oversized_description_is_rejected() {
    let long = "x".repeat(1025);
    let path = profile(
        "bigdesc",
        "probe.md",
        &format!("name: probe\ndescription: {long}\n"),
    )
    .unwrap();
    expect(&path, "description is 1025 bytes (cap 1024)");
}

#[test]
fn missing_trigger_word_is_rejected() {
    let path = profile(
        "notrigger",
        "probe.md",
        "name: probe\ndescription: A quiet auditor.\n",
    )
    .unwrap();
    expect(&path, "description must say when to use the agent");
}

#[test]
fn bad_model_is_rejected() {
    let path = agent("probe.md", "model: Not A Model\n").unwrap();
    expect(&path, "model must be inherit or a bare lowercase id");
}

#[test]
fn unknown_tool_id_is_rejected() {
    let path = agent("probe.md", "tools: bash, read\n").unwrap();
    expect(&path, "unknown tool id 'read' in tools");
}

#[test]
fn the_spawn_pool_vocabulary_is_accepted() {
    let path = agent(
        "probe.md",
        "tools: bash, read_file, spawn_agent, spawn_agents, poll_agents\n",
    )
    .unwrap();
    let file = load_file(&path).expect("the spawn pool ids must load");
    let tools = file.tools.unwrap_or_default();
    assert_eq!(
        tools,
        vec![
            "bash".to_string(),
            "read_file".to_string(),
            "spawn_agent".to_string(),
            "spawn_agents".to_string(),
            "poll_agents".to_string(),
        ]
    );
}

#[test]
fn unknown_disallowed_tool_id_is_rejected() {
    let path = agent("probe.md", "disallowedTools: shell\n").unwrap();
    expect(&path, "unknown tool id 'shell' in disallowedTools");
}

#[test]
fn unbalanced_brackets_are_rejected() {
    let path = agent("probe.md", "tools: [bash\n").unwrap();
    expect(&path, "unbalanced bracket list in tools");
}

#[test]
fn empty_tool_id_is_rejected() {
    let path = agent("probe.md", "tools: bash,,grep\n").unwrap();
    expect(&path, "empty tool id in tools");
}

#[test]
fn empty_tool_list_is_rejected() {
    let path = agent("probe.md", "tools: []\n").unwrap();
    expect(&path, "empty tool list in tools");
}

#[test]
fn bare_empty_tools_value_is_rejected() {
    let path = agent("probe.md", "tools:\n").unwrap();
    expect(&path, "empty tool list in tools");
}

#[test]
fn missing_explicit_file_is_a_typed_io_error() {
    let missing = scratch("missing").unwrap().join("probe.md");
    let err = load_file(&missing).expect_err("a missing file must fail closed");
    assert!(matches!(err, AgentFileError::Io { .. }));
    assert!(err.to_string().contains("cannot read"));
}

#[test]
fn mcp_tokens_are_accepted_structurally() {
    let path = agent(
        "probe.md",
        "tools: read_file, mcp, mcp:repo-stats\ndisallowedTools: mcp:other\n",
    )
    .unwrap();
    let file = load_file(&path).unwrap();
    assert_eq!(
        file.tools,
        Some(vec![
            String::from("read_file"),
            String::from("mcp"),
            String::from("mcp:repo-stats"),
        ])
    );
    assert_eq!(file.disallowed_tools, [String::from("mcp:other")]);
}

#[test]
fn malformed_mcp_tokens_are_rejected() {
    for bad in [
        "mcp:",
        "mcp:Bad_Server",
        "mcpx",
        "mcp:server-name-that-is-way-over-thirty-two-chars",
    ] {
        let path = agent("probe.md", &format!("tools: read_file, {bad}\n")).unwrap();
        expect(&path, &format!("unknown tool id '{bad}' in tools"));
    }
}

#[test]
fn deny_glob_lists_load_from_frontmatter() {
    let path = agent(
        "probe.md",
        "denyRead: tests/**, **/*.spec.ts\ndenyWrite: secrets/**\n",
    )
    .unwrap();
    let file = load_file(&path).expect("a valid deny list must load");
    assert_eq!(file.deny_read, ["tests/**", "**/*.spec.ts"]);
    assert_eq!(file.deny_write, ["secrets/**"]);
}

#[test]
fn absolute_deny_glob_is_rejected() {
    let path = agent("probe.md", "denyRead: /etc/**\n").unwrap();
    expect(&path, "not a workspace-relative glob in denyRead");
}

#[test]
fn traversing_deny_glob_is_rejected() {
    let path = agent("probe.md", "denyWrite: ../secrets/**\n").unwrap();
    expect(&path, "not a workspace-relative glob in denyWrite");
}

#[test]
fn embedded_star_star_deny_glob_is_rejected() {
    let path = agent("probe.md", "denyRead: a**b/**\n").unwrap();
    expect(&path, "not a workspace-relative glob in denyRead");
}

#[test]
fn empty_deny_list_is_rejected() {
    let path = agent("probe.md", "denyRead: []\n").unwrap();
    expect(&path, "empty glob list in denyRead");
}

#[test]
fn an_unknown_frontmatter_key_still_fails() {
    let path = agent("probe.md", "denyread: tests/**\n").unwrap();
    expect(&path, "unknown frontmatter key: denyread");
}
