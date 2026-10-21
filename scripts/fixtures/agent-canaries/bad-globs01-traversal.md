---
name: bad-globs01-traversal
description: >-
  Probes a traversing deny glob. Use when the gate must reject
  parent-directory escapes before the engine trusts them.
denyWrite: ../secrets/**
---
The deny glob traverses out of the workspace; fail closed.
