//! RED corpus: workflow plan location — flags, env knobs, name
//! resolution, and discovery-dir rules.

mod workflow_common;

use std::fs;

use craft_app::cli::WorkflowCli;
use craft_app::error::AppError;
use craft_app::workflow_locate::{WORKFLOW_FILE_KNOB, WORKFLOWS_DIR_KNOB, locate_plan};

use workflow_common::{TINY_PLAN, env_map, parse_workflow_cli, plan_file, repo};

#[test]
fn no_selection_names_the_flags_and_the_knob() {
    let workspace = repo("locate-none").expect("repo");
    let cli = parse_workflow_cli(&[]).expect("bare parse");
    let err = locate_plan(&cli, &env_map(&[]), &workspace).expect_err("nothing selected");
    assert!(matches!(err, AppError::WorkflowNotSelected));
    let text = err.to_string();
    assert!(text.contains("--workflow-file"), "{text}");
    assert!(text.contains(WORKFLOW_FILE_KNOB), "{text}");
}

#[test]
fn the_file_flag_wins_over_everything() {
    let workspace = repo("locate-flag").expect("repo");
    let plan = plan_file(&workspace, TINY_PLAN).expect("plan written");
    let cli = WorkflowCli {
        workflow_file: Some(plan.clone()),
        ..parse_workflow_cli(&[]).expect("bare parse")
    };
    let found = locate_plan(&cli, &env_map(&[]), &workspace).expect("flag wins");
    assert_eq!(found, plan);
}

#[test]
fn the_env_file_knob_resolves_when_no_flag_is_given() {
    let workspace = repo("locate-envfile").expect("repo");
    let plan = plan_file(&workspace, TINY_PLAN).expect("plan written");
    let cli = parse_workflow_cli(&[]).expect("bare parse");
    let found = locate_plan(
        &cli,
        &env_map(&[(WORKFLOW_FILE_KNOB, plan.to_str().expect("utf8"))]),
        &workspace,
    )
    .expect("env knob resolves");
    assert_eq!(found, plan);
}

#[test]
fn a_missing_env_file_names_the_knob() {
    let workspace = repo("locate-envmissing").expect("repo");
    let cli = parse_workflow_cli(&[]).expect("bare parse");
    let missing = workspace.join("nope.md");
    let err = locate_plan(
        &cli,
        &env_map(&[(WORKFLOW_FILE_KNOB, missing.to_str().expect("utf8"))]),
        &workspace,
    )
    .expect_err("missing env file must fail");
    assert!(matches!(err, AppError::WorkflowEnvFileMissing { .. }));
    assert!(err.to_string().contains(WORKFLOW_FILE_KNOB), "{err}");
}

#[test]
fn a_name_resolves_by_plan_name_not_file_stem() {
    let workspace = repo("locate-name").expect("repo");
    let dir = workspace.join(".craft-agent").join("workflows");
    let path = plan_file(&dir, TINY_PLAN).expect("plan written");
    let cli = parse_workflow_cli(&["--workflow", "tiny-flow"]).expect("parses");
    let found = locate_plan(&cli, &env_map(&[]), &workspace).expect("name resolves");
    assert_eq!(found, path);
}

#[test]
fn an_unknown_name_lists_what_is_available() {
    let workspace = repo("locate-unknown").expect("repo");
    let dir = workspace.join(".craft-agent").join("workflows");
    plan_file(&dir, TINY_PLAN).expect("plan written");
    let cli = parse_workflow_cli(&["--workflow", "ghost"]).expect("parses");
    let err = locate_plan(&cli, &env_map(&[]), &workspace).expect_err("unknown name");
    let AppError::WorkflowUnknown {
        name,
        dir,
        available,
        ..
    } = err
    else {
        panic!("expected an unknown-workflow error, got {err:?}");
    };
    assert_eq!(name, "ghost");
    assert_eq!(dir, workspace.join(".craft-agent").join("workflows"));
    assert_eq!(available, "tiny-flow");
}

#[test]
fn an_invalid_plan_file_is_skipped_not_fatal() {
    let workspace = repo("locate-invalid").expect("repo");
    let dir = workspace.join(".craft-agent").join("workflows");
    fs::write(dir.join("broken.md"), "not a plan").expect("broken file written");
    let good = plan_file(&dir, TINY_PLAN).expect("plan written");
    let cli = parse_workflow_cli(&["--workflow", "tiny-flow"]).expect("parses");
    let found = locate_plan(&cli, &env_map(&[]), &workspace).expect("the good file still resolves");
    assert_eq!(found, good);
}

#[test]
fn a_missing_env_dir_is_a_named_error() {
    let workspace = repo("locate-envdir").expect("repo");
    let cli = parse_workflow_cli(&["--workflow", "x"]).expect("parses");
    let err = locate_plan(
        &cli,
        &env_map(&[(WORKFLOWS_DIR_KNOB, "/nonexistent-workflow-dir")]),
        &workspace,
    )
    .expect_err("a set-but-missing dir must fail");
    assert!(matches!(err, AppError::WorkflowDirEnvMissing { .. }));
    assert!(err.to_string().contains(WORKFLOWS_DIR_KNOB), "{err}");
}

#[test]
fn no_discovery_dir_names_the_knob() {
    let workspace = repo("locate-nodir").expect("repo");
    fs::remove_dir_all(workspace.join(".craft-agent").join("workflows")).expect("dir removed");
    let cli = parse_workflow_cli(&["--workflow", "x"]).expect("parses");
    let err = locate_plan(&cli, &env_map(&[]), &workspace).expect_err("no dir must fail");
    assert!(matches!(err, AppError::WorkflowDirMissing { .. }));
    assert!(err.to_string().contains(WORKFLOWS_DIR_KNOB), "{err}");
}

#[test]
fn discovery_walks_up_to_the_git_root() {
    let workspace = repo("locate-walk").expect("repo");
    let dir = workspace.join(".craft-agent").join("workflows");
    let path = plan_file(&dir, TINY_PLAN).expect("plan written");
    let start = workspace.join("nested").join("deep");
    fs::create_dir_all(&start).expect("nested dir");
    let cli = parse_workflow_cli(&["--workflow", "tiny-flow"]).expect("parses");
    let found = locate_plan(&cli, &env_map(&[]), &start).expect("walk-up resolves");
    assert_eq!(found, path);
}

#[test]
fn the_env_dir_knob_replaces_discovery() {
    let workspace = repo("locate-envdir-ok").expect("repo");
    let elsewhere = workspace.join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("dir");
    let path = plan_file(&elsewhere, TINY_PLAN).expect("plan written");
    let cli = parse_workflow_cli(&["--workflow", "tiny-flow"]).expect("parses");
    let found = locate_plan(
        &cli,
        &env_map(&[(WORKFLOWS_DIR_KNOB, elsewhere.to_str().expect("utf8"))]),
        &workspace,
    )
    .expect("env dir resolves");
    assert_eq!(found, path);
}
