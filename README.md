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
  peak RSS well below the heap size. A **35.8 GB** production dump analyzes in
  **~17.5 minutes at ~14 GB peak RSS** (see [Performance](#performance)). MAT typically
  needs a machine with memory comparable to the dump.
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
  analyze       Analyze a heap dump and write a report (Markdown or JSON)
  diff          Compare a MAT report against our canonical JSON (exit 2 on FAIL)
  diff-reports  Cross-dump growth diff: compare two canonical Report JSONs
  render        Render a saved canonical Report JSON to Markdown or JSON
  dev           Developer / diagnostic commands
```

**Analyze a dump.** Pick a format with `-f`; the default is plain Markdown. Output goes
to stdout, or to a file if you pass one:

```sh
hprof-analyzer analyze heap.hprof                      # plain Markdown to stdout
hprof-analyzer analyze heap.hprof -f md-graphs         # Markdown with ASCII graphs
hprof-analyzer analyze heap.hprof -f html report.html  # self-contained HTML page
hprof-analyzer analyze heap.hprof -f json report.json  # machine-readable JSON
```

Gzip-compressed dumps (`.hprof.gz`) are read transparently.

**Diff against a MAT export.** Compare a MAT System Overview HTML export against our
canonical JSON; exits non-zero on a parity failure (useful as a test gate):

```sh
hprof-analyzer analyze heap.hprof -f json > ours.json
hprof-analyzer diff mat_System_Overview.zip ours.json
```

**Track growth across two dumps.** Diff an earlier report against a later one to see what
grew — handy for finding a leak by comparing snapshots over time:

```sh
hprof-analyzer analyze early.hprof -f json > a.json
hprof-analyzer analyze later.hprof -f json > b.json
hprof-analyzer diff-reports a.json b.json
```

**Re-render a saved report.** The JSON is the canonical form; re-render it to any format
without re-parsing the dump:

```sh
hprof-analyzer render report.json -f md-graphs
```

## Sample reports

Generated from the public
[Renaissance benchmark](https://renaissance.dev/) `scala-doku` dump, state of
2026-07-14:

- [`scala-doku.md`](docs/samples/scala-doku.md) — plain Markdown
- [`scala-doku.graphs.md`](docs/samples/scala-doku.graphs.md) — Markdown with ASCII graphs
- [`scala-doku.html`](docs/samples/scala-doku.html) — self-contained HTML (open in a browser)

## Performance

Measured on a real production heap dump. The dump itself is not included (it is
confidential); only the resource numbers are reported here.

| Dump size | Peak RSS | Wall clock | CPU (user + sys) | Machine | Measured |
|-----------|----------|------------|------------------|---------|----------|
| 35.8 GB (33.4 GiB) | 14.07 GiB (14,757,272 KB) | 17 min 33.66 s | 987.45 s + 65.34 s = 1053 s | AMD Ryzen Threadripper PRO 3995WX (64c/128t), 123 GB RAM, Ubuntu 25.10 | 2026-07-13, commit `a1c0bbb` |

Peak RSS stays at roughly 40% of the dump size because the analyzer never holds the whole
dump in memory — it streams the records in two passes and works over compressed,
bounded-size index structures.

## How it works

The two-pass parser, the dominator-tree construction, the shallow/retained size formulas,
and the compressed index structures are described in [DESIGN.md](DESIGN.md).

## License

MIT — see [LICENSE](LICENSE).
