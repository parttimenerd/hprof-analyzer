# Custom OQL-Style Queries — Design

**Date:** 2026-07-21
**Branch:** `worktree-custom-oql-queries`
**Status:** Design (awaiting review)

## Goal

Let users run application-specific queries against a heap dump during report
generation, so that many Eclipse-MAT OQL use cases are covered without hand-
coding a new analyzer pass. The user writes a MAT-style OQL query; the analyzer
runs it and appends the results to the report (Markdown / HTML / JSON).

The guiding constraint from the existing analyzer: **low-memory streaming**. The
analyzer deliberately does not keep per-object data resident. So the design must
be *adaptive*: each query is analyzed to determine the minimum data it needs,
and only that data is materialized, attached to the cheapest pipeline phase(s)
that can supply it. A query that needs nothing but the class histogram pays
nothing; a query that follows a 4-hop reference path pays for exactly that walk.

## Non-Goals

- Full MAT OQL parity (graph primitives `dominators()`/`inbounds()`, `UNION`,
  correlated subqueries). These are explicitly rejected with a clear message.
- Interactive/REPL querying. Queries are supplied up front (CLI or TOML) and run
  once during report generation.
- Mutating or re-indexing the dump. Read-only.

## Supported OQL Subset (target)

```
SELECT  <select-list>
FROM    <class-spec> [ <alias> ]
[ WHERE <predicate> ]
[ GROUP BY <class> ]
[ ORDER BY <expr> [ASC|DESC] ]
[ LIMIT <n> ]
```

**FROM / class-spec**
- Exact class: `com.acme.Order`
- Wildcard / regex: `com.acme.*`, `/.*Cache$/`
- Subclass match: `INSTANCEOF com.acme.AbstractJob` (matches subclasses too)

**SELECT list**
- `*` (default row: object index + class + shallow + retained)
- Scalar/String field references: `f.status`, `f.name`
- Path expressions (any depth, adaptive): `f.owner.department.name`
- Attributes: `@objectId`, `@usedHeapSize` (shallow), `@retainedHeap`, `@type`
- Aggregate functions in SELECT when `GROUP BY` present: `COUNT(*)`,
  `SUM(@retainedHeap)`, `MIN/MAX/AVG(<expr>)`
- `toString(f)` for String-typed values

**WHERE predicate**
- Comparisons on scalars: `=,!=,<,<=,>,>=` against int/long/short/byte/
  char/float/double/boolean
- String ops: `=`, `!=`, `LIKE "sub%"`, regex `f.name =~ /.*tmp.*/`
- Path expressions in predicates: `WHERE f.owner.name = "root"`
- Boolean composition: `AND`, `OR`, `NOT`, parentheses
- `INSTANCEOF` test on a field's runtime type
- Null tests: `f.ref = null`, `f.ref != null`

**Explicitly rejected (clear error, not silent degrade)**
- Graph primitives: `dominators(x)`, `inbounds(x)`, `outbounds(x)`
- `UNION`, subqueries, `DISTINCT` across joins
- Aggregates outside a `GROUP BY` context that require a second materialized set

## Architecture

Five components, each independently testable:

```
query::parse      OQL text            -> Query AST
query::plan       Query AST + schema  -> QueryPlan (what data, which phases, depth)
query::execute    QueryPlan + dump    -> QueryResult (bounded rows/aggregates)
query::model      QueryResult         -> serde structs on the Report
report renderers  QueryResult         -> md / html / json sections
```

### 1. Parser (`src/query/parse.rs`)

Hand-written recursive-descent parser (no new heavy dep; the grammar is small
and we control error messages). Produces a `Query` AST: `select`, `from`,
`where`, `group_by`, `order_by`, `limit`. Path expressions parse to a
`Vec<PathSegment>` (field-name hops). Unsupported constructs parse-error early
with the offending token and a hint.

### 2. Planner (`src/query/plan.rs`) — the adaptive core

Given the parsed query and the class/field schema (from pass1 `class_map` +
`strings`, which are live early in pass2), the planner computes a **`QueryPlan`**
describing the minimum data the query needs:

- **`needs`**: a set of data requirements derived by walking the AST:
  - `Histogram` — only class-level aggregates referenced (instances/shallow/
    retained per class). Served entirely from the already-built histogram; **no
    scan**.
  - `InstanceFields { class_set, field_set }` — scalar/String fields of matched
    instances. Served by piggybacking the always-on field-decode
    `scan_all_records` pass (surface B).
  - `RefPath { max_depth, per_hop_field_sets }` — path expressions. The planner
    records the exact hop depth and which fields are dereferenced at each hop.
    Served by a **bounded, depth-adaptive resolve**: only the object-field edges
    named in the path are followed, using the forward CSR + `IdMap::index_of`
    for addr→index resolution. Depth is whatever the query uses — no fixed cap;
    memory scales with the referenced edges, not the whole graph.
- **`attach_phase`**: which existing pipeline phase(s) the executor hooks into.
  The planner picks the earliest phase where all needed data is live, so a query
  reuses an existing scan when possible instead of adding one.
- **Rejection**: if `needs` includes anything unsupported (graph primitives),
  the planner returns a typed error naming the construct.

The planner is where "be adaptive; analyze the query" lives. It is pure
(AST + schema in, plan out) and therefore heavily unit-testable without a dump.

### 3. Executor (`src/query/execute.rs`)

Runs a `QueryPlan`. Strategy per requirement level:

