#!/usr/bin/env bash
# Regenerate the docs/samples/ report gallery from the committed scala-doku dump.
#
# Produces eight files: {scala-doku, scala-doku-full} × {.md, .graphs.md, .html,
# .json}. The plain set uses default options; the "-full" set adds every opt-in
# analysis (--find-duplicates --collections), mirroring the README's Default / All
# features toggle. All runs use --detail default so the samples stay a
# reasonable size.
#
# Run from the repo root (or anywhere; paths are resolved relative to this
# script). Requires a release binary; builds one if BIN is not provided.
#
#   ./scripts/gen-samples.sh                 # builds target/release binary
#   BIN=/path/to/hprof-analyzer ./scripts/gen-samples.sh   # use a prebuilt one
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

dump="tests/fixtures/dump_2_scala-doku.hprof"
out="docs/samples"

if [[ ! -f "$dump" ]]; then
  echo "error: dump not found at $dump" >&2
  exit 1
fi

bin="${BIN:-}"
if [[ -z "$bin" ]]; then
  cargo build --release
  bin="target/release/hprof-analyzer"
fi

mkdir -p "$out"

# Silence the live progress line; keep runs deterministic.
common=(--progress never --detail default)
full=(--find-duplicates --collections)

echo "generating default samples…"
"$bin" "$dump" "${common[@]}" --format md        > "$out/scala-doku.md"
"$bin" "$dump" "${common[@]}" --format md-graphs  > "$out/scala-doku.graphs.md"
"$bin" "$dump" "${common[@]}" --format html       > "$out/scala-doku.html"
"$bin" "$dump" "${common[@]}" --format json       > "$out/scala-doku.json"

echo "generating full (--find-duplicates --collections) samples…"
"$bin" "$dump" "${common[@]}" "${full[@]}" --format md        > "$out/scala-doku-full.md"
"$bin" "$dump" "${common[@]}" "${full[@]}" --format md-graphs  > "$out/scala-doku-full.graphs.md"
"$bin" "$dump" "${common[@]}" "${full[@]}" --format html       > "$out/scala-doku-full.html"
"$bin" "$dump" "${common[@]}" "${full[@]}" --format json       > "$out/scala-doku-full.json"

# Cross-dump compare sample (§37.4): a two-dump growth series. Uses philosophers
# as the baseline and scala-doku as the current dump; the compare engine reads
# the two JSON reports and renders the growth/spike/churn verdict.
echo "generating compare sample…"
base_dump="tests/fixtures/dump_4_philosophers.hprof"
if [[ -f "$base_dump" ]]; then
  tmp_base="$(mktemp)"
  tmp_curr="$(mktemp)"
  trap 'rm -f "$tmp_base" "$tmp_curr"' EXIT
  "$bin" "$base_dump" "${common[@]}" --format json > "$tmp_base"
  "$bin" "$dump"      "${common[@]}" --format json > "$tmp_curr"
  "$bin" compare reports "$tmp_base" "$tmp_curr" --format md   > "$out/compare-philosophers-scala-doku.md"
  "$bin" compare reports "$tmp_base" "$tmp_curr" --format html > "$out/compare-philosophers-scala-doku.html"
  echo "done: 10 files written to $out/"
else
  echo "note: $base_dump absent; skipped compare sample (8 files written)"
fi
