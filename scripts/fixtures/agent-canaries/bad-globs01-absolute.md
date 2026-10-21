---
name: bad-globs01-absolute
description: >-
  Probes an absolute deny glob. Use when the gate must reject
  workspace-escaping globs before the engine trusts them.
denyRead: /etc/**
---
The deny glob is absolute; the gate must fail closed here.
