# JOIN Feature Exploration for hprof-analyzer OQL

An architectural deep-dive into how JOIN could work within the current
engine's single-FROM scan model.  Three approaches at different cost/power
trade-offs, plus the virtual table design that underpins all of them.

---

## Table of Contents

1. [Why JOIN Matters for Heap Analysis](#1-why-join-matters-for-heap-analysis)
2. [Current Architecture Constraints](#2-current-architecture-constraints)
3. [Virtual Table Catalog](#3-virtual-table-catalog)
4. [Approach A: Hash-Join Over Materialized Subqueries](#4-approach-a-hash-join-over-materialized-subqueries)
5. [Approach B: Implicit Virtual-Table JOINs](#5-approach-b-implicit-virtual-table-joins)
6. [Approach C: LATERAL / Table-Valued Functions](#6-approach-c-lateral--table-valued-functions)
7. [Comparison Matrix](#7-comparison-matrix)
8. [Recommended Path](#8-recommended-path)
9. [Detailed Design: Approach A](#9-detailed-design-approach-a)
10. [Detailed Design: Approach B](#10-detailed-design-approach-b)
11. [Detailed Design: Approach C](#11-detailed-design-approach-c)
12. [What NOT to Build](#12-what-not-to-build)

---

## 1. Why JOIN Matters for Heap Analysis

JOINs in heap analysis are fundamentally different from JOINs in a relational
database.  A heap dump is a single denormalized dataset — every object carries
its class, its fields, its dominator, its GC roots.  The cases where JOIN
genuinely helps are **cross-entity** queries that the single-FROM model can't
express:

### Queries that are impossible today

```sql
-- Q1: "For each class, show instance count AND the class's own retained size"
--     Needs: objects table JOIN class histogram (two different granularities)
SELECT c.name, c.instance_count, c.total_retained,
       o.@displayName, o.@retainedHeapSize
FROM objects o
JOIN class_histogram c ON classof(o) = c.name
WHERE c.total_retained > 10485760
ORDER BY c.total_retained DESC

-- Q2: "Find HashMap entries whose keys are Strings containing 'session'"
--     Needs: collection entries JOIN string values
SELECT m.@objectId, toString(entry.key), entry.value
FROM java.util.HashMap m
JOIN LATERAL entries(m) entry
WHERE toString(entry.key) LIKE ".*session.*"

-- Q3: "Objects referenced by Thread objects with name matching 'http-*'"
--     Needs: thread objects JOIN reference edges JOIN target objects
SELECT t.name, ref.field_name, target.@displayName, target.@retainedHeapSize
FROM threads t
JOIN fwd_edges ref ON ref.src = t.@objectId
JOIN objects target ON target.@objectId = ref.dst
WHERE t.name LIKE "http-.*"
ORDER BY target.@retainedHeapSize DESC

-- Q4: "Classes that dominate more retained memory than their instance count
--      would suggest (disproportionate retainers)"
SELECT c.name, c.instance_count, c.total_shallow, c.total_retained,
       c.total_retained / NULLIF(c.instance_count, 0) AS retained_per_inst
FROM class_histogram c
WHERE c.total_retained > c.total_shallow * 10
ORDER BY retained_per_inst DESC LIMIT 20

-- Q5: "GC roots grouped by root type with their dominated subtree size"
SELECT r.root_type_name, COUNT(*) AS roots,
       SUM(o.@retainedHeapSize) AS total_retained
FROM gc_roots r
JOIN objects o ON r.dense_idx = o.@objectId
GROUP BY r.root_type_name
ORDER BY total_retained DESC
```

### What's actually needed

Most of these queries combine:
- **Object-level data** (the scan) with **class-level aggregates** (the histogram)
- **Object data** with **graph edge data** (forward/inbound references)
- **Object data** with **structural metadata** (GC roots, threads, dominator tree)

The key insight: you rarely need to join two independent heap scans.
You need to join a heap scan with a **pre-materialized dimension table**
(classes, GC roots, threads, edges).

---

## 2. Current Architecture Constraints

### The Scan Model

```
HPROF file
  │
  ▼
Pass2 Scanner ──visit_instance(idx, class_id, blob)──▶ ScanDriver
                                                           │
                                                    fan-out to N executors
                                                           │
                                               ┌───────────┴───────────┐
                                               ▼                       ▼
                                          Executor 0              Executor 1
                                          (query 0)              (query 1 / union branch)
                                               │                       │
                                          class_matches?          class_matches?
                                          where_passes?           where_passes?
                                               │                       │
                                          accumulate row          accumulate row
```

**One pass, push model, single FROM per executor.**  The scanner calls
`visit_instance` once per object, and each executor independently decides
whether to accept it.

### What FROM-Subquery Already Does

The `FROM (SELECT ...)` mechanism is the closest thing to a JOIN today:

1. **Inner pass**: Run the inner query as a separate full scan → materialize
   `Vec<u32>` of matching dense indices, sorted.
2. **Outer pass**: Run the outer query as a normal scan (accepting all objects).
3. **Post-scan merge**: Sorted intersection of inner and outer dense indices.
4. **Result assembly**: Filter outer rows to only those in the intersection.

This is effectively a **semi-join on object identity (dense index)**.  It costs
one extra full scan pass.

### What Cannot Change (Hard Constraints)

- **The scanner is single-pass.**  Re-scanning is expensive (HPROF files can
  be 50+ GB). A second pass is tolerable; a per-row inner scan is not.
- **`Graph` is immutable during queries.** No new arrays can be materialized
  onto `Graph` at query time.
- **Memory budget is bounded.**  Materializing a full edge table (30M edges ×
  12 bytes = 360 MB) is too expensive for a query.

### What CAN Change

- Executor can hold a **build-side hash table** populated from a prior pass or
  from `Graph` metadata arrays (these are already in memory).
- A new **`FromSource` variant** can represent a virtual table backed by
  `Graph` arrays (no scan needed).
- Multiple executors can run in the same scan pass (already true for UNION).
- Post-scan result sets can be **hash-joined** (no scan cost, just CPU).

---

## 3. Virtual Table Catalog

These are the "tables" that exist on the `Graph` struct without any scan:

### `objects` — Per-Object Arrays (PK: dense index, 0..n)

Already the implicit single table today.  Every OQL query's FROM clause
ultimately scans this.

| Column | Source | Available |
|--------|--------|-----------|
| `@objectId` | dense index | always |
| `@objectAddress` | addr map | always |
| `@displayName` | class_names[class_idx[i]] | always |
| `@usedHeapSize` | shallow[i] | always |
| `@retainedHeapSize` | retained[i] | after dominator stage |
| `classof()` | class_names[class_idx[i]] | always |
| `@category` | classifier.classify() | always |
| `@dominatorClass` | class_names[class_idx[idom[i]]] | after dominator |
| `@dominatorDepth` | walk idom[] upward | after dominator |
| `@superclass` | class_names[class_super_idx[class_idx[i]]] | always |
| `@collectionSize` | coll_size_per_dense[i] | always |
| `@fillRatio` | computed from fwd edges | with ref graph |

### `classes` — Per-Class Histogram (PK: class_idx)

**Pre-materialized, O(class_count) rows, always in memory.**

| Column | Source |
|--------|--------|
| `class_name` | class_names[ci] |
| `instance_count` | count from class_idx frequency |
| `shallow_total` | sum of shallow[] grouped by class_idx |
| `retained_total` | sum of retained[] grouped by class_idx (after dominator) |
| `loader_id` | class_loader_id[ci] |
| `superclass` | class_names[class_super_idx[ci]] |
| `incoming_ref_count` | incoming_refs_per_class[ci] |

### `gc_roots` — GC Root Entries (PK: index into gc_root_indices)

**Small, always in memory.**

| Column | Source |
|--------|--------|
| `dense_idx` | gc_root_indices[i] |
| `root_type` | gc_root_types[i] |
| `root_type_name` | tag → string mapping |

### `threads` — Thread Data (PK: thread_serial)

**Small, always in memory.**

| Column | Source |
|--------|--------|
| `serial` | thread_stacks[i].thread_serial |
| `obj_idx` | thread_stacks[i].thread_obj_idx |
| `name` | thread_props[serial].name |
| `is_daemon` | thread_props[serial].is_daemon |
| `priority` | thread_props[serial].priority |
| `status` | thread_props[serial].thread_status |
| `frame_count` | thread_stacks[i].frames.len() |

### `fwd_edges` — Forward Reference Graph (PK: edge index)

**Large (millions of rows), CSR-encoded, always in memory.**

| Column | Source |
|--------|--------|
| `src` | implicit from CSR offset iteration |
| `dst` | fwd_targets[edge_idx] |
| `field_name` | field_name_pool[fwd_field_name_idx[edge_idx]] (optional) |

### `type_ref_pairs` — Class-to-Class Edge Summary

**Medium, optional.**

| Column | Source |
|--------|--------|
| `src_class` | class_names[src_ci] |
| `dst_class` | class_names[dst_ci] |
| `edge_count` | type_ref_pairs[(src_ci, dst_ci)] |

### `dom_children` — Dominator Tree Children (derived)

**Ephemeral, built from `idom[]` on demand.**

| Column | Source |
|--------|--------|
| `parent` | implicit from CSR |
| `child` | dc_tgt[edge_idx] |

---

## 4. Approach A: Hash-Join Over Materialized Subqueries

### Concept

Extend the existing FROM-subquery mechanism to support **explicit equijoin
on arbitrary keys** (not just dense-index identity).  Both sides are
materialized first; the join is a post-scan hash-join.

### Syntax

```sql
-- Standard SQL JOIN syntax
SELECT o.@displayName, o.@retainedHeapSize, c.instance_count
FROM INSTANCEOF java.lang.Object o
JOIN (
    SELECT classof(x) AS name, COUNT(*) AS instance_count
    FROM INSTANCEOF java.lang.Object x
    GROUP BY classof(x)
) c ON classof(o) = c.name
WHERE c.instance_count > 100
ORDER BY o.@retainedHeapSize DESC LIMIT 50
```

### How It Works

```
 Step 1: Materialize RHS (inner subquery)
     ┌──────────────────────────────────┐
     │ Run inner query as normal scan   │
     │ → Vec<Vec<QueryValue>> (rows)    │
     │ → Build HashMap<join_key, rows>  │
     └──────────────────────────────────┘
                     │
 Step 2: Run LHS scan (outer query)
     ┌──────────────────────────────────┐
     │ Normal single-pass scan          │
     │ Per matched object:              │
     │   eval join key expression       │
     │   probe HashMap                  │
     │   if hit: emit joined row(s)     │
     └──────────────────────────────────┘
```

### AST Changes

```rust
// New: a JOIN clause attached to the Query
pub struct JoinClause {
    pub kind: JoinKind,
    /// The right-hand side: a subquery or a virtual table name.
    pub rhs: FromSource,
    pub rhs_alias: Option<String>,
    /// The ON condition: must be an equijoin (lhs_expr = rhs_expr).
    pub on: JoinCondition,
}

pub enum JoinKind {
    Inner,
    Left,  // useful: "show all objects, with class stats where available"
}

pub enum JoinCondition {
    /// Simple equijoin: lhs_key = rhs_key
    Eq { lhs: Expr, rhs: Expr },
    /// Compound: multiple equijoin columns
    Compound(Vec<(Expr, Expr)>),
}

// Extend Query:
pub struct Query {
    // … existing fields …
    pub joins: Vec<JoinClause>,  // NEW — zero or more JOIN clauses
}
```

### Executor Changes

New `JoinExecutor` wraps a `SingleScanExecutor` with a build-side hash map:

```rust
pub(crate) struct JoinState {
    /// Build-side: join_key → Vec<(row_index, row)>
    build_map: HashMap<String, Vec<Vec<QueryValue>>>,
    /// Column metadata for the build side.
    build_columns: Vec<QueryColumn>,
    /// Which column in the build side is the join key.
    build_key_idx: usize,
    /// Join kind (Inner vs Left).
    kind: JoinKind,
}

impl SingleScanExecutor {
    /// After the probe-side scan completes, join rows.
    fn join_rows(
        &mut self,
        probe_rows: Vec<Vec<QueryValue>>,
        probe_key_idx: usize,
        join: &JoinState,
    ) -> Vec<Vec<QueryValue>> {
        let mut result = Vec::new();
        for probe_row in &probe_rows {
            let key = probe_row.get(probe_key_idx)
                .map(|v| format!("{v:?}"))
                .unwrap_or_default();
            if let Some(build_rows) = join.build_map.get(&key) {
                for build_row in build_rows {
                    // Concatenate: [probe_columns..., build_columns...]
                    let mut joined = probe_row.clone();
                    joined.extend(build_row.iter().cloned());
                    result.push(joined);
                }
            } else if matches!(join.kind, JoinKind::Left) {
                // Left join: emit probe row with NULLs for build columns
                let mut joined = probe_row.clone();
                joined.extend(
                    std::iter::repeat(QueryValue::Null)
                        .take(join.build_columns.len())
                );
                result.push(joined);
            }
        }
        result
    }
}
```

### Execution Flow

```rust
fn run_query_with_join(query: &Query, graph: &Graph, …) -> QueryResult {
    // 1. Materialize each JOIN's RHS
    let mut join_states = Vec::new();
    for join in &query.joins {
        let rhs_result = execute_from_source(&join.rhs, graph, …)?;
        let build_key_idx = resolve_key_column(&join.on.rhs, &rhs_result.columns);
        let mut build_map: HashMap<String, Vec<Vec<QueryValue>>> = HashMap::new();
        for row in &rhs_result.rows {
            let key = row.get(build_key_idx)
                .map(|v| format!("{v:?}"))
                .unwrap_or_default();
            build_map.entry(key).or_default().push(row.clone());
        }
        join_states.push(JoinState {
            build_map,
            build_columns: rhs_result.columns,
            build_key_idx,
            kind: join.kind,
        });
    }

    // 2. Run LHS scan (normal single-scan path)
    let mut lhs_result = run_scan(query, graph, …)?;

    // 3. Apply joins sequentially (left-to-right for multi-join)
    for (join_clause, join_state) in query.joins.iter().zip(&join_states) {
        let probe_key_idx = resolve_key_column(&join_clause.on.lhs, &lhs_result.columns);
        lhs_result.rows = join_rows(&lhs_result.rows, probe_key_idx, join_state);
        // Extend column metadata
        lhs_result.columns.extend(join_state.build_columns.iter().cloned());
    }

    // 4. Apply WHERE, ORDER BY, LIMIT on the joined result
    apply_post_join_clauses(&mut lhs_result, query);

    lhs_result
}
```

### Cost Analysis

| Aspect | Cost |
|--------|------|
| Extra scan passes | 1 per JOIN (RHS materialization), same as FROM-subquery today |
| Memory | O(RHS_rows) for the hash map — bounded by `SUBQUERY_SET_CAP` |
| CPU | O(LHS_rows × avg_build_bucket_size) for probing |
| Implementation | ~300 LOC (AST + parser + executor + planner) |

### Pros / Cons

| Pro | Con |
|-----|-----|
| Standard SQL syntax | RHS must be a subquery (or virtual table) — no raw class JOIN |
| Reuses existing scan machinery | Extra scan pass per JOIN (but UNION also costs zero extra) |
| Works with all existing features (GROUP BY, ORDER BY, LIMIT) | Multi-JOIN = sequential, no bushy plans |
| LEFT JOIN gives "all objects with optional enrichment" | RHS memory bounded but could still be large |

---

## 5. Approach B: Implicit Virtual-Table JOINs

### Concept

Instead of requiring a subquery RHS, allow JOINing directly against
**named virtual tables** backed by `Graph` arrays.  These tables are never
scanned from the HPROF file — they're materialized from in-memory arrays
with zero I/O.

### Syntax

```sql
-- JOIN against the class histogram (zero-cost, in-memory)
SELECT o.@displayName, o.@retainedHeapSize,
       c.instance_count, c.shallow_total, c.retained_total
FROM INSTANCEOF java.lang.Object o
JOIN classes c ON classof(o) = c.class_name
WHERE c.retained_total > 10485760
ORDER BY o.@retainedHeapSize DESC LIMIT 50

-- JOIN against GC roots
SELECT o.@displayName, o.@retainedHeapSize, r.root_type_name
FROM INSTANCEOF java.lang.Object o
JOIN gc_roots r ON o.@objectId = r.dense_idx
ORDER BY o.@retainedHeapSize DESC

-- JOIN against threads (small table, always in memory)
SELECT t.name, o.@displayName, o.@retainedHeapSize
FROM java.lang.Thread o
JOIN threads t ON o.@objectId = t.obj_idx
ORDER BY o.@retainedHeapSize DESC

-- Self-join via the edge table (large but CSR-indexed)
SELECT src_obj.@displayName AS from_obj,
       e.field_name,
       dst_obj.@displayName AS to_obj,
       dst_obj.@retainedHeapSize
FROM INSTANCEOF java.util.HashMap src_obj
JOIN fwd_edges e ON src_obj.@objectId = e.src
WHERE dst_obj.@retainedHeapSize > 1048576
ORDER BY dst_obj.@retainedHeapSize DESC LIMIT 50
```

### How It Works

Virtual tables are **not scanned from the HPROF file**.  They are
materialized from `Graph` arrays at query plan time:

```rust
fn materialize_virtual_table(
    name: &str,
    graph: &Graph,
) -> Option<(Vec<QueryColumn>, Vec<Vec<QueryValue>>)> {
    match name {
        "classes" => {
            // Build from class_names[], class_loader_id[], etc.
            let mut rows = Vec::with_capacity(graph.class_names.len());
            for ci in 0..graph.class_names.len() {
                let inst_count = count_instances_of_class(graph, ci);
                rows.push(vec![
                    QueryValue::Str(graph.class_names[ci].clone()),
                    QueryValue::Int(inst_count as i64),
                    QueryValue::Int(shallow_total_for_class(graph, ci) as i64),
                    QueryValue::Int(retained_total_for_class(graph, ci) as i64),
                    // …
                ]);
            }
            let columns = vec![
                QueryColumn { name: "class_name".into() },
                QueryColumn { name: "instance_count".into() },
                QueryColumn { name: "shallow_total".into() },
                QueryColumn { name: "retained_total".into() },
            ];
            Some((columns, rows))
        }
        "gc_roots" => {
            let mut rows = Vec::with_capacity(graph.gc_root_indices.len());
            for i in 0..graph.gc_root_indices.len() {
                rows.push(vec![
                    QueryValue::Int(graph.gc_root_indices[i] as i64),
                    QueryValue::Int(graph.gc_root_types[i] as i64),
                    QueryValue::Str(root_type_name(graph.gc_root_types[i])),
                ]);
            }
            // …
            Some((columns, rows))
        }
        "threads" => {
            // Build from thread_stacks + thread_props
            // …
            Some((columns, rows))
        }
        _ => None,  // unknown table → fall through to class name resolution
    }
}
```

The build-side hash map construction is the same as Approach A, but the
"inner query" is replaced by a direct array scan — **no HPROF pass needed**.

### Virtual Table Sizes (typical 10M-object dump)

| Table | Row count | Memory | Build cost |
|-------|-----------|--------|------------|
| `classes` | ~50K–200K | ~20 MB | O(class_count) |
| `gc_roots` | ~5K–50K | ~2 MB | O(root_count) |
| `threads` | ~50–500 | ~100 KB | O(thread_count) |
| `type_ref_pairs` | ~10K–100K | ~10 MB | O(pair_count) |
| `fwd_edges` | 10M–100M | **TOO LARGE** | — |
| `dom_children` | 10M | **TOO LARGE** | — |

**`fwd_edges` and `dom_children` cannot be fully materialized as row tables.**
They must use the CSR-indexed lookup path (see Approach C).

### AST Changes

Extend `FromSource` with a virtual table variant:

```rust
pub enum FromSource {
    // … existing …
    /// A named virtual table: "classes", "gc_roots", "threads", etc.
    VirtualTable(String),
}
```

The JOIN clause from Approach A applies identically.

### Cost Analysis

| Aspect | Cost |
|--------|------|
| Extra scan passes | **Zero** — virtual tables built from in-memory arrays |
| Memory | O(virtual_table_rows) — typically small (classes, roots, threads) |
| CPU | O(LHS_rows) for hash probing |
| Implementation | ~400 LOC (virtual table materializer + Approach A join machinery) |

### Pros / Cons

| Pro | Con |
|-----|-----|
| Zero-cost RHS (no HPROF re-scan) | Need to maintain virtualizers per table |
| Very natural for class/root/thread enrichment | Large tables (edges, dom children) can't materialize |
| Discoverable (`SHOW TABLES` / tab completion) | New concept users must learn |
| Composable with GROUP BY, window functions | Schema is rigid (can't add user columns) |

---

## 6. Approach C: LATERAL / Table-Valued Functions

### Concept

Instead of materializing a full table for the RHS, expose **table-valued
functions** that return a small row set per input object.  This is the
pattern Eclipse MAT's Calcite plugin uses.

### Syntax

```sql
-- LATERAL: per-object edge expansion
SELECT o.@displayName, ref.field_name, ref.dst_class, ref.dst_retained
FROM INSTANCEOF java.util.HashMap o,
     LATERAL outbound_refs(o) ref
WHERE ref.dst_retained > 1048576
ORDER BY ref.dst_retained DESC LIMIT 100

-- LATERAL: per-object collection entries
SELECT m.@displayName, e.key_str, e.value_class, e.value_retained
FROM java.util.HashMap m,
     LATERAL entries(m) e
WHERE m.@retainedHeapSize > 1048576
ORDER BY e.value_retained DESC LIMIT 100

-- LATERAL: per-object dominator children
SELECT parent.@displayName, child.@displayName, child.@retainedHeapSize
FROM INSTANCEOF java.lang.Object parent,
     LATERAL dom_children(parent) child
WHERE parent.@retainedHeapSize > 10485760
ORDER BY child.@retainedHeapSize DESC LIMIT 100
```

### How It Works

LATERAL is a **per-row function call** — for each row from the LHS, the
table function produces zero or more RHS rows.  The output is the cross
product.

```
LHS scan
  │
  ▼
For each matched object o:
  │
  ├─ outbound_refs(o) → iterate fwd_targets[fwd_offsets[o]..fwd_offsets[o+1]]
  │                      → 0..K rows per object (K = out-degree)
  │
  ├─ inbound_refs(o) → iterate inbound CSR (if available)
  │                    → 0..K rows
  │
  ├─ dom_children(o) → iterate dc_tgt[dc_off[o]..dc_off[o+1]]
  │                    → 0..K rows
  │
  └─ entries(o)      → iterate collection elements (already exists!)
                       → 0..K rows
```

### Connection to Existing Features

**`FROM ENTRIES`** is already a form of LATERAL — it expands a collection
into its elements.  `outbounds()` and `dominators()` as SELECT items also
produce multi-row output.  LATERAL generalizes these into a composable
framework.

The difference: today `outbounds()` must be the **sole SELECT item**.
LATERAL lets you combine outbound refs with other columns from the parent
object.

### AST Changes

```rust
/// A LATERAL table-valued function in a FROM clause.
pub struct LateralSource {
    pub func: LateralFunc,
    pub alias: String,
}

pub enum LateralFunc {
    /// outbound_refs(alias) — forward reference edges of each matched object.
    OutboundRefs,
    /// inbound_refs(alias) — inbound reference edges (requires inbound CSR).
    InboundRefs,
    /// dom_children(alias) — dominator-tree children.
    DomChildren,
    /// entries(alias) — collection elements (generalized FROM ENTRIES).
    Entries,
}

// Extend Query:
pub struct Query {
    // … existing …
    pub lateral: Option<LateralSource>,  // NEW
}
```

### Executor Changes

The key difference from Approach A: **no build-side materialization**.
Instead, the executor emits multiple output rows per matched LHS object:

```rust
// In visit_instance, after class_matches and where_passes:
if let Some(lateral) = &self.lateral {
    match lateral.func {
        LateralFunc::OutboundRefs => {
            let start = graph.fwd_offsets[src_idx] as usize;
            let end = graph.fwd_offsets[src_idx + 1] as usize;
            for edge_idx in start..end {
                let dst = graph.fwd_targets.get(edge_idx);
                let field_name = graph.fwd_field_name_idx
                    .as_ref()
                    .and_then(|fi| graph.field_name_pool.as_ref()
                        .map(|pool| pool[fi[edge_idx] as usize].as_str()))
                    .unwrap_or("");
                let dst_class = &graph.class_names[graph.class_idx[dst as usize] as usize];
                let dst_retained = graph.retained.get(dst as usize).copied().unwrap_or(0);

                // Build combined row: [lhs_columns..., lateral_columns...]
                let mut row = build_lhs_row(src_idx, class_id, blob);
                row.push(QueryValue::Int(dst as i64));
                row.push(QueryValue::Str(field_name.to_string()));
                row.push(QueryValue::Str(dst_class.to_string()));
                row.push(QueryValue::Int(dst_retained as i64));
                self.rows.push(row);

                if self.rows.len() >= self.plan.row_cap() {
                    self.truncated = true;
                    return;
                }
            }
        }
        LateralFunc::DomChildren => {
            // Same pattern with dc_off/dc_tgt (built on demand from idom[])
            // …
        }
        // …
    }
} else {
    // Normal single-row output (existing path)
    self.rows.push(build_lhs_row(src_idx, class_id, blob));
}
```

### Cost Analysis

| Aspect | Cost |
|--------|------|
| Extra scan passes | **Zero** — CSR lookup during the existing scan |
| Memory | O(output_rows) — bounded by row cap |
| CPU | O(LHS_matched × avg_degree) per object |
| Implementation | ~350 LOC (AST + parser + executor lateral dispatch) |

### Pros / Cons

| Pro | Con |
|-----|-----|
| Zero extra scan passes | Only pre-defined functions, not arbitrary subqueries |
| Handles large edge tables (CSR-indexed, no materialization) | Per-row function call — no hash join optimization |
| Natural generalization of existing outbounds/dominators | New syntax concept (LATERAL) |
| Composable with WHERE, ORDER BY, LIMIT, GROUP BY | N:M joins explode row count |
| MAT Calcite plugin uses exactly this pattern | |

---

## 7. Comparison Matrix

| Criterion | A: Hash-Join | B: Virtual Tables | C: LATERAL |
|-----------|-------------|-------------------|-----------|
| **Extra scan passes** | 1 per JOIN | 0 | 0 |
| **RHS flexibility** | Any subquery | Named tables only | Pre-defined functions |
| **Large edge tables** | ❌ Can't materialize | ❌ Can't materialize | ✅ CSR-indexed |
| **Small dimension tables** | ✅ Via subquery | ✅ Direct | ❌ Not applicable |
| **SQL familiarity** | ✅ Standard JOIN | ✅ Standard JOIN | ⚠️ LATERAL is advanced |
| **Implementation cost** | ~300 LOC | ~400 LOC (includes A) | ~350 LOC |
| **Composability** | ✅ Full | ✅ Full | ✅ Full |
| **Row explosion risk** | Low (equijoin) | Low (equijoin) | ⚠️ High (N:M) |
| **Reuses existing code** | ~70% (subquery path) | ~60% | ~50% (new path) |

---

## 8. Recommended Path

### Phase 1: Approach B (Virtual Table JOINs) — ~400 LOC

This gives the highest value for heap analysis workflows:
- JOIN objects with `classes` for enriched per-class analysis
- JOIN objects with `gc_roots` for root-type grouping
- JOIN objects with `threads` for thread-attributed memory analysis
- Zero extra scan passes (virtual tables are in-memory)

Includes Approach A's hash-join machinery as the execution strategy.

### Phase 2: Approach C (LATERAL functions) — ~350 LOC

Once Phase 1 ships, add LATERAL for edge-table queries:
- `outbound_refs(o)` for per-object reference expansion
- `dom_children(o)` for dominator-tree navigation
- `entries(o)` as a generalization of the existing FROM ENTRIES

This replaces the "must be sole SELECT item" restriction on `outbounds()`,
`dominators()`, and `@inbounds` with a composable framework.

### Phase 3: Approach A generalization

Allow arbitrary subqueries as JOIN RHS (already works from Phase 1
machinery).  This is mostly a parser change — the execution path already
supports it.

### Total: ~750 LOC across all three phases

---

## 9. Detailed Design: Approach A

*(Covered in section 4 above with full AST, executor, and flow sketches.)*

### Parser Sketch

```rust
// After the FROM clause, parse optional JOIN clauses:
let join_kind = ident_ci("JOIN").to(JoinKind::Inner)
    .or(ident_ci("LEFT").ignore_then(ident_ci("JOIN")).to(JoinKind::Left));

let join_on = ident_ci("ON")
    .ignore_then(expr.clone())
    .then_ignore(just(Token::Eq))
    .then(expr.clone())
    .map(|(lhs, rhs)| JoinCondition::Eq { lhs, rhs });

let join_clause = join_kind
    .then(from_source.clone())     // subquery or virtual table
    .then(alias.or_not())
    .then(join_on)
    .map(|(((kind, rhs), rhs_alias), on)| JoinClause {
        kind, rhs, rhs_alias, on,
    });

// In base_query, after FROM:
.then(join_clause.repeated().collect::<Vec<_>>())
```

### Planner Validation

```rust
// In plan_query, validate JOIN clauses:
for join in &query.joins {
    // 1. RHS must be a subquery or virtual table (not a raw class)
    match &join.rhs {
        FromSource::Class(_) => {
            return Err("JOIN RHS must be a subquery or virtual table, \
                        not a class name; wrap in (SELECT * FROM <class>)");
        }
        _ => {}
    }
    // 2. ON condition must be an equijoin
    match &join.on {
        JoinCondition::Eq { .. } => {} // ok
        _ => {
            return Err("only equijoin (ON a = b) is supported");
        }
    }
    // 3. Plan the RHS
    let rhs_plan = plan_from_source(&join.rhs, …)?;
    plan.join_plans.push(rhs_plan);
}
```

---

## 10. Detailed Design: Approach B

### Virtual Table Recognition

In the parser, a `FROM` identifier that matches a known virtual table name
resolves to `FromSource::VirtualTable(name)`.  This happens at plan time,
not parse time (the parser can't distinguish `gc_roots` from a class name
without schema knowledge):

```rust
// In plan_query, when resolving FromSource::Class(spec):
fn resolve_from_source(spec: &ClassSpec, graph: &Graph) -> FromSource {
    let name = &spec.class_name;
    if is_virtual_table(name) {
        FromSource::VirtualTable(name.clone())
    } else {
        FromSource::Class(spec.clone())
    }
}

fn is_virtual_table(name: &str) -> bool {
    matches!(name, "classes" | "gc_roots" | "threads" | "type_ref_pairs")
}
```

### Materialization at Execution Time

```rust
fn execute_virtual_table(
    name: &str,
    graph: &Graph,
    classifier: &Classifier,
) -> QueryResult {
    match name {
        "classes" => build_classes_table(graph, classifier),
        "gc_roots" => build_gc_roots_table(graph),
        "threads" => build_threads_table(graph),
        "type_ref_pairs" => build_type_ref_pairs_table(graph),
        _ => unreachable!(),
    }
}

fn build_classes_table(graph: &Graph, classifier: &Classifier) -> QueryResult {
    let n_classes = graph.class_names.len();
    // Pre-compute per-class instance counts and totals
    let mut inst_count = vec![0u64; n_classes];
    let mut shallow_total = vec![0u64; n_classes];
    for i in 0..graph.n {
        let ci = graph.class_idx[i] as usize;
        inst_count[ci] += 1;
        shallow_total[ci] += graph.shallow[i] as u64;
    }
    // retained_total only if retained[] is populated
    let has_retained = !graph.retained.is_empty();
    let mut retained_total = if has_retained {
        let mut rt = vec![0u64; n_classes];
        for i in 0..graph.n {
            let ci = graph.class_idx[i] as usize;
            rt[ci] += graph.retained[i];
        }
        Some(rt)
    } else {
        None
    };

    let mut rows = Vec::with_capacity(n_classes);
    for ci in 0..n_classes {
        let mut row = vec![
            QueryValue::Str(graph.class_names[ci].clone()),
            QueryValue::Int(inst_count[ci] as i64),
            QueryValue::Int(shallow_total[ci] as i64),
        ];
        if let Some(rt) = &retained_total {
            row.push(QueryValue::Int(rt[ci] as i64));
        }
        row.push(QueryValue::Str(
            classifier.classify(&graph.class_names[ci]).to_string()
        ));
        rows.push(row);
    }

    let mut columns = vec![
        QueryColumn { name: "class_name".into() },
        QueryColumn { name: "instance_count".into() },
        QueryColumn { name: "shallow_total".into() },
    ];
    if has_retained {
        columns.push(QueryColumn { name: "retained_total".into() });
    }
    columns.push(QueryColumn { name: "category".into() });

    QueryResult {
        name: "classes".into(),
        oql: String::new(),
        columns,
        rows,
        row_count: n_classes as u64,
        truncated: false,
        error: None,
        note: None,
        viz: None,
        elapsed_ms: None,
    }
}
```

### Tab Completion / REPL Discovery

Add virtual table names to the REPL's completion list:

```rust
const VIRTUAL_TABLES: &[&str] = &[
    "classes", "gc_roots", "threads", "type_ref_pairs",
];

// In complete.rs, when completing after JOIN or FROM:
for &vt in VIRTUAL_TABLES {
    if vt.starts_with(prefix) {
        completions.push(vt.to_string());
    }
}
```

---

## 11. Detailed Design: Approach C

### `LATERAL` as Syntactic Sugar

LATERAL table-valued functions can be parsed as a special FROM extension:

```sql
-- Full LATERAL syntax:
FROM java.util.HashMap m, LATERAL outbound_refs(m) ref

-- Equivalent shorthand (comma-join with LATERAL):
FROM java.util.HashMap m CROSS JOIN LATERAL outbound_refs(m) ref
```

### Parser

```rust
let lateral_func = ident_ci("outbound_refs").to(LateralFunc::OutboundRefs)
    .or(ident_ci("inbound_refs").to(LateralFunc::InboundRefs))
    .or(ident_ci("dom_children").to(LateralFunc::DomChildren))
    .or(ident_ci("entries").to(LateralFunc::Entries));

let lateral_source = ident_ci("LATERAL")
    .or_not()  // LATERAL keyword is optional after comma
    .ignore_then(lateral_func)
    .then_ignore(just(Token::LParen))
    .then(ident_raw())  // alias reference
    .then_ignore(just(Token::RParen))
    .then(ident_raw())  // lateral alias
    .map(|((func, _parent_alias), alias)| LateralSource { func, alias });
```

### Executor: Per-Object Row Expansion

The critical change: when `lateral.is_some()`, the executor emits
**multiple rows per matched LHS object**.  This requires:

1. Building the LHS row columns once per object
2. Iterating the CSR edge list for that object
3. Emitting combined rows (LHS columns + lateral columns)
4. Enforcing a per-query row cap to prevent explosion

```rust
// In visit_instance, inside the lateral dispatch:
LateralFunc::OutboundRefs => {
    let start = self.graph.fwd_offsets[src_idx] as usize;
    let end = self.graph.fwd_offsets[src_idx + 1] as usize;
    if start == end { return; } // no outbound refs

    let lhs_row = self.build_row(src_idx, class_id, blob);

    for edge_idx in start..end {
        let dst = self.graph.fwd_targets.get(edge_idx) as usize;
        let field_name_idx = self.graph.fwd_field_name_idx
            .as_ref().map(|v| v[edge_idx]).unwrap_or(0);
        let field_name = self.graph.field_name_pool
            .as_ref().map(|p| p[field_name_idx as usize].as_str())
            .unwrap_or("");
        let dst_class = &self.graph.class_names[
            self.graph.class_idx[dst] as usize
        ];

        let mut row = lhs_row.clone();
        row.push(QueryValue::Int(dst as i64));           // ref.dst
        row.push(QueryValue::Str(field_name.into()));    // ref.field_name
        row.push(QueryValue::Str(dst_class.into()));     // ref.dst_class
        if !self.graph.retained.is_empty() {
            row.push(QueryValue::Int(self.graph.retained[dst] as i64));
        }

        self.rows.push(row);
        if self.rows.len() >= LATERAL_ROW_CAP {
            self.truncated = true;
            return;
        }
    }
}
```

### Row Cap

LATERAL can explode row counts (an object with 1000 outbound refs produces
1000 rows).  A dedicated cap:

```rust
const LATERAL_ROW_CAP: usize = 1_000_000;
```

This is independent of `scan_limit` (which gates LHS objects, not output
rows).

---

## 12. What NOT to Build

| Feature | Why not |
|---------|---------|
| **Full multi-table FROM (Cartesian product)** | O(N × M) explosion with no join predicate; useless for heap analysis |
| **Nested loop join** | O(N × M) per-row cost; hash join is strictly better for equijoin |
| **Merge join** | Requires both sides sorted on join key; hash join is simpler and usually faster |
| **Bushy join trees** | Multiple JOINs should be left-deep (sequential), not bushy; no cost-based optimizer needed |
| **Non-equijoin (theta join)** | No heap analysis use case for `ON a.size > b.size`; restrict to equijoin |
| **RIGHT JOIN / FULL OUTER JOIN** | Minimal value; LEFT JOIN + INNER cover all heap analysis cases |
| **Correlated LATERAL** | Too expensive (per-row function call with per-row inner scan); uncorrelated LATERAL with CSR lookup is sufficient |
| **DataFusion / embedded SQL engine** | Binary size (+20MB), async requirement, Arrow conversion cost — see previous analysis. The virtual table + hash join approach gives 80% of the value at 5% of the cost |

---

## Summary

The heap dump JOIN problem is fundamentally different from general-purpose
SQL JOINs.  You're not joining two independent datasets — you're enriching
a heap scan with **dimension tables** (classes, roots, threads) and
**graph lookups** (edges, dominator children).

The three-phase plan (virtual tables → LATERAL → general subquery JOIN)
covers all heap analysis use cases at ~750 LOC total, with zero extra HPROF
scan passes for the most common patterns.  Each phase delivers standalone
value and doesn't require the subsequent phases to be useful.
