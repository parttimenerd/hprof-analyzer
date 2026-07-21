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
- Wildcard: `com.acme.*` (glob; `*` = any run, `?` = any char)
- Regex: `/.*Cache$/` (glob-subset in the first cut — see "Dependencies")
- Subclass match: `INSTANCEOF com.acme.AbstractJob` (matches subclasses too)

**SELECT list**
- `*` (default row: object index + class + shallow + retained)
- Scalar/String field references: `f.status`, `f.name`
- Path expressions (any depth, adaptive): `f.owner.department.name`
- Attributes: `@objectId`, `@usedHeapSize` (shallow), `@retainedHeap`, `@type`
  (runtime class name), `@displayName`
- Arithmetic on numeric expressions: `f.count * 8`, `@usedHeapSize + f.pad`
- Aggregate functions with `GROUP BY`: `COUNT(*)`, `SUM(<expr>)`,
  `MIN/MAX/AVG(<expr>)`; and top-level aggregates without `GROUP BY` that fold to
  a single row (`SELECT COUNT(*) FROM ...`, `SELECT SUM(@retainedHeap) FROM ...`)
- `toString(f)` for String-typed values
- Column aliasing: `SELECT f.name AS owner`

**WHERE predicate**
- Comparisons on scalars: `=,!=,<,<=,>,>=` against int/long/short/byte/
  char/float/double/boolean
- String ops: `=`, `!=`, `LIKE "sub%"` (glob), regex `f.name =~ /.*tmp.*/`
  (glob-subset in the first cut — see "Dependencies")
- Numeric/attribute predicates: `WHERE @retainedHeap > 1048576`,
  `WHERE @usedHeapSize > 64` (retained predicates force the CrossPhase strategy)
- Path expressions in predicates, any depth: `WHERE f.owner.name = "root"`
- Boolean composition: `AND`, `OR`, `NOT`, parentheses (full precedence via Pratt)
- Set membership: `f.status IN ("OPEN", "PENDING")`
- `INSTANCEOF` test on a field's runtime type
- Null tests: `f.ref = null`, `f.ref != null`

**Class-spec grammar** applies uniformly to `FROM` and to `INSTANCEOF` operands.

**Explicitly rejected (clear error naming the construct, not silent degrade)**
- Graph primitives as query functions: `dominators(x)`, `inbounds(x)`,
  `outbounds(x)`, `path(a,b)` — the analyzer does not expose the raw graph to
  queries (the CSR is torn down mid-pipeline).
- `UNION`, correlated subqueries, `SELECT ... FROM (subquery)`.
- Joins between two independently-scanned classes (only reference-path traversal
  from a single FROM class is supported, not arbitrary N×M joins).
- `DISTINCT` across a projection that would require a second materialized set
  larger than the match-set cap.

Each rejection is produced by the planner with the exact offending construct in
the message, so the user learns *why* and what to change.

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

**Decision: hand-written recursive-descent + Pratt expression parser. No parser
library.** See "Parser choice" below for the evaluated alternatives. Produces a
`Query` AST: `select`, `from`, `where`, `group_by`, `order_by`, `limit`. A small
hand-rolled tokenizer feeds a precedence-climbing (Pratt) parser for the WHERE
expression (AND/OR/NOT, comparisons, `LIKE`, regex, `INSTANCEOF`, dotted paths).
Path expressions parse to a `Vec<PathSegment>` (field-name hops). Every token
carries its source column so errors read `expected <X> near column N`.
Unsupported constructs parse-error early with the offending token and a hint.

### 2. Planner (`src/query/plan.rs`) — the adaptive core

This is where "be adaptive; analyze the query" lives. The planner is a pure
function (AST + schema → plan) and therefore fully unit-testable without a dump.
It does three things: derive the query's data requirements, bind each requirement
to the *earliest* pipeline phase where that data is live, and decide the
execution strategy (including cross-phase hand-offs).

#### Pipeline data-liveness map (the substrate adaptivity plans against)

The analyzer's phases each hold different data live. The planner is built around
this exact map (verified against `main.rs::run` + `pass2::build`):

