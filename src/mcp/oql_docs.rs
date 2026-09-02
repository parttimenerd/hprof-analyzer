/// Static OQL documentation for `get_oql_docs` / `heap docs`.
///
/// Returns Markdown for the requested topic. `"all"` concatenates all sections.
pub fn get_oql_docs(topic: Option<&str>) -> String {
    match topic.unwrap_or("all") {
        "syntax" => SYNTAX.to_string(),
        "attributes" => ATTRIBUTES.to_string(),
        "examples" => EXAMPLES.to_string(),
        "workflow" => WORKFLOW.to_string(),
        _ => format!("{SYNTAX}\n\n{ATTRIBUTES}\n\n{EXAMPLES}\n\n{WORKFLOW}"),
    }
}

// ── Syntax ────────────────────────────────────────────────────────────────────

const SYNTAX: &str = r#"# OQL Syntax

OQL (Object Query Language) is a SQL-like language for querying Java heap dumps.

## Basic SELECT

```sql
SELECT <columns> FROM <class> [alias] [WHERE <cond>]
       [GROUP BY <expr>] [ORDER BY <expr> [DESC]] [LIMIT n]
```

**Column expressions** can be:
- Object attributes: `@objectAddress`, `@usedHeapSize`, `@retainedHeapSize`, `@displayName`, `@objectId`, `@length`
- Field access: `s.value`, `m.table`, `e.key`
- Aggregate functions: `COUNT(*)`, `SUM(expr)`, `MIN(expr)`, `MAX(expr)`, `MEDIAN(expr)`, `PERCENTILE(expr, p)`
- Functions: `classof(x)`, `toString(x)`
- Column alias: `expr AS name`

## FROM clause — class matching

| Pattern | Matches |
|---------|---------|
| `java.lang.String` | Exact class name |
| `java.lang.String*` | Glob (prefix match — does NOT match inner classes like `String$Entry`) |
| `INSTANCEOF java.lang.Object` | Class + all subclasses (Java instanceof) |
| `byte[]`, `int[]`, `char[]` | Primitive arrays |
| `java.lang.Object[]` | Object arrays |

**⚠️ Inner classes** use `$` separator: `java.util.HashMap$Node`, `java.util.zip.ZipFile$Source`.
Glob `HashMap*` does NOT match `HashMap$Node` — inner classes must be named exactly or with `INSTANCEOF`.

**⚠️ Reserved alias conflict**: Do not use `retained` or `RETAINED` as a column alias — it conflicts with the `AS RETAINED SET` keyword. Use `retained_bytes`, `ret`, or `ret_b` instead.

## WHERE clause

```sql
WHERE @usedHeapSize > 1024
WHERE toString(s) = "hello"
WHERE x.size() > 0 AND x.size() < 100
WHERE classof(x) IN (SELECT @objectAddress FROM java.lang.Class WHERE ...)
```

Comparison operators: `=`, `!=`, `<`, `<=`, `>`, `>=`
Logical operators: `AND`, `OR`, `NOT`
Subquery: `IN (<subquery>)` — inner query must project `@objectAddress`

## UNION

Combine results from multiple FROM clauses:

```sql
SELECT @objectAddress, @usedHeapSize AS bytes FROM byte[]
UNION SELECT @objectAddress, @usedHeapSize AS bytes FROM char[]
ORDER BY bytes DESC LIMIT 20
```

`ORDER BY` and `LIMIT` after the last UNION apply to the merged result.

## GROUP BY / aggregates

```sql
SELECT classof(x) AS class, COUNT(*) AS n, SUM(@usedHeapSize) AS bytes
FROM INSTANCEOF java.lang.Object x
GROUP BY classof(x)
ORDER BY bytes DESC LIMIT 30
```

`GROUP BY` requires every non-aggregate column to be in the group key.
Aggregates without `GROUP BY` reduce the entire result to one row.

## Subqueries

