# hprof-analyzer

[![CI](https://github.com/parttimenerd/hprof-analyzer/actions/workflows/ci.yml/badge.svg)](https://github.com/parttimenerd/hprof-analyzer/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hprof-analyzer.svg)](https://crates.io/crates/hprof-analyzer)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Your JVM died with an `OutOfMemoryError` and left behind a multi-gigabyte
`.hprof` heap dump. You want to know **what filled the heap** without opening a
34 GB file in a GUI or provisioning a machine as big as the dump.

`hprof-analyzer` is a command-line tool that reads the dump and writes a
self-contained report covering the same ground as [Eclipse MAT](https://eclipse.dev/mat/)'s System
Overview, Leak Suspects, and Top Consumers analyses, plus additional views. Peak
RSS stays well below the dump size: on a 33 GiB dump it peaks at ~15 GiB where
MAT needs ~62 GiB (see [Performance](#performance)). The report is a single file you can email, attach to a
ticket, or diff in CI.

*An experimental tool by the [SapMachine](https://sapmachine.io) team.*

## What you get

Run one command and get a report with these sections:

- **System Overview**: tells you how large the heap is, which classes and class
  loaders dominate it, how many duplicate class definitions exist, and which GC
  roots are anchoring memory. Includes a per-class histogram with a
  "largest single instance" column so you can spot the one object inflating a
  count.
- **Leak Suspects**: identifies the objects retaining the most memory and traces
  each one back to its GC root, so you can see the exact reference chain keeping
  it alive — the same root-path view MAT shows in its Leak Suspects report.
- **Top Consumers**: ranks classes, class loaders, and packages by *retained*
  size (not just shallow size), so allocations hidden inside containers show up
  under the right owner.
- **Threads**: maps each thread's stack frames to the local variables they keep
  alive, so thread-local accumulation is visible without a GUI.
- **Duplicate strings** (opt-in, `--find-duplicates`): quantifies wasted memory
  from identical `java.lang.String` values — how many bytes are duplicated, the
  top 25 offending values, a string-length histogram, and the top 25 classes
  holding the most String references.
- **Collections analysis** (opt-in, `--collections`): deep inspection of every
  Map, List, Set, and array in the heap. Shows fill ratios (how full backing
  arrays are), size distributions, map collision rates, the biggest individual
  collections, constant primitive arrays that could be deduplicated, and
  container attribution by `Class#field` — which field on which class holds the
  most elements and retains the most memory across all live instances. Supports
  standard JDK collections, Kotlin collections, and Eclipse Collections
  out of the box; custom types can be registered via a TOML config file.

Pick the format that fits: plain **Markdown**, **Markdown with ASCII graphs**
(bars, sparklines, dominator trees), a self-contained **HTML** page you can open
in any browser, or machine-readable **JSON**.

A live viewer shows all four output formats side by side, built from the public
[Renaissance benchmark](https://renaissance.dev/) `scala-doku` dump:

**➡ [Open the sample report viewer](https://parttimenerd.github.io/hprof-analyzer/)**

In the HTML report, long tables are capped to their top 20 rows with a "Show N
more" control to expand a single table; a "Expand all tables" toggle at the top
of the page unfolds every capped table at once.

Switch formats in the top-left; the *Default* / *All features* toggle beside
them swaps between a default-options run and one with every optional analysis on
(`--find-duplicates --collections`). On either Markdown view, hit *Render to HTML*
in the top-right to see it formatted. The raw files are here:

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
onto your `PATH` (e.g. `sudo mv hprof-analyzer-*/hprof-analyzer /usr/local/bin/`),
then just `hprof-analyzer heap.hprof report.html`. The rest of this README
assumes it is on your `PATH`.

There is no subcommand to remember. Hand the tool a `.hprof` dump and it
analyzes it; hand it a saved report JSON and it re-renders it to any format
without re-parsing the dump. With an output path, the format is inferred from
the extension; without one, it prints Markdown to stdout.

```sh
hprof-analyzer heap.hprof report.html    # write an HTML report
hprof-analyzer heap.hprof                 # or Markdown to stdout
```

Analysis time scales with the dump. A small dump is done in seconds; a
multi-gigabyte dump takes minutes (see [Performance](#performance)).
Gzip-compressed dumps (`.hprof.gz`) are read transparently.

## Why you might want it

- **Very large dumps at bounded memory.** Two-pass streaming keeps peak RSS far
  below the dump size. A **33.4 GiB** dump (**~7.5 GiB** gzip-compressed,
  **~20 GiB** live heap) analyzes in **~13.5 minutes at ~14.7 GiB peak RSS**,
  where MAT needs ~62 GiB (see [Performance](#performance)). No heap-size flag
  to tune.
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
run OQL queries, walk the dominator tree interactively, inspect arbitrary
objects and their fields, or use the full breadth of MAT's analyses — reach for
**[Eclipse MAT](https://eclipse.dev/mat/)**, the complete interactive GUI. Use
`hprof-analyzer` instead when you already know you want those reports and want
them fast, scriptable, or on a dump too large to open comfortably.

If all you need is a class histogram,
[`hprof-slurp`](https://github.com/agourlay/hprof-slurp) is faster and lighter
because it never builds the dominator tree. But that also means it cannot report
retained sizes, leak suspects, root paths, or Top Consumers — the analyses
`hprof-analyzer` exists to provide.

## Install

Three ways to install, fastest first.

### 1. Prebuilt binary (recommended, no toolchain)

Download the archive for your platform, unpack it, and put the `hprof-analyzer`
binary on your `PATH`. The archives bundle the HTML report's assets already, so
you need **no Rust and no Node.js**.

| Platform | Archive |
| --- | --- |
| Linux x86_64 (glibc) | [`hprof-analyzer-x86_64-unknown-linux-gnu.tar.gz`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-unknown-linux-gnu.tar.gz) |
| Linux x86_64 (static musl) | [`hprof-analyzer-x86_64-unknown-linux-musl.tar.gz`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-unknown-linux-musl.tar.gz) |
| Linux aarch64 (glibc) | [`hprof-analyzer-aarch64-unknown-linux-gnu.tar.gz`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-aarch64-unknown-linux-gnu.tar.gz) |
| Linux aarch64 (static musl) | [`hprof-analyzer-aarch64-unknown-linux-musl.tar.gz`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-aarch64-unknown-linux-musl.tar.gz) |
| macOS (Apple Silicon) | [`hprof-analyzer-aarch64-apple-darwin.tar.gz`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-aarch64-apple-darwin.tar.gz) |
| Windows x86_64 | [`hprof-analyzer-x86_64-pc-windows-msvc.zip`](https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-pc-windows-msvc.zip) |

A rolling [`nightly`](https://github.com/parttimenerd/hprof-analyzer/releases/tag/nightly)
pre-release always tracks the latest commit on `main`. Download it in one line
(swap in the archive for your platform):

```sh
curl -L https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv hprof-analyzer-*/hprof-analyzer /usr/local/bin/
hprof-analyzer --help
```

Once a versioned release is tagged, the same archives are also published there
and reachable via the stable `.../releases/latest/download/<archive>` URL; until
then, use the `nightly` URL above. You can also browse every asset on the
[Releases page](https://github.com/parttimenerd/hprof-analyzer/releases).

The static **musl** build has no libc dependency, so it runs on any Linux
(including minimal containers and older distros). Prefer it if the glibc build
complains about a missing or too-old `libc`.

### 2. With Cargo

Needs a **Rust toolchain (1.85+, edition 2024)**. Install
[rustup](https://rustup.rs/) and run `rustup update stable` if your toolchain is
older, then:

```sh
cargo install hprof-analyzer
```

This installs `hprof-analyzer` into `~/.cargo/bin` (ensure it is on your
`PATH`). Node.js/npm is not required.

### 3. From source

Same Rust toolchain requirement (1.85+). Node.js/npm only needed if you change
the web sources under `web/src/`:

```sh
git clone https://github.com/parttimenerd/hprof-analyzer
cd hprof-analyzer
cargo build --release
# binary at target/release/hprof-analyzer
```

## Usage

```
hprof-analyzer <INPUT> [OUTPUT] [OPTIONS]

Give it a path and it does the right thing:

  <INPUT>   a .hprof[.gz] heap dump  → analyze it and write a report
            a saved report .json[.gz] → re-render it to another format

Named subcommands:
  compare      Compare reports (MAT export vs ours, or two of ours across time)
  completions  Generate a shell completion script
  dev          Developer / diagnostic commands
```

### Analyze a dump

Output goes to stdout, or to a file if you pass one. When you give an output
file and no `-f`, the format is inferred from its extension (`.html` → HTML,
`.json` / `.json.gz` → JSON, `.md` → Markdown); `-f` always wins. Stdout
defaults to plain Markdown.

```sh
hprof-analyzer heap.hprof                    # plain Markdown to stdout
hprof-analyzer heap.hprof report.html        # HTML (inferred from .html)
hprof-analyzer heap.hprof report.json        # JSON (inferred from .json)
hprof-analyzer heap.hprof report.json.gz     # gzip-compressed JSON
hprof-analyzer heap.hprof -f md-graphs       # Markdown with ASCII graphs
```

`md-graphs` shares the `.md` extension with plain Markdown, so it is never
inferred; ask for it explicitly with `-f md-graphs`.

**Duplicate strings.** Add `--find-duplicates` to include the duplicate-`String`
section (see [What you get](#what-you-get)). It adds two extra scans of the heap
file, so it is off by default:

```sh
hprof-analyzer heap.hprof report.html --find-duplicates
```

**Progress.** Long runs on multi-GB dumps print a live phase line to stderr when
stderr is a terminal. Control it with `--progress auto|always|never` (default
`auto`; `auto` stays silent when stderr is piped or when `--verbose` /
`--trace-rss` are already printing phases).

### Tune the report size with `--detail`

Every report includes four deeper analyses, always on:

- **Root paths**: dominator chain from each single-object suspect up to its GC
  root (MAT-style).
- **Allocation sites**: objects aggregated by allocation stack-trace serial.
- **Thread locals**: each thread's local root objects.
- **Dominator subtree**: the multi-level dominator subtree per accumulation
  point.

One flag scales the output-size caps for these sections (and the top-consumer /
leak-suspect lists):

```sh
hprof-analyzer heap.hprof --detail minimal   # smaller report, tighter caps
hprof-analyzer heap.hprof --detail default   # the default
hprof-analyzer heap.hprof --detail max       # larger report, looser caps
```

The preset controls seven caps:

| `--detail`  | root depth | alloc top | thread locals | dom nodes | dom depth | leak children | top consumers |
| ----------- | ---------: | --------: | ------------: | --------: | --------: | ------------: | ------------: |
| `minimal`   |         10 |        15 |             5 |       500 |        10 |            15 |            10 |
| `default`   |         30 |        50 |            20 |     5,000 |        20 |            50 |            20 |
| `max`       |        200 |       500 |           100 |   100,000 |        50 |           500 |           100 |

Two caveats. **Memory:** `--detail max` can raise the dominator-tree cap to 100k
nodes and push peak RSS higher on very large dumps; that is the documented
tradeoff. **Allocation tracking:** allocation sites only yield real stacks if
the JVM recorded allocation stack traces (`stack_trace_serial`); most HotSpot
dumps have this off, and the report says so rather than inventing data.

### Compare against a MAT export

Compare a MAT System Overview HTML export against our canonical JSON; exits
non-zero on a parity failure (useful as a test gate):

```sh
hprof-analyzer heap.hprof report.json
hprof-analyzer compare mat mat_System_Overview.zip report.json
```

### Track growth across two dumps

Compare an earlier report against a later one to see what grew. This is a handy
way to find a leak by comparing snapshots over time:

```sh
hprof-analyzer early.hprof a.json
hprof-analyzer later.hprof b.json
hprof-analyzer compare reports a.json b.json
```

### Re-render a saved report

The JSON is the canonical form; re-render it to any format without re-parsing
the dump. Just pass the report path as the input, and the tool sees it is a
saved report, not a dump, and re-renders it. It takes an optional output path
with the same extension inference:

```sh
hprof-analyzer report.json                    # Markdown to stdout
hprof-analyzer report.json report.html        # HTML (inferred from .html)
hprof-analyzer report.json -f md-graphs       # Markdown with ASCII graphs
```

The analyze-only flags (`--find-duplicates`, `--collections`, non-default
`--detail`) have no effect when re-rendering, because those sections are baked
into the JSON at analyze time, so passing one on a report input is an error with
a hint to re-run on the `.hprof` dump.

### Compressed JSON

Write the canonical report gzip-compressed by giving the output path a `.gz`
suffix (the JSON is repetitive and typically shrinks ~20×). A `.gz` report reads
back transparently, because the tool sniffs the gzip magic bytes, so it also
works from stdin:

```sh
hprof-analyzer heap.hprof report.json.gz    # gzip-compressed JSON (inferred)
hprof-analyzer report.json.gz -f md-graphs  # read it back, no manual gunzip
```

### Shell completions

Generate a completion script for your shell and install it where the shell looks
for completions:

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

MAT was run with `ParseHeapDump.sh` (leak-suspects + top-components) with its
`MemoryAnalyzer.ini` configured for `-Xmx60g`; peak RSS scales with the dump
(62 GiB on the largest). `hprof-analyzer` holds peak RSS far below the dump size
and needs no heap tuning. Correctness is validated against MAT 1.17.0: the
`compare mat` subcommand diffs a MAT System Overview export against our JSON, and
the parity fixtures gate on it (see [Compare against a MAT
export](#compare-against-a-mat-export)).

Peak RSS stays well below both the dump-file size and the live-heap size (~40%
of the uncompressed file on the big run, and a fraction of that on the smaller
ones) because the analyzer never holds the whole dump in memory; it streams the
records in two passes and works over compressed, bounded-size index structures.

## How it works

The two-pass parser, the dominator-tree construction, the shallow/retained size
formulas, and the compressed index structures are described in
[DESIGN.md](DESIGN.md).

## Contributing

Contributions are welcome. See [DESIGN.md](DESIGN.md) for the two-pass parser,
dominator-tree construction, size formulas, and index structures — the context
you need to extend the analyzer.

### Building and testing

Requires a stable Rust toolchain (1.85+, edition 2024); see
[Install](#install). All commands run from the repository root:

```sh
cargo build --release        # optimized binary at target/release/hprof-analyzer
cargo test --release         # unit tests + JSON-schema + report parity fixtures
cargo fmt --all -- --check   # formatting gate (matches CI)
cargo clippy --release --all-targets -- -D warnings   # lint gate (matches CI)
```

CI (`.github/workflows/ci.yml`) runs the same `fmt`, `clippy -D warnings`, and
`test` steps on `stable`. The parity fixtures the tests read live under
`tests/fixtures/` (checked in alongside the tests).

The self-contained HTML report embeds a small React bundle
(`web/dist/bundle.js`). This bundle is committed to the repository so that
`cargo install` and crates.io builds work without Node.js. When you change
the web sources under `web/src/`, `build.rs` re-bundles automatically on the
next `cargo build` if `npm` is on your `PATH`; you can also rebuild it by hand:

```sh
cd web && npm install && npm run build   # regenerates web/dist/bundle.js
```

If npm is absent, the committed bundle is used as-is. If you only need the
binary and want to avoid the Node toolchain entirely, download a prebuilt
release binary instead (see [Install](#install)).

## Support & Feedback

Bug reports, feature requests, and contributions are welcome via
[GitHub issues](https://github.com/parttimenerd/hprof-analyzer/issues).

## License

MIT. See [LICENSE](LICENSE).

Copyright 2026 SAP SE or an SAP affiliate company, Johannes Bechberger and
contributors.
