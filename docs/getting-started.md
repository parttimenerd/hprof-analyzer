# Getting Started

## Installation

Build from source (requires Rust ≥ 1.80 and Node.js ≥ 18):

```bash
git clone https://github.com/your-org/hprof-analyzer
cd hprof-analyzer
npm --prefix web ci
node web/esbuild.config.mjs
cargo build --release -p hprof-analyzer
```

The binary is at `target/release/hprof-analyzer`. Copy it anywhere on your `$PATH`.

## Taking a heap dump

From a running JVM:

```bash
# jmap (any JDK)
jmap -dump:live,format=b,file=/tmp/app.hprof <pid>

# jcmd (JDK 9+)
jcmd <pid> GC.heap_dump /tmp/app.hprof

# Via JVM flag at startup
java -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=/tmp/dumps ...
```

The `:live` qualifier triggers a GC before the dump — use it to exclude unreachable objects.

## Your first report

```bash
hprof-analyzer app.hprof report.html
```

Open `report.html` in any browser. No server required — the file is fully self-contained.

The tool accepts `.hprof`, `.hprof.gz`, `.hprof.zip`, `.hprof.tar.gz`, `.tar.gz`, and `.tgz` directly — no manual decompression needed.

**Truncated dumps work.** If the JVM was killed mid-dump or the file was copied incompletely, the analyzer recovers whatever was written and produces a partial report. A warning is printed to stderr; the report covers the objects that were successfully parsed.

## What the report shows

| Section | What it answers |
|---------|----------------|
| System Overview | Heap size, GC roots, dump age, compressed OOPs |
| Leak Suspects | Objects with unexpectedly high retained heap; path to GC root |
| Top Consumers | By class, by package (zoomable treemap), biggest individual objects |
| Threads | Live threads and their local references |
| Collections | Fill ratios, collision rates, wasted capacity |
| Duplicate Strings | Content-identical `String` objects (opt-in) |
| OQL Results | Results of embedded queries (opt-in) |

## Common workflows

### Find the leak suspect

```bash
hprof-analyzer app.hprof report.html
```

Open **Leak Suspects** in the report. The top entry usually names the class and shows its path to a GC root.

### Drill into the biggest objects

```bash
hprof-analyzer app.hprof report.html --obj-graph
```

Open **Top Consumers → Biggest Objects**. Click any row to open the Object Graph Explorer — outbound refs, inbound refs, dominator chain, and path to GC root. See [Object Graph Explorer](obj-graph.md) for details.

### Ad-hoc query

```bash
hprof-analyzer query app.hprof --query 'SELECT * FROM java.lang.Thread'
```

### Interactive REPL

```bash
hprof-analyzer query app.hprof --repl
```

Starts a readline-equipped shell with tab completion, result history, and inline chart directives. See [OQL](OQL.md).

### Compare two snapshots for memory growth

```bash
hprof-analyzer app1.json app2.json report.html  # re-renders; or:
hprof-analyzer compare reports r1.json r2.json
```

### Everything at once

```bash
hprof-analyzer app.hprof report.html --full-analysis
```

Equivalent to `--obj-graph --collections --find-duplicates`. Adds ~330 MB peak RSS.

## Output formats

| Extension / flag | Format |
|-----------------|--------|
| `.html` / `-f html` | Self-contained HTML (default for most users) |
| `.json` / `-f json` | Canonical machine-readable JSON |
| `.md` / `-f md` | Plain Markdown |
| `-f md-graphs` | Markdown with ASCII charts |

## Shell completions

```bash
# zsh
hprof-analyzer completions zsh > "${fpath[1]}/_hprof-analyzer"

# bash
hprof-analyzer completions bash >> ~/.bash_completion

# fish
hprof-analyzer completions fish > ~/.config/fish/completions/hprof-analyzer.fish
```