```sql
SELECT @objectAddress, @usedHeapSize
FROM java.util.HashMap m
WHERE @objectAddress IN (
    SELECT m.@objectAddress FROM java.lang.Thread t
)
```

The inner subquery must project `@objectAddress` for the outer `IN` check.

## AS RETAINED SET

Computes the retained set of a class (all objects kept alive only by instances
of this class). Produces a synthetic object count + retained size summary.
The `AS RETAINED SET` clause goes after the SELECT expression:

```sql
SELECT x AS RETAINED SET FROM java.lang.Thread x
```

## dominators(x)

Returns the immediate dominator set of an object in SELECT position:

```sql
SELECT @objectId AS idx, dominators(s) FROM java.lang.String s LIMIT 10
```

Useful to find which object dominates each string (i.e. what keeps it alive).
"#;

// ── Attributes ────────────────────────────────────────────────────────────────

const ATTRIBUTES: &str = r#"# OQL Attributes and Functions

## Object attributes (@ prefix)

| Attribute | Type | Description |
|-----------|------|-------------|
| `@objectId` | integer | Dense object index, **0-based** (use directly with browse_dominators/inspect_object) |
| `@objectAddress` | integer | Raw heap address from the HPROF file (⚠️ not the same as object_index) |
| `@usedHeapSize` | integer | Shallow size in bytes |
| `@retainedHeapSize` | integer | Retained size in bytes (requires full pipeline) |
| `@displayName` | string | `ClassName@hexAddr` label |
| `@length` | integer | Array element count (arrays only; null for non-arrays) |
| `@inbounds` | objects | Incoming references (OQL queries build edges on demand; `with_graph=true` on load_dump enables them in inspect_object) |
| `@outbounds` | objects | Outgoing references (same as @inbounds) |

**Index types summary:**
- `@objectId` from OQL: **0-based** → use directly with `browse_dominators({object_index: N})`
- `index` from `browse_dominators` output: **0-based** → use directly
- `obj_index_1based` from `get_report` (dominator_tree + root_path): **1-based** → subtract 1 before using

`@retainedHeapSize` is always available when running via `heap query` (it uses
the cached pipeline). In the REPL without full analysis it may require `--full`.

## Functions

| Function | Description |
|----------|-------------|
| `classof(x)` | Class object for the instance `x` |
| `toString(x)` | String value (for java.lang.String instances only) |

## Aggregate functions

| Function | Description |
|----------|-------------|
| `COUNT(*)` | Number of rows / objects |
| `SUM(expr)` | Sum of numeric expression |
| `MIN(expr)` | Minimum value |
| `MAX(expr)` | Maximum value |
| `MEDIAN(expr)` | Median value (50th percentile) |
| `PERCENTILE(expr, p)` | p-th percentile (0–100) |

## @viz directive

Append `-- @viz value=<col> [label=<col>] [type=bar|pie|heatmap]` after a query
to render results as a chart in the web UI or REPL:

```sql
SELECT classof(x) AS class, SUM(@usedHeapSize) AS bytes
FROM INSTANCEOF java.lang.Object x
GROUP BY classof(x) ORDER BY bytes DESC LIMIT 10
-- @viz value=bytes label=class type=bar
```

## Field access

Access Java field values with dot notation:
- `s.value` — the `char[]` backing array of a String
- `m.table` — the entry array of a HashMap
- `e.key`, `e.value` — fields of a Map.Entry
- `x.size()` — reads the `size` field from common collection types (ArrayList, HashMap, etc.)

Field access is available for instance/static fields declared in the class.
Method dispatch is limited to a fixed set: `size()` on known collection types (ArrayList, HashMap, HashSet, etc.),
`intValue()`/`longValue()` on boxed primitives, `length()` on arrays, `getName()`/`getObjectId()` etc. on any object,
and `equals()`. Arbitrary method calls not in this list return `NULL`.

## Class matching modifiers

| Syntax | Meaning |
|--------|---------|
| `FROM java.lang.String` | Exact class match |
| `FROM java.lang.String*` | Glob: matches String, StringBuilder, StringBuffer, … |
| `FROM INSTANCEOF java.util.List` | Includes ArrayList, LinkedList, and all List implementations |
"#;

