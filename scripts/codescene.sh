#!/usr/bin/env bash
# CodeScene code-health gate: `cs check` on every first-party Rust file,
# in parallel, with an mtime-keyed cache. Any CodeScene warning or error
# fails the gate. Standalone gate — deliberately not part of `just check`
# or CI (runners carry no CodeScene license).
#
# Fail closed: a scan that cannot run is a red gate, never a silent pass.
#
# Exit codes: 0 = clean, 1 = findings or failed scan, 2 = usage error.
set -euo pipefail
cd "$(dirname "$0")/.."

ROOTS=(crates) # keep in lockstep with check-file-length.sh
CACHE_DIR="scripts/codescene-cache"
MTIMES_FILE="$CACHE_DIR/mtimes.tsv"
RESULTS_FILE="$CACHE_DIR/results.tsv"
CS_VERSION_FILE="$CACHE_DIR/cs-version"
CS_BIN="${CODESCENE_BIN:-cs}"
DEFAULT_CONCURRENCY=4
MAX_RETRIES=3
# Transient license/network conditions worth one retry round.
TRANSIENT_RE='GOAWAY|License check failed|java\.io\.IOException|socket closed|ECONNRESET|ETIMEDOUT'
LICENSE_RE='License check failed|unauthorized'

RED=$'\033[1;31m'
GREEN=$'\033[1;32m'
YELLOW=$'\033[1;33m'
BLUE=$'\033[1;34m'
NC=$'\033[0m'

fail() {
  printf '%s✖ %s\033[0m\n' "$RED" "$*" >&2
  exit 1
}

note() {
  printf '%s%s\033[0m\n' "$BLUE" "$*"
}

print_help() {
  cat <<'EOF'
usage: codescene.sh [--scan] [--full] | --file <path> | --update

CodeScene code-health gate over the first-party Rust sources.

  (no args)         incremental scan; unchanged clean files skip via cache
  --scan            same as no args
  --scan --full     ignore the cache, rescan every file
  --file <path>     check one file, print the full cs output
  --update          update the CodeScene CLI itself

Environment:
  CS_CONCURRENCY   worker count (default 4, capped at nproc)
  CODESCENE_BIN    cs binary (default: cs)
  CS_ACCESS_TOKEN  CodeScene PAT, when the CLI needs one

Exit codes: 0 clean, 1 findings or failed scan, 2 usage error.
EOF
}

usage_err() {
  printf '✖ %s\n' "$*" >&2
  printf 'usage: codescene.sh [--scan] [--full] | --file <path> | --update\n' >&2
  exit 2
}


require_cs() {
  command -v "$CS_BIN" >/dev/null 2>&1 && return 0
  fail "CodeScene CLI ($CS_BIN) not found on PATH.
  Install it with:
    curl -fsSL https://downloads.codescene.io/enterprise/cli/install-cs-tool.sh | sh -s -- -y"
}

print_license_hint() {
  printf '%s🔑 The CodeScene license check failed. Generate a PAT at https://codescene.io/users/me/pat\n%s' \
    "$YELLOW" "$NC" >&2
  printf '%s   and export it in the environment: export CS_ACCESS_TOKEN=<pat>%s\n' "$YELLOW" "$NC" >&2
}

file_mtime() {
  stat -c %Y -- "$1" 2>/dev/null || printf '0'
}

# Fingerprint of the CodeScene CLI actually being run: version string plus
# binary digest. An upgrade or reinstall invalidates the results cache.
cs_fingerprint() {
  local ver digest
  ver="$(CS_DISABLE_VERSION_CHECK=true "$CS_BIN" --version 2>/dev/null | head -n1)" || ver=""
  digest="$(sha256sum -- "$(command -v "$CS_BIN")" 2>/dev/null | cut -d' ' -f1)" || digest=""
  printf '%s|%s' "$ver" "$digest"
}

# Worker body: one `cs check` with retry rounds on transient license/network
# conditions. All output goes to $2; prints the verdict on stdout:
# perfect | warning | error. Never prints anything else, never fails loud —
# the caller reads the verdict.
scan_file() {
  local file="$1" log="$2" rc=0 attempt=1
  : >"$log"
  while :; do
    rc=0
    CS_DISABLE_VERSION_CHECK=true "$CS_BIN" check "$file" >"$log" 2>&1 || rc=$?
    if [ "$attempt" -lt "$MAX_RETRIES" ] && grep -Eqi "$TRANSIENT_RE" "$log"; then
      attempt=$((attempt + 1))
      sleep "$(awk "BEGIN { print $attempt * 0.4 }")"
      continue
    fi
    break
  done
  if grep -Eqi "^(error|err):" "$log" || grep -Eqi "$LICENSE_RE" "$log" || [ "$rc" -ne 0 ]; then
    if grep -Eqi "$LICENSE_RE" "$log"; then
      printf 'license\n' >"${log%.log}.license"
    fi
    printf 'error\n'
  elif grep -Ei '^(warn|warning|error):' "$log" | grep -Eviq 'Code health score:'; then
    printf 'warning\n'
  else
    printf 'perfect\n'
  fi
}

