---
name: hprof-analyzer
description: Analyze Java heap dumps with hprof-analyzer — start the server, run OQL queries, use the REPL, and produce heap analysis reports
---

# hprof-analyzer skill

`hprof-analyzer` is a fast, low-memory Java heap-dump analyzer. It exposes an
HTTP API, an OQL query engine, and a CLI/browser REPL. Use it to investigate
memory leaks, top consumers, thread locals, and duplicate allocations.

---

## 1. Starting the server

```sh
hprof-analyzer server heap.hprof           # default port 7070, loopback only
hprof-analyzer server heap.hprof --port 8080
```

The server prints a startup banner listing all endpoints. It starts with a
fast query-only parse. `@retainedHeapSize` and dominator attributes require
the full analysis — trigger it with `POST /analyze` and poll `GET /status`.

**Trigger analysis and wait:**
```sh
curl -s -X POST http://127.0.0.1:7070/analyze
until curl -sf http://127.0.0.1:7070/status | grep -q '"ready"'; do sleep 1; done
```

---

## 2. Key endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/status` | `{"status":"ready"\|"analyzing"\|"not_started"}` |
| POST | `/analyze` | Trigger full analysis |
| POST | `/` | Run OQL query → JSON |
| POST | `/stream` | Run OQL query → NDJSON (streaming) |
| GET | `/report` | Full report JSON (add `?format=md` for Markdown) |
| GET | `/report/overview` | System overview |
| GET | `/report/leaks` | Leak suspects |
| GET | `/report/top` | Top consumers |
| GET | `/report/threads` | Thread overview |

Report endpoints accept `?limit=N` to cap rows. Use `?format=md` for
Markdown instead of JSON.

---

## 3. OQL queries

Post a query to `POST /`:

```sh
curl -s http://127.0.0.1:7070/ \
  -d 'SELECT @displayName, COUNT(*) AS n FROM INSTANCEOF java.lang.Object GROUP BY @displayName ORDER BY n DESC LIMIT 10'
```

### Grammar quick-reference

```
SELECT [DISTINCT] [OBJECTS] <select-list>
  FROM [OBJECTS] <class | "regex" | INSTANCEOF class | (subquery)> [alias]
  [WHERE <predicate>]
  [GROUP BY <expr> [HAVING <predicate>]]
  [ORDER BY <expr> [ASC|DESC]]
  [LIMIT <n>]
[UNION <select> ...]
```

### Attributes

| Attribute | Available | Description |
|-----------|-----------|-------------|
| `@objectId` | always | Dense heap object ID |
| `@objectAddress` | always | Native heap address |
| `@usedHeapSize` | always | Shallow (used) size in bytes |
| `@displayName` | always | Class name |
| `@length` | always | Array length (null for non-arrays) |
| `@retainedHeapSize` | after `/analyze` | Retained size (whole sub-graph) |
| `@inbounds` / `@outbounds` | after `/analyze` | Ref-graph edge counts |

### Functions

| Function | Description |
|----------|-------------|
| `classof(x)` | Class name string |
| `toString(x)` | String value (String objects only; null otherwise) |
| `COUNT(*)`, `SUM`, `MIN`, `MAX` | Aggregates |
| `MEDIAN(e)`, `PERCENTILE(e, n)` | Statistical aggregates |

### Five common query patterns

**1. Count instances by class (top 20):**
```sql
SELECT @displayName, COUNT(*) AS n
FROM INSTANCEOF java.lang.Object
GROUP BY @displayName
ORDER BY n DESC
LIMIT 20
```

**2. Top retained-size holders (requires analysis):**
```sql
SELECT @displayName, @retainedHeapSize AS ret_bytes
FROM INSTANCEOF java.lang.Object
ORDER BY ret_bytes DESC
LIMIT 20
```

**3. Thread names and their retained heap:**
```sql
SELECT @displayName, @retainedHeapSize AS ret_bytes
FROM java.lang.Thread
ORDER BY ret_bytes DESC
```

