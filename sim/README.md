# Runner simulation

`sim/` hosts a local rehearsal of the composite action: a container that
behaves like a GitHub Actions runner consuming the release binaries, so a
mission can be exercised end to end before it ships. `sim/entrypoint.sh`
mirrors the action's steps against runtime mounts instead of a release
download; `sim/run-sim.sh` is the host-side orchestrator.

## Running one mission

    bash sim/run-sim.sh [task]

Prerequisites: docker, host `gh`, and a repo-root `.env` carrying
`TEST_BYOK_API_KEY`, `TEST_BYOK_BASE_URL`, and a valid `GH_TOKEN` — the
same live-corpus knobs the integration tests use. Values are read at run
time and are never baked into the image or printed. The `simulate`
mission needs none of those.

One run:

1. builds the release for `x86_64-unknown-linux-musl` (replacing `dist/`),
2. builds the `craft-agent-sim:sim` image on the host network,
3. clones a fresh workspace into the run dir,
4. runs one `run` mission in the container, preserving its exit code.

The image also takes direct binary invocations, e.g.
`docker run --rm craft-agent-sim:sim --version`.

### The workflow rehearsal

A judged workflow rehearses locally before it ships, the same way:

    CRAFT_AGENT_MISSION=workflow CRAFT_AGENT_WORKFLOW=adopt-libs \
      CRAFT_AGENT_REPO_URL=https://github.com/ahmedhosnypro/craft-libs-adoption.git \
      CRAFT_AGENT_REPO_SLUG=ahmedhosnypro/craft-libs-adoption \
      CRAFT_AGENT_SUBAGENT_MAX_DEPTH=4 \
      CRAFT_AGENT_SUBAGENT_MAX_TOTAL=1024 \
      CRAFT_AGENT_SUBAGENT_MAX_CONCURRENCY=1024 \
      CRAFT_AGENT_WRITE_REPO=ahmedhosnypro/craft-libs \
      bash sim/run-sim.sh

The container runs `craft-agent workflow --workflow <name>` against the
fresh clone with the same knobs the action passes, then mirrors the run
journal and per-step sessions into `out/` next to the stream — the same
evidence the Actions artifact uploads. `sim/assert-workflow.sh` checks a
finished run dir against the five proofs (spawn tools live, parallelism
reached, wall time, gates settled, fail-closed on a hostile cap knob):

    bash sim/assert-workflow.sh sim/runs/<run> \
      [--min-children N] [--min-profiles N] [--max-seconds N]

`--min-children` (default 8) and `--min-profiles` (default 2) size the
fan-out proofs to the plan under test — a single-profile probe passes
with `--min-profiles 1`, and the real scan's clone + classify waves
expect both to be in the hundreds.

### The simulation (`simulate`)

The same architecture, minus the world: one `simulate` mission runs a
standalone integration binary that drives the judged workflow's full
16-step plan — fan-out waves, per-PR grandchildren, panel gates, a
repaired seat, appeals, draft waves, committers, rollup — over a
**scripted model** and local fixtures. No gateway, no `GH_TOKEN`, no
network, and it settles in seconds:

    CRAFT_AGENT_MISSION=simulate bash sim/run-sim.sh

The binary is built host-side (`cargo test -p craft-app --test
workflow_simulation --no-run`), mounted read-only, and its stream is
libtest output. A green run is `conclusion: success` with all four
proofs named in the final message: every step settles, the waves launch
and finish every child with real overlap, the malformed seat is repaired,
and a hostile hub knob fails closed. This is the cheap gate the
workflow-mode CI rehearsal runs first — it proves the engine's wiring
(spawn tools, gate panels, seat repair, draft/commit split) before any
real gateway spend.

## The run dir

Runs land in `sim/runs/<timestamp>/`:

| path | content |
|---|---|
| `manifest.json` | run provenance — see below |
| `container.log` | everything the container printed |
| `out/outputs.env` | the action-shaped outputs (below) |
| `out/stream.jsonl` | the stream-json transcript, or the simulation's libtest output |
| `out/journal.jsonl` | the workflow run journal, workflow mission only |
| `out/sessions/` | the per-step session transcripts, workflow mission only |
| `tmp/` | the mounted `RUNNER_TEMP`, holding the prompt file |
| `workspace/` | the clone the mission ran against |

The entrypoint chowns the mounted artifacts to the invoking user as it
runs, so a finished run is removable without sudo. Runs made before that
chown step may still hold container-owned files; reclaim one with
`sudo chown -R "$(id -u):$(id -g)" sim/runs/<run>`.

## The manifest

`manifest.json` is written host-side in three stages — at run-dir creation,
after the clone, and after the container exits — so a failed run still
describes itself. A failed clone records `status: "clone-failed"` and the
run dir is kept as evidence.