// ── Examples ──────────────────────────────────────────────────────────────────

const EXAMPLES: &str = r#"# OQL Examples

## Top consumers

```sql
-- Top classes by retained size
SELECT classof(x) AS class, SUM(@retainedHeapSize) AS ret_bytes
FROM INSTANCEOF java.lang.Object x
GROUP BY classof(x) ORDER BY ret_bytes DESC LIMIT 20
```

```sql
-- Largest individual objects (idx usable with browse_dominators/inspect_object)
SELECT @objectId AS idx, classof(x) AS class, @retainedHeapSize AS ret_bytes
FROM INSTANCEOF java.lang.Object x ORDER BY ret_bytes DESC LIMIT 10
```

## String analysis

```sql
-- Duplicate string values (memory waste) — count only (SUM alongside toString is not supported)
SELECT toString(s) AS value, COUNT(*) AS count
FROM java.lang.String s
GROUP BY toString(s) ORDER BY count DESC LIMIT 20
```

```sql
-- Largest string objects (idx usable with browse_dominators)
SELECT @objectId AS idx, toString(s) AS value, @usedHeapSize AS bytes
FROM java.lang.String s ORDER BY bytes DESC LIMIT 10
```

```sql
-- Total string overhead
SELECT COUNT(*) AS count, SUM(@usedHeapSize) AS total_bytes
FROM java.lang.String
```

## Collections

```sql
-- Large collections (over 10k elements)
SELECT @objectId AS idx, classof(x) AS class, x.size() AS size
FROM INSTANCEOF java.util.AbstractCollection x
WHERE x.size() > 10000 ORDER BY size DESC LIMIT 20
```

```sql
-- Empty collections (wasted overhead)
SELECT @objectId AS idx, classof(x) AS class
FROM INSTANCEOF java.util.AbstractCollection x
WHERE x.size() = 0 LIMIT 50
```

```sql
-- Large primitive arrays (over 1 MB)
SELECT @objectId AS idx, classof(x) AS class, @usedHeapSize AS bytes
FROM byte[] x WHERE @usedHeapSize > 1048576
UNION SELECT @objectId AS idx, classof(x) AS class, @usedHeapSize AS bytes
FROM int[] x WHERE @usedHeapSize > 1048576
ORDER BY bytes DESC LIMIT 20
```

## Leak investigation

```sql
-- Objects with retained size over 10 MB (potential leak suspects)
SELECT @objectId AS idx, classof(x) AS class, @retainedHeapSize AS ret_bytes
FROM INSTANCEOF java.lang.Object x
WHERE @retainedHeapSize > 10000000 ORDER BY ret_bytes DESC LIMIT 20
```

```sql
-- Find specific cached objects
SELECT @objectId AS idx, @usedHeapSize AS bytes
FROM com.example.MyCache* ORDER BY bytes DESC
```

```sql
-- Thread locals by thread (idx usable with browse_dominators)
SELECT @objectId AS idx, t.name AS thread_name, @retainedHeapSize AS ret_bytes
FROM java.lang.Thread t ORDER BY ret_bytes DESC
```

## Retained set analysis

```sql
-- What does this class retain?
SELECT x AS RETAINED SET FROM java.util.WeakHashMap x
```

```sql
-- Shallow vs retained ratio (identifies objects that dominate large subgraphs)
SELECT classof(x) AS class,
       SUM(@usedHeapSize) AS shallow,
       SUM(@retainedHeapSize) AS ret_bytes
FROM INSTANCEOF java.lang.Object x
GROUP BY classof(x)
ORDER BY ret_bytes DESC LIMIT 20
```

## Class loader analysis

```sql
-- All class loaders
SELECT @objectId AS idx, classof(x) AS class
FROM INSTANCEOF java.lang.ClassLoader
```

