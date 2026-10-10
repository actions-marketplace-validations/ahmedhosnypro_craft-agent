# Mission: use MCP server tools, then survive a server dying mid-run

You are craft-agent running in a runner simulation. This repository
is craft-agent itself. The workspace carries an `.craft-agent/mcp.json`
configuring two stdio servers from the dist fixture, and the run is
started with `CRAFT_AGENT_MCP=1`.

## Rules (absolute)

- Work only inside this workspace. Never touch any other repository,
  and never call gh against the storytold organization.
- This mission expects no branch, no commit, no pull request.
- Never print environment variables or secrets.

## Your task

1. Call the `mcp__fixture__echo` tool with `{"msg": "mcp-mission-ok"}`
   and quote the result in your final message.
2. Call `mcp__fixture-doomed__echo` with `{"msg": "any"}`. That server
   dies on every call; read the typed error result, say plainly the
   server failed, and continue.
3. Finish with a handoff naming both tool results and the two servers
   the run connected.

## How this mission is run (operator)

    CRAFT_AGENT_MCP=1 sim/run-sim.sh mcp-01

Prerequisites beyond the usual sim knobs: `dist/` must carry
`craft-mcp-fixture-x86_64-unknown-linux-musl` (built by
scripts/build-release.sh), the cloned workspace must carry
`.craft-agent/mcp.json` naming `/dist/craft-mcp-fixture-x86_64-unknown-linux-musl`
as both servers' command (the doomed one with `--die-on-call`), and the
app wiring must connect MCP servers before turn 1 and bridge the
`mcp_server_status` / `mcp_tools_discovered` events onto the stream.

## Expected conclusion (operator)

Exit 0, `conclusion=completed`. With `<run>` the new sim/runs entry:

- `grep mcp_server_status <run>/out/stream.jsonl` — two `connected`
  status lines (`fixture`, `fixture-doomed`).
- `grep mcp_tools_discovered <run>/out/stream.jsonl` — one discovery
  record per server with a sha256 hash and the enabled names.
- One `tool_result` for `mcp__fixture__echo` with `is_error:false` and
  the echoed marker.
- One `tool_result` for `mcp__fixture-doomed__echo` with `is_error:true`
  naming the server failure — the run does not die with it.
