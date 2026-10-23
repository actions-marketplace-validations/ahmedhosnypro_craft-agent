//! The scripted model the simulation drives: one Provider that routes on
//! the prompt marker and replays the judged workflow's turn shapes — the
//! spawn waves, the judge verdicts, and the one malformed seat that
//! forces the engine's seat-repair loop.

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
}

impl SimProvider {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            malformed: AtomicU32::new(0),
        })
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
}

impl Provider for SimProvider {
    async fn stream(&self, request: ModelRequest) -> Result<ModelEventStream, ProviderError> {
        self.seen.lock().expect("seen").push(request.clone());
        let prompt = Self::prompt_of(&request);
        let events = route(&prompt, &self.malformed);
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

/// The scripted model's behaviour per marker. Every spawn call carries
/// `maxTurns: 10000`, `timeoutSecs: 0` and omits `maxTokens`; the single
/// child uses `spawn_agent`; the rest use one `spawn_agents` call.
fn route(prompt: &str, malformed: &AtomicU32) -> Vec<Result<ModelEvent, ProviderError>> {
    // A child prompt is built by this model's own template, so it starts
    // with the marker; a step prompt carries its body's marker inline.
    if prompt.starts_with("CLONE ") {
        call(
            "clone-write",
            "write_file",
            r#"{"path":"run-ledger/clone/app-x/outcome.md","content":"clone-ok\n"}"#,
        )
    } else if prompt.starts_with("BATCH ") {
        // A batch child fans out one grandchild per PR, in one call.
        call(
            "spawn-prs",
            "spawn_agents",
            r#"{"agent":"sim-classifier","promptTemplate":"PR {{item}} — write one verdict file","items":["app-a#101","app-a#102","app-a#103","app-b#201","app-b#202","app-c#301"],"maxTurns":10000,"timeoutSecs":0}"#,
        )
    } else if prompt.starts_with("PR ") {
        let pr = prompt.trim_start_matches("PR ").trim();
        call(
            "verdict-write",
            "write_file",
            &format!(
                r#"{{"path":"run-ledger/classifier/{}.verdict","content":"irrelevant | {} | docs-only change | files: none\n"}}"#,
                pr, pr
            ),
        )
    } else if prompt.starts_with("ITEM ") {
        let slug = prompt
            .trim_start_matches("ITEM ")
            .split('|')
            .next()
            .unwrap_or("x");
        call(
            "draft-write",
            "write_file",
            &format!(
                r#"{{"path":"run-ledger/drafts/{}.md","content":"draft with evidence\n"}}"#,
                slug.trim()
            ),
        )
    } else if prompt.starts_with("ROLLUP ") {
        call(
            "rollup-write",
            "write_file",
            r#"{"path":"run-ledger/rollup/state-body.md","content":"matched\n"}"#,
        )
    } else if prompt.contains("GATE:") {
        // A judge seat. Seat 1 of the classify gate first answers in
        // prose so the engine's seat-repair loop must correct it; the
        // repair round carries the parser's complaint and gets JSON.
        if prompt.contains("[seat 1 of 3]")
            && prompt.contains("GATE:classify")
            && !prompt.contains("Verdict format repair")
        {
            malformed.fetch_add(1, Ordering::Relaxed);
            text("The classification looks complete to me; I saw the files and they matched.")
        } else {
            text(r#"{"verdict":"pass","issues":[]}"#)
        }
    } else if prompt.contains("SIM PHASE 1") {
        call(
            "spawn-clones",
            "spawn_agents",
            r#"{"agent":"sim-cloner","promptTemplate":"CLONE {{item}} — record your outcome locally","items":["app-a|crates/ui","app-b|crates/engine","app-c|crates/geom","app-d|crates/text","app-e|crates/gpu","app-f|crates/mcp"],"maxTurns":10000,"timeoutSecs":0}"#,
        )
    } else if prompt.contains("SIM PHASE 2") {
        call(
            "spawn-batches",
            "spawn_agents",
            r#"{"agent":"sim-classifier","promptTemplate":"BATCH {{item}} — classify every PR in the batch","items":["batch-01|app-a#101;title a1|app-a#102;title a2|app-a#103;title a3","batch-02|app-b#201;title b1|app-b#202;title b2|app-c#301;title c1"],"maxTurns":10000,"timeoutSecs":0}"#,
        )
    } else if prompt.contains("SIM DRAFT — file") {
        call(
            "spawn-filers",
            "spawn_agents",
            r#"{"agent":"sim-filer","promptTemplate":"ITEM {{item}} — write the draft locally","items":["finding-one|app-a#101","finding-two|app-b#201"],"maxTurns":10000,"timeoutSecs":0}"#,
        )
    } else if prompt.contains("SIM DRAFT — bump") {
        call(
            "spawn-appliers",
            "spawn_agents",
            r#"{"agent":"sim-applier","promptTemplate":"ITEM {{item}} — write the branch package locally","items":["bump-one|app-a|rev-abc","bump-two|app-b|rev-abc"],"maxTurns":10000,"timeoutSecs":0}"#,
        )
    } else if prompt.contains("SIM ROLLUP — draft") {
        call(
            "spawn-rollup",
            "spawn_agent",
            r#"{"agent":"sim-rollup","prompt":"ROLLUP state — write the three delivery drafts locally","maxTurns":10000,"timeoutSecs":0}"#,
        )
    } else if prompt.contains("SIM COMMIT") {
        text("commit recorded: run-ledger/outcomes/row.md")
    } else {
        text("done")
    }
}
