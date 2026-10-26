#!/usr/bin/env bash
# Regenerate the README performance table.
#
# Must run on the ThinkStation (Linux, /usr/bin/time -v, enough RAM).
# Usage:
#   scripts/update-bench.sh [--skip-mat] [--skip-hprof] [--skip-34g]
#
# Options:
#   --skip-mat    Skip MAT runs (use existing numbers from last run)
#   --skip-hprof  Skip hprof-analyzer runs (use existing numbers from last run)
#   --skip-34g    Skip the 34 GiB real-world dump (takes 15-25 min each way)
#
# On success the script rewrites the "## Performance" section of README.md
# in-place and prints a diff.  Commit the result yourself.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${BIN:-$REPO_ROOT/target/release/hprof-analyzer}"
MAT="${MAT:-/home/i560383/mat/mat/ParseHeapDump.sh}"
DUMPS_DIR="${DUMPS_DIR:-/home/i560383/test-heapdumps}"

SKIP_MAT=0
SKIP_HPROF=0
SKIP_34G=0
for arg in "$@"; do
  case $arg in
    --skip-mat)   SKIP_MAT=1 ;;
    --skip-hprof) SKIP_HPROF=1 ;;
    --skip-34g)   SKIP_34G=1 ;;
  esac
done

if [[ ! -x "$BIN" ]]; then
  echo "error: binary not found: $BIN" >&2
  echo "       Run: cargo build --release" >&2
  exit 1
fi

TMPDIR_BENCH="$(mktemp -d -t hprof-bench.XXXXXX)"
trap 'rm -rf "$TMPDIR_BENCH"' EXIT

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

fmt_wall() {
  # Convert fractional seconds to m:ss or Xs string
  local secs="$1"
  local isecs="${secs%.*}"
  if (( isecs < 60 )); then
    printf "%.1f s" "$secs"
  else
    printf "%d:%02d" "$(( isecs / 60 ))" "$(( isecs % 60 ))"
  fi
}

fmt_rss() {
  # Input: kibibytes (from GNU time -v "Maximum resident set size")
  local kb="$1"
  local mb=$(( kb / 1024 ))
  if (( mb < 1024 )); then
    echo "${mb} MiB"
  else
    # Round to 2 decimal places in GiB
    awk "BEGIN { printf \"%.2f GiB\", $kb / 1048576 }"
  fi
}

run_hprof() {
  local dump="$1" tag="$2" extra_flags="${3:-}"
  local out="$TMPDIR_BENCH/${tag}.md"
  local log="$TMPDIR_BENCH/${tag}.log"

  # GNU time -v writes its stats to stderr; redirect both so we can grep
  /usr/bin/time -v bash -c \
    "$BIN '$dump' '$out' --verbose $extra_flags 2>&1" \
    > "$log" 2>&1 || true

  local wall rss
  wall=$(grep "Elapsed (wall clock)" "$log" | sed 's/.*: //' | tr -d '[:space:]')
  rss=$(grep "Maximum resident set size" "$log" | sed 's/.*: //' | tr -d '[:space:]')

  # GNU time outputs wall as h:mm:ss or m:ss; convert to total seconds
  local secs
  secs=$(awk -v t="$wall" 'BEGIN {
    n = split(t, a, ":");
    if (n == 3) { print a[1]*3600 + a[2]*60 + a[3] }
    else if (n == 2) { print a[1]*60 + a[2] }
    else { print a[1] }
  }')

  echo "${secs} ${rss}"  # "WALL_SECONDS RSS_KB"
}

run_mat() {
  local dump="$1" tag="$2"
  local log="$TMPDIR_BENCH/mat_${tag}.log"
  local dir base

  dir=$(dirname "$dump")
  base=$(basename "$dump" .hprof)

  # Remove MAT index files so we measure a cold parse, not cache reuse
  rm -f "${dir}/${base}".*.index "${dir}/${base}".index 2>/dev/null || true

  /usr/bin/time -v bash "$MAT" -Xmx80g "$dump" \
    > "$log" 2>&1 || true

  local wall rss
  wall=$(grep "Elapsed (wall clock)" "$log" | sed 's/.*: //' | tr -d '[:space:]')
  rss=$(grep "Maximum resident set size" "$log" | sed 's/.*: //' | tr -d '[:space:]')

  local secs
  secs=$(awk -v t="$wall" 'BEGIN {
    n = split(t, a, ":");
    if (n == 3) { print a[1]*3600 + a[2]*60 + a[3] }
    else if (n == 2) { print a[1]*60 + a[2] }
    else { print a[1] }
  }')

  echo "${secs} ${rss}"
}

# ---------------------------------------------------------------------------
# Benchmark runs
# ---------------------------------------------------------------------------

declare -A WALL_BASIC RSS_BASIC WALL_FULL RSS_FULL WALL_MAT RSS_MAT

