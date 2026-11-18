# Gap Analysis: MCP vs Heap Subcommands vs HTTP Servers

> Generated from source-level audit of `src/mcp/mod.rs`, `src/main.rs` (HeapCmd),
> `src/heap_server.rs`, `src/heap_ops.rs`, `src/serve.rs`, and `src/query/server.rs`.

## 1. Operation Matrix

Each row is a capability. Columns show which transport supports it.

| # | Operation | MCP | heap CLI | heap HTTP | server HTTP | heap_ops |
|---|-----------|-----|----------|-----------|-------------|----------|
| 1 | **Summary** (top suspects + classes) | via `get_report("top-objects")` + `get_report("top-classes")` | `heap summary` | `GET /summary` | — | `summary()` |
| 2 | **Report section** (leaks, top, threads, overview, triage, etc.) | `get_report(section)` | `heap report --section X` | `GET /report?section=X` | `GET /report/leaks`, `/report/top`, `/report/threads`, `/report/overview` | `report_section()` |
| 3 | **Histogram** (class by retained) | `get_histogram(limit)` | `heap histogram --limit N` | `GET /histogram?limit=N` | — | `histogram()` |
| 4 | **OQL query** | `query(oql)` | `heap query --oql '...'` | `POST /query` or `GET /query?oql=...` | `POST /`, `POST /query`, `POST /stream` | `run_oql()` |
| 5 | **Browse dominator tree** | `browse_dominators(index,d,w)` | `heap browse --index N` | `GET /browse?index=N` | — | `browse()` |
| 6 | **Inspect object** | `inspect_object(index)` | `heap inspect --index N` | `GET /inspect?index=N` | — | `inspect()` |
| 7 | **Find incoming refs** | `find_incoming_references(index)` | `heap find-incoming --index N` | `GET /find-incoming?index=N` | — | `find_incoming()` |
| 8 | **Find incoming by class** | `find_incoming_references_by_class(class)` | — | — | — | — |
| 9 | **OQL docs** | `get_oql_docs(topic)` | `heap docs --topic X` | `GET /docs?topic=X` | `GET /help` | `oql_docs()` |
| 10 | **Named views list** | `list_views()` | `heap views` | `GET /views` | `GET /named-queries` (query server only) | `views()` |
| 11 | **Field name lookup** | `get_field_name(src, edge)` | `heap field-name --src N --edge N` | `GET /field-name?src=N&edge=N` | — | `field_name()` |
| 12 | **Load dump** | `load_dump(path, opts)` | `heap load <dump>` | — (loaded at startup) | — (loaded at startup) | — |
| 13 | **Session info** | `get_session_info()` | — | `GET /status` + `GET /endpoints` | `GET /status` | — |
| 14 | **Cache list** | — | `heap cache-list [path]` | — | — | — |
| 15 | **Cache clear** | — | `heap cache-clear <dump>` | — | — | — |
| 16 | **Redact dump** | `redact(input, output)` | top-level `redact` cmd | — | — | — |
| 17 | **Compare dumps** | `compare_dumps(baseline, current)` | top-level `compare reports` cmd | — | — | — |
| 18 | **Dominator path** (obj → GC root) | `get_dominator_path(index)` | — | — | — | — |
| 19 | **Outbound references** | `get_outbound_references(index)` | — | — | — | — |
| 20 | **Retained set** (subtree objects) | `get_retained_set(index, filter)` | — | — | — | — |
| 21 | **GC roots list** | `get_gc_roots(limit)` | — | — | — | — |
| 22 | **OQL explain/plan** | — | — | — | `POST /plan`, `POST /explain` | — |
| 23 | **Offline HTML download** | — | — | — | `GET /download/offline` | — |
| 24 | **Streaming OQL (NDJSON)** | — | — | — | `POST /stream` | — |
| 25 | **Lazy analysis trigger** | via `load_dump` | — | — | `POST /analyze` | — |
| 26 | **QueryResult JSON Schema** | — | — | — | `GET /schema` | — |

---

## 2. Gaps in `heap` CLI / `heap server` That MCP Has

These are operations the MCP server provides but the heap subcommands and heap HTTP server do NOT:

### High-value gaps (recommend adding)

