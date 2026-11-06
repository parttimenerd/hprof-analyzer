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
that can supply it. This adaptivity is **per-feature and intelligent** — cost is
armed one-to-one by what the query invokes (see "Intelligent cost gating"), with
no baseline query tax — and **execution latency scales with what the query
touches, not the dump size** (see "Fast execution"). A query that needs nothing
but the class histogram pays nothing and runs in a single pass; a query that
follows a 4-hop reference path pays for exactly that walk, over only the rows
that survive its filter.

## Non-Goals

- Full MAT OQL parity: correlated subqueries and non-homogeneous / nested `UNION`
  are rejected with a clear message. (Graph primitives `dominators()`,
  `inbounds()`/`outbounds()`, bounded `path()`, `AS RETAINED SET`, and homogeneous
  `UNION` *are* supported — see the coverage table.)
- A persistent query server / long-running daemon. The interactive `query`
  subcommand is a simple one-shot REPL over a single dump, not a service.
- Mutating or re-indexing the dump. Read-only.

## Supported OQL Subset (target)

The grammar tracks **Eclipse MAT OQL** so real MAT queries paste in unchanged
wherever the underlying data exists. MAT's own grammar is:

```
SELECT [ DISTINCT | AS RETAINED SET | OBJECTS ] <select-list>
FROM   [ INSTANCEOF | OBJECTS ] <class-spec> [ <alias> ]
[ WHERE <predicate> ]
[ UNION ( <query> ) ]
```

MAT has **no** `GROUP BY` / `ORDER BY` / `LIMIT` — those are our additive
extensions (flagged as such below), chosen because "top N by retained" is the
single most common heap question and MAT makes users do it in the UI instead.

**FROM / class-spec** (MAT-compatible)
- Exact class: `com.acme.Order`
- Array types: `char[]`, `byte[]`, `java.lang.Object[]` (first-class in MAT; the
  canonical "biggest arrays" queries use these)
- Subclass match: `FROM INSTANCEOF com.acme.AbstractJob` (MAT keyword form;
  matches subclasses too)
- Wildcard / regex class-spec: `com.acme.*`, `"com\.acme\..*Cache"` — **MAT
  treats a quoted class-spec as a Java regex.** We accept both a glob form
  (`com.acme.*`) and a regex form; see "Regex compatibility" in Dependencies.
- *(Extension, not MAT)* `FROM OBJECTS <addr>` and `FROM (<subquery>)` are
  recognized and **rejected** with a clear message (see rejected list).

**SELECT list** (MAT attribute names — this is a hard compatibility point)
- `*` (MAT default row) and `SELECT OBJECTS <expr>` (the whole object, not columns)
- Scalar/String field references and instance fields: `s.value`, `f.status`,
  `m.size` (collection sizes are plain field reads, exactly as in MAT)
- Path expressions (any depth, adaptive): `f.owner.department.name`
- **Built-in attributes, spelled as MAT spells them**:
  - `@objectId` — dense object id
  - `@objectAddress` — heap address (hex via `toHex(...)`)
  - `@usedHeapSize` — shallow size
  - `@retainedHeapSize` — retained size *(MAT's name; we accept `@retainedHeap`
    as an alias for ergonomics but echo the canonical name)*
  - `@displayName` — MAT-style label
  - `@length` — array length (`arr.@length`), a very common filter
  - `@clazz` / `classof(x)` — the object's class (runtime type)
  - `@GCRootInfo` — GC-root kind, when the object is a root
- Arithmetic on numeric expressions: `f.count * 8`, `@usedHeapSize + f.pad`
- `toString(x)` for String/CharSequence values; `toHex(@objectAddress)`
- *(Extension)* Aggregates with `GROUP BY`: `COUNT(*)`, `SUM(<expr>)`,
  `MIN/MAX/AVG(<expr>)`; and top-level aggregates without `GROUP BY` folding to a
  single row (`SELECT COUNT(*) FROM ...`, `SELECT SUM(@retainedHeapSize) FROM ...`)
- Column aliasing: `SELECT f.name AS owner`

**WHERE predicate** (MAT operators)
- Comparisons on scalars: `=,!=,<,<=,>,>=` against int/long/short/byte/
  char/float/double/boolean
- String matching: **`LIKE` is a Java regex in MAT**, e.g.
  `WHERE toString(s) LIKE ".*passwd.*"` — we implement `LIKE` as regex to match
  MAT (see "Regex compatibility"); `=`/`!=` are exact string compares
- Attribute predicates: `WHERE @retainedHeapSize > 1048576`,
  `WHERE s.@length > 100000`, `WHERE @usedHeapSize > 64` (retained predicates
  force a P3 stage)
- Path expressions in predicates, any depth: `WHERE f.owner.name = "root"`
- Boolean composition: `and`, `or`, `not`, parentheses (MAT keywords; we also
  accept `AND`/`OR`/`NOT`. Full precedence via Pratt)
- Set membership *(extension)*: `f.status IN ("OPEN", "PENDING")`
- `INSTANCEOF` test on a field's runtime type
- Null tests: `f.ref = null`, `f.ref != null`

**Class-spec grammar** applies uniformly to `FROM` and to `INSTANCEOF` operands.

**Explicitly rejected (clear error naming the MAT construct, not silent degrade)**
- **Unbounded `path(a, b)`**: a full shortest-path search over the whole object
  graph. Bounded-depth `path` (default small cap) is supported via edge retention;
  an unbounded/large-depth request is rejected with a message stating the depth cap
  and pointing to the path report.
- Non-homogeneous / nested `UNION`: branches whose column shapes differ, or
  `UNION` nested inside other clauses. Homogeneous `UNION` (matching column shapes)
  *is* supported (see below).
- Correlated subqueries, `SELECT ... FROM (<subquery>)`, `FROM OBJECTS <addr>` —
  recognized MAT forms, rejected as out of subset.
- `SELECT DISTINCT` across a projection that would require a second materialized
  set larger than the match-set cap (bounded-`DISTINCT` on a small projection is
  allowed).
- Joins between two independently-scanned classes (only reference-path traversal
  from a single FROM class is supported, not arbitrary N×M joins).

**Supported via surviving/retained graph data**
- `dominators(x)` — immediate dominees of `x` (dominator-tree children); reads the
  surviving `idom`/`dc_*` (see "Dominator-based primitives").
- `SELECT ... AS RETAINED SET` — the retained set of the matched objects.
- `x.@retainedHeapSize` and retained-ordered results (CrossPhase).
- `outbounds(x)` / `inbounds(x)` — out/in reference edges of `x`, served by
  **query-gated edge retention** (the planner keeps the needed CSR resident only
  when such a query is present; see that section).
- Bounded `path(a, b)` — a depth-capped reference path.
- **Homogeneous `UNION`** — `SELECT <cols> FROM A UNION (SELECT <cols> FROM B)`
  where the branches share a column shape: each branch is planned and executed as
  its own sub-plan, and their bounded rows are concatenated under one overall cap.

Each rejection is produced by the planner with the exact offending construct in
the message, so the user learns *why* and what to change.

