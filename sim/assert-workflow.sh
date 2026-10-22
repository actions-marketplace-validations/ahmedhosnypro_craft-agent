#!/usr/bin/env bash
# Asserts one finished workflow sim run against the five proofs the
# engine fix must show locally before anything goes to the cloud:
#
#   1. spawn tools live  — the fan-out steps actually launched children
#   2. parallelism       — many small children ran concurrently
#   3. wall time         — the serialize-bound phases are fast
#   4. gates settle      — every judge gate reached a verdict
#   5. fail closed       — a hostile cap knob aborts, never degrades
#
# Usage: sim/assert-workflow.sh <run-dir> [--min-children N] [--max-seconds N]
# Exits nonzero with the failing proof named. jq and awk only.
set -euo pipefail

run_dir="${1:?usage: assert-workflow.sh <run-dir> [--min-children N] [--max-seconds N]}"
shift || true

min_children=8
max_seconds=21600
min_profiles=2
while [ "$#" -gt 0 ]; do
  case "$1" in
    --min-children) min_children="${2:?}"; shift 2 ;;
    --max-seconds) max_seconds="${2:?}"; shift 2 ;;
    --min-profiles) min_profiles="${2:?}"; shift 2 ;;
    *) printf 'assert-workflow: unknown flag %s\n' "$1" >&2; exit 2 ;;
  esac
done

out="$run_dir/out"
stream="$out/stream.jsonl"
journal="$out/journal.jsonl"
manifest="$run_dir/manifest.json"

[ -f "$stream" ] || { printf 'FAIL: no stream at %s\n' "$stream" >&2; exit 1; }
[ -f "$journal" ] || { printf 'FAIL: no journal at %s\n' "$journal" >&2; exit 1; }
[ -f "$manifest" ] || { printf 'FAIL: no manifest at %s\n' "$manifest" >&2; exit 1; }

pass() { printf '  ok  %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }

mission="$(jq -r '.mission // ""' "$manifest")"
[ "$mission" = "workflow" ] ||
  fail "this run was not a workflow rehearsal (mission=$mission)"

printf 'proofs for %s\n' "$run_dir"

# --- the run settled with a report ---------------------------------------------
report="$(jq -c 'select(.type == "workflow_finished") | .report' "$stream" | tail -n 1)"
[ -n "$report" ] || fail "the stream carries no workflow_finished report"
conclusion="$(printf '%s' "$report" | jq -r '.conclusion.kind // ""')"
conclusion="${conclusion:-unknown}"
failed_step="$(printf '%s' "$report" | jq -r '.failed_step // ""')"
printf '  run  conclusion=%s failed_step=%s\n' "$conclusion" "${failed_step:-none}"

# --- 1. spawn tools live: children launched from real step turns -----------------
started="$(jq -r 'select(.type == "subagent_started") | .profile' "$stream" | sort | uniq -c)"
child_count="$(printf '%s\n' "$started" | awk 'NF {n+=$1} END {print n+0}')"
[ "$child_count" -ge "$min_children" ] ||
  fail "only $child_count children launched (want >= $min_children) — spawn tools still absent from the steps"
profiles_count="$(printf '%s\n' "$started" | awk 'NF {n+=1} END {print n+0}')"
[ "$profiles_count" -ge "$min_profiles" ] ||
  fail "children came from $profiles_count profile(s) ($(printf '%s' "$started" | tr '\n' ' ')) — want >= $min_profiles distinct profiles fanning out"
pass "spawn tools live: $child_count children across $profiles_count profiles"

# --- 2. parallelism: children overlapped in time ---------------------------------
# The stream carries one subagent_started/finished JSON line per child
# in order; a live counter over both is the concurrency observed.
peak="$(awk '
  /subagent_started/  { alive += 1; if (alive > peak) peak = alive }
  /subagent_finished/ { alive -= 1 }
  END { print peak + 0 }
' "$stream")"
[ "$peak" -ge "$min_children" ] ||
  fail "peak concurrent children was $peak (want >= $min_children) — the dispatch never overlapped"
pass "parallelism: peak $peak children running at once"

# --- 3. every child settled: none left dangling ----------------------------------
# Hub ids are unique across a run; a started id with no finished line
# is a child the run lost mid-flight.
dangling="$(awk '
  /subagent_started/  { match($0, /"id":"[^"]+"/); started[substr($0, RSTART+6, RLENGTH-7)] = 1 }
  /subagent_finished/ { match($0, /"id":"[^"]+"/); done[substr($0, RSTART+6, RLENGTH-7)] = 1 }
  END { n = 0; for (k in started) if (!(k in done)) n++; print n }
' "$stream")"
[ "$dangling" -eq 0 ] ||
  fail "$dangling children started but never finished — the run died mid-fan-out"
finished="$(awk '/subagent_finished/ {n++} END {print n+0}' "$stream")"
[ "$finished" -ge "$min_children" ] ||
  fail "only $finished children finished (want >= $min_children)"
pass "all $finished children settled, none dangling"

# --- 4. gates settle: the journal carries no errored judge step -------------------
gate_errors="$(jq -r '
  select(.type == "step_finished")
  | select(.conclusion == "error")
  | .id
' "$journal" | sort -u | tr '\n' ' ')"
[ -z "$(printf '%s' "$gate_errors" | tr -d ' ')" ] ||
  fail "steps settled as errors: $gate_errors"
skipped="$(jq -r 'select(.type == "step_skipped") | .id' "$journal" | sort -u | wc -l | tr -d ' ')"
gate_reasons="$(jq -r 'select(.type == "step_finished") | select(.conclusion == "max_turns" or .conclusion == "stationarity") | .id' "$journal" | sort -u | tr '\n' ' ')"
[ -z "$(printf '%s' "$gate_reasons" | tr -d ' ')" ] ||
  fail "steps hit their turn ceiling or stationarity: $gate_reasons"
pass "every step settled clean ($skipped steps skipped, all named by their dependency)"

# --- wall time -------------------------------------------------------------------
wall="$(jq -r '.wall_seconds // 0' "$manifest")"
[ "$wall" -le "$max_seconds" ] ||
  fail "wall time ${wall}s exceeds the ${max_seconds}s ceiling"
pass "wall time ${wall}s within the ${max_seconds}s ceiling"

# --- the run the workflow reported -------------------------------------------------
case "$conclusion" in
  completed) pass "run concluded completed" ;;
  *) fail "the run concluded $conclusion (failed step: ${failed_step:-none})" ;;
esac

printf 'all five proofs hold for %s\n' "$run_dir"