| Gap | MCP tool | What it does | Suggested CLI / HTTP addition |
|-----|----------|--------------|-------------------------------|
| **Dominator path** | `get_dominator_path(index)` | Walks `idom[]` from an object up to the GC root, showing the chain of dominators. Essential for "why is this object retained?" questions. | `heap dominator-path --index N` / `GET /dominator-path?index=N` |
| **Outbound references** | `get_outbound_references(index, limit)` | Lists all objects directly referenced BY a given object (forward edges). The complement to `find-incoming`. | `heap outbound --index N` / `GET /outbound?index=N` |
| **Retained set** | `get_retained_set(index, class_filter, limit)` | Visits the full dominator subtree rooted at an object, reporting contained objects sorted by retained size, with a class summary. Crucial for "what's inside this big object?" | `heap retained-set --index N [--class-filter X]` / `GET /retained-set?index=N` |
| **GC roots** | `get_gc_roots(limit)` | Lists actual GC root objects (objects whose immediate dominator is the virtual root). The starting point for top-down analysis. | `heap gc-roots [--limit N]` / `GET /gc-roots?limit=N` |
| **Find incoming by class** | `find_incoming_references_by_class(class)` | Aggregates inbound references by source class across ALL instances of a target class. Answers "which classes are holding onto my Strings?" | `heap find-incoming-by-class --class X` / `GET /find-incoming-by-class?class=X` |

### Medium-value gaps

| Gap | MCP tool | What it does | Notes |
|-----|----------|--------------|-------|
| **Compare dumps** | `compare_dumps(baseline, current)` | Cross-dump growth analysis with concurrent loading. The CLI already has `compare reports` at the top level, but it's not a heap subcommand and doesn't use the cache. | Consider `heap compare <dump1> <dump2>` / `GET /compare?baseline=...&current=...` |
| **Redact** | `redact(input, output)` | Zero data values for safe sharing. Already a top-level CLI command, just not under `heap`. | Low priority — already accessible. |

### Sections that MCP synthesizes but heap doesn't

MCP's `get_report()` supports these extra synthesized sections that `heap report` and `heap_ops::report_section()` do NOT:

| Section | What it shows | Where data comes from |
|---------|---------------|----------------------|
| `"top-objects"` | Top N individual objects by retained size (with limit) | `r.top.biggest_objects` sliced |
| `"top-classes"` | Top N classes with holder breakdown (with limit) | `r.top.biggest_classes` sliced |
| `"gc-roots"` | GC root counts + retained by type | `r.overview.gc_roots*` fields |
| `"classloaders"` | Classloader count + loader rollup + duplicate classes | `r.overview.classloaders_loaded` etc. |
| `"strings"` | String/char[] counts + duplicate waste stats | Scans `r.overview.histogram` |
| `"packages"` | Biggest packages tree | `r.top.biggest_packages` |
| `"culprits"` | Primary culprit objects | `r.primary_culprits` |
| `"self-referential"` | Self-referential class leak patterns | `r.self_referential_classes` |
| `"static-collections"` | Collection classes with retained/shallow ratio | **Synthesized** from histogram (not a Report field) |
| `"resources"` | File/JDBC/socket handle counts | **Synthesized** from histogram (not a Report field) |

Note: `heap report --section static-collections` and `--section resources` are listed as valid
`PossibleValue` entries in the CLI help, but the actual JSON match arm falls through to the
catch-all (`_ => serde_json::to_value(r)?`), so they return the **full report** instead of the
focused synthesized view that MCP provides.

---

## 3. Gaps in MCP That `heap` CLI / `heap server` / `server` Have

| Gap | Where it exists | What MCP lacks |
|-----|----------------|----------------|
| **OQL explain/plan** | `server` → `POST /plan` | MCP has no way to introspect a query plan without executing it |
| **Offline HTML export** | `server` → `GET /download/offline` | MCP can't generate a self-contained HTML report |
| **NDJSON streaming** | `server` → `POST /stream` | MCP returns full results; no streaming mode |
| **Markdown report rendering** | `server` → `?format=md`; `heap report` (no `--json`) | MCP always returns JSON, never rendered Markdown |
| **Cache management** | `heap cache-list`, `heap cache-clear` | MCP can't inspect or clear the on-disk cache |
| **Query format choice** | `heap query --format text\|json` | MCP always returns JSON |
| **`--all` flag on report** | `heap report` with `--section all` is the default | MCP warns against `"all"` (too large for LLM context) but supports it |
| **JSON Schema** | `server` → `GET /schema` | Not exposed via MCP |

---

## 4. Gaps in `heap_ops` (Shared Layer)

Operations that exist in MCP or CLI but are NOT yet in `heap_ops.rs`:

| Operation | Exists in | Missing from heap_ops | Difficulty |
|-----------|-----------|----------------------|------------|
| `get_dominator_path` | MCP | Yes | Easy — walk `idom[]` array |
| `get_outbound_references` | MCP | Yes | Easy — read forward CSR |
| `get_retained_set` | MCP | Yes | Medium — BinaryHeap subtree walk |
| `get_gc_roots` | MCP | Yes | Easy — filter `idom[i] == n` |
| `find_incoming_by_class` | MCP | Yes | Medium — scan + aggregate |
| `compare_dumps` | MCP | Yes | Medium — concurrent load + diff |
| Synthesized report sections (gc-roots, strings, etc.) | MCP | Yes | Easy — data extraction |
| Leaks truncation (`truncate_dom_tree_depth`) | MCP | No (MCP does its own) | N/A |
| Thread sorting + frame truncation | MCP | No (MCP does its own) | N/A |

