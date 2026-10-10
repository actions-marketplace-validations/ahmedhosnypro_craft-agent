//! `run`: mount a backend, drive the runner, stream events, exit.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use craft_runtime::{Runner, RuntimeEvent, profile};
use craft_tools::ToolLimits;

use crate::agent_select::{RunWatch, resolve};
use crate::cli::RunCli;
use crate::config::{env_from_process, mount_backend, resolve_env_config};
use crate::error::AppError;
use crate::event_writer;
use crate::prompt::read_prompt;
use crate::redaction;
use crate::stream_json;
use crate::subagent_setup::{self, build_run_config, default_session_path};
use crate::summary;

/// Spawns the run-event writer; on failure the run still ends with its
/// terminal stream line, written straight to stdout.
fn spawn_run_writer(
    format: crate::cli::OutputFormat,
    redactor: &craft_redact::Redactor,
) -> Result<(event_writer::EventSink, event_writer::EventWriterHandle), AppError> {
    match event_writer::spawn_event_writer(format) {
        Ok(pair) => Ok(pair),
        Err(error) => {
            stream_json::write_terminal_line(
                std::io::stdout().lock(),
                format,
                redaction::terminal_detail(&error, redactor),
            );
            Err(error)
        }
    }
}

pub fn execute_run(run: RunCli) -> Result<i32, AppError> {
    let format = run.output_format;
    let redactor = Arc::new(craft_redact::Redactor::from_env());
    let env = env_from_process();
    redaction::emit_add_mask(&redactor, &env);
    execute_run_inner(run, &redactor).inspect_err(|error| {
        stream_json::write_terminal_line(
            std::io::stdout().lock(),
            format,
            redaction::terminal_detail(error, &redactor),
        );
    })
}

fn execute_run_inner(run: RunCli, redactor: &Arc<craft_redact::Redactor>) -> Result<i32, AppError> {
    let prompt = read_prompt(&run.prompt)?;
    let env = env_from_process();
    let config = resolve_env_config(&env)?;
    let backend = mount_backend(&config)?;
    let workspace_root =
        std::env::current_dir().map_err(|source| AppError::WorkspaceRoot { source })?;
    let session_path = default_session_path(&workspace_root)?;
    let agent = match resolve(&run, &workspace_root) {
        Ok(agent) => agent,
        Err(error) => {
            stream_json::write_terminal_line(
                std::io::stdout().lock(),
                run.output_format,
                redaction::terminal_detail(&error, redactor),
            );
            eprintln!("craft-agent: {error}");
            return Ok(1);
        }
    };
    let run_config = build_run_config(&run, agent.as_ref(), workspace_root.clone(), session_path);
    let session_path = run_config.session_path.clone();
    let turn_cap = run_config.max_turns;
    let token_budget = run_config.max_total_tokens;
    let subagents = subagent_setup::mount(&run_config, &workspace_root, backend.clone())?;
    let run_registry =
        profile::build_registry(agent.as_ref(), ToolLimits::default(), subagents.clone());
    let mut run_config = run_config;
    run_config.redactor = redactor.clone();
    run_config.subagents = subagents;
    let runner = Runner::new(backend, run_registry, run_config);
    let (sink, writer) = spawn_run_writer(run.output_format, redactor)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| AppError::AsyncRuntime { source })?;
    let outcome = {
        let deadline = agent.as_ref().and_then(|agent| agent.timeout_secs);
        let watch = Arc::new(Mutex::new(RunWatch::default()));
        let driven = async {
            let run_future = runner.run(&prompt, |mut event| {
                craft_runtime::redact_event(&mut event, redactor);
                RunWatch::record(&watch, &event);
                if matches!(event, RuntimeEvent::RunFinished { .. }) {
                    sink.finish(event);
                } else {
                    sink.emit(event);
                }
            });
            tokio::pin!(run_future);
            let kill = async {
                match deadline {
                    Some(secs) => tokio::time::sleep(Duration::from_secs(secs)).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::pin!(kill);
            tokio::select! {
                biased;
                outcome = &mut run_future => outcome,
                () = &mut kill => {
                    let (outcome, event) =
                        RunWatch::expire(&watch, deadline.unwrap_or(0), session_path.as_deref());
                    sink.finish(event);
                    outcome
                }
            }
        };
        runtime.block_on(event_writer::drive(driven, writer, session_path.clone()))
    };
    summary::emit_run_summary(
        &env,
        run.output_format,
        &outcome,
        summary::SummaryFacts {
            mission: "run",
            model: config.model.clone(),
            provider: summary::provider_name(config.provider).to_string(),
            turn_cap: Some(turn_cap),
            token_budget: Some(token_budget),
            rows: redaction::summary_rows(redactor),
        },
    );
    Ok(outcome.exit_code())
}
