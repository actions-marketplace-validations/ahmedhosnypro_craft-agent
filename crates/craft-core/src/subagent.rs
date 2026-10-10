//! Subagent vocabulary shared by the runtime and the adapters: the
//! spawn-request types, the child stop taxonomy, and the parent-facing
//! handoff with its envelope renderers. Pure data — the spawn hub, the
//! spawn tools, and their wiring live in craft-runtime.

pub mod handoff;
pub mod spec;

pub use handoff::{
    ChildStatus, ChildStop, DETAIL_MAX_CHARS, Handoff, HandoffPayload, SUMMARY_MAX_CHARS,
    render_batch, truncate_chars,
};
pub use spec::{SpawnBatch, SpawnSpec, SubagentError};

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A child id: `agent-N`, 1-based, allocated by the parent's spawn hub
/// in spawn order and never reused within a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubagentId(u32);

impl SubagentId {
    /// The first id a run allocates.
    pub const FIRST: Self = Self(1);

    /// The 1-based allocation number.
    pub fn index(self) -> u32 {
        self.0
    }

    /// The next id in allocation order; saturates rather than overflow.
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Parses `agent-N` exactly; nothing else is an id.
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix("agent-")?;
        let index = rest.parse::<u32>().ok()?;
        (index > 0).then_some(Self(index))
    }
}

impl fmt::Display for SubagentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "agent-{}", self.0)
    }
}

impl Serialize for SubagentId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SubagentId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| serde::de::Error::custom("expected `agent-N`"))
    }
}

/// The fixed system-prompt overlay every child receives on top of its
/// persona: the context firewall, stated once.
pub const SUBAGENT_OVERLAY: &str = "You are a subagent. Your task arrives as one message from the parent \
agent that spawned you; the parent shares no other context and reads \
only your final message. Work the task with your tools, then finish \
with a complete, self-standing handoff: what was done, the file paths \
touched, how it was verified, and what remains. Write for the parent \
agent, not the end user.";
