#!/usr/bin/env bash
# run-click-sweep — build, generate a report, and run the click-sweep test.
#
# USAGE
# ─────
#   # Quick local run with a heap dump you have on hand:
#   ./scripts/run-click-sweep.sh /path/to/app.hprof
#
#   # 5-minute CI-style soak (the same budget used in CI):
#   ./scripts/run-click-sweep.sh /path/to/app.hprof --budget 300
#
#   # Headed (visible browser) for debugging:
#   ./scripts/run-click-sweep.sh /path/to/app.hprof --no-headless
#
#   # Pass any click-sweep option through:
#   ./scripts/run-click-sweep.sh /path/to/app.hprof --verbose --no-hover
#
#   # Provide a pre-generated report (skip dump processing):
#   HPROF_HTML=/path/to/report.html ./scripts/run-click-sweep.sh
#
# WHAT IT DOES
# ────────────
#   1. Locates or builds the hprof-analyzer binary (release).
#   2. Generates an HTML report from the dump into /tmp/click-sweep-report.html
#      (or skips this step when HPROF_HTML is already set).
#   3. cd's to tests/playwright and runs `npx ts-node click-sweep.ts`
#      forwarding all extra arguments.
#
# EXIT CODES
# ──────────
#   0  No issues
#   1  Issues found (see click-sweep output)
#   2  Usage/build error
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ── Usage ──────────────────────────────────────────────────────────────────────

DUMP_PATH=""
EXTRA_ARGS=()

for arg in "$@"; do
  if [[ -z "$DUMP_PATH" && "$arg" != --* && -f "$arg" ]]; then
    DUMP_PATH="$arg"
  else
    EXTRA_ARGS+=("$arg")
  fi
done

# ── Binary ─────────────────────────────────────────────────────────────────────

BINARY="$REPO_ROOT/target/release/hprof-analyzer"

if [[ ! -x "$BINARY" ]]; then
  echo "→ Binary not found — building (this takes a few minutes)..."
  (cd "$REPO_ROOT" && cargo build --release --features native)
  echo "→ Build complete."
elif [[ -n "$DUMP_PATH" && "$BINARY" -ot "$REPO_ROOT/src/main.rs" ]]; then
  echo "→ Binary is older than src/main.rs — rebuilding..."
  (cd "$REPO_ROOT" && cargo build --release --features native)
  echo "→ Build complete."
fi

# ── Report ─────────────────────────────────────────────────────────────────────

if [[ -n "${HPROF_HTML:-}" ]]; then
  echo "→ Using existing report: $HPROF_HTML"
elif [[ -n "$DUMP_PATH" ]]; then
  HPROF_HTML="/tmp/click-sweep-report.html"
  echo "→ Generating report from $DUMP_PATH..."
  "$BINARY" heap report "$DUMP_PATH" --format html --output "$HPROF_HTML"
  echo "→ Report written to $HPROF_HTML"
else
  echo "Error: provide a .hprof path or set HPROF_HTML to a pre-generated report."
  echo ""
  echo "  Usage: $0 /path/to/dump.hprof [click-sweep options...]"
  echo "  Or:    HPROF_HTML=/path/to/report.html $0 [click-sweep options...]"
  exit 2
fi

export HPROF_HTML

# ── npm install ────────────────────────────────────────────────────────────────

PLAYWRIGHT_DIR="$REPO_ROOT/tests/playwright"
if [[ ! -d "$PLAYWRIGHT_DIR/node_modules" ]]; then
  echo "→ Installing npm dependencies..."
  (cd "$PLAYWRIGHT_DIR" && npm install)
  echo "→ Installing Playwright browsers..."
  (cd "$PLAYWRIGHT_DIR" && npx playwright install chromium)
fi

# ── Run ────────────────────────────────────────────────────────────────────────

echo ""
(cd "$PLAYWRIGHT_DIR" && npx ts-node click-sweep.ts "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}")