## Coverage vs. real-world MAT OQL

The subset above is sized against the queries people actually run in MAT. Each
row is a common query pattern, its status here, and the strategy shape it lands
in:

| Common MAT query | Status | Shape |
|---|---|---|
| `SELECT * FROM java.lang.String s WHERE toString(s) LIKE ".*passwd.*"` | ✅ | SingleScan (String decode + regex) |
| `SELECT * FROM char[] s WHERE s.@length > 100000` | ✅ | SingleScan (array `@length`) |
| `SELECT * FROM byte[] b WHERE b.@usedHeapSize > 1048576` | ✅ | SingleScan (shallow attr) |
| `SELECT * FROM INSTANCEOF java.util.HashMap m WHERE m.size > 1000` | ✅ | SingleScan (INSTANCEOF + field) |
| `SELECT x, x.@retainedHeapSize FROM ... ORDER BY x.@retainedHeapSize DESC` (ext.) | ✅ | CrossPhase (match P1 → join P3) |
| `SELECT toString(s.name), s.@objectAddress FROM com.acme.Job s` | ✅ | SingleScan (String + attr projection) |
| `SELECT * FROM INSTANCEOF java.lang.ClassLoader k` | ✅ | SingleScan (class bitset) |
| `SELECT f.owner.name FROM com.acme.File f WHERE f.owner.name = "root"` | ✅ | RefWalk (1 hop) |
| `SELECT COUNT(*) FROM com.acme.Order` (ext.) | ✅ | HistogramOnly |
| `SELECT * FROM java.lang.String s WHERE s.@GCRootInfo != null` | ⚠️ partial | needs GC-root flags live at match time; served only if the query is planned onto a phase where root info is available, else rejected with a clear message |
| `SELECT dominators(s) FROM ... s` | ✅ | DominatorStage (P3, reads surviving `idom`/`dc_off`/`dc_tgt`) |
| `SELECT s AS RETAINED SET FROM ... s` | ✅ | RetainedSetStage (P3, dominator-subtree closure over matches) |
| `SELECT outbounds(s) FROM ... s` | ✅ (edge-retention) | bounded set: re-derives out-edges by a rescan (Lever 3), no forward CSR retained |
| `SELECT inbounds(s) FROM ... s` | ✅ (edge-retention) | keeps only matched rows' inbound adjacency, in compressed delta+vbyte form (Levers 1–2) |
| `SELECT path(a, b) FROM ...` | ⚠️ bounded | bounded depth over the pruned matched-class subgraph (Lever 4); depth-capped |
| `SELECT * FROM A UNION (SELECT * FROM B)` | ✅ (homogeneous) | each branch planned independently, rows concatenated; mixed-shape/nested rejected |

The former "raw-edge family" gap is closed on demand: `inbounds`/`outbounds`/
`path` are served by **query-gated edge retention** (next section) — the edge CSRs
are freed as before *unless* a query in the run actually needs them, in which case
the planner keeps exactly the CSR(s) required alive to report time. The only
remaining rejections are unbounded `path` searches and non-homogeneous/nested
`UNION`. Everything else in the common-query set is covered.

## Dominator-based primitives (`dominators()`, `AS RETAINED SET`)

The initial reject-everything-graph stance was too conservative. Reading the
actual pipeline (`main.rs::run`) shows which graph structures *survive* to report
time, and they are exactly the dominator ones:

| Structure | Built at | Freed at | Live at report build? |
|---|---|---|---|
| forward CSR (`fwd_offsets`/`fwd_targets`) | pass2 | moved into inbound transpose (~L849) | **no** |
| inbound CSR | transpose | consumed by dominator (~L883-884) | **no** |
| `g.idom` (immediate dominator per object) | ~L876 | *not freed* | **yes** |
| dominator-children CSR (`dc_off`/`dc_tgt`) | ~L894 | dropped after `build_model` (~L971) | **yes** (during build) |
| `g.retained[]` | ~L920 | end of pipeline | **yes** |

So the *raw edges* are gone (killing `inbounds`/`outbounds`/`path`), but the
**dominator tree is intact**. That is enough for the two primitives MAT users
actually want:

- **`dominators(x)`** = the dominator-tree children of `x` = the `dc_tgt` slice at
  `dc_off[x]`. A direct O(children) lookup, no graph walk.
- **`SELECT ... AS RETAINED SET`** = the union of the dominator *subtrees* rooted
  at the matched objects (MAT's definition of a set's retained set). A bounded DFS
  over `dc_off`/`dc_tgt` from each match, deduped via a visited bitset — the same
  traversal `compute_retained` already performs, reused read-only.

**Planner/executor integration.** Both become **P3/P4 stages** that consume the
match carry from P1 (`IndexOnly`) and read the surviving `idom`/`dc_*`/`retained`
structures — no new pipeline data, no new pass, just a read of state that is
already resident when the report is built:

- `DominatorChildrenStage` (for `dominators(x)`): for each carried match index,
  emit its `dc` children (bounded by the result cap).
- `RetainedSetStage` (for `AS RETAINED SET`): seed a work queue with the carried
  matches, DFS the dominator children, mark a visited bitset, emit/aggregate the
  closure (bounded; overflow sets `truncated`).

**Constraint — timing.** These stages must run while `dc_off`/`dc_tgt` are alive,
i.e. *inside or before* `build_model` (they are dropped just after). The executor
therefore hooks the query stages into the same window `build_model` uses, rather
than after it. The planner marks such a query `finalize_at = P3(dom)` and the
runner schedules the dominator stages in that window. `idom` and `retained[]`
alone survive even past that window, so `dominatorof(x)` (single immediate
dominator) and retained *sizes* are available report-wide; only the
subtree-expanding forms need the `dc_*` window.

**`UNION`** in its non-homogeneous/nested form stays rejected; the common
`AS RETAINED SET` idiom — a frequent reason for `UNION`-shaped MAT queries — is
served directly, and homogeneous `UNION` is supported by branch concatenation
(see "Homogeneous UNION").

This is a genuine capability win: it moves `dominators()` and `AS RETAINED SET`
from "rejected" to "supported" purely by *reading data the pipeline already keeps
resident at report time*, with zero extra memory or passes.

## Query-gated edge retention (`inbounds()`, `outbounds()`, bounded `path()`)

The reference CSRs are freed mid-pipeline **for a memory reason, not a logical
one**: `main.rs` frees the forward CSR inside `build_from_fwd` and drops the
inbound CSR right after `compute_dominators`, with comments citing a ~7.5 GB CSR
and a ~22 GB global RSS peak. The naive fix — "keep the whole CSR when a query
needs edges" — reintroduces exactly that multi-GB array and is unacceptable. So
edge primitives are served by **retaining as little of the graph as the query can
actually touch, in the compressed form the pipeline already produces**, driven by
four levers. The common case (no edge query) pays nothing; an edge query pays for
a pruned, compressed slice rather than the full CSR.

