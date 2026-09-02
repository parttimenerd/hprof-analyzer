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
| `java.lang.String*` | Glob (prefix match) |
| `INSTANCEOF java.lang.Object` | Class + all subclasses (Java instanceof) |
| `byte[]`, `int[]`, `char[]` | Primitive arrays |
| `java.lang.Object[]` | Object arrays |

## WHERE clause

```sql
WHERE @usedHeapSize > 1024
WHERE toString(s) = "hello"
WHERE x.size() > 0 AND x.capacity() < 100
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
of this class). Produces a synthetic object count + retained size summary:

```sql
SELECT * FROM java.lang.Thread AS RETAINED SET
```

## dominators()

Returns the virtual root dominator tree entry point for browsing:

```sql
SELECT @objectAddress, @retainedHeapSize FROM dominators()
```

Useful as a starting point to find the top retained objects.
"#;

// ── Attributes ────────────────────────────────────────────────────────────────

const ATTRIBUTES: &str = r#"# OQL Attributes and Functions

## Object attributes (@ prefix)

| Attribute | Type | Description |
|-----------|------|-------------|
| `@objectId` | integer | Dense object index (stable within one run) |
| `@objectAddress` | integer | Raw heap address from the HPROF file |
| `@usedHeapSize` | integer | Shallow size in bytes |
| `@retainedHeapSize` | integer | Retained size in bytes (requires full pipeline) |
| `@displayName` | string | `ClassName@hexAddr` label |
| `@length` | integer | Array element count (arrays only; null for non-arrays) |
| `@inbounds` | objects | Incoming references (requires `--with-graph`) |
| `@outbounds` | objects | Outgoing references (requires `--with-graph`) |

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
- `x.size()` — calls size() method (read-only reflection on live heap data)

Field access is available for instance/static fields declared in the class.
Method calls (`size()`, `length()`) are supported for common collection APIs.

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
SELECT classof(x) AS class, SUM(@retainedHeapSize) AS retained
FROM INSTANCEOF java.lang.Object x
GROUP BY classof(x) ORDER BY retained DESC LIMIT 20
```

```sql
-- Largest individual objects
SELECT @objectAddress, classof(x) AS class, @retainedHeapSize AS retained
FROM INSTANCEOF java.lang.Object x ORDER BY retained DESC LIMIT 10
```

## String analysis

```sql
-- Duplicate string values (memory waste)
SELECT toString(s) AS value, COUNT(*) AS count, SUM(@usedHeapSize) AS bytes
FROM java.lang.String s
GROUP BY toString(s) ORDER BY count DESC LIMIT 20
```

```sql
-- Largest string objects
SELECT @objectAddress, toString(s) AS value, @usedHeapSize AS bytes
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
SELECT @objectAddress, classof(x) AS class, x.size() AS size
FROM INSTANCEOF java.util.AbstractCollection x
WHERE x.size() > 10000 ORDER BY size DESC LIMIT 20
```

```sql
-- Empty collections (wasted overhead)
SELECT @objectAddress, classof(x) AS class
FROM INSTANCEOF java.util.AbstractCollection x
WHERE x.size() = 0 LIMIT 50
```

```sql
-- Large primitive arrays (over 1 MB)
SELECT @objectAddress, classof(x) AS class, @usedHeapSize AS bytes
FROM byte[] x WHERE @usedHeapSize > 1048576
UNION SELECT @objectAddress, classof(x) AS class, @usedHeapSize AS bytes
FROM int[] x WHERE @usedHeapSize > 1048576
ORDER BY bytes DESC LIMIT 20
```

## Leak investigation

```sql
-- Objects with retained size over 10 MB (potential leak suspects)
SELECT @objectAddress, classof(x) AS class, @retainedHeapSize AS retained
FROM INSTANCEOF java.lang.Object x
WHERE @retainedHeapSize > 10000000 ORDER BY retained DESC LIMIT 20
```

```sql
-- Find specific cached objects
SELECT @objectAddress, @usedHeapSize AS bytes
FROM com.example.MyCache* ORDER BY bytes DESC
```

```sql
-- Thread locals by thread
SELECT @objectAddress, t.name AS thread_name, @retainedHeapSize AS retained
FROM java.lang.Thread t ORDER BY retained DESC
```

## Retained set analysis

```sql
-- What does this class retain?
SELECT * FROM java.util.WeakHashMap AS RETAINED SET
```

```sql
-- Shallow vs retained ratio (identifies objects that dominate large subgraphs)
SELECT classof(x) AS class,
       SUM(@usedHeapSize) AS shallow,
       SUM(@retainedHeapSize) AS retained
FROM INSTANCEOF java.lang.Object x
GROUP BY classof(x)
ORDER BY retained DESC LIMIT 20
```

## Class loader analysis

```sql
-- All class loaders
SELECT @objectAddress, classof(x) AS class
FROM INSTANCEOF java.lang.ClassLoader
```

```sql
-- Class count per loader
SELECT classof(x) AS loader, COUNT(*) AS class_count
FROM INSTANCEOF java.lang.ClassLoader x
GROUP BY classof(x) ORDER BY class_count DESC
```

