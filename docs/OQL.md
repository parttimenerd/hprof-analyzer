# OQL — Object Query Language

`hprof-analyzer` ships a SQL-flavoured query language, modelled on Eclipse MAT's
OQL, for interrogating a heap dump. You can run queries three ways:

1. **`query` subcommand** — parse the dump once and print result tables to the
   terminal (or start an interactive REPL). Fast; no full report is built.
2. **`--query` / `--query-file` on the main command** — run queries *and* fold
   their results into a full Markdown/HTML/JSON report.
3. **`[[query]]` entries in a config file** — persist a query set alongside a
   dump so every report includes the same views.

Every example below was run against `tests/fixtures/dump_4_philosophers.hprof`.

---

## Quick start

```console
$ hprof-analyzer query heap.hprof --query "SELECT COUNT(*) FROM java.lang.String"
COUNT(*)
24760
(1 row)
```

Interactive REPL (tab-completion for keywords, class names, attributes, and the
`-- @viz` directive):

```console
$ hprof-analyzer query heap.hprof --repl
oql> SELECT @displayName FROM java.lang.Thread LIMIT 3
```

Multiple queries in one run (repeat `--query`, or one query per line in a file):

```console
$ hprof-analyzer query heap.hprof \
    --query "SELECT COUNT(*) FROM java.lang.String" \
    --query "SELECT COUNT(*) FROM java.lang.Thread"
```

---

## Grammar surface

```
SELECT [DISTINCT] [OBJECTS] <select-list> [AS RETAINED SET]
  FROM [OBJECTS] <class | "regex" | INSTANCEOF class | ( subquery )> [alias]
  [WHERE <predicate>]
  [ORDER BY <expr> [ASC|DESC]]
  [LIMIT <n>]
[UNION <select> ...]
```

### SELECT list

- `*` — project the matched object itself.
- A bare alias (`s`) — projects the object, same as `*`.
- An attribute: `@objectId`, `@objectAddress`, `@usedHeapSize`,
  `@retainedHeapSize`¹, `@displayName`, `@length`, `@inbounds`¹, `@outbounds`¹.
- A field path: `s.fieldName`, `s.a.b`.
- A function: `classof(x)`, `toString(x)`, `path(a, b)`, `dominators(x)`¹,
  `dominatorof(x)`¹.
- An aggregate: `COUNT(*)`, `SUM(e)`, `MIN(e)`, `MAX(e)`, `AVG(e)`,
  `MEDIAN(e)`, `PERCENTILE(e, <int>)`.
- `<expr> AS <name>` — rename the output column.

¹ Requires the **full analysis pipeline** — see the caveat below.

### FROM source

| Form | Meaning |
|------|---------|
| `FROM java.lang.String` | exact class name |
| `FROM OBJECTS java.lang.String` | identical to the above (`OBJECTS` is an optional MAT no-op keyword) |
| `FROM INSTANCEOF java.lang.Object` | the class and all subclasses |
| `FROM "java\.lang\..*"` | class-name regex (double-quoted) |
| `FROM ( SELECT … ) s` | subquery as the row source |

### Predicates (WHERE)

- Comparisons: `=`, `!=`, `<`, `<=`, `>`, `>=`.
- `LIKE "regex"` / `NOT LIKE "regex"` — RHS must be a string literal.
- `<attr> INSTANCEOF <class>`.
- `<attr> IN ( SELECT @objectAddress FROM … )` — membership against a subquery
  that selects a single address-valued column. (`IN` takes a **subquery**, not a
  literal value list.)
- Combine with `AND`, `OR`, `NOT`, parentheses.

### Ordering, limiting, union

- `ORDER BY <expr> [ASC|DESC]` sorts the full result before `LIMIT`.
- `LIMIT n` caps rows. In a `UNION`, a trailing `LIMIT` caps the combined result.
- `UNION` concatenates branches. Aggregates are **not** allowed inside a UNION
  branch.

### Not supported

`eval(...)` and `${snapshot}` reflection are MAT features we do **not**
implement.

---

## GROUP BY and HAVING

`GROUP BY` groups rows by one or more expressions and applies aggregate
functions per group. `HAVING` filters groups after aggregation (analogous to
`WHERE` filtering rows before aggregation).

