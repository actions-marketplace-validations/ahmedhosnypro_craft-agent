//! The scripted model the simulation drives: one Provider that routes on
//! the prompt marker and replays the per-PR pipeline's turn shapes — the
//! detached spawn that returns at admission, the polls that collect each
//! PR as it lands, and the stage-2 children an advancing PR gets while
//! its siblings are still in their first stage.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use craft_core::error::ProviderError;
use craft_core::events::ModelEvent;
use craft_core::provider::{ModelEventStream, Provider, boxed};
use craft_core::types::{ChatMessage, ModelRequest, StopReason, Usage};
use futures::stream;

/// The scripted model: routes on the user prompt's marker, records every
/// request, and counts how many malformed judge verdicts it emitted (the
/// seat-repair proof).
pub struct SimProvider {
    seen: Mutex<Vec<ModelRequest>>,
    malformed: AtomicU32,
    /// The workspace root: the orchestrator's turns read the ledger to
    /// learn which PRs still need their stage-2 child, exactly as the
    /// real orchestrator does instead of trusting one poll's envelopes.
    workspace: std::path::PathBuf,
}

impl SimProvider {
    pub fn new(workspace: std::path::PathBuf) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            malformed: AtomicU32::new(0),
            workspace,
        })
    }

    /// The actionable shards whose verdict landed but whose package does
    /// not exist yet, in dispatch order: the ledger's stage-2 owed list.
    fn staged(&self) -> Vec<String> {
        PRS.iter()
            .filter(|shard| {
                let number = shard.split('|').next().unwrap_or("x");
                let class = shard.split('|').nth(1).unwrap_or("irrelevant");
                if !matches!(class, "rev-sync" | "hoist" | "port") {
                    return false;
                }
                let verdict = self
                    .workspace
                    .join("run-ledger/classifier")
                    .join(format!("{number}.verdict"));
                let package = self.package_path(shard);
                verdict.is_file() && !package.is_file()
            })
            .map(|shard| shard.to_string())
            .collect()
    }

    /// Where a PR's stage-2 package lands: the applier and the filer
    /// both write under run-ledger/drafts/<number>.md in the simulation.
    fn package_path(&self, shard: &str) -> std::path::PathBuf {
        let number = shard.split('|').next().unwrap_or("x").trim();
        self.workspace
            .join("run-ledger/drafts")
            .join(format!("{number}.md"))
    }

    pub fn calls(&self) -> usize {
        self.seen.lock().expect("seen").len()
    }

    pub fn malformed(&self) -> u32 {
        self.malformed.load(Ordering::Relaxed)
    }

    /// The last user message in full — the routing key.
    fn prompt_of(request: &ModelRequest) -> String {
        request
            .messages
            .last()
            .map(|message| match message.as_ref() {
                ChatMessage::User { text, .. } => text.clone(),
                _ => String::new(),
            })
            .unwrap_or_default()
    }

    /// The last tool result's content — the other routing key. A turn
    /// that follows a detached spawn's terminal or a poll's envelopes
    /// sees a tool result, not a user prompt.
    fn tool_result_of(request: &ModelRequest) -> String {
        request
            .messages
            .iter()
            .rev()
            .find_map(|message| match message.as_ref() {
                ChatMessage::Tool { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

impl Provider for SimProvider {
    async fn stream(&self, request: ModelRequest) -> Result<ModelEventStream, ProviderError> {
        self.seen.lock().expect("seen").push(request.clone());
        let prompt = Self::prompt_of(&request);
        let tool_result = Self::tool_result_of(&request);
        let staged = self.staged();
        let events = route(&prompt, &tool_result, &staged, &self.malformed);
        Ok(boxed(stream::iter(events)))
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 3,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        incomplete: false,
    }
}

fn text(text: &str) -> Vec<Result<ModelEvent, ProviderError>> {
    vec![
        Ok(ModelEvent::TextDelta {
            text: text.to_string(),
        }),
        Ok(ModelEvent::TurnFinished {
            stop_reason: StopReason::EndTurn,
            usage: usage(),
        }),
    ]
}

fn call(id: &str, name: &str, args: &str) -> Vec<Result<ModelEvent, ProviderError>> {
    vec![
        Ok(ModelEvent::ToolCallDelta {
            index: 0,
            id: Some(id.to_string()),
            name: Some(name.to_string()),
            arguments: args.to_string(),
        }),
        Ok(ModelEvent::TurnFinished {
            stop_reason: StopReason::ToolUse,
            usage: usage(),
        }),
    ]
}

/// The six synthetic PRs. Two are actionable — one rev-sync, one hoist —
/// and four are irrelevant, so the pipeline's polls drain in mixed order
/// and exactly two PRs advance to stage 2 while four end early.
const PRS: [&str; 6] = [
    "app-a#101|rev-sync|Cargo.lock",
    "app-b#201|hoist|crates/engine",
    "app-c#301|irrelevant|docs",
    "app-a#102|irrelevant|.github",
    "app-b#202|irrelevant|tests",
    "app-d#401|irrelevant|README.md",
];

/// The scripted model's behaviour per marker. Every spawn call carries
/// `maxTurns: 10000`, a finite `timeoutSecs`, and omits `maxTokens`.
fn route(
    prompt: &str,
    tool_result: &str,
    staged: &[String],
    malformed: &AtomicU32,
) -> Vec<Result<ModelEvent, ProviderError>> {
    // A child prompt is built by this model's own template, so it starts
    // with the marker; a step prompt carries its body's marker inline.
    if prompt.starts_with("CLONE ") {
        call(
            "clone-write",
            "write_file",
            r#"{"path":"run-ledger/clone/app-x/outcome.md","content":"clone-ok\n"}"#,
        )
    } else if prompt.starts_with("PR ") {
        // One per-PR classifier child: read its shard line, write its
        // verdict file, and return the verdict line. The verdict class
        // comes from the shard's second field.
        let shard = prompt.trim_start_matches("PR ").trim();
        let number = shard.split('|').next().unwrap_or("x").to_string();
        let class = shard.split('|').nth(1).unwrap_or("irrelevant");
        call(
            "verdict-write",
            "write_file",
            &format!(
                r#"{{"path":"run-ledger/classifier/{}.verdict","content":"{} | {} | scripted reason | files: {}\n"}}"#,
                number,
                class,
                number,
                shard.split('|').nth(2).unwrap_or("none")
            ),
        )
    } else if prompt.starts_with("STAGE2 ") {
        // A stage-2 child, spawned the moment its PR's verdict landed.
        let shard = prompt.trim_start_matches("STAGE2 ").trim();
        let slug = shard.split('|').next().unwrap_or("x").trim().to_string();
        call(
            "draft-write",
            "write_file",
            &format!(
                r#"{{"path":"run-ledger/drafts/{}.md","content":"draft with evidence\n"}}"#,
                slug
            ),
        )
    } else if prompt.starts_with("ROLLUP ") {
        call(
            "rollup-write",
            "write_file",
            r#"{"path":"run-ledger/rollup/state-body.md","content":"matched\n"}"#,
        )
    } else if prompt.contains("GATE:") {
        // A judge seat. Seat 1 of the pipeline gate first answers in
        // prose so the engine's seat-repair loop must correct it; the
        // repair round carries the parser's complaint and gets JSON.
        if prompt.contains("[seat 1 of 3]")
            && prompt.contains("GATE:pipeline")
            && !prompt.contains("Verdict format repair")
        {
            malformed.fetch_add(1, Ordering::Relaxed);
            text("The pipeline looks complete to me; I saw the files and they matched.")
        } else {
            text(r#"{"verdict":"pass","issues":[]}"#)
        }
    } else if prompt.contains("SIM SWEEP") {
        // The scripted sweep: one bash call records the three artifacts
        // the launch would have produced.
        call(
            "sweep-bash",
            "bash",
            r#"{"command":"mkdir -p run-ledger/prepare run-ledger/classifier run-ledger/drafts run-ledger/outcomes && : > run-ledger/prepare/.collect-done && : > run-ledger/prepare/.sweep-done && : > run-ledger/prepare/.digest-done && date -u +%Y-%m-%dT%H:%M:%SZ | tee run-ledger/prepare/.collect-done"}"#,
        )
    } else if prompt.contains("SIM PIPELINE") {
        // The per-PR fan-out: ONE detached call for every PR, returning
        // at admission — no wave is awaited.
        let items = PRS
            .iter()
            .map(|shard| format!("\"{shard}\""))
            .collect::<Vec<_>>()
            .join(", ");
        call(
            "spawn-prs",
            "spawn_agents",
            &format!(
                r#"{{"agent":"sim-classifier","promptTemplate":"PR {{{{item}}}} — read your shard and write your verdict file","items":[{items}],"maxTurns":10000,"timeoutSecs":3600,"detached":true}}"#
            ),
        )
    } else if !tool_result.is_empty() {
        // A turn that follows a tool result. Which turn it is comes from
        // the result's shape, and only the orchestrator's branches may
        // spawn or poll: a classifier or stage-2 child sees its own write
        // result here and must answer with its verdict line instead.
        if tool_result.starts_with("detached:") {
            // A detached spawn's acceptance: it names ids only, so the
            // PR's class is learned from the POLL, never from this.
            poll_call(&outstanding_ids(tool_result), 10)
        } else if tool_result.contains("agent_id:") {
            // Poll envelopes. The ledger — not the envelope list —
            // decides what is left: one poll drains every settled
            // envelope at once, so a PR whose envelope was already read
            // still needs its stage-2 child.
            if let Some(shard) = staged.first().and_then(|shard| shard_for(shard, staged)) {
                emit_stage_two(shard)
            } else if tool_result.contains("outstanding: none") {
                text("per-PR pipeline complete: every PR's journey ended")
            } else {
                poll_call(&outstanding_ids(tool_result), 10)
            }
        } else if tool_result.contains("accepted: agent-") {
            // A stage-2 child just launched: collect its package.
            poll_call(&outstanding_ids(tool_result), 10)
        } else if tool_result.contains("run-ledger/classifier/") {
            // A classifier child's verdict write landed: return the
            // verdict line so the envelope carries the PR number.
            let verdict = verdict_line_for(tool_result);
            text(&verdict)
        } else if tool_result.contains("run-ledger/drafts/") {
            text("package written")
        } else {
            text("done")
        }
    } else if prompt.contains("SIM ROLLUP") {
        call(
            "spawn-rollup",
            "spawn_agent",
            r#"{"agent":"sim-rollup","prompt":"ROLLUP state — write the three delivery drafts locally","maxTurns":10000,"timeoutSecs":1800}"#,
        )
    } else if prompt.contains("SIM COMMIT") {
        text("commit recorded: run-ledger/outcomes/row.md")
    } else {
        text("done")
    }
}

/// The ids a tool result names as still outstanding.
fn outstanding_ids(tool_result: &str) -> Vec<String> {
    let Some(line) = tool_result
        .lines()
        .find(|line| line.starts_with("outstanding:"))
    else {
        return Vec::new();
    };
    line.trim_start_matches("outstanding:")
        .split(',')
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect()
}

/// The next poll: by the ids still outstanding when the previous result
/// named them, otherwise a bounded wait. Each call is distinct, so the
/// stationarity guard never stops a working pool.
fn poll_call(ids: &[String], wait: u64) -> Vec<Result<ModelEvent, ProviderError>> {
    if ids.is_empty() {
        return call("poll", "poll_agents", &format!(r#"{{"waitSecs": {wait}}}"#));
    }
    let list = ids
        .iter()
        .map(|id| format!("\"{id}\""))
        .collect::<Vec<_>>()
        .join(", ");
    call(
        "poll-ids",
        "poll_agents",
        &format!(r#"{{"ids": [{list}], "waitSecs": {wait}}}"#),
    )
}

/// The shard for a PR number, or the first available shard when the
/// ledger's list and the dispatch order disagree.
fn shard_for<'a>(number: &str, available: &'a [String]) -> Option<&'a str> {
    available
        .iter()
        .find(|shard| shard.split('|').next().unwrap_or("x") == number)
        .or_else(|| available.first())
        .map(|shard| shard.as_str())
}

/// The stage-2 detached spawn for one PR: `rev-sync` goes to the
/// applier, `hoist` and `port` to the filer. Both return at admission.
fn emit_stage_two(shard: &str) -> Vec<Result<ModelEvent, ProviderError>> {
    let class = shard.split('|').nth(1).unwrap_or("irrelevant");
    let profile = if class == "rev-sync" {
        "sim-applier"
    } else {
        "sim-filer"
    };
    let number = shard.split('|').next().unwrap_or("x").trim();
    call(
        "stage2",
        "spawn_agent",
        &format!(
            r#"{{"agent":"{profile}","prompt":"STAGE2 {shard} — write your package for {number} locally","maxTurns":10000,"timeoutSecs":1800,"detached":true}}"#
        ),
    )
}

/// The verdict line a classifier child returns, recovered from the path
/// its write landed at: the file's own first line is the verdict, so the
/// envelope's summary carries the PR number and its class.
fn verdict_line_for(tool_result: &str) -> String {
    for shard in &PRS {
        let number = shard.split('|').next().unwrap_or("x");
        let class = shard.split('|').nth(1).unwrap_or("irrelevant");
        if tool_result.contains(number) {
            return format!("{class} | {number} | scripted reason | files: scripted");
        }
    }
    String::from("unverified | unknown | scripted reason | files: none")
}
