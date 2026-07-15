# hprof-analyzer

[![CI](https://github.com/parttimenerd/hprof-analyzer/actions/workflows/ci.yml/badge.svg)](https://github.com/parttimenerd/hprof-analyzer/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A fast, low-memory command-line analyzer for Java **HPROF** heap dumps. It parses a
dump and produces static reports that replicate three of the
[Eclipse Memory Analyzer (MAT)](https://eclipse.dev/mat/) views:

- **System Overview** — heap size, object/class counts, biggest consumers by class and
  class loader, duplicate classes, GC roots.
- **Leak Suspects** — the dominator-tree suspects that retain the most memory, with the
  accumulation points and the objects they keep alive.
- **Top Consumers** — the largest objects, classes, and class loaders by retained size.

It also emits a **Threads** overview (stacks, thread objects, and the local variables
they root). Reports come in four formats: plain **Markdown**, **Markdown with ASCII
graphs** (bars, sparklines, dominator trees), a self-contained **HTML** page, and
machine-readable **JSON**.

## Why you might want it

- **Very large dumps at bounded memory.** It streams the dump in two passes and keeps
  peak RSS well below the heap size. A large dump with a **20 GB live Java heap** —
  **35.8 GB (33.4 GiB)** as an uncompressed `.hprof` file, or **~8 GB gzip-compressed** —
  analyzes in **~17.5 minutes at ~14 GB peak RSS** (see [Performance](#performance)). MAT
  typically needs a machine with memory comparable to the dump.
- **Scriptable / CI-friendly.** It is fully non-interactive. Emit JSON and diff two dumps
  to catch memory growth in a pipeline, or gate a build on retained-size regressions.
- **Emailable output.** The HTML report is a single self-contained file — no server, no
  external assets — so you can attach it to a ticket or share it directly.
- **Deterministic.** The Markdown output is byte-stable (modulo the generation
  timestamp), so it diffs cleanly across runs and across dumps.

## Why you might *not* want it — use MAT instead

This tool is **deliberately narrow**. It renders static replicas of the three views named
above plus threads, and nothing else. If you need to *explore* a heap — run OQL queries,
walk the dominator tree interactively, inspect arbitrary objects and their fields, follow
inbound/outbound references by hand, or run the full breadth of MAT's analyses — use
**[Eclipse MAT](https://eclipse.dev/mat/)**, which is a complete interactive GUI. This
analyzer is for the common case where you already know you want those three reports,
want them quickly, on a dump too large to open comfortably, or from a script.

## Install

**Release binary.** Download the archive for your platform from the
[Releases page](https://github.com/parttimenerd/hprof-analyzer/releases), unpack it, and
put `hprof-analyzer` on your `PATH`. Prebuilt targets: Linux x86_64 (glibc and static
musl), macOS aarch64, Windows x86_64.

**With Cargo** (needs a recent Rust toolchain, edition 2024):

```sh
cargo install --path .
```

**From source:**

```sh
git clone https://github.com/parttimenerd/hprof-analyzer
cd hprof-analyzer
cargo build --release
# binary at target/release/hprof-analyzer
```

## Usage

```
hprof-analyzer <COMMAND>

Commands:
  analyze      Analyze a heap dump and write a report
  render       Re-render a saved canonical Report JSON to another format
  compare      Compare reports (MAT export vs ours, or two of ours across time)
  completions  Generate a shell completion script
  dev          Developer / diagnostic commands
```

**Analyze a dump.** Output goes to stdout, or to a file if you pass one. When you give an
output file and no `-f`, the format is inferred from its extension (`.html`→HTML,
`.json`/`.json.gz`→JSON, `.md`→Markdown); `-f` always overrides. Stdout defaults to plain
Markdown:

```sh
hprof-analyzer analyze heap.hprof                    # plain Markdown to stdout
hprof-analyzer analyze heap.hprof report.html        # HTML (inferred from .html)
hprof-analyzer analyze heap.hprof report.json        # JSON (inferred from .json)
hprof-analyzer analyze heap.hprof report.json.gz     # gzip-compressed JSON
hprof-analyzer analyze heap.hprof -f md-graphs       # Markdown with ASCII graphs
```

`md-graphs` shares the `.md` extension with plain Markdown, so it is never inferred — ask
for it explicitly with `-f md-graphs`.

Gzip-compressed dumps (`.hprof.gz`) are read transparently.

**Progress.** Long runs on multi-GB dumps print a live phase line to stderr when stderr is
a terminal. Control it with `--progress auto|always|never` (default `auto`; `auto` stays
silent when stderr is piped or when `--verbose`/`--trace-rss` are already printing phases).

**Heavy analyses (always on).** Four deeper analyses run unconditionally and add a section
to every format (Markdown, md-graphs, HTML, JSON):

- **Root paths** — dominator chain from each single-object suspect up to its GC root (MAT-style).
- **Allocation sites** — objects aggregated by allocation stack-trace serial.
- **Thread locals** — each thread's local root objects.
- **Dominator subtree** — full multi-level dominator subtree per accumulation point.

**`--detail` preset.** A single flag scales the output-size caps for these sections (plus the
top-consumer / leak-suspect lists):

```sh
hprof-analyzer analyze heap.hprof --detail minimal   # smaller report, tighter caps
hprof-analyzer analyze heap.hprof --detail default   # the default; historical cap values
hprof-analyzer analyze heap.hprof --detail max        # larger report, looser caps
```

The preset controls seven caps — root-path max depth, alloc-sites top-N, thread-locals per
thread, dominator-tree max nodes / max depth, leak-children cap, and top-consumers count:

| `--detail`  | root depth | alloc top | thread locals | dom nodes | dom depth | leak children | top consumers |
| ----------- | ---------: | --------: | ------------: | --------: | --------: | ------------: | ------------: |
| `minimal`   |         10 |        15 |             5 |       500 |        10 |            15 |            10 |
| `default`   |         30 |        50 |            20 |     5,000 |        20 |            50 |            20 |
| `max`       |        200 |       500 |           100 |   100,000 |        50 |           500 |           100 |

Two caveats. **Memory:** `--detail max` can raise the dominator-tree cap to 100k nodes and
push peak RSS higher on very large dumps — that is the documented tradeoff. **Allocation
tracking:** allocation sites only yield real stacks if the JVM recorded allocation stack
traces (`stack_trace_serial`); most HotSpot dumps have this off, and the report says so
honestly rather than inventing data.

**Compare against a MAT export.** Compare a MAT System Overview HTML export against our
canonical JSON; exits non-zero on a parity failure (useful as a test gate):

```sh
hprof-analyzer analyze heap.hprof report.json
hprof-analyzer compare mat mat_System_Overview.zip report.json
```

**Track growth across two dumps.** Compare an earlier report against a later one to see what
grew — handy for finding a leak by comparing snapshots over time:

```sh
hprof-analyzer analyze early.hprof a.json
hprof-analyzer analyze later.hprof b.json
hprof-analyzer compare reports a.json b.json
```

**Re-render a saved report.** The JSON is the canonical form; re-render it to any format
without re-parsing the dump. `render` also takes an optional output path with the same
extension inference as `analyze`:

```sh
hprof-analyzer render report.json                    # Markdown to stdout
hprof-analyzer render report.json report.html        # HTML (inferred from .html)
hprof-analyzer render report.json -f md-graphs       # Markdown with ASCII graphs
```

**Shell completions.** Generate a completion script for your shell and install it where the
shell looks for completions:

```sh
hprof-analyzer completions zsh  > ~/.zsh/completions/_hprof-analyzer
hprof-analyzer completions bash > /etc/bash_completion.d/hprof-analyzer
```

**Compressed JSON.** Write the canonical report gzip-compressed by giving the output path a
`.gz` suffix (the JSON is repetitive and typically shrinks ~20×). `render` reads a `.gz`
report back transparently — it sniffs the gzip magic bytes, so it also works from stdin:

```sh
hprof-analyzer analyze heap.hprof report.json.gz   # gzip-compressed JSON (inferred)
hprof-analyzer render report.json.gz -f md-graphs   # read it back, no manual gunzip
```

## Sample reports

Generated from the public
[Renaissance benchmark](https://renaissance.dev/) `scala-doku` dump, state of
2026-07-14:

- [`scala-doku.md`](docs/samples/scala-doku.md) — plain Markdown
- [`scala-doku.graphs.md`](docs/samples/scala-doku.graphs.md) — Markdown with ASCII graphs
- [`scala-doku.html`](docs/samples/scala-doku.html) — self-contained HTML (open in a browser)

## Performance

Measured on a large real-world heap dump. The dump itself is not included; only
the resource numbers are reported here. The dump holds a **20 GB
live Java heap**; the `.hprof` file is **35.8 GB (33.4 GiB)** uncompressed, or **~8 GB
gzip-compressed**.

| Heap (live) | Dump file | Compressed | Peak RSS | Wall clock | CPU (user + sys) | Machine | Measured |
|-------------|-----------|------------|----------|------------|------------------|---------|----------|
| ~20 GB | 35.8 GB (33.4 GiB) | ~8 GB (.hprof.gz) | 14.07 GiB (14,757,272 KB) | 17 min 33.66 s | 987.45 s + 65.34 s = 1053 s | AMD Ryzen Threadripper PRO 3995WX (64c/128t), 123 GB RAM, Ubuntu 25.10 | 2026-07-13, commit `a1c0bbb` |

Peak RSS stays at roughly 40% of the uncompressed dump size (and below the 20 GB live
heap) because the analyzer never holds the whole dump in memory — it streams the records
in two passes and works over compressed, bounded-size index structures.

## How it works

The two-pass parser, the dominator-tree construction, the shallow/retained size formulas,
and the compressed index structures are described in [DESIGN.md](DESIGN.md).

## License

MIT — see [LICENSE](LICENSE).