```sql
-- Count instances per class
SELECT @displayName, COUNT(*) AS n
FROM INSTANCEOF java.lang.Object
GROUP BY @displayName
ORDER BY n DESC
LIMIT 10

-- Only classes with many instances
SELECT @displayName, COUNT(*) AS n
FROM INSTANCEOF java.lang.Object
GROUP BY @displayName
HAVING COUNT(*) > 100
ORDER BY n DESC
```

---

## CASE WHEN

`CASE WHEN … THEN … ELSE … END` returns different values based on conditions.
It can be used in `SELECT` and `GROUP BY` expressions.

```sql
-- Bucket objects by size class
SELECT
  CASE
    WHEN @usedHeapSize > 10000 THEN 'large'
    WHEN @usedHeapSize > 1000  THEN 'medium'
    ELSE 'small'
  END AS size_class,
  COUNT(*) AS n
FROM INSTANCEOF java.lang.Object
GROUP BY
  CASE
    WHEN @usedHeapSize > 10000 THEN 'large'
    WHEN @usedHeapSize > 1000  THEN 'medium'
    ELSE 'small'
  END
ORDER BY n DESC
```

---

## COALESCE, NULLIF, BETWEEN

`COALESCE(e1, e2, …)` returns the first non-null argument.
`NULLIF(e1, e2)` returns `null` when `e1 = e2`, otherwise returns `e1`.
`e BETWEEN a AND b` is equivalent to `e >= a AND e <= b`.

```sql
-- Replace null with a default
SELECT COALESCE(toString(s), '<null>') AS val FROM java.lang.String s

-- Filter to objects of moderate size
SELECT COUNT(*) FROM java.lang.Object
WHERE @usedHeapSize BETWEEN 100 AND 1000
```

---

## EXISTS subquery

`EXISTS (SELECT …)` is true when the inner query returns at least one row.
It is non-correlated: the inner query runs once before the outer scan. If
`EXISTS` evaluates to false, the outer query returns 0 rows immediately.
Because the subquery is not re-evaluated per outer row, `EXISTS` cannot filter
by a per-row condition — it either admits all outer rows or none.

```sql
-- Run analysis only when leaked connections exist
SELECT COUNT(*) FROM java.lang.Object
WHERE EXISTS (SELECT * FROM com.example.Connection c WHERE c.closed = false)
```

---

## INTERSECT and EXCEPT

`INTERSECT` returns rows present in **both** result sets.
`EXCEPT` returns rows present in the first set but **not** the second.

```sql
-- Class names in both cache and pool namespaces
SELECT @displayName FROM "com\.example\.cache\..*"
INTERSECT
SELECT @displayName FROM "com\.example\.pool\..*"

-- Strings only in the large set, not the small set
SELECT toString(s) FROM java.lang.String s WHERE s.count > 100
EXCEPT
SELECT toString(s) FROM java.lang.String s WHERE s.count > 1000
```

---

## Array indexing and slicing

`value[i]` returns the element at index `i` (0-based). `value[i:j]` returns a
slice of elements from index `i` up to (but not including) `j`. Out-of-bounds
accesses return `null`.

```sql
-- First element of each array (null if empty or out of bounds)
SELECT @objectId, value[0] AS first FROM byte[] b LIMIT 10

-- Slice of elements
SELECT @objectId, value[1:4] AS mid FROM byte[] b LIMIT 10
```

---

## Worked examples

### Counting and filtering

```console
$ hprof-analyzer query heap.hprof --query "SELECT COUNT(*) FROM java.lang.String"
COUNT(*)
24760

$ hprof-analyzer query heap.hprof --query "SELECT COUNT(*) FROM INSTANCEOF java.lang.Object"
COUNT(*)
134277

$ hprof-analyzer query heap.hprof --query 'SELECT COUNT(*) FROM "java\.lang\..*"'
COUNT(*)
169980
```

`FROM OBJECTS <class>` is identical to `FROM <class>`:

```console
$ hprof-analyzer query heap.hprof --query "SELECT COUNT(*) FROM OBJECTS java.lang.String"
COUNT(*)
24760
```

### Attributes, ordering, and limits

```console
$ hprof-analyzer query heap.hprof \
    --query "SELECT @displayName, @usedHeapSize FROM java.lang.Thread ORDER BY @usedHeapSize DESC LIMIT 3"
@displayName | @usedHeapSize
java.lang.Thread | 104
java.lang.Thread | 104
java.lang.Thread | 104
(3 rows)
```

