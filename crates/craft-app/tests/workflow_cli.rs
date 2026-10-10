//! RED corpus: the `workflow` mission surface — CLI parse rows, arg
//! conversion, step-agent pre-resolution, and the run-dir layout.

mod workflow_common;

use std::fs;
use std::path::{Path, PathBuf};

use craft_app::cli::{OutputFormat, parse_cli, parse_kv};
use craft_app::error::AppError;
use craft_app::workflow_cmd::{convert_arg_flags, resolve_step_agents, workflow_run_dir};
use craft_workflow::plan::{ArgKind, ArgSpec, ArgValue};
use craft_workflow::{WorkflowPlan, plan};

use workflow_common::{AGENT_NAME, TINY_PLAN, parse_workflow_cli, repo, write_step_agent};

pub const GHOST_PLAN: &str = r#"---
name: ghost-flow
steps:
  - id: only
    agent: ghost-agent
    body: |
      Do the one step.
---
"#;

#[test]
fn workflow_name_flag_parses() {
    let cli = parse_workflow_cli(&["--workflow", "tiny-flow"]).expect("name flag parses");
    assert_eq!(cli.workflow.as_deref(), Some("tiny-flow"));
    assert_eq!(cli.workflow_file, None);
    assert!(cli.args.is_empty());
    assert_eq!(cli.output_format, OutputFormat::StreamJson);
    assert_eq!(cli.max_tokens, None);
    assert_eq!(cli.idle_timeout_secs, None);
}

#[test]
fn workflow_file_flag_parses() {
    let cli =
        parse_workflow_cli(&["--workflow-file", "plans/weekly.md"]).expect("file flag parses");
    assert_eq!(cli.workflow, None);
    assert_eq!(
        cli.workflow_file.as_deref(),
        Some(Path::new("plans/weekly.md"))
    );
}

#[test]
fn workflow_flags_are_mutually_exclusive() {
    let err = parse_cli([
        "craft-agent",
        "workflow",
        "--workflow",
        "a",
        "--workflow-file",
        "b.md",
    ])
    .expect_err("both flags must conflict");
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn workflow_arg_flags_parse_as_kv_pairs() {
    let cli =
        parse_workflow_cli(&["--arg", "window=48h", "--arg", "repos=a,b"]).expect("kv args parse");
    assert_eq!(
        cli.args,
        vec![
            ("window".to_string(), "48h".to_string()),
            ("repos".to_string(), "a,b".to_string())
        ]
    );
}

#[test]
fn workflow_arg_without_equals_is_rejected() {
    let err = parse_cli(["craft-agent", "workflow", "--arg", "noequals"])
        .expect_err("keyless arg must fail");
    assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    assert!(parse_kv("noequals").is_err());
    assert!(parse_kv("=value").is_err(), "an empty key is invalid");
    assert_eq!(
        parse_kv("k=").expect("empty value parses"),
        ("k".to_string(), String::new())
    );
}

fn tiny_plan() -> Result<WorkflowPlan, craft_workflow::WorkflowError> {
    WorkflowPlan::parse("tiny.md", TINY_PLAN)
}

fn arg_spec(kind: ArgKind) -> ArgSpec {
    ArgSpec {
        kind,
        default: None,
    }
}

#[test]
fn string_args_keep_their_value_verbatim() {
    let mut plan = tiny_plan().expect("tiny plan parses");
    plan.args.insert("window".into(), arg_spec(ArgKind::String));
    let provided =
        convert_arg_flags(&plan, &[("window".into(), "a,b with spaces".into())]).expect("converts");
    assert_eq!(
        provided.get("window"),
        Some(&ArgValue::Str("a,b with spaces".into()))
    );
}

#[test]
fn array_args_split_trim_and_drop_empty_items() {
    let mut plan = tiny_plan().expect("tiny plan parses");
    plan.args.insert("repos".into(), arg_spec(ArgKind::Array));
    let provided =
        convert_arg_flags(&plan, &[("repos".into(), " a , b , ".into())]).expect("converts");
    assert_eq!(
        provided.get("repos"),
        Some(&ArgValue::Seq(vec!["a".into(), "b".into()]))
    );
    let empty = convert_arg_flags(&plan, &[("repos".into(), String::new())]).expect("converts");
    assert_eq!(empty.get("repos"), Some(&ArgValue::Seq(Vec::new())));
}

#[test]
fn a_duplicate_arg_key_is_rejected() {
    let plan = tiny_plan().expect("tiny plan parses");
    let err = convert_arg_flags(
        &plan,
        &[("window".into(), "1".into()), ("window".into(), "2".into())],
    )
    .expect_err("a repeated key must fail");
    assert!(matches!(err, AppError::WorkflowArgDuplicate { key, .. } if key == "window"));
}

#[test]
fn an_undeclared_key_is_rejected_by_name_at_resolution() {
    let plan = tiny_plan().expect("tiny plan parses");
    let provided = convert_arg_flags(&plan, &[("ghost".into(), "1".into())])
        .expect("conversion passes undeclared keys through");
    let err = plan::resolve_args(&plan, &provided).expect_err("resolution rejects by name");
    assert!(
        err.to_string().contains("ghost"),
        "the error names the undeclared arg: {err}"
    );
}

#[test]
fn step_agents_pre_resolve_from_the_catalog() {
    let workspace = repo("agents-ok").expect("repo");
    write_step_agent(&workspace).expect("profile written");
    let plan = tiny_plan().expect("tiny plan parses");
    let agents = resolve_step_agents(&plan, &workspace).expect("profiles resolve");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents["only"].name, AGENT_NAME);
}

