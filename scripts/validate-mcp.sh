#!/usr/bin/env bash
# mcp.json validator. Lints every discoverable .craft-agent/mcp.json
# (or explicit files): the mcpServers object, the closed server-entry
# key set, the stdio-only transport (a `url` or a non-"stdio" `type`
# is a typed v2-later error), server-name charset, absolute in-workspace
# cwd, non-empty command, provider-key-free env overlays, and bounded
# timeouts and roster size. jq-based; runtime validation never trusts
# that this gate ran.
#
# Modes:
#   (no argument)  scan the tree for .craft-agent/mcp.json files. No
#                  file anywhere is a printed skip (exit 0); invalid
#                  present files fail the gate.
#   --selftest     prove every rule on the canary fixtures — each rule
#                  fires on its labeled bad canary, nothing fires on
#                  the good canary, the fail-closed probes hold, and
#                  the tree scan passes clean.
#   <path>...      per-file checker mode.
#
# Exit codes: 0 = clean, 1 = findings OR a broken scan (a scan that
# cannot run must never pass silently), 2 = usage error.
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
  printf 'usage: %s [--selftest | <mcp.json>...]\n' "$0" >&2
  exit 2
}

case "${1:-}" in
  "") scan_mode=tree ;;
  --selftest) scan_mode=selftest ;;
  -*) usage ;;
  *) scan_mode=paths ;;
esac
if [ "$scan_mode" = "selftest" ] && [ "$#" -gt 1 ]; then
  usage
fi

if ! command -v jq >/dev/null 2>&1; then
  printf 'mcp validate: jq is required but not installed (failing closed)\n' >&2
  exit 1
fi

FILE_BYTE_CAP=65536
MAX_SERVERS=8
KNOWN_KEYS=" command args env cwd enabled enabledTools disabledTools startupTimeoutSecs toolTimeoutSecs url type "
FIXTURE_DIR="scripts/fixtures/mcp-canaries"
NAME_RE='^[a-z][a-z0-9_-]{0,31}$'

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM

# fail_rule <file> <rule> <message>
fail_rule() {
  printf '%s: %s %s\n' "$1" "$2" "$3" >&2
}

# validate_file <path> — one config file; findings to stderr, rc 0/1.
validate_file() {
  local file="$1" found=0 size
  if [ ! -f "$file" ] || [ ! -r "$file" ]; then
    printf 'mcp validate: cannot read %s (failing closed)\n' "$file" >&2
    return 1
  fi
  size="$(wc -c < "$file")"
  if [ "$size" -gt "$FILE_BYTE_CAP" ]; then
    fail_rule "$file" MCP02 "file is ${size} bytes (cap ${FILE_BYTE_CAP})"
    found=1
  fi
  if ! jq -e . "$file" >/dev/null 2>&1; then
    fail_rule "$file" MCP01 "not valid JSON"
    return 1
  fi
  if [ "$(jq -r 'type' "$file")" != "object" ] || ! jq -e 'has("mcpServers")' "$file" >/dev/null; then
    fail_rule "$file" MCP03 "the root must be an object with a mcpServers key"
    return 1
  fi
  if [ "$(jq -r '.mcpServers | type' "$file")" != "object" ]; then
    fail_rule "$file" MCP03 "mcpServers must be a JSON object of server entries"
    return 1
  fi
  if [ "$(jq -r '.mcpServers | length' "$file")" -gt "$MAX_SERVERS" ]; then
    fail_rule "$file" MCP04 "the file configures more than ${MAX_SERVERS} servers"
    found=1
  fi
  validate_servers "$file" || found=1
  return "$found"
}

