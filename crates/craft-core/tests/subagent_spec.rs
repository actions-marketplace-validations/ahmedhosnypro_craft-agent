//! Spawn-argument law: the single and batch shapes, their hostile-input
//! bounds, and the strict camelCase deserialization the spawn tools
//! parse against.

use craft_core::subagent::spec::SpawnBudget;
use craft_core::subagent::{SpawnBatch, SpawnSpec, SubagentError};

fn no_budget() -> SpawnBudget {
    SpawnBudget::default()
}

fn turns(value: u32) -> SpawnBudget {
    SpawnBudget {
        max_turns: Some(value),
        ..SpawnBudget::default()
    }
}

fn tokens(value: u64) -> SpawnBudget {
    SpawnBudget {
        max_tokens: Some(value),
        ..SpawnBudget::default()
    }
}

fn timeout(value: u64) -> SpawnBudget {
    SpawnBudget {
        timeout_secs: Some(value),
        ..SpawnBudget::default()
    }
}

fn batch(template: &str, items: Vec<String>) -> SpawnBatch {
    SpawnBatch {
        agent: "repo-scout".to_string(),
        prompt_template: template.to_string(),
        items,
        max_turns: None,
        max_tokens: None,
        timeout_secs: None,
    }
}

#[test]
fn a_minimal_spawn_validates() {
    let spec = SpawnSpec::new("repo-scout", "find the seam", no_budget()).expect("valid");
    assert_eq!(spec.agent, "repo-scout");
    assert_eq!(spec.prompt, "find the seam");
    assert!(spec.max_turns.is_none());
    assert!(spec.max_tokens.is_none());
    assert!(spec.timeout_secs.is_none());
}

#[test]
fn every_argument_is_bounded() {
    assert!(matches!(
        SpawnSpec::new("", "task", no_budget()),
        Err(SubagentError::EmptyAgent)
    ));
    assert!(matches!(
        SpawnSpec::new(&"a".repeat(65), "task", no_budget()),
        Err(SubagentError::AgentNameSize { bytes: 65, .. })
    ));
    assert!(SpawnSpec::new(&"a".repeat(64), "task", no_budget()).is_ok());
    assert!(matches!(
        SpawnSpec::new("repo-scout", "", no_budget()),
        Err(SubagentError::PromptSize { bytes: 0, .. })
    ));
    assert!(matches!(
        SpawnSpec::new("repo-scout", &"a".repeat(65_537), no_budget()),
        Err(SubagentError::PromptSize { bytes: 65_537, .. })
    ));
    assert!(SpawnSpec::new("repo-scout", &"a".repeat(65_536), no_budget()).is_ok());
    assert!(matches!(
        SpawnSpec::new("repo-scout", "t", turns(0)),
        Err(SubagentError::MaxTurns { value: 0, .. })
    ));
    assert!(SpawnSpec::new("repo-scout", "t", turns(10_000)).is_ok());
    assert!(matches!(
        SpawnSpec::new("repo-scout", "t", turns(10_001)),
        Err(SubagentError::MaxTurns { value: 10_001, .. })
    ));
    assert!(matches!(
        SpawnSpec::new("repo-scout", "t", tokens(0)),
        Err(SubagentError::MaxTokens { value: 0 })
    ));
    assert!(SpawnSpec::new("repo-scout", "t", tokens(1)).is_ok());
    assert!(SpawnSpec::new("repo-scout", "t", timeout(0)).is_ok());
    assert!(SpawnSpec::new("repo-scout", "t", timeout(21_600)).is_ok());
    assert!(matches!(
        SpawnSpec::new("repo-scout", "t", timeout(21_601)),
        Err(SubagentError::TimeoutSecs { value: 21_601, .. })
    ));
}

