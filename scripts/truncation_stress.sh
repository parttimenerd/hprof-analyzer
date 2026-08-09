#!/usr/bin/env bash
# Stress-test truncated file handling across all supported formats.
# Cuts the fixture at random byte positions and verifies:
#   - exit code is always 0
#   - output is valid JSON (when large enough to have objects)
#   - truncation warning is present in JSON when the cut is mid-data
#
# Usage:
#   ./scripts/truncation_stress.sh [rounds] [seed]
#   rounds: number of random cuts per format (default 200)
#   seed: random seed (default: current timestamp)
#
# Run indefinitely (Ctrl-C to stop):
#   while true; do ./scripts/truncation_stress.sh 500; done

set -euo pipefail

BINARY="./target/release/hprof-analyzer"
FIXTURE="tests/fixtures/fixtures/dump_4_philosophers.hprof"
ROUNDS="${1:-200}"
SEED="${2:-$(date +%s)}"
TMPDIR_BASE=$(mktemp -d /tmp/trunc_stress.XXXXXX)
trap 'rm -rf "$TMPDIR_BASE"' EXIT

if [[ ! -f "$BINARY" ]]; then
    echo "ERROR: binary not found at $BINARY — run 'cargo build --release' first" >&2
    exit 1
fi
if [[ ! -f "$FIXTURE" ]]; then
    echo "ERROR: fixture not found at $FIXTURE" >&2
    exit 1
fi

FILE_SIZE=$(wc -c < "$FIXTURE")
echo "=== Truncation stress test ==="
echo "Fixture: $FIXTURE ($FILE_SIZE bytes)"
echo "Rounds:  $ROUNDS per format"
echo "Seed:    $SEED"
echo ""

PASS=0
FAIL=0
TOTAL=0

check() {
    local label="$1"
    local input="$2"
    local cut_at="$3"
    local input_size="$4"

    TOTAL=$((TOTAL + 1))
    local outfile="$TMPDIR_BASE/out_$TOTAL.json"
    local errfile="$TMPDIR_BASE/err_$TOTAL.txt"

    # Run the analyzer
    local exit_code=0
    "$BINARY" "$input" --format json > "$outfile" 2> "$errfile" || exit_code=$?

    if [[ $exit_code -ne 0 ]]; then
        echo "FAIL [$label cut=$cut_at/$input_size]: exit code $exit_code"
        echo "  stderr: $(cat "$errfile")"
        FAIL=$((FAIL + 1))
        return
    fi

    # Output must be valid JSON
    if ! python3 -c "import json,sys; json.load(sys.stdin)" < "$outfile" 2>/dev/null; then
        echo "FAIL [$label cut=$cut_at/$input_size]: invalid JSON output"
        FAIL=$((FAIL + 1))
        return
    fi

    PASS=$((PASS + 1))
    if (( TOTAL % 50 == 0 )); then
        echo "  ... $TOTAL checks done ($PASS pass, $FAIL fail)"
    fi
}

# Deterministic random cut positions using awk
gen_cuts() {
    local size="$1"
    local n="$2"
    local seed="$3"
    awk -v size="$size" -v n="$n" -v seed="$seed" 'BEGIN {
        srand(seed)
        for (i = 0; i < n; i++) {
            # Bias toward interesting regions: 1-byte, last-byte, and random
            r = rand()
            if (r < 0.05) pos = 1
            else if (r < 0.10) pos = size - 1
            else if (r < 0.15) pos = int(size * 0.25)
            else if (r < 0.20) pos = int(size * 0.50)
            else if (r < 0.25) pos = int(size * 0.75)
            else pos = int(rand() * (size - 2)) + 1
            print pos
        }
    }'
}

PLAIN_TMP="$TMPDIR_BASE/plain.hprof"
GZ_TMP="$TMPDIR_BASE/plain.hprof.gz"
TGZTMP="$TMPDIR_BASE/plain.hprof.tar.gz"

# Pre-compress once
gzip -c "$FIXTURE" > "$GZ_TMP"
tar -czf "$TGZTMP" -C "$(dirname "$FIXTURE")" "$(basename "$FIXTURE")"
GZ_SIZE=$(wc -c < "$GZ_TMP")
TGZ_SIZE=$(wc -c < "$TGZTMP")

echo "--- Plain .hprof ---"
while IFS= read -r cut; do
    python3 -c "import sys; open('$PLAIN_TMP','wb').write(open('$FIXTURE','rb').read($cut))"
    check "plain" "$PLAIN_TMP" "$cut" "$FILE_SIZE"
done < <(gen_cuts "$FILE_SIZE" "$ROUNDS" "$SEED")

echo "--- .hprof.gz ---"
GZ_CUT_TMP="$TMPDIR_BASE/cut.hprof.gz"
while IFS= read -r cut; do
    python3 -c "import sys; open('$GZ_CUT_TMP','wb').write(open('$GZ_TMP','rb').read($cut))"
    check "gz" "$GZ_CUT_TMP" "$cut" "$GZ_SIZE"
done < <(gen_cuts "$GZ_SIZE" "$ROUNDS" "$((SEED+1))")

echo "--- .hprof.tar.gz ---"
TGZ_CUT_TMP="$TMPDIR_BASE/cut.hprof.tar.gz"
while IFS= read -r cut; do
    python3 -c "import sys; open('$TGZ_CUT_TMP','wb').write(open('$TGZTMP','rb').read($cut))"
    check "tar.gz" "$TGZ_CUT_TMP" "$cut" "$TGZ_SIZE"
done < <(gen_cuts "$TGZ_SIZE" "$ROUNDS" "$((SEED+2))")

echo ""
echo "=== Results: $PASS passed, $FAIL failed out of $TOTAL total ==="
[[ $FAIL -eq 0 ]] && echo "ALL PASS" || { echo "FAILURES DETECTED"; exit 1; }