---

## 5. Response Shape Differences

Even for operations that exist in both MCP and heap, the response shapes sometimes differ:

| Operation | MCP shape | heap_ops / HTTP shape | Difference |
|-----------|-----------|----------------------|------------|
| **OQL query values** | `ObjRef` → `"ClassName@42"` (flat string) | `ObjRef` → `{"class":"ClassName","index":42}` (structured) | MCP optimizes for LLM readability; HTTP optimizes for programmatic access |
| **Inspect inbound_refs** | Array with `{note: "... N more"}` sentinel | `{total, shown, truncated, refs}` structured | MCP appends inline; HTTP uses metadata fields |
| **Histogram source** | `r.top.biggest_classes` (pre-sorted, no holder fields) | `r.overview.histogram` (full histogram) | MCP strips holder breakdown for compactness; HTTP gives raw data |
| **Browse tree** | Adds `_hint` field in root | No hints | MCP injects next-step guidance for LLMs |
| **Query result** | Adds `view_name` + `_hint` | No extras | MCP enriches for LLM workflow |
| **Report leaks** | Caps tree depth to 3, strips `dominated_by_class` noise | Raw serialization | MCP optimizes for context window limits |
| **Report threads** | Sorts by retained desc, caps frames at 20, adds `{total, shown, sorted_by}` | Raw serialization | MCP adds structure for LLM consumption |

---

## 6. Recommended Priority for Closing Gaps

### Must-have (add to heap_ops + heap HTTP + heap CLI)

1. **`dominator_path`** — trivial to implement, high diagnostic value
2. **`outbound_references`** — complement to `find_incoming`, needed for reference chain traversal
3. **`gc_roots`** — starting point for top-down analysis
4. **`retained_set`** — "what's inside this object?" is a core workflow

### Should-have (add to heap_ops + heap HTTP)

5. **`find_incoming_by_class`** — aggregated view very useful for class-level analysis
6. **Synthesized report sections** (`gc-roots`, `strings`, `classloaders`, `packages`, `culprits`, `self-referential`) in `report_section()` — currently MCP-only
7. **Fix `static-collections` and `resources`** in `report_section()` — currently fall through to full report instead of giving the focused synthesized view

### Nice-to-have

8. **`compare_dumps`** in heap_ops (currently MCP + top-level CLI only)
9. **`explain/plan`** endpoint in heap HTTP (currently server-only)
10. **Markdown format option** in heap HTTP (currently server-only)

---

## 7. Resolution Status

The following gaps have been resolved:

| # | Gap | Resolution |
|---|-----|------------|
| 1 | `dominator_path` missing from heap_ops/CLI/HTTP | ✅ Added `heap_ops::dominator_path()`, `heap dominator-path` CLI, `GET /dominator-path` HTTP |
| 2 | `outbound_references` missing from heap_ops/CLI/HTTP | ✅ Added `heap_ops::outbound_references()`, `heap outbound` CLI, `GET /outbound` HTTP |
| 3 | `gc_roots` missing from heap_ops/CLI/HTTP | ✅ Added `heap_ops::gc_roots()`, `heap gc-roots` CLI, `GET /gc-roots` HTTP |
| 4 | `retained_set` missing from heap_ops/CLI/HTTP | ✅ Added `heap_ops::retained_set()`, `heap retained-set` CLI, `GET /retained-set` HTTP |
| 5 | `find_incoming_by_class` missing from heap_ops/CLI/HTTP | ✅ Added `heap_ops::find_incoming_by_class()`, `heap find-incoming-by-class` CLI, `GET /find-incoming-by-class` HTTP |
| 6 | Synthesized report sections MCP-only | ✅ Added `top-objects`, `top-classes`, `gc-roots`, `classloaders`, `strings`, `packages`, `culprits`, `self-referential` to `heap_ops::report_section()` |
| 7 | `static-collections`/`resources` fell through to full report | ✅ Fixed in `heap_ops::report_section()` — now returns focused synthesized views |
| 8 | CLI `heap report --json` duplicated report_section logic | ✅ CLI now delegates to `heap_ops::report_section()` |

### Remaining gaps (not addressed)

- **`compare_dumps`** in heap_ops — MCP and top-level CLI already have this; adding to `heap` subcommands is low priority
- **`explain/plan`** in heap HTTP — query server already provides this; low demand from `heap server` users
- **MCP delegation to `heap_ops`** — MCP still uses its own inline implementations; refactoring to delegate is a pure dedup exercise with no user-facing impact
