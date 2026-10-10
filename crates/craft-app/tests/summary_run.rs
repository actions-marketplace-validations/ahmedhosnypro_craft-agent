//! Run-summary corpus: a real runner session over a scripted provider
//! drives the summary seam end to end.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use craft_app::cli::OutputFormat;
use craft_app::config::EnvMap;
use craft_app::summary::{self, SummaryFacts};
use craft_core::error::ProviderError;
use craft_core::events::ModelEvent;
use craft_core::provider::{ModelEventStream, Provider, boxed};
use craft_core::types::{ModelRequest, StopReason, Usage};
use craft_runtime::{RunConfig, Runner};
use craft_tools::event::ToolEvent;
use craft_tools::{Registry, Schema, Tool, ToolCtx, ToolLimits, ToolResult, ToolStream};
use futures::stream::{self, StreamExt};

struct ProbeTool {
    output: String,
}

impl Tool for ProbeTool {
    fn name(&self) -> &'static str {
        "probe"
    }
    fn description(&self) -> &'static str {
        "returns one fixed result"
    }
    fn params_schema(&self) -> Schema {
        Schema::object("fixed result", &[])
    }
    fn execute(&self, _args: serde_json::Value, _ctx: &ToolCtx) -> ToolStream {
        stream::iter(vec![Ok(ToolEvent::Terminal(ToolResult {
            output: self.output.clone(),
        }))])
        .boxed()
    }
}

type Turn = Vec<Result<ModelEvent, ProviderError>>;

struct ScriptedProvider {
    turns: Mutex<VecDeque<Turn>>,
}

impl Provider for ScriptedProvider {
    async fn stream(&self, _request: ModelRequest) -> Result<ModelEventStream, ProviderError> {
        let turn = self
            .turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .unwrap_or_default();
        Ok(boxed(stream::iter(turn)))
    }
}

fn zero_usage() -> Usage {
    Usage {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        incomplete: false,
    }
}

#[tokio::test]
async fn a_real_run_summary_carries_its_deliverables() {
    let dir = PathBuf::from(format!(
        "{}/craft-app-summary-run-{}",
        std::env::temp_dir().display(),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let session_path = dir.join("session.jsonl");
    let target = dir.join("summary.md");

    let provider = Arc::new(ScriptedProvider {
        turns: Mutex::new(VecDeque::from(vec![
            vec![
                Ok(ModelEvent::ToolCallDelta {
                    index: 0,
                    id: Some(String::from("c1")),
                    name: Some(String::from("probe")),
                    arguments: String::from("{}"),
                }),
                Ok(ModelEvent::TurnFinished {
                    stop_reason: StopReason::ToolUse,
                    usage: zero_usage(),
                }),
            ],
            vec![
                Ok(ModelEvent::TextDelta {
                    text: String::from("opened the PR and finished"),
                }),
                Ok(ModelEvent::TurnFinished {
                    stop_reason: StopReason::EndTurn,
                    usage: zero_usage(),
                }),
            ],
        ])),
    });
    let mut registry = Registry::new(ToolLimits::default());
    registry.register(Box::new(ProbeTool {
        output: String::from("created https://github.com/octo/app/pull/9 — see the diff"),
    }));
    let mut config = RunConfig::new(dir.clone());
    config.session_path = Some(session_path);
    config.max_turns = 4;
    config.max_total_tokens = 1_000_000;
    let runner = Runner::new(provider, registry, config);
    let outcome = runner.run("open the PR", |_| {}).await;
    assert_eq!(outcome.exit_code(), 0, "the scripted run completes");

    let env: EnvMap = HashMap::from([
        (
            String::from("GITHUB_STEP_SUMMARY"),
            target.display().to_string(),
        ),
        (
            String::from("OPENAI_API_KEY"),
            String::from("sk-sentinel-never-in-summaries"),
        ),
    ]);
    summary::emit_run_summary(
        &env,
        OutputFormat::Plain,
        &outcome,
        SummaryFacts {
            mission: "run",
            model: String::from("test-model"),
            provider: String::from("openai"),
            turn_cap: Some(4),
            token_budget: Some(1_000_000),
            rows: Vec::new(),
        },
    );
    let written = std::fs::read_to_string(&target).expect("summary written");
    assert!(
        written.contains("## craft-agent — completed (exit 0)"),
        "{written}"
    );
    assert!(
        written.contains("- [PR #9 (octo/app)](https://github.com/octo/app/pull/9)"),
        "{written}"
    );
    assert!(written.contains("### Final message"));
    assert!(written.contains("> opened the PR and finished"));
    assert!(written.contains("Transcript tail"));
    assert!(!written.contains("sk-sentinel-never-in-summaries"));
}