| Phase | Live data a query can read | Freed after |
|---|---|---|
| **P0 schema** (pass1 done) | class table: names, superclass chain, per-field `(name, type)`; loader labels | — (schema stays cheap) |
| **P1 field-decode scan** (in `pass2::build`, `scan_all_records`) | every object's raw blob + `class_id`; `IdMap` (addr→index) live; field offsets decodable; String-decode machinery live | `id_map`/field plans freed at end of pass2 |
| **P2 forward CSR** (post-pass2, pre-inbound) | `fwd_offsets`/`fwd_targets` (out-edges per object) + reachability `dfn` | moved into inbound transpose, then freed |
| **P3 dominator/retained** (late, in `main.rs`) | `idom`, `retained[]`, `shallow[]`, `class_idx[]` (restored) | end of pipeline |
| **P4 histogram** (build_model) | per-class aggregates: instances/shallow/retained | report phase |

The two load-bearing consequences the planner must encode:

1. **`@retainedHeap` and `@objectId`-dominator data do not exist during the field
   scan (P1).** They first exist at P3. So any query that *filters/decodes fields*
   (needs P1) **and** *projects or orders by retained* (needs P3) is inherently
   **cross-phase**: it cannot be answered in a single visit.
2. **`IdMap` (addr→index) and per-object blobs are alive together only at P1.**
   Reference-path resolution that needs to read a *referent's* fields must either
   be done within P1 (buffering frontiers) or via the forward CSR at P2. After
   P2, the blob-reading machinery is gone.

#### `QueryNeeds` — derived by walking the AST

```
Histogram                  class-level aggregates only (COUNT/SUM/MIN/MAX/AVG
                           over instances/shallow/retained grouped by class)
InstanceScalar { fields }  scalar fields of matched instances (decode by offset)
InstanceString { fields }  String fields (decode String -> backing array)
RefPath { hops }           per-hop: the field dereferenced and whether the hop's
                           target contributes to WHERE (must resolve) or only to
                           SELECT projection (resolve only for surviving rows)
Retained                   any use of @retainedHeap (P3-only)
RuntimeType                @type / INSTANCEOF on a field's *runtime* class
```

The planner unions these across SELECT, WHERE, GROUP BY, and ORDER BY. Each
requirement carries the **minimal field/edge set** it touches — never "all fields
of the class", only the ones the query names. That minimality is the whole point:
a query over one `int` field decodes one `int`.

#### Binding requirements to phases + the strategy

The planner emits a `QueryPlan { strategy, needs, caps, projection, rejection }`.
Strategy is chosen by the highest phase any requirement forces:

- **`HistogramOnly`** — needs ⊆ {Histogram}. Zero scans; evaluated at P4.
- **`SingleScan`** — needs ⊆ {InstanceScalar, InstanceString, RuntimeType} and
  **no** Retained and **no** RefPath. Fully answered in one P1 visit: decode,
  filter, project, feed bounded accumulators. Cost = zero extra scans (rides the
  always-on field-decode scan).
- **`RefWalk { depth }`** — has RefPath. Adaptive resolution (see executor). The
  planner records, per hop, whether that hop is *predicate-critical* (must be
  resolved for all candidates) or *projection-only* (resolve lazily for only the
  rows that survive WHERE + LIMIT) — so a 4-hop SELECT path over a filtered set
  resolves 4 hops for only the surviving handful of rows, not the whole class.
- **`CrossPhase { match_at, finalize_at }`** — needs both P1 (field match) and P3
  (Retained). The planner splits the query: at P1 it records the bounded set of
  **matched object indices** + any P1-decoded projection values into a carry
  buffer (capped); at P3 it joins those indices against `retained[]`/`idom` to
  finish ORDER BY/SELECT. The carry buffer is bounded by an explicit cap
  (`--query-match-cap`, default e.g. 200k indices ≈ 800 KB), and overflow sets
  `truncated` — never unbounded.

`caps` collects every bound the plan will apply (match-set cap, per-hop frontier
cap, group cap, top-N). `rejection` is `Some(msg)` when a requirement is
unsupported (graph primitives, multi-hop *aggregation* joins, etc.), naming the
exact construct.

Because strategy selection is a pure function of `needs`, the planner has a small
finite decision table — every branch of which is a unit test (see Testing).

### 3. Executor (`src/query/execute.rs`)

One executor per strategy; each is a thin driver over existing scan hooks and the
bounded accumulators. The accumulators (top-N heap, grouped aggregators) are
shared across strategies.

- **`HistogramOnly`**: fold the in-memory per-class histogram through the
  aggregators. No dump access.
- **`SingleScan`**: register a `(predicate, projector)` on the P1 field-decode
  `scan_all_records` callback. Per matched instance: decode only the named scalar
  fields (offset read) and String fields (via the existing String→backing-array
  decode), evaluate WHERE, and on pass, project SELECT into the accumulator
  (top-N for ORDER BY+LIMIT; grouped aggregators for GROUP BY). Class matching
  (`INSTANCEOF`, wildcard) is precompiled to a class-index bitset at P0 so the
  hot loop does an O(1) membership test.