**Lever 1 — retain rows, not the graph (class-index row pruning).** The planner
already knows the `FROM` class spec, so it knows the *matched-class row set*
before pass2. Only the CSR rows whose source node is in that set can ever be read
by `inbounds`/`outbounds` on the query alias. The planner emits a
`retain_rows: ClassBitset` alongside the retain flag; the pipeline keeps only the
adjacency of matched rows and discards the rest as the CSR is walked. For the
typical single-class `FROM com.example.Foo`, the retained fraction is
(instances-of-Foo / all-nodes) — usually a low-single-digit percent, not 100 %.

**Lever 2 — keep the existing delta+vbyte block encoding (no flat expansion).**
`inb_data` is *already* delta-encoded and vbyte-compressed (~4×, `src/vbyte.rs`,
"output must not change") and lives in freeable 256 MB chunks (`ChunkU32`,
`src/chunkvec.rs`). Retention keeps those exact compressed blocks for the pruned
rows; it does **not** expand them into a flat `Vec<u32>`. At query time each
`inbounds(x)` decodes one row's block on demand via `vbyte::decode_delta` — cheap,
O(degree), and the retained footprint stays at the pipeline's ~4×-compressed size.
The forward CSR, when retained, reuses the same block-delta-vbyte scheme rather
than a flat array.

**Lever 3 — re-derive `outbounds` by a scan instead of retaining the forward
CSR.** The forward CSR is the heavier of the two and is consumed earliest. For an
`outbounds` query over a *bounded matched set*, we do not retain it at all:
instead the runner performs one extra P1-style resolve-scan over just the matched
objects' field regions (reusing the `AddrFrontier` batched-resolve mechanism
already in the spec), emitting each object's outbound targets directly into the
bounded accumulator. This trades a small, bounded second scan for eliminating the
single largest retained array. Full-forward retention is only used as a fallback
when the matched set is not bounded (which the planner otherwise rejects).

**Lever 4 — matched-class subgraph only for `path`.** Bounded `path(a, b)` never
retains a whole CSR. It retains only the pruned subgraph induced by the
matched-class row set (via Lever 1, compressed via Lever 2), and runs a capped BFS
(`--query-path-depth`, default 5) with a frontier cap like a RefWalk. Both `a` and
`b` must resolve within the retained subgraph; an unbounded or unrooted request is
rejected up front naming the cap.

**Cost model, made explicit.** Peak RSS added by an edge query is
`retained_fraction × compressed_CSR_size`, not the full ~7.5 GB. For a
single-class edge query on a large dump that is typically tens to low-hundreds of
MB, and `outbounds` (Lever 3) adds essentially nothing beyond one bounded rescan.
This is:
- **opt-in** — incurred only when an `inbounds`/`outbounds`/`path` query is present;
- **pruned + compressed** — Levers 1–2 bound the retained set to matched rows in
  the pipeline's own ~4× encoding; Lever 3 avoids forward retention entirely for
  the bounded case;
- **surfaced** — `!plan`/`!explain` and a one-line note on the query result state
  "retained ~N MB of the <forward|inbound> graph (M% of rows, compressed)";
- **capped** — result frontiers/rows are bounded independently of retention.

Because the flags *and* the row bitset are computed pre-pass2 from the AST + P0
schema, a run with no edge queries is byte-for-byte and RSS-for-RSS identical to
today. This is the same "adaptive: pay only for what the query touches" principle
as the phase planner, sharpened from "which arrays" to "which *rows*, in which
*encoding*".

The planner surface reflects this: `RunFlags` carries not just booleans but the
pruning set —

```rust
pub struct RunFlags {
    pub retain_forward: bool,          // fallback: unbounded outbounds/path
    pub retain_inbound: bool,          // inbounds present
    pub retain_rows: Option<ClassBitset>, // Lever 1: only these source rows
    pub outbounds_by_rescan: bool,     // Lever 3: bounded outbounds, no fwd retain
}
```

## Homogeneous UNION

`SELECT <cols> FROM A UNION (SELECT <cols> FROM B [UNION (...)])` is supported
when every branch projects the **same column shape** (same arity and compatible
types). Each branch is parsed and planned into its own sub-plan (with its own
stages/carries), the executor runs them and **concatenates** their bounded rows
into one `QueryResult`, applying a single overall row cap across branches.
Branches may individually use different strategies (one `SingleScan`, one
`RefWalk`) — the union is just row concatenation at the end. Non-homogeneous
column shapes and `UNION` nested inside other clauses are rejected with a message
naming the mismatch. This covers the common "objects of type A *or* type B"
idiom without the complexity of full column-type reconciliation.

## Architecture

Six components, each independently testable:

```
query::parse      OQL text            -> Query AST
query::plan       Query AST + schema  -> QueryPlan (staged: which phases, carries, caps, depth)
query::execute    QueryPlan + dump    -> QueryResult (bounded rows/aggregates)
query::model      QueryResult         -> serde structs on the Report
query::repl       dump + stdin        -> interactive queries + !plan/!explain
report renderers  QueryResult         -> md / html / json sections
```

### 1. Parser (`src/query/parse.rs`)

**Decision: hand-written recursive-descent + Pratt expression parser. No parser
library.** See "Parser choice" below for the evaluated alternatives. Produces a
`Query` AST: `select`, `from`, `where`, `group_by`, `order_by`, `limit`, and an
optional `union` tail (a boxed `Query` per homogeneous branch). Graph functions
(`dominators`/`inbounds`/`outbounds`/`path`) and `AS RETAINED SET` parse to
dedicated AST nodes so the planner can classify them. A small hand-rolled
tokenizer feeds a precedence-climbing (Pratt) parser for the WHERE expression
(AND/OR/NOT, comparisons, `LIKE`, regex, `INSTANCEOF`, dotted paths). Path
expressions parse to a `Vec<PathSegment>` (field-name hops). Every token carries
its source column so errors read `expected <X> near column N`. Constructs the
planner cannot serve parse successfully but are rejected at *plan* time with a
specific message (so the error names the semantic limit, not a syntax surprise).

### 2. Planner (`src/query/plan.rs`) — the adaptive core

This is where "be adaptive; analyze the query" lives. The planner is a pure
function (AST + schema → plan) and therefore fully unit-testable without a dump.
It does three things: derive the query's data requirements, bind each requirement
to the *earliest* pipeline phase where that data is live, and compile a
**multi-phase plan** — an ordered set of per-phase *stages* connected by
*compressed carry buffers* — rather than picking one of a few hardcoded
strategies.

A "proper planner" here means the plan is a first-class, inspectable structure,
not an opaque enum branch:

