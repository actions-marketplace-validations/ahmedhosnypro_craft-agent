//! The parent-facing record of one settled child: the envelope data,
//! the bounded payload, and the renderers. The child transcript never
//! crosses this boundary — the parent carries the envelope only.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::subagent::SubagentId;
use crate::types::Usage;

/// The summary block of an envelope caps at this many chars.
pub const SUMMARY_MAX_CHARS: usize = 8_192;
/// Failure detail caps at this many chars.
pub const DETAIL_MAX_CHARS: usize = 2_000;

/// Why a child ended. The token is the envelope `stop_reason` and the
/// stream-event vocabulary; a child ending is data for the parent's
/// next decision, never a parent abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStop {
    Completed,
    Error,
    MaxTurns,
    Stationarity,
    Budget,
    TimedOut,
    NoFinalMessage,
    SpawnError,
}

/// Whether a settled child produced its deliverable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStatus {
    Completed,
    Failed,
}

impl ChildStatus {
    /// The envelope token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

impl ChildStop {
    /// The stable token used in envelopes and events.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Error => "error",
            Self::MaxTurns => "max_turns",
            Self::Stationarity => "stationarity",
            Self::Budget => "budget",
            Self::TimedOut => "timed_out",
            Self::NoFinalMessage => "no_final_message",
            Self::SpawnError => "spawn_error",
        }
    }

    /// Envelope `status`: only a completed child completed.
    pub fn status(self) -> ChildStatus {
        match self {
            Self::Completed => ChildStatus::Completed,
            _ => ChildStatus::Failed,
        }
    }

    /// The `is_error` of the parent's tool result.
    pub fn is_error(self) -> bool {
        self != Self::Completed
    }
}

/// The bounded text a handoff carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffPayload {
    /// The child's final assistant message.
    Summary(String),
    /// Why a failed child ended.
    Detail(String),
}

/// One settled child as the parent reads it: envelope data plus the
/// bounded payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Handoff {
    pub id: SubagentId,
    pub profile: String,
    pub stop: ChildStop,
    /// The child's own turn count.
    pub turns: u32,
    pub usage: Usage,
    /// The parent's run budget left after this child settled.
    pub budget_remaining: u64,
    /// The child transcript pointer; absent for in-memory parents.
    pub session_path: Option<String>,
    pub payload: HandoffPayload,
}

impl Handoff {
    /// Tokens the child consumed: every usage bucket summed, the
    /// figure the run budget guard prices.
    pub fn total_tokens(&self) -> u64 {
        self.usage
            .input_tokens
            .saturating_add(self.usage.cache_read_tokens)
            .saturating_add(self.usage.cache_write_tokens)
            .saturating_add(self.usage.output_tokens)
            .saturating_add(self.usage.reasoning_tokens)
    }

    /// The model-readable envelope: stable `key: value` lines plus the
    /// bounded payload block.
    pub fn render(&self) -> String {
        let mut out = format!(
            "agent_id: {}\nprofile: {}\nstatus: {}\nstop_reason: {}\n\
             turns: {}  usage: {} tokens  budget_remaining: {}\n",
            self.id,
            self.profile,
            self.stop.status().as_str(),
            self.stop.as_str(),
            self.turns,
            self.total_tokens(),
            self.budget_remaining,
        );
        if let Some(path) = &self.session_path {
            out.push_str("transcript: ");
            out.push_str(path);
            out.push('\n');
        }
        match &self.payload {
            HandoffPayload::Summary(text) => {
                out.push_str("[summary]\n");
                push_truncated(&mut out, text, SUMMARY_MAX_CHARS);
            }
            HandoffPayload::Detail(text) => {
                out.push_str("[error]\n");
                push_truncated(&mut out, text, DETAIL_MAX_CHARS);
            }
        }
        out
    }
}

/// The batch result envelope: every member's envelope in id order,
/// then the explicit tally. Per-member failures ride their own blocks;
/// the batch itself is data, never an error.
pub fn render_batch(members: &[Handoff]) -> String {
    let completed = members
        .iter()
        .filter(|member| member.stop == ChildStop::Completed)
        .count();
    let timed_out = members
        .iter()
        .filter(|member| member.stop == ChildStop::TimedOut)
        .count();
    let failed = members
        .len()
        .saturating_sub(completed)
        .saturating_sub(timed_out);
    let mut out = format!(
        "batch: {} members  status: {}\n",
        members.len(),
        batch_status(members)
    );
    for member in members {
        out.push('[');
        out.push_str(&member.id.to_string());
        out.push_str("]\n");
        out.push_str(&member.render());
        out.push('\n');
    }
    let _ = write!(
        out,
        "tally: completed: {completed}, failed: {failed}, timed_out: {timed_out}"
    );
    out
}

/// The batch label: completed needs every member completed, partial is
/// any mix, failed is none.
fn batch_status(members: &[Handoff]) -> &'static str {
    if members.is_empty() {
        return "failed";
    }
    let any_completed = members
        .iter()
        .any(|member| member.stop == ChildStop::Completed);
    let all_completed = members
        .iter()
        .all(|member| member.stop == ChildStop::Completed);
    if all_completed {
        "completed"
    } else if any_completed {
        "partial"
    } else {
        "failed"
    }
}

/// Push at most `cap` chars of `text`, never splitting a char; a
/// fitting text borrows, no copy.
fn push_truncated(out: &mut String, text: &str, cap: usize) {
    match text.char_indices().nth(cap) {
        Some((idx, _)) => out.push_str(&text[..idx]),
        None => out.push_str(text),
    }
}

/// Truncate to at most `cap` chars, never splitting a char.
pub fn truncate_chars(text: &str, cap: usize) -> String {
    match text.char_indices().nth(cap) {
        Some((idx, _)) => text[..idx].to_string(),
        None => text.to_string(),
    }
}