```sql
-- Class count per loader
SELECT classof(x) AS loader, COUNT(*) AS class_count
FROM INSTANCEOF java.lang.ClassLoader x
GROUP BY classof(x) ORDER BY class_count DESC
```

## Reference graph (@inbounds / @outbounds)

```sql
-- Objects that hold a reference to a specific address
SELECT @objectId AS idx, classof(x) AS class
FROM INSTANCEOF java.lang.Object x
WHERE <target_address> IN (SELECT @objectAddress FROM @outbounds)
LIMIT 20
```

## Working with object indices

`@objectId` gives a **0-based** dense integer — use directly with `heap inspect --index N`,
`heap browse --index N`, `browse_dominators({object_index: N})`, `inspect_object({object_index: N})`.

`obj_index_1based` from `get_report` (dominator_tree + root_path) is **1-based** — subtract 1 before using.

```sql
-- Get the object index for inspection
SELECT @objectId AS idx, classof(x) AS class, @retainedHeapSize AS ret_bytes
FROM INSTANCEOF java.lang.Object x
WHERE @retainedHeapSize > 50000000
ORDER BY ret_bytes DESC LIMIT 5
```

Then: `heap inspect --index <idx>` or `heap browse --index <idx>` (idx is 0-based, use directly)
"#;

// ── Workflow ──────────────────────────────────────────────────────────────────

const WORKFLOW: &str = r#"# LLM Workflow Guide

Recommended tool call sequence for analyzing heap dumps. Send ONE tool call at a time and
wait for the response before sending the next.

## Answering "find the leak" or "why is there an OOM"

```
Step 1: load_dump({"path": "/absolute/path/to/dump.hprof"})
  → Response immediately shows: top suspects + top classes by retained size
  → WAIT for this to complete (5–15 min first time, ~1 s cached)

Step 2: get_report({"section": "triage"})
  → ⭐ Severity-tagged signals (critical/warning/info)
  → Fastest way to identify the main problem category
  → Read "critical" signals first — they point directly to the issue

Step 3: get_report({"section": "leaks"})
  → Root paths, dominated objects, accumulation point, dominator_tree per suspect
  → Most reliable leak detection — uses dominator-tree analysis
  → obj_index_1based in root_path, dominated, dominator_tree is 1-BASED: subtract 1!

Step 4: get_report({"section": "top"})
  → Biggest classes by retained size + what holds them (2-level breakdown)
  → Confirms which class is retaining the most memory

Step 5 (optional — drill into a specific class):
  query({"oql": "SELECT @objectId AS idx, @retainedHeapSize AS ret FROM <ClassName> ORDER BY ret DESC LIMIT 10"})
  → The idx values can be passed to browse_dominators or inspect_object

Step 6: browse_dominators({"object_index": <idx>})
  → Shows what this object keeps alive; follow largest child to root cause
```

NOTE: The "leak-suspects" view (query({oql:"leak-suspects"})) uses a >10 MB threshold —
returns empty for small/medium heaps. Always use get_report({section:"leaks"}) instead.

## Comprehensive deep-dive workflow