DUMPS=(
  "scala-doku:$DUMPS_DIR/dump_2_scala-doku.hprof"
  "gauss-mix:$DUMPS_DIR/dump_7_gauss-mix.hprof"
  "naive-bayes:$DUMPS_DIR/dump_5_naive-bayes.hprof"
  "vscode:$DUMPS_DIR/dump_vscode.hprof"
  "h16g:$DUMPS_DIR/heapothesys/hyperalloc_16g.hprof"
  "h28g:$DUMPS_DIR/heapothesys/hyperalloc_28g.hprof"
)
if (( SKIP_34G == 0 )); then
  DUMPS+=("r34g:$DUMPS_DIR/pc52bs2job-triage-c475689d4-brk4b-20260617_145230.hprof")
fi

for entry in "${DUMPS[@]}"; do
  tag="${entry%%:*}"
  dump="${entry#*:}"

  echo "=== $tag ==="

  if (( SKIP_HPROF == 0 )); then
    echo "  hprof basic..."
    read -r ws rk <<< "$(run_hprof "$dump" "${tag}_basic")"
    WALL_BASIC[$tag]="$ws"; RSS_BASIC[$tag]="$rk"

    echo "  hprof full..."
    read -r ws rk <<< "$(run_hprof "$dump" "${tag}_full" "--full-analysis")"
    WALL_FULL[$tag]="$ws"; RSS_FULL[$tag]="$rk"
  fi

  if (( SKIP_MAT == 0 )); then
    echo "  MAT..."
    read -r ws rk <<< "$(run_mat "$dump" "$tag")"
    WALL_MAT[$tag]="$ws"; RSS_MAT[$tag]="$rk"
  fi
done

# ---------------------------------------------------------------------------
# Build table rows
# ---------------------------------------------------------------------------

COMMIT=$(git -C "$REPO_ROOT" rev-parse --short HEAD)
DATE=$(date +%Y-%m-%d)

row() {
  local label="$1" size="$2" tag="$3"
  local wb rb wf rf wm rm
  wb=$(fmt_wall "${WALL_BASIC[$tag]}")
  rb=$(fmt_rss  "${RSS_BASIC[$tag]}")
  wf=$(fmt_wall "${WALL_FULL[$tag]}")
  rf=$(fmt_rss  "${RSS_FULL[$tag]}")
  wm=$(fmt_wall "${WALL_MAT[$tag]}")
  rm=$(fmt_rss  "${RSS_MAT[$tag]}")
  printf "| %s | %s | %s | %s | %s | %s | %s | %s |\n" \
    "$label" "$size" "$wb" "$rb" "$wf" "$rf" "$wm" "$rm"
}

TABLE_ROWS=""
TABLE_ROWS+="$(row "Renaissance scala-doku" "51 MiB"   scala-doku)"$'\n'
TABLE_ROWS+="$(row "gauss-mix"              "70 MiB"   gauss-mix)"$'\n'
TABLE_ROWS+="$(row "naive-bayes (1.3 GiB)" "1.3 GiB"  naive-bayes)"$'\n'
TABLE_ROWS+="$(row "VS Code JVM (1.1 GiB)" "1.1 GiB"  vscode)"$'\n'
TABLE_ROWS+="$(row "HeapothesYs 16g"        "11 GiB"   h16g)"$'\n'
TABLE_ROWS+="$(row "HeapothesYs 28g"        "20 GiB"   h28g)"$'\n'
if (( SKIP_34G == 0 )); then
  TABLE_ROWS+="$(row "Real-world 34g"       "34 GiB"   r34g)"$'\n'
fi

HEADER_LINE="Measured on an AMD Ryzen Threadripper PRO 3995WX (64 cores / 128 threads) with 123 GiB RAM, Linux, commit [\`${COMMIT}\`](https://github.com/parttimenerd/hprof-analyzer/commit/${COMMIT}), ${DATE}. \"Basic\" = default analysis; \"Full\" = \`--full-analysis\` (\`--obj-graph --collections --find-duplicates\`). MAT = Eclipse MAT 1.17.0 (\`ParseHeapDump.sh -Xmx80g\`). Wall-clock in \`m:ss\` or \`s\`. RSS is peak."

TABLE_HEADER="| Workload | Dump file | Wall basic | RSS basic | Wall full | RSS full | Wall (MAT) | RSS (MAT) |
|----------|-----------|------------|-----------|-----------|----------|------------|-----------|"

# ---------------------------------------------------------------------------
# Rewrite README in-place
# ---------------------------------------------------------------------------

python3 - "$REPO_ROOT/README.md" "$HEADER_LINE" "$TABLE_HEADER" "$TABLE_ROWS" <<'PYEOF'
import sys, re

readme_path = sys.argv[1]
header_line = sys.argv[2]
table_header = sys.argv[3]
table_rows = sys.argv[4].rstrip('\n')

with open(readme_path) as f:
    text = f.read()

# Replace the "Measured on..." header line
text = re.sub(
    r'^Measured on an AMD Ryzen.*$',
    header_line,
    text,
    flags=re.MULTILINE
)

# Replace the table: header row + separator + all data rows up to the blank line
table_block = table_header + '\n' + table_rows
text = re.sub(
    r'^\| Workload \| Dump file \|.*?(?=^\s*$)',
    table_block + '\n',
    text,
    flags=re.MULTILINE | re.DOTALL
)

with open(readme_path, 'w') as f:
    f.write(text)

print("README.md updated.")
PYEOF

echo ""
echo "Done. Review the diff:"
git -C "$REPO_ROOT" diff README.md
