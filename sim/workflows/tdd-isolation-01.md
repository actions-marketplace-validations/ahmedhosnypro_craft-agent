---
name: tdd-isolation-01
description: "TDD isolation rehearsal — a blind implementer denied the test corpus, judged by a verdict gate."
args:
  spec: { type: string, default: "A greet(name) function in src/greeting.py returning 'hello, {name}'." }
limits:
  max-steps: 3
  max-total-turns: 30
  max-total-tokens: 4000000
  max-retries-per-step: 1
steps:
  - id: write-tests
    agent: spec-writer
    needs: []
    max-turns: 6
    retry: 0
    on-failure: stop
    outputs: [greeting-test]
    body: |
      Write the failing test for this spec: {{arg.spec}} The test file
      lands at tests/greeting_test.py in the workspace and fails against
      an empty src/. Run it once to prove it fails, then report the
      exact failure line. Never touch src/.
  - id: implement
    agent: blind-implementer
    needs: [write-tests]
    deny-read:
      - tests/**
    max-turns: 8
    retry: 1
    on-failure: stop
    outputs: [greeting-impl]
    body: |
      Implement {{arg.spec}} Write to src/greeting.py only. You cannot
      read the test corpus: the deny scope makes every read of tests/**
      fail closed, bash included, so verify by reading your own code and
      the spec, never by opening the tests. Final message: the files you
      wrote and the contract you implemented.
  - id: gate
    kind: judge
    agent: reviewer
    needs: [implement]
    judge:
      panel: 1
      merge: unanimous
      max-feedback: 2
    max-turns: 4
    body: |
      Judge the implementation. Run the suite with
      `python3 -m pytest tests/ -q` and answer with exactly one JSON
      object: {"verdict":"pass"} on green, or {"verdict":"fail",
      "issues":["..."]} naming the concrete failing expectations on red.
      Never edit any test; never weaken the suite.
---

# Rules (absolute) — prepended to every step

- The implementer never opens the test corpus — not with read_file, not
  with bash, not with grep. The deny scope is the contract, and a
  failure to read is the correct outcome, never a workaround.
- The judge never edits the suite and never writes under tests/.
- Every write lands in the workspace only: src/greeting.py, tests/, and
  the report.
- Measured outcomes only: the judge cites the failing expectation lines.
- The workspace starts empty apart from this plan's fixtures: create
  src/ if missing and keep an __init__.py-free layout runnable with
  plain `python3 -m pytest`.