- **`RefWalk { depth }`**: adaptive, breadth-first over hops, HPROF-order-safe
  (HPROF gives no record ordering, so referents are resolved in a *later* batch,
  never inline).
  - **Frontier collection (P1):** while scanning, for each candidate object,
    record `(candidate_index, first_hop_target_addr)` for each path used. This is
    the level-0 frontier, bounded by the match-set cap.
  - **Hop resolution:** two mechanisms, planner picks the cheaper per hop:
    (a) **CSR hop** — when the needed edge is an object-reference field and the
    forward CSR is still available (P2), follow `fwd_targets`; addr→index via the
    live map. (b) **Batched resolve-scan** — collect the frontier's target
    *addresses* into a hash set, then in one more P1-style pass decode just those
    objects' needed fields. One extra scan per resolve *batch*, not per object.
  - **Predicate-critical vs projection-only hops:** predicate hops resolve for
    the whole frontier (needed to filter); projection-only hops resolve *after*
    WHERE + LIMIT have pruned the survivors, so deep SELECT paths cost O(surviving
    rows), not O(class). This is the sharpest adaptivity lever.
  - Every hop frontier is capped; hitting a cap sets `truncated`.
- **`CrossPhase`**: run the `SingleScan`/`RefWalk` matcher at P1 into a **carry
  buffer** of `(object_index, partial_projection)` (capped). Persist it past the
  pass2 teardown (it is tiny — indices + a few decoded scalars). At P3, once
  `retained[]`/`idom` exist, join each carried index to its retained size,
  complete the projection, apply ORDER BY/LIMIT, emit rows.

All result buffers and carry buffers are explicitly bounded, so RSS stays within
budget regardless of dump size — matching every other analysis in the codebase.
When a query needs no dump-scan data at all (`HistogramOnly`), no scan is added;
when it needs only P1 data, it rides the always-on field-decode scan with zero
extra passes; only ref-walks and cross-phase joins add work, and only in
proportion to what the query touches.

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

// A single projected cell — a closed value enum so JSON is self-describing.
pub enum QueryValue { Null, Bool(bool), Int(i64), Float(f64), Str(String),
                      ObjRef { index: u64, class: String } }
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

## Parser choice (evaluated alternatives)

No Rust crate parses Eclipse MAT OQL (confirmed: the `oql` crate is an unrelated
iterator DSL; the HPROF crates ship no OQL). So the parser is net-new regardless.
Options evaluated:

- **`sqlparser-rs`** — lightweight (only `log`) but a poor structural fit. Its
  `Dialect` trait tweaks lexing, not grammar productions; the AST is a fixed SQL
  enum. OQL diverges structurally (regex/wildcard class-specs in FROM,
  `INSTANCEOF`, dotted path expressions, regex operators), so we'd fork it or
  abuse its AST. Rejected.
- **`chumsky`** — best-in-class error messages and recovery out of the box, but
  heavier compile times, a churning 0.x API, and three transitive deps. Worth it
  only for large/evolving grammars.
- **`winnow` / `nom` / `pest` / `peg` / `lalrpop`** — all viable but each adds a
  dependency and either weaker default errors (`nom`), stringly-typed ASTs
  (`pest`), or generator/build-script weight (`lalrpop`) for a grammar this small.

**Chosen: hand-written recursive-descent + Pratt.** The grammar is small and
fixed (it will not churn), the WHERE clause is a textbook precedence-climbing
expression parser, and we get full control over error-message text — which
matters because users *will* write malformed queries. Cost: a few hundred lines.
Benefit: zero new dependencies, instant compiles, and exactly the diagnostics we
want. This aligns with the project's lean-dependency posture (see `Cargo.toml`).

## Dependencies

- **No new runtime dependency for parsing** (hand-written, per above).
- **Regex / LIKE evaluation**: the project currently has **no `regex` crate**.
  Rather than pull in `regex` (a heavy dep for a deliberately-lean tool), the
  first cut implements:
  - `LIKE` via a tiny hand-rolled glob matcher (`%` = any run, `_` = any char) —
    trivial and dependency-free.
  - Class-spec wildcards (`com.acme.*`) via the same glob matcher.
  - **Regex operators** (`=~ /.../`, `/.*Cache$/` class-specs) are parsed but, in
    the first cut, evaluated by a minimal anchored-substring/`*` engine; anything
    requiring true regex features returns a clear "regex feature unsupported"
    error. Full regex is a deferred decision: if real demand appears, adding
    `regex` is a one-line `Cargo.toml` change confined to the query executor.
  This keeps the dependency graph unchanged while covering the common cases.
