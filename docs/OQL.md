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

`GROUP BY`, array-slice (`s[1:3]`), `eval(...)`, and `${snapshot}` reflection
are MAT features we do **not** implement. `GROUP BY` in particular parses to an
error — aggregate over a filtered `FROM` instead.

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