# xargs child: scan one file, then leave the verdict where the parent
# finds it: <workdir>/<key>.status holds "status<TAB>mtime".
check_one() {
  local file="$1"
  local key log status mtime
  key="$(printf '%s' "$file" | sha256sum | cut -d' ' -f1)"
  log="$CS_WORK_DIR/$key.log"
  status="$(scan_file "$file" "$log")"
  mtime="$(file_mtime "$file")"
  printf '%s\t%s\n' "$status" "$mtime" >"$CS_WORK_DIR/$key.status"
}


load_cache() {
  local f s m
  [ -f "$RESULTS_FILE" ] && while IFS=$'\t' read -r f s; do
    case "$s" in
      perfect|warning) cached_status[$f]="$s" ;;
    esac
  done <"$RESULTS_FILE" || true
  [ -f "$MTIMES_FILE" ] && while IFS=$'\t' read -r f m; do
    cached_mtime[$f]="$m"
  done <"$MTIMES_FILE" || true
}

# Rewrite both cache halves from one in-memory state so they never drift.
write_cache() {
  local f s m
  mkdir -p "$CACHE_DIR"
  {
    for f in "${files[@]}"; do
      s="${cached_status[$f]:-}"
      m="${cached_mtime[$f]:-}"
      if [ -n "$s" ] && { [ "$s" = perfect ] || [ "$s" = warning ]; } && [ -n "$m" ]; then
        printf '%s\t%s\n' "$f" "$s"
      fi
    done
  } >"$RESULTS_FILE.tmp" && mv "$RESULTS_FILE.tmp" "$RESULTS_FILE"
  {
    for f in "${files[@]}"; do
      s="${cached_status[$f]:-}"
      m="${cached_mtime[$f]:-}"
      if [ -n "$s" ] && { [ "$s" = perfect ] || [ "$s" = warning ]; } && [ -n "$m" ]; then
        printf '%s\t%s\n' "$f" "$m"
      fi
    done
  } >"$MTIMES_FILE.tmp" && mv "$MTIMES_FILE.tmp" "$MTIMES_FILE"
}

resolve_concurrency() {
  local n cpus
  n="${CS_CONCURRENCY:-$DEFAULT_CONCURRENCY}"
  case "$n" in ''|*[!0-9]*) n=$DEFAULT_CONCURRENCY ;; esac
  cpus="$(nproc 2>/dev/null)" || cpus=1
  [ "$cpus" -ge 1 ] || cpus=1
  if [ "$n" -gt "$cpus" ]; then n=$cpus; fi
  [ "$n" -ge 1 ] || n=1
  printf '%s' "$n"
}


