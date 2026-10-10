//! The workflow step executor: one bounded runtime run per step over the
//! pre-resolved agent profile, mapping the run outcome onto the engine's
//! step vocabulary.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use craft_core::agents::AgentFile;
use craft_core::provider::Provider;
use craft_runtime::{
    ChildKnobs, DEFAULT_REQUEST_TOKENS, ExitStatus, Runner, RuntimeEvent, prepare_child,
};
use craft_tools::ToolLimits;
use craft_workflow::status::{StepConclusion, StepUsage};
use craft_workflow::{StepExecutor, StepInputs, StepOutcome, WorkflowError};

use crate::prompt::DEFAULT_SYSTEM_PROMPT;

/// Mounts the provider one step runs against, for its model id.
pub type StepMount<P> = Arc<dyn Fn(&str) -> Result<Arc<P>, String> + Send + Sync>;

/// The production `StepExecutor`: the caller injects the provider
/// mount, the pre-resolved step profiles, and the run-dir paths.
pub struct WorkflowExecutor<P: Provider + Send + Sync> {
    mount: StepMount<P>,
    default_model: String,
    workspace_root: PathBuf,
    sessions_dir: PathBuf,
    step_token_cap: u64,
    idle_timeout: Option<Duration>,
    step_attempts: Option<u32>,
    agents: BTreeMap<String, AgentFile>,
}

impl<P: Provider + Send + Sync> WorkflowExecutor<P> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mount: StepMount<P>,
        default_model: String,
        workspace_root: PathBuf,
        sessions_dir: PathBuf,
        step_token_cap: u64,
        idle_timeout: Option<Duration>,
        step_attempts: Option<u32>,
        agents: BTreeMap<String, AgentFile>,
    ) -> Self {
        Self {
            mount,
            default_model,
            workspace_root,
            sessions_dir,
            step_token_cap,
            idle_timeout,
            step_attempts,
            agents,
        }
    }
}

impl<P: Provider + Send + Sync> StepExecutor for WorkflowExecutor<P> {
    fn execute(
        &self,
        step: StepInputs,
    ) -> impl Future<Output = Result<StepOutcome, WorkflowError>> + Send {
        let mount = self.mount.clone();
        let default_model = self.default_model.clone();
        let workspace_root = self.workspace_root.clone();
        let session_path = self.sessions_dir.join(format!("{}.jsonl", step.id));
        let step_token_cap = self.step_token_cap;
        let idle_timeout = self.idle_timeout;
        let step_attempts = self.step_attempts;
        let agent = self.agents.get(&step.id).cloned();
        async move {
            let launch = |detail: String| WorkflowError::StepLaunch {
                step: step.id.clone(),
                detail,
            };
            let Some(agent) = agent else {
                return Err(launch(String::from(
                    "no agent profile resolved for the step",
                )));
            };
            let model = step.model.clone().unwrap_or(default_model);
            let provider = mount(&model).map_err(launch)?;
            let child = prepare_child(
                &agent,
                DEFAULT_SYSTEM_PROMPT,
                None,
                ChildKnobs {
                    id: step.id.clone(),
                    workspace_root,
                    max_turns: step.max_turns,
                    max_total_tokens: step_token_cap,
                    per_request_tokens: DEFAULT_REQUEST_TOKENS,
                    limits: ToolLimits::default(),
                    session_path: Some(session_path),
                    idle_timeout,
                    step_attempts,
                    spawn: None,
                },
            )
            .map_err(launch)?;
            let runner = Runner::new(provider, child.registry, child.config);
            let mut final_text = String::new();
            let outcome = runner
                .run(&step.prompt, |event| match event {
                    RuntimeEvent::TurnStarted { .. } | RuntimeEvent::TurnRetried { .. } => {
                        final_text.clear();
                    }
                    RuntimeEvent::AssistantDelta { text, .. } => final_text.push_str(&text),
                    _ => {}
                })
                .await;
            let conclusion = map_conclusion(outcome.exit);
            // A successful run always carries text: the fold rejects
            // zero-content turns, so the last turn's text is the handoff.
            let final_message = if conclusion.is_success() {
                final_text
            } else {
                outcome.detail
            };
            Ok(StepOutcome {
                conclusion,
                usage: map_usage(outcome.turns, &outcome.usage),
                final_message,
            })
        }
    }
}

/// ExitStatus and StepConclusion share the run exit taxonomy 1:1.
pub fn map_conclusion(exit: ExitStatus) -> StepConclusion {
    match exit {
        ExitStatus::Ok => StepConclusion::Success,
        ExitStatus::Error => StepConclusion::Error,
        ExitStatus::MaxTurns => StepConclusion::MaxTurns,
        ExitStatus::Stationarity => StepConclusion::Stationarity,
        ExitStatus::Budget => StepConclusion::Budget,
    }
}

/// Turn and token accounting onto the engine's buckets; the run's cache
/// buckets have no step counterpart.
pub fn map_usage(turns: u32, usage: &craft_core::types::Usage) -> StepUsage {
    StepUsage {
        turns: u64::from(turns),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_tokens: usage.reasoning_tokens,
    }
}
