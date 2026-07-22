#!/usr/bin/env bash
# Eclipse MAT headless OQL runner for the differential oracle (tests/mat_oracle.rs).
#
# Given an hprof dump and an OQL query, this asks Eclipse MAT to run the query
# headless and prints the resulting cell values to stdout (one row per line,
# columns joined by tabs). The Rust harness filters stdout down to the lines it
# cares about (addresses / scalars) and ignores banner/log noise.
#
# VERIFIED against MAT 1.13.0 (2026-07-23) on macOS.
#
# Mechanism: MAT's parse application (org.eclipse.mat.api.parse, invoked by
# ParseHeapDump.sh) runs a *report spec*, not a raw command. The registered
# `org.eclipse.mat.api:query` report substitutes ${command} from a report
# parameter map that the CLI never populates, so `-command=...` silently
# produces an empty command. Instead we generate a self-contained report XML
# with the OQL hard-coded into a <query><command>oql "..."</command></query>
# element and a per-query `format=csv` param. MAT writes the result table to
# <dump>_<section>.zip containing pages/<QueryName>N.csv (';'-separated,
# addresses in DECIMAL). We unzip it, strip the header, and emit the rows.
#
# Notes on parity:
#   * MAT discards UNREACHABLE objects during indexing; our `query` subcommand
#     scans the raw heap. So MAT returns a subset for class queries that match
#     unreachable instances. Compare address SETS accordingly (MAT ⊆ ours).
#   * OQL must be wrapped as `oql "<query>"` — a bare `SELECT ...` is parsed as
#     a MAT command name ("Command SELECT not found").
#
# Usage:
#   MAT_HOME=/path/to/mat/Eclipse scripts/mat-oracle.sh <hprof> <oql>
# Env:
#   MAT_HOME  path to an Eclipse MAT installation directory containing
#             ParseHeapDump.sh (e.g. /Applications/mat.app/Contents/Eclipse).
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

# Escape the OQL for embedding inside an XML text node and a double-quoted
# `oql "..."` MAT command. XML-escape &, <, >; the OQL itself may contain "
# (e.g. LIKE "regex") — MAT's command parser handles inner quotes poorly, so
# callers should prefer single-quoted regexes. We escape " as &quot; defensively.
xml_escape() {
  printf '%s' "$1" \
    | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g'
}

oql_escaped="$(xml_escape "$oql")"

# Build a self-contained report spec with the OQL hard-coded.
report_xml="$(mktemp -t mat_oql_report.XXXXXX.xml)"
trap 'rm -f "$report_xml"' EXIT
cat > "$report_xml" <<XML
<?xml version="1.0" encoding="UTF-8"?>
<section name="OQLOracle" xmlns="http://www.eclipse.org/mat/report.xsd">
    <param key="limit" value="100000000" />
    <query name="OQLResult">
        <param key="format" value="csv" />
        <param key="limit" value="100000000" />
        <command>oql "${oql_escaped}"</command>
    </query>
</section>
XML

# Run MAT headless. It writes <dump-basename>_OQLOracle.zip next to the dump.
# ParseHeapDump.sh resets cwd; run it from MAT_HOME and pass absolute paths.
hprof_abs="$(cd "$(dirname "$hprof")" && pwd)/$(basename "$hprof")"
dump_dir="$(dirname "$hprof_abs")"
dump_base="$(basename "${hprof_abs%.*}")"
result_zip="$dump_dir/${dump_base}_OQLOracle.zip"

rm -f "$result_zip"
(
  cd "$MAT_HOME"
  ./ParseHeapDump.sh "$hprof_abs" "$report_xml" >/dev/null 2>&1 || true
)

if [[ ! -f "$result_zip" ]]; then
  echo "error: MAT produced no result zip ($result_zip) — query may have failed" >&2
  exit 4
fi

# Extract and emit CSV rows (skip header). MAT CSV is ';'-separated; join the
# columns with tabs and drop trailing separators.
extract_dir="$(mktemp -d -t mat_oql_out.XXXXXX)"
trap 'rm -f "$report_xml"; rm -rf "$extract_dir"' EXIT
unzip -o -q "$result_zip" -d "$extract_dir"

csv="$(find "$extract_dir/pages" -name '*.csv' 2>/dev/null | head -1)"
if [[ -z "$csv" ]]; then
  # Scalar results (e.g. COUNT(*)) are not emitted as CSV by MAT; surface the
  # HTML result instead so the caller can decide. Emit nothing and exit 0 —
  # the harness treats an empty address set as "no comparable rows".
  exit 0
fi

# Strip header line, remove ';' separators (single-column addr queries), and
# for multi-column rows join with tabs.
tail -n +2 "$csv" | sed -e 's/;$//' -e 's/;/\t/g'
