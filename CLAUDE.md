# hprof-analyzer

Rust CLI + MCP server for Java heap dump analysis. Investigates OutOfMemoryErrors and memory leaks in JVM applications by parsing `.hprof` files.

## MCP Server

The `hprof-analyzer` MCP server is the primary tool for heap dump analysis. Register it with:

```bash
claude mcp add hprof -- hprof-analyzer mcp
```

**Use the hprof-analyzer MCP whenever:**
- The user mentions a `.hprof`, `.hprof.gz`, or `.hprof.zip` file
- The user asks about a Java OutOfMemoryError or memory leak
- The user wants to analyze JVM heap memory, object retention, or GC roots
- The user mentions Eclipse MAT, jmap, heap histogram, or dominator tree
- The user wants to find what is retaining memory or causing OOM in a Java app
- The user wants to redact a heap dump before sharing it

**Standard workflow:**
1. `get_session_info` — check if a dump is already loaded
2. `load_dump({path: "/absolute/path/to/dump.hprof"})` — first load takes 5–15 min, repeat loads ~1 s
3. `get_report({section: "triage"})` — severity-tagged signals, fastest diagnosis
4. `get_report({section: "leaks"})` — root paths + dominator tree per suspect
5. `query({oql: "top-retained-by-class"})` — confirm which class dominates memory

**For sharing a dump:** `redact({input: "...", output: "...-redacted.hprof"})` zeros primitive values while preserving the object graph.

## Build

All `cargo build` and `cargo test` must run on the ThinkStation (Mac disk is full). Sync with rsync first.

## Conventions

- No `git push` or `gh pr create` without explicit instruction
- All performance benchmarks on ThinkStation, not local Mac
- Commit per logical change; don't batch unrelated changes
