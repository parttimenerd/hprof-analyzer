#!/usr/bin/env bash
# gen-dumps.sh — generate heap dumps at increasing sizes using HyperAlloc + jmap
# Usage: bash tests/web-stress/gen-dumps.sh
# Output: tests/web-stress/dumps/dump-<size>m.hprof
set -euo pipefail

HYPERALLOC_JAR="/tmp/heapothesys/HyperAlloc/target/HyperAlloc.jar"
OUT_DIR="$(git rev-parse --show-toplevel)/tests/web-stress/dumps"
mkdir -p "$OUT_DIR"

# Target heap occupancies in MB.
# HyperAlloc -s sets live object occupancy; we set -Xmx to occupancy + 30% headroom.
SIZES=(128 256 512 1024 2048 2560 3072)

for OCC in "${SIZES[@]}"; do
  OUT="$OUT_DIR/dump-${OCC}m.hprof"
  if [[ -f "$OUT" ]]; then
    echo "Skipping $OUT (already exists)"
    continue
  fi

  # Add 30% headroom above live occupancy so GC isn't constantly running
  XMX=$(( OCC * 13 / 10 ))
  echo "Generating ${OCC}MB occupancy dump (Xmx=${XMX}m)…"

  # Run HyperAlloc for 60s to stabilise heap, then dump with jmap.
  # -a 256  = 256 MB/s allocation rate (low; we care about occupancy, not throughput)
  # -s $OCC = target live-object occupancy
  # -d 60   = run for 60 seconds so occupancy stabilises
  java -Xmx${XMX}m -Xms${XMX}m \
    -jar "$HYPERALLOC_JAR" \
    -a 256 -s "$OCC" -d 60 -l /dev/null &
  JAVA_PID=$!

  # Wait 35s for occupancy to stabilise, then dump
  sleep 35
  echo "  jmap dumping PID $JAVA_PID → $OUT"
  jmap -dump:format=b,file="$OUT" "$JAVA_PID" || {
    echo "  jmap failed for PID $JAVA_PID; killing and skipping"
    kill "$JAVA_PID" 2>/dev/null || true
    continue
  }

  # Let HyperAlloc finish its run
  wait "$JAVA_PID" 2>/dev/null || true
  SIZE_MB=$(( $(wc -c < "$OUT") / 1048576 ))
  echo "  Done: $OUT (${SIZE_MB} MB on disk)"
done

echo "All dumps in $OUT_DIR:"
ls -lh "$OUT_DIR"/*.hprof 2>/dev/null || echo "  (none)"
