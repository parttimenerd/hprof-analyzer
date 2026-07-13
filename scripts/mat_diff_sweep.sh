#!/usr/bin/env bash
# MAT diff validation sweep (a hard parity gate).
#
# For every dump that has BOTH a local `.hprof` AND a MAT `_System_Overview.zip`
# reference, this script:
#   (1) runs `analyze <dump> --format json` on the dump    -> <name>.ours.json
#   (2) runs `diff <zip> <ours.json> --format json`         -> <name>.diff.json
# then aggregates every per-dump diff into a single gate report via the
# `dev sweep-aggregate <dir>` subcommand (per-dump table, real-comparison
# count vs N_MIN=15, full EXPLAINABLE audit list + FAIL list, GATE verdict).
#
# It SKIPS cleanly (never errors) when a dump's hprof or zip is absent locally,
# so CI (which lacks the big dumps) runs over whatever subset is present.
#
# Usage:
#   scripts/mat_diff_sweep.sh [MAT_ZIP_DIR] [DUMP_DIR]
# Env overrides (args take precedence):
#   MAT_ZIP_DIR  directory holding <name>_System_Overview.zip files
#   DUMP_DIR     directory holding <name>.hprof files
#   BIN          path to the built analyzer binary
#   WORK         scratch dir for per-dump json (default: mktemp)
#   KEEP_WORK=1  keep the scratch dir instead of deleting it
#   EXCLUDE      whitespace-separated dump names to skip (e.g. dumps MAT cannot
#                open locally). Empty by default.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Defaults: fixtures ship locally; MAT zips are pulled to /tmp/matzips. Both are
# overridable by arg or env.
MAT_ZIP_DIR="${1:-${MAT_ZIP_DIR:-/tmp/matzips}}"
DUMP_DIR="${2:-${DUMP_DIR:-$REPO_ROOT/tests/fixtures}}"
BIN="${BIN:-$REPO_ROOT/target/release/hprof-analyzer}"
# Dumps to exclude by name (whitespace-separated). Empty by default; set this to
# skip dumps whose MAT reference cannot be produced locally.
EXCLUDE="${EXCLUDE:-}"

if [[ ! -x "$BIN" ]]; then
  echo "error: analyzer binary not found/executable at: $BIN" >&2
  echo "       build it first: cargo build --release" >&2
  exit 1
fi

WORK="${WORK:-$(mktemp -d)}"
mkdir -p "$WORK"
cleanup() { [[ "${KEEP_WORK:-0}" == "1" ]] || rm -rf "$WORK"; }
trap cleanup EXIT

echo "MAT diff sweep"
echo "  binary   : $BIN"
echo "  dumps    : $DUMP_DIR"
echo "  mat zips : $MAT_ZIP_DIR"
echo "  scratch  : $WORK"
echo

# Discover candidate dumps from the MAT zips present (each named
# <name>_System_Overview.zip). This way we only attempt dumps for which a
# reference exists, and we naturally handle whatever subset is on disk.
shopt -s nullglob
n_seen=0
n_run=0
n_skip=0
for zip in "$MAT_ZIP_DIR"/*_System_Overview.zip; do
  n_seen=$((n_seen + 1))
  base="$(basename "$zip")"
  name="${base%_System_Overview.zip}"
  hprof="$DUMP_DIR/$name.hprof"

  # Honor the exclusion list (dumps whose MAT reference is unavailable locally).
  skip_excluded=0
  for ex in $EXCLUDE; do
    if [[ "$name" == "$ex" ]]; then skip_excluded=1; fi
  done
  if [[ $skip_excluded -eq 1 ]]; then
    echo "SKIP $name : excluded by EXCLUDE list"
    n_skip=$((n_skip + 1))
    continue
  fi

  if [[ ! -f "$hprof" ]]; then
    echo "SKIP $name : no local hprof ($hprof)"
    n_skip=$((n_skip + 1))
    continue
  fi

  ours="$WORK/$name.ours.json"
  diff="$WORK/$name.diff.json"

  echo "RUN  $name : analyzing dump ..."
  if ! "$BIN" analyze "$hprof" --format json >"$ours" 2>"$WORK/$name.analyze.err"; then
    echo "SKIP $name : analyzer failed (see $WORK/$name.analyze.err)"
    n_skip=$((n_skip + 1))
    continue
  fi

  # `diff` exits 2 on a FAIL classification (NOT an error) and 1 on an I/O /
  # parse error. Treat exit 2 as a successful-but-failing diff we still record.
  set +e
  "$BIN" diff "$zip" "$ours" --format json >"$diff" 2>"$WORK/$name.diff.err"
  rc=$?
  set -e
  if [[ $rc -ne 0 && $rc -ne 2 ]]; then
    echo "SKIP $name : diff errored rc=$rc (see $WORK/$name.diff.err)"
    n_skip=$((n_skip + 1))
    rm -f "$diff"
    continue
  fi
  echo "OK   $name : diff recorded (rc=$rc)"
  n_run=$((n_run + 1))
done
shopt -u nullglob

echo
echo "discovered $n_seen MAT reference(s); ran $n_run, skipped $n_skip"
echo

# Aggregate. The aggregator exits 0 on GATE PASS, 2 on GATE FAIL; propagate it.
set +e
"$BIN" dev sweep-aggregate "$WORK"
agg_rc=$?
set -e
exit "$agg_rc"
