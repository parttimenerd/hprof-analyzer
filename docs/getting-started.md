# Getting Started

## Installation

### Prebuilt binary (recommended)

No Rust, no Node.js required.

```bash
# macOS (Apple Silicon)
curl -L https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-aarch64-apple-darwin.tar.gz | tar xz
sudo mv hprof-analyzer-*/hprof-analyzer /usr/local/bin/

# Linux (x86_64, glibc)
curl -L https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv hprof-analyzer-*/hprof-analyzer /usr/local/bin/
```

See the [nightly release page](https://github.com/parttimenerd/hprof-analyzer/releases/tag/nightly) for all platforms (Linux aarch64, Linux musl, Windows x86_64).

### Homebrew (macOS / Linux)

```bash
brew tap parttimenerd/hprof-analyzer
brew trust parttimenerd/hprof-analyzer   # required once for third-party taps
brew install hprof-analyzer
```

### Build from source (Rust ≥ 1.85)

```bash
git clone https://github.com/parttimenerd/hprof-analyzer
cd hprof-analyzer
cargo build --release
# binary at target/release/hprof-analyzer
```

## Taking a heap dump

From a running JVM:

```bash
# jmap (any JDK)
jmap -dump:live,format=b,file=/tmp/app.hprof <pid>

# jcmd (JDK 9+)
jcmd <pid> GC.heap_dump /tmp/app.hprof

# Via JVM flag at startup (triggers on OutOfMemoryError)
java -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=/tmp/dumps ...
```

The `:live` qualifier triggers a GC before the dump — use it to exclude unreachable objects.

**Tip:** use `-XX:+HeapDumpGzip` to write compressed dumps directly. Gzip dumps are typically 5–10× smaller, making them faster to copy off the production machine.

## Your first report

```bash
hprof-analyzer app.hprof report.html
```

Open `report.html` in any browser. No server required — the file is fully self-contained.

The tool reads `.hprof`, `.hprof.gz`, `.hprof.zip`, `.hprof.tar.gz`, `.tar.gz`, and `.tgz` directly — no manual decompression needed.

**Truncated dumps work.** If the JVM was killed mid-dump or the file was copied incompletely, the analyzer recovers whatever was written and produces a partial report with a warning on stderr.

## What the report shows

| Section | What it answers |
|---------|----------------|
| System Overview | Heap size, GC roots, class/classloader breakdown |
| Leak Suspects | Objects with unexpectedly high retained heap; path to GC root |
| Top Consumers | By class, by package (zoomable treemap), biggest individual objects |
| Threads | Live threads and their local references |
| Collections | Fill ratios, collision rates, wasted capacity (opt-in: `--collections`) |
| Duplicate Strings | Content-identical `String` objects (opt-in: `--find-duplicates`) |
| OQL Results | Results of embedded queries (opt-in: `--query`) |

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

### Repeated exploration with fast cached access

```bash
# First run: full analysis, writes cache alongside the dump
hprof-analyzer heap summary app.hprof

# All subsequent runs load from cache in ~1 s
hprof-analyzer heap histogram app.hprof
hprof-analyzer heap report app.hprof --section leaks
hprof-analyzer heap query app.hprof --oql "SELECT COUNT(*) FROM java.lang.String"
hprof-analyzer heap browse app.hprof      # dominator tree
```

The cache lives at `<dump>.hprof-cache/` alongside the file.

### Compare two snapshots for memory growth

```bash
hprof-analyzer before.hprof before.json
hprof-analyzer after.hprof after.json
hprof-analyzer compare reports before.json after.json
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

## MCP server (AI-assisted analysis)

```bash
claude mcp add hprof -- hprof-analyzer mcp
```

Once registered, Claude can load dumps, run OQL queries, browse the dominator tree, and redact dumps directly. See the [README MCP section](../README.md#mcp-server--ai-assisted-heap-analysis) for full setup instructions for Claude Desktop and Cline.

## Redacting a dump before sharing

```bash
# Lean mode (default): zeroes all primitive array elements
hprof-analyzer redact app.hprof safe.hprof

# Complete mode: also zeroes scalar instance fields
hprof-analyzer redact --complete app.hprof safe.hprof
```

## Updating

```bash
hprof-analyzer update nightly   # replace the running binary with the latest nightly
hprof-analyzer update           # show current version + latest nightly, no changes
```

## Shell completions

```bash
# zsh
hprof-analyzer completions zsh > "${fpath[1]}/_hprof-analyzer"

# bash
hprof-analyzer completions bash >> ~/.bash_completion

# fish
hprof-analyzer completions fish > ~/.config/fish/completions/hprof-analyzer.fish
```
