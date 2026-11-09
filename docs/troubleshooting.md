# Troubleshooting

Common issues and their solutions when using hprof-analyzer.

## Installation

### "command not found" after downloading

The downloaded archive unpacks into a directory (e.g. `hprof-analyzer-aarch64-apple-darwin/`). Move the binary onto your `PATH`:

```sh
sudo mv hprof-analyzer-*/hprof-analyzer /usr/local/bin/
```

Verify with `hprof-analyzer --version`.

### macOS: "hprof-analyzer cannot be opened because the developer cannot be verified"

macOS Gatekeeper blocks unsigned binaries. Remove the quarantine attribute:

```sh
xattr -d com.apple.quarantine /usr/local/bin/hprof-analyzer
```

### Linux: "No such file or directory" (despite the file existing)

This usually means the binary was built for glibc but you're on a musl-based distro (Alpine, etc.). Use the musl build instead:

```sh
curl -L https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-x86_64-unknown-linux-musl.tar.gz | tar xz
```

## Analysis

### Out of memory during analysis

hprof-analyzer's peak RSS scales with the number of objects in the dump, not the dump file size. For very large dumps (20+ GiB), you may need 15–20 GiB of available RAM.

**Mitigations:**
- Close other memory-intensive applications during analysis.
- Use the streaming `query` subcommand for simple queries — it does not build the full dominator tree:
  ```sh
  hprof-analyzer query heap.hprof --query "SELECT COUNT(*) FROM java.lang.String"
  ```
- Generate a JSON report on a machine with enough RAM, then re-render locally:
  ```sh
  # On a big machine:
  hprof-analyzer heap.hprof report.json.gz
  # Locally (no heap dump needed):
  hprof-analyzer report.json.gz report.html
  ```

### Analysis is slow

First-time analysis builds the full dominator tree and computes retained sizes. This is inherently compute-intensive for large dumps:

| Dump size | Typical wall time |
|-----------|------------------|
| < 100 MiB | 1–3 seconds |
| 1 GiB | 3–30 seconds |
| 10 GiB | 1–2 minutes |
| 34 GiB | 10–15 minutes |

**Tips:**
- Use the `heap` subcommand group — it caches results, so subsequent queries take ~1 second:
  ```sh
  hprof-analyzer heap summary heap.hprof    # slow first time, cached after
  hprof-analyzer heap query heap.hprof --oql "..."   # ~1 s from cache
  ```
- For quick triage without retained sizes, use the `query` subcommand (streaming, no cache, no dominator tree).
- Use `--detail minimal` to reduce report size on huge dumps.

### "Truncated input" warning

This means the heap dump file is incomplete — the JVM was likely killed mid-dump, or the file was copied before the dump finished. hprof-analyzer recovers whatever objects were parsed and produces a partial report. The warning is informational, not an error.

To get a complete dump, ensure the JVM finishes writing before you copy the file. Use `jcmd <pid> GC.heap_dump /tmp/app.hprof` and wait for completion.

### Stale results after code changes (developers)

The `heap` subcommand group and MCP server cache analysis results in `<dump>.hprof-cache/`. If you modify triage rules or report logic, the cache may serve stale data.

**Fix:** clear the cache:
```sh
hprof-analyzer heap cache-clear <dump>
# or: rm -rf <dump>.hprof-cache/
```

If you're a contributor, also bump `VERSION` in `src/cache.rs` — see [CONTRIBUTING.md](../CONTRIBUTING.md#cache-invalidation-important).

## OQL queries

### "Sort key not in SELECT list" note

OQL requires the `ORDER BY` column to be in the `SELECT` list. This query silently ignores the sort:

```sql
SELECT @displayName FROM java.lang.Thread ORDER BY @usedHeapSize DESC
```

Fix: include the sort key in `SELECT`:

```sql
SELECT @displayName, @usedHeapSize FROM java.lang.Thread ORDER BY @usedHeapSize DESC
```

### @retainedHeapSize returns NULL

Retained sizes require the full analysis pipeline (dominator tree computation). They are **not** available in:
- The streaming `query` subcommand
- The REPL started via `hprof-analyzer query ... --repl`

Use one of these instead:
- `hprof-analyzer heap query heap.hprof --oql "..."` (cached, full pipeline)
- `--query` flag on the main command: `hprof-analyzer heap.hprof report.html --query "..."`
- The HTTP server: `hprof-analyzer server heap.hprof`, then `POST` queries to `http://127.0.0.1:7070/`
- The MCP server's `query` tool

### Query hangs or is very slow

Queries without a `LIMIT` clause scan all matching objects. On large dumps with millions of instances, a `SELECT * FROM java.lang.Object` can take a long time.

**Fix:** add `LIMIT` or use a more specific class:

```sql
SELECT @displayName, @usedHeapSize FROM java.lang.String
ORDER BY @usedHeapSize DESC LIMIT 20
```

## MCP server

### Claude/Cline can't find the MCP server

Make sure `hprof-analyzer` is on your `PATH`. Test with:

```sh
which hprof-analyzer
hprof-analyzer mcp --help
```

If you installed to a non-standard location, use the full path in the MCP config:

```json
{
  "mcpServers": {
    "hprof": {
      "command": "/full/path/to/hprof-analyzer",
      "args": ["mcp"]
    }
  }
}
```

### "load_dump" takes a long time

The first `load_dump` runs the full analysis pipeline. This is expected — it can take 5–15 minutes for large dumps. Subsequent loads of the same dump use the cache and return in ~1 second.

You can also pre-populate the cache:

```sh
hprof-analyzer heap load /path/to/heap.hprof
```

## Browser UI

### "File too large" error in the browser

The WebAssembly browser UI supports dumps up to approximately 3 GiB. For larger dumps, use the local server mode:

```sh
hprof-analyzer server heap.hprof
# Then open http://127.0.0.1:7070 in your browser
```

### Charts don't render

This can happen if the browser blocks WebAssembly or inline scripts. Try:
- A different browser (Chrome, Firefox, Safari all work)
- Opening the HTML file from a local server instead of `file://`
- Checking the browser console for errors

## MAT compatibility

### MAT shows "Index file is older than heap dump"

MAT compares timestamps. If you regenerated cache files after the `.hprof` was modified, MAT may reject them. Re-run:

```sh
hprof-analyzer mat caches heap.hprof
```

### Slight differences in root paths vs. MAT

The synthetic GC-root model differs slightly between hprof-analyzer and MAT. Some root-path queries may return slightly different paths. The retained sizes and dominator tree structure are validated against MAT and match.

See [docs/mat-cache.md](mat-cache.md) for a full list of known differences.

## Getting help

- **GitHub issues:** [github.com/parttimenerd/hprof-analyzer/issues](https://github.com/parttimenerd/hprof-analyzer/issues) — bug reports, feature requests, questions
- **OQL reference:** `hprof-analyzer heap docs` or [docs/OQL.md](OQL.md)
- **AI-assisted analysis:** register the MCP server and ask your AI assistant to help investigate the dump