- **Testing** reuses the existing `proptest` dev-dependency for parser
  round-trip/fuzz tests (already in `Cargo.toml`). `toml` is already present
  (`default-features = false, features = ["parse"]`), exactly what `[[query]]`
  packs need — no change required.

## Input Surfaces

Both, per the requirement:

- **CLI**: `--query 'SELECT ...'` (repeatable). Ad-hoc. Title auto-assigned
  (`query 1`, `query 2`) unless the query provides one.
- **TOML**: a `[[query]]` array-of-tables in the existing config file
  (`.hprof-analyzer.toml` / `$HOME/.config/hprof-analyzer/...`), each with
  `name` and `oql`. Saved reusable query packs. Merged with any `--query` flags.
- **`--query-match-cap <n>`** (optional): overrides the default CrossPhase/RefWalk
  match-set and per-hop frontier cap for users who knowingly want larger (or
  smaller) bounded results. Defaults to a safe value (~200k).

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
2. **Planner unit tests** (no dump) — the planner is a pure function, so its
   decision table is exhaustively testable. For a fixed synthetic schema, assert
   the derived `QueryNeeds`, the chosen **strategy**, path depth, per-hop
   predicate-critical/projection-only classification, and the caps for a matrix of
   queries:
   - `HistogramOnly`: `SELECT COUNT(*)`, `SUM(@retainedHeap) GROUP BY class`.
   - `SingleScan`: scalar-only filter; String-only filter; `@usedHeapSize`
     predicate; `@type`/`INSTANCEOF`; `IN (...)`; arithmetic projection.
   - `RefWalk`: 1-hop predicate; 3-hop predicate; deep **projection-only** path
     over a filtered set (assert deep hops are marked projection-only so they
     resolve lazily).
   - `CrossPhase`: field filter + `ORDER BY @retainedHeap LIMIT n` (assert
     match_at=P1, finalize_at=P3); field filter + `SELECT @retainedHeap`.
   - Every **rejection** case, asserting the message names the construct.
   The mapping `needs → strategy` is a finite table; there is a test per cell.
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

The strategy ladder gives a natural implementation order that keeps each step
shippable and independently tested:

1. Parser + AST + planner (pure, no dump), incl. the full `needs → strategy`
   decision table — fully unit tested first.
2. `HistogramOnly` execution + result model + renderers — smallest end-to-end
   slice.
3. `SingleScan` (scalar, then String, then `@type`/`INSTANCEOF`/`IN`) on the P1
   field-decode scan.
4. `CrossPhase` (field match at P1 → carry buffer → join `retained[]` at P3),
   enabling `ORDER BY @retainedHeap` / `SELECT @retainedHeap`.
5. `RefWalk` (1-hop, then adaptive N-hop; CSR-hop and batched-resolve-scan;
   predicate-critical vs projection-only).
6. TOML input + CLI wiring + golden e2e fixtures.

Each step is independently valuable: many real queries are satisfied by steps
1–3 alone; steps 4–5 unlock the "biggest X held by Y" class of questions.

## Open Risks

- **String field decode cost**: decoding String fields requires resolving the
  String's backing array; on String-heavy queries this adds work. Mitigated by
  only decoding fields the query references and bounding matched rows.
- **Path resolution on huge frontiers**: a broad `com.acme.*` FROM with a deep
  path could produce a large hop frontier. Mitigated by per-hop caps +
  `truncated`, and by resolving projection-only hops *after* WHERE+LIMIT pruning.
- **CrossPhase carry-buffer overflow**: a field filter matching millions of
  objects before an `ORDER BY @retainedHeap` would overflow the carry buffer.
  Mitigated by the match-set cap: overflow sets `truncated`, and the result is
  the top-N by the *available* ordering signal, documented as approximate when
  truncated. This is the one place a query result can be a bounded sample rather
  than exact; the flag makes it honest.
- **Grammar scope creep**: the subset is deliberately fixed; anything outside it
  errors rather than silently under-delivering.
- **Glob-vs-regex gap**: the first cut serves `LIKE`/wildcards and a glob-subset
  of regex without the `regex` crate. Queries using true regex features get a
  clear "regex feature unsupported" error rather than a wrong match. If demand
  warrants, adding `regex` is a `Cargo.toml` one-liner confined to the executor.
