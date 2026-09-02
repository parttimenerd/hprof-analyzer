# Changelog

All notable changes to hprof-analyzer are documented here.

## [Unreleased]

### Added

- **Setup & Usage modal in the browser UI.** Clicking "Setup & Usage" in the
  footer opens a modal with install instructions (Homebrew, Cargo, prebuilt
  binary), CLI quick reference, and MCP setup for Claude Code, Cline, and
  Claude Desktop. All code blocks are syntax-highlighted (shell and JSON).

- **Collapsible install panel on the browser landing page.** "— or install it
  locally (and with coding agents) —" expands to show CLI install tabs and AI
  integration cards inline, without leaving the page.

- **OQL syntax highlighting in the browser shell.** Both the sidebar query
  preview (HTML spans) and the xterm.js terminal input (ANSI codes) now
  highlight keywords, strings, numbers, identifiers, and `--` comments.

- **Shell and JSON syntax highlighting in setup guide code blocks.** Shell
  commands are tokenised into command/subcommand/flag/string/path/comment
  classes; JSON config blocks highlight keys, string values, keywords, and
  numbers. Both follow the page's light/dark theme.

- **Sample dump cards always visible on the landing page.** Sample dumps
  (mnemonics, scala-doku, philosophers, gauss-mix) are shown at all times;
  cards are grayed out when the server does not serve the files (e.g. local
  `file://` open) and become clickable once probed as available.

- **`mat caches` in CLI usage guides.** The setup modal, install panel,
  and README quick reference now include `hprof-analyzer mat caches dump.hprof`
  as a key command.

### Fixed

- **MCP: OQL alias `AS RETAINED` conflicted with reserved keyword.** All
  generated queries (examples, `get_summary`) now use `AS ret` / `AS ret_bytes`
  instead of `AS retained` / `AS RETAINED`.

- **MCP: `browse_dominators` / `query` responses had a trailing `/* HINT */`
  comment** that broke `json.loads()` for every model calling the tool. The
  hint is now in a `_hint` JSON field.

- **MCP: `get_report` now exposes all 17 report sections** with documentation
  updated to match actual JSON field names.

- **MCP: named queries use `@objectId AS idx`** instead of `@objectAddress`,
  so results plug directly into `browse_dominators` / `inspect_object` without
  address conversion.

- **CLI: `heap cache-list` now accepts a `.hprof` file path** (previously
  failed when passed a file rather than a directory).

- **MCP: leak detection fixed for small/medium heaps.** `load_dump` now embeds
  the top-5 suspect list in its response; `NEXT STEPS` now point to
  `get_report({section:"leaks"})` as the first action rather than the OQL
  `leak-suspects` view, which has a >10 MB threshold and returns 0 rows on
  small/medium heaps.

### Changed

- **OQL footguns documented:** glob patterns do not match inner classes
  (`ZipFile*` misses `ZipFile$Source` — use exact name or `INSTANCEOF`);
  `retained` / `RETAINED` column alias conflicts with `AS RETAINED SET`;
  `@objectId` is 0-based while `obj_index_1based` from `get_report` is 1-based.

- **MCP `get_report` description** updated: calls it "BEST TOOL for finding the
  leak / why OOM", explains single vs. group suspect JSON structures, and
  clarifies that the `leak-suspects` OQL view has a >10 MB threshold.

- **README "Browser mode" section removed.** Content was stale and duplicated
  the "Try it in the browser" section above it.


  Protocol so Claude Code, Cline, Claude Desktop, and any other MCP-compatible
  AI assistant can load and analyze heap dumps interactively. Tools: `get_session_info`,
  `get_oql_docs`, `load_dump`, `get_summary`, `get_histogram`, `get_report`, `query`,
  `browse_dominators`, `inspect_object`. The server is stateful — `load_dump` must be
  called first, after which all other tools use the same session. Optionally pre-load
  a dump with `hprof-analyzer mcp --dump heap.hprof`.

- **`heap` subcommand group.** Every MCP tool is also a CLI command for scripting
  and LLM-driven workflows:
  ```
  hprof-analyzer heap summary <dump>
  hprof-analyzer heap histogram <dump> [--limit N] [--json]
  hprof-analyzer heap report <dump> [--section leaks|top|threads|overview|all] [--json]
  hprof-analyzer heap query <dump> --oql "..." [--json]
  hprof-analyzer heap browse <dump> [--index N] [--depth D] [--width W] [--json]
  hprof-analyzer heap inspect <dump> --index N [--json]
  hprof-analyzer heap docs [--topic syntax|attributes|examples|workflow|all]
  hprof-analyzer heap load <dump> [--with-graph]
  hprof-analyzer heap cache-list [<dump>]
  hprof-analyzer heap cache-clear <dump>
  ```
  Output is human-readable text by default; `--json` for machine-parseable output.

- **Disk cache.** The first analysis of a dump writes a cache to
  `<dump>.hprof-cache/<hash>/` (5–15 min, 70–400 MB). Every subsequent call for
  the same dump loads in ~1 s. Cache is content-addressed (first 64 bytes + file
  size + mtime) and busts automatically on dump changes. Add `--with-graph` on the
  first load to cache the reference graph for OQL `@inbounds`/`@outbounds` traversal
  (adds 200–600 MB).

- **Homebrew tap.** Install via:
  ```sh
  brew tap parttimenerd/hprof-analyzer
  brew trust parttimenerd/hprof-analyzer
  brew install hprof-analyzer
  ```
  A rolling nightly formula tracks every push to `main`; a stable versioned formula
  is published on each tagged release.

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

### Fixed

- `inspect_object` no longer panics when called with `with_graph=true` if
  the inbound CSR was not cached (the graph cache stores forward edges only;
  a bounds-check now guards the inbound lookup).

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