run_scan() {
  local force_full="$1" concurrency
  require_cs
  concurrency="$(resolve_concurrency)"

  # A CodeScene upgrade or reinstall invalidates the cache: verdicts are
  # version-keyed, so a cached pass under an older CLI must not survive.
  local fp
  fp="$(cs_fingerprint)"
  if [ "$force_full" = no ] && [ -f "$CS_VERSION_FILE" ] \
    && [ "$(cat "$CS_VERSION_FILE")" != "$fp" ]; then
    note "CodeScene CLI version changed; forcing full scan"
    force_full=yes
  fi

  local files=() f
  while IFS= read -r -d '' f; do
    files+=("$f")
  done < <(find "${ROOTS[@]}" -type d \( -name '.*' -o -name 'target' \) -prune -o \
    -type f -name '*.rs' -print0 | sort -z)
  [ "${#files[@]}" -gt 0 ] || fail "no .rs files found under ${ROOTS[*]}"

  declare -A cached_status=() cached_mtime=()
  load_cache

  local needs=() cached_perfect=0 cached_warn=0 mtime
  local warn_files=() err_files=() perfect=0
  for f in "${files[@]}"; do
    mtime="$(file_mtime "$f")"
    if [ "$force_full" = no ] && [ "${cached_status[$f]:-}" = perfect ] \
      && [ "${cached_mtime[$f]:-}" = "$mtime" ]; then
      cached_perfect=$((cached_perfect + 1))
      continue
    fi
    if [ "$force_full" = no ] && [ "${cached_status[$f]:-}" = warning ] \
      && [ "${cached_mtime[$f]:-}" = "$mtime" ]; then
      warn_files+=("$f")
      cached_warn=$((cached_warn + 1))
      continue
    fi
    needs+=("$f")
  done

  note "CodeScene scan: ${#files[@]} file(s), ${#needs[@]} to check, $cached_perfect cached clean, $concurrency worker(s)"

  if [ "${#needs[@]}" -gt 0 ]; then
    local workdir
    workdir="$(mktemp -d)"
    export CS_WORK_DIR="$workdir"
    cleanup_workdir() { rm -rf -- "${CS_WORK_DIR:-}" 2>/dev/null || true; }
    trap cleanup_workdir EXIT
    trap 'cleanup_workdir; exit 143' TERM
    trap 'cleanup_workdir; exit 130' INT
    export CS_BIN MAX_RETRIES TRANSIENT_RE LICENSE_RE
    export -f scan_file check_one file_mtime
    printf '%s\0' "${needs[@]}" \
      | xargs -0 -r -P "$concurrency" -n1 bash -c 'check_one "$1"' _
    local key status fresh_mtime missing=0
    for f in "${needs[@]}"; do
      key="$(printf '%s' "$f" | sha256sum | cut -d' ' -f1)"
      if [ ! -f "$CS_WORK_DIR/$key.status" ]; then
        err_files+=("$f")
        missing=1
        continue
      fi
      IFS=$'\t' read -r status fresh_mtime <"$CS_WORK_DIR/$key.status"
      case "$status" in
        perfect)
          perfect=$((perfect + 1))
          cached_status[$f]=perfect
          cached_mtime[$f]="$fresh_mtime"
          ;;
        warning)
          warn_files+=("$f")
          cached_status[$f]=warning
          cached_mtime[$f]="$fresh_mtime"
          ;;
        *)
          err_files+=("$f")
          unset "cached_status[$f]" 2>/dev/null || true
          unset "cached_mtime[$f]" 2>/dev/null || true
          ;;
      esac
    done
    if compgen -G "$CS_WORK_DIR/*.license" >/dev/null; then
      print_license_hint
    fi
    rm -rf -- "$CS_WORK_DIR"
    unset CS_WORK_DIR
    trap - EXIT TERM INT
    if [ "$missing" -ne 0 ]; then
      printf '%s✖ %s worker verdict(s) missing — counted as errors (fail closed)\033[0m\n' \
        "$RED" "$missing" >&2
    fi
  fi

  local warns=0 errs=0
  for f in "${warn_files[@]}"; do
    printf '%s⚠  %s\033[0m\n' "$YELLOW" "$f" >&2
  done
  for f in "${err_files[@]}"; do
    printf '%s✖ %s\033[0m\n' "$RED" "$f" >&2
  done
  warns=${#warn_files[@]}
  errs=${#err_files[@]}

  write_cache
  if [ "$errs" -eq 0 ]; then
    printf '%s\n' "$(cs_fingerprint)" >"$CS_VERSION_FILE"
  fi

  printf '\n'
  printf '━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n'
  printf '%s📈 Code health summary\033[0m\n' "$BLUE"
  printf '━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n'
  printf '%s✔ Perfect: %s\033[0m\n' "$GREEN" "$((perfect + cached_perfect))"
  printf '%s⚠  With warnings: %s\033[0m\n' "$YELLOW" "$warns"
  printf '%s✖ With errors: %s\033[0m\n' "$RED" "$errs"
  printf '%s⚡ Served from cache: %s\033[0m\n' "$BLUE" "$((cached_perfect + cached_warn))"
  printf '━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n'
  [ "$warns" -eq 0 ] && [ "$errs" -eq 0 ] || return 1
  return 0
}


# One file, full cs output printed, no cache involvement.
run_file() {
  local file="$1" log status
  require_cs
  [ -f "$file" ] || fail "not a readable file: $file"
  log="$(mktemp)"
  trap 'rm -f -- "$log"' EXIT
  status="$(scan_file "$file" "$log")"
  cat "$log"
  case "$status" in
    perfect)
      printf '%s✔ %s: perfect health\033[0m\n' "$GREEN" "$file"
      ;;
    warning)
      printf '%s⚠  %s: warnings found\033[0m\n' "$YELLOW" "$file" >&2
      rm -f -- "$log"
      trap - EXIT
      return 1
      ;;
    *)
      if grep -Eqi "$LICENSE_RE" "$log"; then
        print_license_hint
      fi
      printf '%s✖ %s: errors found\033[0m\n' "$RED" "$file" >&2
      rm -f -- "$log"
      trap - EXIT
      return 1
      ;;
  esac
  rm -f -- "$log"
  trap - EXIT
}

run_update() {
  require_cs
  note "Updating the CodeScene CLI..."
  "$CS_BIN" update
}

main() {
  local mode=scan force_full=no file=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --scan) mode=scan ;;
      --full) force_full=yes ;;
      --file)
        [ "$#" -ge 2 ] || usage_err "--file requires a path argument"
        mode=file
        file="$2"
        shift
        ;;
      --update) mode=update ;;
      --help|-h)
        print_help
        exit 0
        ;;
      *)
        usage_err "unknown argument: $1"
        ;;
    esac
    shift
  done

  case "$mode" in
    scan) run_scan "$force_full" ;;
    file) run_file "$file" ;;
    update) run_update ;;
  esac
}

main "$@"