#[test]
fn args_deserialize_strictly() {
    let spec: SpawnSpec = serde_json::from_str(
        r#"{"agent":"repo-scout","prompt":"task","maxTurns":15,"maxTokens":1000}"#,
    )
    .expect("camelCase args parse");
    assert_eq!(spec.max_turns, Some(15));
    assert_eq!(spec.max_tokens, Some(1000));
    assert!(spec.timeout_secs.is_none());
    let missing = serde_json::from_str::<SpawnSpec>(r#"{"agent":"a"}"#);
    assert!(missing.is_err());
    let unknown = serde_json::from_str::<SpawnSpec>(r#"{"agent":"a","prompt":"b","prior":[]}"#);
    assert!(unknown.is_err());
    let negative = serde_json::from_str::<SpawnSpec>(r#"{"agent":"a","prompt":"b","maxTurns":-1}"#);
    assert!(negative.is_err());
    let hostile: SpawnSpec = serde_json::from_str(r#"{"agent":"a","prompt":"b","maxTurns":99999}"#)
        .expect("shape parses");
    assert!(hostile.validate().is_err());
    let malformed = serde_json::from_str::<SpawnBatch>(r#"{"agent":"a"}"#);
    assert!(malformed.is_err());
}

#[test]
fn a_batch_expands_into_member_specs() {
    let specs = batch(
        "Scan {{item}} and report the seams",
        vec!["photocraft".into(), "filmcraft".into()],
    )
    .expand()
    .expect("batch validates");
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].prompt, "Scan photocraft and report the seams");
    assert_eq!(specs[1].prompt, "Scan filmcraft and report the seams");
    assert_eq!(specs[0].agent, "repo-scout");
    assert!(specs[1].max_tokens.is_none());
    let two_placeholders = batch(
        "Compare {{item}} against {{item}} history",
        vec!["photocraft".into()],
    );
    assert!(matches!(
        two_placeholders.expand(),
        Err(SubagentError::BatchItems { count: 1, .. })
    ));
    let shared = SpawnBatch {
        max_turns: Some(15),
        max_tokens: Some(50_000),
        timeout_secs: Some(600),
        ..batch("Scan {{item}}", vec!["a".into(), "b".into()])
    };
    let specs = shared.expand().expect("batch validates");
    assert_eq!(specs[0].max_turns, Some(15));
    assert_eq!(specs[1].max_tokens, Some(50_000));
    assert_eq!(specs[1].timeout_secs, Some(600));
}

#[test]
fn batch_shapes_are_bounded() {
    let one = vec![String::from("a")];
    assert!(matches!(
        batch("{{item}}", one).expand(),
        Err(SubagentError::BatchItems { count: 1, .. })
    ));
    let many: Vec<String> = (0..33).map(|n| n.to_string()).collect();
    assert!(matches!(
        batch("{{item}}", many).expand(),
        Err(SubagentError::BatchItems { count: 33, .. })
    ));
    let thirty_two: Vec<String> = (0..32).map(|n| n.to_string()).collect();
    assert!(batch("{{item}}", thirty_two).expand().is_ok());
    assert!(matches!(
        batch("no placeholder", vec!["a".into(), "b".into()]).expand(),
        Err(SubagentError::MissingPlaceholder)
    ));
    let big_template = format!("{}{{{{item}}}}", "t".repeat(65_529));
    assert!(matches!(
        batch(&big_template, vec!["a".into(), "b".into()]).expand(),
        Err(SubagentError::TemplateSize { bytes: 65_537, .. })
    ));
    let big_item = vec![String::from("a"), "x".repeat(65_537)];
    assert!(matches!(
        batch("{{item}}", big_item).expand(),
        Err(SubagentError::ItemSize {
            index: 2,
            bytes: 65_537,
            ..
        })
    ));
    let heavy: Vec<String> = (0..17).map(|_| "y".repeat(65_000)).collect();
    assert!(matches!(
        batch("{{item}}", heavy).expand(),
        Err(SubagentError::BatchBytes {
            bytes: 1_105_000,
            ..
        })
    ));
}

#[test]
fn filled_prompts_must_be_distinct_and_bounded() {
    let dups = vec!["a".into(), "b".into(), "a".into()];
    let err = batch("Scan {{item}}", dups)
        .expand()
        .expect_err("duplicates");
    assert_eq!(
        err,
        SubagentError::DuplicatePrompts {
            indices: vec![1, 3]
        }
    );
    assert_eq!(err.to_string(), "duplicate filled prompts at items [1, 3]");
    let template = format!("{} {{{{item}}}}", "t".repeat(65_000));
    let filled = vec!["x".repeat(1_000), "y".into()];
    assert!(matches!(
        batch(&template, filled).expand(),
        Err(SubagentError::FilledPromptSize {
            index: 1,
            bytes: 66_001,
            ..
        })
    ));
    let distinct = vec!["a".into(), "b".into()];
    assert!(batch("Scan {{item}}", distinct).expand().is_ok());
}

#[test]
fn the_refusal_text_matches_the_tool_contract() {
    let err = batch("no placeholder", vec!["a".into(), "b".into()])
        .expand()
        .expect_err("placeholder");
    assert_eq!(
        err.to_string(),
        "`promptTemplate` must contain the {{item}} placeholder"
    );
    let err = batch("{{item}}", vec!["one".into()])
        .expand()
        .expect_err("few items");
    assert_eq!(err.to_string(), "`items` must be 2..=32 entries, got 1");
    let err = batch("report on {{item}}", vec!["same".into(), "same".into()])
        .expand()
        .expect_err("duplicates");
    assert_eq!(err.to_string(), "duplicate filled prompts at items [1, 2]");
    let template = format!("{} {{{{item}}}}", "t".repeat(65_000));
    let err = batch(&template, vec!["x".repeat(1_000), "y".into()])
        .expand()
        .expect_err("filled over cap");
    assert_eq!(
        err.to_string(),
        "the filled prompt for item 1 is 66001 bytes, over 65536"
    );
    let err = batch("{{item}}", vec![String::new(), "b".into()])
        .expand()
        .expect_err("empty fill");
    assert_eq!(
        err,
        SubagentError::FilledPromptSize {
            index: 1,
            bytes: 0,
            max: 65_536,
        }
    );
}
