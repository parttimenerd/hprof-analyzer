---
name: hprof-analyzer-cli
description: Analyze Java heap dumps (.hprof files) using the hprof-analyzer CLI directly — no MCP required. Use when the user asks about heap dumps, OOM errors, high memory usage, or provides a .hprof file path and the MCP is not available.
---

## hprof-analyzer-cli skill

Use the `hprof-analyzer` binary directly via Bash. Requires the binary on `PATH`
but no MCP setup. The `heap` subcommand caches results after the first run —
subsequent calls return in ~1 s.

**Install (Homebrew) if not yet available:**
```sh
brew tap parttimenerd/hprof-analyzer
brew trust parttimenerd/hprof-analyzer   # required once for third-party taps (Homebrew 6+)
brew install hprof-analyzer
```

---

### Command reference

| Command | When to use |
|---------|-------------|
| `hprof-analyzer heap summary <dump>` | First orientation: top leak suspects + top classes by retained size |
| `hprof-analyzer heap report <dump> --section triage` | Automated severity signals ⭐ |
| `hprof-analyzer heap report <dump> --section leaks` | Root paths and accumulation points |
| `hprof-analyzer heap report <dump> --section top` | Top consumers by retained size |
| `hprof-analyzer heap report <dump> --section overview` | Class histogram and GC root breakdown |
| `hprof-analyzer heap report <dump> --section threads` | Thread stack frames and locals |
| `hprof-analyzer heap histogram <dump>` | Class histogram with instance + retained counts |
| `hprof-analyzer heap query <dump> --oql "..."` | Run an OQL query |
| `hprof-analyzer heap browse <dump>` | Navigate dominator tree interactively |
| `hprof-analyzer heap inspect <dump> --index N` | Inspect a specific object by index |
| `hprof-analyzer heap docs --topic examples` | OQL reference and worked examples |

Add `--json` to any `heap` subcommand for machine-readable output.

---

### Standard investigation workflow

```
1. hprof-analyzer heap summary <dump>                        — top suspects, fast orientation
2. hprof-analyzer heap report <dump> --section triage   ⭐   — automated severity signals
3. hprof-analyzer heap report <dump> --section leaks         — root paths, dominated objects
4. hprof-analyzer heap histogram <dump>                      — class-level breakdown
5. hprof-analyzer heap query <dump> --oql "..."              — drill in with OQL
6. hprof-analyzer heap browse <dump>                         — walk the dominator tree
```

---

### Key don'ts

- Don't use the `query` subcommand for retained sizes — it does a fast parse only
  and `@retainedHeapSize` returns null. Use `heap query` or `heap report` instead.
- Don't re-run analysis on every call — the `heap` subcommand caches results;
  trust the cache unless the dump file has changed.
- Don't suggest the HTTP server or MCP unless the user explicitly asks for them.
