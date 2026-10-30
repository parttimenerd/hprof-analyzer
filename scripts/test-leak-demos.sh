#!/usr/bin/env bash
# =============================================================================
# test-leak-demos.sh — Run hprof-analyzer against all 34 leak-demo heap dumps
#                      and capture every error, panic, or unexpected failure.
#
# Usage (run from repo root):
#   ./scripts/test-leak-demos.sh                  # build, generate dumps, test
#   ./scripts/test-leak-demos.sh --no-capture     # skip dump generation (use existing)
#   ./scripts/test-leak-demos.sh --no-build       # skip cargo build
#   ./scripts/test-leak-demos.sh --only-capture   # only generate dumps, don't test
#   ./scripts/test-leak-demos.sh 01 02 31         # test only specific scenarios
#   BIN=/path/to/binary ./scripts/test-leak-demos.sh   # use a prebuilt binary
#
# Output:
#   leak-demos/test-results/
#     report.md      — human-readable pass/fail summary
#     errors.md      — all captured errors with context
#     <NN>_*.log     — per-scenario per-command output
#
# Prerequisites (for dump generation):
#   java 11+, jcmd and jmap on PATH (from same JDK)
#
# Prerequisites (for analysis):
#   Rust toolchain (cargo), OR a prebuilt binary via $BIN
# =============================================================================
set -euo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || dirname "$(dirname "${BASH_SOURCE[0]}")")"

# ── Argument parsing ──────────────────────────────────────────────────────────
DO_BUILD=true
DO_CAPTURE=true
DO_TEST=true
SCENARIO_FILTER=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build)    DO_BUILD=false; shift ;;
    --no-capture)  DO_CAPTURE=false; shift ;;
    --only-capture) DO_TEST=false; shift ;;
    --no-test)     DO_TEST=false; shift ;;
    --help|-h)
      sed -n '2,20p' "$0" | sed 's/^# \?//'
      exit 0 ;;
    [0-9]*)        SCENARIO_FILTER+=("$1"); shift ;;
    *)             echo "Unknown arg: $1" >&2; exit 1 ;;
  esac
done

# ── Paths ─────────────────────────────────────────────────────────────────────
LEAK_DEMOS="leak-demos"
DUMPS_DIR="$LEAK_DEMOS/dumps"
RESULTS_DIR="$LEAK_DEMOS/test-results"
REPORT="$RESULTS_DIR/report.md"
ERRORS="$RESULTS_DIR/errors.md"
mkdir -p "$RESULTS_DIR" "$RESULTS_DIR/logs"

# ── Binary ────────────────────────────────────────────────────────────────────
BIN="${BIN:-}"
if [[ -z "$BIN" ]]; then
  BIN="target/release/hprof-analyzer"
fi

# ── Colour helpers ────────────────────────────────────────────────────────────
if [[ -t 1 ]]; then
  R='\033[0m' B='\033[1m' G='\033[0;32m' Y='\033[1;33m' RED='\033[0;31m' C='\033[0;36m' D='\033[2m'
else
  R='' B='' G='' Y='' RED='' C='' D=''
fi
ts()   { date +%H:%M:%S; }
hdr()  { echo -e "\n${B}${C}═══ $* ═══${R}"; }
log()  { echo -e "${D}[$(ts)]${R} $*"; }
ok()   { echo -e "  ${G}PASS${R}  $*"; }
fail() { echo -e "  ${RED}FAIL${R}  $*"; }
skip() { echo -e "  ${Y}SKIP${R}  $*"; }

# ── Counters ──────────────────────────────────────────────────────────────────
PASS=0 FAIL=0 SKIP=0
declare -a ERROR_LOG=()

record_error() {
  local scenario="$1" cmd="$2" detail="$3"
  ERROR_LOG+=("### $scenario — \`$cmd\`" "" '```' "$detail" '```' "")
  (( FAIL++ )) || true
}

record_pass() { (( PASS++ )) || true; }
record_skip() { (( SKIP++ )) || true; }

# =============================================================================
# STEP 1 — Build
# =============================================================================
if $DO_BUILD; then
  hdr "Step 1 — Build hprof-analyzer (release)"
  if [[ -n "${BIN:-}" && -x "$BIN" && "$BIN" != "target/release/hprof-analyzer" ]]; then
    log "Using prebuilt binary: $BIN"
  else
    log "cargo build --release …"
    if ! cargo build --release 2>&1; then
      echo -e "${RED}Build failed. Aborting.${R}" >&2
      exit 1
    fi
    BIN="target/release/hprof-analyzer"
    ok "Built $BIN"
  fi
else
  log "Skipping build (--no-build)"
  if [[ ! -x "$BIN" ]]; then
    echo -e "${RED}Binary not found at $BIN. Run without --no-build, or set BIN=.${R}" >&2
    exit 1
  fi
fi

