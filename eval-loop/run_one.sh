#!/usr/bin/env bash
# Blind-diagnosis evaluation loop: run opencode+GPT-Luna against ONE heap dump
# via the hprof MCP (no ground truth given), capture its diagnosis.
#
# Usage: run_one.sh <dump.hprof> <out_basename>
# Writes: results/<out_basename>.json  (raw opencode JSON events)
#         results/<out_basename>.txt   (Luna's final text answer)
set -uo pipefail

DUMP="$1"
OUT="$2"
HERE="$(cd "$(dirname "$0")" && pwd)"
RESULTS="$HERE/results"
mkdir -p "$RESULTS"

PROMPT="$(sed "s#{DUMP_PATH}#$DUMP#g" "$HERE/prompt_template.txt")"

# Run opencode non-interactively in this dir (picks up ./opencode.json → hprof MCP
# + gpt-6-luna). JSON format captures the full event stream incl. tool calls.
cd "$HERE"
timeout 600 opencode run --model localproxy/gpt-5-mini --format json "$PROMPT" \
  > "$RESULTS/$OUT.json" 2> "$RESULTS/$OUT.stderr"
RC=$?

# Extract the assistant's final text (best-effort: last text parts from JSON events).
python3 -I - "$RESULTS/$OUT.json" > "$RESULTS/$OUT.txt" <<'PY'
import json, sys
texts = []
try:
    raw = open(sys.argv[1], encoding="utf-8", errors="replace").read()
except Exception:
    raw = ""
# opencode --format json may emit one JSON array or NDJSON; handle both.
events = []
raw_strip = raw.strip()
if raw_strip.startswith("["):
    try: events = json.loads(raw_strip)
    except Exception: events = []
if not events:
    for line in raw.splitlines():
        line = line.strip()
        if not line: continue
        try: events.append(json.loads(line))
        except Exception: pass
def walk(o):
    if isinstance(o, dict):
        # collect assistant text parts
        if o.get("type") == "text" and isinstance(o.get("text"), str):
            texts.append(o["text"])
        for v in o.values(): walk(v)
    elif isinstance(o, list):
        for v in o: walk(v)
walk(events)
print("\n".join(texts).strip() if texts else "(no text extracted — see .json)")
PY

echo "rc=$RC dump=$DUMP → $RESULTS/$OUT.txt"
exit 0
