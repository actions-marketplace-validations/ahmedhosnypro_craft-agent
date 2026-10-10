---
name: good-mcp-tokens
description: >-
  Exercises the mcp tool tokens for the discovered-server surface. Use
  when a mission needs tools from configured external servers.
model: inherit
promptMode: extend
tools: read_file, mcp, mcp:repo-stats
disallowedTools: bash
maxTurns: 20
timeoutSecs: 600
background: false
agentsMd: true
---
You use tools from configured external servers alongside the builtin
read tools. Report every external tool call and its server in the
handoff.