# validate_servers <file> — per-entry rules via jq.
validate_servers() {
  local file="$1" bad=0 name
  while IFS= read -r name; do
    [ -n "$name" ] || continue
    if ! printf '%s' "$name" | grep -Eq "$NAME_RE"; then
      fail_rule "$file" MCP05 "server name '$name' must match ^[a-z][a-z0-9_-]{0,31}\$"
      bad=1
    fi
    local key
    while IFS= read -r key; do
      case "$KNOWN_KEYS" in
        *" $key "*) ;;
        *) fail_rule "$file" MCP06 "server '$name': unknown key '$key'"
           bad=1 ;;
      esac
    done < <(jq -r --arg n "$name" '.mcpServers[$n] | keys[]?' "$file")
    if [ "$(jq -r --arg n "$name" '.mcpServers[$n].command // ""' "$file")" = "" ]; then
      fail_rule "$file" MCP07 "server '$name': command must not be empty"
      bad=1
    fi
    if jq -e --arg n "$name" '.mcpServers[$n].url != null' "$file" >/dev/null 2>&1; then
      fail_rule "$file" MCP08 "server '$name': url servers need the HTTP transport, which this version does not speak"
      bad=1
    fi
    local kind
    kind="$(jq -r --arg n "$name" '.mcpServers[$n].type // "stdio"' "$file")"
    if [ "$kind" != "stdio" ]; then
      fail_rule "$file" MCP09 "server '$name': type must be \"stdio\", got \"$kind\""
      bad=1
    fi
    local cwd
    cwd="$(jq -r --arg n "$name" '.mcpServers[$n].cwd // ""' "$file")"
    if [ -n "$cwd" ] && { case "$cwd" in /*) false ;; *) true ;; esac; }; then
      fail_rule "$file" MCP10 "server '$name': cwd must be an absolute path inside the workspace"
      bad=1
    fi
    if [ -n "$cwd" ] && printf '%s' "$cwd" | grep -qE '(^|/)\.\.(/|$)'; then
      fail_rule "$file" MCP10 "server '$name': cwd '$cwd' escapes the workspace root"
      bad=1
    fi
    local envkey
    while IFS= read -r envkey; do
      [ -n "$envkey" ] || continue
      if [[ "$envkey" == "API_KEY" || "$envkey" == *_API_KEY || "$envkey" == TEST_BYOK_* ]]; then
        fail_rule "$file" MCP11 "server '$name': env key '$envkey' is a provider key and never reaches servers"
        bad=1
      fi
    done < <(jq -r --arg n "$name" '.mcpServers[$n].env // {} | keys[]?' "$file")
    local field
    for field in startupTimeoutSecs toolTimeoutSecs; do
      if jq -e --arg n "$name" --arg f "$field" \
        '(.mcpServers[$n][$f] // 1) as $v | ($v < 1 or $v > 86400)' "$file" >/dev/null 2>&1; then
        fail_rule "$file" MCP12 "server '$name': $field must be in 1..=86400"
        bad=1
      fi
    done
  done < <(jq -r '.mcpServers | keys[]?' "$file")
  return "$bad"
}

# scan_tree — the gate scan: every .craft-agent/mcp.json in the tree.
scan_tree() {
  local -a files=()
  mapfile -t files < <(find . -type f -path '*/.craft-agent/mcp.json' -not -path './target/*' | sort)
  if [ "${#files[@]}" -eq 0 ]; then
    printf 'mcp validate: no mcp.json discovered in the tree — skipped\n'
    return 0
  fi
  local f found=0
  for f in "${files[@]}"; do
    if ! validate_file "$f"; then
      found=1
    fi
  done
  if [ "$found" -ne 0 ]; then
    printf 'mcp validate FAIL: findings above (%s files)\n' "${#files[@]}" >&2
    return 1
  fi
  printf 'mcp validate OK: 0 findings (%s files)\n' "${#files[@]}"
}

check_paths() {
  local f found=0
  for f in "$@"; do
    if ! validate_file "$f"; then
      found=1
    fi
  done
  if [ "$found" -ne 0 ]; then
    printf 'mcp validate FAIL: findings above (%s files)\n' "$#" >&2
    return 1
  fi
  printf 'mcp validate OK: 0 findings (%s files)\n' "$#"
}

CANARIES=(
  good.json
  bad-mcp01-not-json.json
  bad-mcp02-oversize.json
  bad-mcp03-no-servers-key.json
  bad-mcp04-nine-servers.json
  bad-mcp05-name-charset.json
  bad-mcp06-unknown-key.json
  bad-mcp07-empty-command.json
  bad-mcp08-url.json
  bad-mcp09-type-http.json
  bad-mcp10-relative-cwd.json
  bad-mcp11-provider-env.json
  bad-mcp12-timeout-range.json
)
EXPECT=(
  "MCP01 bad-mcp01-not-json.json"
  "MCP02 bad-mcp02-oversize.json"
  "MCP03 bad-mcp03-no-servers-key.json"
  "MCP04 bad-mcp04-nine-servers.json"
  "MCP05 bad-mcp05-name-charset.json"
  "MCP06 bad-mcp06-unknown-key.json"
  "MCP07 bad-mcp07-empty-command.json"
  "MCP08 bad-mcp08-url.json"
  "MCP09 bad-mcp09-type-http.json"
  "MCP10 bad-mcp10-relative-cwd.json"
  "MCP11 bad-mcp11-provider-env.json"
  "MCP12 bad-mcp12-timeout-range.json"
)

run_selftest() {
  local failed=0 fixture rule entry rc cap="$work/cap"
  if [ ! -d "$FIXTURE_DIR" ]; then
    printf 'mcp validate selftest: canary fixtures missing: %s\n' "$FIXTURE_DIR" >&2
    return 1
  fi
  for fixture in "${CANARIES[@]}"; do
    if [ ! -f "$FIXTURE_DIR/$fixture" ]; then
      printf 'mcp validate selftest: canary fixture missing: %s\n' "$fixture" >&2
      failed=1
    fi
  done
  for entry in "${EXPECT[@]}"; do
    rule="${entry%% *}"
    fixture="${entry#* }"
    [ -f "$FIXTURE_DIR/$fixture" ] || continue
    rc=0
    validate_file "$FIXTURE_DIR/$fixture" 2>"$cap" || rc=$?
    if [ "$rc" -eq 0 ] || ! grep -q "$rule" "$cap"; then
      printf 'mcp validate selftest: rule %s did not fire on %s\n' "$rule" "$fixture" >&2
      failed=1
    fi
  done
  rc=0
  validate_file "$FIXTURE_DIR/good.json" 2>"$cap" || rc=$?
  if [ "$rc" -ne 0 ] || [ -s "$cap" ]; then
    printf 'mcp validate selftest: the good canary is not clean\n' >&2
    cat "$cap" >&2
    failed=1
  fi
  if ! validate_file "$work/missing.json" >/dev/null 2>&1; then
    :
  else
    printf 'mcp validate selftest: an unreadable path did not fail closed\n' >&2
    failed=1
  fi
  if ! scan_tree >"$cap" 2>&1; then
    printf 'mcp validate selftest: the tree scan is not clean\n' >&2
    cat "$cap" >&2
    failed=1
  fi
  if [ "$failed" -ne 0 ]; then
    printf 'mcp validate selftest FAILED — a rule is unfalsifiable or over-broad\n' >&2
    return 1
  fi
  printf 'mcp validate selftest OK: every rule fires on its canary; good canary clean; fail-closed holds; tree scan clean\n'
}

case "$scan_mode" in
  selftest)
    if ! run_selftest; then
      exit 1
    fi
    ;;
  paths)
    for arg in "$@"; do
      case "$arg" in
        -*) usage ;;
      esac
    done
    if ! check_paths "$@"; then
      exit 1
    fi
    ;;
  *)
    if ! scan_tree; then
      exit 1
    fi
    ;;
esac