```
Step 1: get_session_info({})
  → If loaded=true: skip to step 3
  → If loaded=false: go to step 2

Shortcut at any time: list_views({})
  → Shows all 20 built-in named queries. No dump needed.
    Use names DIRECTLY: query({"oql": "top-retained-by-class"}) — no copy-paste of SQL needed.

Step 2: load_dump({"path": "/absolute/path/to/dump.hprof"})
  IMPORTANT: this blocks for 5–15 min on first load; ~1 s on repeat (cached). Wait for it.
  The response already shows top suspects and top classes — read it before calling get_summary.

Step 3: get_report({"section": "triage"})
  → Automated severity signals; read "critical" first. Best quick orientation.

Step 4: get_summary({})
  → Top suspects + top classes + suggested OQL queries for each suspect.
    Read the "Suggested OQL Queries" section — it has ready-to-run queries.

Step 5: get_histogram({"limit": 20})
  → Confirms which classes dominate. Pick the top class and run step 6.

Step 6: query({"oql": "top-retained-by-class"})  ← VIEW NAME (no SQL needed)
  → Top classes by retained size. All 20 view names: list_views({})

  Or custom OQL:
  query({"oql": "SELECT @objectId AS idx, @retainedHeapSize AS ret FROM <ClassName> ORDER BY ret DESC LIMIT 10"})
  → Replace <ClassName> with the top suspect from step 4 or 5.
    The idx column gives object_index values for steps 7 and 8.

Step 7: browse_dominators({"object_index": <idx_from_step6>})
  → Tree of what this object retains. Children sorted by retained_bytes desc.
    Follow the largest child to find the root cause.
    Each node has an "index" field for drilling deeper.

Step 8: inspect_object({"object_index": <index_from_step7>})
  → Class name + shallow_bytes + retained_bytes for a specific object.

Step 9 (collection/reference analysis):
  get_report({"section": "collections"})  → fill ratios, map load factors, waste budget
  get_report({"section": "waste"})        → reclaimable bytes by source
  get_report({"section": "references"})  → Soft/Weak/Phantom reference breakdown
  get_report({"section": "retainers"})   → which stack frames/fields keep things alive
```

## Tool reference

### get_session_info({})
Returns the currently loaded dump path + basic stats, or `{loaded: false}`.
Always call first. If `loaded=true`, skip `load_dump`.

### load_dump({ path, with_graph? })
Loads a heap dump. **Blocks until complete** — wait for the response.
- First load: 5–15 min (writes disk cache)
- Repeat load of same file: ~1 s
- `path`: absolute path (accepts `.hprof`, `.hprof.gz`, `.hprof.zip`, `.tgz`)
- `with_graph`: set `true` only if you need the `inspect_object` inbound-refs list. OQL `@inbounds`/`@outbounds` traversal works in queries without it (edges are built on demand during the query scan).

### get_summary({})
Markdown summary: top 5 suspects + top 5 classes by retained size + suggested OQL queries.
**Read the suggested queries** — they are ready to copy into `query()`.

### get_histogram({ limit? })
Class histogram sorted by retained size: `[{class, instances, retained_bytes}]`.
Good for confirming which class dominates before writing OQL.
- `limit`: default 50; use 20 for a quick scan

### get_report({ section? })
Full report section as JSON. **Best tool for leak investigation** — use before query().

**Core sections (always available):**
- `"leaks"` — ranked suspects with root paths, holder chains, stack traces ← START HERE for OOM/leak
- `"top"` — biggest classes + objects with 2-level holder breakdown
- `"threads"` — per-thread retained size and stack traces
- `"overview"` — heap totals, object count

**Analysis sections (granular, targeted):**
- `"triage"` — ⭐ automated severity signals (critical/warning/info) — fastest diagnosis after leaks
- `"waste"` — reclaimable memory: duplicate strings, empty collections, boxed primitives
- `"indicators"` — anonymous class count, ThreadLocal null keys, DirectByteBuffer total
- `"retainers"` — which stack frames and fields are keeping objects alive by retained size
- `"arrays"` — array length distribution by power-of-two buckets
- `"collections"` — fill ratios, map load factors, kind stats, constant arrays
- `"references"` — Soft/Weak/Phantom reference counts + which classes are referenced
- `"dominators"` — big-drop objects (retain >> largest child) + immediate-dominator class pairs
- `"components"` — retained heap per class loader with top classes per component
- `"alloc_sites"` — allocation sites (only present when dump has allocation tracking)
- `"thread_locals"` — ThreadLocal leak analysis (full-analysis only, may be empty list)
- `"framework"` — detected framework signatures + recommendations (full-analysis only)
- `"field_stats"` — field-level size statistics (CLI --field-stats flag only; always null in MCP)
- `"all"` — everything merged (large, use targeted sections for efficiency)

