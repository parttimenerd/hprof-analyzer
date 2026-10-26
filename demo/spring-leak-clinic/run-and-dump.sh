#!/usr/bin/env bash
# run-and-dump.sh — Build, run, trigger leaks, capture heap dump, analyse.
#
# Usage:
#   bash run-and-dump.sh                  # full pipeline
#   bash run-and-dump.sh --skip-build     # reuse existing jar
#   bash run-and-dump.sh --rounds 200     # more traffic (bigger dump)
#
# Prerequisites:
#   - Java 17+ (java, jcmd on PATH)
#   - Maven 3.8+ (mvn on PATH, or edit to use ./mvnw)
#   - hprof-analyzer on PATH (optional — skipped if missing)

set -euo pipefail

# ── Configuration ────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

PORT=18080
ROUNDS=100           # number of warm-up rounds per leak
SKIP_BUILD=false
DUMP_FILE="leak-clinic.hprof"
REPORT_FILE="leak-clinic-report.html"
XMX="512m"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) SKIP_BUILD=true; shift ;;
    --rounds)     ROUNDS="$2"; shift 2 ;;
    --xmx)        XMX="$2"; shift 2 ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

# ── Step 1: Build ────────────────────────────────────────────────────────────
if [[ "$SKIP_BUILD" == "false" ]]; then
  echo "▸ Building Spring Leak Clinic..."
  mvn -q package -DskipTests
  echo "  Done."
else
  echo "▸ Skipping build (--skip-build)."
fi

JAR=$(ls target/spring-leak-clinic-*.jar 2>/dev/null | head -1)
if [[ -z "$JAR" ]]; then
  echo "ERROR: No jar found in target/. Run without --skip-build first."
  exit 1
fi

# ── Step 2: Start the app ───────────────────────────────────────────────────
echo ""
echo "▸ Starting app (-Xmx${XMX}) on port ${PORT}..."
java \
  -Xmx"$XMX" \
  -XX:+HeapDumpOnOutOfMemoryError \
  -XX:HeapDumpPath="$SCRIPT_DIR" \
  -jar "$JAR" \
  --server.port="$PORT" \
  &
APP_PID=$!
echo "  PID: $APP_PID"

cleanup() {
  echo ""
  echo "▸ Stopping app (PID $APP_PID)..."
  kill "$APP_PID" 2>/dev/null || true
  wait "$APP_PID" 2>/dev/null || true
}
trap cleanup EXIT

# Wait for startup
echo "  Waiting for startup..."
for i in $(seq 1 60); do
  if curl -sf "http://localhost:$PORT/api/health" >/dev/null 2>&1; then
    echo "  App is up (${i}s)."
    break
  fi
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    echo "ERROR: App process died."
    exit 1
  fi
  sleep 1
done

if ! curl -sf "http://localhost:$PORT/api/health" >/dev/null 2>&1; then
  echo "ERROR: App did not start within 60s."
  exit 1
fi

# ── Step 3: Trigger all leaks ───────────────────────────────────────────────
echo ""
echo "▸ Triggering leaks ($ROUNDS rounds via /api/warm-up)..."
RESULT=$(curl -sf -X POST "http://localhost:$PORT/api/warm-up?rounds=$ROUNDS")
echo "  Result: $RESULT"

# Extra traffic to spread the ThreadLocal leak across more threads.
# Sequential, not parallel — keeps memory predictable.
TRACK_EXTRA=$(( ROUNDS * 5 > 100 ? 100 : ROUNDS * 5 ))
echo "  Sending $TRACK_EXTRA sequential requests to spread ThreadLocal leak across threads..."
for i in $(seq 1 "$TRACK_EXTRA"); do
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    echo "  WARNING: app died during ThreadLocal traffic."
    break
  fi
  curl -sf -X POST "http://localhost:$PORT/api/track?requestId=extra-$i" >/dev/null
done
echo "  ThreadLocal traffic done."

# ── Step 4: Capture heap dump ───────────────────────────────────────────────
# Dump now — before any extra report traffic — so we don't OOM on small heaps.
# All 7 leaks are already active from warm-up.
echo ""
echo "▸ Capturing heap dump..."
if ! kill -0 "$APP_PID" 2>/dev/null; then
  echo "ERROR: App (PID $APP_PID) is no longer running — was it OOM-killed?"
  echo "       Try: bash run-and-dump.sh --rounds $ROUNDS --xmx 1g"
  exit 1
fi
rm -f "$DUMP_FILE"
jcmd "$APP_PID" GC.heap_dump "$SCRIPT_DIR/$DUMP_FILE"

DUMP_MB=$(( $(wc -c < "$DUMP_FILE") / 1024 / 1024 ))
echo "  Heap dump: $DUMP_FILE ($DUMP_MB MB)"

# ── Step 5: Analyse with hprof-analyzer ─────────────────────────────────────
echo ""
if command -v hprof-analyzer &>/dev/null; then
  echo "▸ Analysing with hprof-analyzer (full analysis)..."
  hprof-analyzer "$DUMP_FILE" "$REPORT_FILE" \
    --find-duplicates \
    --collections \
    --progress always

  echo ""
  echo "  Report: $REPORT_FILE"
  echo ""

  # Quick text summary
  echo "▸ Quick summary:"
  echo "  ─────────────────────────────────────────"
  hprof-analyzer heap summary "$DUMP_FILE" 2>/dev/null || true
  echo "  ─────────────────────────────────────────"

  # Check for secrets
  echo ""
  echo "▸ Secret detection:"
  hprof-analyzer detect-secrets "$DUMP_FILE" 2>/dev/null || echo "  (detect-secrets not available in this build)"

else
  echo "▸ hprof-analyzer not on PATH — skipping analysis."
  echo "  Install it, then run:"
  echo ""
  echo "    hprof-analyzer $DUMP_FILE $REPORT_FILE --find-duplicates --collections"
  echo "    hprof-analyzer detect-secrets $DUMP_FILE"
  echo ""
fi

# ── Done ────────────────────────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════════════════════════════"
echo "  Done! Files:"
echo "    Heap dump : $DUMP_FILE ($DUMP_MB MB)"
if [[ -f "$REPORT_FILE" ]]; then
  REPORT_KB=$(( $(wc -c < "$REPORT_FILE") / 1024 ))
  echo "    Report    : $REPORT_FILE ($REPORT_KB KB)"
fi
echo ""
echo "  Open the report in your browser, or try the web UI:"
echo "    hprof-analyzer server $DUMP_FILE"
echo "    # then open http://127.0.0.1:7070"
echo "════════════════════════════════════════════════════════════════════"
