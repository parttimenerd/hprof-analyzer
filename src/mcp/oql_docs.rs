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

This guide describes the recommended sequence of tools for LLMs analyzing heap dumps.

## Quick start

```
1. get_session_info     — check if a dump is already loaded (returns path + stats if yes)
2. get_oql_docs         — learn the query language (this file; no dump needed)
3. load_dump            — load a heap dump file (fast if previously cached)
4. get_summary          — orient: top suspects + top classes
5. get_histogram        — class-level overview with instance/retained counts
6. query                — drill in with OQL queries
7. browse_dominators    — navigate the dominator tree from root or a suspect
8. inspect_object       — detailed view of a specific object
```

## Tool reference

### get_session_info({})
Returns the currently loaded dump path + basic stats, or `{loaded: false}` if nothing is loaded.
Call this first to check state before calling load_dump.

### get_oql_docs({ topic? })
Returns OQL documentation. No dump needed — call this first.
- `topic`: `"syntax"`, `"attributes"`, `"examples"`, `"workflow"`, or `"all"` (default)

### load_dump({ path, with_graph? })
Loads a heap dump. First call takes 5–15 min; subsequent calls load in ~1 s (cached).
- `path`: absolute path to the `.hprof` file (also accepts `.hprof.gz`, `.hprof.zip`)
- `with_graph`: set `true` to enable OQL `@inbounds`/`@outbounds` reference traversal. Adds 1–3 min + 200–600 MB cache. Not needed for most analyses.

### get_summary({})
Returns a Markdown summary: top 5 leak suspects + top 5 classes by retained size.
Good first step after loading. No parameters.

### get_report({ section? })
Returns a section of the full analysis report as JSON.
- `section`: `"leaks"`, `"top"`, `"threads"`, `"overview"`, or `"all"` (default)

### get_histogram({ limit? })
Returns a class histogram: `[{class, instances, retained_bytes}]`.
- `limit`: number of classes to return (default 50)

### query({ oql })
Runs an OQL query. Returns `{columns: [...], rows: [[...]], truncated: bool, row_count: N}`.
- Rows contain plain JSON values: numbers, strings, nulls
- Object references appear as `"ClassName@index"` — extract the index for inspect_object / browse_dominators
- Use `@objectId` to get object indices
- Use `@retainedHeapSize` to find large retained objects
- Rows are capped to 10,000; `truncated: true` means there are more

### browse_dominators({ object_index?, depth?, width? })
Navigates the dominator tree.
- `object_index`: omit to start at the GC root (recommended starting point)
- `depth`: levels to expand (default 3, max 8)
- `width`: children per node (default 10, max 50)
- Returns a tree of `{class, index, retained_bytes, children: [...]}`
- Use `index` values with `inspect_object` or nested `browse_dominators` calls

### inspect_object({ object_index })
Returns details for one object: class, shallow size, retained size.
- `object_index`: from `@objectId` column in a query or from `browse_dominators`
- Use OQL with `@inbounds`/`@outbounds` to follow references; `inspect_object` shows sizes and class only.

## Typical investigation workflow

### "Find the memory leak"
```
1. get_summary()          → see top suspects
2. get_histogram(50)      → confirm which class dominates
3. query("SELECT @objectId, @retainedHeapSize FROM com.example.Suspect ORDER BY @retainedHeapSize DESC LIMIT 5")
4. browse_dominators(index=<from step 3>)  → see what's holding it
5. inspect_object(index=<from step 4>)    → field-level detail
```

### "Understand memory composition"
```
1. get_histogram(20)
2. query("SELECT classof(x) AS class, SUM(@retainedHeapSize) AS ret FROM INSTANCEOF java.lang.Object x GROUP BY classof(x) ORDER BY ret DESC LIMIT 10")
3. browse_dominators()    → top retained from root
```

### "String waste"
```
1. query("SELECT toString(s) AS v, COUNT(*) AS n FROM java.lang.String s GROUP BY toString(s) ORDER BY n DESC LIMIT 20")
2. query("SELECT COUNT(*), SUM(@usedHeapSize) FROM java.lang.String")
```

## Tips

- Object indices (`@objectId`) are dense integers stable within one run but not across runs.
- `@objectAddress` is the raw heap address — useful for cross-referencing with other tools.
- After `load_dump`, the dump stays loaded in memory for the entire server session.
- Reload a different dump by calling `load_dump` again with the new path.
- Cache files are stored next to the dump as `<name>.hprof-cache/`. Delete them to force re-analysis.
"#;
