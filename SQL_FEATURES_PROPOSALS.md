# Missing SQL Features — Draft Implementations for hprof-analyzer OQL

Gap analysis and implementation drafts for SQL features not yet supported in
the OQL engine. Each proposal includes motivation, heap-analysis use cases,
AST changes, parser changes, executor changes, and an implementation sketch
against the current codebase.

Existing engine reference points (line numbers approximate — files shift):

| Component | File | Key structures |
|-----------|------|----------------|
| AST | `src/query/ast.rs` | `Query`, `OrderBy`, `AggFunc`, `ArithOp`, `Expr`, `Predicate` |
| Parser | `src/query/parse.rs` | logos lexer `Token`, chumsky combinators, `agg_func()` helper |
| Executor | `src/query/execute.rs` | `SingleScanExecutor`, `AggAcc`, `arith()`, `sort_rows_by_column()`, `total_cmp_query_value()` |
| Planner | `src/query/plan.rs` | `QueryPlan`, `QueryNeeds`, `StageKind`, `LateOp` |
| Optimizer | `src/query/optimize.rs` | `pushdown_limit`, `defer_projections`, `eliminate_dead_needs` |
| Stage runner | `src/query/stage_runner.rs` | Late-phase P2/P3 execution, `join_retained_group_by` |
| Run harness | `src/query/run.rs` | `collapse_union_results`, UNION/INTERSECT/EXCEPT assembly |

---

## Table of Contents

