# Changelog

All notable changes to hprof-analyzer are documented here.

## [Unreleased]

### Added

- **Holder breakdown in HTML report.** The Biggest Classes section now shows
  a collapsible "Held by (immediate dominators)" widget. For each top class,
  it lists the immediate-dominator classes (with instance count and retained
  size) and their own immediate dominators (level 2), making it easy to
  answer "which classes are keeping all these HashMaps alive?" without
  leaving the Top Consumers section. Computed via two O(n) passes over all
  objects after the class ranking is built; capped at 10 holders per class
  and 10 level-2 entries per holder.

- **`holder_chain` on biggest objects.** `ObjRow` now carries up to 2
  immediate-dominator class names toward the GC root, available in the JSON
  model for tooling that consumes the report programmatically.

### Changed

- Holder breakdown is omitted from the plain Markdown renderer (it is
  present in the JSON model and HTML report only).

## [0.2.0] — 2026-08-10

### Added

- **Truncation resilience.** Truncated dumps, corrupt gzip streams, and
  malformed heap records all produce a partial report with a `truncated_input`
  warning rather than a crash or error exit. Corrupt length fields are capped
  before allocation to prevent OOM. Validated with a proptest fuzzing suite
  (8 suites × 500 cases) and a 10 000-iteration prefix-fuzz campaign.

- **`.tar.gz` / `.tgz` input support.** Heap dumps packaged as gzip-compressed
  tar archives are accepted everywhere a plain `.hprof` or `.hprof.gz` is —
  CLI, `diff`, `server`, `query`, and `mat` subcommands. Streamed on-the-fly;
  no decompression to disk required.

- **Android ART and IBM J9 support.** All HPROF sub-tags from Android ART
  (`ROOT_INTERNED_STRING`, `ROOT_DEBUGGER`, `ROOT_VM_INTERNAL`, `ROOT_JNI_MONITOR`,
  `PRIM_ARRAY_NODATA_DUMP`) and IBM J9 (`ROOT_SYSTEM_CLASS`) are now parsed
  correctly. Previously these produced `unknown heap sub tag` errors and aborted.

- **Interactive Object Graph Explorer.** Force-directed graph view for leak
  suspects and dominator tree nodes, powered by d3-force. Includes Inspector
  integration, neighbor dimming, and edge labels.

- **Heap Inspector panel.** Click-through from any object to see its fields,
  inbound references, retained size, and per-field value scan (top instances by
  retained size).

- **ThreadLocal Analysis section** in Markdown reports, plus Collection Waste
  Budget section with TOC.

- **`--field-stats` flag** for per-class reference-field null/non-null/retained
  breakdown.

- **WASM memory optimizations.** Compressed `shallow`/`retained`/`fwd_targets`
  arrays and a `run_fast_analysis_with_progress()` path that skips the retained
  pass — enables large dumps to load in-browser without OOM.

### Fixed

- All scan loops in pass 1, pass 2, and the field-decode layer now treat
  `InvalidData` (corrupt sub-tag, segment overrun, oversized array) identically
  to `UnexpectedEof` — stop scanning and emit a partial report instead of
  propagating a hard error.
- Saturating arithmetic throughout shallow-size helpers, collection-waste
  calculations, and array byte-length computations prevents integer overflow on
  file-derived values.
- Numerous HTML report display fixes: number formatting, percentage rendering,
  exact-byte tooltips, table overflow, column truncation.