```rust
pub struct QueryPlan {
    pub stages: Vec<Stage>,       // one per pipeline phase this query touches,
                                  // in pipeline order (P0..P4)
    pub carries: Vec<CarrySpec>,  // data handed from stage[i] to a later stage,
                                  // with its compressed layout + cap
    pub caps: Caps,               // every bound the plan will enforce
    pub projection: Projection,   // final column layout + how each column is sourced
    pub union_branches: Vec<QueryPlan>, // homogeneous-UNION sub-plans; rows
                                  // concatenated under the shared cap (empty = no UNION)
    pub rejection: Option<String>,// unsupported construct, named
}

// Run-level flags, unioned across ALL queries in the run and consumed BEFORE
// pass2 so the pipeline can decide what to keep resident (see edge retention).
// Retention is row-pruned (Lever 1) and stays in the pipeline's compressed
// delta+vbyte block form (Lever 2); bounded outbounds avoids forward retention
// via a rescan (Lever 3).
pub struct RunFlags {
    pub retain_inbound: bool,             // some query needs inbounds / path
    pub retain_forward: bool,             // fallback only: unbounded outbounds/path
    pub retain_rows: Option<ClassBitset>, // Lever 1: keep adjacency of these source rows only
    pub outbounds_by_rescan: bool,        // Lever 3: bounded outbounds -> extra P1-style scan, no fwd retain
}

pub struct Stage {
    pub phase: Phase,             // P0 | P1 | P2 | P3 | P4
    pub reads: Vec<Requirement>,  // minimal fields/edges/attrs read at this phase
    pub op: StageOp,              // Match | ResolveHop | JoinRetained | DominatorChildren
                                  // | RetainedSet | EdgeLookup | BoundedPath | Aggregate | Finalize
    pub produces: Option<CarryId>,// the carry this stage fills (if any)
    pub consumes: Vec<CarryId>,   // carries this stage reads
}
```

The old "strategy" names (`HistogramOnly`, `SingleScan`, `RefWalk`,
`CrossPhase`) survive only as *derived labels* for the shape of the stage list —
useful in tests and diagnostics — but the executor is driven by the stages
themselves. This generalizes cleanly: a query that filters fields (P1), follows a
ref hop (P2), *and* orders by retained (P3) is just a three-stage plan with two
carries; there is no bespoke "P1→P3" special case to special-case again.

#### Pipeline data-liveness map (the substrate adaptivity plans against)

The analyzer's phases each hold different data live. The planner is built around
this exact map (verified against `main.rs::run` + `pass2::build`):

| Phase | Live data a query can read | Freed after |
|---|---|---|
| **P0 schema** (pass1 done) | class table: names, superclass chain, per-field `(name, type)`; loader labels | — (schema stays cheap) |
| **P1 field-decode scan** (in `pass2::build`, `scan_all_records`) | every object's raw blob + `class_id`; `IdMap` (addr→index) live; field offsets decodable; String-decode machinery live | `id_map`/field plans freed at end of pass2 |
| **P2 forward CSR** (post-pass2, pre-inbound) | `fwd_offsets`/`fwd_targets` (out-edges per object) + reachability `dfn` | moved into inbound transpose, then freed — bounded `outbounds` re-derives edges by rescan (Lever 3) rather than retaining this; full retention only for the unbounded fallback |
| **P3 dominator/retained** (late, in `main.rs`) | `idom`, `retained[]`, `shallow[]`, `class_idx[]` (restored); dominator-children CSR `dc_off`/`dc_tgt` (live *during* `build_model`); *pruned+compressed* inbound-CSR rows *if `retain_inbound` set* (matched rows only, delta+vbyte); forward CSR *only if the unbounded fallback is taken* | `dc_*` dropped post-`build_model`; retained edge rows dropped after the query window; `idom`/`retained` at end of pipeline |
| **P4 histogram** (build_model) | per-class aggregates: instances/shallow/retained | report phase |

The two load-bearing consequences the planner must encode:

1. **`@retainedHeapSize` and `@objectId`-dominator data do not exist during the field
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
Retained                   any use of @retainedHeapSize (P3-only)
RuntimeType                @clazz / classof / INSTANCEOF on a value's *runtime* class
DominatorTree { mode }     dominators(x) (children) or AS RETAINED SET (subtree
                           closure) — reads surviving idom/dc_*/retained at P3
Edges { dir, path }        outbounds/inbounds/bounded-path. Sets retain_rows to
                           the matched-class bitset (Lever 1) so only those source
                           rows' compressed adjacency is kept (Lever 2). inbounds
                           -> retain_inbound; bounded outbounds -> outbounds_by_rescan
                           (Lever 3, no forward retention); path -> pruned subgraph
                           only (Lever 4). Reads at P3.
```

Beyond the per-query `QueryNeeds`, the planner emits **run-level pipeline flags**
consumed *before pass2*: `retain_inbound` / `retain_forward` / `retain_rows` /
`outbounds_by_rescan` (set if any query needs `outbounds`/`inbounds`/`path`) tell
the pipeline *which rows* of *which* reference CSR to keep, in compressed form,
and when to prefer a bounded rescan over retention (see "Query-gated edge
retention"). These are unioned across all queries in the run, so a single edge
query in a pack retains the pruned graph for that run and no other.

The planner unions these across SELECT, WHERE, GROUP BY, and ORDER BY. Each
requirement carries the **minimal field/edge set** it touches — never "all fields
of the class", only the ones the query names. That minimality is the whole point:
a query over one `int` field decodes one `int`.

#### Binding requirements to phases — staging

For each `Requirement`, the planner looks up the **earliest** phase in the
liveness map that can supply it, and appends its read to that phase's `Stage`.
Requirements that only *project* (never gate a match) are deferred to the latest
phase that still has the survivors, so their reads run over the pruned set, not
the whole class. This produces the ordered stage list directly from `needs`; the
"strategy" is just the observed shape:

- reads ⊆ {Histogram} → single P4 stage (derived label `HistogramOnly`).
- reads ⊆ P1 field/type data, nothing later → single P1 stage (`SingleScan`).
- any `RefPath` hop → adds P1 (frontier collection) + P2/P1 resolve stages
  (`RefWalk`); each hop is tagged predicate-critical (resolve for all candidates)
  or projection-only (resolve after WHERE+LIMIT prunes survivors).
- any `Retained` use → adds a P3 stage that consumes the carry from the P1 match
  stage and joins it against `retained[]`/`idom` (`CrossPhase`).

Nothing is hardcoded to two phases: a query needing P1 + P2 + P3 emits three
stages and two carries, composed the same way as any two-phase plan.

#### Carries + compression — spanning phases without holding the heap

A stage that must feed a *later* stage cannot keep the full per-object data alive
across the intervening teardown (that would defeat the streaming design). Instead
each cross-phase hand-off is an explicit **carry buffer** with a **compressed,
phase-appropriate layout** the planner chooses up front — mirroring the codebase's
existing "compress cold data, free early" discipline:

```rust
pub struct CarrySpec {
    pub id: CarryId,
    pub from: Phase, pub to: Phase,
    pub layout: CarryLayout,   // how each retained record is packed
    pub cap: usize,            // max records before `truncated` trips
}

