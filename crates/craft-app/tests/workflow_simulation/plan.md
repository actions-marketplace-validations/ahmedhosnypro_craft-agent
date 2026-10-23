---
name: sim-adopt
description: "Synthetic rehearsal of the judged adoption workflow: the same 16-step architecture — collect, fan-out waves, judge gates, appeals, draft/commit split — driven by a scripted model over local fixtures, with no network and no real GitHub."
args:
  window: { type: string, default: "7h" }
limits:
  max-steps: "16"
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
      SIM PHASE 1 — collect and clone: fire ONE spawn_agents call
      (profile sim-cloner) with six items, one per synthetic app repo,
      then report the count. Children clone nothing; they record their
      outcome locally under run-ledger/clone/<app>/.
  - id: classify
    agent: sim-orchestrator
    needs: [prepare]
    max-turns: "60"
    retry: "0"
    on-failure: stop
    outputs: [batch-reports]
    body: |
      SIM PHASE 2 — classify: shard the six synthetic PRs into two
      batches of three and fire ONE spawn_agents call (profile
      sim-classifier), then report the batch count.
  - id: classify-gate
    agent: sim-judge
    kind: judge
    needs: [classify]
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
      GATE:classify — judge the classification step: the batch reports and
      verdict files under run-ledger/classifier/ must all exist and their
      counts must match. Count them with one bash pass. Your final
      message must be exactly the verdict JSON and nothing else.
  - id: classify-appeal
    agent: sim-judge
    kind: judge
    needs: [classify-gate]
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
      GATE:classify-appeal — re-derive the classify-gate rubric yourself
      and return the verdict JSON as your final message.
  - id: file-draft
    agent: sim-orchestrator
    needs: [classify-appeal]
    max-turns: "60"
    retry: "0"
    on-failure: stop
    outputs: [filer-outcomes]
    body: |
      SIM DRAFT — file: fire ONE spawn_agents call (profile sim-filer)
      with two items, one per synthetic finding, then report the count.
  - id: file-gate
    agent: sim-judge
    kind: judge
    needs: [file-draft]
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
      GATE:file — judge the drafted findings: both drafts must exist
      under run-ledger/filer/ and cite their evidence. Return the
      verdict JSON as your final message.
  - id: file-appeal
    agent: sim-judge
    kind: judge
    needs: [file-gate]
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
      GATE:file-appeal — re-derive the file-gate rubric yourself and
      return the verdict JSON as your final message.
  - id: file-commit
    agent: sim-committer
    needs: [file-appeal]
    max-turns: "40"
    retry: "0"
    on-failure: skip
    outputs: [issue-rows]
    body: |
      SIM COMMIT — file: record one outcome row per ready finding under
      run-ledger/outcomes/, then report the rows.
  - id: bump-draft
    agent: sim-orchestrator
    needs: [classify-appeal]
    max-turns: "60"
    retry: "0"
    on-failure: stop
    outputs: [applier-outcomes]
    body: |
      SIM DRAFT — bump: fire ONE spawn_agents call (profile sim-applier)
      with two items, one per synthetic bump candidate, then report.
  - id: bump-gate
    agent: sim-judge
    kind: judge
    needs: [bump-draft]
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
      GATE:bump — judge the bump drafts: both branch packages must exist
      under run-ledger/revsync/ and carry a pin rev. Return the verdict
      JSON as your final message.
  - id: bump-appeal
    agent: sim-judge
    kind: judge
    needs: [bump-gate]
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
      GATE:bump-appeal — re-derive the bump-gate rubric yourself and
      return the verdict JSON as your final message.
  - id: bump-commit
    agent: sim-committer
    needs: [bump-appeal]
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
      SIM ROLLUP — draft: fire ONE spawn_agent call (profile
      sim-rollup) carrying the state snapshot, then report the paths.
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
workflow's architecture. Every artifact is a local file; no GitHub API
is called and no network is touched.