### Column aliases

```console
$ hprof-analyzer query heap.hprof \
    --query "SELECT @usedHeapSize AS bytes FROM java.lang.String LIMIT 1"
bytes
24
```

### DISTINCT

`DISTINCT` de-duplicates whole result rows (after any `UNION`, before `LIMIT`):

```console
$ hprof-analyzer query heap.hprof \
    --query "SELECT DISTINCT @displayName FROM java.lang.Thread"
@displayName
java.lang.Thread
(1 row)
```

### Aggregates

```console
$ hprof-analyzer query heap.hprof --query "SELECT MEDIAN(@usedHeapSize) FROM java.lang.String"
MEDIAN(@usedHeapSize)
24

$ hprof-analyzer query heap.hprof --query "SELECT PERCENTILE(@usedHeapSize, 95) FROM java.lang.String"
PERCENTILE(95)(@usedHeapSize)
24
```

`PERCENTILE`'s second argument is an **integer** percentile (`95`), not a
fraction (`0.95`).

### Membership with IN

```console
$ hprof-analyzer query heap.hprof \
    --query 'SELECT COUNT(*) FROM java.lang.String s WHERE s IN (SELECT @objectAddress FROM java.lang.String)'
COUNT(*)
24760
```

### UNION

```console
$ hprof-analyzer query heap.hprof \
    --query "SELECT @displayName FROM java.lang.Thread LIMIT 2 UNION SELECT @displayName FROM java.lang.String LIMIT 2"
```

---

## Visualization directives (`-- @viz`)

Prefix a query with a `-- @viz` comment line to declare how its result should be
drawn in the **report** (HTML rich charts, Markdown ASCII bars). The directive is
a stripped comment — it is not part of the OQL grammar and has no effect on the
plain `query`-subcommand table output.

```
-- @viz <kind> [label=<col>] [value=<col>] [cap=<n>] [title="<text>"] [name="<text>"]
SELECT ...
```

| Field | Meaning |
|-------|---------|
| `<kind>` | `table` (default), `histogram`, `piechart`, or `treemap` |
| `label=<col>` | column supplying slice/bar labels (defaults to first non-numeric column) |
| `value=<col>` | column supplying numeric magnitudes (defaults to first numeric column) |
| `cap=<n>` | limit the **chart** to the first `n` rows (the table still shows all) |
| `title="<text>"` | heading rendered above the chart; quote it for multiple words |
| `name="<text>"` | display name for the whole query block, replacing the auto `q{N}` label |

Column names may be attributes (`@usedHeapSize`), aliases, or positional. A
`title=`/`name=` value may be a single bare word or a `"quoted string"` for
multiple words. A malformed directive or an unchartable result falls back to a
plain table with a warning — it never hard-fails the query. Note that `name=`
applies even when the chart itself cannot be drawn (it only labels the block).

Example (as it appears in a `--query-file` or config entry):

```
-- @viz histogram title="Threads by heap" name=threads label=@displayName value=@usedHeapSize cap=10
SELECT @displayName, @usedHeapSize FROM java.lang.Thread ORDER BY @usedHeapSize DESC
```

- **HTML report:** renders a horizontal-bar chart / pie / treemap.
- **Markdown report:** renders ASCII bars (treemap degrades to a
  "chart available in the HTML report" note plus the table).

### CLI gotcha: use the `--query=` equals form for directives

`clap` treats a leading `--` in an argument value as the start of a flag, so
`--query "-- @viz …"` is **rejected**:

```console
$ hprof-analyzer query heap.hprof --query "-- @viz histogram
SELECT * FROM java.lang.String LIMIT 1"
error: unexpected argument '-- @viz histogram …' found
```

Use the attached `--query=` form (or put the directive in a `--query-file` /
config entry, where it is not a shell argument):

```console
$ hprof-analyzer query heap.hprof --query="-- @viz histogram label=@displayName value=@usedHeapSize
SELECT @displayName, @usedHeapSize FROM java.lang.Thread ORDER BY @usedHeapSize DESC LIMIT 10"
```

---

## Two execution paths: query-only vs. full report

