#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# capture_all.sh — Run every demo scenario and store a heap dump for each one.
#
# Usage:
#   ./capture_all.sh              # run all scenarios
#   ./capture_all.sh 01           # run only scenario 01
#   ./capture_all.sh 31 32 33     # run specific scenarios by number
#   ./capture_all.sh oom          # run only OOM/leak scenarios
#   ./capture_all.sh normal       # run only normal-app scenarios
#   ./capture_all.sh waste        # run only collection/duplicate waste scenarios
#
# Prerequisites:
#   java 11+, jcmd and jmap on PATH (from same JDK), hprof-analyzer (optional)
#
# Output layout:
#   dumps/
#     01_StaticCacheLeak.hprof
#     02_ListenerLeak.hprof
#     …
#     34_AppProductCatalog.hprof
#     logs/
#       01_StaticCacheLeak.log
#       …
#
# Environment overrides:
#   JAVA      — path to java binary      (default: java)
#   JMAP      — path to jmap binary      (default: jmap)
#   JCMD      — path to jcmd binary      (default: jcmd)
#   DUMPS_DIR — output directory         (default: ./dumps)
#   SETTLE    — seconds to wait before   (default: per-scenario setting)
#               dumping idle scenarios
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

# ── Config ────────────────────────────────────────────────────────────────────
JAVA="${JAVA:-java}"
JMAP="${JMAP:-jmap}"
JCMD="${JCMD:-jcmd}"
DUMPS_DIR="${DUMPS_DIR:-./dumps}"
LOGS_DIR="$DUMPS_DIR/logs"

mkdir -p "$DUMPS_DIR" "$LOGS_DIR"

# ── Colour helpers ────────────────────────────────────────────────────────────
if [[ -t 1 ]]; then
  C_RESET='\033[0m'
  C_BOLD='\033[1m'
  C_GREEN='\033[0;32m'
  C_YELLOW='\033[1;33m'
  C_RED='\033[0;31m'
  C_CYAN='\033[0;36m'
  C_DIM='\033[2m'
else
  C_RESET='' C_BOLD='' C_GREEN='' C_YELLOW='' C_RED='' C_CYAN='' C_DIM=''
fi

ts()   { date +%H:%M:%S; }
hdr()  { echo -e "\n${C_BOLD}${C_CYAN}── $* ──${C_RESET}"; }
log()  { echo -e "${C_DIM}[$(ts)]${C_RESET} $*"; }
ok()   { echo -e "  ${C_GREEN}✓${C_RESET} $*"; }
warn() { echo -e "  ${C_YELLOW}⚠${C_RESET} $*"; }
fail() { echo -e "  ${C_RED}✗${C_RESET} $*" >&2; }

# ── Prerequisite check ────────────────────────────────────────────────────────
check_prereqs() {
  local ok=true
  for cmd in "$JAVA" "$JMAP" "$JCMD"; do
    if ! command -v "$cmd" &>/dev/null; then
      fail "Not found on PATH: $cmd"
      ok=false
    fi
  done
  java_ver=$("$JAVA" -version 2>&1 | head -1)
  log "Java: $java_ver"
  $ok || { echo "Fix the above and retry."; exit 1; }
}

# ─────────────────────────────────────────────────────────────────────────────
# capture_oom FILE [JVM_FLAGS…]
#
#   Runs a scenario that terminates with OutOfMemoryError.
#   The JVM captures the heap dump automatically via HeapDumpOnOutOfMemoryError.
# ─────────────────────────────────────────────────────────────────────────────
capture_oom() {
  local file="$1"; shift
  local name="${file%.java}"
  local dump="$DUMPS_DIR/${name}.hprof"
  local logf="$LOGS_DIR/${name}.log"

  [[ -n "${FILTER:-}" ]] && [[ "$name" != *"$FILTER"* ]] && return 0

  log "OOM  ${C_BOLD}$name${C_RESET}"
  rm -f "$dump"

  "$JAVA" "$@" \
    -XX:+HeapDumpOnOutOfMemoryError \
    "-XX:HeapDumpPath=$dump" \
    "$file" >"$logf" 2>&1 || true   # non-zero exit is expected (OOM)

  if [[ -f "$dump" ]]; then
    local sz; sz="$(du -sh "$dump" 2>/dev/null | cut -f1)"
    ok "${name}.hprof  ${C_DIM}(${sz})${C_RESET}"
    echo "$dump" >> "$DUMPS_DIR/.manifest"
  else
    # Some OOMs (NativeThread, ArraySize) don't produce a heap dump
    warn "${name} — no heap dump (check ${logf##*/})"
  fi
}

