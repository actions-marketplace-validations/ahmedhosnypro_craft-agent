---
name: sim-adopt
description: "Synthetic rehearsal of the judged adoption workflow's per-PR architecture: sweep, one detached child per PR, merged gates, committers, rollup."
args:
  window: { type: string, default: "7h" }
limits:
  max-steps: "12"
  max-total-turns: "2000"
  max-total-tokens: "1000000"
  max-retries-per-step: "2"
  max-concurrent-steps: "4"
steps:
  - id: prepare
    agent: sim-orchestrator
    needs: []
    max-turns: "30"
    retry: "0"
    on-failure: stop
    outputs: [collection]
    body: |
      SIM SWEEP — the scripted parallel sweep: one bash call runs the
      collection, the clone sweep, and the digest read concurrently.
      Children record local artifacts; nothing here decides anything.
  - id: pr-pipeline
    agent: sim-orchestrator
    needs: [prepare]
    max-turns: "120"
    retry: "0"
    on-failure: stop
    outputs: [verdict-files, draft-paths]
    body: |
      SIM PIPELINE — one pipeline per PR: fire ONE detached spawn_agents
      call (profile sim-classifier), one item per PR (six PRs total),
      carrying `detached: true`. The call returns at admission; then
      poll with `poll_agents` as each PR's verdict lands and advance
      that PR immediately — a `rev-sync` verdict gets one detached
      `spawn_agent` call to sim-applier, a `hoist` or `port` verdict one
      detached `spawn_agent` call to sim-filer. Two actionable PRs,
      four irrelevant, no batch shapes, no coordinator.
  - id: pipeline-gate
    agent: sim-judge
    kind: judge
    needs: [pr-pipeline]
    max-turns: "40"
    retry: "1"
    on-failure: stop
    deny-write: ["**"]
    outputs: [verdict]
    judge:
      panel: "3"
      merge: majority
      max-feedback: "2"
    body: |
      GATE:pipeline — judge the per-PR output: one verdict file per PR
      under run-ledger/classifier/ and one draft per actionable PR.
      Count them with one bash pass. Your final message must be exactly
      the verdict JSON and nothing else.
  - id: pipeline-appeal
    agent: sim-judge
    kind: judge
    needs: [pipeline-gate]
    max-turns: "40"
    retry: "1"
    on-failure: stop
    deny-write: ["**"]
    outputs: [verdict]
    judge:
      panel: "1"
      merge: unanimous
      max-feedback: "0"
    body: |
      GATE:pipeline-appeal — re-derive the pipeline-gate rubric yourself
      and return the verdict JSON as your final message.
  - id: file-commit
    agent: sim-committer
    needs: [pipeline-appeal]
    max-turns: "40"
    retry: "0"
    on-failure: skip
    outputs: [issue-rows]
    body: |
      SIM COMMIT — file: record one outcome row per ready finding under
      run-ledger/outcomes/, then report the rows.
  - id: bump-commit
    agent: sim-committer
    needs: [pipeline-appeal]
    max-turns: "40"
    retry: "0"
    on-failure: skip
    outputs: [pr-rows]
    body: |
      SIM COMMIT — bump: record one outcome row per ready bump under
      run-ledger/outcomes/, then report the rows.
  - id: rollup-draft
    agent: sim-orchestrator
    needs: [prepare]
    max-turns: "60"
    retry: "0"
    on-failure: stop
    outputs: [delivery-drafts]
    body: |
      SIM ROLLUP — draft: fire ONE spawn_agent call (profile sim-rollup)
      carrying the state snapshot, then report the paths.
  - id: rollup-gate
    agent: sim-judge
    kind: judge
    needs: [rollup-draft]
    max-turns: "40"
    retry: "1"
    on-failure: stop
    deny-write: ["**"]
    outputs: [verdict]
    judge:
      panel: "3"
      merge: majority
      max-feedback: "2"
    body: |
      GATE:rollup — judge the delivery drafts under run-ledger/rollup/:
      every row must cite a verified artifact. Return the verdict JSON
      as your final message.
  - id: rollup-appeal
    agent: sim-judge
    kind: judge
    needs: [rollup-gate]
    max-turns: "40"
    retry: "1"
    on-failure: stop
    deny-write: ["**"]
    outputs: [verdict]
    judge:
      panel: "1"
      merge: unanimous
      max-feedback: "0"
    body: |
      GATE:rollup-appeal — re-derive the rollup-gate rubric yourself and
      return the verdict JSON as your final message.
  - id: rollup-commit
    agent: sim-committer
    needs: [rollup-appeal]
    max-turns: "40"
    retry: "0"
    on-failure: skip
    outputs: [delivery-rows]
    body: |
      SIM COMMIT — rollup: record the delivery outcome under
      run-ledger/outcomes/rollup.md, then report it.
---

Simulation rules: this plan is a local rehearsal of the judged adoption
workflow's per-PR architecture. Every artifact is a local file; no
GitHub API is called and no network is touched.