The `query` subcommand runs a **query-only** path: it parses the dump in a few
streaming passes and answers queries directly, without building the dominator
tree, retained sizes, or the reference graph. This is fast and low-memory, but
some attributes and functions are unavailable there:

| Needs the full pipeline | query-only error |
|-------------------------|------------------|
| `@retainedHeapSize`, `AS RETAINED SET` | "…requires the full analysis pipeline…" |
| `dominators(x)`, `dominatorof(x)` | "dominator queries … require the full analysis pipeline…" |
| `@inbounds`, `@outbounds`, `path(a, b)` | reference-graph queries need the full path |

To use those, run the **full report** command (no `query` subcommand) and pass
`--query` / `--query-file`. The report build produces the retained sizes,
dominator tree, and reference graph the query then reads:

```console
$ hprof-analyzer heap.hprof report.html \
    --query "SELECT @displayName, @retainedHeapSize FROM java.lang.Thread ORDER BY @retainedHeapSize DESC LIMIT 20"
```

---

## Query files and config

**`--query-file`** — one query per line; blank lines and `#` comments are
skipped. A `-- @viz` line attaches to the query on the following line:

```
# threads-by-retained.oql
-- @viz histogram label=@displayName value=@retainedHeapSize cap=15
SELECT @displayName, @retainedHeapSize FROM java.lang.Thread ORDER BY @retainedHeapSize DESC
SELECT COUNT(*) FROM java.lang.String
```

```console
$ hprof-analyzer heap.hprof report.html --query-file threads-by-retained.oql
```

**Config `[[query]]` entries** — a `.hprof-analyzer.toml` in the working
directory (or `$HOME/.config/hprof-analyzer/collections.toml`) is auto-discovered:

```toml
[[query]]
oql = """
-- @viz piechart label=@displayName value=@retainedHeapSize
SELECT @displayName, @retainedHeapSize FROM java.lang.Thread ORDER BY @retainedHeapSize DESC
"""

[[query]]
oql = "SELECT COUNT(*) FROM INSTANCEOF java.util.Map"
```

Point at a specific file with `--collection-config <path>`.

---

## server subcommand

```console
$ hprof-analyzer server heap.hprof [--port 7070]
```

Starts an HTTP server on `127.0.0.1` (loopback only, default port 7070) that
exposes OQL query execution and report sections as JSON/Markdown endpoints.
The server prints a startup banner listing every available endpoint.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/` | Welcome + endpoint catalog |
| GET | `/version` | `{"version":…,"endpoints":[…]}` |
| GET | `/status` | `{"status":"ready"\|"analyzing"\|"not_started"}` |
| POST | `/analyze` | Trigger full analysis |
| POST | `/` | Run OQL → JSON `QueryResult` |
| POST | `/query` | Alias of `POST /` |
| POST | `/stream` | Run OQL → NDJSON |
| GET | `/help` | OQL language reference JSON |
| GET | `/schema` | JSON Schema for `QueryResult` |
| GET | `/report` | Full report JSON (or `?format=md`) |
| GET | `/report/overview` | `SystemOverview` JSON (or `?format=md`) |
| GET | `/report/leaks` | `LeakSuspects` JSON (or `?format=md`) |
| GET | `/report/top` | `TopConsumers` JSON (or `?format=md`) |
| GET | `/report/threads` | `ThreadOverview` JSON (or `?format=md`) |

### Format negotiation

Append `?format=md` to any `/report/…` endpoint to receive Markdown instead of
JSON:

```console
$ curl -s 'http://127.0.0.1:7070/report?format=md'
$ curl -s 'http://127.0.0.1:7070/report/leaks?format=md'
```

### Lazy analysis

The first `GET /report/…` request automatically triggers analysis if it has not
been started yet. While analysis is running the server returns `202 Accepted`.
Use `GET /status` to poll until the status is `"ready"`, or trigger analysis
explicitly with `POST /analyze` before issuing report requests.

`POST /analyze` always returns 200 with a JSON body describing what happened:

```json
{"ok":true,"status":"started"}          // analysis kicked off now
{"ok":true,"status":"already_running"}  // already in progress
{"ok":true,"status":"already_done"}     // nothing to do
```

### Retained sizes and full-analysis queries

At startup the server runs a **query-only parse** (fast, no dominator tree).
This means `@retainedHeapSize`, `dominators()`, `dominatorof()`, `@inbounds`,
and `@outbounds` are **not available** until the full analysis has been
completed via `POST /analyze` (or an implicit trigger from a `GET /report/…`
request).

Once analysis is done the OQL engine automatically upgrades to the full
pipeline, and retained-size queries work without restarting the server.

### Limiting result rows

Append `?limit=N` to any `/report/leaks`, `/report/top`, or `/report/threads`
endpoint to cap the number of rows in the response:

```console
$ curl -s 'http://127.0.0.1:7070/report/leaks?limit=5'
$ curl -s 'http://127.0.0.1:7070/report/leaks?limit=5&format=md'
```

### OQL body format

`POST /` (and its alias `POST /query`) accept either a raw SQL string or a JSON
object in the request body:

```console
# Plain SQL string
$ curl -s http://127.0.0.1:7070/ -d 'SELECT COUNT(*) FROM java.lang.String'

