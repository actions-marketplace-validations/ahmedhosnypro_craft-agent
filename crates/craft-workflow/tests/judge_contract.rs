//! Judge contract corpus: the seat verdict parse, the panel merge
//! policies, the settled verdict text round-trip, and the hostile-input
//! bounds on a seat message.
#![allow(clippy::unwrap_used)]

use craft_workflow::WorkflowError;
use craft_workflow::judge::{
    MAX_ISSUES, MergedVerdictRead, SeatVerdict, is_verdict_message, merge_seats, merged_json,
    parse_merged, parse_seat,
};
use craft_workflow::plan::JudgeMerge;

fn seat(pass: bool, issues: &[&str]) -> SeatVerdict {
    SeatVerdict {
        pass,
        issues: issues.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn a_seat_verdict_parses_pass_and_fail() {
    let ok = parse_seat("judge", r#"{"verdict":"pass"}"#).unwrap();
    assert!(ok.pass);
    assert!(ok.issues.is_empty());
    let bad = parse_seat("judge", r#"{"verdict":"fail","issues":["x"]}"#).unwrap();
    assert!(!bad.pass);
    assert_eq!(bad.issues, ["x"]);
}

#[test]
fn a_seat_verdict_survives_prose_wrapping() {
    let text = "I reviewed the diff.\n{\"verdict\":\"fail\",\"issues\":[\"no tests\"]}\nDone.";
    let verdict = parse_seat("judge", text).unwrap();
    assert!(!verdict.pass);
    assert_eq!(verdict.issues, ["no tests"]);
}

#[test]
fn a_malformed_seat_message_is_a_typed_gate_error() {
    let cases = [
        "no object at all",
        "{ not json",
        r#"{"issues":["x"]}"#,
        r#"{"verdict":"maybe"}"#,
        r#"{"verdict":"fail","issues":"x"}"#,
        r#"{"verdict":"fail","issues":[1]}"#,
    ];
    for case in cases {
        let err = parse_seat("gate", case).unwrap_err();
        assert!(
            matches!(err, WorkflowError::JudgeGate { ref step, .. } if step == "gate"),
            "expected a judge gate for {case:?}, got {err:?}"
        );
    }
}

#[test]
fn the_scan_window_is_bounded() {
    let padding = "x".repeat(20_000);
    let text = format!("{padding}{{\"verdict\":\"pass\"}}");
    assert!(parse_seat("judge", &text).is_err());
    let mut json = String::from(r#"{"verdict":"fail","issues":["#);
    let long = "y".repeat(5_000);
    json.push_str(&serde_json::to_string(&long).unwrap());
    for index in 0..40 {
        json.push(',');
        json.push_str(&serde_json::to_string(&format!("issue-{index}")).unwrap());
    }
    json.push_str("]}");
    let verdict = parse_seat("judge", &json).unwrap();
    assert_eq!(verdict.issues.len(), MAX_ISSUES, "the count cap holds");
    assert!(
        verdict.issues[0].len() <= 2_000,
        "the per-issue char cap holds"
    );
}

#[test]
fn majority_passes_on_a_strict_majority_and_fails_a_tie() {
    let one = merge_seats("g", JudgeMerge::Majority, &[seat(true, &[])]).unwrap();
    assert!(one.pass);
    assert_eq!((one.passes, one.fails), (1, 0));
    let tie = merge_seats(
        "g",
        JudgeMerge::Majority,
        &[seat(true, &[]), seat(false, &["a"])],
    )
    .unwrap();
    assert!(!tie.pass);
    let win = merge_seats(
        "g",
        JudgeMerge::Majority,
        &[seat(true, &[]), seat(false, &["a"]), seat(true, &[])],
    )
    .unwrap();
    assert!(win.pass);
}

#[test]
fn unanimous_passes_only_when_every_seat_passed() {
    let all = merge_seats(
        "g",
        JudgeMerge::Unanimous,
        &[seat(true, &[]), seat(true, &[])],
    )
    .unwrap();
    assert!(all.pass);
    let split = merge_seats(
        "g",
        JudgeMerge::Unanimous,
        &[seat(true, &[]), seat(false, &[])],
    )
    .unwrap();
    assert!(!split.pass);
}

#[test]
fn a_panel_with_no_seats_is_a_gate_error() {
    let err = merge_seats("g", JudgeMerge::Majority, &[]).unwrap_err();
    assert!(matches!(err, WorkflowError::JudgeGate { .. }));
}

#[test]
fn merged_issues_are_deduped_across_seats() {
    let merged = merge_seats(
        "g",
        JudgeMerge::Majority,
        &[
            seat(false, &["dup", "only-first"]),
            seat(false, &["dup", "only-second"]),
        ],
    )
    .unwrap();
    assert_eq!(
        merged.issues,
        ["dup", "only-first", "only-second"],
        "failing seats contribute their issues once each, in seat order"
    );
}

#[test]
fn the_settled_verdict_text_round_trips() {
    let merged = merge_seats(
        "g",
        JudgeMerge::Majority,
        &[seat(false, &["missing null guard"])],
    )
    .unwrap();
    let text = merged_json(&merged);
    assert!(is_verdict_message(&text));
    let read: MergedVerdictRead = parse_merged(&text).unwrap();
    assert!(!read.pass);
    assert_eq!(read.issues, ["missing null guard"]);
    let plain = "the step finished";
    assert!(!is_verdict_message(plain));
    assert!(parse_merged(plain).is_err());
}
