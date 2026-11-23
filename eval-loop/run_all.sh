#!/usr/bin/env bash
# Batch blind-eval: run every captured dump through run_one.sh (opencode+model+hprof MCP).
# Usage: run_all.sh [dumps_dir]   (default: ../leak-demos/dumps)
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
DUMPS="${1:-$HERE/../leak-demos/dumps}"

for f in "$DUMPS"/*.hprof; do
  [ -f "$f" ] || continue
  base="$(basename "$f" .hprof)"
  echo "=== EVAL $base ==="
  "$HERE/run_one.sh" "$f" "$base"
done
echo "=== all evals done → $HERE/results/ ==="