# JSON body (useful for embedding queries with special characters)
$ curl -s http://127.0.0.1:7070/ \
    -H 'Content-Type: application/json' \
    -d '{"query":"SELECT COUNT(*) FROM java.lang.String","limit":10}'
```

The JSON body accepts:

| Field | Type | Description |
|-------|------|-------------|
| `query` | string | OQL query text (required) |
| `limit` | integer | Cap result rows (optional; overrides `LIMIT` clause) |

Request bodies larger than **64 KiB** are rejected with HTTP 413.

### NDJSON streaming (`POST /stream`)

`POST /stream` runs an OQL query and returns results as
[Newline-Delimited JSON](https://ndjson.org/): one JSON object per line,
flushed as rows are produced. This is useful for large result sets or
streaming to a pipeline.

```console
$ curl -s http://127.0.0.1:7070/stream \
    -d 'SELECT @objectId, @usedHeapSize FROM java.lang.String' \
    | head -5
{"@objectId":1234,"@usedHeapSize":24}
{"@objectId":1235,"@usedHeapSize":24}
...
```

### Error responses

All endpoints return structured JSON errors:

```json
{"ok":false,"error":{"kind":"query","message":"<reason>"}}
{"ok":false,"error":{"kind":"analysis_failed","message":"<reason>"}}
{"ok":false,"error":{"kind":"not_ready","message":"analysis not started — POST /analyze first"}}
{"ok":false,"error":{"kind":"body_too_large","message":"request body exceeds 65536 bytes"}}
```

HTTP status codes: `400` for bad queries or missing `query` field, `404` for
unknown paths, `405` for wrong method, `413` for oversized bodies, `503` if
a `/report/…` section is requested before analysis has been triggered.

### Example workflow

```console
# Start the server
$ hprof-analyzer server heap.hprof --port 7070 &

# Check current status
$ curl -s http://127.0.0.1:7070/status
{"status":"not_started"}

# Trigger full analysis
$ curl -s -X POST http://127.0.0.1:7070/analyze
{"ok":true,"status":"started"}

# Poll until ready
$ until curl -sf http://127.0.0.1:7070/status | grep -q '"ready"'; do sleep 1; done

# Full report as Markdown
$ curl -s 'http://127.0.0.1:7070/report?format=md'

# Leak suspects as JSON (top 5)
$ curl -s 'http://127.0.0.1:7070/report/leaks?limit=5' | jq .

# Run an OQL query (plain text)
$ curl -s http://127.0.0.1:7070/ -d 'SELECT @displayName FROM java.lang.Thread'

# Run a retained-size query (requires analysis to be done first)
$ curl -s http://127.0.0.1:7070/ \
    -d 'SELECT @displayName, @retainedHeapSize FROM java.lang.Thread ORDER BY @retainedHeapSize DESC LIMIT 5'

# Stream a large result set
$ curl -s http://127.0.0.1:7070/stream \
    -d 'SELECT @objectId, @usedHeapSize FROM java.lang.String' \
    | wc -l
