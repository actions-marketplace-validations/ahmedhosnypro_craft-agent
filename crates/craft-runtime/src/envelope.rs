//! The runtime's child-result record: the mapping from a child run's
//! outcome onto the shared [`ChildStop`] taxonomy and the conversion
//! into the model-readable [`Handoff`] envelope. Rendering itself is
//! craft-core's; the runtime only produces the data.

use craft_core::subagent::handoff::{Handoff, HandoffPayload};
use craft_core::subagent::{ChildStop, SubagentId};
use craft_core::types::Usage;

use crate::outcome::{ExitStatus, RunOutcome};

/// Catalog listing cap for the unknown-profile error.
pub const LISTING_CAP: usize = 2000;

/// One settled child: what the parent model reads. A child ending is
/// data for the parent's next decision, never a parent abort.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildReport {
    pub stop: ChildStop,
    pub usage: Usage,
    pub turns: u32,
    pub final_text: String,
    pub detail: String,
    pub session_path: Option<String>,
}

impl ChildReport {
    /// Maps a child run's outcome plus folded final text onto the
    /// shared taxonomy: exit 0 with a handoff completed; the
    /// hub-level endings (`timed_out`, `spawn_error`) are built by
    /// the hub itself.
    pub fn from_outcome(outcome: RunOutcome, final_text: String) -> Self {
        let stop = match outcome.exit {
            ExitStatus::Ok if !final_text.trim().is_empty() => ChildStop::Completed,
            ExitStatus::Ok => ChildStop::NoFinalMessage,
            ExitStatus::Error => ChildStop::Error,
            ExitStatus::MaxTurns => ChildStop::MaxTurns,
            ExitStatus::Stationarity => ChildStop::Stationarity,
            ExitStatus::Budget => ChildStop::Budget,
        };
        let detail = if stop == ChildStop::NoFinalMessage {
            String::from("the subagent completed without a final message")
        } else {
            outcome.detail
        };
        Self {
            stop,
            usage: outcome.usage,
            turns: outcome.turns,
            final_text,
            detail,
            session_path: outcome.session_path.map(|path| path.display().to_string()),
        }
    }

    /// The envelope the parent reads: summary for a completed child,
    /// the typed detail for every other ending.
    pub fn into_handoff(self, id: &str, profile: &str, budget_remaining: u64) -> Handoff {
        let payload = if self.stop == ChildStop::Completed {
            HandoffPayload::Summary(self.final_text)
        } else if self.final_text.trim().is_empty() {
            HandoffPayload::Detail(self.detail)
        } else {
            HandoffPayload::Detail(format!("{}\n[summary]\n{}", self.detail, self.final_text))
        };
        Handoff {
            id: SubagentId::parse(id).unwrap_or_else(|| {
                debug_assert!(false, "hub-minted id `{id}` must parse");
                SubagentId::FIRST
            }),
            profile: profile.to_string(),
            stop: self.stop,
            turns: self.turns,
            usage: self.usage,
            budget_remaining,
            session_path: self.session_path,
            payload,
        }
    }
}

/// Caps text at `cap` chars on a char boundary, marking the cut.
pub(crate) fn cap_chars(text: &str, cap: usize) -> String {
    if text.chars().count() > cap {
        let mut cut = craft_core::subagent::truncate_chars(text, cap);
        cut.push('…');
        cut
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::CancelCategory;

    fn outcome(exit: ExitStatus, category: CancelCategory, detail: &str) -> RunOutcome {
        RunOutcome {
            exit,
            category,
            detail: detail.to_string(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..crate::budget::zero_usage()
            },
            turns: 2,
            session_path: None,
        }
    }

    #[test]
    fn taxonomy_maps_every_exit_onto_child_stop() {
        let completed = ChildReport::from_outcome(
            outcome(ExitStatus::Ok, CancelCategory::Completed, "run completed"),
            String::from("handoff"),
        );
        assert_eq!(completed.stop, ChildStop::Completed);
        let no_text = ChildReport::from_outcome(
            outcome(ExitStatus::Ok, CancelCategory::Completed, "run completed"),
            String::new(),
        );
        assert_eq!(no_text.stop, ChildStop::NoFinalMessage);
        let budget = ChildReport::from_outcome(
            outcome(ExitStatus::Budget, CancelCategory::Budget, "over"),
            String::from("halfway"),
        );
        assert_eq!(budget.stop, ChildStop::Budget);
        assert!(budget.stop.is_error());
    }

    #[test]
    fn handoffs_carry_the_envelope_data() {
        let report = ChildReport::from_outcome(
            outcome(ExitStatus::Ok, CancelCategory::Completed, "run completed"),
            String::from("done"),
        );
        let handoff = report.into_handoff("agent-3", "repo-scout", 900);
        assert_eq!(handoff.id.to_string(), "agent-3");
        assert_eq!(handoff.profile, "repo-scout");
        assert_eq!(handoff.stop.as_str(), "completed");
        assert_eq!(handoff.total_tokens(), 15);
        assert_eq!(handoff.budget_remaining, 900);
        assert_eq!(
            handoff.payload,
            HandoffPayload::Summary(String::from("done"))
        );
        let failed = ChildReport::from_outcome(
            outcome(
                ExitStatus::Budget,
                CancelCategory::Budget,
                "token budget exceeded",
            ),
            String::new(),
        );
        let handoff = failed.into_handoff("agent-4", "repo-scout", 0);
        assert!(handoff.stop.status().as_str() == "failed");
        assert!(handoff.render().contains("stop_reason: budget"));
        assert!(handoff.render().contains("[error]"));
    }

    #[test]
    fn cap_chars_cuts_on_a_char_boundary_with_a_marker() {
        assert_eq!(cap_chars("abcdef", 3), "abc…");
        assert_eq!(cap_chars("abc", 3), "abc");
        assert_eq!(cap_chars("héllo", 2), "hé…");
        assert_eq!(cap_chars("", 0), "");
    }

    #[test]
    #[should_panic(expected = "must parse")]
    fn into_handoff_rejects_an_unparseable_id() {
        let report = ChildReport::from_outcome(
            outcome(ExitStatus::Ok, CancelCategory::Completed, "run completed"),
            String::from("done"),
        );
        let _ = report.into_handoff("not-an-id", "repo-scout", 0);
    }
}
