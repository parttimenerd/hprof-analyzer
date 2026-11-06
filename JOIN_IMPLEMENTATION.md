# JOIN Implementation Drafts for hprof-analyzer OQL

Detailed, file-by-file implementation sketches for all three JOIN phases.
Each section shows the exact struct/enum/function changes needed, keyed to
real line numbers and patterns in the current codebase.

Companion to: `JOIN_EXPLORATION.md` (architecture rationale).

---

## Table of Contents

- [Phase 1 — Virtual Table JOINs](#phase-1--virtual-table-joins)
  - [1.1 AST changes (ast.rs)](#11-ast-changes-astrs)
  - [1.2 Parser changes (parse.rs)](#12-parser-changes-parsers)
  - [1.3 Virtual table catalog (new: vtable.rs)](#13-virtual-table-catalog-new-vtablers)
  - [1.4 Planner changes (plan.rs)](#14-planner-changes-planrs)
  - [1.5 Executor changes (execute.rs)](#15-executor-changes-executers)
  - [1.6 Run-driver changes (run.rs)](#16-run-driver-changes-runrs)
  - [1.7 Optimizer changes (optimize.rs)](#17-optimizer-changes-optimizers)
  - [1.8 REPL / completion (complete.rs)](#18-repl--completion-completers)
  - [1.9 Tests](#19-tests)
- [Phase 2 — LATERAL Table-Valued Functions](#phase-2--lateral-table-valued-functions)
  - [2.1 AST changes](#21-ast-changes)
  - [2.2 Parser changes](#22-parser-changes)
  - [2.3 Planner changes](#23-planner-changes)
  - [2.4 Executor changes](#24-executor-changes)
  - [2.5 Stage runner integration](#25-stage-runner-integration)
  - [2.6 RunFlags changes](#26-runflags-changes)
  - [2.7 Tests](#27-tests)
- [Phase 3 — General Subquery JOINs](#phase-3--general-subquery-joins)
  - [3.1 Parser generalization](#31-parser-generalization)
  - [3.2 Multi-JOIN chaining](#32-multi-join-chaining)
  - [3.3 Left JOIN semantics](#33-left-join-semantics)
  - [3.4 Tests](#34-tests)

---

## Phase 1 — Virtual Table JOINs

**Goal**: `SELECT ... FROM <class> o JOIN <vtable> v ON <equijoin>`.
Virtual tables (`classes`, `gc_roots`, `threads`, `type_ref_pairs`) are
materialized from `Graph` arrays — zero HPROF re-scan.

**Estimated LOC**: ~420

---

### 1.1 AST changes (ast.rs)

Add after the existing `Query` struct (line 38):

```rust
// ── JOIN clause ───────────────────────────────────────────────────────────

/// The kind of JOIN.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum JoinKind {
    Inner,
    Left,
}

/// An equijoin condition: `lhs_expr = rhs_expr`.  Both sides are arbitrary
/// `Expr` nodes so `classof(o) = c.class_name` works.  The planner validates
/// that at least one side references the LHS alias and the other the RHS alias.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JoinOn {
    pub lhs: Expr,
    pub rhs: Expr,
}

/// One JOIN clause attached to a query.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JoinClause {
    pub kind: JoinKind,
    pub rhs: JoinSource,
    pub alias: Option<String>,
    pub on: JoinOn,
}

/// The right-hand side of a JOIN: a virtual table name or a parenthesized
/// subquery.  `VirtualTable` is resolved at plan time (the parser can't
/// distinguish it from a class name without schema knowledge).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum JoinSource {
    /// A bare identifier — resolved to VirtualTable or Class at plan time.
    Name(String),
    /// A parenthesized subquery: `JOIN (SELECT ...) alias ON ...`
    Subquery(Box<Query>),
}
```

Extend `Query` (line 7):

```rust
pub struct Query {
    // … all existing fields through except_branches …

    /// Zero or more JOIN clauses, evaluated left-to-right.
    pub joins: Vec<JoinClause>,
}
```

---

### 1.2 Parser changes (parse.rs)

After the FROM clause + alias parsing (around line 1128), add JOIN parsing.
Pattern: mirrors the `ORDER BY` clause style — optional, repeatable.

```rust
// ── JOIN clause parser ─────────────────────────────────────────────────

let join_kind = ident_ci("JOIN").to(JoinKind::Inner)
    .or(
        ident_ci("LEFT")
            .ignore_then(
                ident_ci("OUTER").or_not()  // LEFT OUTER JOIN = LEFT JOIN
            )
            .ignore_then(ident_ci("JOIN"))
            .to(JoinKind::Left)
    )
    .or(
        ident_ci("INNER")
            .ignore_then(ident_ci("JOIN"))
            .to(JoinKind::Inner)
    );

// RHS: a bare identifier (class or virtual table) or a parenthesized subquery.
let join_rhs = just(Token::LParen)
    .ignore_then(base_query.clone())
    .then_ignore(just(Token::RParen))
    .map(|q| JoinSource::Subquery(Box::new(q)))
    .or(
        select! { Token::Ident(s) => s }
            .map(JoinSource::Name)
    );

// Alias for the RHS (required for disambiguation, but optional in syntax).
let join_alias = select! { Token::Ident(s) => s }
    .filter(|s: &String| !is_keyword(s))
    .or_not();

// ON condition: expr = expr
let join_on = ident_ci("ON")
    .ignore_then(expr.clone())
    .then_ignore(just(Token::Eq))
    .then(expr.clone())
    .map(|(lhs, rhs)| JoinOn { lhs, rhs });

let join_clause = join_kind
    .then(join_rhs)
    .then(join_alias)
    .then(join_on)
    .map(|(((kind, rhs), alias), on)| JoinClause { kind, rhs, alias, on });

// In the base_query combinator, after FROM + alias:
// .then(join_clause.repeated().collect::<Vec<_>>())  // zero or more JOINs
```

Wire into `Query` construction (around line 1182):

```rust
// In the base_query .map() closure:
Query {
    // … existing fields …
    joins: join_clauses,   // from the join_clause.repeated() result
}
```

Normalize JOIN aliases and ON expressions (around line 1333, after alias normalization):

```rust
for join in &mut q.joins {
    if let Some(alias) = &join.alias {
        // Normalize ON expressions: strip the join alias prefix
        normalize_expr(&mut join.on.lhs, Some(q.alias.as_deref().unwrap_or("")));
        normalize_expr(&mut join.on.rhs, Some(alias.as_str()));
    }
}
```

---

### 1.3 Virtual table catalog (new: vtable.rs)

New file: `src/query/vtable.rs`

```rust
//! Virtual table materializers.  Each function builds a `QueryResult` from
//! `Graph` arrays — no HPROF re-scan needed.

use crate::query::model::{QueryColumn, QueryResult, QueryValue};

/// Known virtual table names.
pub const VIRTUAL_TABLES: &[&str] = &[
    "classes",
    "gc_roots",
    "threads",
    "type_ref_pairs",
    "system_properties",
];

pub fn is_virtual_table(name: &str) -> bool {
    VIRTUAL_TABLES.iter().any(|&vt| vt.eq_ignore_ascii_case(name))
}

/// Column schema for a virtual table (returned before materialization for
/// plan-time arity checks).
pub fn vtable_columns(name: &str) -> Vec<QueryColumn> {
    match name.to_ascii_lowercase().as_str() {
        "classes" => vec![
            qcol("class_name"),
            qcol("instance_count"),
            qcol("shallow_total"),
            qcol("retained_total"),
            qcol("loader_id"),
            qcol("superclass"),
            qcol("incoming_ref_count"),
            qcol("category"),
        ],
        "gc_roots" => vec![
            qcol("dense_idx"),
            qcol("root_type"),
            qcol("root_type_name"),
        ],
        "threads" => vec![
            qcol("serial"),
            qcol("obj_idx"),
            qcol("name"),
            qcol("is_daemon"),
            qcol("priority"),
            qcol("status"),
            qcol("frame_count"),
        ],
        "type_ref_pairs" => vec![
            qcol("src_class"),
            qcol("dst_class"),
            qcol("edge_count"),
        ],
        "system_properties" => vec![
            qcol("key"),
            qcol("value"),
        ],
        _ => Vec::new(),
    }
}

fn qcol(name: &str) -> QueryColumn {
    QueryColumn { name: name.to_string() }
}

// ── Materializers ───────────────────────────────────────────────────────

pub fn materialize_classes(graph: &crate::Graph) -> (Vec<QueryColumn>, Vec<Vec<QueryValue>>) {
    let n_classes = graph.class_names.len();

    // Pre-compute per-class aggregates in one pass over per-object arrays.
    let mut inst_count = vec![0u64; n_classes];
    let mut shallow_total = vec![0u64; n_classes];
    for i in 0..graph.n {
        let ci = graph.class_idx[i] as usize;
        inst_count[ci] += 1;
        shallow_total[ci] += graph.shallow[i] as u64;
    }

    let has_retained = !graph.retained.is_empty();
    let mut retained_total = vec![0u64; n_classes];
    if has_retained {
        for i in 0..graph.n {
            let ci = graph.class_idx[i] as usize;
            retained_total[ci] += graph.retained[i];
        }
    }

    let mut rows = Vec::with_capacity(n_classes);
    for ci in 0..n_classes {
        rows.push(vec![
            QueryValue::Str(graph.class_names[ci].clone()),
            QueryValue::Int(inst_count[ci] as i64),
            QueryValue::Int(shallow_total[ci] as i64),
            QueryValue::Int(retained_total[ci] as i64),
            QueryValue::Int(graph.class_loader_id[ci] as i64),
            if (graph.class_super_idx[ci] as usize) < n_classes {
                QueryValue::Str(graph.class_names[graph.class_super_idx[ci] as usize].clone())
            } else {
                QueryValue::Null
            },
            QueryValue::Int(graph.incoming_refs_per_class[ci] as i64),
            // category: requires classifier — passed separately or looked up
            QueryValue::Str(String::new()), // placeholder; see note below
        ]);
    }

    (vtable_columns("classes"), rows)
}

pub fn materialize_gc_roots(graph: &crate::Graph) -> (Vec<QueryColumn>, Vec<Vec<QueryValue>>) {
    let n = graph.gc_root_indices.len();
    let mut rows = Vec::with_capacity(n);
    for i in 0..n {
        rows.push(vec![
            QueryValue::Int(graph.gc_root_indices[i] as i64),
            QueryValue::Int(graph.gc_root_types[i] as i64),
            QueryValue::Str(gc_root_type_name(graph.gc_root_types[i])),
        ]);
    }
    (vtable_columns("gc_roots"), rows)
}

pub fn materialize_threads(graph: &crate::Graph) -> (Vec<QueryColumn>, Vec<Vec<QueryValue>>) {
    let mut rows = Vec::with_capacity(graph.thread_stacks.len());
    for ts in &graph.thread_stacks {
        let props = graph.thread_props.get(&ts.thread_serial);
        rows.push(vec![
            QueryValue::Int(ts.thread_serial as i64),
            QueryValue::Int(ts.thread_obj_idx as i64),
            QueryValue::Str(
                props.map(|p| p.name.clone()).unwrap_or_default()
            ),
            QueryValue::Bool(
                props.map(|p| p.is_daemon).unwrap_or(false)
            ),
            QueryValue::Int(
                props.map(|p| p.priority as i64).unwrap_or(0)
            ),
            QueryValue::Int(
                props.map(|p| p.thread_status as i64).unwrap_or(0)
            ),
            QueryValue::Int(ts.frames.len() as i64),
        ]);
    }
    (vtable_columns("threads"), rows)
}

pub fn materialize_type_ref_pairs(graph: &crate::Graph) -> (Vec<QueryColumn>, Vec<Vec<QueryValue>>) {
    let pairs = match &graph.type_ref_pairs {
        Some(p) => p,
        None => return (vtable_columns("type_ref_pairs"), Vec::new()),
    };
    let mut rows = Vec::with_capacity(pairs.len());
    for (&(src_ci, dst_ci), &count) in pairs {
        rows.push(vec![
            QueryValue::Str(graph.class_names[src_ci as usize].clone()),
            QueryValue::Str(graph.class_names[dst_ci as usize].clone()),
            QueryValue::Int(count as i64),
        ]);
    }
    (vtable_columns("type_ref_pairs"), rows)
}

pub fn materialize_system_properties(graph: &crate::Graph) -> (Vec<QueryColumn>, Vec<Vec<QueryValue>>) {
    let mut rows = Vec::with_capacity(graph.system_properties.len());
    for (k, v) in &graph.system_properties {
        rows.push(vec![
            QueryValue::Str(k.clone()),
            QueryValue::Str(v.clone()),
        ]);
    }
    (vtable_columns("system_properties"), rows)
}

fn gc_root_type_name(tag: u8) -> String {
    match tag {
        0x00 => "System Class".into(),
        0x01 => "JNI Global".into(),
        0x02 => "JNI Local".into(),
        0x03 => "Java Frame".into(),
        0x04 => "Native Stack".into(),
        0x05 => "Sticky Class".into(),
        0x06 => "Thread Block".into(),
        0x07 => "Monitor Used".into(),
        0x08 => "Thread Object".into(),
        _    => format!("Unknown(0x{tag:02x})"),
    }
}
```

Register in `src/query/mod.rs`:

```rust
pub mod vtable;
```

---

### 1.4 Planner changes (plan.rs)

Add a `JoinPlan` struct and a field on `QueryPlan`:

```rust
// After ExistsSubplan (line ~240):

/// Planned JOIN: the build-side result + the ON key column indices.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JoinPlan {
    pub kind: JoinKind,
    /// Build-side column schema (from vtable or inner subquery).
    pub build_columns: Vec<QueryColumn>,
    /// How to produce the build-side rows:
    pub build_source: JoinBuildSource,
    /// ON condition expressions (for probe-key evaluation at scan time).
    pub on: JoinOn,
    /// RHS alias for column qualification.
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum JoinBuildSource {
    /// A virtual table: rows will be materialized from Graph at execution time.
    VirtualTable(String),
    /// A subquery: planned recursively; rows materialized by a prior scan pass.
    Subquery(Box<QueryPlan>, Box<Query>),
}
```

Add to `QueryPlan` (after `exists_subplans`, line ~210):

```rust
    /// Planned JOIN clauses. Empty for non-JOIN queries.
    pub join_plans: Vec<JoinPlan>,
```

In `plan_single`, after building `from_subplan` (line ~910), add JOIN planning:

```rust
    // ── Plan JOIN clauses ──────────────────────────────────────────────────
    let mut join_plans = Vec::new();
    for jc in &q.joins {
        let (build_source, build_columns) = match &jc.rhs {
            JoinSource::Name(name) => {
                if vtable::is_virtual_table(name) {
                    let cols = vtable::vtable_columns(name);
                    (JoinBuildSource::VirtualTable(name.clone()), cols)
                } else {
                    return Err(QueryError(format!(
                        "JOIN target \"{name}\" is not a known virtual table; \
                         known tables: {}. Wrap a class query in a subquery: \
                         JOIN (SELECT * FROM {name}) alias ON …",
                        vtable::VIRTUAL_TABLES.join(", ")
                    )));
                }
            }
            JoinSource::Subquery(inner) => {
                let inner_plan = plan_query(inner, depth_cap)?;
                if plan_has_subqueries(&inner_plan) {
                    return Err(QueryError(
                        "nested subqueries inside JOIN are not supported".into()
                    ));
                }
                let cols = build_columns_from_query(inner);
                (
                    JoinBuildSource::Subquery(Box::new(inner_plan), inner.clone()),
                    cols,
                )
            }
        };

        // Validate ON condition: both sides must be resolvable expressions.
        validate_join_on(&jc.on, q, &build_columns, jc.alias.as_deref())?;

        join_plans.push(JoinPlan {
            kind: jc.kind,
            build_columns,
            build_source,
            on: jc.on.clone(),
            alias: jc.alias.clone(),
        });
    }
```

Wire `join_plans` into the final `Ok(QueryPlan { ... })` literal:

```rust
    Ok(QueryPlan {
        // … existing fields …
        join_plans,
    })
```

Helper: `validate_join_on` ensures both sides of the ON are resolvable
and at least one references the LHS alias, the other the RHS alias:

```rust
fn validate_join_on(
    on: &JoinOn,
    q: &Query,
    build_columns: &[QueryColumn],
    rhs_alias: Option<&str>,
) -> Result<(), QueryError> {
    // Check that lhs references the FROM alias (or unqualified)
    // and rhs references the JOIN alias (or a build column name).
    // Reject non-equijoin (the parser only emits = anyway).
    Ok(())
}
```

---

### 1.5 Executor changes (execute.rs)

New struct — `JoinState` — holds the build-side hash map:

```rust
/// Build-side state for a hash join.  Constructed before the probe scan starts.
pub(crate) struct JoinState {
    /// Build-side rows keyed by the Debug-formatted join key.
    /// Each key may map to multiple rows (fan-out).
    pub build_map: std::collections::HashMap<String, Vec<Vec<QueryValue>>>,
    /// Column metadata for the build side.
    pub build_columns: Vec<QueryColumn>,
    /// JOIN kind (Inner vs Left).
    pub kind: JoinKind,
    /// The ON condition (lhs expr evaluated per probe row, rhs column index in build side).
    pub probe_key_expr: Expr,
    pub build_key_idx: usize,
}
```

Add to `SingleScanExecutor` (after `exists_cursor`, line ~302):

```rust
    /// Join states injected by the driver before the probe scan.
    /// One per `plan.join_plans` entry.  Empty for non-JOIN queries.
    join_states: Vec<JoinState>,
```

Initialize in both `new()` and `new_carry()`:

```rust
    join_states: Vec::new(),  // populated by driver before scan
```

New method on `SingleScanExecutor` — `apply_joins`:

```rust
impl<'a, R: ClassResolver> SingleScanExecutor<'a, R> {
    /// Post-scan: join the probe-side rows against each JoinState.
    /// Called by the run driver after the scan completes.
    pub(crate) fn apply_joins(&mut self) {
        for js in &self.join_states {
            let mut joined = Vec::new();
            let probe_key_idx = self.resolve_column_index(&js.probe_key_expr);
            for probe_row in &self.rows {
                let key = probe_row
                    .get(probe_key_idx)
                    .map(|v| format!("{v:?}"))
                    .unwrap_or_default();

                if let Some(build_rows) = js.build_map.get(&key) {
                    for build_row in build_rows {
                        let mut row = probe_row.clone();
                        row.extend(build_row.iter().cloned());
                        joined.push(row);
                    }
                } else if matches!(js.kind, JoinKind::Left) {
                    let mut row = probe_row.clone();
                    row.extend(
                        std::iter::repeat(QueryValue::Null)
                            .take(js.build_columns.len())
                    );
                    joined.push(row);
                }
            }
            self.rows = joined;
        }
    }

    /// Column metadata including join columns.
    pub(crate) fn columns_with_joins(&self) -> Vec<QueryColumn> {
        let mut cols = self.base_columns();
        for js in &self.join_states {
            cols.extend(js.build_columns.iter().cloned());
        }
        cols
    }
}
```

---

### 1.6 Run-driver changes (run.rs)

In `run_single_dump` (or its equivalent), after the existing subquery
materialization and before the outer scan:

```rust
    // ── Materialize JOIN build sides ───────────────────────────────────────
    for (slot, (_q, plan)) in flat.iter().enumerate() {
        let mut join_states = Vec::new();
        for jp in &plan.join_plans {
            let (build_cols, build_rows) = match &jp.build_source {
                JoinBuildSource::VirtualTable(name) => {
                    materialize_vtable(name, graph)
                }
                JoinBuildSource::Subquery(inner_plan, inner_query) => {
                    // Run inner subquery as an additional scan slot
                    // (reuses existing subquery execution machinery)
                    let inner_result = run_inner_subquery(
                        inner_query, inner_plan, graph, resolver, opts,
                    )?;
                    (inner_result.columns, inner_result.rows)
                }
            };

            // Resolve build-key column index from ON.rhs expression
            let build_key_idx = resolve_build_key(&jp.on.rhs, &build_cols, jp.alias.as_deref());

            // Build the hash map: key → Vec<row>
            let mut build_map: std::collections::HashMap<String, Vec<Vec<QueryValue>>>
                = std::collections::HashMap::new();
            for row in build_rows {
                let key = row.get(build_key_idx)
                    .map(|v| format!("{v:?}"))
                    .unwrap_or_default();
                build_map.entry(key).or_default().push(row);
            }

            join_states.push(JoinState {
                build_map,
                build_columns: build_cols,
                kind: jp.kind,
                probe_key_expr: jp.on.lhs.clone(),
                build_key_idx,
            });
        }
        // Inject into executor
        executors[slot].join_states = join_states;
    }

    // … existing scan pass …

    // ── Post-scan: apply JOINs ─────────────────────────────────────────────
    for exec in &mut executors {
        if !exec.join_states.is_empty() {
            exec.apply_joins();
        }
    }
```

Virtual table materializer dispatch:

```rust
fn materialize_vtable(
    name: &str,
    graph: &crate::Graph,
) -> (Vec<QueryColumn>, Vec<Vec<QueryValue>>) {
    match name.to_ascii_lowercase().as_str() {
        "classes"           => vtable::materialize_classes(graph),
        "gc_roots"          => vtable::materialize_gc_roots(graph),
        "threads"           => vtable::materialize_threads(graph),
        "type_ref_pairs"    => vtable::materialize_type_ref_pairs(graph),
        "system_properties" => vtable::materialize_system_properties(graph),
        _ => (Vec::new(), Vec::new()),
    }
}
```

---

### 1.7 Optimizer changes (optimize.rs)

In `optimize()`, recurse into JOIN subquery plans:

```rust
pub fn optimize(plan: &mut QueryPlan, stats: &SchemaStats) {
    // … existing passes …

    // Recurse into JOIN subquery plans.
    for jp in &mut plan.join_plans {
        if let JoinBuildSource::Subquery(ref mut inner_plan, _) = jp.build_source {
            optimize(inner_plan, stats);
        }
    }
}
```

JOIN presence disables `scan_limit` pushdown (same as subqueries):

```rust
fn pushdown_limit(plan: &mut QueryPlan) {
    if plan.order_sensitive
        || !plan.late_ops.is_empty()
        || !plan.union_branches.is_empty()
        || plan.from_subplan.is_some()
        || !plan.in_subplans.is_empty()
        || !plan.join_plans.is_empty()   // NEW: JOIN disables early-stop
    {
        return; // cannot push limit to scan
    }
    plan.scan_limit = plan.limit;
}
```

---

### 1.8 REPL / completion (complete.rs)

Add virtual table names to completions after `JOIN`:

```rust
// In the completion handler, when the cursor follows JOIN:
fn join_completions(prefix: &str) -> Vec<String> {
    let mut completions = Vec::new();
    for &vt in crate::query::vtable::VIRTUAL_TABLES {
        if vt.starts_with(&prefix.to_ascii_lowercase()) {
            completions.push(vt.to_string());
        }
    }
    completions
}
```

Also add `JOIN`, `LEFT`, `INNER`, `ON` to the keyword completion list.

---

### 1.9 Tests

```rust
#[test]
fn join_classes_basic() {
    let q = parse("SELECT o.@displayName, c.instance_count \
                    FROM java.lang.String o \
                    JOIN classes c ON classof(o) = c.class_name").unwrap();
    assert_eq!(q.joins.len(), 1);
    assert!(matches!(q.joins[0].kind, JoinKind::Inner));
    assert!(matches!(q.joins[0].rhs, JoinSource::Name(ref n) if n == "classes"));
}

#[test]
fn join_left_gc_roots() {
    let q = parse("SELECT * FROM INSTANCEOF java.lang.Object o \
                    LEFT JOIN gc_roots r ON o.@objectId = r.dense_idx").unwrap();
    assert_eq!(q.joins.len(), 1);
    assert!(matches!(q.joins[0].kind, JoinKind::Left));
}

#[test]
fn join_unknown_table_rejected() {
    let result = plan_query(
        &parse("SELECT * FROM java.lang.String s \
                JOIN unknown_table t ON s.@objectId = t.id").unwrap(),
        5,
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().0.contains("not a known virtual table"));
}

#[test]
fn join_subquery_rhs() {
    let q = parse("SELECT o.@displayName, sub.cnt \
                    FROM java.lang.String o \
                    JOIN (SELECT classof(x) AS cls, COUNT(*) AS cnt \
                          FROM INSTANCEOF java.lang.Object x \
                          GROUP BY classof(x)) sub \
                    ON classof(o) = sub.cls").unwrap();
    assert_eq!(q.joins.len(), 1);
    assert!(matches!(q.joins[0].rhs, JoinSource::Subquery(_)));
}
```

---

## Phase 2 — LATERAL Table-Valued Functions

**Goal**: `FROM <class> o, LATERAL outbound_refs(o) ref` — per-object
expansion via CSR lookup, composable with other SELECT columns.

**Estimated LOC**: ~350

---

### 2.1 AST changes

Add to `ast.rs`:

```rust
/// A table-valued function for LATERAL expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum LateralFunc {
    /// `outbound_refs(alias)` — forward reference edges from each matched object.
    OutboundRefs,
    /// `inbound_refs(alias)` — inbound reference edges to each matched object.
    InboundRefs,
    /// `dom_children(alias)` — dominator-tree children of each matched object.
    DomChildren,
    /// `entries(alias)` — collection elements (generalized FROM ENTRIES).
    Entries,
}

/// A LATERAL source in a FROM clause.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LateralSource {
    pub func: LateralFunc,
    /// The alias for the parent object (must match the FROM alias).
    pub parent_alias: String,
    /// The alias for the lateral output rows.
    pub alias: String,
}
```

Add to `Query`:

```rust
pub struct Query {
    // … existing fields …
    pub lateral: Option<LateralSource>,
}
```

---

### 2.2 Parser changes

After the FROM alias, parse an optional LATERAL clause:

```rust
// LATERAL keyword is optional when preceded by comma
let lateral_func = ident_ci("outbound_refs").to(LateralFunc::OutboundRefs)
    .or(ident_ci("inbound_refs").to(LateralFunc::InboundRefs))
    .or(ident_ci("dom_children").to(LateralFunc::DomChildren))
    .or(ident_ci("entries").to(LateralFunc::Entries));

let lateral_source = just(Token::Comma)
    .ignore_then(
        ident_ci("LATERAL").or_not()  // optional LATERAL keyword
    )
    .ignore_then(lateral_func)
    .then_ignore(just(Token::LParen))
    .then(select! { Token::Ident(s) => s })  // parent alias
    .then_ignore(just(Token::RParen))
    .then(select! { Token::Ident(s) => s })  // lateral alias
    .map(|((func, parent_alias), alias)| LateralSource {
        func, parent_alias, alias,
    })
    .or_not();
```

Wire into `Query` construction and validate that `parent_alias` matches the
FROM alias:

```rust
if let Some(ref lat) = lateral {
    if q.alias.as_deref() != Some(&lat.parent_alias) {
        // emit validation error: LATERAL parent must match FROM alias
    }
}
q.lateral = lateral;
```

---

### 2.3 Planner changes

Add lateral columns to the query's output schema:

```rust
// In plan_single, when lateral.is_some():
if let Some(lat) = &q.lateral {
    let lateral_columns = lateral_output_columns(lat.func);
    // select_arity += lateral_columns.len();
    // Set needs based on function:
    match lat.func {
        LateralFunc::OutboundRefs => {
            needs.ref_walk = true;  // arms forward CSR
        }
        LateralFunc::InboundRefs => {
            // requires inbound CSR — reject if not available
            needs.ref_walk = true;
        }
        LateralFunc::DomChildren => {
            needs.dominator_children = true;
        }
        LateralFunc::Entries => {
            // already handled by existing ENTRIES machinery
        }
    }
}

fn lateral_output_columns(func: LateralFunc) -> Vec<QueryColumn> {
    match func {
        LateralFunc::OutboundRefs => vec![
            qcol("dst"),
            qcol("field_name"),
            qcol("dst_class"),
            qcol("dst_retained"),
        ],
        LateralFunc::InboundRefs => vec![
            qcol("src"),
            qcol("field_name"),
            qcol("src_class"),
            qcol("src_retained"),
        ],
        LateralFunc::DomChildren => vec![
            qcol("child"),
            qcol("child_class"),
            qcol("child_retained"),
        ],
        LateralFunc::Entries => vec![
            qcol("element"),
            qcol("element_class"),
        ],
    }
}
```

Set `finalize_at` to P2 or P3 depending on the lateral function (forward
CSR is available at P2; dominator children at P3).

---

### 2.4 Executor changes

Add to `SingleScanExecutor`:

```rust
    /// LATERAL expansion function, if any.
    lateral: Option<LateralFunc>,
    /// LATERAL alias for output column qualification.
    lateral_alias: Option<String>,
```

In `visit_instance`, when `lateral.is_some()`, replace the single-row push
with a multi-row expansion loop:

```rust
// This runs AFTER class_matches and where_passes succeed.
if let Some(func) = &self.lateral {
    let lhs_row = self.build_row(src_idx, class_id, blob);

    match func {
        LateralFunc::OutboundRefs => {
            // These lookups happen in P2 (forward CSR is live).
            // At P1 time, just carry the dense index.
            // The actual expansion happens in stage_runner.rs.
            if let Some(carry) = &mut self.carry {
                carry.push(src_idx as u32);
            }
        }
        // … other variants …
    }
    return;
}
```

---

### 2.5 Stage runner integration

In `stage_runner.rs`, add a new `StageOp` variant:

```rust
StageOp::LateralExpand { func: LateralFunc }
```

In the P2 late-window loop, when this op is encountered:

```rust
StageOp::LateralExpand { func } => {
    let mut expanded_rows = Vec::new();
    for (row, src_idx) in rows.iter().zip(carry_indices.iter()) {
        match func {
            LateralFunc::OutboundRefs => {
                let start = graph.fwd_offsets[*src_idx as usize] as usize;
                let end = graph.fwd_offsets[*src_idx as usize + 1] as usize;
                for edge_idx in start..end {
                    let dst = graph.fwd_targets.get(edge_idx) as usize;
                    let field_name = graph.fwd_field_name_idx
                        .as_ref()
                        .and_then(|fi| graph.field_name_pool.as_ref()
                            .map(|pool| pool[fi[edge_idx] as usize].as_str()))
                        .unwrap_or("");
                    let dst_class = &graph.class_names[graph.class_idx[dst] as usize];
                    let dst_ret = if !graph.retained.is_empty() {
                        QueryValue::Int(graph.retained[dst] as i64)
                    } else {
                        QueryValue::Null
                    };

                    let mut new_row = row.clone();
                    new_row.push(QueryValue::Int(dst as i64));
                    new_row.push(QueryValue::Str(field_name.to_string()));
                    new_row.push(QueryValue::Str(dst_class.to_string()));
                    new_row.push(dst_ret);
                    expanded_rows.push(new_row);

                    if expanded_rows.len() >= LATERAL_ROW_CAP {
                        truncated = true;
                        break;
                    }
                }
            }
            LateralFunc::DomChildren => {
                // Uses dc_off/dc_tgt (built from idom[])
                let start = dc_off[*src_idx as usize] as usize;
                let end = dc_off[*src_idx as usize + 1] as usize;
                for ci in start..end {
                    let child = dc_tgt[ci] as usize;
                    let child_class = &graph.class_names[graph.class_idx[child] as usize];
                    let child_ret = if !graph.retained.is_empty() {
                        QueryValue::Int(graph.retained[child] as i64)
                    } else {
                        QueryValue::Null
                    };
                    let mut new_row = row.clone();
                    new_row.push(QueryValue::Int(child as i64));
                    new_row.push(QueryValue::Str(child_class.to_string()));
                    new_row.push(child_ret);
                    expanded_rows.push(new_row);

                    if expanded_rows.len() >= LATERAL_ROW_CAP {
                        truncated = true;
                        break;
                    }
                }
            }
            _ => { /* InboundRefs, Entries — similar patterns */ }
        }
        if truncated { break; }
    }
    rows = expanded_rows;
}

const LATERAL_ROW_CAP: usize = 1_000_000;
```

---

### 2.6 RunFlags changes

In `runflags.rs`, `accumulate_query` must scan the LATERAL source:

```rust
fn accumulate_query(q: &Query, class_index: &dyn ClassIndexResolver, flags: &mut RunFlags) {
    // … existing select/where scanning …

    // NEW: LATERAL functions may require edge CSRs
    if let Some(lat) = &q.lateral {
        match lat.func {
            LateralFunc::OutboundRefs => {
                flags.retain_forward = true;
            }
            LateralFunc::InboundRefs => {
                flags.retain_inbound = true;
            }
            LateralFunc::DomChildren => {
                // dominator children built from idom[] — no CSR retention needed
            }
            LateralFunc::Entries => {
                // uses existing entries machinery
            }
        }
    }
}
```

---

### 2.7 Tests

```rust
#[test]
fn lateral_outbound_refs_parse() {
    let q = parse("SELECT o.@displayName, ref.field_name, ref.dst_class \
                    FROM java.util.HashMap o, LATERAL outbound_refs(o) ref \
                    WHERE ref.dst_retained > 1048576 \
                    ORDER BY ref.dst_retained DESC LIMIT 100").unwrap();
    assert!(q.lateral.is_some());
    let lat = q.lateral.as_ref().unwrap();
    assert_eq!(lat.func, LateralFunc::OutboundRefs);
    assert_eq!(lat.parent_alias, "o");
    assert_eq!(lat.alias, "ref");
}

#[test]
fn lateral_dom_children_parse() {
    let q = parse("SELECT p.@displayName, c.child_class, c.child_retained \
                    FROM INSTANCEOF java.lang.Object p, LATERAL dom_children(p) c \
                    WHERE p.@retainedHeapSize > 10485760 \
                    ORDER BY c.child_retained DESC LIMIT 50").unwrap();
    assert!(q.lateral.is_some());
    assert_eq!(q.lateral.as_ref().unwrap().func, LateralFunc::DomChildren);
}

#[test]
fn lateral_without_keyword() {
    // LATERAL keyword is optional after comma
    let q = parse("SELECT * FROM java.lang.Object o, outbound_refs(o) r").unwrap();
    assert!(q.lateral.is_some());
}

#[test]
fn lateral_parent_alias_mismatch_rejected() {
    let result = parse("SELECT * FROM java.lang.Object o, LATERAL outbound_refs(x) r");
    assert!(result.is_err() || {
        let q = result.unwrap();
        plan_query(&q, 5).is_err()
    });
}
```

---

## Phase 3 — General Subquery JOINs

**Goal**: Allow `JOIN (SELECT ... FROM <class> ...) alias ON ...` with
a full subquery as the RHS.  This is already supported by Phase 1's
`JoinSource::Subquery` variant — Phase 3 adds multi-join chaining and
Left Join NULL propagation.

**Estimated LOC**: ~100 (incremental over Phase 1)

---

### 3.1 Parser generalization

The Phase 1 parser already accepts `JoinSource::Subquery`.  Phase 3 adds:

**Multi-table shorthand** — allow `FROM ClassA a JOIN ClassB b ON ...` as
sugar for `FROM ClassA a JOIN (SELECT * FROM ClassB) b ON ...`:

```rust
// In the join_rhs parser, add a bare class name path:
let join_rhs = just(Token::LParen)
    .ignore_then(base_query.clone())
    .then_ignore(just(Token::RParen))
    .map(|q| JoinSource::Subquery(Box::new(q)))
    .or(
        // Virtual table or bare class name
        select! { Token::Ident(s) => s }
            .map(JoinSource::Name)
    );
```

At plan time, `JoinSource::Name` that isn't a virtual table is now
auto-wrapped into a `SELECT * FROM <class>` subquery:

```rust
JoinSource::Name(name) => {
    if vtable::is_virtual_table(name) {
        (JoinBuildSource::VirtualTable(name.clone()), vtable::vtable_columns(name))
    } else {
        // Auto-wrap as: SELECT * FROM <name>
        let synthetic_inner = Query {
            select: vec![SelectItem::Star],
            from: FromSource::Class(ClassSpec {
                instanceof: false,
                class_name: name.clone(),
                is_regex: false,
            }),
            // … all other fields default …
            ..Query::default()
        };
        let inner_plan = plan_query(&synthetic_inner, depth_cap)?;
        let cols = build_columns_from_query(&synthetic_inner);
        (
            JoinBuildSource::Subquery(Box::new(inner_plan), Box::new(synthetic_inner)),
            cols,
        )
    }
}
```

---

### 3.2 Multi-JOIN chaining

The Phase 1 executor already handles `Vec<JoinState>` and applies them
sequentially.  For correctness with multiple JOINs, ensure column indices
are updated after each join application:

```rust
pub(crate) fn apply_joins(&mut self) {
    let mut total_cols = self.base_columns();

    for js in &self.join_states {
        let probe_key_idx = find_column_idx(&total_cols, &js.probe_key_expr);
        let mut joined = Vec::new();

        for probe_row in &self.rows {
            let key = probe_row
                .get(probe_key_idx)
                .map(|v| format!("{v:?}"))
                .unwrap_or_default();

            if let Some(build_rows) = js.build_map.get(&key) {
                for build_row in build_rows {
                    let mut row = probe_row.clone();
                    row.extend(build_row.iter().cloned());
                    joined.push(row);
                }
            } else if matches!(js.kind, JoinKind::Left) {
                let mut row = probe_row.clone();
                row.extend(
                    std::iter::repeat(QueryValue::Null)
                        .take(js.build_columns.len())
                );
                joined.push(row);
            }
        }

        self.rows = joined;
        // Extend column metadata for the next join in the chain
        total_cols.extend(js.build_columns.iter().cloned());
    }
}
```

---

### 3.3 Left JOIN semantics

Left JOIN produces NULL-padded rows for unmatched probe rows.  This is
already handled in `apply_joins` above (the `JoinKind::Left` branch).

For WHERE clause evaluation on Left JOIN results, NULL build-side columns
must be handled gracefully — the existing `QueryValue::Null` comparison
semantics already do this (Null != anything, Null sorts first).

Add a note in the executor to skip post-join WHERE evaluation for
build-side columns when the row is NULL-padded (or let the existing
NULL comparison handle it naturally).

---

### 3.4 Tests

```rust
#[test]
fn multi_join() {
    let q = parse(
        "SELECT o.@displayName, c.instance_count, r.root_type_name \
         FROM java.lang.Object o \
         JOIN classes c ON classof(o) = c.class_name \
         LEFT JOIN gc_roots r ON o.@objectId = r.dense_idx"
    ).unwrap();
    assert_eq!(q.joins.len(), 2);
    assert!(matches!(q.joins[0].kind, JoinKind::Inner));
    assert!(matches!(q.joins[1].kind, JoinKind::Left));
}

#[test]
fn join_bare_class_auto_wrapped() {
    // Bare class name → auto-wrapped as SELECT * FROM <class>
    let q = parse(
        "SELECT s.@displayName, t.@displayName \
         FROM java.lang.String s \
         JOIN java.lang.Thread t ON s.@objectId = t.@objectId"
    ).unwrap();
    assert_eq!(q.joins.len(), 1);
    assert!(matches!(q.joins[0].rhs, JoinSource::Name(ref n) if n == "java.lang.Thread"));
}

#[test]
fn left_join_null_padding() {
    // Execution test: Left JOIN should produce NULL for unmatched rows
    // (requires a mock Graph / resolver)
}
```

---

## File Change Summary

| File | Phase | Changes |
|------|-------|---------|
| `src/query/ast.rs` | 1, 2 | `JoinKind`, `JoinOn`, `JoinClause`, `JoinSource`, `LateralFunc`, `LateralSource`; extend `Query` |
| `src/query/parse.rs` | 1, 2, 3 | JOIN clause parser, LATERAL parser, ON condition parser |
| `src/query/vtable.rs` | 1 | **New file**: virtual table catalog + materializers |
| `src/query/mod.rs` | 1 | Register `vtable` module |
| `src/query/plan.rs` | 1, 2 | `JoinPlan`, `JoinBuildSource`; extend `QueryPlan`; JOIN validation in `plan_single` |
| `src/query/execute.rs` | 1, 2 | `JoinState`; extend `SingleScanExecutor` with `join_states`, `lateral`; `apply_joins()` |
| `src/query/run.rs` | 1, 2 | Build-side materialization; virtual table dispatch; post-scan join application |
| `src/query/stage_runner.rs` | 2 | `StageOp::LateralExpand`; per-object CSR expansion loop |
| `src/query/optimize.rs` | 1 | Recurse into `join_plans`; block `scan_limit` for JOINs |
| `src/query/runflags.rs` | 2 | Scan LATERAL for edge CSR retention flags |
| `src/query/complete.rs` | 1 | Virtual table name completions after `JOIN` |

**Total estimated LOC**: ~870 across all three phases.

---

## Merge Order

1. **Phase 1a**: AST + parser for JOIN syntax (no execution) — testable immediately
2. **Phase 1b**: Virtual table catalog (`vtable.rs`) — testable independently
3. **Phase 1c**: Planner + executor + run-driver for virtual table JOINs — end-to-end
4. **Phase 2a**: AST + parser for LATERAL syntax
5. **Phase 2b**: Planner + stage_runner lateral expansion
6. **Phase 3**: General subquery JOINs + multi-join chaining (incremental)