```

---

## Eclipse MAT OQL compatibility

`hprof-analyzer`'s OQL is modelled on Eclipse MAT's OQL dialect. This section
documents what works identically, what works with known differences, and what is
not yet supported.

### Extensions beyond MAT

These features are available in `hprof-analyzer` but not in Eclipse MAT's OQL:

| Feature | Example |
|---------|---------|
| `MEDIAN(e)`, `PERCENTILE(e, n)` aggregates | `MEDIAN(@usedHeapSize)` |
| `path(a, b)` bounded forward-reachability walk | `SELECT path(root, target)` |
| `-- @viz <type> …` visualization directive | `-- @viz histogram label=class value=bytes` |
| `GROUP BY` / `HAVING` | `GROUP BY classof(x) HAVING COUNT(*) > 100` |
| Arithmetic expressions in SELECT and WHERE | `@usedHeapSize * 8 AS bits` |
| System properties snapshot (`@systemProperties`) | `SELECT @systemProperties` |
| Reachable-only filter (`--reachable-only` flag) | excludes GC-unreachable objects |
| Interactive REPL with Tab completion and history | `hprof-analyzer query heap.hprof --repl` |
| Report embedding (`--query` / `--query-file`) | fold OQL results into HTML/MD/JSON report |
| Named queries library (`/run <name>`) | `/run top-classes-by-count` |

### Compatible with MAT

These constructs produce results matching MAT (modulo the reachability note below):

| Construct | Example |
|-----------|---------|
| `FROM <class>`, `FROM OBJECTS <class>` | `FROM java.lang.Thread` |
| `FROM INSTANCEOF <class>` including subclasses | `FROM INSTANCEOF java.util.Map` |
| `FROM "<regex>"` double-quoted class-name regex | `FROM "java\.util\..*"` |
| `FROM (subquery)` semi-join | `FROM (SELECT * FROM java.lang.Thread) t` |
| `WHERE`, `ORDER BY`, `LIMIT`, `UNION`, `SELECT DISTINCT` | standard SQL clauses |
| `LIKE "<regex>"` / `NOT LIKE` | `WHERE toString(s) LIKE ".*error.*"` |
| `WHERE x INSTANCEOF C` | |
| `SELECT … AS RETAINED SET`, `SELECT OBJECTS …` | MAT set-operation markers (no-ops here) |
| `<expr> AS <name>` column alias | `@usedHeapSize AS bytes` |
| `COUNT(*)`, `SUM`, `MIN`, `MAX`, `AVG` | |
| `classof(x)`, `toString(x)` (String only), `dominators(x)`, `dominatorof(x)` | |
| `@objectAddress`, `@objectId`, `@usedHeapSize`, `@retainedHeapSize`¹, `@displayName`, `@length`, `@GCRoots` | |
| Field paths: `s.fieldName`, `s.a.b` | |
| MAT-API method aliases: `getObjectAddress()`, `getUsedHeapSize()`, `getKey()`, `getValue()`, `intValue()`, `size()`, … | method → attr/field rewrite |

¹ Requires full analysis (`hprof-analyzer analyze` or `POST /analyze` on the server).

### Differences from MAT

| Area | MAT | hprof-analyzer |
|------|-----|----------------|
| **Unreachable objects** | Discarded at index time | Included in raw scan; MAT ⊆ ours |
| **`s.count` / `s.offset` on String** | Works (pre-JDK9 layout) | "Unknown field" — use `s.value`, `s.coder`, `s.hash` (modern JDK) |
| **Integer `/0`** | Throws `ArithmeticException` | Returns `NULL` — analyzer never crashes on a bad row |
| **`toString()` on non-String** | Calls JVM `toString()` via reflection | Returns `NULL` — static analysis cannot invoke live JVM methods |
| **`get(n)` indexed access** | Works on arrays/collections | Rejected — dereference the backing field directly (e.g. `a.elementData`) |
| **`SELECT COUNT(*) FROM (subquery)`** | Returns count | Rejected — aggregate the inner query instead |

### Not yet supported

| Construct | Notes |
|-----------|-------|
| `FROM OBJECTS 0x7f3a` / `FROM OBJECTS 123456` | Single object by address or id |
| Numeric literal suffixes (`100L`, `1.5F`) | Parsed as plain int/float |
| `${snapshot}.getClasses()` reflection-style FROM | Out of scope |

### Query-file parse error format

When `--query-file` contains a syntax error, the error includes the filename
and 1-based line number:

```
error: --query-file 'queries.oql': parse error on line 3
  3 | SELEC COUNT(*) FROM java.lang.String
    | ^^^^^ expected SELECT
  hint: did you mean SELECT?
```
