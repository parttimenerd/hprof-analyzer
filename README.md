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
ticket, or diff in CI.

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

**➡ [Open the sample report viewer](https://parttimenerd.github.io/hprof-analyzer/)**

| Format | Default options | All optional features |
|--------|-----------------|-----------------------|
| Plain Markdown | [`scala-doku.md`](docs/samples/scala-doku.md) | [`scala-doku-full.md`](docs/samples/scala-doku-full.md) |
| Markdown with ASCII graphs | [`scala-doku.graphs.md`](docs/samples/scala-doku.graphs.md) | [`scala-doku-full.graphs.md`](docs/samples/scala-doku-full.graphs.md) |
| Self-contained HTML (opens live) | [`scala-doku.html`](https://parttimenerd.github.io/hprof-analyzer/samples/scala-doku.html) | [`scala-doku-full.html`](https://parttimenerd.github.io/hprof-analyzer/samples/scala-doku-full.html) |
| Machine-readable JSON | [`scala-doku.json`](docs/samples/scala-doku.json) | [`scala-doku-full.json`](docs/samples/scala-doku-full.json) |

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

This tool is **deliberately narrow** and **experimental**: it renders static
replicas of the views above plus threads, and nothing else. If you need to
*explore* a heap — run OQL queries, walk the dominator tree interactively,
inspect arbitrary objects and their fields, or use the full breadth of MAT's
analyses — reach for **[Eclipse MAT](https://eclipse.dev/mat/)**, the complete
interactive GUI. Use `hprof-analyzer` instead when you already know you want
those reports and want them fast, scriptable, or on a dump too large to open
comfortably.

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

## Performance

Three representative workloads are measured below: a large real-world dump
(resource numbers shared but the dump itself is not), a
**[HeapothesYs](https://github.com/corretto/heapothesys) HyperAlloc** synthetic
allocation dump (~10 GiB file), and a **VS Code / Eclipse-based JVM** dump
(~1 GiB file). The latter two are reproducible public dumps you can regenerate.
All sizes are in binary units (GiB/MiB), and wall-clock times are
`minutes:seconds`. Each row records the exact commit so the numbers stay
meaningful as the tool evolves.

All rows were measured on an AMD Ryzen Threadripper PRO 3995WX (64 cores /
128 threads) with 123 GiB RAM. The "ours" columns are `hprof-analyzer`; the
"MAT" columns are Eclipse MAT 1.17.0 on the same dump, for comparison.

| Workload | Heap (live) | Dump file | RSS (ours) | RSS (MAT) | Wall (ours) | Wall (MAT) | Measured |
|----------|-------------|-----------|------------|-----------|-------------|------------|----------|
| Large real-world dump | ~20 GiB | 33.4 GiB (~7.5 GiB gzip) | 14.65 GiB | 62.05 GiB | 13:21 | 27:16 | 2026-07-19, [`86006f7`](https://github.com/parttimenerd/hprof-analyzer/commit/86006f7) |
| HeapothesYs HyperAlloc | 7.91 GiB | 10.32 GiB | 0.94 GiB | 20.32 GiB | 1:20 | 1:48 | 2026-07-19, [`86006f7`](https://github.com/parttimenerd/hprof-analyzer/commit/86006f7) |
| VS Code JVM | 0.73 GiB | 1.01 GiB | 0.49 GiB | 5.27 GiB | 0:22 | 1:27 | 2026-07-19, [`86006f7`](https://github.com/parttimenerd/hprof-analyzer/commit/86006f7) |

MAT was run with `ParseHeapDump.sh` (leak-suspects + top-components). Its
`MemoryAnalyzer.ini` was set to `-Xmx60g` to avoid OOM during analysis — MAT
requires a heap large enough to hold its in-memory index, so the RSS reported
here reflects a generously provisioned run, not MAT's minimum. With a tighter
`-Xmx` (e.g. `-Xmx5g` on the HeapothesYs dump) MAT completes successfully at
lower RSS but takes longer due to GC pressure. `hprof-analyzer` holds peak RSS
far below the dump size and needs no heap tuning. Correctness is validated
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

## Support & Feedback

Bug reports, feature requests, and contributions are welcome via
[GitHub issues](https://github.com/parttimenerd/hprof-analyzer/issues).

## License

MIT. See [LICENSE](LICENSE).

Copyright 2026 SAP SE or an SAP affiliate company, Johannes Bechberger and
contributors.
