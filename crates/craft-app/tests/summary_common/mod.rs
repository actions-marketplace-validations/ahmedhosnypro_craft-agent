//! Shared builders for the run-summary corpora.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use craft_app::config::EnvMap;
use craft_app::summary::SummaryFacts;
use craft_app::{CancelCategory, ExitStatus, RunOutcome, Usage};
use craft_core::types::{ChatMessage, ToolCall};
use craft_runtime::session::Session;
use craft_workflow::status::{
    RunConclusion, SkipReason, StepConclusion, StepReport, StepStatus, StepUsage, WorkflowReport,
};

pub fn usage() -> Usage {
    Usage {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: 10,
        cache_write_tokens: 0,
        reasoning_tokens: 5,
        incomplete: false,
    }
}

pub fn outcome(category: CancelCategory, exit: ExitStatus, detail: &str) -> RunOutcome {
    RunOutcome {
        exit,
        category,
        detail: detail.to_string(),
        usage: usage(),
        turns: 2,
        session_path: Some(PathBuf::from("/tmp/sessions/record.jsonl")),
    }
}

pub fn facts() -> SummaryFacts {
    SummaryFacts {
        mission: "run",
        model: String::from("test-model"),
        provider: String::from("openai"),
        turn_cap: Some(40),
        token_budget: Some(4_000_000),
        rows: Vec::new(),
    }
}

pub fn env_with(target: &Path) -> EnvMap {
    HashMap::from([
        (
            String::from("GITHUB_STEP_SUMMARY"),
            target.display().to_string(),
        ),
        (
            String::from("OPENAI_API_KEY"),
            String::from("sk-sentinel-never-in-summaries"),
        ),
    ])
}

pub fn user(text: &str) -> ChatMessage {
    ChatMessage::User {
        text: text.to_string(),
        images: Vec::new(),
    }
}

pub fn assistant_text(text: &str) -> ChatMessage {
    ChatMessage::Assistant {
        text: text.to_string(),
        tool_calls: Vec::new(),
        native_blocks: None,
    }
}

pub struct ScriptedCall {
    name: String,
}

impl ScriptedCall {
    pub fn named(name: &str) -> ScriptedCall {
        ScriptedCall {
            name: name.to_string(),
        }
    }
}

pub fn assistant_call(call: ScriptedCall) -> ChatMessage {
    ChatMessage::Assistant {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: String::from("c1"),
            name: call.name,
            arguments: String::from("{}"),
        }],
        native_blocks: None,
    }
}

pub fn tool_result(content: &str) -> ChatMessage {
    ChatMessage::Tool {
        call_id: String::from("c1"),
        content: content.to_string(),
        is_error: false,
        images: Vec::new(),
    }
}

pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("craft-app-summary-{name}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub async fn session_file(name: &str, messages: &[ChatMessage]) -> Result<PathBuf, String> {
    let path = scratch(name).join("session.jsonl");
    session_file_at(&path, messages).await?;
    Ok(path)
}

pub async fn session_file_at(path: &Path, messages: &[ChatMessage]) -> Result<(), String> {
    let mut session = Session::open(path.to_path_buf()).map_err(|err| err.to_string())?;
    for message in messages {
        session
            .append(message.clone())
            .await
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

pub fn run_session() -> Vec<ChatMessage> {
    vec![
        user("do the task"),
        assistant_call(ScriptedCall::named("gh")),
        tool_result(
            "To https://github.com/octo/app.git\n \
             9f7c1a2..3b8d4e5  main -> main\nopened https://github.com/octo/app/pull/7",
        ),
        assistant_text("done — see https://github.com/octo/app/pull/7 for the PR\nsecond line"),
    ]
}

pub fn report() -> WorkflowReport {
    WorkflowReport {
        run_id: String::from("r1"),
        name: String::from("suite"),
        conclusion: RunConclusion::Failed {
            step: String::from("scan"),
            conclusion: StepConclusion::MaxTurns,
        },
        exit_code: 2,
        failed_step: Some(String::from("scan")),
        steps: vec![
            StepReport {
                id: String::from("scan"),
                status: StepStatus::Settled {
                    conclusion: StepConclusion::MaxTurns,
                    usage: StepUsage {
                        turns: 2,
                        input_tokens: 80,
                        output_tokens: 10,
                        reasoning_tokens: 0,
                    },
                    final_message: String::from("scan died at the cap"),
                },
            },
            StepReport {
                id: String::from("report"),
                status: StepStatus::Skipped {
                    reason: SkipReason::DependencyFailed {
                        dep: String::from("scan"),
                    },
                },
            },
        ],
        total_usage: StepUsage {
            turns: 2,
            input_tokens: 80,
            output_tokens: 10,
            reasoning_tokens: 0,
        },
    }
}