- **Histogram-only**: evaluate directly over the in-memory histogram. Zero scan.
- **InstanceFields**: register a predicate/projector callback on the field-decode
  scan. For each matched instance, decode the referenced scalar/String fields via
  `ClassInfo.fields` offsets (scalars) and the existing String-decode machinery
  (String fields → backing char[]/byte[]). Evaluate WHERE, project SELECT,
  accumulate into bounded result buffers (top-N heap for ORDER BY+LIMIT; grouped
  aggregators for GROUP BY).
- **RefPath (depth d)**: adaptive multi-level resolve. Level 0 = matched roots
  and their first-hop target addrs (collected during the field-decode scan).
  Levels 1..d: resolve the needed target objects' fields. Because HPROF gives no
  record ordering, resolution uses either (a) the forward CSR when the needed
  edge is an indexed object-field, or (b) a batched resolve-scan keyed by the
  frontier addr-set for that hop. Each hop's frontier is bounded by a cap; when a
  cap is hit the result is marked `truncated` (same convention as existing
  analyses).

All result buffers are bounded (top-N, per-group caps) so RSS stays within
budget regardless of dump size — matching every other analysis in the codebase.

### 4. Result model (`src/query/model.rs`)

Serde structs added to `Report`:

```rust
pub struct QueryResult {
    pub name: String,          // query title (TOML name or "query N")
    pub oql: String,           // the source text, echoed
    pub columns: Vec<String>,  // projected column headers
    pub rows: Vec<Vec<QueryValue>>, // bounded rows
    pub row_count: u64,         // total matches (pre-LIMIT)
    pub truncated: bool,        // a cap was hit
    pub error: Option<String>,  // parse/plan/exec error, rendered in-band
}
```

`Report` gains `#[serde(default)] pub queries: Vec<QueryResult>` (additive; older
JSON round-trips). Schema version bumps to 7.

### 5. Renderers

- **Markdown / md-graphs**: one `## Query: <name>` section per result, with the
  echoed OQL in a code fence and a results table. Errors render as a warning
  block.
- **HTML**: a "Custom Queries" section, one card/table per query, consistent with
  existing section styling.
- **JSON**: the `queries` array, verbatim.

## Input Surfaces

Both, per the requirement:

- **CLI**: `--query 'SELECT ...'` (repeatable). Ad-hoc. Title auto-assigned
  (`query 1`, `query 2`) unless the query provides one.
- **TOML**: a `[[query]]` array-of-tables in the existing config file
  (`.hprof-analyzer.toml` / `$HOME/.config/hprof-analyzer/...`), each with
  `name` and `oql`. Saved reusable query packs. Merged with any `--query` flags.

Queries only run in the analyze path (hprof → report), never on JSON re-render
(there is no dump to scan then). Re-rendering a saved report shows the stored
`queries` results as-is.

## Error Handling

Errors never abort the whole report. Each query that fails to parse, plan, or
execute produces a `QueryResult` with `error: Some(msg)` and no rows, rendered
in-band. Valid queries in the same run still succeed. Rejected-unsupported
constructs produce a specific message (e.g.
`"dominators() is not supported; the analyzer does not expose graph primitives to queries"`).

## Testing Strategy (heavy, per requirement)

Test-driven throughout. Layers:

1. **Parser unit tests** — table-driven: each grammar construct parses to the
   expected AST; each unsupported construct produces the expected parse error.
   Property-ish fuzz: random-but-valid queries round-trip parse→display→parse.
2. **Planner unit tests** (no dump) — for a fixed synthetic schema, assert the
   computed `needs`, `attach_phase`, and path depth for representative queries:
   histogram-only, scalar-filter, string-filter, 1-hop, 3-hop, group-by,
   order-by+limit, and every rejection case.
3. **Executor tests on a tiny hand-built dump** — construct a minimal in-memory
   graph fixture (a few classes with known scalar/String/ref fields and known
   sizes) and assert exact query results: counts, sums, top-N ordering, path
   traversal correctness, null handling, truncation flags.
4. **End-to-end CLI tests** (`tests/`) — run the binary against the existing
   benchmark fixtures with representative `--query` flags and TOML packs; assert
   the rendered md/json contains the expected sections/rows. Add golden fixtures.
5. **Determinism** — same query + same dump ⇒ byte-identical output (stable
   sort, stable group order). Covered by the golden fixtures.
6. **Bounds** — a query with no LIMIT on a large fixture must not blow the JSON
   size budget or RSS; assert `truncated` is set and caps hold.

## Rollout / Phasing (still one spec, one plan)

The planner's `needs` levels give a natural implementation order that keeps each
step shippable and independently tested:

1. Parser + AST + planner (pure, no dump) — fully unit tested first.
2. Histogram-only execution + model + renderers — smallest end-to-end slice.
3. InstanceFields (scalar, then String) execution on the field-decode scan.
4. RefPath execution (1-hop, then adaptive N-hop).
5. TOML input + CLI wiring + golden e2e fixtures.

## Open Risks

- **String field decode cost**: decoding String fields requires resolving the
  String's backing array; on String-heavy queries this adds work. Mitigated by
  only decoding fields the query references and bounding matched rows.
- **Path resolution on huge frontiers**: a broad `com.acme.*` FROM with a deep
  path could produce a large hop frontier. Mitigated by per-hop caps +
  `truncated`, consistent with existing analyses.
- **Grammar scope creep**: the subset is deliberately fixed; anything outside it
  errors rather than silently under-delivering.