## Reference graph (requires --with-graph)

```sql
-- Objects that hold a reference to a specific address
SELECT @objectAddress, classof(x) AS class
FROM INSTANCEOF java.lang.Object x
WHERE <target_address> IN (SELECT @objectAddress FROM @outbounds)
LIMIT 20
```

## Working with object indices

Object indices from `@objectId` (dense index) and `@objectAddress` (heap
address) can be used with `heap inspect --index N` and `heap browse --index N`.

```sql
-- Get the object index for inspection
SELECT @objectId AS idx, @objectAddress AS addr, classof(x) AS class
FROM INSTANCEOF java.lang.Object x
WHERE @retainedHeapSize > 50000000
ORDER BY @retainedHeapSize DESC LIMIT 5
```

Then: `heap inspect --index <idx>` or `heap browse --index <idx>`
"#;

// ── Workflow ──────────────────────────────────────────────────────────────────

const WORKFLOW: &str = r#"# LLM Workflow Guide

Recommended tool call sequence for analyzing heap dumps. Send ONE tool call at a time and
wait for the response before sending the next.

## Session start

```
Step 1: get_session_info({})
  → If loaded=true: skip to step 3
  → If loaded=false: go to step 2

Shortcut at any time: list_views({})
  → Shows all 20 built-in named queries. No dump needed.
    Use names DIRECTLY: query({"oql": "leak-suspects"}) — no copy-paste of SQL needed.

Step 2: load_dump({"path": "/absolute/path/to/dump.hprof"})
  IMPORTANT: this blocks for 5–15 min on first load; ~1 s on repeat (cached). Wait for it.

Step 3: get_summary({})
  → Returns top suspects + top classes + suggested OQL queries for each suspect.
    Read the "Suggested OQL Queries" section — it has ready-to-run queries.

Step 4: get_histogram({"limit": 20})
  → Confirms which classes dominate. Pick the top class and run step 5.

Step 5a: query({"oql": "leak-suspects"})   ← VIEW NAME SHORTCUT (no SQL needed)
  → Objects retaining >10 MB. Best starting point.
    All 20 view names are listed in the load_dump response. More: list_views({})

Step 5b: query({"oql": "SELECT @objectId AS idx, @retainedHeapSize AS ret FROM <ClassName> ORDER BY ret DESC LIMIT 10"})
  → Replace <ClassName> with the top suspect from step 3 or 4.
    The idx column gives object_index values for steps 6 and 7.

Step 6: browse_dominators({"object_index": <idx_from_step5>})
  → Tree of what this object retains. Children sorted by retained_bytes desc.
    Follow the largest child to find the root cause.
    Each node has an "index" field for drilling deeper.

Step 7: inspect_object({"object_index": <index_from_step6>})
  → Class name + shallow_bytes + retained_bytes for a specific object.
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
- `with_graph`: set `true` only if you need `@inbounds`/`@outbounds` OQL traversal (rare)

### get_summary({})
Markdown summary: top 5 suspects + top 5 classes by retained size + suggested OQL queries.
**Read the suggested queries** — they are ready to copy into `query()`.

### get_histogram({ limit? })
Class histogram sorted by retained size: `[{class, instances, retained_bytes}]`.
Good for confirming which class dominates before writing OQL.
- `limit`: default 50; use 20 for a quick scan

### get_report({ section? })
Full report section as JSON. Use specific sections — `"all"` is large.
- `"leaks"` — ranked suspects with root paths and holder chains (start here for leak investigation)
- `"top"` — biggest classes + objects with 2-level holder breakdown
- `"threads"` — per-thread retained size and stack traces
- `"overview"` — heap totals

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
   → Loaded. Suspects: com.example.CacheManager

3. get_summary({})
   → #1 suspect: CacheManager (850 MB retained)
     Suggested query: SELECT @objectId AS idx, @retainedHeapSize AS ret FROM com.example.CacheManager ORDER BY ret DESC LIMIT 10

4. query({"oql": "SELECT @objectId AS idx, @retainedHeapSize AS ret FROM com.example.CacheManager ORDER BY ret DESC LIMIT 10"})
   → rows: [[12345, 850000000], ...]
   (object_index 12345 retains 850 MB)

5. browse_dominators({"object_index": 12345})
   → {index:12345, class:"CacheManager", retained_bytes:850000000,
      children: [{index:67890, class:"java.util.HashMap", retained_bytes:849000000, children:[...]}]}

6. browse_dominators({"object_index": 67890, "depth": 5})
   → Drill into the HashMap — see what entries it holds

7. inspect_object({"object_index": 67890})
   → {class:"java.util.HashMap", shallow_bytes:48, retained_bytes:849000000}
```

## Tips

- `@objectId` indices are dense integers, stable within one session but NOT across sessions/reloads.
- `@objectAddress` is the raw heap address — for cross-referencing with other JVM tools.
- Cache files live next to the dump as `<dump>.hprof-cache/`. Safe to delete to force re-analysis.
- To analyze a different dump: call `load_dump` again with the new path (replaces current session).
- `get_oql_docs({"topic":"examples"})` has 20 more ready-to-run query patterns.
"#;