pub enum CarryLayout {
    // just the dense object index — 4/8 bytes each. For "match here, look up
    // an attribute (retained) later" joins. Sorted+delta-varint on flush so a
    // monotonic index set costs ~1-2 bytes/entry.
    IndexOnly,
    // index + a small fixed tuple of already-decoded scalars, bit-packed to the
    // field widths the query actually named (an i32 field carries as 4 bytes,
    // a bool as 1 bit). For "decode cheap fields at P1, combine with retained
    // at P3".
    IndexPlusScalars { widths: Vec<ScalarWidth> },
    // target addresses to resolve at the next phase, deduped into a sorted set
    // (many candidates point at the same referent) then delta-varint encoded.
    // For ref-hop frontiers.
    AddrFrontier,
}
```

The planner picks the layout from what actually crosses the boundary, so the
carry holds the *minimum* needed to finish, in its most compact form:

- **Index-only** for the common "filter on fields at P1, order/print retained at
  P3" query — the carry is a sorted `Vec<u32>` of matched indices, delta+varint
  encoded on flush. Millions of matches compress to a few MB; the cap still bounds
  it, but compression pushes the truncation point far out so most real queries are
  exact.
- **Index + packed scalars** when the query also projects a P1-decoded field
  alongside a P3 attribute — the scalars ride along bit-packed to their declared
  widths, not as tagged `QueryValue`s.
- **Address-frontier** for ref hops resolved at a later phase — deduped (collapsing
  fan-in) then delta-varint encoded, which is where most of the shrink comes from
  on real object graphs.

Carries are write-once at their producing stage and read-once (streamed, decoded
lazily) at the consuming stage, so peak extra RSS is one compressed buffer, not
the decoded rows. Every carry is capped; overflow sets `truncated` and the result
becomes a bounded sample rather than growing unbounded.

`caps` collects every bound the plan will apply (per-carry caps, per-hop frontier
cap, group cap, top-N). `rejection` is `Some(msg)` when a requirement is
unsupported (graph primitives, multi-hop *aggregation* joins, etc.), naming the
exact construct.

Because staging and carry-layout selection are pure functions of `needs`, the
planner has a small finite decision surface — every branch of which is a unit
test (see Testing).

#### Intelligent cost gating — you pay only for the features you use

Cost is **per-need**, not per-tier: each `QueryNeeds` flag independently arms
exactly one piece of machinery, and an unset flag arms nothing. There is no
baseline "query tax". The mapping is one-to-one and additive:

| A query that only… | Arms | Leaves untouched |
|---|---|---|
| aggregates by class (`Histogram`) | one P4 read over per-class stats that already exist for the report | field decode, carries, CSR retention, dominator window |
| filters/reads scalar fields (`InstanceScalar`) | one P1 predicate over named field offsets | String decode, ref resolution, retained/dominator, edges |
| reads String fields (`InstanceString`) | String backing-array decode **for referenced fields only** | ref hops, retained, edges |
| follows ref hops (`RefPath`) | P1 frontier collect + P2/P1 resolve, `AddrFrontier` carry | retained/dominator, edge retention |
| uses `@retainedHeapSize` (`Retained`) | a P3 join against the already-resident `retained[]` | edge retention, forward scan |
| uses `dominators()`/`AS RETAINED SET` | a P3 stage in the surviving `dc_*` window | edge retention |
| uses `inbounds/outbounds/path` (`Edges`) | **only** the row-pruned, compressed retention (Levers 1–4) | nothing beyond the matched rows |

The load-bearing property: a `Histogram`-only query never touches field decode,
never allocates a carry, never keeps a CSR resident, never opens the dominator
window — its cost is a single pass over aggregates the report already computes.
Each heavier feature layers its own cost on top *only when its flag is set*.
Because the flags are unioned across the run **before pass2**, a run containing no
edge/dominator/retained query is byte-for-byte and RSS-for-RSS identical to today.
`!plan`/`!explain` prints the armed needs and their cost so the trade is visible
before running.

#### Fast execution — latency scales with what the query touches, not the dump

Four mechanisms keep per-query wall-clock bounded by the query's own footprint,
independent of total heap size:

1. **Early WHERE/LIMIT pruning.** WHERE predicates and the row `LIMIT` are applied
   *during* the P1 match, so every later stage carries only survivors. Projection
   work that is not needed to decide a match — resolving a referent, decoding a
   `@displayName`, joining `@retainedHeapSize` — is deferred to run *after* pruning,
   over the bounded survivor set, never the whole class. (This is the
   predicate-critical vs projection-only split already threaded through `RefPath`.)

2. **Single-pass when nothing crosses phases.** If `needs` fits within one phase
   (`HistogramOnly`, or a `SingleScan` field filter with no retained/edge/dominator
   projection), the plan emits **one stage, zero carries** and finishes in that
   phase's existing traversal — no buffer flush, no second scan, no P3 window.

3. **Short-circuit cheapest predicates first.** The planner orders WHERE conjuncts
   by ascending cost — class-index / `INSTANCEOF` and scalar compares first, then
   String decode, then ref resolution, then `LIKE` regex — so a row that fails a
   cheap test never pays for the expensive ones. Expensive operands are evaluated
   lazily per-row behind the cheap guards.

4. **Bounded work everywhere.** Every frontier, carry, group table, and result set
   has a cap (`caps`); total work is bounded by `caps × max_degree`, never by heap
   size. Hitting a cap sets `truncated` and returns a bounded top-N rather than
   silently doing unbounded work — the one place a result is a sample, made honest
   by the flag.

Together these mean the *common* interactive query — a field filter with a LIMIT —
runs in a single bounded P1 pass with no carries, and even a cross-phase query only
carries (compressed) the rows that survived WHERE+LIMIT.



The executor is a **stage runner**, not a set of per-strategy drivers. It holds
the plan's carry buffers and, at each pipeline phase, runs whatever `Stage`s the
plan bound to that phase — reading the carries they consume and filling the carry
they produce. Because the plan is an ordered stage list with explicit carries,
the same runner handles a one-stage histogram query and a four-stage
P1→P2→P3→finalize query with no special cases. The bounded accumulators (top-N
heap, grouped aggregators) and the compressed-carry codec are shared by all
stages.

Stage operators:

- **`Aggregate` at P4 (`HistogramOnly` shape)**: fold the in-memory per-class
  histogram through the aggregators. No dump access.
- **`Match` at P1 (`SingleScan` shape)**: register a `(predicate, projector)` on
  the P1 field-decode `scan_all_records` callback. Per matched instance: decode
  only the named scalar fields (offset read) and String fields (via the existing
  String→backing-array decode), evaluate WHERE, and on pass either project into
  the accumulator (if this is the terminal stage) or append to the produced carry
  in its compressed layout. Class matching (`INSTANCEOF`, wildcard) is precompiled
  to a class-index bitset at P0 so the hot loop does an O(1) membership test.
- **`ResolveHop` (`RefWalk` shape)**: adaptive, breadth-first over hops,
  HPROF-order-safe (HPROF gives no record ordering, so referents are resolved in a
  *later* batch, never inline).
  - **Frontier collection (P1):** while scanning, for each candidate object,
    append `(candidate_index, first_hop_target_addr)` to the hop's `AddrFrontier`
    carry — deduped and delta-varint encoded on flush, bounded by the frontier cap.
  - **Hop resolution:** two mechanisms, the planner picks the cheaper per hop and
    encodes the choice in the stage:
    (a) **CSR hop** — when the needed edge is an object-reference field and the
    forward CSR is still live (P2), follow `fwd_targets`; addr→index via the live
    map. (b) **Batched resolve-scan** — decode the frontier's deduped target set in
    one more P1-style pass. One extra scan per resolve *batch*, not per object.
  - **Predicate-critical vs projection-only hops:** predicate hops resolve for the
    whole frontier (needed to filter); projection-only hops resolve *after* WHERE +
    LIMIT have pruned the survivors, so deep SELECT paths cost O(surviving rows),
    not O(class). This is the sharpest adaptivity lever.
  - Every hop frontier is capped; hitting a cap sets `truncated`.
- **`JoinRetained` at P3 (`CrossPhase` shape)**: stream the carry produced by an
  earlier `Match`/`ResolveHop` stage (decoding the compressed layout lazily), and
  for each carried index look up `retained[]`/`idom`. Complete the projection
  (combining any packed-scalar payload from the carry with the freshly-read
  retained size), feed the accumulator. Because the carry is index-only or
  index+packed-scalars, this stage never re-reads object blobs — the blob
  machinery is already gone by P3, which is exactly why the needed scalars were
  compressed into the carry at P1.
- **`DominatorChildren` at P3 (`dominators(x)`)**: consume the P1 match carry; for
  each carried index, read its dominator-tree children from the surviving
  `dc_off`/`dc_tgt` and emit them (with any requested attribute like retained
  size). O(children) per match, result-capped. Must run in the `build_model`
  window where `dc_*` are alive.
- **`RetainedSet` at P3 (`AS RETAINED SET`)**: seed a work queue with the carried
  matches, DFS the dominator children over `dc_off`/`dc_tgt` marking a visited
  bitset, and emit/aggregate the deduped closure — MAT's retained-set-of-a-set.
  Bounded by the result cap; overflow sets `truncated`. Also runs in the `dc_*`
  window.
- **`EdgeLookup` at P3 (`outbounds(x)`/`inbounds(x)`)**: consume the P1 match
  carry; for each carried index, read its out-edges (retained forward CSR) or
  in-edges (retained inbound CSR) and emit the referents. O(degree) per match,
  result-capped. Requires the corresponding `retain_*` flag was set pre-pass2.
- **`BoundedPath` at P3 (`path(a, b)`)**: capped BFS over the retained CSR from
  `a` toward `b`, depth-limited (`--query-path-depth`) and frontier-capped; emits
  the first path found (or rows up to the cap). Requires both CSRs retained.
- **`Finalize`**: apply ORDER BY / LIMIT to the accumulator and emit
  `QueryResult` rows. For a `UNION`, run each `union_branches` sub-plan through the
  same runner and concatenate their finalized rows under one shared cap before
  emitting.

All result buffers and carry buffers are explicitly bounded and stored compressed
(see the planner's `CarryLayout`), so RSS stays within budget regardless of dump
size — matching every other analysis in the codebase. When a query needs no
dump-scan data at all (`HistogramOnly` shape), no scan is added; when it needs
only P1 data, it rides the always-on field-decode scan with zero extra passes;
only ref-walks and cross-phase joins add work, and only in proportion to what the
query touches. Spanning three or more phases is just three or more stages chained
by compressed carries — the same machinery, not a new code path.

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

### 6. Interactive `query` subcommand (`src/query/repl.rs`)

A small REPL for authoring and testing queries against a dump without
regenerating a full report — the fast feedback loop while writing OQL.

```
hprof-analyzer query <dump.hprof>
```

On start it runs the pipeline once up to the point where the queried data can be
served, then reads lines from stdin. Because re-running the pipeline per query is
expensive, the REPL keeps the analyzed state resident for the session (this is the
one place we deliberately hold more than the streaming path — acceptable for an
interactive, user-invoked session, not the batch report path). A plain line is
treated as an OQL query, parsed → planned → executed, and its result table is
printed. Lines beginning with `!` are meta-commands:

- `!plan <query>` — parse + plan only; pretty-print the `QueryPlan`: the derived
  `QueryNeeds`, the ordered stage list (phase + `StageOp` + reads), the carries
  (`CarryLayout` + cap) and the derived shape label. This is the insight tool —
  it shows *how* a query would run without running it.
- `!explain <query>` — run the query, then print the plan **plus** actuals: rows
  matched, whether `truncated` tripped, per-stage/carry counts.
- `!schema [<class-glob>]` — list matching classes and their queryable fields
  (name + type), so the user knows what to filter on.
- `!help` — list commands. `!quit` / Ctrl-D — exit.

Kept intentionally simple: line-based stdin (no readline/history dependency), no
new crates, output reuses the Markdown table renderer. It shares the exact
`parse`/`plan`/`execute`/`model` code the report path uses, so a query that works
in the REPL works identically in a report — the REPL is a thin front-end, not a
second engine.

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
Benefit: no new *parsing* dependency, instant compiles, and exactly the
diagnostics we want (the one dep this feature adds is `regex`, for MAT-compatible
`LIKE`/class-spec matching — see Dependencies). This aligns with the project's
lean-dependency posture (see `Cargo.toml`).

## Dependencies

- **No new runtime dependency for parsing** (hand-written, per above).
- **Regex compatibility (`LIKE` and class-specs)**: this is a real MAT-fidelity
  decision, not a nicety. In MAT, `LIKE` takes a **Java regular expression**
  (`toString(s) LIKE ".*passwd.*"`), and a quoted class-spec is also a Java
  regex. So `LIKE` cannot be a SQL-style glob (`%`/`_`) — that would silently
  mismatch every pasted MAT query. The project currently has **no `regex`
  crate**. Options:
  - **(Chosen) add the `regex` crate**, scoped to the query executor. `LIKE`,
    `=~`, and quoted regex class-specs compile to `regex::Regex` with MAT
    semantics (unanchored `find`, as MAT uses `Matcher.find()`). This is the only
    way to be *actually* MAT-compatible on the single most common query family
    (String content search). `regex` is pure-Rust, widely used, and pulls
    `aho-corasick`/`memchr` (both already common transitively); the cost is
    justified by correctness on the flagship use case. It is feature-gateable if
    binary size ever matters.
  - The convenience glob class-spec (`com.acme.*`) is still handled by a tiny
    hand-rolled matcher (no regex needed for the common `pkg.*` case); only true
    regex forms hit the `regex` crate.
  Bumping `regex` into the runtime deps is the one dependency change this feature
  makes; it is confined to `src/query/` and does not touch the hot analysis path.
- **Testing** reuses the existing `proptest` dev-dependency for parser
  round-trip/fuzz tests (already in `Cargo.toml`). `toml` is already present
  (`default-features = false, features = ["parse"]`), exactly what `[[query]]`
  packs need — no change required.

## Input Surfaces

Three, per the requirement:

- **CLI**: `--query 'SELECT ...'` (repeatable). Ad-hoc. Title auto-assigned
  (`query 1`, `query 2`) unless the query provides one.
- **TOML**: a `[[query]]` array-of-tables in the existing config file
  (`.hprof-analyzer.toml` / `$HOME/.config/hprof-analyzer/...`), each with
  `name` and `oql`. Saved reusable query packs. Merged with any `--query` flags.
- **Interactive `query` subcommand**: `hprof-analyzer query <dump.hprof>` — a
  line-based REPL (see component 6) for authoring/testing queries and inspecting
  their plans via `!plan` / `!explain`. Shares the report path's engine.
- **`--query-match-cap <n>`** (optional): overrides the default CrossPhase/RefWalk
  match-set and per-hop frontier cap for users who knowingly want larger (or
  smaller) bounded results. Defaults to a safe value (~200k).
- **`--query-path-depth <n>`** (optional): max depth for bounded `path(a, b)`
  queries. Defaults to a small value (e.g. 5); larger values raise BFS cost.

Batch queries (`--query`/TOML) only run in the analyze path (hprof → report),
never on JSON re-render (there is no dump to scan then). Re-rendering a saved
report shows the stored `queries` results as-is. The `query` subcommand always
needs a dump argument for the same reason.

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
   decision surface is exhaustively testable. For a fixed synthetic schema, assert
   the derived `QueryNeeds`, the compiled **stage list** (which phases, in order),
   the **carries** (which `CarryLayout` + cap for each hand-off), path depth,
   per-hop predicate-critical/projection-only classification, and caps for a
   matrix of queries:
   - `HistogramOnly` shape (single P4 stage, no carries): `SELECT COUNT(*)`,
     `SUM(@retainedHeapSize) GROUP BY class`.
   - `SingleScan` shape (single P1 stage): scalar-only filter; String-only filter;
     `@usedHeapSize` predicate; `@clazz`/`INSTANCEOF`; array `@length`; `IN (...)`;
     `LIKE` regex on a String field; arithmetic projection.
   - `RefWalk` shape: 1-hop predicate (assert an `AddrFrontier` carry); 3-hop
     predicate; deep **projection-only** path over a filtered set (assert deep hops
     are marked projection-only so they resolve lazily).
   - `CrossPhase` shape: field filter + `ORDER BY @retainedHeapSize LIMIT n`
     (assert a P1 `Match` stage → `IndexOnly` carry → P3 `JoinRetained` stage);
     field filter that also projects a P1 scalar + `SELECT @retainedHeapSize`
     (assert the carry is `IndexPlusScalars` with the correct packed widths).
   - **Three-phase** span: field filter (P1) + ref hop (P2) + `ORDER BY
     @retainedHeapSize` (P3) — assert three stages and two carries chain
     correctly, proving no two-phase special-casing.
   - **Dominator primitives**: `SELECT dominators(s) FROM ...` (assert a P1
     `Match` → `IndexOnly` carry → P3 `DominatorChildren` stage marked to run in
     the `dc_*` window); `SELECT s AS RETAINED SET FROM ...` (assert `RetainedSet`
     stage).
   - **Edge primitives + run flags**: `SELECT outbounds(s) FROM ...` (assert
     `outbounds_by_rescan` set and `retain_forward` **false** — Lever 3 avoids
     forward retention; `EdgeLookup` stage present); `inbounds(s)` (assert
     `retain_inbound` set *and* `retain_rows` populated with the matched-class
     bitset — Lever 1); bounded `path(a,b)` (assert pruned-subgraph retention +
     `BoundedPath` stage + depth cap); assert *unbounded* `path` is rejected, and
     a run with **no** edge query leaves all retain flags false / `retain_rows`
     `None` (proving the common case is byte/RSS-identical).
   - **Homogeneous UNION**: `SELECT * FROM A UNION (SELECT * FROM B)` (assert two
     `union_branches` sub-plans, shared cap); assert a mixed-column-shape UNION is
     rejected naming the mismatch.
   - Every **rejection** case, asserting the message names the construct.
   The mapping `needs → stages/carries` is a finite table; there is a test per cell.
   - **Cost gating** (per-need, additive): a `Histogram`-only query arms *no* carry,
     *no* CSR retention, *no* dominator window, *no* field decode (assert one P4
     stage and all retain flags false / `needs` minimal); a scalar field filter
     arms P1 decode but leaves String-decode, ref, retained, and edge machinery
     off. One assertion per row of the cost-gating table, proving each feature's
     cost is armed *only* by its own flag.
   - **Fast-path shape**: a `SELECT ... FROM C WHERE <scalar> LIMIT n` plans to a
     **single P1 stage with zero carries** (assert `stages.len() == 1`,
     `carries.is_empty()`), and the LIMIT/WHERE are marked applied *in* that scan.
     Assert that a projection-only referent/`@displayName`/`@retainedHeapSize` is
     tagged to resolve after WHERE+LIMIT, not before.
   - **Predicate ordering**: a WHERE mixing a class/scalar test with a `LIKE` regex
     and a ref-hop test plans the conjuncts cheap-first (assert the evaluation order:
     class-index/scalar → String decode → ref resolve → regex).
3. **Executor tests on a tiny hand-built dump** — construct a minimal in-memory
   graph fixture (a few classes with known scalar/String/ref fields and known
   sizes) and assert exact query results: counts, sums, top-N ordering, path
   traversal correctness, null handling, truncation flags. Include a multi-stage
   query (field filter → ref hop → order by retained) to exercise carry chaining.
3a. **Carry codec unit tests** — round-trip each `CarryLayout` (index-only
   delta-varint, index+packed-scalars at each width, deduped address frontier):
   encode → decode yields the exact records; assert the encoded size for a
   monotonic/duplicate-heavy input is a small fraction of the naive size (the
   compression claim is tested, not assumed); assert cap overflow trips
   `truncated` deterministically.
4. **End-to-end CLI tests** (`tests/`) — run the binary against the existing
   benchmark fixtures with representative `--query` flags and TOML packs; assert
   the rendered md/json contains the expected sections/rows. Add golden fixtures.
   Also drive the `query` subcommand by piping stdin (`echo "SELECT ...\n!plan SELECT ..." | hprof-analyzer query <dump>`) and assert the query table and the
   `!plan` output (stage list + carries) appear as expected.
5. **Determinism** — same query + same dump ⇒ byte-identical output (stable
   sort, stable group order). Covered by the golden fixtures.
6. **Bounds** — a query with no LIMIT on a large fixture must not blow the JSON
   size budget or RSS; assert `truncated` is set and caps hold.
7. **MAT differential oracle** (opt-in, `#[ignore]` by default) — since the whole
   point is MAT compatibility, use *real Eclipse MAT as the oracle* for the subset
   we claim to match. MAT ships a headless batch mode
   (`ParseHeapDump.sh <dump> -command "oql <query>"`, or the
   `org.eclipse.mat.api:query` app) that runs an OQL query and writes CSV. The
   harness:
   - Runs a curated list of subset queries through **both** MAT headless and our
     analyzer against the *same* fixture dump (the existing benchmark `.hprof`s).
   - Compares results as **sets/multisets of object addresses** (`@objectAddress`),
     not row order or formatting — MAT and we differ on ordering, labels, and
     column rendering, but the *matched object set* must be identical for a
     faithful subset query. For aggregate queries (`COUNT`/`SUM`), compares the
     scalar.
   - Normalizes the known-legitimate divergences up front: our extension clauses
     (`ORDER BY`/`LIMIT`/`GROUP BY`) are stripped before handing the query to MAT
     (MAT lacks them); `@retainedHeap` is rewritten to `@retainedHeapSize`; a
     query that hit our cap (`truncated`) is compared only as "our set ⊆ MAT set".
   - Is **gated behind an env var + feature** (`MAT_HOME` present and
     `--features mat-oracle`) so normal `cargo test` and CI never require a JVM or
     the MAT install. A small script (`scripts/mat-oracle.sh`) documents obtaining
     MAT headless. When `MAT_HOME` is unset the tests `eprintln!` a skip notice and
     pass, so the suite is green without MAT but *runnable* with it.
   - Doubles as a **coverage-table verifier**: every ✅ row in "Coverage vs.
     real-world MAT OQL" has a corresponding oracle case, so the compatibility
     claims in this spec are executable, not aspirational. Rejected (❌) queries
     assert *we* reject while MAT accepts — documenting the boundary, not a bug.
   - Dominator primitives (`dominators()`, `AS RETAINED SET`) are included: MAT
     computes the same dominator tree, so its result set is the oracle for ours.
     Edge primitives (`outbounds`/`inbounds`, bounded `path`) and homogeneous
     `UNION` are likewise cross-checked against MAT's own results.