# ─────────────────────────────────────────────────────────────────────────────
# capture_live FILE SETTLE_SECS [JVM_FLAGS…]
#
#   Runs an idle scenario that sleeps once initialised.
#   Waits SETTLE_SECS for the process to print "HEAP_DUMP_READY", then
#   triggers a dump with `jcmd GC.heap_dump` (full GC first → live objects only).
# ─────────────────────────────────────────────────────────────────────────────
capture_live() {
  local file="$1"; shift
  local settle="${1:-20}"; shift
  local name="${file%.java}"
  local dump="$DUMPS_DIR/${name}.hprof"
  local logf="$LOGS_DIR/${name}.log"

  [[ -n "${FILTER:-}" ]] && [[ "$name" != *"$FILTER"* ]] && return 0

  log "LIVE ${C_BOLD}$name${C_RESET}  ${C_DIM}(settle ${settle}s)${C_RESET}"
  rm -f "$dump"

  "$JAVA" "$@" "$file" >"$logf" 2>&1 &
  local pid=$!

  # Wait for HEAP_DUMP_READY marker (up to settle+30 s)
  local deadline=$(( $(date +%s) + settle + 30 ))
  until grep -q "HEAP_DUMP_READY" "$logf" 2>/dev/null || [[ $(date +%s) -ge $deadline ]]; do
    sleep 1
  done
  # Extra settle time requested by caller
  sleep 2

  if ! kill -0 "$pid" 2>/dev/null; then
    fail "${name} — process exited early (see ${logf##*/})"
    return
  fi

  # Prefer jcmd (runs full GC → live objects only); fall back to jmap live
  if ! "$JCMD" "$pid" GC.heap_dump "$dump" >/dev/null 2>&1; then
    "$JMAP" -dump:live,format=b,file="$dump" "$pid" >/dev/null 2>&1 || true
  fi

  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true

  if [[ -f "$dump" ]]; then
    local sz; sz="$(du -sh "$dump" 2>/dev/null | cut -f1)"
    ok "${name}.hprof  ${C_DIM}(${sz}, live only)${C_RESET}"
    echo "$dump" >> "$DUMPS_DIR/.manifest"
  else
    fail "${name} — dump failed (see ${logf##*/})"
  fi
}

# ─────────────────────────────────────────────────────────────────────────────
# capture_all_objects FILE SETTLE_SECS [JVM_FLAGS…]
#
#   Like capture_live but uses `jmap -dump:format=b` WITHOUT the `live` filter.
#   The dump includes unreachable objects not yet collected by GC.
#   Used for scenario 30 (GcOverwhelmedGarbage).
# ─────────────────────────────────────────────────────────────────────────────
capture_all_objects() {
  local file="$1"; shift
  local settle="${1:-15}"; shift
  local name="${file%.java}"
  local dump="$DUMPS_DIR/${name}.hprof"
  local logf="$LOGS_DIR/${name}.log"

  [[ -n "${FILTER:-}" ]] && [[ "$name" != *"$FILTER"* ]] && return 0

  log "ALL  ${C_BOLD}$name${C_RESET}  ${C_DIM}(settle ${settle}s, unreachable included)${C_RESET}"
  rm -f "$dump"

  "$JAVA" "$@" "$file" >"$logf" 2>&1 &
  local pid=$!

  # Wait for DUMP WINDOW OPEN marker
  local deadline=$(( $(date +%s) + settle + 30 ))
  until grep -qE "DUMP WINDOW OPEN|HEAP_DUMP_READY" "$logf" 2>/dev/null \
        || [[ $(date +%s) -ge $deadline ]]; do
    sleep 1
  done
  sleep 1  # tiny extra margin — grab the heap while garbage is still hot

  if ! kill -0 "$pid" 2>/dev/null; then
    fail "${name} — process exited early (see ${logf##*/})"
    return
  fi

  # jmap WITHOUT 'live' → captures unreachable objects too
  "$JMAP" -dump:format=b,file="$dump" "$pid" >/dev/null 2>&1 || true

  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true

  if [[ -f "$dump" ]]; then
    local sz; sz="$(du -sh "$dump" 2>/dev/null | cut -f1)"
    ok "${name}.hprof  ${C_DIM}(${sz}, ALL objects incl. unreachable)${C_RESET}"
    echo "$dump" >> "$DUMPS_DIR/.manifest"
  else
    fail "${name} — dump failed (see ${logf##*/})"
  fi
}

