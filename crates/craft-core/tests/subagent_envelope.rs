//! Envelope goldens: the child stop taxonomy, the single handoff
//! envelope with its summary and error caps, and the batch aggregate
//! with its tally and honesty labels.

use craft_core::subagent::{
    ChildStatus, ChildStop, DETAIL_MAX_CHARS, Handoff, HandoffPayload, SUMMARY_MAX_CHARS,
    SubagentId, render_batch,
};
use craft_core::types::Usage;

fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        incomplete: false,
    }
}

/// A settled-child fixture; callers unwrap inside the test body.
fn sample(id: &str, stop: ChildStop) -> Option<Handoff> {
    Some(Handoff {
        id: SubagentId::parse(id)?,
        profile: "repo-scout".to_string(),
        stop,
        turns: 2,
        usage: usage(30, 10),
        budget_remaining: 1_000,
        session_path: None,
        payload: HandoffPayload::Detail(String::from("why")),
    })
}

#[test]
fn golden_success_envelope() {
    let mut handoff = sample("agent-1", ChildStop::Completed).unwrap();
    handoff.turns = 9;
    handoff.usage = usage(30_000, 11_200);
    handoff.budget_remaining = 3_580_000;
    handoff.session_path = Some(String::from(
        ".craft-agent/sessions/run-01/agents/agent-1/session.jsonl",
    ));
    handoff.payload = HandoffPayload::Summary(String::from("Found the seam."));
    assert_eq!(
        handoff.render(),
        "agent_id: agent-1\nprofile: repo-scout\nstatus: completed\n\
         stop_reason: completed\n\
         turns: 9  usage: 41200 tokens  budget_remaining: 3580000\n\
         transcript: .craft-agent/sessions/run-01/agents/agent-1/session.jsonl\n\
         [summary]\n\
         Found the seam."
    );
}

#[test]
fn golden_failed_envelope_omits_the_transcript_when_in_memory() {
    let mut handoff = sample("agent-3", ChildStop::Budget).unwrap();
    handoff.turns = 3;
    handoff.usage = usage(1_000, 200);
    handoff.budget_remaining = 2_400;
    handoff.payload = HandoffPayload::Detail(String::from(
        "token budget exceeded: 1200 tokens spent, 1000 allowed",
    ));
    assert_eq!(
        handoff.render(),
        "agent_id: agent-3\nprofile: repo-scout\nstatus: failed\n\
         stop_reason: budget\n\
         turns: 3  usage: 1200 tokens  budget_remaining: 2400\n\
         [error]\n\
         token budget exceeded: 1200 tokens spent, 1000 allowed"
    );
}

#[test]
fn payload_blocks_are_capped_on_char_boundaries() {
    let mut handoff = sample("agent-1", ChildStop::Completed).unwrap();
    handoff.payload = HandoffPayload::Summary("s".repeat(9_000));
    let rendered = handoff.render();
    let block = rendered.split_once("[summary]\n").expect("summary block").1;
    assert_eq!(block.chars().count(), SUMMARY_MAX_CHARS);
    handoff.payload = HandoffPayload::Summary("é".repeat(9_000));
    let rendered = handoff.render();
    let block = rendered.split_once("[summary]\n").expect("summary block").1;
    assert_eq!(block.chars().count(), SUMMARY_MAX_CHARS);
    handoff.stop = ChildStop::Error;
    handoff.payload = HandoffPayload::Detail("e".repeat(3_000));
    let rendered = handoff.render();
    let block = rendered.split_once("[error]\n").expect("error block").1;
    assert_eq!(block.chars().count(), DETAIL_MAX_CHARS);
}

#[test]
fn total_tokens_sums_every_bucket() {
    let mut handoff = sample("agent-1", ChildStop::Completed).unwrap();
    handoff.usage = Usage {
        input_tokens: 100,
        output_tokens: 20,
        cache_read_tokens: 3,
        cache_write_tokens: 4,
        reasoning_tokens: 5,
        incomplete: false,
    };
    assert_eq!(handoff.total_tokens(), 132);
}

#[test]
fn stop_tokens_status_and_serde_agree() {
    let cases = [
        (ChildStop::Completed, "completed", false),
        (ChildStop::Error, "error", true),
        (ChildStop::MaxTurns, "max_turns", true),
        (ChildStop::Stationarity, "stationarity", true),
        (ChildStop::Budget, "budget", true),
        (ChildStop::TimedOut, "timed_out", true),
        (ChildStop::NoFinalMessage, "no_final_message", true),
        (ChildStop::SpawnError, "spawn_error", true),
    ];
    for (stop, token, is_error) in cases {
        assert_eq!(stop.as_str(), token);
        assert_eq!(stop.is_error(), is_error);
        let status = if is_error {
            ChildStatus::Failed
        } else {
            ChildStatus::Completed
        };
        assert_eq!(stop.status(), status);
        assert_eq!(
            status.as_str(),
            if is_error { "failed" } else { "completed" }
        );
        let wire = serde_json::to_string(&stop).expect("serializes");
        assert_eq!(wire, format!("\"{token}\""));
        let back: ChildStop = serde_json::from_str(&wire).expect("round trip");
        assert_eq!(back, stop);
    }
}

#[test]
fn golden_batch_aggregate() {
    let mut first = sample("agent-1", ChildStop::Completed).unwrap();
    first.payload = HandoffPayload::Summary(String::from("Found the seam."));
    let mut second = sample("agent-2", ChildStop::Budget).unwrap();
    second.payload = HandoffPayload::Detail(String::from("slice exceeded"));
    let mut third = sample("agent-3", ChildStop::TimedOut).unwrap();
    third.payload = HandoffPayload::Detail(String::from("wall clock expired"));
    let text = render_batch(&[first, second, third]);
    assert!(text.starts_with("batch: 3 members  status: partial\n"));
    assert!(text.contains("[agent-1]\nagent_id: agent-1\nprofile: repo-scout\n"));
    assert!(text.contains("[agent-2]\nagent_id: agent-2\n"));
    assert!(text.contains("[agent-3]\nagent_id: agent-3\n"));
    assert!(text.contains("stop_reason: timed_out\n"));
    assert!(text.ends_with("tally: completed: 1, failed: 1, timed_out: 1"));
}

#[test]
fn batch_labels_are_honest() {
    let completed = || sample("agent-1", ChildStop::Completed).unwrap();
    let failed = || sample("agent-2", ChildStop::Error).unwrap();
    assert!(render_batch(&[completed()]).starts_with("batch: 1 members  status: completed\n"));
    assert!(
        render_batch(&[completed(), failed()]).starts_with("batch: 2 members  status: partial\n")
    );
    assert!(render_batch(&[failed(), failed()]).starts_with("batch: 2 members  status: failed\n"));
    let empty = render_batch(&[]);
    assert!(empty.starts_with("batch: 0 members  status: failed\n"));
    assert!(empty.ends_with("tally: completed: 0, failed: 0, timed_out: 0"));
}