## Rollout / Phasing (still one spec, one plan)

The strategy ladder gives a natural implementation order that keeps each step
shippable and independently tested:

1. Parser + AST + planner (pure, no dump), incl. the full `needs → stages/carries`
   decision surface and the compressed-carry codec — fully unit tested first.
2. `HistogramOnly` execution + result model + renderers — smallest end-to-end
   slice (single stage, no carries).
3. `SingleScan` (scalar, then String + `LIKE` regex, then `@clazz`/`INSTANCEOF`/
   array `@length`/`IN`) on the P1 field-decode scan (single stage).
4. `CrossPhase` (P1 `Match` → compressed `IndexOnly`/`IndexPlusScalars` carry →
   P3 `JoinRetained`), enabling `ORDER BY @retainedHeapSize` /
   `SELECT @retainedHeapSize`. This lands the general stage-runner + carry codec.
5. `RefWalk` (1-hop, then adaptive N-hop; CSR-hop and batched-resolve-scan;
   `AddrFrontier` carries; predicate-critical vs projection-only) — and, riding
   the same stage machinery, arbitrary 3+-phase spans.
6. Dominator primitives (`dominators()`, `AS RETAINED SET`) as P3 stages hooked
   into the `build_model` `dc_*` window — reusing the surviving dominator tree.