| field | value |
|---|---|
| `schema` | manifest shape version (`1`) |
| `status` | `cloning`, `clone-failed`, or `completed` |
| `task`, `mission`, `workflow` | the task file that ran; the mission command (`run`); the workflow plan name (workflow mission only) |
| `repo`, `clone_commit` | cloned URL and the cloned commit SHA |
| `image`, `image_id` | the sim image tag and its docker digest |
| `binaries` | `sha256` of both release binaries, from `dist/checksums.txt` |
| `knobs` | the env overrides below — values only, never secrets |
| `started_at`, `finished_at`, `wall_seconds` | run timing |
| `exit_code`, `conclusion` | the container's exit code and its named form |

Keys and tokens (`GH_TOKEN`, provider keys) are never written to the
manifest, the log, or any artifact.

## Knobs

Environment overrides read by `run-sim.sh` at run time:

| env | default | meaning |
|---|---|---|
| `CRAFT_AGENT_REPO_URL` | the craft-agent origin | repository the run clones and the pull request targets |
| `CRAFT_AGENT_REPO_SLUG` | derived from the URL | `owner/name` the gh token read-check runs against |
| `CRAFT_AGENT_MAX_TURNS` | `40` | turn cap passed to the mission |
| `CRAFT_AGENT_MAX_TOKENS` | `4000000` | token budget cap |
| `CRAFT_AGENT_PROVIDER` | `openai` | provider id |
| `CRAFT_AGENT_MODEL` | `glm-5.3-flash` | model id |
| `CRAFT_AGENT_STEP_RETRIES` | `3` | attempts per model step on a dropped stream |
| `CRAFT_AGENT_RESULT_WINDOW_TURNS` | `0` | turns of tool results kept verbatim in the prompt; 0 keeps the prompt append-only, preserving gateway cache affinity |
| `CRAFT_AGENT_RESULT_TRUNCATE_BYTES` | `4096` | bytes of each tool result kept verbatim in the prompt; oversized results ride as head plus a spill pointer |
| `CRAFT_AGENT_EGRESS_ALLOWLIST` | unset | hosts bash commands may talk to; unset or empty means ungated |
| `CRAFT_AGENT_WRITE_REPO` | unset | `owner/name` the gh tool may write to; unset or empty means ungated |
| `CRAFT_AGENT_SUBAGENT_MAX_DEPTH` | `2` | subagent tree depth; `0` disables spawning |
| `CRAFT_AGENT_SUBAGENT_MAX_TOTAL` | `32` | subagents admitted per run, cumulative |
| `CRAFT_AGENT_SUBAGENT_MAX_CONCURRENCY` | `4` | subagents running at once, tree-wide |
| `CRAFT_AGENT_REQUEST_TIMEOUT_SECS` | client default | one model call's deadline |
| `CRAFT_AGENT_READ_ROOTS` | `/runner/temp` | extra read-only roots for read_file/glob/grep, comma separated; the file tools read absolute paths inside them |
| `CRAFT_AGENT_MISSION` | `run` | `run`, `digest`, `workflow`, or `simulate` |
| `CRAFT_AGENT_WORKFLOW` | unset | the plan name, workflow mission only |
| `SIMULATE_BIN` | auto-built | path to the simulation integration binary, simulate mission only |

## Task files

`sim/tasks/<name>.md` is the prompt source; the first argument picks it
(default `open-pr-01`). The tasks directory is mounted read-only at
`/tasks` and the entrypoint passes `/tasks/<name>.md` as the prompt file —
the same seam the action's `prompt` input fills.

## Outputs

`out/outputs.env` mirrors the action's `GITHUB_OUTPUT` block:

- `conclusion` — `success`, `error`, `max-turns`, `budget`, `denied`, or `failed` (workflow mission)
- `exit-code` — the container's typed exit code
- `digest-path` — the digest markdown, digest mission on success only
- `stream` — the transcript path relative to the run dir
- `journal` — the workflow journal path, workflow mission only
- `sessions` — the per-step sessions directory, workflow mission only
- `artifacts-owned` — `true` when the mounts were chowned to the invoking user, `false` when a chown failed, `na` when no host uid was passed
- `final-message` — the terminal `run_finished` detail, the workflow
  report's failed step and skipped count, or the simulation's proof
  results, heredoc-quoted

`run-sim.sh` carries `exit-code` into the manifest's `exit_code` and
`artifacts-owned` into `artifacts_owned` (null when the run produced no
outputs.env).

Exit codes follow the documented taxonomy: the mission run maps
0 success, 2 max-turns, 3 budget, 4 denied-path; the workflow run maps
0 success, **1 a failed step or gate**, 2 max-turns, 3 budget; anything
else maps to `error`, and a nonzero exit prints the last stream events
into the log. `run-sim.sh` prints the conclusion, exit code, stream
path, journal and session paths, and final message when the run ends.