| # | Feature | Priority | Est. LOC |
|---|---------|----------|----------|
| 1 | [Multi-key ORDER BY](#1-multi-key-order-by) | **P0** | ~60 |
| 2 | [Modulo `%` operator](#2-modulo--operator) | **P0** | ~15 |
| 3 | [COUNT(DISTINCT expr)](#3-countdistinct-expr) | **P0** | ~50 |
| 4 | [String functions](#4-string-functions-lower-upper-substr-length-trim-concat-replace) | **P1** | ~120 |
| 5 | [HAVING with group-key references](#5-having-with-group-key-references) | **P1** | ~30 |
| 6 | [Array index/slice in WHERE predicates](#6-array-indexslice-in-where-predicates) | **P1** | ~50 |
| 7 | [Window functions: ROW_NUMBER, RANK, DENSE_RANK](#7-window-functions-row_number-rank-dense_rank) | **P1** | ~400 |
| 8 | [LAG / LEAD window functions](#8-lag--lead-window-functions) | **P2** | ~120 |
| 9 | [Conditional aggregates (FILTER clause)](#9-conditional-aggregates-filter-clause) | **P2** | ~70 |
| 10 | [Correlated EXISTS subquery](#10-correlated-exists-subquery) | **P2** | ~250 |
| 11 | [WITH / CTE (Common Table Expressions)](#11-with--cte-common-table-expressions) | **P3** | ~180 |
| 12 | [CAST() type conversion](#12-cast-type-conversion) | **P3** | ~50 |

---

## 1. Multi-key ORDER BY

### Motivation

Single-key ORDER BY means you can't write
`ORDER BY @category ASC, @retainedHeapSize DESC`.  You must choose one or
accept unsorted secondary order.

### Use cases

```sql
-- Objects by category, largest first within each
SELECT @displayName, @category, @retainedHeapSize
FROM INSTANCEOF java.lang.Object
ORDER BY @category ASC, @retainedHeapSize DESC
LIMIT 100

-- Class histogram: break ties by count
SELECT @dominatorClass, COUNT(*) AS n, SUM(@usedHeapSize) AS total
FROM byte[]
GROUP BY @dominatorClass
ORDER BY total DESC, n DESC
```

### AST changes (`ast.rs`)

```rust
// BEFORE
pub struct OrderBy {
    pub key: Attr,
    pub dir: SortDir,
}

// AFTER — Vec of key+dir pairs, minimum 1 element.
pub struct OrderBy {
    pub keys: Vec<(Attr, SortDir)>,
}
```

Update `Query.order_by: Option<OrderBy>` — no change to the `Option` wrapper.

### Parser changes (`parse.rs`)

Replace the single-key ORDER BY combinator:

```rust
// BEFORE (≈ line 1128)
ident_ci("ORDER")
    .ignore_then(ident_ci("BY"))
    .ignore_then(attr.clone())
    .then(
        ident_ci("ASC").to(SortDir::Asc)
            .or(ident_ci("DESC").to(SortDir::Desc))
            .or_not().map(|d| d.unwrap_or(SortDir::Asc)),
    )
    .map(|(key, dir)| OrderBy { key, dir })
    .or_not()

// AFTER — comma-separated list of (attr, dir)
ident_ci("ORDER")
    .ignore_then(ident_ci("BY"))
    .ignore_then(
        attr.clone()
            .then(
                ident_ci("ASC").to(SortDir::Asc)
                    .or(ident_ci("DESC").to(SortDir::Desc))
                    .or_not().map(|d| d.unwrap_or(SortDir::Asc)),
            )
            .separated_by(just(Token::Comma))
            .at_least(1)
            .collect::<Vec<_>>()
    )
    .map(|keys| OrderBy { keys })
    .or_not()
```

Post-parse normalization loop:

```rust
// BEFORE
if let Some(ob) = &mut q.order_by {
    normalize_attr(&mut ob.key, alias.as_deref());
}

// AFTER
if let Some(ob) = &mut q.order_by {
    for (key, _dir) in &mut ob.keys {
        normalize_attr(key, alias.as_deref());
    }
}
```

### Executor changes (`execute.rs`)

Replace `sort_rows_by_column` call site and the function itself:

```rust
/// Stable multi-key sort. Keys are tried left-to-right; the first non-Equal
/// comparison decides the row order. Falls back to scan order for equal rows.
pub(crate) fn sort_rows_multi_key(
    rows: &mut [Vec<QueryValue>],
    keys: &[(usize, SortDir)],  // (column_index, direction)
) {
    rows.sort_by(|a, b| {
        for &(idx, dir) in keys {
            let av = a.get(idx).unwrap_or(&QueryValue::Null);
            let bv = b.get(idx).unwrap_or(&QueryValue::Null);
            let ord = total_cmp_query_value(av, bv);
            let ord = match dir {
                SortDir::Asc  => ord,
                SortDir::Desc => ord.reverse(),
            };
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });
}
```

Call site in `finish()` (and the late-phase sort paths):

```rust
// BEFORE
if let Some(ob) = &self.query.order_by {
    if let Some(idx) = order_by_column_index(self.query, &columns, &ob.key) {
        sort_rows_by_column(&mut rows, idx, ob.dir);
    }
}

// AFTER
if let Some(ob) = &self.query.order_by {
    let resolved: Vec<(usize, SortDir)> = ob.keys.iter()
        .filter_map(|(key, dir)| {
            order_by_column_index(self.query, &columns, key)
                .map(|idx| (idx, *dir))
        })
        .collect();
    if !resolved.is_empty() {
        sort_rows_multi_key(&mut rows, &resolved);
    }
}
```

### Planner changes (`plan.rs`)

`order_sensitive` stays `q.order_by.is_some()` — no logic change.

Field validation loop updates to iterate `ob.keys`:

```rust
if let Some(ob) = &q.order_by {
    for (key, _dir) in &ob.keys {
        if let Attr::Field(name) = key {
            let is_alias = q.select_aliases.iter()
                .any(|a| a.as_deref() == Some(name.as_str()));
            if !is_alias {
                referenced.push(name.clone());
            }
        }
    }
}
```

### `sort_rows_with_src` variant

Same pattern — sort a permutation by multi-key comparator, then reorder both
`rows` and `src`:

```rust
pub(crate) fn sort_rows_with_src_multi_key(
    rows: &mut Vec<Vec<QueryValue>>,
    src: &mut Option<Vec<u32>>,
    keys: &[(usize, SortDir)],
) {
    let Some(src_vec) = src.as_mut() else {
        sort_rows_multi_key(rows, keys);
        return;
    };
    let mut perm: Vec<usize> = (0..rows.len()).collect();
    perm.sort_by(|&a, &b| {
        for &(idx, dir) in keys {
            let av = rows[a].get(idx).unwrap_or(&QueryValue::Null);
            let bv = rows[b].get(idx).unwrap_or(&QueryValue::Null);
            let ord = total_cmp_query_value(av, bv);
            let ord = match dir {
                SortDir::Asc  => ord,
                SortDir::Desc => ord.reverse(),
            };
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });
    let new_rows: Vec<Vec<QueryValue>> =
        perm.iter().map(|&i| std::mem::take(&mut rows[i])).collect();
    let new_src: Vec<u32> = perm.iter().map(|&i| src_vec[i]).collect();
    *rows = new_rows;
    *src_vec = new_src;
}
```

### Estimate: ~60 LOC

---

## 2. Modulo `%` Operator

### Motivation

Enables bucketing, alignment checks, and remainder-based filtering.
Integer division already works for bucketing, but modulo completes the picture.

### Use cases

```sql
-- Retained-size distribution in 1 KB buckets
SELECT (@retainedHeapSize / 1024) AS bucket_kb, COUNT(*) AS n
FROM INSTANCEOF java.lang.Object
GROUP BY @retainedHeapSize / 1024
ORDER BY bucket_kb

-- Objects at non-8-byte-aligned addresses (unusual)
SELECT @objectAddress, @displayName
FROM INSTANCEOF java.lang.Object
WHERE @objectAddress % 8 != 0
LIMIT 20
```

### AST changes (`ast.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,  // NEW
}
```

### Lexer changes (`parse.rs`)

Add a `Percent` token to the logos lexer:

```rust
#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t\r\n]+")]
pub enum Token {
    // … existing tokens …
    #[token("%")]
    Percent,  // NEW
    // …
}
```

Update `token_display`:

```rust
Token::Percent => "'%'".into(),
```

### Parser changes (`parse.rs`)

Add `%` at the same precedence tier as `*` and `/`:

```rust
// BEFORE — Tier 1: * and /
let mul = unary.clone().foldl(
    just(Token::Star).to(ArithOp::Mul)
        .or(just(Token::Divide).to(ArithOp::Div))
        .then(unary)
        .repeated(),
    |lhs, (op, rhs)| …,
);

// AFTER — Tier 1: *, /, %
let mul = unary.clone().foldl(
    just(Token::Star).to(ArithOp::Mul)
        .or(just(Token::Divide).to(ArithOp::Div))
        .or(just(Token::Percent).to(ArithOp::Mod))  // NEW
        .then(unary)
        .repeated(),
    |lhs, (op, rhs)| …,
);
```

### Executor changes (`execute.rs`)

Extend the `arith()` function:

```rust
pub(crate) fn arith(lhs: &QueryValue, op: ArithOp, rhs: &QueryValue) -> QueryValue {
    match (lhs, rhs) {
        (QueryValue::Int(a), QueryValue::Int(b)) => match op {
            // … existing Add/Sub/Mul/Div …
            ArithOp::Mod => {
                if *b == 0 { QueryValue::Null }
                else { QueryValue::Int(a.wrapping_rem(*b)) }
            }
        },
        (QueryValue::Float(a), QueryValue::Float(b)) => match op {
            // … existing …
            ArithOp::Mod => QueryValue::Float(a % b),  // IEEE 754 remainder
        },
        // Int×Float promotions: same pattern as existing
        (QueryValue::Int(a), QueryValue::Float(b)) => {
            let a = *a as f64;
            match op {
                // … existing …
                ArithOp::Mod => QueryValue::Float(a % b),
            }
        }
        (QueryValue::Float(a), QueryValue::Int(b)) => {
            let b = *b as f64;
            match op {
                // … existing …
                ArithOp::Mod => QueryValue::Float(a % b),
            }
        }
        _ => QueryValue::Null,
    }
}
```

### Estimate: ~15 LOC

---

## 3. COUNT(DISTINCT expr)

### Motivation

Cardinality queries: "how many distinct classes dominate byte[]?", "how many
unique superclass chains exist?"

### Use cases

```sql
SELECT COUNT(DISTINCT @dominatorClass) AS distinct_dominators
FROM byte[]

SELECT @superclass, COUNT(DISTINCT classof(o)) AS subclass_count
FROM INSTANCEOF java.lang.Object o
GROUP BY @superclass
ORDER BY subclass_count DESC LIMIT 20
```

### AST changes (`ast.rs`)

Extend `AggFunc` with a distinct flag:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AggFunc {
    Count,
    CountDistinct,  // NEW
    Sum,
    Min,
    Max,
    Avg,
    Percentile(u8),
    Median,
}
```

Alternatively, add a `distinct: bool` flag to the `Aggregate` select item:

```rust
// Alternative (less disruptive to match arms):
pub enum SelectItem {
    // …
    Aggregate {
        func: AggFunc,
        arg: Box<SelectItem>,
        distinct: bool,  // NEW — only meaningful for Count
    },
}
```

The enum-variant approach is simpler to pattern-match; the flag approach is
more extensible (SUM(DISTINCT …) later). Either works; the enum approach is
shown below.

### Parser changes (`parse.rs`)

Extend the aggregate combinator to recognize `COUNT(DISTINCT …)`:

```rust
// NEW: COUNT(DISTINCT expr) — parsed before the generic agg combinator
let count_distinct = ident_ci("COUNT")
    .ignore_then(just(Token::LParen))
    .ignore_then(ident_ci("DISTINCT"))
    .ignore_then(item.clone())
    .then_ignore(just(Token::RParen))
    .map(|(arg, _alias): (SelectItem, Option<String>)| {
        (
            SelectItem::Aggregate {
                func: AggFunc::CountDistinct,
                arg: Box::new(arg),
            },
            None::<String>,
        )
    });

// Add to the select item alternatives:
// count_distinct.or(percentile_item).or(agg).or(…)
```

### Executor changes (`execute.rs`)

Add a new `AggAcc` variant:

```rust
pub(crate) enum AggAcc {
    // … existing …
    /// COUNT(DISTINCT expr): collects unique non-Null value representations
    /// in a HashSet, then returns the set cardinality.
    CountDistinct { seen: std::collections::HashSet<String> },
}
```

Init:

```rust
AggFunc::CountDistinct => AggAcc::CountDistinct {
    seen: std::collections::HashSet::new(),
},
```

Fold:

```rust
AggAcc::CountDistinct { seen } => {
    if !matches!(value, QueryValue::Null) {
        // Use the Debug repr as the hash key (same trick as GROUP BY
        // bucket keys — avoids f64 Hash issues while covering all types).
        seen.insert(format!("{value:?}"));
    }
}
```

Finalize:

```rust
AggAcc::CountDistinct { seen } => QueryValue::Int(seen.len() as i64),
```

### Late-phase (`stage_runner.rs`)

In `join_retained_group_by`, add the `CountDistinct` arm following the
existing `CountStar`/`CountExpr` pattern:

```rust
AggFunc::CountDistinct => {
    // In the late-phase re-aggregation, the per-bucket HashSet is
    // not available (the carry only holds indices). Collect the
    // projected value string for each carried index.
    let mut seen = std::collections::HashSet::new();
    for &idx in bucket_indices {
        let v = project_late_value(idx, &agg_arg, graph, …);
        if !matches!(v, QueryValue::Null) {
            seen.insert(format!("{v:?}"));
        }
    }
    QueryValue::Int(seen.len() as i64)
}
```

### Estimate: ~50 LOC

---

## 4. String Functions: LOWER, UPPER, SUBSTR, LENGTH, TRIM, CONCAT, REPLACE

### Motivation

String analysis is critical for heap dumps — inspecting String contents for
secrets, filtering by patterns, previewing large values.

### Use cases

```sql
-- Case-insensitive search for passwords
SELECT toString(s), @usedHeapSize
FROM java.lang.String s
WHERE LOWER(toString(s)) LIKE ".*password.*"

-- Preview first 50 chars of large Strings
SELECT SUBSTR(toString(s), 0, 50) AS preview, @usedHeapSize
FROM java.lang.String s
WHERE @usedHeapSize > 10000
ORDER BY @usedHeapSize DESC LIMIT 20

-- String length distribution
SELECT LENGTH(toString(s)) AS len, COUNT(*) AS n
FROM java.lang.String s
GROUP BY LENGTH(toString(s))
ORDER BY n DESC LIMIT 20
```

### AST changes (`ast.rs`)

Add scalar function variants to `Expr`:

```rust
pub enum Expr {
    // … existing variants …

    /// A built-in scalar function call: `LOWER(expr)`, `UPPER(expr)`, etc.
    ScalarFunc {
        func: ScalarFuncKind,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ScalarFuncKind {
    Lower,
    Upper,
    /// SUBSTR(str, start) or SUBSTR(str, start, len)
    Substr,
    /// LENGTH(str) — character count (not byte count)
    Length,
    Trim,
    LTrim,
    RTrim,
    /// CONCAT(a, b, …) — 2+ args
    Concat,
    /// REPLACE(str, from, to)
    Replace,
}
```

### Parser changes (`parse.rs`)

Add a scalar function recognizer similar to the `agg_func()` helper:

```rust
fn scalar_func(s: &str) -> Option<(ScalarFuncKind, usize, usize)> {
    // Returns (kind, min_args, max_args)
    match () {
        _ if s.eq_ignore_ascii_case("LOWER")   => Some((ScalarFuncKind::Lower, 1, 1)),
        _ if s.eq_ignore_ascii_case("UPPER")   => Some((ScalarFuncKind::Upper, 1, 1)),
        _ if s.eq_ignore_ascii_case("SUBSTR")  => Some((ScalarFuncKind::Substr, 2, 3)),
        _ if s.eq_ignore_ascii_case("SUBSTRING") => Some((ScalarFuncKind::Substr, 2, 3)),
        _ if s.eq_ignore_ascii_case("LENGTH")  => Some((ScalarFuncKind::Length, 1, 1)),
        _ if s.eq_ignore_ascii_case("TRIM")    => Some((ScalarFuncKind::Trim, 1, 1)),
        _ if s.eq_ignore_ascii_case("LTRIM")   => Some((ScalarFuncKind::LTrim, 1, 1)),
        _ if s.eq_ignore_ascii_case("RTRIM")   => Some((ScalarFuncKind::RTrim, 1, 1)),
        _ if s.eq_ignore_ascii_case("CONCAT")  => Some((ScalarFuncKind::Concat, 2, 16)),
        _ if s.eq_ignore_ascii_case("REPLACE") => Some((ScalarFuncKind::Replace, 3, 3)),
        _ => None,
    }
}
```

Parse combinator (inside the `expr` atom layer, before the method-call
fallthrough):

```rust
let scalar_fn = select! {
    Token::Ident(s) if scalar_func(&s).is_some() => scalar_func(&s).unwrap(),
}
.then_ignore(just(Token::LParen))
.then(
    expr.clone()
        .separated_by(just(Token::Comma))
        .at_least(1)
        .collect::<Vec<_>>()
)
.then_ignore(just(Token::RParen))
.validate(|((kind, min, max), args): ((ScalarFuncKind, usize, usize), Vec<Expr>), e, emitter| {
    if args.len() < min || args.len() > max {
        emitter.emit(Rich::custom(
            e.span(),
            format!("{kind:?} expects {min}–{max} arguments, got {}", args.len()),
        ));
    }
    Expr::ScalarFunc { func: kind, args }
});
```

### Executor changes (`execute.rs`)

Add `eval_scalar_func` alongside the existing `arith()`:

```rust
pub(crate) fn eval_scalar_func(
    func: &ScalarFuncKind,
    args: &[QueryValue],
) -> QueryValue {
    use ScalarFuncKind::*;
    match func {
        Lower => match args.first() {
            Some(QueryValue::Str(s)) => QueryValue::Str(s.to_lowercase()),
            _ => QueryValue::Null,
        },
        Upper => match args.first() {
            Some(QueryValue::Str(s)) => QueryValue::Str(s.to_uppercase()),
            _ => QueryValue::Null,
        },
        Length => match args.first() {
            Some(QueryValue::Str(s)) => QueryValue::Int(s.chars().count() as i64),
            _ => QueryValue::Null,
        },
        Trim => match args.first() {
            Some(QueryValue::Str(s)) => QueryValue::Str(s.trim().to_string()),
            _ => QueryValue::Null,
        },
        LTrim => match args.first() {
            Some(QueryValue::Str(s)) => QueryValue::Str(s.trim_start().to_string()),
            _ => QueryValue::Null,
        },
        RTrim => match args.first() {
            Some(QueryValue::Str(s)) => QueryValue::Str(s.trim_end().to_string()),
            _ => QueryValue::Null,
        },
        Substr => {
            let s = match args.get(0) {
                Some(QueryValue::Str(s)) => s,
                _ => return QueryValue::Null,
            };
            let start = match args.get(1) {
                Some(QueryValue::Int(i)) => (*i).max(0) as usize,
                _ => return QueryValue::Null,
            };
            let chars: Vec<char> = s.chars().collect();
            if start >= chars.len() {
                return QueryValue::Str(String::new());
            }
            let end = match args.get(2) {
                Some(QueryValue::Int(len)) => {
                    let len = (*len).max(0) as usize;
                    (start + len).min(chars.len())
                }
                _ => chars.len(),
            };
            QueryValue::Str(chars[start..end].iter().collect())
        }
        Concat => {
            let mut result = String::new();
            for arg in args {
                match arg {
                    QueryValue::Str(s) => result.push_str(s),
                    QueryValue::Int(i) => result.push_str(&i.to_string()),
                    QueryValue::Float(f) => result.push_str(&f.to_string()),
                    QueryValue::Bool(b) => result.push_str(if *b { "true" } else { "false" }),
                    QueryValue::Null => {} // skip nulls (SQL CONCAT semantics)
                    QueryValue::ObjRef { .. } => result.push_str("<obj>"),
                }
            }
            QueryValue::Str(result)
        }
        Replace => {
            let s = match args.get(0) {
                Some(QueryValue::Str(s)) => s,
                _ => return QueryValue::Null,
            };
            let from = match args.get(1) {
                Some(QueryValue::Str(s)) => s,
                _ => return QueryValue::Null,
            };
            let to = match args.get(2) {
                Some(QueryValue::Str(s)) => s,
                _ => return QueryValue::Null,
            };
            QueryValue::Str(s.replace(from.as_str(), to.as_str()))
        }
    }
}
```

Wire into `eval_expr`:

```rust
// In eval_expr's match arm for Expr variants:
Expr::ScalarFunc { func, args } => {
    let evaled: Vec<QueryValue> = args.iter()
        .map(|a| self.eval_expr(a, src_idx, class_id, blob))
        .collect();
    eval_scalar_func(func, &evaled)
}
```

### Estimate: ~120 LOC

---

## 5. HAVING with Group-Key References

### Motivation

Currently HAVING only supports aggregate expressions
(`HAVING COUNT(*) > 10`).  Filtering on the GROUP BY key itself requires a
subquery workaround.

### Use case

```sql
-- Retained by class, excluding JDK, over 1 MB
SELECT classof(o) AS class, SUM(@retainedHeapSize) AS total
FROM INSTANCEOF java.lang.Object o
GROUP BY classof(o)
HAVING total > 1048576 AND classof(o) NOT LIKE "java\..*"
```

### Implementation

The HAVING predicate is already parsed as a `Predicate` tree and evaluated in
`eval_having_term()`. The issue is that non-aggregate expressions in HAVING
are rejected at plan time (or resolve to Null because the group key value
isn't in scope during HAVING evaluation).

**Planner fix**: In `plan_having()`, allow references that match a GROUP BY
key expression (not just aggregate expressions). Tag them as `GroupKeyRef(idx)`
pointing into the `group_by` expression list.

**Executor fix**: In `eval_having_term()`, when encountering a `GroupKeyRef(idx)`,
look up the group's key vector at index `idx`:

```rust
// In the HAVING evaluation loop (execute.rs, finish() for GROUP BY):
for (_key_str, (key_values, accs)) in &group_map {
    let finalized = …;
    // Evaluate HAVING with both agg results AND group key values in scope
    let having_ctx = HavingContext {
        group_keys: &key_values,
        agg_values: &finalized,
    };
    if eval_having_pred(&self.query.having, &having_ctx) {
        rows.push(…);
    }
}

fn eval_having_pred(pred: &Predicate, ctx: &HavingContext) -> bool {
    match pred {
        Predicate::Compare { lhs, op, rhs } => {
            let lv = eval_having_expr(lhs, ctx);
            let rv = eval_having_expr(rhs, ctx);
            compare_values(&lv, *op, &rv)
        }
        // … And, Or, Not …
    }
}

fn eval_having_expr(expr: &Expr, ctx: &HavingContext) -> QueryValue {
    match expr {
        Expr::Aggregate { func, arg } => {
            // Look up in ctx.agg_values by position
            …
        }
        Expr::Attr(attr) => {
            // Check if attr matches a GROUP BY key
            if let Some(idx) = find_group_key_index(attr, &plan.group_by_exprs) {
                ctx.group_keys[idx].clone()
            } else {
                QueryValue::Null
            }
        }
        // … other expr forms (ScalarFunc, etc.) …
    }
}
```

### Estimate: ~30 LOC

---

## 6. Array Index/Slice in WHERE Predicates

### Motivation

Currently `base[i]` is explicitly rejected in WHERE with:
`"array indexing is not supported in WHERE predicates"`.  This prevents
filtering on string characters or array elements.

### Use case

```sql
-- Strings starting with 'H'
SELECT toString(s), @usedHeapSize
FROM java.lang.String s
WHERE s.value[0] = 72     -- 'H' == 72

-- Object arrays with a null first element
SELECT @displayName
FROM java.lang.Object[] a
WHERE a[0] IS NULL
```

### Implementation

The restriction exists because array index resolution requires the P2 forward
reference CSR (for object arrays) or the disk-based `objread` path (for
primitive arrays). The WHERE predicate evaluation runs at P1 scan time for
most predicates, but the engine already supports **deferred WHERE passes** at
P2 for `toString(s)` predicates.

The fix uses the same deferred-predicate mechanism:

**Planner change** (`plan.rs`): Remove the hard rejection. Instead, when a
WHERE predicate contains `Attr::ArrayIndex` or `Attr::ArraySlice`, flag it
as a deferred P2 predicate (same as `toString` predicates):

```rust
// In the predicate analysis walk:
fn predicate_phase(pred: &Predicate) -> Phase {
    match pred {
        Predicate::Compare { lhs, .. } => {
            if expr_contains_array_access(lhs) {
                Phase::P2  // defer to post-scan
            } else if expr_contains_toString(lhs) {
                Phase::P2
            } else {
                Phase::P1  // scan-time
            }
        }
        // …
    }
}
```

**Executor change**: Add `ArrayIndex`/`ArraySlice` resolution in the P2
`deferred_where_passes` loop, which already handles `toString` predicates.
The array resolution code already exists for SELECT projections — reuse it:

```rust
// In the deferred WHERE pass (stage_runner.rs, P2 window):
for (row_idx, row) in carried_rows.iter_mut().enumerate() {
    let obj_idx = carried_indices[row_idx];
    // Evaluate the deferred predicate with array access resolved
    let value = resolve_array_access(obj_idx, &attr, graph, offsets_cache);
    if !eval_deferred_predicate(value, op, rhs_value) {
        mark_filtered(row_idx);
    }
}
```

### Estimate: ~50 LOC

---

## 7. Window Functions: ROW_NUMBER, RANK, DENSE_RANK

### Motivation

This is the highest-value missing feature. "Top N per group" queries are
currently impossible without running N separate queries. Window functions
also enable cumulative analysis (running totals).

### Use cases

```sql
-- Top 3 objects per class by retained size
SELECT @displayName, classof(o) AS class, @retainedHeapSize,
       ROW_NUMBER() OVER (PARTITION BY classof(o)
                          ORDER BY @retainedHeapSize DESC) AS rn
FROM INSTANCEOF java.lang.Object o
WHERE rn <= 3

-- Rank leak suspects by retained size within category
SELECT @displayName, @category, @retainedHeapSize,
       RANK() OVER (PARTITION BY @category
                    ORDER BY @retainedHeapSize DESC) AS rank
FROM INSTANCEOF java.lang.Object
WHERE @retainedHeapSize > 1048576

-- Cumulative retained size (Pareto chart data)
SELECT classof(o) AS class, SUM(@retainedHeapSize) AS total,
       SUM(SUM(@retainedHeapSize)) OVER (ORDER BY SUM(@retainedHeapSize) DESC)
           AS cumulative
FROM INSTANCEOF java.lang.Object o
GROUP BY classof(o)
ORDER BY total DESC LIMIT 50
```

### AST changes (`ast.rs`)

```rust
/// A window function specification.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WindowSpec {
    /// PARTITION BY expressions (empty = whole result is one partition).
    pub partition_by: Vec<Expr>,
    /// ORDER BY within the window (required for ranking functions).
    pub order_by: Vec<(Expr, SortDir)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum WindowFunc {
    RowNumber,
    Rank,
    DenseRank,
}

// Add to SelectItem:
pub enum SelectItem {
    // … existing …
    /// A window function call: `ROW_NUMBER() OVER (…)`.
    Window {
        func: WindowFunc,
        over: WindowSpec,
    },
}
```

### Parser changes (`parse.rs`)

Window functions follow the pattern `FUNC() OVER (PARTITION BY … ORDER BY …)`:

```rust
let window_func_kind = ident_ci("ROW_NUMBER").to(WindowFunc::RowNumber)
    .or(ident_ci("RANK").to(WindowFunc::Rank))
    .or(ident_ci("DENSE_RANK").to(WindowFunc::DenseRank));

let partition_by = ident_ci("PARTITION")
    .ignore_then(ident_ci("BY"))
    .ignore_then(
        expr.clone()
            .separated_by(just(Token::Comma))
            .at_least(1)
            .collect::<Vec<_>>()
    )
    .or_not()
    .map(|v| v.unwrap_or_default());

let window_order_by = ident_ci("ORDER")
    .ignore_then(ident_ci("BY"))
    .ignore_then(
        expr.clone()
            .then(
                ident_ci("ASC").to(SortDir::Asc)
                    .or(ident_ci("DESC").to(SortDir::Desc))
                    .or_not()
                    .map(|d| d.unwrap_or(SortDir::Asc))
            )
            .separated_by(just(Token::Comma))
            .at_least(1)
            .collect::<Vec<_>>()
    )
    .or_not()
    .map(|v| v.unwrap_or_default());

let window_spec = just(Token::LParen)
    .ignore_then(partition_by)
    .then(window_order_by)
    .then_ignore(just(Token::RParen))
    .map(|(partition_by, order_by)| WindowSpec { partition_by, order_by });

let window_item = window_func_kind
    .then_ignore(just(Token::LParen))
    .then_ignore(just(Token::RParen))
    .then_ignore(ident_ci("OVER"))
    .then(window_spec)
    .map(|(func, over)| {
        (SelectItem::Window { func, over }, None::<String>)
    });
```

### Planner changes (`plan.rs`)

Window functions are a **post-scan, post-GROUP BY** operation. Add a plan
flag:

```rust
pub struct QueryPlan {
    // … existing …
    /// Window function columns, evaluated after the base result set is
    /// materialized (post-WHERE, post-GROUP BY, pre-ORDER BY).
    pub window_columns: Vec<(usize, WindowFunc, WindowSpec)>,
    // (select_position, func, spec)
}
```

The planner rejects window functions in WHERE, GROUP BY, and HAVING:

```rust
if query.select.iter().any(|s| matches!(s, SelectItem::Window { .. })) {
    // Reject if window appears in WHERE/GROUP BY/HAVING
    if query.where_.as_ref().is_some_and(|p| pred_contains_window(p)) {
        return Err("window functions are not allowed in WHERE");
    }
    // Populate window_columns from select positions
    for (i, item) in query.select.iter().enumerate() {
        if let SelectItem::Window { func, over } = item {
            plan.window_columns.push((i, *func, over.clone()));
        }
    }
}
```

### Executor changes (`execute.rs`)

Window evaluation runs as a **post-processing pass** on the materialized row
set, before ORDER BY and LIMIT:

```rust
/// Apply window functions to a materialized result set. Modifies rows in-place,
/// filling in the window-column positions (which were initially Null).
pub(crate) fn apply_window_functions(
    rows: &mut [Vec<QueryValue>],
    columns: &[QueryColumn],
    window_cols: &[(usize, WindowFunc, WindowSpec)],
    query: &Query,
) {
    for &(col_idx, func, ref spec) in window_cols {
        // 1. Resolve partition-by and order-by to column indices
        let part_indices: Vec<usize> = spec.partition_by.iter()
            .filter_map(|e| expr_to_column_index(e, columns, query))
            .collect();
        let ord_keys: Vec<(usize, SortDir)> = spec.order_by.iter()
            .filter_map(|(e, dir)| {
                expr_to_column_index(e, columns, query).map(|idx| (idx, *dir))
            })
            .collect();

        // 2. Group rows by partition key
        let mut partitions: Vec<Vec<usize>> = Vec::new();
        let mut part_map: std::collections::HashMap<String, usize> = HashMap::new();
        for (row_idx, row) in rows.iter().enumerate() {
            let key: String = part_indices.iter()
                .map(|&i| format!("{:?}", row.get(i).unwrap_or(&QueryValue::Null)))
                .collect::<Vec<_>>()
                .join("|");
            let part_idx = *part_map.entry(key).or_insert_with(|| {
                partitions.push(Vec::new());
                partitions.len() - 1
            });
            partitions[part_idx].push(row_idx);
        }

        // 3. Within each partition, sort by order-by keys and assign ranks
        for partition in &mut partitions {
            // Sort partition's row indices by the window ORDER BY
            partition.sort_by(|&a, &b| {
                for &(oidx, dir) in &ord_keys {
                    let av = rows[a].get(oidx).unwrap_or(&QueryValue::Null);
                    let bv = rows[b].get(oidx).unwrap_or(&QueryValue::Null);
                    let ord = total_cmp_query_value(av, bv);
                    let ord = match dir {
                        SortDir::Asc  => ord,
                        SortDir::Desc => ord.reverse(),
                    };
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                }
                std::cmp::Ordering::Equal
            });

            // Assign window function values
            let mut rank = 1i64;
            let mut dense_rank = 1i64;
            for (pos, &row_idx) in partition.iter().enumerate() {
                let value = match func {
                    WindowFunc::RowNumber => QueryValue::Int(pos as i64 + 1),
                    WindowFunc::Rank => {
                        if pos > 0 {
                            let prev = partition[pos - 1];
                            let same = ord_keys.iter().all(|&(oidx, _)| {
                                let a = rows[row_idx].get(oidx).unwrap_or(&QueryValue::Null);
                                let b = rows[prev].get(oidx).unwrap_or(&QueryValue::Null);
                                total_cmp_query_value(a, b) == std::cmp::Ordering::Equal
                            });
                            if !same { rank = pos as i64 + 1; }
                        }
                        QueryValue::Int(rank)
                    }
                    WindowFunc::DenseRank => {
                        if pos > 0 {
                            let prev = partition[pos - 1];
                            let same = ord_keys.iter().all(|&(oidx, _)| {
                                let a = rows[row_idx].get(oidx).unwrap_or(&QueryValue::Null);
                                let b = rows[prev].get(oidx).unwrap_or(&QueryValue::Null);
                                total_cmp_query_value(a, b) == std::cmp::Ordering::Equal
                            });
                            if !same { dense_rank += 1; }
                        }
                        QueryValue::Int(dense_rank)
                    }
                };
                rows[row_idx][col_idx] = value;
            }
        }
    }
}
```

Call site — in `finish()` and `collapse_union_results`, between
GROUP BY finalization and ORDER BY:

```rust
// After GROUP BY finalization, before ORDER BY:
if !self.plan.window_columns.is_empty() {
    apply_window_functions(&mut rows, &columns, &self.plan.window_columns, self.query);
}
// Then ORDER BY, then LIMIT
```

### Optimizer impact

`pushdown_limit` must be blocked when window functions are present (the full
result set must be materialized before window evaluation):

```rust
// In pushdown_limit:
if !plan.window_columns.is_empty() {
    return; // cannot early-stop: need full set for windowing
}
```

### Estimate: ~400 LOC

---

## 8. LAG / LEAD Window Functions

### Motivation

Compare adjacent rows in sorted order — detect "jumps" in retained size,
find disproportionate growth, delta analysis.

### Use cases

```sql
-- Objects where retained size jumps > 10x vs. next-smaller
SELECT @displayName, @retainedHeapSize AS size,
       LAG(@retainedHeapSize, 1, 0) OVER (ORDER BY @retainedHeapSize DESC) AS prev,
       @retainedHeapSize * 100 / LAG(@retainedHeapSize, 1, 1)
           OVER (ORDER BY @retainedHeapSize DESC) AS pct_of_prev
FROM INSTANCEOF java.lang.Object
WHERE @retainedHeapSize > 10000
ORDER BY @retainedHeapSize DESC LIMIT 50
```

### AST changes (`ast.rs`)

Extend `WindowFunc`:

```rust
pub enum WindowFunc {
    RowNumber,
    Rank,
    DenseRank,
    /// LAG(expr, offset, default) — value from `offset` rows before.
    Lag { offset: u32, default: Option<Box<Expr>> },
    /// LEAD(expr, offset, default) — value from `offset` rows after.
    Lead { offset: u32, default: Option<Box<Expr>> },
}
```

And extend `SelectItem::Window` to carry an argument expression:

```rust
Window {
    func: WindowFunc,
    /// The expression to evaluate (e.g., `@retainedHeapSize` for
    /// `LAG(@retainedHeapSize, 1)`). Empty for ROW_NUMBER/RANK.
    arg: Option<Box<Expr>>,
    over: WindowSpec,
},
```

### Parser changes (`parse.rs`)

```rust
let lag_lead = ident_ci("LAG").to(true)
    .or(ident_ci("LEAD").to(false))
    .then_ignore(just(Token::LParen))
    .then(expr.clone())                           // arg expression
    .then(
        just(Token::Comma)
            .ignore_then(select! { Token::Int(n) => n })
            .or_not()
            .map(|n| n.unwrap_or(1) as u32)
    )                                              // offset (default 1)
    .then(
        just(Token::Comma)
            .ignore_then(expr.clone())
            .or_not()
    )                                              // default value
    .then_ignore(just(Token::RParen))
    .then_ignore(ident_ci("OVER"))
    .then(window_spec.clone())
    .map(|((((is_lag, arg), offset), default_val), over)| {
        let func = if is_lag {
            WindowFunc::Lag { offset, default: default_val.map(Box::new) }
        } else {
            WindowFunc::Lead { offset, default: default_val.map(Box::new) }
        };
        (SelectItem::Window { func, arg: Some(Box::new(arg)), over }, None)
    });
```

### Executor changes

Add to the `apply_window_functions` dispatch inside the partition loop:

```rust
WindowFunc::Lag { offset, default } | WindowFunc::Lead { offset, default } => {
    let is_lag = matches!(func, WindowFunc::Lag { .. });
    let default_val = default.as_ref()
        .map(|e| eval_const_expr(e))
        .unwrap_or(QueryValue::Null);

    for (pos, &row_idx) in partition.iter().enumerate() {
        let source_pos = if is_lag {
            pos.checked_sub(*offset as usize)
        } else {
            let target = pos + *offset as usize;
            if target < partition.len() { Some(target) } else { None }
        };

        let value = match source_pos {
            Some(sp) => {
                let source_row = partition[sp];
                // Evaluate the arg expression against the source row
                eval_window_arg(arg, &rows[source_row], columns)
            }
            None => default_val.clone(),
        };
        rows[row_idx][col_idx] = value;
    }
}
```

### Estimate: ~120 LOC (on top of window function framework)

---

## 9. Conditional Aggregates (FILTER Clause)

### Motivation

Compute multiple filtered aggregates in a single scan pass instead of
requiring N separate queries or CASE WHEN hacks.

### Use case

```sql
SELECT
    COUNT(*) FILTER (WHERE @category = "jdk") AS jdk_count,
    COUNT(*) FILTER (WHERE @category = "framework") AS fw_count,
    SUM(@retainedHeapSize) FILTER (WHERE @category = "other") AS app_retained
FROM INSTANCEOF java.lang.Object
```

### AST changes (`ast.rs`)

Extend the `Aggregate` select item with an optional filter:

```rust
pub enum SelectItem {
    // …
    Aggregate {
        func: AggFunc,
        arg: Box<SelectItem>,
        /// Optional FILTER (WHERE pred) — aggregate only over rows matching this.
        filter: Option<Predicate>,
    },
}
```

### Parser changes (`parse.rs`)

Add a trailing `FILTER` clause after the aggregate close-paren:

```rust
// After the existing agg combinator, add a FILTER parse:
let agg_with_filter = agg
    .then(
        ident_ci("FILTER")
            .ignore_then(just(Token::LParen))
            .ignore_then(ident_ci("WHERE"))
            .ignore_then(predicate.clone())
            .then_ignore(just(Token::RParen))
            .or_not()
    )
    .map(|((item, alias), filter)| {
        match item {
            SelectItem::Aggregate { func, arg, .. } => {
                (SelectItem::Aggregate { func, arg, filter }, alias)
            }
            other => (other, alias),
        }
    });
```

### Executor changes (`execute.rs`)

In `fold_agg_acc`, gate the accumulation on the filter predicate:

```rust
// In the per-object aggregation loop:
for (i, (item, acc)) in select_items.iter().zip(accs.iter_mut()).enumerate() {
    if let SelectItem::Aggregate { filter: Some(pred), .. } = item {
        // Evaluate the filter predicate for this object
        if !eval_pred_for_object(pred, src_idx, class_id, blob) {
            continue; // skip this object for this aggregate
        }
    }
    let value = eval_agg_arg(item, src_idx, class_id, blob);
    fold_agg_acc(acc, value);
}
```

### Estimate: ~70 LOC

---

## 10. Correlated EXISTS Subquery

### Motivation

Graph-pattern queries: "find objects where the dominator is the same class",
"find strings referenced by more than one collection".

### Use cases

```sql
-- Objects dominated by same-class objects (self-retention)
SELECT @displayName, @retainedHeapSize
FROM INSTANCEOF java.lang.Object outer
WHERE EXISTS (
    SELECT * FROM OBJECTS dominatorof(outer)
    WHERE classof(inner) = classof(outer)
)

-- Collections retaining > 1 MB that contain strings
SELECT @displayName, @retainedHeapSize
FROM INSTANCEOF java.util.ArrayList outer
WHERE @retainedHeapSize > 1048576
  AND EXISTS (
    SELECT * FROM ENTRIES java.util.ArrayList
    WHERE @objectAddress IN (SELECT @objectAddress FROM java.lang.String)
  )
```

### AST changes (`ast.rs`)

The `Predicate::Exists` variant already has the right shape:

```rust
Exists {
    inner: Box<Query>,
    negated: bool,
}
```

Add a correlation flag and outer-reference bindings:

```rust
Exists {
    inner: Box<Query>,
    negated: bool,
    /// Non-empty when the inner query references the outer alias.
    /// Each entry is (inner_ref_name, outer_attr) — the planner resolves
    /// which outer attribute the inner reference binds to.
    correlations: Vec<(String, Attr)>,  // NEW
}
```

### Planner changes (`plan.rs`)

Currently the planner rejects any inner query that references the outer alias.
Change to: detect correlations, record them, and plan the inner query as a
**correlated subplan** instead of a pre-evaluated one:

```rust
fn detect_correlations(
    inner: &Query,
    outer_alias: Option<&str>,
) -> Vec<(String, Attr)> {
    let mut corrs = Vec::new();
    // Walk inner WHERE predicates looking for references to outer_alias
    if let Some(pred) = &inner.where_ {
        walk_pred_refs(pred, |attr| {
            if let Attr::Field(name) = attr {
                if let Some(alias) = outer_alias {
                    if name.starts_with(alias) && name[alias.len()..].starts_with('.') {
                        let outer_attr_name = &name[alias.len() + 1..];
                        corrs.push((name.clone(), Attr::Field(outer_attr_name.to_string())));
                    }
                }
            }
        });
    }
    corrs
}
```

### Executor changes

Correlated EXISTS is evaluated **per outer row** — this is inherently O(N*M)
where N is outer rows and M is inner scan cost. Gate with a row-count cap:

```rust
const CORRELATED_OUTER_CAP: usize = 100_000;

// In the per-object scan loop, for correlated EXISTS:
if let Some(corr_exists) = &self.plan.correlated_exists {
    for corr in corr_exists {
        // Bind outer values for this object
        let bindings: Vec<QueryValue> = corr.correlations.iter()
            .map(|(_, outer_attr)| self.eval_attr(outer_attr, src_idx, class_id, blob))
            .collect();

        // Evaluate inner query with bindings substituted
        let inner_has_rows = eval_correlated_inner(
            &corr.inner_plan,
            &corr.correlations,
            &bindings,
            graph,
        );

        let matched = if corr.negated { !inner_has_rows } else { inner_has_rows };
        if !matched {
            return; // filter out this object
        }
    }
}
```

The inner evaluation can use a lightweight path — for EXISTS, we only need to
know if at least one row matches, so we can early-out on the first match:

```rust
fn eval_correlated_inner(
    inner_plan: &QueryPlan,
    correlations: &[(String, Attr)],
    bindings: &[QueryValue],
    graph: &Graph,
) -> bool {
    // Substitute correlation references in the inner WHERE with bound values,
    // then run a mini-scan (scan-time only, no late phase, no ORDER BY)
    // with an early-out on first match.
    let substituted_pred = substitute_correlations(
        &inner_plan.where_pred,
        correlations,
        bindings,
    );
    // Run inner scan with LIMIT 1 semantics
    graph.scan_class(inner_plan.class_idx, |src_idx, class_id, blob| {
        if eval_pred(&substituted_pred, src_idx, class_id, blob) {
            return ControlFlow::Break(true); // found a match
        }
        ControlFlow::Continue(())
    })
    .unwrap_or(false)
}
```

### Constraints

- **P1 scan-time only**: Correlated inner queries are limited to scan-phase
  evaluation (no retained size, no ref-walk). The planner rejects correlated
  subqueries that require P2/P3 features.
- **Outer row cap**: `CORRELATED_OUTER_CAP` limits the outer result set to
  prevent O(N²) blowup. The cap triggers truncation.
- **No correlated IN**: Only EXISTS/NOT EXISTS — correlated `IN (subquery)`
  is deferred.

### Estimate: ~250 LOC

---

## 11. WITH / CTE (Common Table Expressions)

### Motivation

Multi-step queries without deeply nested subqueries. Improves readability for
complex analytical queries.

### Use case

```sql
WITH big_objects AS (
    SELECT @objectId, classof(o) AS class, @retainedHeapSize AS retained
    FROM INSTANCEOF java.lang.Object o
    WHERE @retainedHeapSize > 1048576
),
class_totals AS (
    SELECT class, COUNT(*) AS n, SUM(retained) AS total
    FROM big_objects
    GROUP BY class
)
SELECT * FROM class_totals
ORDER BY total DESC LIMIT 20
```

### AST changes (`ast.rs`)

Add a top-level wrapper:

```rust
/// A complete OQL statement: optional CTEs followed by the main query.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Statement {
    /// Named CTEs defined in the WITH clause (empty when none).
    pub ctes: Vec<Cte>,
    /// The main query body.
    pub query: Query,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Cte {
    pub name: String,
    pub query: Query,
}
```

Extend `FromSource` to reference a CTE by name:

```rust
pub enum FromSource {
    // … existing …
    /// A reference to a CTE defined in the WITH clause.
    CteRef(String),
}
```

### Parser changes (`parse.rs`)

```rust
let cte = ident_raw()
    .then_ignore(ident_ci("AS"))
    .then_ignore(just(Token::LParen))
    .then(full_query.clone())
    .then_ignore(just(Token::RParen))
    .map(|(name, query)| Cte { name, query });

let with_clause = ident_ci("WITH")
    .ignore_then(
        cte.separated_by(just(Token::Comma))
            .at_least(1)
            .collect::<Vec<_>>()
    )
    .or_not()
    .map(|v| v.unwrap_or_default());

let statement = with_clause
    .then(full_query)
    .map(|(ctes, query)| Statement { ctes, query });
```

In the FROM clause parser, recognize CTE names (detected during planning, not
parsing — the parser allows any identifier in FROM position and the planner
resolves it to either a class name or a CTE reference).

### Executor changes

CTEs are **materialized once** before the main query runs. Each CTE produces
a `Vec<Vec<QueryValue>>` result set with column metadata. Subsequent CTE or
main queries that reference a CTE scan the materialized rows:

```rust
fn execute_statement(stmt: &Statement, graph: &Graph, …) -> QueryResult {
    let mut cte_results: HashMap<String, (Vec<QueryColumn>, Vec<Vec<QueryValue>>)> =
        HashMap::new();

    // Materialize CTEs in order (each can reference earlier CTEs)
    for cte in &stmt.ctes {
        let result = execute_query_with_ctes(&cte.query, graph, &cte_results, …)?;
        cte_results.insert(cte.name.clone(), (result.columns, result.rows));
    }

    // Execute main query with CTE results available
    execute_query_with_ctes(&stmt.query, graph, &cte_results, …)
}
```

When the planner encounters a `FromSource::CteRef(name)`, it resolves the
CTE's column schema and creates a scan plan that iterates the materialized
rows instead of heap objects.

### Constraints

- **Non-recursive only**: `WITH RECURSIVE` is not supported. Attempting to
  reference a CTE from within its own definition is a plan-time error.
- **Materialized**: Each CTE is fully materialized (no lazy/streaming). Row
  caps apply to CTE results.
- **Column schema**: CTE columns inherit names from the inner query's SELECT
  aliases.

### Estimate: ~180 LOC

---

## 12. CAST() Type Conversion

### Motivation

Explicit type conversion for edge cases where auto-promotion isn't enough:
parsing string field values as integers, converting between int and float
explicitly.

### Use cases

```sql
-- Compare a string field numerically
SELECT @displayName, field_value
FROM com.example.Config c
WHERE CAST(c.size AS INT) > 1000

-- Force float division
SELECT @displayName,
       CAST(@usedHeapSize AS FLOAT) / CAST(@retainedHeapSize AS FLOAT) AS ratio
FROM INSTANCEOF java.lang.Object
WHERE @retainedHeapSize > 0
ORDER BY ratio ASC LIMIT 20
```

### AST changes (`ast.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CastTarget {
    Int,
    Float,
    Str,
    Bool,
}

pub enum Expr {
    // … existing …
    /// CAST(expr AS type)
    Cast {
        expr: Box<Expr>,
        target: CastTarget,
    },
}
```

### Parser changes (`parse.rs`)

```rust
let cast_target = ident_ci("INT").or(ident_ci("INTEGER")).to(CastTarget::Int)
    .or(ident_ci("FLOAT").or(ident_ci("DOUBLE")).to(CastTarget::Float))
    .or(ident_ci("STRING").or(ident_ci("VARCHAR")).to(CastTarget::Str))
    .or(ident_ci("BOOL").or(ident_ci("BOOLEAN")).to(CastTarget::Bool));

let cast_expr = ident_ci("CAST")
    .ignore_then(just(Token::LParen))
    .ignore_then(expr.clone())
    .then_ignore(ident_ci("AS"))
    .then(cast_target)
    .then_ignore(just(Token::RParen))
    .map(|(inner, target)| Expr::Cast {
        expr: Box::new(inner),
        target,
    });
```

### Executor changes (`execute.rs`)

```rust
// In eval_expr:
Expr::Cast { expr, target } => {
    let value = self.eval_expr(expr, src_idx, class_id, blob);
    match target {
        CastTarget::Int => match value {
            QueryValue::Int(_) => value,
            QueryValue::Float(f) => QueryValue::Int(f as i64),
            QueryValue::Str(s) => s.parse::<i64>()
                .map(QueryValue::Int)
                .unwrap_or(QueryValue::Null),
            QueryValue::Bool(b) => QueryValue::Int(if b { 1 } else { 0 }),
            _ => QueryValue::Null,
        },
        CastTarget::Float => match value {
            QueryValue::Float(_) => value,
            QueryValue::Int(i) => QueryValue::Float(i as f64),
            QueryValue::Str(s) => s.parse::<f64>()
                .map(QueryValue::Float)
                .unwrap_or(QueryValue::Null),
            _ => QueryValue::Null,
        },
        CastTarget::Str => match value {
            QueryValue::Str(_) => value,
            QueryValue::Int(i) => QueryValue::Str(i.to_string()),
            QueryValue::Float(f) => QueryValue::Str(f.to_string()),
            QueryValue::Bool(b) => QueryValue::Str(b.to_string()),
            QueryValue::Null => QueryValue::Str("null".to_string()),
            QueryValue::ObjRef { .. } => QueryValue::Null,
        },
        CastTarget::Bool => match value {
            QueryValue::Bool(_) => value,
            QueryValue::Int(i) => QueryValue::Bool(i != 0),
            QueryValue::Str(s) => QueryValue::Bool(
                s.eq_ignore_ascii_case("true") || s == "1"
            ),
            _ => QueryValue::Null,
        },
    }
}
```

### Estimate: ~50 LOC

---

## Implementation Order and Dependencies

```
 P0 (no dependencies, ship independently):
   ├── #1  Multi-key ORDER BY     ~60 LOC
   ├── #2  Modulo % operator      ~15 LOC
   └── #3  COUNT(DISTINCT)        ~50 LOC
                                  ───────
                            P0 total: ~125 LOC

 P1 (independent of each other, some need P0 done):
   ├── #4  String functions       ~120 LOC
   ├── #5  HAVING group-key       ~30 LOC
   ├── #6  Array index in WHERE   ~50 LOC
   └── #7  Window functions       ~400 LOC  ← biggest single item
                                  ───────
                            P1 total: ~600 LOC

 P2 (depends on P1 framework for #8):
   ├── #8  LAG/LEAD (needs #7)    ~120 LOC
   ├── #9  FILTER clause          ~70 LOC
   └── #10 Correlated EXISTS      ~250 LOC
                                  ───────
                            P2 total: ~440 LOC

 P3 (independent):
   ├── #11 CTEs                   ~180 LOC
   └── #12 CAST                   ~50 LOC
                                  ───────
                            P3 total: ~230 LOC

 Grand total: ~1,395 LOC
```

### Suggested merge order

1. **#2 Modulo** — smallest change, zero risk, immediate utility
2. **#1 Multi-key ORDER BY** — next smallest, touches a well-understood path
3. **#3 COUNT(DISTINCT)** — small, self-contained accumulator addition
4. **#12 CAST** — small expression evaluator addition
5. **#4 String functions** — medium, self-contained, high user value
6. **#5 HAVING group-key** — small fix to an existing limitation
7. **#6 Array index in WHERE** — lifts an artificial restriction
8. **#7 Window functions** — large but self-contained post-processing pass
9. **#8 LAG/LEAD** — incremental on #7
10. **#9 FILTER clause** — incremental on existing aggregate machinery
11. **#10 Correlated EXISTS** — largest architectural change
12. **#11 CTEs** — largest parser change, but isolated

---

## Appendix: Features Explicitly NOT Proposed

| Feature | Reason |
|---------|--------|
| Multi-table JOIN | Architecture mismatch: single-FROM scan model. Subquery semi-join covers most cases. Would need DataFusion-level refactor. |
| `LIMIT PER GROUP` | Non-standard; window functions (ROW_NUMBER + WHERE) are the proper solution. |
| Full-text search (CONTAINS, FREETEXT) | Out of scope for heap analysis; LIKE regex covers the use cases. |
| Recursive CTE (`WITH RECURSIVE`) | The heap's object graph is navigable via existing functions (pathToRoot, dominators, outbounds). Recursive CTE would duplicate these with worse performance. |
| Lateral JOIN / CROSS APPLY | Same architecture constraint as JOIN. |
| User-defined functions (CREATE FUNCTION) | Over-engineering; the built-in function set covers heap analysis needs. |