7. Query-gated edge retention: `RunFlags` (incl. `retain_rows` bitset +
   `outbounds_by_rescan`) computed pre-pass2; pipeline retains only matched rows'
   compressed adjacency (Levers 1–2), re-derives bounded `outbounds` by rescan
   (Lever 3), and confines `path` to the pruned subgraph (Lever 4). `EdgeLookup`
   + bounded `BoundedPath` stages. (Sequenced late because it touches the
   pipeline's teardown decisions.)
8. Homogeneous `UNION` (sub-plan-per-branch + row concatenation).
9. TOML input + CLI wiring + golden e2e fixtures.
10. Interactive `query` subcommand (`!plan`/`!explain`/`!schema`) — a thin
    stdin front-end over the same engine; lands once the executor is stable.

Each step is independently valuable: many real queries are satisfied by steps
1–3 alone; steps 4–6 unlock the "biggest X held by Y" and dominator questions;
steps 7–8 close the raw-edge and set-union gaps for the users who need them.

## Open Risks

- **String field decode cost**: decoding String fields requires resolving the
  String's backing array; on String-heavy queries this adds work. Mitigated by
  only decoding fields the query references and bounding matched rows.
- **Path resolution on huge frontiers**: a broad `com.acme.*` FROM with a deep
  path could produce a large hop frontier. Mitigated by per-hop caps +
  `truncated`, and by resolving projection-only hops *after* WHERE+LIMIT pruning.
- **Carry-buffer overflow across phases**: a field filter matching millions of
  objects before an `ORDER BY @retainedHeapSize` fills the P1→P3 carry. Mitigated
  first by *compression* — the index-only carry is delta-varint encoded, so a
  monotonic match set costs ~1–2 bytes/entry and the cap is reached far later than
  with naive 8-byte indices — and then by the cap itself: overflow sets
  `truncated`, and the result is the top-N by the *available* ordering signal,
  documented as approximate when truncated. This is the one place a query result
  can be a bounded sample rather than exact; the flag makes it honest. The same
  bound applies to any cross-phase carry (address frontiers, packed-scalar
  payloads), not just retained joins.
- **Grammar scope creep**: the subset is deliberately fixed; anything outside it
  errors rather than silently under-delivering.
- **Edge-retention RSS**: an `inbounds`/`outbounds`/`path` query keeps some
  reference-graph data resident that the pipeline otherwise frees near the
  ~22 GB peak window. This is *not* the full ~7.5 GB CSR: retention is
  **row-pruned** to the matched-class rows (Lever 1) and stays in the pipeline's
  **compressed delta+vbyte block** form (Lever 2, ~4×), so the added peak is
  `matched_fraction × compressed_size` — typically tens to low-hundreds of MB for
  a single-class edge query. Bounded `outbounds` retains nothing extra, deriving
  edges by one bounded rescan (Lever 3); `path` retains only the pruned subgraph
  (Lever 4) and is depth-capped. Further mitigated by making it strictly
  **opt-in** (computed pre-pass2, so no-edge runs are byte/RSS-identical) and
  **surfaced** (`!plan`/`!explain` + result note report retained MB and % of
  rows). The residual risk is a pathological `FROM` matching a huge fraction of
  all objects *and* using edges; the planner surfaces the estimate so the user
  sees the cost before running.
- **MAT `LIKE` regex fidelity**: `LIKE` is a Java regex in MAT. We use the
  Rust `regex` crate, whose syntax covers the vast majority of Java patterns but
  differs on a few Java-specific constructs (e.g. certain named groups /
  backreferences). Such a pattern yields a clear "unsupported regex construct"
  error at plan time rather than a wrong match. The MAT differential oracle
  (testing layer 7) will surface any semantic drift here concretely.
- **MAT oracle brittleness**: the oracle depends on an external MAT install, a
  JVM, and MAT's CSV output format, which can shift across MAT versions. Mitigated
  by keeping it opt-in (`#[ignore]` + `MAT_HOME`/feature gate) so it never blocks
  normal CI, comparing on *object-address sets* rather than MAT's formatting, and
  pinning a documented MAT version in `scripts/mat-oracle.sh`. It is a
  correctness *amplifier*, not a required gate; the golden e2e fixtures remain the
  authoritative always-on check.