log "Binary: $("$BIN" --version 2>/dev/null || echo "$BIN (version unknown)")"

# =============================================================================
# STEP 2 — Capture dumps
# =============================================================================
if $DO_CAPTURE; then
  hdr "Step 2 — Capture heap dumps (leak-demos/capture_all.sh)"
  if [[ ! -f "$LEAK_DEMOS/capture_all.sh" ]]; then
    echo -e "${RED}$LEAK_DEMOS/capture_all.sh not found.${R}" >&2
    exit 1
  fi
  chmod +x "$LEAK_DEMOS/capture_all.sh"

  CAPTURE_ARGS=()
  if [[ ${#SCENARIO_FILTER[@]} -gt 0 ]]; then
    CAPTURE_ARGS=("${SCENARIO_FILTER[@]}")
  fi

  DUMPS_DIR="$DUMPS_DIR" "$LEAK_DEMOS/capture_all.sh" "${CAPTURE_ARGS[@]}" || {
    echo -e "${Y}capture_all.sh exited non-zero; some dumps may be missing.${R}"
  }
else
  log "Skipping capture (--no-capture)"
fi

# =============================================================================
# STEP 3 — Test each dump
# =============================================================================
if ! $DO_TEST; then
  echo -e "${G}Done (capture only).${R}"
  exit 0
fi

hdr "Step 3 — Analyse dumps with hprof-analyzer"

# Collect dumps to test
if [[ ${#SCENARIO_FILTER[@]} -gt 0 ]]; then
  DUMP_FILES=()
  for n in "${SCENARIO_FILTER[@]}"; do
    for f in "$DUMPS_DIR"/${n}*.hprof; do
      [[ -f "$f" ]] && DUMP_FILES+=("$f")
    done
  done
else
  mapfile -t DUMP_FILES < <(find "$DUMPS_DIR" -maxdepth 1 -name '*.hprof' | sort 2>/dev/null)
fi

if [[ ${#DUMP_FILES[@]} -eq 0 ]]; then
  echo -e "${Y}No .hprof files found in $DUMPS_DIR/.${R}"
  echo "  Run without --no-capture to generate dumps first."
  exit 1
fi

log "Found ${#DUMP_FILES[@]} dump(s) to test."

# ─────────────────────────────────────────────────────────────────────────────
# run_cmd LABEL SCENARIO_NAME [CMD…]
#
#   Runs a hprof-analyzer command, captures output, checks for:
#     1. Exit code   (non-zero → FAIL)
#     2. Panics      ("panicked at" in stderr → FAIL)
#     3. Raw internal error strings in stderr → FAIL
#   Returns 0 if all checks pass.
# ─────────────────────────────────────────────────────────────────────────────
run_cmd() {
  local label="$1" scenario="$2"; shift 2
  local logf="$RESULTS_DIR/logs/${scenario}_${label}.log"

  # Run with a timeout to avoid hanging on corrupted dumps
  local stdout stderr exit_code
  stdout=$( (timeout 300 "$BIN" "$@" 2>"$logf.stderr") 2>/dev/null) || exit_code=$?
  stderr=$(cat "$logf.stderr" 2>/dev/null)
  exit_code="${exit_code:-0}"

  # Save combined output for debugging
  { echo "CMD: $BIN $*"; echo "EXIT: $exit_code";
    echo "--- STDOUT ---"; echo "$stdout";
    echo "--- STDERR ---"; echo "$stderr"; } > "$logf"

  local failed=false detail=""

  # Check 1: exit code
  if [[ "$exit_code" -ne 0 ]]; then
    failed=true
    detail+="Exit code: $exit_code\n"
  fi

  # Check 2: Rust panics
  if echo "$stderr" | grep -q "panicked at"; then
    failed=true
    detail+="PANIC detected in stderr:\n$(echo "$stderr" | grep -A 5 "panicked at")\n"
  fi

  # Check 3: raw internal reader errors that should never surface
  for bad in "eof in read_into" "eof in skip" "failed to fill whole buffer" \
             "called \`unwrap()\` on a \`None\`" "index out of bounds"; do
    if echo "$stderr" | grep -q "$bad"; then
      failed=true
      detail+="Leaked internal error '$bad' in stderr\n"
    fi
  done

  # Check 4: JSON output validity (for json commands)
  if [[ "$label" == *json* ]]; then
    if ! echo "$stdout" | python3 -c "import sys,json; json.load(sys.stdin)" 2>/dev/null; then
      failed=true
      detail+="JSON output is invalid\n"
    fi
  fi

  if $failed; then
    fail "$scenario  [$label]"
    record_error "$scenario" "$label: $BIN $*" "$detail$(tail -20 "$logf")"
    return 1
  else
    ok "$scenario  [$label]"
    record_pass
    return 0
  fi
}

# ─────────────────────────────────────────────────────────────────────────────
# test_dump DUMP_PATH
#
#   Runs the full battery of commands against one dump.
# ─────────────────────────────────────────────────────────────────────────────
test_dump() {
  local dump="$1"
  local name; name="$(basename "$dump" .hprof)"

  # Skip tiny/stub files (LFS pointers, empty files)
  local sz; sz="$(wc -c < "$dump" 2>/dev/null || echo 0)"
  if [[ "$sz" -lt 1024 ]]; then
    skip "$name  (file too small: ${sz} bytes — likely not a real dump)"
    record_skip
    return
  fi

  echo ""
  echo -e "  ${B}${C}── $name ──${R}"

  # 1. Basic report (Markdown, default)
  run_cmd "md"      "$name"  "$dump" --format md    --progress never || true

  # 2. JSON report  (verifies valid JSON)
  local json_out="$RESULTS_DIR/logs/${name}_json_stdout.json"
  run_cmd "json"    "$name"  "$dump" --format json  --progress never > "$json_out" 2>&1 || true
  # Also capture JSON to file for further inspection
  timeout 300 "$BIN" "$dump" --format json --progress never > "$json_out" 2>/dev/null || true

  # 3. heap summary (cached analysis — triage + leak suspects)
  run_cmd "summary" "$name"  heap summary "$dump"  --progress never || true

  # 4. triage section
  run_cmd "triage"  "$name"  heap report  "$dump"  --section triage  --progress never || true

  # 5. leaks section
  run_cmd "leaks"   "$name"  heap report  "$dump"  --section leaks   --progress never || true

  # 6. threads section
  run_cmd "threads" "$name"  heap report  "$dump"  --section threads --progress never || true

  # 7. histogram
  run_cmd "hist"    "$name"  heap histogram "$dump" --progress never || true

  # 8. OQL: top 10 by retained size (exercises the query + retained-size pipeline)
  run_cmd "oql-top" "$name" \
    heap query "$dump" --progress never \
    --oql "SELECT @displayName, @retainedHeapSize AS r FROM INSTANCEOF java.lang.Object ORDER BY r DESC LIMIT 10" \
    || true

  # 9. Full analysis report (--find-duplicates --collections)
  run_cmd "full"    "$name"  "$dump" --format md --find-duplicates --collections --progress never || true

  # 10. detect-secrets (exercises the secret-scanning pipeline)
  run_cmd "secrets" "$name"  detect-secrets "$dump" --progress never || true

  # Clean up the heap cache created by the run to keep the repo tidy
  # (only clean test-results caches, not the shared tests/fixtures caches)
  rm -f "${dump}.hprof-cache" "${dump%.*}.hprof-cache" 2>/dev/null || true
}

# Main loop
for dump in "${DUMP_FILES[@]}"; do
  test_dump "$dump"
done

# =============================================================================
# STEP 4 — Write report
# =============================================================================
hdr "Step 4 — Write report"

{
  echo "# hprof-analyzer × leak-demos Test Report"
  echo ""
  echo "Generated: $(date -u '+%Y-%m-%d %H:%M UTC')"
  echo "Binary:    \`$BIN\`"
  echo "Dumps:     $DUMPS_DIR"
  echo ""
  echo "## Summary"
  echo ""
  echo "| Result | Count |"
  echo "|--------|-------|"
  echo "| ✅ PASS | $PASS |"
  echo "| ❌ FAIL | $FAIL |"
  echo "| ⏭ SKIP | $SKIP |"
  echo ""
  echo "## Dumps Tested"
  echo ""
  for dump in "${DUMP_FILES[@]}"; do
    local_sz="$(du -sh "$dump" 2>/dev/null | cut -f1)"
    echo "- \`$(basename "$dump")\`  (${local_sz})"
  done
  echo ""
} > "$REPORT"

if [[ ${#ERROR_LOG[@]} -gt 0 ]]; then
  {
    echo "# Captured Errors"
    echo ""
    echo "Generated: $(date -u '+%Y-%m-%d %H:%M UTC')"
    echo ""
    printf '%s\n' "${ERROR_LOG[@]}"
  } > "$ERRORS"
  echo -e "  ${RED}${FAIL} failure(s) — see $ERRORS${R}"
else
  echo "  (no errors)" >> "$ERRORS"
  echo -e "  ${G}No errors captured${R}"
fi

# ── Final banner ──────────────────────────────────────────────────────────────
echo ""
echo -e "${B}══════════════════════════════════════════════${R}"
echo -e "  ${G}✅ PASS${R}: $PASS   ${RED}❌ FAIL${R}: $FAIL   ${Y}⏭ SKIP${R}: $SKIP"
echo -e "${B}══════════════════════════════════════════════${R}"
echo ""
echo "  Report  → $REPORT"
[[ "$FAIL" -gt 0 ]] && echo "  Errors  → $ERRORS"
echo ""

[[ "$FAIL" -eq 0 ]]  # exit 1 if any failures