#[test]
fn a_missing_step_profile_names_the_step_and_lists_profiles() {
    let workspace = repo("agents-missing").expect("repo");
    write_step_agent(&workspace).expect("profile written");
    let plan = WorkflowPlan::parse("ghost.md", GHOST_PLAN).expect("ghost plan parses");
    let err = resolve_step_agents(&plan, &workspace).expect_err("ghost profile must fail");
    let AppError::WorkflowAgentMissing {
        step,
        name,
        available,
        ..
    } = err
    else {
        panic!("expected a missing-profile error, got {err:?}");
    };
    assert_eq!(step, "only");
    assert_eq!(name, "ghost-agent");
    assert_eq!(available, AGENT_NAME);
}

#[test]
fn an_invalid_named_profile_fails_closed() {
    let workspace = repo("agents-invalid").expect("repo");
    let dir = workspace.join(".craft-agent").join("agents");
    fs::write(
        dir.join("step-agent.md"),
        "---\nname: step-agent\n---\nBody.\n",
    )
    .expect("invalid profile written");
    let plan = tiny_plan().expect("tiny plan parses");
    let err = resolve_step_agents(&plan, &workspace).expect_err("invalid profile must fail");
    assert!(matches!(err, AppError::WorkflowAgentFile { .. }));
}

#[test]
fn the_run_dir_lives_under_craft_agent_workflows() {
    let dir = workflow_run_dir(Path::new("/ws"), "abcd1234-ef567890");
    assert_eq!(
        dir,
        Path::new("/ws/.craft-agent/workflows/abcd1234-ef567890")
    );
}

#[test]
fn the_production_mount_honors_the_step_model() {
    let config = craft_app::config::EnvConfig {
        provider: craft_app::config::ProviderKind::OpenAi,
        model: "base".to_string(),
        api_key: "k".to_string(),
        base_url: "http://localhost:1/v1".to_string(),
        anthropic_auth: craft_providers::anthropic::AuthScheme::ApiKey,
        vertex_project: String::new(),
        vertex_location: String::new(),
        gh_token: None,
    };
    let mount = craft_app::workflow_cmd::production_mount(config);
    mount("step-model").expect("mounting a step model is config-only");
}

#[test]
fn the_sim_adoption_fixture_parses_through_the_plan_entry() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let plan = WorkflowPlan::parse_file(&root.join("sim/workflows/adoption-report-01.md"))
        .expect("the sim fixture parses");
    assert_eq!(plan.name, "adoption-report-01");
    assert_eq!(plan.steps.len(), 3);
    let args = plan::resolve_args(&plan, &Default::default()).expect("defaults resolve");
    assert_eq!(args.get("window"), Some(&ArgValue::Str("24h".into())));
}
