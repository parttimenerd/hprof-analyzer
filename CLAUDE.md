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

**Disk cache gotcha:** `heap report`/`heap summary`/MCP `load_dump` all read from a per-dump on-disk cache (`<dump>.hprof-cache/`), and `Report.triage` is computed ONCE and cached as part of the serialized report — it is NOT re-evaluated on every read. If you change triage-rule logic, a new report-model field's population, or anything else that alters what `build_model`/`evaluate_triage` produce WITHOUT changing the raw array format, you must bump `VERSION` in `src/cache.rs` (it's the only thing that invalidates a stale cached report under these circumstances — `report.json.zst`'s own staleness check is just `.exists()`, never content-based). Forgetting this makes a correct code change silently appear to do nothing when tested against an already-cached dump. When in doubt, `hprof-analyzer heap cache-clear <dump>` or delete `<dump>.hprof-cache/` before testing.

**Synthetic scenario tests (`tests/scenarios.rs`):** fast, in-process, no-JVM regression tests for "does the report correctly name the culprit" (not just "doesn't crash"), built on a minimal synthetic-HPROF builder (`tests/common/heap_builder.rs`). Add a new leak/waste scenario by appending one `Scenario` table row — see the doc comment at the top of `tests/scenarios.rs` for the exact steps. These run as part of plain `cargo test` (no opt-in flag, no dump files needed); the JVM-captured `leak-demos/`/`test-projects/` suites remain the slower, opt-in "realism" check.

## Conventions

- No `git push` or `gh pr create` without explicit instruction
- All performance benchmarks on ThinkStation, not local Mac
- Commit per logical change; don't batch unrelated changes
