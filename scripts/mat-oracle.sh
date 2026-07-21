#!/usr/bin/env bash
# Eclipse MAT headless OQL runner for the differential oracle (tests/mat_oracle.rs).
#
# Given an hprof dump and an OQL query, this asks Eclipse MAT to run the query
# headless and prints the matching object addresses to stdout (one per line).
# The Rust harness filters stdout down to address lines (0x<hex> or decimal) and
# ignores everything else, so extra banner/log output here is harmless.
#
# !!! UNVERIFIED against a live MAT !!!
# MAT is NOT installed in this development environment, so this invocation is
# written against MAT's *documented* headless interface (ParseHeapDump.sh /
# org.eclipse.mat.api:query). The exact OQL export format varies between MAT
# versions; treat this script as a documented starting point that must be
# validated against a real MAT install before the oracle is trusted.
#
# Usage:
#   MAT_HOME=/path/to/mat scripts/mat-oracle.sh <hprof> <oql>
# Env:
#   MAT_HOME  path to an Eclipse MAT installation (containing ParseHeapDump.sh).
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <hprof> <oql>" >&2
  echo "       (requires MAT_HOME to point at an Eclipse MAT installation)" >&2
  exit 2
fi

hprof="$1"
oql="$2"

if [[ -z "${MAT_HOME:-}" ]]; then
  echo "error: MAT_HOME must point to an Eclipse MAT installation (containing ParseHeapDump.sh)" >&2
  exit 3
fi

MAT_SH="$MAT_HOME/ParseHeapDump.sh"
if [[ ! -x "$MAT_SH" ]]; then
  echo "error: ParseHeapDump.sh not found or not executable at: $MAT_SH" >&2
  exit 3
fi

# MAT headless OQL. The exact export format varies by MAT version; this uses the
# documented org.eclipse.mat.api:query application via ParseHeapDump.sh. The
# downstream Rust harness parses one 0x<hex> or decimal address per line and
# ignores any non-address lines, so we simply emit whatever MAT prints.
"$MAT_SH" "$hprof" \
  -command="oql $oql" \
  2>/dev/null || true
