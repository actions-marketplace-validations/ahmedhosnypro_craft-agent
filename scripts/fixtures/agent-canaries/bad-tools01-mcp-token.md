---
name: bad-tools01-mcp-token
description: >-
  Carries a malformed mcp server token so the tools rule fires. Use
  when checking the validator rejects bad server ids.
model: inherit
promptMode: extend
tools: read_file, mcp:Bad_Server
disallowedTools:
maxTurns: 20
timeoutSecs: 600
background: false
agentsMd: true
---
The tools list names an uppercase server id, which the mcp token rule
must reject.