**triage JSON structure** (array of signals):
```json
[
  {
    "id": "headline-retainer",      // signal identifier
    "severity": "critical",         // "critical", "warning", or "info"
    "title": "Headline Retainer",   // short label
    "detail": "java.lang.Thread retains 5.5 MB (48.8% of heap). ...",
    "anchor": "leak-suspects",      // HTML anchor in the full report
    "anchor_label": "Leak Suspects",
    "nav_class": "java.lang.Thread" // class name for navigation (may be null)
  }
]
```
Read triage first — it surfaces the most important signals without requiring manual search.

**waste JSON structure:**
```json
{
  "total_bytes": 754336,        // total estimated reclaimable bytes
  "reachable_bytes": 713497,    // reachable portion
  "sources": [
    {
      "label": "Under-filled Object Arrays",
      "bytes": 754336,
      "anchor": "collections"   // which section to inspect for details
    }
  ]
}
```

**leaks JSON structure** (`suspects` array, each element):
```json
{
  "pretty_class": "com.example.MyClass",
  "retained": 123456789,      // bytes retained by all instances of this class
  "shallow": 48,              // shallow bytes
  "instance_count": 1,        // number of instances in this suspect group
  "is_single": true,          // true = one object; false = class group
  "root_type_label": "Thread", // GC root type ("Thread", "JNI Global", etc.)
  "keywords": ["com.example.MyClass"],
  "path": [...],              // accumulation path: steps from suspect → accumulation point
  "accumulation_obj_1based": 67891, // ⚠️ 1-based → subtract 1 for browse_dominators
  "accumulation_class": "java.util.HashMap",
  "accumulation_retained": 123000000,
  "dominated": [              // top objects directly dominated by accumulation point
    {"obj_index_1based": 999, "display_class": "...", "retained": 1234, "shallow": 32}
  ],
  "dominated_total_count": 5000,  // total children (dominated list is capped)
  "dominated_shown": 50,          // how many are shown in "dominated"
  "dominated_by_class": [...],    // class-aggregated histogram of dominated objects

  // Single suspects only (is_single=true):
  "root_path": [              // chain from GC root → this suspect
    {"obj_index_1based": 314096, "display_class": "java.lang.Thread",
     "retained": 5761696, "root_type_label": "Thread", "depth": 0}
  ],
  "dominator_tree": {         // pre-built subtree at the accumulation point
    "obj_index_1based": 67891, // ⚠️ 1-based → subtract 1
    "display_class": "java.util.HashMap",
    "retained": 123000000,
    "shallow": 48,
    "children": [...]
  },

  // Group suspects only (is_single=false):
  "merged_paths": {           // class-keyed prefix tree of top root paths
    "display_class": "com.example.MyClass",
    "object_count": 94,
    "retained": 2791424,
    "children": [...]
  }
}
```
⚠️ **CRITICAL**: `obj_index_1based` values in `root_path`, `dominator_tree`, `dominated`, and `accumulation_obj_1based` are **1-based indices**.
You MUST subtract 1 before passing to `browse_dominators` or `inspect_object`:
  `object_index = obj_index_1based - 1`

Look at `retained` (highest = biggest leak), `pretty_class`, and `root_path`/`merged_paths`
to understand what's holding the leaking objects alive.
The `dominator_tree` gives you a pre-built subtree — use `obj_index_1based - 1` with `browse_dominators`.

### query({ oql })
Runs an OQL query. Returns `{columns, rows, truncated, row_count}`.

**Essential rules:**
- Always `SELECT @objectId AS idx` to get object indices for follow-up calls
- Objects in results appear as `"ClassName@123"` — the number after `@` is the object_index
- `@retainedHeapSize` = everything kept alive only by this object (key metric for leaks)
- `@usedHeapSize` = shallow size (just the object itself, not its children)
- Rows capped to 10,000; `truncated: true` means there are more

**Copy-paste starter queries:**

```sql
-- Top 10 instances of a class by retained size (replace the class name)
SELECT @objectId AS idx, @retainedHeapSize AS ret
FROM com.example.SuspectClass ORDER BY ret DESC LIMIT 10
```