# ─────────────────────────────────────────────────────────────────────────────
# analyse_all  (optional post-step)
#
#   Runs hprof-analyzer heap summary on every collected dump, if the tool
#   is available on PATH.
# ─────────────────────────────────────────────────────────────────────────────
analyse_all() {
  if ! command -v hprof-analyzer &>/dev/null; then
    warn "hprof-analyzer not found — skipping automatic analysis."
    warn "Install: brew tap parttimenerd/hprof-analyzer && brew install hprof-analyzer"
    return
  fi
  hdr "Quick Analysis (hprof-analyzer heap summary)"
  while IFS= read -r dump; do
    [[ -f "$dump" ]] || continue
    local name; name="$(basename "$dump" .hprof)"
    echo -e "\n${C_BOLD}── $name ──${C_RESET}"
    hprof-analyzer heap summary "$dump" 2>/dev/null || warn "analysis failed for $dump"
  done < <(sort -u "$DUMPS_DIR/.manifest" 2>/dev/null)
}

# ─────────────────────────────────────────────────────────────────────────────
# Main — scenario table
# ─────────────────────────────────────────────────────────────────────────────
main() {
  # Parse filter argument(s)
  FILTER=""
  declare -A GROUP_MAP=(
    [oom]="01 02 03 04 05 06 07 08 09 10 11 12 13 14 15 16 17"
    [waste]="18 19 20 21 22 28 29"
    [normal]="23 24 25 26 27 31 32 33 34"
    [unreachable]="30"
  )

  RUN_LIST=()
  if [[ $# -eq 0 ]]; then
    RUN_LIST=("all")
  else
    for arg in "$@"; do
      case "$arg" in
        oom|waste|normal|unreachable)
          RUN_LIST+=("${GROUP_MAP[$arg]}");;
        analyse|analyze)
          analyse_all; exit 0;;
        all)
          RUN_LIST+=("all");;
        *)
          RUN_LIST+=("$arg");;  # treat as scenario number prefix
      esac
    done
  fi

  # Build a space-separated list of numbers to run (or "all")
  local run_all=false
  local numbers=()
  for item in "${RUN_LIST[@]}"; do
    if [[ "$item" == "all" ]]; then run_all=true; break; fi
    for n in $item; do numbers+=("$n"); done
  done

  # Helper: should we run this scenario?
  should_run() {
    $run_all && return 0
    local n="$1"
    for wanted in "${numbers[@]}"; do
      # match by prefix: "01" matches "01_StaticCacheLeak"
      [[ "$n" == "${wanted}"* ]] && return 0
    done
    return 1
  }

  rm -f "$DUMPS_DIR/.manifest"

  echo -e "${C_BOLD}"
  echo "════════════════════════════════════════════════"
  echo " Java Memory Demo Suite — Heap Dump Capture    "
  echo "════════════════════════════════════════════════${C_RESET}"
  echo "  Dumps  → $DUMPS_DIR/"
  echo "  Logs   → $LOGS_DIR/"
  echo "  Java   → $("$JAVA" -version 2>&1 | head -1)"
  echo ""

  check_prereqs

  # ── PART 1: Leaks & OOM ──────────────────────────────────────────────────
  hdr "PART 1 — Leaks & OOM"
  should_run 01 && capture_oom 01_StaticCacheLeak.java         -Xmx64m
  should_run 02 && capture_oom 02_ListenerLeak.java            -Xmx64m
  should_run 03 && capture_oom 03_InnerClassLeak.java          -Xmx64m
  should_run 04 && capture_oom 04_UnboundedCollectionLeak.java -Xmx64m
  should_run 05 && capture_oom 05_ThreadLocalLeak.java         -Xmx64m
  should_run 06 && capture_oom 06_ExecutorLeak.java            -Xmx64m
  should_run 07 && capture_oom 07_ShutdownHookLeak.java        -Xmx64m
  should_run 08 && capture_oom 08_ClassLoaderLeak.java         -XX:MaxMetaspaceSize=64m
  should_run 09 && capture_oom 09_ClassGenerationOOM.java      -XX:MaxMetaspaceSize=64m
  should_run 10 && capture_oom 10_FinalizerQueueLeak.java      -Xmx128m
  should_run 11 && capture_oom 11_WeakHashMapLeak.java         -Xmx64m
  should_run 12 && capture_oom 12_SoftReferenceCacheLeak.java  -Xmx64m
  should_run 13 && capture_oom 13_DirectBufferLeak.java        -Xmx32m -XX:MaxDirectMemorySize=64m
  should_run 14 && capture_oom 14_GcOverheadOOM.java           -Xmx64m
  # 15 (NativeThreadOOM) and 16 (ArraySizeOOM) rarely produce useful heap dumps
  should_run 15 && warn "15_NativeThreadOOM — skipped (no meaningful heap dump; OS-level limit)"
  should_run 16 && warn "16_ArraySizeOOM    — skipped (immediate error, no dump)"
  should_run 17 && capture_oom 17_PersistenceContextLeak.java  -Xmx64m

  # ── PART 2: Collection Waste ─────────────────────────────────────────────
  hdr "PART 2 — Collection Efficiency Waste"
  should_run 18 && capture_live 18_OversizedHashMapWaste.java    20 -Xmx256m
  should_run 19 && capture_live 19_SparseMapAfterRemoval.java    35 -Xmx128m
  should_run 20 && capture_live 20_ArrayListCapacityWaste.java   20 -Xmx128m

  # ── PART 3: Duplicate Detection ──────────────────────────────────────────
  hdr "PART 3 — Duplicate Detection"
  should_run 21 && capture_live 21_DuplicateStringWaste.java     20 -Xmx256m
  should_run 22 && capture_live 22_DuplicateByteArrayWaste.java  20 -Xmx128m

  # ── PART 4: Structural / Object Overhead ─────────────────────────────────
  hdr "PART 4 — Object Structure & Overhead"
  should_run 23 && capture_live 23_NormalDomainObjectGraph.java  25 -Xmx256m
  should_run 24 && capture_live 24_MetricTimeSeries.java         30 -Xmx256m
  should_run 25 && capture_live 25_DeepObjectChain.java          20 -Xmx256m
  should_run 26 && capture_live 26_BoxedPrimitivesWaste.java     20 -Xmx256m
  should_run 27 && capture_live 27_ManySmallObjects.java         30 -Xmx512m

  # ── PART 5: Edge Cases ───────────────────────────────────────────────────
  hdr "PART 5 — Edge Cases"
  should_run 28 && capture_live 28_StringInternAbuse.java        20 -Xmx256m
  should_run 29 && capture_live 29_HashMapHighCollisionRate.java 20 -Xmx256m

  # ── PART 6: Unreachable Objects ──────────────────────────────────────────
  hdr "PART 6 — Unreachable Objects (jmap without live filter)"
  should_run 30 && capture_all_objects 30_GcOverwhelmedGarbage.java 20 \
    -Xmx256m -Xms256m -XX:+UseSerialGC -XX:NewSize=160m -XX:MaxNewSize=160m

  # ── PART 7: Normal Applications ──────────────────────────────────────────
  hdr "PART 7 — Normal Applications (no bugs)"
  should_run 31 && capture_live 31_AppWordFrequency.java  30 -Xmx256m
  should_run 32 && capture_live 32_AppSessionStore.java   25 -Xmx256m
  should_run 33 && capture_live 33_AppEventPipeline.java  25 -Xmx256m
  should_run 34 && capture_live 34_AppProductCatalog.java 25 -Xmx256m

  # ── PART 8: New Leak Patterns ────────────────────────────────────────────
  hdr "PART 8 — New Leak Patterns (ClassLoader / ThreadLocal / Thread / DirectBuffer)"
  should_run 35 && capture_live 35_ClassLoaderSpam.java          20 -Xmx128m
  should_run 36 && capture_live 36_ClassLoaderLeak.java          25 -Xmx256m
  should_run 37 && capture_live 37_ThreadLocalPooledThread.java  20 -Xmx64m
  should_run 38 && capture_live 38_ExcessiveThreads.java         20 -Xmx256m
  should_run 39 && capture_live 39_ThreadExecutorLeak.java       25 -Xmx256m
  should_run 40 && capture_live 40_DirectBufferLeak.java         20 -Xmx64m -XX:MaxDirectMemorySize=256m

  # ── Summary ──────────────────────────────────────────────────────────────
  echo ""
  hdr "Summary"
  local count=0
  if [[ -f "$DUMPS_DIR/.manifest" ]]; then
    count=$(wc -l < "$DUMPS_DIR/.manifest")
    echo -e "  ${C_GREEN}${C_BOLD}${count} dump(s) collected${C_RESET} in $DUMPS_DIR/"
    echo ""
    while IFS= read -r dump; do
      [[ -f "$dump" ]] || continue
      local sz; sz="$(du -sh "$dump" | cut -f1)"
      echo -e "  ${C_DIM}${sz}${C_RESET}  $(basename "$dump")"
    done < <(sort "$DUMPS_DIR/.manifest")
  else
    warn "No dumps collected."
  fi

  echo ""
  echo -e "  ${C_BOLD}Next steps:${C_RESET}"
  echo "    hprof-analyzer heap summary      <dump>"
  echo "    hprof-analyzer heap report       <dump> --section leaks"
  echo "    hprof-analyzer heap report       <dump> --section triage"
  echo "    hprof-analyzer <dump> report.html --full-analysis"
  echo ""
  echo "  Run with 'analyse' to automatically summarise all dumps:"
  echo "    ./capture_all.sh analyse"
}

main "$@"