**4. Duplicate string values (top 10 by count — SUM alongside toString is not supported):**
```sql
SELECT toString(s) AS value, COUNT(*) AS n
FROM java.lang.String s
GROUP BY toString(s)
HAVING COUNT(*) > 1
ORDER BY n DESC
LIMIT 10
```

**5. Instances of a specific class with field values:**
```sql
SELECT @objectAddress, fieldName
FROM com.example.MyClass
ORDER BY @usedHeapSize DESC
LIMIT 50
```

---

## 4. REPL commands

Start the CLI REPL with:
```sh
hprof-analyzer query heap.hprof --repl
```

Commands that start with `!` operate on the **last query result** without
re-running the query. Key ones:

| Command | What it does |
|---------|-------------|
| `!top [N]` / `!tail [N]` | First / last N rows |
| `!row [N\|next\|prev]` | Show one row as key=value pairs |
| `!obj <class>#<idx>` | Inspect a specific heap object |
| `!filter <pat>` | Keep rows matching a substring or `/regex/` |
| `!sort <col> [desc]` | Sort result by column |
| `!stats [col]` | Numeric summary: min/max/mean/stddev/p50/p90/p99 |
| `!unique <col> [N]` | Distinct value counts, top N by frequency |
| `!undo` | Restore result before last shaping command |
| `!analyze` | Run full analysis (enables `@retainedHeapSize`) |
| `!run [<name>]` | Run a named query |
| `!describe <class>` | Show fields and types of a class |
| `!help` | Full command reference |

---

## 5. Common agent workflows

### Find the leak

```
1. GET /report/leaks — identify the top leak suspect
2. OQL: SELECT @displayName, @retainedHeapSize FROM INSTANCEOF <suspect-class> ORDER BY @retainedHeapSize DESC LIMIT 5
3. OQL: SELECT @displayName, fieldName FROM <suspect-class> WHERE @retainedHeapSize > 1000000 LIMIT 10
4. Summarize: "Class X retains Y MB. Likely cause: Z based on field W."
```

### Summarize heap composition

```
1. GET /report/overview — read class histogram
2. OQL: SELECT @displayName, COUNT(*) AS n, SUM(@usedHeapSize) AS total_bytes FROM INSTANCEOF java.lang.Object GROUP BY @displayName ORDER BY total_bytes DESC LIMIT 20
3. Identify the top 3 space consumers and explain what they likely represent.
```

### Diff two dumps (growth investigation)

```sh
hprof-analyzer early.hprof a.json
hprof-analyzer later.hprof b.json
hprof-analyzer compare reports a.json b.json --format md
```
Interpret the output: classes marked `spike` grew sharply; `churn` means high
allocation turnover.

---

## 6. Embedding queries in reports

Pass `--query` to the main analysis command (runs after full analysis, so
`@retainedHeapSize` works):

```sh
hprof-analyzer heap.hprof report.html \
  --query="-- @viz histogram label=@displayName value=@retainedHeapSize cap=10
SELECT @displayName, @retainedHeapSize FROM java.lang.Thread ORDER BY @retainedHeapSize DESC LIMIT 10"
```

`-- @viz` kinds: `table`, `histogram`, `piechart`, `treemap`.
Use `--query=` (with `=`) — `clap` treats a leading `--` in argument values as
a flag unless the equals form is used.

---

## 7. Troubleshooting

| Problem | Fix |
|---------|-----|
| `@retainedHeapSize` returns null | `POST /analyze` and wait for `{"status":"ready"}` |
| Query returns 0 rows unexpectedly | Try `FROM INSTANCEOF <class>` instead of `FROM <class>` — the latter is exact-match only |
| `s.count` / `s.offset` on String returns null | Use `s.value` / `s.coder` / `s.hash` (modern JDK 9+ layout) |
| `toString()` returns null | Only works on `java.lang.String` instances; returns null for other types |
| `--query "-- @viz …"` rejected by shell | Use `--query=` (with `=`) so the leading `--` is not treated as a flag |
| Server returns 503 on `/report/*` | Analysis not yet triggered — `POST /analyze` first |
| Large result set hangs | Use `/stream` endpoint for NDJSON, or add `LIMIT N` to the query |
