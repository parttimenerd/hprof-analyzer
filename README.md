# hprof-analyzer

[![CI](https://github.com/parttimenerd/hprof-analyzer/actions/workflows/ci.yml/badge.svg)](https://github.com/parttimenerd/hprof-analyzer/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hprof-analyzer.svg)](https://crates.io/crates/hprof-analyzer)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Your JVM left behind a multi-gigabyte `.hprof` heap dump — after an
`OutOfMemoryError`, a memory leak investigation, or just a routine heap snapshot.
You want to know **what is in the heap** without opening a large file in a GUI
or provisioning a machine as big as the dump.

`hprof-analyzer` is a command-line tool that reads the dump and writes a
self-contained report covering the same ground as [Eclipse MAT](https://eclipse.dev/mat/)'s System
Overview, Leak Suspects, and Top Consumers analyses, plus additional views. Peak
RSS stays well below the dump size: on a 33 GiB dump it peaks at ~15 GiB where
MAT needs ~62 GiB (see [Performance](#performance)). The report is a single file you can email, attach to a
ticket, or diff in CI. For dumps up to 3 GB you can also [run the analysis directly in
your browser](https://parttimenerd.github.io/hprof-analyzer/) — no install needed.

*An experimental tool by the [SapMachine](https://sapmachine.io) team.*

## What you get

Run one command and get a report with these sections:

- **System Overview**: heap size, class/classloader breakdown, duplicate class
  definitions, GC roots, and a per-class histogram with a largest-instance column.
- **Leak Suspects**: objects retaining the most memory, each traced back to its
  GC root via the full reference chain.
- **Top Consumers**: classes, classloaders, and packages ranked by *retained* size
  (not just shallow), so allocations hidden inside containers show up under the right owner.
- **Threads**: stack frames and the local variables each thread keeps alive.
- **Duplicate strings** (opt-in, `--find-duplicates`): wasted bytes from identical
  `String` values, top offenders, and which classes hold the most string references.
- **Collections analysis** (opt-in, `--collections`): fill ratios, size distributions,
  collision rates, and per-`Class#field` attribution for every Map, List, Set, and array.
  Covers standard JDK, Kotlin, and Eclipse Collections; custom types via TOML config.

Pick the format that fits: plain **Markdown**, **Markdown with ASCII graphs**
(bars, sparklines, dominator trees), a self-contained **HTML** page you can open
in any browser, or machine-readable **JSON**.

A live viewer shows all four output formats side by side, built from the public
[Renaissance benchmark](https://renaissance.dev/) `scala-doku` dump:

**➡ [Open the sample report viewer](https://parttimenerd.github.io/hprof-analyzer/reports/)**

| Format | Default options | All optional features |
|--------|-----------------|-----------------------|
| Plain Markdown | [`scala-doku.md`](docs/samples/scala-doku.md) | [`scala-doku-full.md`](docs/samples/scala-doku-full.md) |
| Markdown with ASCII graphs | [`scala-doku.graphs.md`](docs/samples/scala-doku.graphs.md) | [`scala-doku-full.graphs.md`](docs/samples/scala-doku-full.graphs.md) |
| Self-contained HTML (opens live) | [`scala-doku.html`](https://parttimenerd.github.io/hprof-analyzer/samples/scala-doku.html) | [`scala-doku-full.html`](https://parttimenerd.github.io/hprof-analyzer/samples/scala-doku-full.html) |
| Machine-readable JSON | [`scala-doku.json`](docs/samples/scala-doku.json) | [`scala-doku-full.json`](docs/samples/scala-doku-full.json) |

## Try it in the browser

**➡ [Open the browser UI](https://parttimenerd.github.io/hprof-analyzer/)**

Drop a `.hprof` file directly onto the page — the entire analysis runs in your
browser via WebAssembly, no install required. Heap dumps **up to 3 GB** are
supported.

| Landing page | OQL shell | Leak suspects report |
|:---:|:---:|:---:|
| ![Landing page](docs/screenshots/screenshot-landing.png) | ![OQL shell](docs/screenshots/screenshot-shell.png) | ![Leak suspects](docs/screenshots/screenshot-report-leaks.png) |

Three modes are available after dropping a file:

- **Analysis** — histogram, leak suspects, dominator tree, GC roots, retained sizes
- **Full Analysis** — adds duplicate-string/array detection and collection fill-ratio
- **OQL Shell** — interactive query shell; named-query sidebar + tab-completion work offline

You can also connect to a locally running server for larger dumps:

```sh
hprof-analyzer server heap.hprof   # prints http://127.0.0.1:7070
```

All REPL commands (`!top`, `!sort`, `!stats`, `!obj`, …) and the full OQL
engine work the same way in the browser and in the CLI. Useful shell commands:

```
/help oql        — full OQL language reference (works offline via WASM)
/examples        — guided tour of OQL examples by category
/examples group  — examples for GROUP BY / HAVING queries
```

Tab-completion and named-query browsing work fully offline via WASM.

## Browser mode

Load `web/dist/hprof-analyzer-browser.html` in Chrome or Firefox — no install needed.
Drag and drop a `.hprof` file to analyze it entirely in-browser via WebAssembly.

**Analysis modes:**
- **Full Analysis** — complete report with dominator tree, leak suspects, top consumers, and interactive Heap Inspector
- **Fast Analysis** — skips retained-heap computation; histogram and OQL work but Leak Suspects are unavailable. Use for dumps > 2 GB.

**Interactive features (WASM only):**
- **Heap Inspector** — click any class or instance to open a panel with fields, GC root path, and peer navigation
- **Field Scan** — top instances of any class ranked by retained heap
- **Object Graph Explorer** — force-directed graph with edge labels and neighbor dimming
- **OQL REPL** — run queries against the loaded dump

**Memory limits:** up to ~3 GB HPROF files in Chrome (4 GB WASM address space). After full analysis, large arrays are deflate-compressed (~75% reduction) to fit within memory limits.

## Quick start

Grab a prebuilt binary and analyze a dump in two commands. No Rust, no Node, no
build step. Pick the line for your platform (see [Install](#install) for all
targets and other install methods):

```sh
# macOS (Apple Silicon)
curl -L https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-aarch64-apple-darwin.tar.gz | tar xz

# Linux (x86_64, glibc)
curl -L https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-unknown-linux-gnu.tar.gz | tar xz
```

That unpacks a folder containing the `hprof-analyzer` binary. Run it on your
dump:

```sh
./hprof-analyzer-*/hprof-analyzer heap.hprof report.html
```

Open `report.html` in any browser. To run it from anywhere, move the binary
onto your `PATH`:

```sh
sudo mv hprof-analyzer-*/hprof-analyzer /usr/local/bin/
hprof-analyzer heap.hprof report.html
```

Gzip-compressed dumps (`.hprof.gz`) are read transparently. Analysis time
scales with the dump — seconds for small dumps, minutes for multi-gigabyte
ones (see [Performance](#performance)).

## Why you might want it

- **Memory-efficient and fast.** Two-pass streaming keeps peak RSS well below
  the dump size and uses a fraction of what MAT needs — no heap-size flag to
  tune. See [Performance](#performance) for measured numbers.
- **Scriptable and CI-friendly.** Never prompts, never opens a window. Emit
  JSON, diff two dumps to catch memory growth in a pipeline, or gate a build on
  retained-size regressions.
- **Emailable output.** The HTML report is a single self-contained file with no
  server and no external assets — attach it to a ticket or share it as-is.
- **Deterministic.** Markdown output is byte-stable (modulo the generation
  timestamp), so it diffs cleanly across runs and across dumps.

## When to use alternatives

This tool is **deliberately narrow**: it renders static replicas of the three
views above plus threads, and nothing else. If you need to *explore* a heap —
walk the dominator tree interactively, inspect arbitrary objects and their
fields, or use the full breadth of MAT's analyses — reach for
**[Eclipse MAT](https://eclipse.dev/mat/)**, the complete interactive GUI.

`hprof-analyzer` now also ships an **OQL query engine** (see
[docs/OQL.md](docs/OQL.md)) and an HTTP server (`server` subcommand) for
programmatic access, so for scripting and LLM-assisted dump triage it is
often a better fit than MAT. Use MAT when you need interactive GUI exploration.

If all you need is a class histogram,
[`hprof-slurp`](https://github.com/agourlay/hprof-slurp) is faster and lighter
because it never builds the dominator tree. But that also means it cannot report
retained sizes, leak suspects, root paths, or Top Consumers — the analyses
`hprof-analyzer` exists to provide.

## Speeding up Eclipse MAT

If you use Eclipse MAT for interactive heap exploration, hprof-analyzer can
dramatically reduce the time and memory needed for MAT's first open of a large
dump.

MAT parses a 34 GB heap dump in ~4 s and writes 12 cache files alongside the
`.hprof`. On the next open it reads from cache and loads in ~0.9 s — but the
first parse peaks at **~55 GB RSS** inside the JVM. hprof-analyzer generates
the same cache files in a single pass peaking at **~19 GB RSS**:

```sh
# Generate MAT cache files (low RSS, no JVM tuning needed)
hprof-analyzer mat caches heap.hprof /path/to/heap-dir/

# Now open heap.hprof in MAT as usual — it detects the cache and skips parsing
```

MAT auto-detects the cache: if the index files are present and newer than the
`.hprof`, it prints "Reopening parsed heap dump file" and skips its own parser.

If you also want hprof-analyzer's own report, generate both in one pass (single
hprof read, shared pipeline):

```sh
hprof-analyzer analyze heap.hprof --mat /path/to/heap-dir/ report.html
```

See [`docs/mat-cache.md`](docs/mat-cache.md) for the full list of generated
files, known divergences from MAT's output, and the RSS budget details.

## Install

### Prebuilt binary (recommended)

No Rust, no Node.js. Download for your platform from the rolling
[`nightly`](https://github.com/parttimenerd/hprof-analyzer/releases/tag/nightly)
release (always tracks `main`):

| Platform | Archive |
| --- | --- |
| Linux x86_64 (glibc) | [`hprof-analyzer-x86_64-unknown-linux-gnu.tar.gz`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-unknown-linux-gnu.tar.gz) |
| Linux x86_64 (static musl) | [`hprof-analyzer-x86_64-unknown-linux-musl.tar.gz`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-unknown-linux-musl.tar.gz) |
| Linux aarch64 (glibc) | [`hprof-analyzer-aarch64-unknown-linux-gnu.tar.gz`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-aarch64-unknown-linux-gnu.tar.gz) |
| Linux aarch64 (static musl) | [`hprof-analyzer-aarch64-unknown-linux-musl.tar.gz`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-aarch64-unknown-linux-musl.tar.gz) |
| macOS (Apple Silicon) | [`hprof-analyzer-aarch64-apple-darwin.tar.gz`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-aarch64-apple-darwin.tar.gz) |
| Windows x86_64 | [`hprof-analyzer-x86_64-pc-windows-msvc.zip`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-pc-windows-msvc.zip) |

Use the musl build on minimal containers or older distros (no libc dependency).

```sh
curl -L https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv hprof-analyzer-*/hprof-analyzer /usr/local/bin/
```

### With Cargo

Requires Rust 1.85+. If you don't have it, install [rustup](https://rustup.rs/) first:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo install hprof-analyzer
```

### From source

```sh
git clone https://github.com/parttimenerd/hprof-analyzer
cd hprof-analyzer
cargo build --release
# binary at target/release/hprof-analyzer
```

Node.js/npm is only needed if you modify the web sources under `web/src/`.

## Usage

```
hprof-analyzer <INPUT> [OUTPUT] [OPTIONS]

  <INPUT>   a .hprof[.gz] heap dump  → analyze it and write a report
            a saved report .json[.gz] → re-render it to another format

Named subcommands:
  compare      Compare reports (MAT export vs ours, or two of ours across time)
  completions  Generate a shell completion script
  mat          Generate Eclipse MAT index cache files (low-RSS alternative to MAT's first parse)
  query        Run OQL queries against a heap dump (no full report needed)
  server       Serve OQL + report sections over HTTP
  dev          Developer / diagnostic commands
```

### Analyze a dump

Output format is inferred from the extension; `-f` always wins. Stdout defaults
to plain Markdown.

```sh
hprof-analyzer heap.hprof                    # plain Markdown to stdout
hprof-analyzer heap.hprof report.html        # HTML
hprof-analyzer heap.hprof report.json        # JSON
hprof-analyzer heap.hprof report.json.gz     # gzip-compressed JSON (~20× smaller)
hprof-analyzer heap.hprof -f md-graphs       # Markdown with ASCII graphs
```

Add `--find-duplicates` or `--collections` to enable the opt-in sections.

**Progress** on long runs is printed to stderr when it is a terminal; control
with `--progress auto|always|never`.

### Flag reference

| Flag | Description |
|------|-------------|
| `--find-duplicates` | Detect content-identical `String` values and primitive arrays; reports wasted bytes and top offenders. Hashes to 64 bits — does not retain raw data. Adds a few extra heap scans. |
| `--collections` | Container attribution by holder `Class#field`: fill ratios, size distributions, collision rates. Adds ~300 MB peak RSS on large dumps. |
| `--obj-graph[=small\|medium\|large]` | Capture the outbound-reference graph for the top retained objects. Enables the interactive Object Graph Explorer in HTML reports. Implied by `--full-analysis`. |
| `--full-analysis` | Shorthand for `--obj-graph --collections --find-duplicates`. Adds ~330 MB peak RSS on large dumps. `--ref-paths` is excluded because it can add 100–500 MB on top; pass it separately when needed. |
| `--field-stats` | Per-class reference-field null/non-null/retained stats for the top-50 classes by instance count. Shows which fields are null-heavy or hold the most retained heap. |
| `--ref-paths` | Capture field-name labels on reference edges (~2 bytes/edge). Enables named field breakdowns in `--field-stats` and richer root paths. |
| `--reachable-only` | Restrict OQL results to GC-reachable objects (Eclipse MAT parity). Off by default so the report stays byte-stable. |
| `--detail minimal\|default\|max` | Adjust output-size caps (leak suspects, dominator subtree, alloc sites, thread locals, top consumers). See the [detail preset table](#tune-the-report-size-with---detail) below. |
| `--mat <DIR>` | Emit Eclipse MAT-compatible binary index files into `DIR` alongside the normal analysis. See [Speeding up Eclipse MAT](#speeding-up-eclipse-mat). |
| `--query <OQL>` | Run an OQL query and embed results in the report. May be repeated. See [OQL queries](#oql-queries-query-subcommand). |
| `--query-file <PATH>` | Read one OQL query per non-empty line from a file. |
| `--progress auto\|always\|never` | Control the live progress line on stderr. Default: `auto` (terminal only). |

### Tune the report size with `--detail`

| `--detail`  | root depth | alloc top | thread locals | dom nodes | dom depth | leak children | top consumers |
| ----------- | ---------: | --------: | ------------: | --------: | --------: | ------------: | ------------: |
| `minimal`   |         10 |        15 |             5 |       500 |        10 |            15 |            10 |
| `default`   |         30 |        50 |            20 |     5,000 |        20 |            50 |            20 |
| `max`       |        200 |       500 |           100 |   100,000 |        50 |           500 |           100 |

`--detail max` raises the dominator-tree cap to 100k nodes and pushes peak RSS
higher on very large dumps.

### Compare against a MAT export

```sh
hprof-analyzer heap.hprof report.json
hprof-analyzer compare mat mat_System_Overview.zip report.json
```

### Track growth across two dumps

```sh
hprof-analyzer early.hprof a.json
hprof-analyzer later.hprof b.json
hprof-analyzer compare reports a.json b.json
```

### Re-render a saved report

```sh
hprof-analyzer report.json                    # Markdown to stdout
hprof-analyzer report.json report.html        # HTML
hprof-analyzer report.json -f md-graphs       # Markdown with ASCII graphs
hprof-analyzer report.json.gz -f md-graphs    # reads .gz transparently
```

### JSON schema

The JSON report format is described by [`docs/schema.json`](docs/schema.json)
(JSON Schema draft-2020-12). To regenerate it after model changes:

```sh
hprof-analyzer dev emit-schema > schema/report.schema.json
cp schema/report.schema.json docs/schema.json
```

### Shell completions

```sh
hprof-analyzer completions zsh  > ~/.zsh/completions/_hprof-analyzer
hprof-analyzer completions bash > /etc/bash_completion.d/hprof-analyzer
```

### OQL queries (`query` subcommand)

Run SQL-flavoured queries against a heap dump without building a full report.
The `query` subcommand does a fast streaming parse and answers queries directly.

```sh
# Count all String instances
hprof-analyzer query heap.hprof --query "SELECT COUNT(*) FROM java.lang.String"

# Top 10 threads by shallow size
hprof-analyzer query heap.hprof \
    --query "SELECT @displayName, @usedHeapSize FROM java.lang.Thread ORDER BY @usedHeapSize DESC LIMIT 10"

# Multiple queries in one pass
hprof-analyzer query heap.hprof \
    --query "SELECT COUNT(*) FROM java.lang.String" \
    --query "SELECT COUNT(*) FROM java.lang.Thread"

# Interactive REPL with tab-completion
hprof-analyzer query heap.hprof --repl
```

Retained sizes (`@retainedHeapSize`), dominators, and reference-graph
attributes (`@inbounds`, `@outbounds`) require the **full analysis pipeline**
and are not available in the `query` subcommand. Pass `--query` /
`--query-file` to the main command instead (see below), or use the `server`
subcommand which unlocks retained-size queries after `POST /analyze`.

To embed queries in the full report, pass `--query` / `--query-file` to the
main analysis command:

```sh
hprof-analyzer heap.hprof report.html \
    --query "SELECT @displayName, @retainedHeapSize FROM java.lang.Thread ORDER BY @retainedHeapSize DESC LIMIT 20"
```

The full OQL language reference — grammar, attributes, aggregates, visualization
directives, worked examples — is in [docs/OQL.md](docs/OQL.md).

#### Visualization directives (`-- @viz`)

Prefix any query with a `-- @viz` comment to request a chart in the report:

```sh
hprof-analyzer heap.hprof report.html --query="-- @viz histogram label=@displayName value=@retainedHeapSize cap=10
SELECT @displayName, @retainedHeapSize FROM java.lang.Thread ORDER BY @retainedHeapSize DESC"
```

Kinds: `table` (default), `histogram`, `piechart`, `treemap`. HTML renders
interactive charts; Markdown renders ASCII bars. The directive has no effect in
the `query` subcommand — it applies only to embedded queries in reports.

Use `--query=` (with `=`) to avoid `clap` misinterpreting the leading `--`
in the directive as a flag.

#### Compatibility with Eclipse MAT OQL

`hprof-analyzer`'s OQL is modelled on Eclipse MAT's dialect and is largely
compatible. Key differences:

**Extensions (not in MAT):** `MEDIAN`/`PERCENTILE` aggregates, `GROUP BY` /
`HAVING`, `path()` reachability, `-- @viz` directives, arithmetic in `SELECT`
and `WHERE`, system-properties snapshot (`@systemProperties`), interactive REPL
with tab-completion, named queries library (`!run <name>`), report embedding
(`--query` / `--query-file`).

**Behavioural differences:** unreachable objects are *included* (MAT discards
them); `s.count`/`s.offset` are absent (modern JDK layout — use `s.value`,
`s.coder`); integer division by zero returns `NULL` instead of throwing;
`toString()` on non-String returns `NULL` (no live JVM reflection); `get(n)`
array/collection access and `COUNT(*) FROM (subquery)` are rejected.

**Not yet supported:** `FROM OBJECTS <decimal-id>` (hex works), array indexing
(`s[0]`), `${snapshot}.getClasses()`. Some object-ref field navigations (where
the declared type is `Object`) silently return `NULL` — see [docs/OQL.md §
Eclipse MAT OQL compatibility](docs/OQL.md#eclipse-mat-oql-compatibility) for
details and workarounds.

### HTTP server (`server` subcommand)

`server` starts a lightweight HTTP API on `127.0.0.1` (loopback only) that
exposes OQL queries and all four report sections as JSON or Markdown endpoints.
Designed for use with LLM tooling, `curl` pipelines, and CI scripts that need
more than a static report.

```sh
hprof-analyzer server heap.hprof           # default port 7070
hprof-analyzer server heap.hprof --port 8080
```

The server prints a startup banner listing all available endpoints.

**Key endpoints:**

| Method | Path | Description |
|--------|------|-------------|
| GET | `/status` | `{"status":"ready"\|"analyzing"\|"not_started"}` |
| POST | `/analyze` | Trigger full analysis (retained sizes, dominators) |
| POST | `/` | Run OQL query → JSON |
| POST | `/stream` | Run OQL query → NDJSON (streaming) |
| GET | `/report` | Full report JSON (or `?format=md`) |
| GET | `/report/overview` | System overview section |
| GET | `/report/leaks` | Leak suspects section |
| GET | `/report/top` | Top consumers section |
| GET | `/report/threads` | Thread overview section |

**Lazy analysis:** The first `GET /report/…` triggers analysis automatically.
Report endpoints return `202 Accepted` while analysis is running; poll
`GET /status` until `"ready"`.

**Retained-size queries:** At startup the server runs a fast query-only parse.
`@retainedHeapSize` and dominator attributes are available only after the full
analysis completes (via `POST /analyze` or an implicit trigger).

```sh
# Start server, trigger analysis, wait, then query
hprof-analyzer server heap.hprof &
curl -s -X POST http://127.0.0.1:7070/analyze
until curl -sf http://127.0.0.1:7070/status | grep -q '"ready"'; do sleep 1; done

# Report sections
curl -s 'http://127.0.0.1:7070/report/leaks?limit=5' | jq .
curl -s 'http://127.0.0.1:7070/report/overview?format=md'

# OQL query
curl -s http://127.0.0.1:7070/ -d 'SELECT COUNT(*) FROM java.lang.String'
```

See [docs/OQL.md — server subcommand](docs/OQL.md#server-subcommand) for the
full endpoint reference, body format, `?limit=N`, NDJSON streaming, and error
response shapes.

## Use with AI agents

`hprof-analyzer` works well as a tool for LLM agents. Start the server on your
dump, then point an agent at it:

```sh
hprof-analyzer server heap.hprof   # starts on http://127.0.0.1:7070
```

A ready-made **Claude Code skill** is included at
[`skills/hprof-analyzer.md`](skills/hprof-analyzer.md). Load it in Claude Code:

```
@skills/hprof-analyzer.md
"Connect to http://127.0.0.1:7070 and identify the top memory consumers"
```

The skill teaches Claude the server API, OQL syntax, and common heap-triage
workflows. The OQL guide at
[parttimenerd.github.io/hprof-analyzer/oql/](https://parttimenerd.github.io/hprof-analyzer/oql/)
covers grammar, examples, and the full attribute reference.

## Performance

Measured on an AMD Ryzen Threadripper PRO 3995WX (64 cores / 128 threads) with
123 GiB RAM, Linux, commit [`55eaca7`](https://github.com/parttimenerd/hprof-analyzer/commit/55eaca7),
2026-08-03. "Basic" = default analysis; "Full" = `--full-analysis`
(`--obj-graph --collections --find-duplicates`). MAT = Eclipse MAT 1.17.0
(`ParseHeapDump.sh -Xmx80g`). Wall-clock in `m:ss` or `s`. RSS is peak.

| Workload | Dump file | Wall basic | RSS basic | Wall full | RSS full | Wall (MAT) | RSS (MAT) |
|----------|-----------|------------|-----------|-----------|----------|------------|-----------|
| Renaissance scala-doku | 51 MiB | 2.4 s | 40 MiB | 5.7 s | 373 MiB | 0:04 | 1.63 GiB |
| gauss-mix | 70 MiB | 2.1 s | 44 MiB | 3.3 s | 195 MiB | 0:04 | 1.61 GiB |
| naive-bayes (1.3 GiB) | 1.3 GiB | 5.0 s | 67 MiB | 8.2 s | 382 MiB | 0:05 | 1.73 GiB |
| VS Code JVM (1.1 GiB) | 1.1 GiB | 0:43 | 216 MiB | 1:13 | 2.51 GiB | 0:28 | 2.39 GiB |
| HeapothesYs 16g | 11 GiB | 2:49 | 565 MiB | 4:07 | 6.32 GiB | 1:06 | 4.99 GiB |
| HeapothesYs 28g | 20 GiB | 5:31 | 1.05 GiB | 8:00 | 12.1 GiB | 2:10 | 5.27 GiB |
| Real-world 34g | 34 GiB | 22:26 | 7.78 GiB | — (OOM) | — | 2:19:17 | 5.18 GiB |

The 34g `--full-analysis` run OOM'd (12.1 GiB peak at 28g → ~18 GiB needed for 34g exceeds available headroom).
Basic analysis on 34g peaks at 7.78 GiB.

MAT was run with `ParseHeapDump.sh -Xmx80g` (leak-suspects + top-components).
MAT requires a JVM heap large enough to hold its in-memory index, so the RSS
reported here reflects a generously provisioned run. `hprof-analyzer` holds peak
RSS well below the dump size and needs no heap tuning. Correctness is validated
against MAT 1.17.0: the `compare mat` subcommand diffs a MAT System Overview
export against our JSON, and the parity fixtures gate on it (see
[Compare against a MAT export](#compare-against-a-mat-export)).


## How it works

The two-pass parser, the dominator-tree construction, the shallow/retained size
formulas, and the compressed index structures are described in
[DESIGN.md](DESIGN.md).

## Contributing

Contributions are welcome. See [DESIGN.md](DESIGN.md) for architecture context
(two-pass parser, dominator-tree construction, size formulas, index structures).

Requires a stable Rust toolchain (1.85+); see [Install](#install). All commands
from the repository root:

```sh
cargo build --release        # binary at target/release/hprof-analyzer
cargo test --release         # unit tests + JSON-schema + report parity fixtures
cargo fmt --all -- --check   # formatting gate (matches CI)
cargo clippy --release --all-targets -- -D warnings   # lint gate (matches CI)
```

CI runs the same `fmt`, `clippy`, and `test` steps on stable. Parity fixtures
live under `tests/fixtures/`.

The HTML report embeds a pre-committed React bundle (`web/dist/bundle.js`), so
Node.js is not needed for normal builds. To rebuild it after changing
`web/src/`: `cd web && npm install && npm run build`.

The self-contained browser bundle is assembled from the WASM module and the
React bundle by `web-browser/assemble.py`:

```sh
# Build WASM module first
wasm-pack build crates/hprof-wasm --target web --release

# Assemble the browser bundle
python3 web-browser/assemble.py           # → dist/hprof-analyzer-browser.html
python3 web-browser/assemble.py -o /tmp/bundle.html   # custom output path
```

## Support & Feedback

Bug reports, feature requests, and contributions are welcome via
[GitHub issues](https://github.com/parttimenerd/hprof-analyzer/issues).

## License

MIT. See [LICENSE](LICENSE).

Copyright 2026 SAP SE or an SAP affiliate company, Johannes Bechberger and
contributors.