```sql
-- All classes by total retained (find the dominant class)
SELECT classof(x) AS class, COUNT(*) AS n, SUM(@retainedHeapSize) AS ret
FROM INSTANCEOF java.lang.Object x
GROUP BY classof(x) ORDER BY ret DESC LIMIT 20
```

```sql
-- Duplicate strings (find string deduplication opportunities)
SELECT toString(s) AS value, COUNT(*) AS count
FROM java.lang.String s
GROUP BY toString(s) ORDER BY count DESC LIMIT 20
```

```sql
-- Large collections (potential unbounded caches)
SELECT @objectId AS idx, classof(x) AS class, @retainedHeapSize AS ret
FROM INSTANCEOF java.util.AbstractCollection x
WHERE @retainedHeapSize > 1000000 ORDER BY ret DESC LIMIT 20
```

### browse_dominators({ object_index?, depth?, width? })
Navigates the dominator tree.
- Omit `object_index` to start at GC root (recommended)
- Each node: `{index, class, retained_bytes, shallow_bytes, children: [...]}`
- Children sorted by `retained_bytes` desc — the largest child is the next thing to investigate
- `index` values → use with `browse_dominators` (drill deeper) or `inspect_object` (details)
- `depth`: default 3 (levels); increase to 5-6 for a deeper view
- `width`: default 10 (children per node); increase for wide trees

### inspect_object({ object_index })
Details for one object: class, shallow_bytes, retained_bytes.
- `object_index`: from `@objectId` in a query or `index` in browse_dominators output
- After inspect, use `browse_dominators({object_index: <same>})` to see what it retains

## Complete worked example: "Find the memory leak"

```
1. get_session_info({})
   → {loaded: false}

2. load_dump({"path": "/tmp/app.hprof"})
   → Loaded. Suspects: com.example.CacheManager (850 MB retained)

3. get_report({"section": "leaks"})
   → suspects[0].pretty_class = "com.example.CacheManager"
   → suspects[0].retained = 850000000
   → suspects[0].accumulation_obj_1based = 12346  ← 1-based!
   → suspects[0].dominator_tree.obj_index_1based = 12346  ← same, 1-based!
   → suspects[0].dominator_tree.children[0] = {obj_index_1based: 67891, display_class: "HashMap", retained: 849MB}

   IMPORTANT: object_index = obj_index_1based - 1
   So: browse_dominators({object_index: 12345})  ← 12346 - 1 = 12345

4. browse_dominators({"object_index": 12345})
   → {index:12345, class:"CacheManager", retained_bytes:850000000,
      children: [{index:67890, class:"java.util.HashMap", retained_bytes:849000000}]}
   (index here is 0-based, same as object_index — use directly)

5. browse_dominators({"object_index": 67890, "depth": 5})
   → Drill into the HashMap — see what entries it holds

6. inspect_object({"object_index": 67890})
   → {class:"java.util.HashMap", shallow_bytes:48, retained_bytes:849000000}

7. query({"oql": "SELECT @objectId AS idx, @retainedHeapSize AS ret FROM com.example.CacheManager ORDER BY ret DESC LIMIT 10"})
   → rows: [[12345, 850000000], ...]  ← @objectId is 0-based, use directly with browse_dominators
```

## Tips

- `@objectId` values from OQL queries are **0-based** dense integers — use directly with `browse_dominators`/`inspect_object`.
- `obj_index_1based` values from `get_report` (in `dominator_tree` and `root_path`) are **1-based** — subtract 1 before passing to `browse_dominators`/`inspect_object`.
- `index` values returned by `browse_dominators` itself are **0-based** — use directly.
- `@objectAddress` is the raw heap address — for cross-referencing with other JVM tools.
- Cache files live next to the dump as `<dump>.hprof-cache/`. Safe to delete to force re-analysis.
- To analyze a different dump: call `load_dump` again with the new path (replaces current session).
- `get_oql_docs({"topic":"examples"})` has 20 more ready-to-run query patterns.
"#;
