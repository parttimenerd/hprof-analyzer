//! Executor for the supported OQL subset. SingleScanExecutor implements
//! ObjectVisitor and accumulates bounded rows during the pass2 2a scan.
//! HistogramExecutor answers aggregate-only queries from per-class stats.

use crate::query::ObjectVisitor;
use crate::query::ast::{
    AggFunc, ArithOp, Attr, CompareOp, Expr, FromSource, Query, SelectItem, UnaryOp, Value,
};
use crate::query::carry::Carry;
use crate::query::model::{QueryColumn, QueryResult, QueryValue};
use crate::query::plan::QueryPlan;

/// A cross-phase query whose Phase-1 matches were carried; finalized after
/// retained sizes exist. `slot` is the query's index in the caller's list so
/// results reassemble in input order.
pub struct CrossPhaseEntry {
    pub slot: usize,
    pub name: String,
    pub plan: QueryPlan,
    pub carry: Carry,
}

/// The output of Phase 1 for a batch: results finished during the scan plus
/// cross-phase carries awaiting a late stage, each tagged with its slot.
#[derive(Default)]
pub struct QueryExecState {
    finished: Vec<(usize, QueryResult)>,
    pending: Vec<CrossPhaseEntry>,
    /// Per-slot source dense-index sidecar for GC-reachability pruning. Populated
    /// ONLY for row-mode executors that were armed via `arm_row_capture`
    /// (i.e. `--reachable-only` runs); every other run leaves this empty and
    /// allocates nothing. A slot ABSENT here means "no captured source" → the
    /// reachability filter keeps all of that result's rows (aggregates, scalars,
    /// validation errors, carry results).
    row_src_by_slot: std::collections::HashMap<usize, Vec<u32>>,
}

impl QueryExecState {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push_finished(&mut self, slot: usize, r: QueryResult) {
        self.finished.push((slot, r));
    }
    /// Push a finished row-mode result together with its captured per-row source
    /// dense-index sidecar (from `SingleScanExecutor::finish_with_src`). When
    /// `src` is `Some`, it is recorded under `slot` for later GC-reachability
    /// pruning; `None` records nothing (aggregate/disarmed → keep all rows).
    pub fn push_finished_with_src(&mut self, slot: usize, r: QueryResult, src: Option<Vec<u32>>) {
        self.finished.push((slot, r));
        if let Some(v) = src {
            self.row_src_by_slot.insert(slot, v);
        }
    }
    pub fn push_cross_phase(&mut self, slot: usize, name: String, plan: QueryPlan, carry: Carry) {
        self.pending.push(CrossPhaseEntry {
            slot,
            name,
            plan,
            carry,
        });
    }
    #[allow(dead_code)]
    pub fn finished_len(&self) -> usize {
        self.finished.len()
    }
    #[allow(dead_code)]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
    #[allow(dead_code)]
    pub fn pending(&self) -> &[CrossPhaseEntry] {
        &self.pending
    }
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    /// Consume into (finished slots, pending entries) for the stage runner.
    pub fn into_parts(self) -> (Vec<(usize, QueryResult)>, Vec<CrossPhaseEntry>) {
        (self.finished, self.pending)
    }

    /// Take the per-slot source-index sidecar map out of the state (leaving it
    /// empty). Used by the resume layer to prune finished results by
    /// GC-reachability before it consumes the state via `into_parts`.
    pub fn take_row_src_by_slot(&mut self) -> std::collections::HashMap<usize, Vec<u32>> {
        std::mem::take(&mut self.row_src_by_slot)
    }

    /// Re-add a `CrossPhaseEntry` (e.g. one taken from `into_parts` that was
    /// not handled). Used by hybrid resume paths that handle some ops locally
    /// and delegate the rest to `resume_without_late_ctx`.
    pub fn push_cross_phase_entry(&mut self, entry: CrossPhaseEntry) {
        self.pending.push(entry);
    }
}

/// Abstracts class-name resolution + field-offset lookup so the executor can be
/// unit-tested against a fake and run against the real pass2 schema in prod.
pub trait ClassResolver {
    fn class_name(&self, class_id: u64) -> Option<&str>;
    /// True if the object's class (`class_id`) IS the target class named by
    /// `spec` OR a subclass of it — i.e. Java `instanceof` semantics for
    /// `FROM INSTANCEOF C` / `WHERE x INSTANCEOF C`. The default implementation
    /// only matches the exact class (no hierarchy), which is correct for test
    /// resolvers with no super-chain; `LiveResolver` overrides it to walk the
    /// superclass chain via `ClassInfo::super_id`. `spec` carries the class name
    /// (and, for a quoted-regex FROM, the compiled regex) so exact/glob/regex
    /// matching stays consistent with `class_matches`.
    fn is_instance_of(
        &self,
        class_id: u64,
        spec: &crate::query::ast::ClassSpec,
        from_regex: Option<&regex::Regex>,
    ) -> bool {
        match self.class_name(class_id) {
            Some(name) => class_name_matches_spec(name, spec, from_regex),
            None => false,
        }
    }
    fn field(&self, _class_id: u64, _name: &str) -> Option<(u32, crate::types::HprofType)> {
        None
    }
    fn addr_of(&self, _src_idx: usize) -> Option<u64> {
        None
    }
    fn shallow_of(&self, _src_idx: usize) -> Option<u32> {
        None
    }
    /// Reverse of `addr_of`: the dense object index for a heap address, or `None`
    /// if the address is not a live object. Backs RefWalk edge resolution.
    fn index_of_addr(&self, _addr: u64) -> Option<usize> {
        None
    }
    /// Reference (object-pointer) width in bytes, used to decode ref fields from
    /// an instance blob. Defaults to 8; `LiveResolver` returns the dump's real
    /// `id_size`.
    fn ref_width(&self) -> usize {
        8
    }
}

#[cfg(test)]
pub struct TestSchema {
    pub names: std::collections::HashMap<u64, String>,
}

#[cfg(test)]
impl ClassResolver for TestSchema {
    fn class_name(&self, class_id: u64) -> Option<&str> {
        self.names.get(&class_id).map(|s| s.as_str())
    }
}

#[allow(clippy::type_complexity)]
pub struct SingleScanExecutor<'a, R: ClassResolver> {
    query: &'a Query,
    plan: &'a QueryPlan,
    resolver: &'a R,
    rows: Vec<Vec<QueryValue>>,
    matched: u64,
    truncated: bool,
    /// When `Some`, this is a cross-phase (Phase::P3) query: instead of building
    /// result rows during the scan, matched dense indices are carried forward
    /// and finalized later (stage_runner) once retained sizes exist. In carry
    /// mode, `@retainedHeapSize` WHERE terms are skipped (retained size is
    /// unknown here) and LIMIT is NOT applied (the retained-based ORDER BY +
    /// LIMIT run in the late phase); the carry's own cap bounds memory.
    carry: Option<Carry>,
    /// Injected `IN (<subquery>)` membership sets, one per `InSubplan` in the
    /// plan (same order). Each holds the inner subquery's projected addresses;
    /// an `@objectAddress IN (...)` predicate tests the current object's address
    /// against the set that matches its LHS. Empty for a query without any
    /// IN-subquery, or before the driver injects them. `truncated` on any set
    /// means membership is incomplete and the outer result must be marked so.
    in_sets: Vec<InSet>,
    /// Pre-compiled FROM regex for a quoted (`FROM "<regex>"`) class target,
    /// compiled ONCE at executor construction (see [`compile_from_regex`]) and
    /// reused for every object. `None` for a bare-ident/glob FROM (matched by
    /// `class_name_matches`) or a subquery source. Compiling here — not in the
    /// per-object `visit_instance`/`visit_array` hot path — is the performance
    /// contract: a heap scan touches millions of objects but one regex.
    from_regex: Option<regex::Regex>,
    /// Pre-compiled `LIKE`/`NOT LIKE` RHS regexes, keyed by the raw pattern
    /// string, compiled ONCE at executor construction (see
    /// [`compile_like_regexes`]) and reused for every object. Same performance
    /// contract as `from_regex`: the scan touches millions of objects but each
    /// LIKE pattern is compiled exactly once. Empty when the query has no LIKE.
    like_regexes: std::collections::HashMap<String, regex::Regex>,
    /// Per-SELECT-item aggregate accumulators. `Some` iff the query has at least
    /// one `SelectItem::Aggregate` and this executor is not in carry mode. The
    /// Vec is parallel to `query.select`: non-aggregate items get a placeholder.
    /// Accumulation happens per matched object in `visit_instance`/`visit_array`;
    /// `finish` finalizes each accumulator into the single result row.
    agg_acc: Option<Vec<AggAcc>>,
    /// For a `FROM OBJECTS <address>` source, the single dense object index the
    /// address resolves to (via [`ClassResolver::index_of_addr`]), computed ONCE
    /// at construction — never per object. `None` when FROM is not an Object
    /// source, OR when the address does not name a live object (missing address →
    /// no object ever matches → zero rows, matching Eclipse MAT). The per-object
    /// `visit_instance`/`visit_array` gate consults this only when FROM is Object,
    /// so all other queries pay zero cost and stay byte/RSS-identical.
    target_index: Option<usize>,
    /// Per-row source dense-object index sidecar, captured in lockstep with
    /// `rows` when reachability capture is armed. `None` = disarmed (the default,
    /// and the case for carry-mode / aggregate executors), so nothing is
    /// allocated and every non-reachable-only run stays byte/RSS-identical.
    /// When `Some`, each `self.rows.push(row)` is paired with a push of the
    /// object's dense index here; `finish` keeps it aligned through ORDER BY sort
    /// + LIMIT truncate so the caller can prune rows by GC-reachability using the
    ///   EXACT source index (not a lossy re-read of the projected row value).
    row_src: Option<Vec<u32>>,
    /// GROUP BY accumulator: maps each unique key vector (encoded as a canonical
    /// Debug string to avoid Float/Hash issues) to (original key Vec, per-column
    /// AggAcc row). `None` when StageKind != GroupBy.
    group_map: Option<std::collections::HashMap<String, (Vec<QueryValue>, Vec<AggAcc>)>>,
    /// Pre-evaluated EXISTS/NOT EXISTS results, one `bool` per `ExistsSubplan` in
    /// encounter order (DFS left-to-right over the WHERE tree). Populated by the
    /// two-phase driver before the outer scan via `set_exists_results`. Each bool
    /// is the already-negated result: `true` if the predicate passes (≥1 inner row
    /// AND NOT EXISTS, or 0 inner rows AND NOT EXISTS). Empty until injected.
    exists_bools: Vec<bool>,
    /// DFS cursor used to look up the correct `exists_bools` entry when the
    /// predicate walker hits an `Exists` node. Reset to 0 at the start of each
    /// per-object `where_passes`/`array_where_passes` call.
    exists_cursor: std::cell::Cell<usize>,
}

/// Per-select-item running state for a scan-time aggregate accumulator.
/// One entry per position in `query.select`; non-aggregate positions use
/// `AggAcc::None` as a no-op placeholder.
pub(crate) enum AggAcc {
    /// Placeholder for a non-aggregate SELECT item (Attr / Expr / Star / etc.).
    None,
    /// COUNT(*): every matched object increments the counter.
    CountStar { n: i64 },
    /// COUNT(expr): every matched object increments iff the per-object value
    /// is not Null. (MAT semantics: COUNT(expr) counts non-null values.)
    CountExpr { n: i64 },
    /// SUM: running total. Starts as Int(0); if any per-object value is Float
    /// the whole running total is promoted to f64. Non-numeric/Null values are
    /// skipped (MAT semantics: SUM ignores nulls).
    Sum { total: QueryValue, any_value: bool },
    /// AVG: tracks running sum and count of non-null numeric values; finalized
    /// as Float(sum/count). Zero numeric values → Null (matches histogram path).
    Avg { sum: f64, count: i64 },
    /// MIN: running minimum over numeric values; Null until first non-Null value.
    Min { best: Option<QueryValue> },
    /// MAX: running maximum over numeric values; Null until first non-Null value.
    Max { best: Option<QueryValue> },
    /// PERCENTILE/MEDIAN: collects every non-null numeric value (as f64), then at
    /// finalize sorts and picks the p-th percentile (nearest-rank). The Vec is
    /// bounded by the matched set and armed ONLY for percentile queries.
    Percentile { p: u8, values: Vec<f64> },
}

/// Build the initial `AggAcc` for one SELECT item. Non-aggregate items get
/// `AggAcc::None`; aggregate items get their zero state.
pub(crate) fn init_agg_acc(item: &SelectItem) -> AggAcc {
    match item {
        SelectItem::Aggregate { func, arg } => match func {
            AggFunc::Count => {
                if matches!(arg.as_ref(), SelectItem::Star) {
                    AggAcc::CountStar { n: 0 }
                } else {
                    AggAcc::CountExpr { n: 0 }
                }
            }
            AggFunc::Sum => AggAcc::Sum {
                total: QueryValue::Int(0),
                any_value: false,
            },
            AggFunc::Avg => AggAcc::Avg { sum: 0.0, count: 0 },
            AggFunc::Min => AggAcc::Min { best: None },
            AggFunc::Max => AggAcc::Max { best: None },
            AggFunc::Percentile(p) => AggAcc::Percentile {
                p: *p,
                values: Vec::new(),
            },
            AggFunc::Median => AggAcc::Percentile {
                p: 50,
                values: Vec::new(),
            },
        },
        _ => AggAcc::None,
    }
}

/// Fold one per-object `value` into the accumulator `acc` for the corresponding
/// SELECT item. Called once per matched object.
pub(crate) fn fold_agg_acc(acc: &mut AggAcc, value: QueryValue) {
    match acc {
        AggAcc::None => {}
        AggAcc::CountStar { n } => *n += 1,
        AggAcc::CountExpr { n } => {
            // COUNT(expr): count non-null values only (MAT semantics).
            if !matches!(value, QueryValue::Null) {
                *n += 1;
            }
        }
        AggAcc::Sum { total, any_value } => {
            // SUM ignores Null and non-numeric values (MAT semantics).
            match &value {
                QueryValue::Int(v) => {
                    *any_value = true;
                    *total = match total {
                        QueryValue::Int(t) => QueryValue::Int(t.wrapping_add(*v)),
                        QueryValue::Float(t) => QueryValue::Float(*t + *v as f64),
                        _ => QueryValue::Int(*v),
                    };
                }
                QueryValue::Float(v) => {
                    *any_value = true;
                    // Float operand: promote the running total to f64.
                    *total = match total {
                        QueryValue::Int(t) => QueryValue::Float(*t as f64 + v),
                        QueryValue::Float(t) => QueryValue::Float(*t + v),
                        _ => QueryValue::Float(*v),
                    };
                }
                _ => {}
            }
        }
        AggAcc::Avg { sum, count } => {
            // AVG: accumulate numeric values; ignore Null and non-numeric.
            match value {
                QueryValue::Int(v) => {
                    *sum += v as f64;
                    *count += 1;
                }
                QueryValue::Float(v) => {
                    *sum += v;
                    *count += 1;
                }
                _ => {}
            }
        }
        AggAcc::Min { best } => {
            // MIN: track the smallest numeric value; skip Null/non-numeric.
            let candidate = match &value {
                QueryValue::Int(_) | QueryValue::Float(_) => Some(value),
                _ => None,
            };
            if let Some(c) = candidate {
                *best = match best.take() {
                    None => Some(c),
                    Some(prev) => {
                        // Use numeric compare: smaller wins.
                        let prev_lt = match (&prev, &c) {
                            (QueryValue::Int(a), QueryValue::Int(b)) => *a <= *b,
                            (QueryValue::Int(a), QueryValue::Float(b)) => (*a as f64) <= *b,
                            (QueryValue::Float(a), QueryValue::Int(b)) => *a <= (*b as f64),
                            (QueryValue::Float(a), QueryValue::Float(b)) => *a <= *b,
                            _ => true,
                        };
                        Some(if prev_lt { prev } else { c })
                    }
                };
            }
        }
        AggAcc::Max { best } => {
            // MAX: track the largest numeric value; skip Null/non-numeric.
            let candidate = match &value {
                QueryValue::Int(_) | QueryValue::Float(_) => Some(value),
                _ => None,
            };
            if let Some(c) = candidate {
                *best = match best.take() {
                    None => Some(c),
                    Some(prev) => {
                        // Use numeric compare: larger wins.
                        let prev_gt = match (&prev, &c) {
                            (QueryValue::Int(a), QueryValue::Int(b)) => *a >= *b,
                            (QueryValue::Int(a), QueryValue::Float(b)) => (*a as f64) >= *b,
                            (QueryValue::Float(a), QueryValue::Int(b)) => *a >= (*b as f64),
                            (QueryValue::Float(a), QueryValue::Float(b)) => *a >= *b,
                            _ => true,
                        };
                        Some(if prev_gt { prev } else { c })
                    }
                };
            }
        }
        AggAcc::Percentile { values, .. } => {
            // Collect numeric values only; ignore Null/non-numeric (as SUM/AVG do).
            match value {
                QueryValue::Int(v) => values.push(v as f64),
                QueryValue::Float(v) => values.push(v),
                _ => {}
            }
        }
    }
}

/// Finalize an `AggAcc` into its result `QueryValue`.
pub(crate) fn finalize_agg_acc(acc: AggAcc) -> QueryValue {
    match acc {
        AggAcc::None => QueryValue::Null,
        AggAcc::CountStar { n } => QueryValue::Int(n),
        AggAcc::CountExpr { n } => QueryValue::Int(n),
        AggAcc::Sum { total, any_value } => {
            if any_value {
                total
            } else {
                // SUM of zero numeric values: MAT returns 0 for SUM when the
                // class exists but all values are Null; however for an empty
                // result set (no matched objects) Int(0) is the best sentinel.
                // We return Int(0) to match the histogram path's behavior.
                QueryValue::Int(0)
            }
        }
        AggAcc::Avg { sum, count } => {
            if count > 0 {
                QueryValue::Float(sum / count as f64)
            } else {
                // AVG with no numeric values → Null (matches histogram path).
                QueryValue::Null
            }
        }
        AggAcc::Min { best } => best.unwrap_or(QueryValue::Null),
        AggAcc::Max { best } => best.unwrap_or(QueryValue::Null),
        AggAcc::Percentile { p, mut values } => {
            if values.is_empty() {
                return QueryValue::Null;
            }
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            // Nearest-rank: rank = ceil(p/100 * n), 1-based; index = rank-1.
            let n = values.len();
            let rank = ((p as f64 / 100.0) * n as f64).ceil() as usize;
            let idx = rank.saturating_sub(1).min(n - 1);
            QueryValue::Float(values[idx])
        }
    }
}

/// Evaluate a HAVING predicate against a finalized GROUP BY output row.
/// `row` is parallel to `columns`; `query` is needed for alias resolution.
pub(crate) fn eval_having_term(
    pred: &crate::query::ast::Predicate,
    row: &[QueryValue],
    query: &Query,
    columns: &[QueryColumn],
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> bool {
    use crate::query::ast::Predicate as P;
    match pred {
        P::And(a, b) => {
            eval_having_term(a, row, query, columns, like_regexes)
                && eval_having_term(b, row, query, columns, like_regexes)
        }
        P::Or(a, b) => {
            eval_having_term(a, row, query, columns, like_regexes)
                || eval_having_term(b, row, query, columns, like_regexes)
        }
        P::Not(inner) => !eval_having_term(inner, row, query, columns, like_regexes),
        P::Compare { lhs, op, rhs } => {
            let lv = eval_having_expr(lhs, row, query, columns, like_regexes);
            let rv = eval_having_expr(rhs, row, query, columns, like_regexes);
            let like_re = if matches!(op, CompareOp::Like | CompareOp::NotLike) {
                rhs.as_lit().and_then(|v| {
                    if let Value::Str(pat) = v {
                        like_regexes.get(pat.as_str())
                    } else {
                        None
                    }
                })
            } else {
                None
            };
            compare_values(&lv, *op, &rv, like_re)
        }
        _ => true,
    }
}

/// Evaluate a HAVING expression against a finalized GROUP BY output row.
/// Aggregate expressions (COUNT(*), SUM(...)) look up their result column by name.
/// Attribute expressions look up by column name.
/// Literals convert directly.
fn eval_having_expr(
    e: &Expr,
    row: &[QueryValue],
    query: &Query,
    columns: &[QueryColumn],
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> QueryValue {
    match e {
        Expr::Lit(v) => match v {
            Value::Int(n) => QueryValue::Int(*n),
            Value::Float(f) => QueryValue::Float(*f),
            Value::Str(s) => QueryValue::Str(s.clone()),
            Value::Bool(b) => QueryValue::Bool(*b),
            Value::Null => QueryValue::Null,
        },
        Expr::Aggregate { func, arg } => {
            // Find this aggregate by structural match in query.select, bypassing
            // any AS alias — the alias changes the column name but not the
            // SelectItem variant. This is alias-transparent.
            let pos = query.select.iter().position(|it| match it {
                SelectItem::Aggregate { func: f, arg: a } => f == func && a == arg,
                _ => false,
            });
            pos.and_then(|i| row.get(i))
                .cloned()
                .unwrap_or(QueryValue::Null)
        }
        Expr::Attr(attr) => {
            // Non-aggregate column in HAVING — match by column name.
            let name = attr_name(attr);
            columns
                .iter()
                .position(|c| c.name == name)
                .and_then(|i| row.get(i))
                .cloned()
                .unwrap_or(QueryValue::Null)
        }
        Expr::Binary { op, lhs, rhs } => {
            let l = eval_having_expr(lhs, row, query, columns, like_regexes);
            let r = eval_having_expr(rhs, row, query, columns, like_regexes);
            arith(&l, *op, &r)
        }
        Expr::Unary { op, arg } => unary(
            *op,
            &eval_having_expr(arg, row, query, columns, like_regexes),
        ),
        Expr::Method { .. } => QueryValue::Null,
        Expr::Case { branches, else_ } => {
            for (pred, then_expr) in branches {
                if eval_having_term(pred, row, query, columns, like_regexes) {
                    return eval_having_expr(then_expr, row, query, columns, like_regexes);
                }
            }
            match else_ {
                Some(e) => eval_having_expr(e, row, query, columns, like_regexes),
                None => QueryValue::Null,
            }
        }
        Expr::Coalesce(args) => {
            for arg in args {
                let v = eval_having_expr(arg, row, query, columns, like_regexes);
                if !matches!(v, QueryValue::Null) {
                    return v;
                }
            }
            QueryValue::Null
        }
        Expr::NullIf { lhs, rhs } => {
            let lv = eval_having_expr(lhs, row, query, columns, like_regexes);
            let rv = eval_having_expr(rhs, row, query, columns, like_regexes);
            if lv == rv { QueryValue::Null } else { lv }
        }
    }
}

/// A resolved `IN (<subquery>)` membership set injected before the outer scan.
/// `lhs` is the outer attribute compared for membership (must be
/// `@objectAddress`); `set` is the inner subquery's projected addresses; and
/// `truncated` records whether the inner result (and thus membership) was capped.
pub struct InSet {
    pub lhs: Attr,
    pub set: std::collections::HashSet<u64>,
    pub truncated: bool,
}

/// Compile the query's FROM regex once for executor construction. The regex was
/// already validated (with an actionable error) at plan time via
/// [`compile_from_regex`], so a compile failure here cannot happen for a query
/// that planned successfully; if it somehow did, we fall back to `None` (no
/// match) rather than panicking on the scan hot path.
fn compile_from_query(query: &Query) -> Option<regex::Regex> {
    query
        .from
        .class_spec()
        .and_then(|spec| compile_from_regex(spec).ok().flatten())
}

/// Compile the query's LIKE regexes for executor construction. They were already
/// validated (with an actionable error) at plan time via [`compile_like_regexes`],
/// so a compile failure here cannot happen for a query that planned successfully;
/// if it somehow did, we fall back to an empty map (LIKE then never matches)
/// rather than panicking on the scan hot path.
fn compile_like_for_query(query: &Query) -> std::collections::HashMap<String, regex::Regex> {
    compile_like_regexes(query).unwrap_or_default()
}

impl<'a, R: ClassResolver> SingleScanExecutor<'a, R> {
    pub fn new(query: &'a Query, plan: &'a QueryPlan, resolver: &'a R) -> Self {
        // Build per-item accumulators when the query is an aggregate scan. A
        // query is aggregate-scan when it contains any Aggregate item AND is NOT
        // a carry (carry mode is for cross-phase retained queries, never
        // aggregates). The histogram path routes aggregate-only queries that need
        // no per-object data (see plan.rs); anything routed to SingleScan with an
        // aggregate gets its own accumulator here.
        let agg_acc = if plan.kind != crate::query::plan::StageKind::GroupBy
            && query
                .select
                .iter()
                .any(|it| matches!(it, SelectItem::Aggregate { .. }))
        {
            Some(query.select.iter().map(init_agg_acc).collect())
        } else {
            None
        };
        // Initialize GROUP BY accumulator when this is a GroupBy plan.
        let group_map = if plan.kind == crate::query::plan::StageKind::GroupBy {
            Some(std::collections::HashMap::new())
        } else {
            None
        };
        // Resolve a `FROM OBJECTS <address>` seed to its single dense index once,
        // here — not in the per-object hot path. A missing address stays `None`
        // and no object matches (zero rows, MAT parity).
        let target_index = if let FromSource::Object(addr) = &query.from {
            resolver.index_of_addr(*addr)
        } else {
            None
        };
        Self {
            query,
            plan,
            resolver,
            rows: Vec::new(),
            matched: 0,
            truncated: false,
            carry: None,
            in_sets: Vec::new(),
            from_regex: compile_from_query(query),
            like_regexes: compile_like_for_query(query),
            agg_acc,
            target_index,
            row_src: None,
            group_map,
            exists_bools: Vec::new(),
            exists_cursor: std::cell::Cell::new(0),
        }
    }

    /// Construct a cross-phase carry executor. `carry` should be an
    /// `index-only` carry sized by the caller's cap; matched indices are pushed
    /// into it during the scan and extracted with `take_carry` at scan end.
    pub fn new_carry(query: &'a Query, plan: &'a QueryPlan, resolver: &'a R, carry: Carry) -> Self {
        let target_index = if let FromSource::Object(addr) = &query.from {
            resolver.index_of_addr(*addr)
        } else {
            None
        };
        Self {
            query,
            plan,
            resolver,
            rows: Vec::new(),
            matched: 0,
            truncated: false,
            carry: Some(carry),
            in_sets: Vec::new(),
            from_regex: compile_from_query(query),
            like_regexes: compile_like_for_query(query),
            // Carry mode is always for cross-phase retained queries, never for
            // aggregates; no accumulator needed.
            agg_acc: None,
            target_index,
            row_src: None,
            group_map: None,
            exists_bools: Vec::new(),
            exists_cursor: std::cell::Cell::new(0),
        }
    }

    /// Inject the resolved `IN (<subquery>)` membership sets (one per plan
    /// `InSubplan`, in the same order) before the outer scan. Called by the
    /// two-phase driver once the inner subqueries have been scanned. If any set
    /// was truncated, the executor's own result is marked truncated (membership
    /// is incomplete).
    pub fn set_in_subquery_sets(&mut self, sets: Vec<InSet>) {
        if sets.iter().any(|s| s.truncated) {
            self.truncated = true;
        }
        self.in_sets = sets;
    }

    /// Inject the pre-evaluated EXISTS/NOT EXISTS boolean results (one per plan
    /// `ExistsSubplan`, in encounter order). Called by the two-phase driver before
    /// the outer scan. Each `bool` is `true` when the EXISTS/NOT EXISTS condition
    /// is satisfied (already accounts for `negated`), so `eval_pred` returns it
    /// directly as the predicate result for every outer row.
    pub fn set_exists_results(&mut self, bools: Vec<bool>) {
        self.exists_bools = bools;
    }

    /// True if this executor is carrying indices for a later phase.
    pub fn is_carry(&self) -> bool {
        self.carry.is_some()
    }

    /// Arm the per-row source-index sidecar for GC-reachability pruning. Only a
    /// row-mode executor benefits: carry-mode executors flow their indices to the
    /// late stage (never pruned here) and aggregate executors emit a single
    /// scalar row with no source object. Calling this on a carry/aggregate
    /// executor is a no-op, so the sidecar stays `None` and no allocation happens.
    /// The caller (`ScanDriver`) only invokes this when `--reachable-only` is on,
    /// so every default-off / `--all` / analyze run leaves `row_src == None`.
    pub fn arm_row_capture(&mut self) {
        if self.carry.is_none() && self.agg_acc.is_none() {
            self.row_src = Some(Vec::new());
        }
    }

    /// Whether this query's FROM pattern can match an array class, so the scan
    /// only pays the per-array name-construction cost when some executor might
    /// consume it. Array class names end in `[]` (e.g. `char[]`,
    /// `java.lang.Object[]`); a wildcard pattern (`*`) may also match them.
    pub fn wants_arrays(&self) -> bool {
        // A FROM-subquery matches every object (identity is constrained by the
        // outer semi-join), so it must see arrays too.
        if self.query.from.as_subquery().is_some() {
            return true;
        }
        // A `FROM OBJECTS <address>` object may itself be an array — we don't know
        // its kind before resolving it — so the scan must deliver arrays too.
        if matches!(self.query.from, FromSource::Object(_)) {
            return true;
        }
        let from = self.query.from.class_name();
        // A quoted regex could match an array class name (e.g. `.*` or
        // `java\.lang\..*\[\]`) without a literal `*`, so a regex FROM must see
        // arrays too. Compiling once and testing per-array would be cheaper to
        // gate, but arrays are a small minority; be conservative and include them.
        if self.from_regex.is_some() {
            return true;
        }
        from.ends_with("[]") || from.contains('*')
    }

    /// The plan this executor runs (borrowed). Used by the driver to tag a
    /// carried query with its plan for the late phase.
    pub fn plan(&self) -> &QueryPlan {
        self.plan
    }

    /// The query AST this executor runs (borrowed). Used by the driver to gather
    /// RefWalk hop field names when arming edge capture.
    pub fn query(&self) -> &Query {
        self.query
    }

    /// The resolver this executor borrows. Used by the driver to decode ref
    /// fields from instance blobs during RefWalk edge capture.
    pub fn resolver(&self) -> &'a R {
        self.resolver
    }

    /// Consume a carry executor, returning the accumulated carry. Panics if this
    /// executor is not in carry mode (caller must check `is_carry`).
    pub fn take_carry(self) -> Carry {
        self.carry.expect("take_carry on a non-carry executor")
    }
    fn class_matches(&self, class_id: u64) -> bool {
        // A FROM-subquery source has no class pattern of its own: identity is
        // constrained by the outer semi-join against the inner result, so the
        // scan must consider every object here.
        if self.query.from.as_subquery().is_some() {
            return true;
        }
        let Some(spec) = self.query.from.class_spec() else {
            // No class spec means a subquery source, handled above.
            return true;
        };
        if spec.instanceof {
            // `FROM INSTANCEOF C` matches C and every subclass. Walk the
            // superclass chain (LiveResolver override); test resolvers with no
            // hierarchy fall back to exact match.
            return self
                .resolver
                .is_instance_of(class_id, spec, self.from_regex.as_ref());
        }
        match self.resolver.class_name(class_id) {
            None => false,
            Some(name) => class_name_matches_spec(name, spec, self.from_regex.as_ref()),
        }
    }
    /// Strip a leading `<alias>.` from a field reference so `s.count` resolves as
    /// the bare field `count` when the FROM clause binds alias `s`. Fields with
    /// no matching alias prefix (or no alias in scope) pass through unchanged.
    fn strip_alias<'n>(&self, name: &'n str) -> &'n str {
        if let Some(alias) = &self.query.alias {
            if let Some(rest) = name.strip_prefix(alias.as_str()) {
                if let Some(field) = rest.strip_prefix('.') {
                    return field;
                }
            }
        }
        name
    }
    fn project_row(&self, src_idx: usize, class_id: u64, blob: &[u8]) -> Vec<QueryValue> {
        self.query
            .select
            .iter()
            .map(|item| self.project_item(item, src_idx, class_id, blob))
            .collect()
    }
    fn project_item(
        &self,
        item: &SelectItem,
        src_idx: usize,
        class_id: u64,
        blob: &[u8],
    ) -> QueryValue {
        match item {
            SelectItem::Star => QueryValue::ObjRef {
                index: src_idx as u64,
                class: self
                    .resolver
                    .class_name(class_id)
                    .unwrap_or("?")
                    .to_string(),
                addr: self.resolver.addr_of(src_idx),
            },
            SelectItem::Aggregate { .. } => QueryValue::Null,
            SelectItem::Attr(a) => self.project_attr(a, src_idx, class_id, blob),
            // path(a, b) is cross-phase (needs the ref graph); filled later, not here.
            SelectItem::Path { .. } => QueryValue::Null,
            // toString(s): the String path is decoded late (ResolveStringValues);
            // a non-String object gets MAT's fallback display form at scan time.
            SelectItem::ToString(_) => self.tostring_display(class_id, src_idx),
            SelectItem::Expr(e) => self.eval_expr(e, src_idx, class_id, blob),
        }
    }
    /// True when the FROM class is java.lang.String (exact or short/slash form).
    /// Only the String path is decoded late (ResolveStringValues); every other
    /// class renders its toString at scan time as `<class> @ 0x<addr>`.
    // `from_` here names the OQL FROM clause, not a type conversion, so the
    // wrong_self_convention lint (which expects `from_*` to be static) is a false
    // positive for this domain method.
    #[allow(clippy::wrong_self_convention)]
    fn from_is_string(&self) -> bool {
        crate::query::plan::is_string_class_name(self.query.from.class_name())
    }
    /// MAT's fallback display for a non-String object: `<class> @ 0x<addr>`.
    /// Used by toString on any non-String FROM (String is decoded late instead).
    fn tostring_display(&self, class_id: u64, src_idx: usize) -> QueryValue {
        if self.from_is_string() {
            return QueryValue::Null;
        }
        let cname = self.resolver.class_name(class_id).unwrap_or("?");
        self.tostring_display_named(cname, src_idx)
    }
    /// Same MAT fallback display as `tostring_display`, but for callers that
    /// already hold the class name as a `&str` (array rows carry no `class_id`).
    /// The String-Null branch is preserved so the two forms stay in lock-step.
    fn tostring_display_named(&self, class_name: &str, src_idx: usize) -> QueryValue {
        if self.from_is_string() {
            return QueryValue::Null;
        }
        match self.resolver.addr_of(src_idx) {
            Some(a) => QueryValue::Str(format!("{class_name} @ 0x{a:x}")),
            None => QueryValue::Str(format!("{class_name} @ ?")),
        }
    }
    fn project_attr(&self, a: &Attr, src_idx: usize, class_id: u64, blob: &[u8]) -> QueryValue {
        match a {
            Attr::ObjectId => QueryValue::Int(src_idx as i64),
            Attr::ObjectAddress => self
                .resolver
                .addr_of(src_idx)
                .map(|x| QueryValue::Int(x as i64))
                .unwrap_or(QueryValue::Null),
            Attr::UsedHeapSize => self
                .resolver
                .shallow_of(src_idx)
                .map(|x| QueryValue::Int(x as i64))
                .unwrap_or(QueryValue::Null),
            // filled cross-phase (stage runner) — retained size is unknown during the pass2 scan.
            Attr::RetainedHeapSize => QueryValue::Null,
            // dominator-tree attrs are cross-phase: the dominator tree exists only
            // post-scan, so these are filled by the stage runner, not here.
            Attr::Dominators(_) | Attr::DominatorOf(_) => QueryValue::Null,
            Attr::ClassOf | Attr::DisplayName => QueryValue::Str(
                self.resolver
                    .class_name(class_id)
                    .unwrap_or("?")
                    .to_string(),
            ),
            Attr::Length => QueryValue::Null,
            // inbound/outbound reference counts need the post-scan ref graph; filled later.
            Attr::Inbounds | Attr::Outbounds => QueryValue::Null,
            Attr::Field(name) => self.decode_field(class_id, name, blob),
            // N-hop reference paths resolve against the forward-ref graph, which
            // only exists post-scan (P2). Filled by the stage runner, not here.
            Attr::RefPath { .. } => QueryValue::Null,
            // D4b: late resolution — ref-hop attrs; scan-time projects Null.
            Attr::ValueArray | Attr::ReferenceArray => QueryValue::Null,
            // G1: GC-root attrs; resolved in analyze-mode late phase only.
            Attr::GcRoots | Attr::GcRootInfo => QueryValue::Null,
            // Array index/slice: resolved in P2 late window; scan-time projects Null.
            Attr::ArrayIndex { .. } | Attr::ArraySlice { .. } => QueryValue::Null,
            Attr::ToString(_) => {
                // String FROM is decoded late (ResolveStringValues). A non-String
                // object has no decodable text, so we mirror MAT's fallback display
                // form <class> @ 0x<addr>, computed here at scan time (no late op).
                self.tostring_display(class_id, src_idx)
            }
            Attr::ToHex(inner) => match self.eval_expr(inner, src_idx, class_id, blob) {
                // as u64: render high-bit addresses unsigned, not as -0x… (i64 stores
                // addresses above i64::MAX as negative — see the lexer's u64-bit parse).
                QueryValue::Int(n) => QueryValue::Str(format!("0x{:x}", n as u64)),
                _ => QueryValue::Null,
            },
        }
    }
    /// Project a SELECT row for an array object. Arrays carry no field blob and
    /// no resolvable class-object address, so identity/class attrs are served
    /// from `class_name`, `@length` from `length`, and named fields are Null.
    fn project_array_row(&self, src_idx: usize, class_name: &str, length: u32) -> Vec<QueryValue> {
        self.query
            .select
            .iter()
            .map(|item| self.project_array_item(item, src_idx, class_name, length))
            .collect()
    }
    fn project_array_item(
        &self,
        item: &SelectItem,
        src_idx: usize,
        class_name: &str,
        length: u32,
    ) -> QueryValue {
        match item {
            SelectItem::Star => QueryValue::ObjRef {
                index: src_idx as u64,
                class: class_name.to_string(),
                addr: self.resolver.addr_of(src_idx),
            },
            SelectItem::Aggregate { .. } => QueryValue::Null,
            SelectItem::Attr(a) => self.project_array_attr(a, src_idx, class_name, length),
            // path(a, b) is cross-phase (needs the ref graph); filled later, not here.
            SelectItem::Path { .. } => QueryValue::Null,
            // toString(s): arrays are never String-decoded, so this always renders
            // the MAT fallback display form <class> @ 0x<addr> at scan time.
            SelectItem::ToString(_) => self.tostring_display_named(class_name, src_idx),
            SelectItem::Expr(e) => self.eval_expr_array(e, src_idx, class_name, length),
        }
    }
    fn project_array_attr(
        &self,
        a: &Attr,
        src_idx: usize,
        class_name: &str,
        length: u32,
    ) -> QueryValue {
        match a {
            Attr::ObjectId => QueryValue::Int(src_idx as i64),
            Attr::ObjectAddress => self
                .resolver
                .addr_of(src_idx)
                .map(|x| QueryValue::Int(x as i64))
                .unwrap_or(QueryValue::Null),
            Attr::UsedHeapSize => self
                .resolver
                .shallow_of(src_idx)
                .map(|x| QueryValue::Int(x as i64))
                .unwrap_or(QueryValue::Null),
            // filled cross-phase (stage runner) — retained size is unknown during the pass2 scan.
            Attr::RetainedHeapSize => QueryValue::Null,
            // dominator-tree attrs are cross-phase: filled by the stage runner.
            Attr::Dominators(_) | Attr::DominatorOf(_) => QueryValue::Null,
            Attr::ClassOf | Attr::DisplayName => QueryValue::Str(class_name.to_string()),
            Attr::Length => QueryValue::Int(length as i64),
            // inbound/outbound reference counts need the post-scan ref graph; filled later.
            Attr::Inbounds | Attr::Outbounds => QueryValue::Null,
            // Arrays have no named fields; a field reference resolves to Null.
            Attr::Field(_) => QueryValue::Null,
            // Arrays have no reference fields to walk; a RefPath is Null.
            Attr::RefPath { .. } => QueryValue::Null,
            // D4b: late resolution — ref-hop attrs.
            // @valueArray: arrays have no `.value` field (the rewriter would have
            // lowered it to a RefPath); scan-time projects Null.
            Attr::ValueArray => QueryValue::Null,
            // @referenceArray on an array object: return the array itself as an
            // ObjRef (the array IS the reference-typed element container). The
            // plan-time check (`reject_reference_array_on_instance`) already
            // rejects this attr when FROM is a non-array instance class.
            Attr::ReferenceArray => QueryValue::ObjRef {
                index: src_idx as u64,
                class: class_name.to_string(),
                addr: self.resolver.addr_of(src_idx),
            },
            // G1: GC-root attrs; resolved in analyze-mode late phase only.
            Attr::GcRoots | Attr::GcRootInfo => QueryValue::Null,
            // Array index/slice: resolved in P2 late window; scan-time projects Null.
            Attr::ArrayIndex { .. } | Attr::ArraySlice { .. } => QueryValue::Null,
            Attr::ToString(_) => {
                // Arrays are never String-decoded (from_is_string() is false here),
                // so this always renders the MAT fallback display form
                // <class> @ 0x<addr> at scan time — same shape as project_attr.
                self.tostring_display_named(class_name, src_idx)
            }
            Attr::ToHex(inner) => match self.eval_expr_array(inner, src_idx, class_name, length) {
                // as u64: render high-bit addresses unsigned (see the instance arm above).
                QueryValue::Int(n) => QueryValue::Str(format!("0x{:x}", n as u64)),
                _ => QueryValue::Null,
            },
        }
    }
    /// Recursively evaluate an arithmetic `Expr` for an instance object. Leaf
    /// `Expr::Attr` delegates to `project_attr`; `Expr::Lit` converts via
    /// `value_to_qv`; `Binary`/`Unary` apply Java numeric semantics via `arith`/
    /// `unary`. Undefined arithmetic (Null/Str/Bool operands, int div-by-zero)
    /// yields `QueryValue::Null` rather than panicking — a row must never crash
    /// the analyzer.
    fn eval_expr(&self, e: &Expr, src_idx: usize, class_id: u64, blob: &[u8]) -> QueryValue {
        match e {
            Expr::Attr(a) => self.project_attr(a, src_idx, class_id, blob),
            Expr::Lit(v) => value_to_qv(v),
            Expr::Binary { op, lhs, rhs } => {
                let l = self.eval_expr(lhs, src_idx, class_id, blob);
                let r = self.eval_expr(rhs, src_idx, class_id, blob);
                arith(&l, *op, &r)
            }
            Expr::Unary { op, arg } => unary(*op, &self.eval_expr(arg, src_idx, class_id, blob)),
            Expr::Method {
                receiver,
                name,
                args,
            } => self.dispatch_method(receiver, name, args, src_idx, class_id, blob),
            // Aggregate expressions are only valid in HAVING; calling eval_expr
            // on one during per-row scan means the planner allowed it incorrectly.
            Expr::Aggregate { .. } => QueryValue::Null,
            Expr::Case { branches, else_ } => {
                for (cond, then_expr) in branches {
                    if self.eval_pred(cond, src_idx, class_id, blob) {
                        return self.eval_expr(then_expr, src_idx, class_id, blob);
                    }
                }
                match else_ {
                    Some(e) => self.eval_expr(e, src_idx, class_id, blob),
                    None => QueryValue::Null,
                }
            }
            Expr::Coalesce(args) => {
                for arg in args {
                    let v = self.eval_expr(arg, src_idx, class_id, blob);
                    if !matches!(v, QueryValue::Null) {
                        return v;
                    }
                }
                QueryValue::Null
            }
            Expr::NullIf { lhs, rhs } => {
                let lv = self.eval_expr(lhs, src_idx, class_id, blob);
                let rv = self.eval_expr(rhs, src_idx, class_id, blob);
                if lv == rv { QueryValue::Null } else { lv }
            }
        }
    }
    /// semantics as `eval_expr`; delegates attr leaves to `project_array_attr`.
    fn eval_expr_array(
        &self,
        e: &Expr,
        src_idx: usize,
        class_name: &str,
        length: u32,
    ) -> QueryValue {
        match e {
            Expr::Attr(a) => self.project_array_attr(a, src_idx, class_name, length),
            Expr::Lit(v) => value_to_qv(v),
            Expr::Binary { op, lhs, rhs } => {
                let l = self.eval_expr_array(lhs, src_idx, class_name, length);
                let r = self.eval_expr_array(rhs, src_idx, class_name, length);
                arith(&l, *op, &r)
            }
            Expr::Unary { op, arg } => {
                unary(*op, &self.eval_expr_array(arg, src_idx, class_name, length))
            }
            Expr::Method { name, .. } => match name.as_str() {
                // Arrays expose only `length`/`size`; everything else is Null for now.
                "length" | "size" => QueryValue::Int(length as i64),
                _ => QueryValue::Null,
            },
            Expr::Aggregate { .. } => QueryValue::Null,
            Expr::Case { branches, else_ } => {
                for (cond, then_expr) in branches {
                    if self.array_eval_pred(cond, src_idx, class_name, length) {
                        return self.eval_expr_array(then_expr, src_idx, class_name, length);
                    }
                }
                match else_ {
                    Some(e) => self.eval_expr_array(e, src_idx, class_name, length),
                    None => QueryValue::Null,
                }
            }
            Expr::Coalesce(args) => {
                for arg in args {
                    let v = self.eval_expr_array(arg, src_idx, class_name, length);
                    if !matches!(v, QueryValue::Null) {
                        return v;
                    }
                }
                QueryValue::Null
            }
            Expr::NullIf { lhs, rhs } => {
                let lv = self.eval_expr_array(lhs, src_idx, class_name, length);
                let rv = self.eval_expr_array(rhs, src_idx, class_name, length);
                if lv == rv { QueryValue::Null } else { lv }
            }
        }
    }
    /// Dispatch a method call on an instance object. Tier-2: fixed name → `Attr`
    /// alias table (MAT-API names). Unknown names fall through to
    /// `emulate_jvm_method` (stub, filled by D3/D4).
    fn dispatch_method(
        &self,
        receiver: &Expr,
        name: &str,
        args: &[Expr],
        src_idx: usize,
        class_id: u64,
        blob: &[u8],
    ) -> QueryValue {
        match name {
            "getName" => self.project_attr(&Attr::DisplayName, src_idx, class_id, blob),
            "getObjectAddress" => self.project_attr(&Attr::ObjectAddress, src_idx, class_id, blob),
            "getObjectId" => self.project_attr(&Attr::ObjectId, src_idx, class_id, blob),
            "getUsedHeapSize" => self.project_attr(&Attr::UsedHeapSize, src_idx, class_id, blob),
            "getRetainedHeapSize" => {
                self.project_attr(&Attr::RetainedHeapSize, src_idx, class_id, blob)
            }
            "getClazz" => self.project_attr(&Attr::ClassOf, src_idx, class_id, blob),
            // For a String receiver this yields the `<class> @ 0x<addr>` display, not
            // the decoded string content: `Expr::Method` bypasses the `SelectItem::Expr`
            // path that arms `needs.string_values` in the planner, so the value side
            // table is never populated. Non-String toString (the common case) is correct.
            "toString" => {
                self.project_attr(&Attr::ToString(String::new()), src_idx, class_id, blob)
            }
            "length" => self.project_attr(&Attr::Length, src_idx, class_id, blob),
            _ => self.emulate_jvm_method(receiver, name, args, src_idx, class_id, blob),
        }
    }
    fn emulate_jvm_method(
        &self,
        receiver: &Expr,
        name: &str,
        args: &[Expr],
        src_idx: usize,
        class_id: u64,
        blob: &[u8],
    ) -> QueryValue {
        let cname = self.resolver.class_name(class_id).unwrap_or("");
        match (name, cname) {
            ("intValue" | "longValue" | "shortValue" | "byteValue", c) if is_boxed_integral(c) => {
                self.decode_field(class_id, "value", blob)
            }
            ("floatValue" | "doubleValue", c) if is_boxed_fp(c) => {
                self.decode_field(class_id, "value", blob)
            }
            ("booleanValue", "java.lang.Boolean") => self.decode_field(class_id, "value", blob),
            ("charValue", "java.lang.Character") => self.decode_field(class_id, "value", blob),
            ("size", c) if is_sized_collection(c) => self.decode_field(class_id, "size", blob),
            ("equals", _) => {
                let recv = self.eval_expr(receiver, src_idx, class_id, blob);
                let arg = args
                    .first()
                    .map(|a| self.eval_expr(a, src_idx, class_id, blob))
                    .unwrap_or(QueryValue::Null);
                QueryValue::Bool(qv_value_eq(&recv, &arg))
            }
            ("contains", "java.lang.String") => {
                let recv = self.eval_expr(receiver, src_idx, class_id, blob);
                let arg = args
                    .first()
                    .map(|a| self.eval_expr(a, src_idx, class_id, blob));
                match (recv, arg) {
                    (QueryValue::Str(hay), Some(QueryValue::Str(needle))) => {
                        QueryValue::Bool(hay.contains(&needle))
                    }
                    // At scan time a String receiver's decoded text is NOT available:
                    // `Expr::Method` bypasses the `SelectItem::Expr` path that arms
                    // `needs.string_values`, so the value side table is unpopulated and
                    // the receiver projects the `<class> @ 0x<addr>` fallback (or Null),
                    // never `QueryValue::Str(<content>)`. `contains` therefore yields
                    // Null here rather than a wrong Bool. (D5 limitation, option (a).)
                    _ => QueryValue::Null,
                }
            }
            _ => QueryValue::Null, // ref-hop (D4) or rejection (D5)
        }
    }
    fn decode_field(&self, class_id: u64, name: &str, blob: &[u8]) -> QueryValue {
        use crate::types::HprofType;
        let name = self.strip_alias(name);
        let Some((off, ty)) = self.resolver.field(class_id, name) else {
            return QueryValue::Null;
        };
        let o = off as usize;
        match ty {
            HprofType::Boolean | HprofType::Byte => blob
                .get(o)
                .map(|&b| {
                    if ty == HprofType::Boolean {
                        QueryValue::Bool(b != 0)
                    } else {
                        QueryValue::Int(b as i64)
                    }
                })
                .unwrap_or(QueryValue::Null),
            HprofType::Short => read_be(blob, o, 2)
                .map(|v| QueryValue::Int(v as i16 as i64))
                .unwrap_or(QueryValue::Null),
            HprofType::Char => read_be(blob, o, 2)
                .map(|v| QueryValue::Int(v as i64))
                .unwrap_or(QueryValue::Null),
            HprofType::Int => read_be(blob, o, 4)
                .map(|v| QueryValue::Int(v as i32 as i64))
                .unwrap_or(QueryValue::Null),
            HprofType::Long => read_be(blob, o, 8)
                .map(|v| QueryValue::Int(v as i64))
                .unwrap_or(QueryValue::Null),
            HprofType::Float => read_be(blob, o, 4)
                .map(|v| QueryValue::Float(f32::from_bits(v as u32) as f64))
                .unwrap_or(QueryValue::Null),
            HprofType::Double => read_be(blob, o, 8)
                .map(|v| QueryValue::Float(f64::from_bits(v)))
                .unwrap_or(QueryValue::Null),
            HprofType::Object => QueryValue::Null,
        }
    }
    fn where_passes(&self, src_idx: usize, class_id: u64, blob: &[u8]) -> bool {
        // Reset the EXISTS-result cursor so the DFS walk over where_terms picks
        // up exists_bools entries in the same encounter order they were planned.
        self.exists_cursor.set(0);
        for term in &self.plan.where_terms {
            // In carry mode, @retainedHeapSize and toString() WHERE terms can't
            // be evaluated during the scan (retained size / string value are
            // unknown here); they are applied late in stage_runner. Skip them
            // here so the predicate doesn't spuriously compare against Null and
            // drop every row before the late phase runs. Only String toString is
            // deferred: a non-String toString renders `<class> @ 0x<addr>` at scan
            // time, so it IS known here and must be evaluated (not deferred).
            if self.carry.is_some()
                && (crate::query::plan::pred_uses_retained(&term.pred)
                    || crate::query::plan::pred_uses_refpath(&term.pred)
                    || (self.from_is_string()
                        && crate::query::plan::pred_uses_tostring(&term.pred)))
            {
                continue;
            }
            if !self.eval_pred(&term.pred, src_idx, class_id, blob) {
                return false;
            }
        }
        true
    }
    /// Look up the pre-compiled LIKE regex for a Compare RHS. Returns `None` for
    /// non-string RHS (LIKE is string-only) or when no pattern was compiled (a
    /// query without LIKE). Never compiles on this hot path.
    fn like_re_for(&self, rhs: &Value) -> Option<&regex::Regex> {
        match rhs {
            Value::Str(pat) => self.like_regexes.get(pat),
            _ => None,
        }
    }
    fn eval_pred(
        &self,
        pred: &crate::query::ast::Predicate,
        src_idx: usize,
        class_id: u64,
        blob: &[u8],
    ) -> bool {
        use crate::query::ast::Predicate as P;
        match pred {
            P::And(a, b) => {
                self.eval_pred(a, src_idx, class_id, blob)
                    && self.eval_pred(b, src_idx, class_id, blob)
            }
            P::Or(a, b) => {
                self.eval_pred(a, src_idx, class_id, blob)
                    || self.eval_pred(b, src_idx, class_id, blob)
            }
            P::Not(a) => !self.eval_pred(a, src_idx, class_id, blob),
            P::InstanceOf(cname) => {
                // `WHERE x INSTANCEOF C` follows Java semantics: match C and any
                // subclass. Reuse the resolver's hierarchy walk (LiveResolver
                // override) via a synthetic exact-match spec; test resolvers with
                // no super-chain degrade to exact match.
                let spec = crate::query::ast::ClassSpec {
                    instanceof: true,
                    class_name: cname.clone(),
                    is_regex: false,
                };
                self.resolver.is_instance_of(class_id, &spec, None)
            }
            P::InSubquery { lhs, .. } => self.eval_in_subquery(lhs, src_idx),
            P::Exists { .. } => {
                // Non-correlated: look up the pre-evaluated bool by encounter
                // order. The cursor was reset to 0 in `where_passes` before
                // the term loop, so this correctly maps each Exists node to
                // its `exists_bools` entry regardless of tree shape.
                let idx = self.exists_cursor.get();
                self.exists_cursor.set(idx + 1);
                self.exists_bools.get(idx).copied().unwrap_or(false)
            }
            P::Compare { lhs, op, rhs } => {
                // Pass the real `src_idx` so object-identity LHS attrs
                // (@objectAddress/@objectId) compare against the actual object,
                // not a placeholder. Blob-scalar and class/type LHS attrs ignore
                // the index, so this is a no-op for them.
                let lv = self.eval_expr(lhs, src_idx, class_id, blob);
                let rv = self.eval_expr(rhs, src_idx, class_id, blob);
                // LIKE regex is keyed by the literal RHS string pattern; only a
                // string-literal RHS has one (arithmetic RHS → None, which is
                // correct: LIKE RHS is validated as string literal at parse time).
                let like_re = rhs.as_lit().and_then(|v| self.like_re_for(v));
                compare_values(&lv, *op, &rv, like_re)
            }
        }
    }

    /// Evaluate a `WHERE <lhs> IN (<subquery>)` predicate against the injected
    /// membership set. The set is matched to the predicate by its `lhs` attr
    /// (the only IN LHS this slice supports is `@objectAddress`). Membership
    /// tests the current object's real address against the set. A missing set
    /// (driver never injected one — a wiring bug) yields the loud unreachable.
    fn eval_in_subquery(&self, lhs: &Attr, src_idx: usize) -> bool {
        let Some(inset) = self.in_sets.iter().find(|s| &s.lhs == lhs) else {
            return in_subquery_unresolved();
        };
        match self.resolver.addr_of(src_idx) {
            Some(addr) => crate::query::run::in_subquery_contains(&inset.set, addr),
            None => false,
        }
    }

    /// WHERE evaluation for array objects: only `@length`, `@objectId`, and
    /// `INSTANCEOF` are meaningful; named-field compares resolve to Null (and
    /// thus behave like the instance path's unknown-field handling).
    fn array_where_passes(&self, src_idx: usize, class_name: &str, length: u32) -> bool {
        // Reset the EXISTS-result cursor (same as in where_passes).
        self.exists_cursor.set(0);
        for term in &self.plan.where_terms {
            // See `where_passes`: skip retained/String-toString terms in carry
            // mode. A non-String toString is scan-time-known, so it is NOT skipped.
            if self.carry.is_some()
                && (crate::query::plan::pred_uses_retained(&term.pred)
                    || crate::query::plan::pred_uses_refpath(&term.pred)
                    || (self.from_is_string()
                        && crate::query::plan::pred_uses_tostring(&term.pred)))
            {
                continue;
            }
            if !self.array_eval_pred(&term.pred, src_idx, class_name, length) {
                return false;
            }
        }
        true
    }
    fn array_eval_pred(
        &self,
        pred: &crate::query::ast::Predicate,
        src_idx: usize,
        class_name: &str,
        length: u32,
    ) -> bool {
        use crate::query::ast::Predicate as P;
        match pred {
            P::And(a, b) => {
                self.array_eval_pred(a, src_idx, class_name, length)
                    && self.array_eval_pred(b, src_idx, class_name, length)
            }
            P::Or(a, b) => {
                self.array_eval_pred(a, src_idx, class_name, length)
                    || self.array_eval_pred(b, src_idx, class_name, length)
            }
            P::Not(a) => !self.array_eval_pred(a, src_idx, class_name, length),
            P::InstanceOf(cname) => class_name_matches(class_name, cname),
            P::InSubquery { lhs, .. } => self.eval_in_subquery(lhs, src_idx),
            P::Exists { .. } => {
                let idx = self.exists_cursor.get();
                self.exists_cursor.set(idx + 1);
                self.exists_bools.get(idx).copied().unwrap_or(false)
            }
            P::Compare { lhs, op, rhs } => {
                let lv = self.eval_expr_array(lhs, src_idx, class_name, length);
                let rv = self.eval_expr_array(rhs, src_idx, class_name, length);
                let like_re = rhs.as_lit().and_then(|v| self.like_re_for(v));
                compare_values(&lv, *op, &rv, like_re)
            }
        }
    }

    pub fn finish(self, name: &str) -> QueryResult {
        self.finish_with_src(name).0
    }

    /// Finalize like [`finish`], but also return the per-row source dense-index
    /// sidecar when reachability capture was armed (`arm_row_capture`). The
    /// sidecar is kept in lockstep with the output rows through ORDER BY sort +
    /// LIMIT truncate so element `i` of the returned `Vec<u32>` is the dense
    /// object index that produced output row `i`. `None` when capture was
    /// disarmed (the default) or for the aggregate path (a single scalar row with
    /// no source object) — in which case the caller keeps all rows unconditionally.
    pub fn finish_with_src(self, name: &str) -> (QueryResult, Option<Vec<u32>>) {
        let columns = query_columns(self.query);
        // GROUP BY mode: finalize all per-group accumulators into result rows.
        if let Some(group_map) = self.group_map {
            let mut rows: Vec<Vec<QueryValue>> = Vec::with_capacity(group_map.len());
            for (_key_str, (key, accs)) in group_map {
                // Finalize all accumulators first (consuming them).
                let finalized: Vec<QueryValue> = accs.into_iter().map(finalize_agg_acc).collect();
                // Build one output row: aggregates from finalized acc, non-aggregates
                // from the GROUP BY key vector matched by position in group_by_exprs.
                let row: Vec<QueryValue> = self
                    .query
                    .select
                    .iter()
                    .enumerate()
                    .map(|(i, item)| match item {
                        SelectItem::Aggregate { .. } => {
                            finalized.get(i).cloned().unwrap_or(QueryValue::Null)
                        }
                        _ => {
                            // Non-aggregate: find the matching GROUP BY key by position.
                            // group_by_exprs[j] corresponds to key[j]; find the first
                            // group_by_expr that structurally matches this select item.
                            let col_name = column_name(item);
                            let gb_match =
                                self.plan.group_by_exprs.iter().enumerate().find(|(_, ge)| {
                                    // Match by column name equality: the select item's
                                    // display name should match the group-by expr's name.
                                    let ge_name = expr_name(ge);
                                    ge_name == col_name
                                        || match (ge, item) {
                                            (Expr::Attr(ga), SelectItem::Attr(a)) => ga == a,
                                            (Expr::Attr(ga), SelectItem::Expr(e)) => {
                                                matches!(e.as_ref(), Expr::Attr(ea) if ea == ga)
                                            }
                                            _ => false,
                                        }
                                });
                            match gb_match {
                                Some((j, _)) => key.get(j).cloned().unwrap_or(QueryValue::Null),
                                // Fallback: take the first key value if available.
                                None => key.first().cloned().unwrap_or(QueryValue::Null),
                            }
                        }
                    })
                    .collect();
                // Apply HAVING filter.
                let having_ok = self.plan.having_terms.iter().all(|term| {
                    eval_having_term(&term.pred, &row, self.query, &columns, &self.like_regexes)
                });
                if having_ok {
                    rows.push(row);
                }
            }
            // Apply ORDER BY.
            if let Some(ob) = &self.query.order_by {
                if let Some(idx) = order_by_column_index(self.query, &columns, &ob.key) {
                    sort_rows_by_column(&mut rows, idx, ob.dir);
                }
            }
            // Apply LIMIT.
            if let Some(limit) = self.plan.limit {
                if rows.len() > limit as usize {
                    rows.truncate(limit as usize);
                }
            }
            let row_count = rows.len() as u64;
            return (
                QueryResult {
                    name: name.to_string(),
                    oql: String::new(),
                    columns,
                    row_count,
                    rows,
                    truncated: self.truncated,
                    error: None,
                    note: None,
                    viz: None,
                    elapsed_ms: None,
                },
                None,
            );
        }
        if let Some(accs) = self.agg_acc {
            // Aggregate mode: finalize each accumulator and emit exactly one row.
            // LIMIT on an aggregate produces at most one output row; if
            // `limit == Some(0)` the caller wants zero rows (degenerate but valid).
            if self.plan.limit == Some(0) {
                return (
                    QueryResult {
                        name: name.to_string(),
                        oql: String::new(),
                        columns,
                        row_count: 0,
                        rows: vec![],
                        truncated: false,
                        error: None,
                        note: None,
                        viz: None,
                        elapsed_ms: None,
                    },
                    None,
                );
            }
            let row: Vec<QueryValue> = accs.into_iter().map(finalize_agg_acc).collect();
            (
                QueryResult {
                    name: name.to_string(),
                    oql: String::new(),
                    columns,
                    row_count: 1,
                    rows: vec![row],
                    truncated: self.truncated,
                    error: None,
                    note: None,
                    viz: None,
                    elapsed_ms: None,
                },
                None,
            )
        } else {
            let mut rows = self.rows;
            let mut row_src = self.row_src;
            let mut note = None;
            // Apply a general ORDER BY sort for the scan-time (non-carry) path.
            // The retained-late path carries indices forward and sorts in
            // stage_runner, so it never reaches here with rows to sort; guard on
            // carry so we never emit a spurious note for a late-sorted query.
            if self.carry.is_none() {
                // A FROM-subquery defers its LIMIT to the post-scan semi-join in
                // run.rs (truncating here would cap rows the semi-join has not yet
                // filtered). We still sort, so the semi-join preserves top-N order.
                let defer_limit = self.query.from.as_subquery().is_some();
                if let Some(ob) = &self.query.order_by {
                    match order_by_column_index(self.query, &columns, &ob.key) {
                        Some(idx) => {
                            sort_rows_with_src(&mut rows, &mut row_src, idx, ob.dir);
                            if !defer_limit {
                                if let Some(limit) = self.plan.limit {
                                    if rows.len() > limit as usize {
                                        rows.truncate(limit as usize);
                                        if let Some(v) = &mut row_src {
                                            v.truncate(limit as usize);
                                        }
                                        // Truncation after an explicit sort is the intended
                                        // top-N, not a lost-data warning: leave `truncated`
                                        // reflecting only scan-cap loss (none on this path).
                                    }
                                }
                            }
                        }
                        None => {
                            // Key is not a projected column we can order by here
                            // (e.g. a non-selected field). Keep scan order and say so,
                            // rather than silently pretending the sort happened.
                            note = Some(format!(
                                "ORDER BY `{}` was not applied: the sort key must be a \
                                 selected column on this query path; rows are in scan order",
                                attr_name(&ob.key)
                            ));
                            if !defer_limit {
                                if let Some(limit) = self.plan.limit {
                                    if rows.len() > limit as usize {
                                        rows.truncate(limit as usize);
                                        if let Some(v) = &mut row_src {
                                            v.truncate(limit as usize);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            (
                QueryResult {
                    name: name.to_string(),
                    oql: String::new(),
                    columns,
                    row_count: rows.len() as u64,
                    rows,
                    truncated: self.truncated,
                    error: None,
                    note,
                    viz: None,
                    elapsed_ms: None,
                },
                row_src,
            )
        }
    }

    /// Evaluate one aggregate SELECT item's argument to a per-object `QueryValue`.
    /// Used by the accumulator in `visit_instance`/`visit_array`. `Star` returns
    /// a sentinel `Int(1)` (only COUNT(*) uses Star, and it does not call this at
    /// all — see the fold loop below; this branch is unreachable in practice but
    /// safe). `Attr` delegates to `project_attr`; `Expr` to `eval_expr`.
    fn eval_agg_arg_instance(
        &self,
        arg: &SelectItem,
        src_idx: usize,
        class_id: u64,
        blob: &[u8],
    ) -> QueryValue {
        match arg {
            SelectItem::Star => QueryValue::Int(1), // CountStar handles this without calling here
            SelectItem::Attr(a) => self.project_attr(a, src_idx, class_id, blob),
            SelectItem::Expr(e) => self.eval_expr(e, src_idx, class_id, blob),
            _ => QueryValue::Null,
        }
    }

    fn eval_agg_arg_array(
        &self,
        arg: &SelectItem,
        src_idx: usize,
        class_name: &str,
        length: u32,
    ) -> QueryValue {
        match arg {
            SelectItem::Star => QueryValue::Int(1),
            SelectItem::Attr(a) => self.project_array_attr(a, src_idx, class_name, length),
            SelectItem::Expr(e) => self.eval_expr_array(e, src_idx, class_name, length),
            _ => QueryValue::Null,
        }
    }
}

impl<'a, R: ClassResolver> ObjectVisitor for SingleScanExecutor<'a, R> {
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
        // `FROM OBJECTS <address>`: only the single resolved dense index matches.
        // Gate only for an Object source so every other query is unaffected.
        if let FromSource::Object(_) = &self.query.from {
            if Some(src_idx) != self.target_index {
                return;
            }
        }
        if !self.class_matches(class_id) {
            return;
        }
        if !self.where_passes(src_idx, class_id, blob) {
            return;
        }
        if let Some(carry) = &mut self.carry {
            // Carry mode: no LIMIT here (retained ORDER BY + LIMIT run late);
            // the carry's own cap bounds memory and sets its truncated flag.
            carry.push_index(src_idx as u32);
            return;
        }
        // GROUP BY mode: fold this object into the per-group accumulators.
        if self.group_map.is_some() {
            let key: Vec<QueryValue> = self
                .plan
                .group_by_exprs
                .iter()
                .map(|e| self.eval_expr(e, src_idx, class_id, blob))
                .collect();
            let key_str = format!("{key:?}");
            let values: Vec<QueryValue> = self
                .query
                .select
                .iter()
                .map(|item| match item {
                    SelectItem::Aggregate { arg, .. } => {
                        self.eval_agg_arg_instance(arg, src_idx, class_id, blob)
                    }
                    _ => QueryValue::Null,
                })
                .collect();
            let query_select = self.query.select.as_slice();
            #[allow(clippy::unnecessary_unwrap)]
            let entry = self
                .group_map
                .as_mut()
                .unwrap()
                .entry(key_str)
                .or_insert_with(|| {
                    let init_accs: Vec<AggAcc> = query_select.iter().map(init_agg_acc).collect();
                    (key.clone(), init_accs)
                });
            for (i, acc) in entry.1.iter_mut().enumerate() {
                fold_agg_acc(acc, values[i].clone());
            }
            self.matched += 1;
            return;
        }
        // Aggregate mode: fold this object into the accumulators. LIMIT is not
        // applied per-object — the aggregate produces exactly one output row.
        // (If limit==0 that is handled in finish().)
        if self.agg_acc.is_some() {
            self.matched += 1;
            // Evaluate each arg value outside the mutable borrow of agg_acc to
            // satisfy the borrow checker. Collect into a temporary Vec first.
            let n = self.query.select.len();
            let mut values = Vec::with_capacity(n);
            for item in self.query.select.iter() {
                let v = match item {
                    SelectItem::Aggregate { arg, .. } => {
                        self.eval_agg_arg_instance(arg, src_idx, class_id, blob)
                    }
                    _ => QueryValue::Null,
                };
                values.push(v);
            }
            #[allow(clippy::unnecessary_unwrap)]
            let accs = self.agg_acc.as_mut().unwrap();
            for (i, acc) in accs.iter_mut().enumerate() {
                if !matches!(acc, AggAcc::None) {
                    fold_agg_acc(acc, values[i].clone());
                }
            }
            return;
        }
        // When ORDER BY is present we must sort the FULL matched set in
        // `finish()` before applying LIMIT, so we cannot early-stop here — doing
        // so would take the first N in scan order, not the top N by the sort key.
        // A FROM-subquery source likewise cannot early-stop: the outer LIMIT is
        // applied post-scan, after the semi-join in run.rs, so the scan must
        // collect every match (a scan cap here would cap non-matching objects
        // and the semi-join would then discard them → too few rows).
        // No-ORDER-BY, no-subquery queries keep the byte-identical early-stop.
        if self.query.order_by.is_none() && self.query.from.as_subquery().is_none() {
            if let Some(limit) = self.plan.limit {
                if self.matched >= limit {
                    self.truncated = true;
                    return;
                }
            }
        }
        self.matched += 1;
        let row = self.project_row(src_idx, class_id, blob);
        self.rows.push(row);
        if let Some(v) = &mut self.row_src {
            v.push(src_idx as u32);
        }
    }

    fn visit_array(&mut self, src_idx: usize, class_name: &str, length: u32) {
        // `FROM OBJECTS <address>`: only the single resolved dense index matches
        // (the target object may be an array). Gate only for an Object source.
        if let FromSource::Object(_) = &self.query.from {
            if Some(src_idx) != self.target_index {
                return;
            }
        }
        // A FROM-subquery source matches every object (identity is constrained
        // by the outer semi-join), so it considers arrays too.
        let class_ok = self.query.from.as_subquery().is_some()
            || matches!(self.query.from, FromSource::Object(_))
            || match self.query.from.class_spec() {
                Some(spec) => class_name_matches_spec(class_name, spec, self.from_regex.as_ref()),
                None => false,
            };
        if !class_ok {
            return;
        }
        if !self.array_where_passes(src_idx, class_name, length) {
            return;
        }
        if let Some(carry) = &mut self.carry {
            carry.push_index(src_idx as u32);
            return;
        }
        // GROUP BY mode: fold this array object into the per-group accumulators.
        if self.group_map.is_some() {
            let key: Vec<QueryValue> = self
                .plan
                .group_by_exprs
                .iter()
                .map(|e| self.eval_expr_array(e, src_idx, class_name, length))
                .collect();
            let key_str = format!("{key:?}");
            let values: Vec<QueryValue> = self
                .query
                .select
                .iter()
                .map(|item| match item {
                    SelectItem::Aggregate { arg, .. } => {
                        self.eval_agg_arg_array(arg, src_idx, class_name, length)
                    }
                    _ => QueryValue::Null,
                })
                .collect();
            let query_select = self.query.select.as_slice();
            #[allow(clippy::unnecessary_unwrap)]
            let entry = self
                .group_map
                .as_mut()
                .unwrap()
                .entry(key_str)
                .or_insert_with(|| {
                    let init_accs: Vec<AggAcc> = query_select.iter().map(init_agg_acc).collect();
                    (key.clone(), init_accs)
                });
            for (i, acc) in entry.1.iter_mut().enumerate() {
                fold_agg_acc(acc, values[i].clone());
            }
            self.matched += 1;
            return;
        }
        // Aggregate mode: fold this array object into the accumulators.
        if self.agg_acc.is_some() {
            self.matched += 1;
            let n = self.query.select.len();
            let mut values = Vec::with_capacity(n);
            for item in self.query.select.iter() {
                let v = match item {
                    SelectItem::Aggregate { arg, .. } => {
                        self.eval_agg_arg_array(arg, src_idx, class_name, length)
                    }
                    _ => QueryValue::Null,
                };
                values.push(v);
            }
            #[allow(clippy::unnecessary_unwrap)]
            let accs = self.agg_acc.as_mut().unwrap();
            for (i, acc) in accs.iter_mut().enumerate() {
                if !matches!(acc, AggAcc::None) {
                    fold_agg_acc(acc, values[i].clone());
                }
            }
            return;
        }
        // See the ORDER BY / FROM-subquery note in `visit_instance`: don't
        // early-stop when a sort or a semi-join is pending.
        if self.query.order_by.is_none() && self.query.from.as_subquery().is_none() {
            if let Some(limit) = self.plan.limit {
                if self.matched >= limit {
                    self.truncated = true;
                    return;
                }
            }
        }
        self.matched += 1;
        let row = self.project_array_row(src_idx, class_name, length);
        self.rows.push(row);
        if let Some(v) = &mut self.row_src {
            v.push(src_idx as u32);
        }
    }
}

/// A `WHERE <attr> IN (<subquery>)` predicate must be resolved into an
/// address-set membership filter at plan time (see the subquery planner) before
/// the pass2 scan runs; the scan executor never evaluates it directly. Reaching
/// this means the planner failed to rewrite the predicate — a wiring bug.
fn in_subquery_unresolved() -> bool {
    unreachable!("IN(<subquery>) predicate must be resolved at plan time, not during the scan")
}

/// Match an object's dotted class name against a FROM pattern (exact, or a
/// trailing `.*` package-prefix wildcard). Allocation-free on the hot path:
/// `/` and `.` are treated as equivalent separators so a slash-form pattern
/// still matches a dot-form name without normalizing either into a new String.
pub fn class_name_matches(name_dotted: &str, pattern: &str) -> bool {
    // Package-prefix wildcard `pkg.*`: the name must equal `pkg` or start with
    // `pkg` followed by a separator.
    if let Some(prefix) = pattern
        .strip_suffix(".*")
        .or_else(|| pattern.strip_suffix("/*"))
    {
        if !sep_eq(name_dotted.get(..prefix.len()).unwrap_or(""), prefix) {
            return false;
        }
        return name_dotted.len() == prefix.len()
            || matches!(
                name_dotted.as_bytes().get(prefix.len()),
                Some(b'.') | Some(b'/')
            );
    }
    name_dotted.len() == pattern.len() && sep_eq(name_dotted, pattern)
}

/// Compile the FROM spec's regex ONCE per query, or `Ok(None)` when the spec is
/// not a quoted-regex target (bare ident/glob → matched by `class_name_matches`).
/// A bad regex is an actionable [`QueryError`] naming the problem, surfaced at
/// plan/construction time — never a silent `false` and never a per-row panic.
/// The returned `Regex` is held on the executor / histogram loop and reused for
/// every object, so `Regex::new` is NEVER called on the per-object hot path.
///
/// MAT matches class names like `java.util.regex.Pattern.matches`: the WHOLE
/// string must match. We anchor the source as `^(?:<src>)$` to get that
/// full/anchored semantics regardless of the user's pattern.
pub fn compile_from_regex(
    spec: &crate::query::ast::ClassSpec,
) -> Result<Option<regex::Regex>, crate::query::QueryError> {
    if !spec.is_regex {
        return Ok(None);
    }
    let anchored = format!("^(?:{})$", spec.class_name);
    match regex::Regex::new(&anchored) {
        Ok(re) => Ok(Some(re)),
        Err(e) => Err(crate::query::QueryError(format!(
            "invalid regex in FROM \"{}\": {} \
             (the quoted FROM target is matched as a Java-style regex; \
             fix the pattern or use a bare class name / `pkg.*` glob instead)",
            spec.class_name, e
        ))),
    }
}

/// Match a dotted class `name_dotted` against a FROM [`ClassSpec`]. When the spec
/// is a quoted-regex target, `from_regex` MUST be the pre-compiled regex for that
/// spec (compiled once via [`compile_from_regex`]); the name matches iff the
/// whole string matches (Java `Pattern.matches` semantics). Otherwise this falls
/// through to the exact/glob [`class_name_matches`]. `from_regex` being `None`
/// for a regex spec means the caller failed to compile it — treated as no match
/// (the actionable error is raised earlier at plan/construction time).
pub fn class_name_matches_spec(
    name_dotted: &str,
    spec: &crate::query::ast::ClassSpec,
    from_regex: Option<&regex::Regex>,
) -> bool {
    if spec.is_regex {
        return match from_regex {
            Some(re) => re.is_match(name_dotted),
            None => false,
        };
    }
    class_name_matches(name_dotted, &spec.class_name)
}

/// Byte-wise equality treating `/` and `.` as the same separator.
fn sep_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .all(|(x, y)| x == y || (matches!(x, b'.' | b'/') && matches!(y, b'.' | b'/')))
}

/// Read `n` big-endian bytes at `off` as a u64. None if out of range.
fn read_be(blob: &[u8], off: usize, n: usize) -> Option<u64> {
    if off + n > blob.len() {
        return None;
    }
    let mut v = 0u64;
    for i in 0..n {
        v = (v << 8) | blob[off + i] as u64;
    }
    Some(v)
}

/// Convert a literal AST `Value` to a `QueryValue`.
pub(crate) fn value_to_qv(v: &Value) -> QueryValue {
    match v {
        Value::Int(n) => QueryValue::Int(*n),
        Value::Float(f) => QueryValue::Float(*f),
        Value::Str(s) => QueryValue::Str(s.clone()),
        Value::Bool(b) => QueryValue::Bool(*b),
        Value::Null => QueryValue::Null,
    }
}

/// Apply a binary arithmetic operator with Java numeric semantics.
///
/// - Both `Int`: result is `Int`, using wrapping arithmetic (Java `long` overflow
///   wraps). Division by zero → `QueryValue::Null` (safe row-level sentinel; Java
///   would throw `ArithmeticException` which must not crash the analyzer).
///   `i64::MIN / -1` also uses `wrapping_div` so it wraps to `i64::MIN`.
/// - Any `Float` operand: both sides promoted to `f64`, result is `Float`.
///   Float division by zero → IEEE 754 `±inf`/`NaN` (Java parity).
/// - Any `Null`/`Str`/`Bool`/`ObjRef` operand → `QueryValue::Null` (arithmetic
///   undefined on those types; no coercion).
pub(crate) fn arith(lhs: &QueryValue, op: ArithOp, rhs: &QueryValue) -> QueryValue {
    match (lhs, rhs) {
        (QueryValue::Int(a), QueryValue::Int(b)) => match op {
            ArithOp::Add => QueryValue::Int(a.wrapping_add(*b)),
            ArithOp::Sub => QueryValue::Int(a.wrapping_sub(*b)),
            ArithOp::Mul => QueryValue::Int(a.wrapping_mul(*b)),
            ArithOp::Div => {
                if *b == 0 {
                    QueryValue::Null
                } else {
                    QueryValue::Int(a.wrapping_div(*b))
                }
            }
        },
        // Any Float operand → promote both to f64.
        (QueryValue::Float(a), QueryValue::Float(b)) => match op {
            ArithOp::Add => QueryValue::Float(a + b),
            ArithOp::Sub => QueryValue::Float(a - b),
            ArithOp::Mul => QueryValue::Float(a * b),
            ArithOp::Div => QueryValue::Float(a / b), // IEEE 754: ÷0 → ±inf/NaN
        },
        (QueryValue::Int(a), QueryValue::Float(b)) => {
            let a = *a as f64;
            match op {
                ArithOp::Add => QueryValue::Float(a + b),
                ArithOp::Sub => QueryValue::Float(a - b),
                ArithOp::Mul => QueryValue::Float(a * b),
                ArithOp::Div => QueryValue::Float(a / b),
            }
        }
        (QueryValue::Float(a), QueryValue::Int(b)) => {
            let b = *b as f64;
            match op {
                ArithOp::Add => QueryValue::Float(a + b),
                ArithOp::Sub => QueryValue::Float(a - b),
                ArithOp::Mul => QueryValue::Float(a * b),
                ArithOp::Div => QueryValue::Float(a / b),
            }
        }
        // Null/Str/Bool/ObjRef operands: arithmetic is undefined.
        _ => QueryValue::Null,
    }
}

/// Apply a unary operator. `Pos` is identity; `Neg` negates numeric values with
/// wrapping semantics (`i64::MIN.wrapping_neg() == i64::MIN`). Non-numeric
/// values under `Neg` yield `QueryValue::Null`.
pub(crate) fn unary(op: UnaryOp, v: &QueryValue) -> QueryValue {
    match op {
        UnaryOp::Pos => v.clone(),
        UnaryOp::Neg => match v {
            QueryValue::Int(n) => QueryValue::Int(n.wrapping_neg()),
            QueryValue::Float(f) => QueryValue::Float(-f),
            _ => QueryValue::Null,
        },
    }
}

/// Evaluate a WHERE comparison of a projected LHS `QueryValue` against an
/// evaluated RHS `QueryValue`. For `LIKE`/`NOT LIKE`, `like_re` MUST be the
/// pre-compiled, anchored regex for the RHS pattern (compiled ONCE at
/// plan/executor-construction time via [`compile_like_regexes`] — NEVER on this
/// per-object hot path). LIKE is meaningful only for a string LHS; a non-string
/// LHS never matches, so `Like` is `false` and `NotLike` is `true` ("not like"
/// holds for anything that isn't a matching string). A missing `like_re` for a
/// LIKE op (a wiring bug — the actionable compile error is raised earlier at plan
/// time) is treated as no match rather than a panic.
pub(crate) fn compare_values(
    lv: &QueryValue,
    op: CompareOp,
    rv: &QueryValue,
    like_re: Option<&regex::Regex>,
) -> bool {
    if matches!(op, CompareOp::Like | CompareOp::NotLike) {
        let is_like = match (lv, rv) {
            (QueryValue::Str(s), QueryValue::Str(_)) => {
                like_re.map(|re| re.is_match(s)).unwrap_or(false)
            }
            // Non-string LHS (or non-string RHS): never "like".
            _ => false,
        };
        return if matches!(op, CompareOp::Like) {
            is_like
        } else {
            !is_like
        };
    }
    let ord = match (lv, rv) {
        (QueryValue::Int(a), QueryValue::Int(b)) => (*a).partial_cmp(b),
        (QueryValue::Int(a), QueryValue::Float(b)) => (*a as f64).partial_cmp(b),
        (QueryValue::Float(a), QueryValue::Int(b)) => a.partial_cmp(&(*b as f64)),
        (QueryValue::Float(a), QueryValue::Float(b)) => a.partial_cmp(b),
        (QueryValue::Str(a), QueryValue::Str(b)) => Some(a.as_str().cmp(b.as_str())),
        (QueryValue::Bool(a), QueryValue::Bool(b)) => Some(a.cmp(b)),
        (QueryValue::Null, QueryValue::Null) => Some(std::cmp::Ordering::Equal),
        _ => None,
    };
    match ord {
        None => matches!(op, CompareOp::Ne),
        Some(o) => match op {
            CompareOp::Eq => o.is_eq(),
            CompareOp::Ne => o.is_ne(),
            CompareOp::Lt => o.is_lt(),
            CompareOp::Le => o.is_le(),
            CompareOp::Gt => o.is_gt(),
            CompareOp::Ge => o.is_ge(),
            // Handled above; unreachable here.
            CompareOp::Like | CompareOp::NotLike => false,
        },
    }
}

/// Compile every `LIKE`/`NOT LIKE` RHS pattern in the query's WHERE predicate
/// ONCE, keyed by the raw pattern string, so the per-object scan hot path only
/// does a hash lookup + `Regex::is_match` (never `Regex::new`). Patterns are
/// anchored `^(?:<pat>)$` to get Java `Pattern.matches` FULL-match semantics
/// (mirroring [`compile_from_regex`]). A bad pattern is an ACTIONABLE error here,
/// surfaced at plan time (see `plan_single`), not a per-row panic or silent false.
///
/// Also walks CASE WHEN predicates in SELECT expressions, GROUP BY, and HAVING
/// so that LIKE patterns inside CASE conditions are also pre-compiled.
pub fn compile_like_regexes(
    query: &Query,
) -> Result<std::collections::HashMap<String, regex::Regex>, crate::query::QueryError> {
    let mut out = std::collections::HashMap::new();
    if let Some(pred) = &query.where_ {
        collect_like_regexes(pred, &mut out)?;
    }
    // Walk CASE WHEN predicates in SELECT items (LIKE inside CASE conditions).
    for item in &query.select {
        collect_like_in_select_item(item, &mut out)?;
    }
    // Walk GROUP BY expressions for CASE WHEN LIKE.
    for expr in &query.group_by {
        collect_like_in_expr(expr, &mut out)?;
    }
    // Walk HAVING predicate.
    if let Some(pred) = &query.having {
        collect_like_regexes(pred, &mut out)?;
    }
    Ok(out)
}

fn collect_like_in_select_item(
    item: &crate::query::ast::SelectItem,
    out: &mut std::collections::HashMap<String, regex::Regex>,
) -> Result<(), crate::query::QueryError> {
    use crate::query::ast::SelectItem;
    match item {
        SelectItem::Expr(e) => collect_like_in_expr(e, out)?,
        SelectItem::Aggregate { arg, .. } => collect_like_in_select_item(arg, out)?,
        _ => {}
    }
    Ok(())
}

fn collect_like_in_expr(
    e: &crate::query::ast::Expr,
    out: &mut std::collections::HashMap<String, regex::Regex>,
) -> Result<(), crate::query::QueryError> {
    use crate::query::ast::Expr;
    match e {
        Expr::Attr(_) | Expr::Lit(_) | Expr::Aggregate { .. } => {}
        Expr::Binary { lhs, rhs, .. } => {
            collect_like_in_expr(lhs, out)?;
            collect_like_in_expr(rhs, out)?;
        }
        Expr::Unary { arg, .. } => collect_like_in_expr(arg, out)?,
        Expr::Method { receiver, args, .. } => {
            collect_like_in_expr(receiver, out)?;
            for a in args {
                collect_like_in_expr(a, out)?;
            }
        }
        Expr::Case { branches, else_ } => {
            for (cond, then_e) in branches {
                collect_like_regexes(cond, out)?;
                collect_like_in_expr(then_e, out)?;
            }
            if let Some(e) = else_ {
                collect_like_in_expr(e, out)?;
            }
        }
        Expr::Coalesce(args) => {
            for arg in args {
                collect_like_in_expr(arg, out)?;
            }
        }
        Expr::NullIf { lhs, rhs } => {
            collect_like_in_expr(lhs, out)?;
            collect_like_in_expr(rhs, out)?;
        }
    }
    Ok(())
}

fn collect_like_regexes(
    pred: &crate::query::ast::Predicate,
    out: &mut std::collections::HashMap<String, regex::Regex>,
) -> Result<(), crate::query::QueryError> {
    use crate::query::ast::Predicate as P;
    match pred {
        P::And(a, b) | P::Or(a, b) => {
            collect_like_regexes(a, out)?;
            collect_like_regexes(b, out)?;
        }
        P::Not(a) => collect_like_regexes(a, out)?,
        P::Compare {
            op: CompareOp::Like | CompareOp::NotLike,
            rhs,
            ..
        } => {
            if let Some(Value::Str(pat)) = rhs.as_lit() {
                if !out.contains_key(pat) {
                    // Detect SQL-glob habit: `%` as wildcard, `_` as single-char.
                    // LIKE uses Java-style full-match regex, not SQL globs.
                    // Give an actionable hint before the regex compile attempt.
                    if pat.contains('%') && !pat.contains(".*") && !pat.starts_with('^') {
                        let suggested = pat.replace('%', ".*").replace('_', ".");
                        return Err(crate::query::QueryError(format!(
                            "LIKE \"{pat}\": looks like a SQL glob (`%` wildcard), but LIKE \
                             uses Java-style regex (full-match). \
                             Did you mean LIKE \"{suggested}\"?"
                        )));
                    }
                    let anchored = format!("^(?:{pat})$");
                    let re = regex::Regex::new(&anchored).map_err(|e| {
                        crate::query::QueryError(format!(
                            "invalid regex in LIKE \"{pat}\": {e} \
                             (the LIKE right-hand side is matched as a Java-style regex \
                             with whole-string semantics; fix the pattern)"
                        ))
                    })?;
                    out.insert(pat.clone(), re);
                }
            }
        }
        // Non-LIKE compares, InstanceOf, IN-subqueries, and EXISTS carry no LIKE pattern.
        P::Compare { .. } | P::InstanceOf(_) | P::InSubquery { .. } | P::Exists { .. } => {}
    }
    Ok(())
}

/// Find the output-column index that an ORDER BY key refers to, if any.
/// Matches the key's rendered name (e.g. `@usedHeapSize`, a field name, or an
/// alias) against the query's output column names. Returns None when the key is
/// not a projected column (the caller then keeps scan order + notes it).
pub(crate) fn order_by_column_index(
    q: &Query,
    columns: &[QueryColumn],
    key: &Attr,
) -> Option<usize> {
    let key_name = attr_name(key);
    let bare = strip_leading_alias(&key_name, q.alias.as_deref());
    columns.iter().position(|c| {
        c.name == key_name || strip_leading_alias(&c.name, q.alias.as_deref()) == bare
    })
}

/// Strip a leading `alias.` prefix from a rendered name so `s.name` and `name`
/// compare equal when `s` is the FROM alias.
fn strip_leading_alias<'n>(name: &'n str, alias: Option<&str>) -> &'n str {
    if let Some(a) = alias {
        if let Some(rest) = name.strip_prefix(a).and_then(|r| r.strip_prefix('.')) {
            return rest;
        }
    }
    name
}

/// Stable in-place sort of result rows by a single column index, honoring
/// direction. Uses a total ordering over `QueryValue` (Null sorts first in ASC).
pub(crate) fn sort_rows_by_column(
    rows: &mut [Vec<QueryValue>],
    idx: usize,
    dir: crate::query::ast::SortDir,
) {
    use crate::query::ast::SortDir;
    rows.sort_by(|a, b| {
        let av = a.get(idx).unwrap_or(&QueryValue::Null);
        let bv = b.get(idx).unwrap_or(&QueryValue::Null);
        let ord = total_cmp_query_value(av, bv);
        match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        }
    });
}

/// Sort `rows` by column `idx` (same ordering as [`sort_rows_by_column`]) while
/// keeping the optional `row_src` source-index sidecar aligned. When `src` is
/// `None` this defers to `sort_rows_by_column` and is byte-identical (the
/// non-reachable-only path, so no permutation allocation happens). When `Some`,
/// it sorts an index permutation by the same comparator and applies it to both
/// `rows` and `src` so element `i` of each stays paired.
pub(crate) fn sort_rows_with_src(
    rows: &mut Vec<Vec<QueryValue>>,
    src: &mut Option<Vec<u32>>,
    idx: usize,
    dir: crate::query::ast::SortDir,
) {
    use crate::query::ast::SortDir;
    let Some(src_vec) = src.as_mut() else {
        sort_rows_by_column(rows, idx, dir);
        return;
    };
    // Sort a permutation so the sidecar can be reordered identically. Same
    // comparator as `sort_rows_by_column`, so the row ordering is unchanged.
    let mut perm: Vec<usize> = (0..rows.len()).collect();
    perm.sort_by(|&a, &b| {
        let av = rows[a].get(idx).unwrap_or(&QueryValue::Null);
        let bv = rows[b].get(idx).unwrap_or(&QueryValue::Null);
        let ord = total_cmp_query_value(av, bv);
        match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        }
    });
    let new_rows: Vec<Vec<QueryValue>> =
        perm.iter().map(|&i| std::mem::take(&mut rows[i])).collect();
    let new_src: Vec<u32> = perm.iter().map(|&i| src_vec[i]).collect();
    *rows = new_rows;
    *src_vec = new_src;
}

/// A TOTAL ordering over `QueryValue` for sorting. Numeric values compare
/// numerically (Int/Float mixed via f64); strings lexicographically; bools
/// false<true; Null sorts before everything. Cross-type falls back to a stable
/// kind rank so the sort never panics on `f64` NaN or mixed columns.
fn total_cmp_query_value(a: &QueryValue, b: &QueryValue) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn num(v: &QueryValue) -> Option<f64> {
        match v {
            QueryValue::Int(i) => Some(*i as f64),
            QueryValue::Float(f) => Some(*f),
            _ => None,
        }
    }
    if let (Some(x), Some(y)) = (num(a), num(b)) {
        return x.partial_cmp(&y).unwrap_or_else(|| {
            // NaN handling: push NaN to the end deterministically.
            match (x.is_nan(), y.is_nan()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                _ => Ordering::Equal,
            }
        });
    }
    match (a, b) {
        (QueryValue::Str(x), QueryValue::Str(y)) => x.cmp(y),
        (QueryValue::Bool(x), QueryValue::Bool(y)) => x.cmp(y),
        (QueryValue::Null, QueryValue::Null) => Ordering::Equal,
        _ => kind_rank(a).cmp(&kind_rank(b)),
    }
}

fn kind_rank(v: &QueryValue) -> u8 {
    match v {
        QueryValue::Null => 0,
        QueryValue::Bool(_) => 1,
        QueryValue::Int(_) | QueryValue::Float(_) => 2,
        QueryValue::Str(_) => 3,
        QueryValue::ObjRef { .. } => 4,
    }
}

pub fn column_name(it: &SelectItem) -> String {
    match it {
        SelectItem::Star => "*".to_string(),
        SelectItem::Attr(a) => attr_name(a),
        SelectItem::Aggregate { func, arg } => {
            let f = format!("{func:?}").to_uppercase();
            format!("{f}({})", column_name(arg))
        }
        SelectItem::Path { from, to } => format!(
            "path({}, {})",
            path_operand_name(from),
            path_operand_name(to)
        ),
        SelectItem::ToString(a) => format!("toString({a})"),
        SelectItem::Expr(e) => expr_name(e),
    }
}

/// Build output columns for a query, applying per-item AS aliases where present.
pub(crate) fn query_columns(q: &Query) -> Vec<QueryColumn> {
    debug_assert_eq!(
        q.select.len(),
        q.select_aliases.len(),
        "select and select_aliases must stay parallel"
    );
    q.select
        .iter()
        .zip(
            q.select_aliases
                .iter()
                .map(Option::as_deref)
                .chain(std::iter::repeat(None)),
        )
        .map(|(it, alias)| QueryColumn {
            name: alias
                .map(|s| s.to_string())
                .unwrap_or_else(|| column_name(it)),
        })
        .collect()
}

/// Value equality for `x.equals(y)`. Object refs compare by identity (dense
/// index); scalars compare by value; mixed types are unequal. NOTE: this is
/// NOT Java `Object.equals()` — user-defined overrides are unreachable in a
/// static heap reader (no live JVM); this is reference-identity + primitive
/// value equality only.
fn qv_value_eq(a: &QueryValue, b: &QueryValue) -> bool {
    use QueryValue::*;
    match (a, b) {
        (Null, Null) => true,
        (Bool(x), Bool(y)) => x == y,
        (Int(x), Int(y)) => x == y,
        (Float(x), Float(y)) => x == y,
        (Str(x), Str(y)) => x == y,
        // Numeric cross-type: compare Int and Float by value so `i.equals(1)` and
        // `f.equals(1.0)` behave intuitively across the boxing boundary.
        (Int(x), Float(y)) | (Float(y), Int(x)) => (*x as f64) == *y,
        // Object refs compare by heap identity (dense index); the class label is
        // display metadata and does not affect identity.
        (ObjRef { index: i, .. }, ObjRef { index: j, .. }) => i == j,
        // Mixed / incomparable types are unequal (never panics).
        _ => false,
    }
}

fn is_boxed_integral(c: &str) -> bool {
    matches!(
        c,
        "java.lang.Integer" | "java.lang.Long" | "java.lang.Short" | "java.lang.Byte"
    )
}

fn is_boxed_fp(c: &str) -> bool {
    matches!(c, "java.lang.Float" | "java.lang.Double")
}

fn is_sized_collection(c: &str) -> bool {
    // Classes that store element count directly in a `size` int field
    // (either declared on the class or inherited through the super chain).
    // Classes that compute size via a backing structure (HashSet→map,
    // TreeSet→m, ArrayDeque→head/tail, ConcurrentHashMap→baseCount+cells)
    // are deliberately excluded — decode_field would read the wrong bytes.
    matches!(
        c,
        "java.util.ArrayList"
            | "java.util.Vector"
            | "java.util.LinkedList"
            | "java.util.HashMap"
            | "java.util.LinkedHashMap"
            | "java.util.WeakHashMap"
            | "java.util.TreeMap"
            | "java.util.IdentityHashMap"
    )
}

fn path_operand_name(p: &crate::query::ast::PathOperand) -> String {
    use crate::query::ast::PathOperand;
    match p {
        PathOperand::Alias(s) | PathOperand::Class(s) => s.clone(),
    }
}

/// Render an `Expr` as a readable default column name, e.g. `@usedHeapSize * 2`.
/// `Binary` children that are themselves `Binary` are parenthesized so the output
/// reads unambiguously. `Unary::Neg` renders as `-<operand>`.
pub fn expr_name(e: &Expr) -> String {
    match e {
        Expr::Attr(a) => attr_name(a),
        Expr::Lit(v) => match v {
            Value::Int(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Str(s) => format!("\"{s}\""),
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".to_string(),
        },
        Expr::Binary { op, lhs, rhs } => {
            let op_str = match op {
                ArithOp::Add => " + ",
                ArithOp::Sub => " - ",
                ArithOp::Mul => " * ",
                ArithOp::Div => " / ",
            };
            // Wrap Binary children in parens so nested expressions read correctly.
            let l = if matches!(lhs.as_ref(), Expr::Binary { .. }) {
                format!("({})", expr_name(lhs))
            } else {
                expr_name(lhs)
            };
            let r = if matches!(rhs.as_ref(), Expr::Binary { .. }) {
                format!("({})", expr_name(rhs))
            } else {
                expr_name(rhs)
            };
            format!("{l}{op_str}{r}")
        }
        Expr::Unary { op, arg } => match op {
            UnaryOp::Neg => format!("-{}", expr_name(arg)),
            UnaryOp::Pos => expr_name(arg),
        },
        Expr::Method { name, args, .. } => format!(
            "{name}({})",
            args.iter().map(expr_name).collect::<Vec<_>>().join(", ")
        ), // D2 fills this
        Expr::Aggregate { func, arg } => {
            let func_name = match func {
                AggFunc::Count => "COUNT",
                AggFunc::Sum => "SUM",
                AggFunc::Min => "MIN",
                AggFunc::Max => "MAX",
                AggFunc::Avg => "AVG",
                AggFunc::Percentile(p) => return format!("PERCENTILE(_, {p})"),
                AggFunc::Median => "MEDIAN",
            };
            let arg_str = match arg.as_ref() {
                SelectItem::Star => "*".to_string(),
                SelectItem::Attr(a) => attr_name(a),
                _ => unreachable!("Expr::Aggregate arg is always Star or Attr"),
            };
            format!("{func_name}({arg_str})")
        }
        Expr::Case { .. } => "CASE".to_string(),
        Expr::Coalesce(_) => "COALESCE".to_string(),
        Expr::NullIf { .. } => "NULLIF".to_string(),
    }
}

pub(crate) fn attr_name(a: &Attr) -> String {
    match a {
        Attr::ObjectId => "@objectId".into(),
        Attr::ObjectAddress => "@objectAddress".into(),
        Attr::UsedHeapSize => "@usedHeapSize".into(),
        Attr::RetainedHeapSize => "@retainedHeapSize".into(),
        Attr::DisplayName => "@displayName".into(),
        Attr::Length => "@length".into(),
        Attr::Inbounds => "@inbounds".into(),
        Attr::Outbounds => "@outbounds".into(),
        Attr::ClassOf => "classof".into(),
        Attr::Dominators(a) => format!("dominators({a})"),
        Attr::DominatorOf(a) => format!("dominatorof({a})"),
        Attr::ToString(a) => format!("toString({a})"),
        Attr::ToHex(inner) => format!("toHex({})", expr_name(inner)),
        Attr::Field(f) => f.clone(),
        Attr::RefPath { hops, tail, .. } => {
            let mut s = hops.join(".");
            s.push('.');
            s.push_str(&attr_name(tail));
            s
        }
        // D4b: late resolution — ref-hop attrs.
        Attr::ValueArray => "@valueArray".into(),
        Attr::ReferenceArray => "@referenceArray".into(),
        // G1: GC-root attrs.
        Attr::GcRoots => "@GCRoots".into(),
        Attr::GcRootInfo => "@GCRootInfo".into(),
        // Array index/slice postfix notation.
        Attr::ArrayIndex { base, .. } => format!("{}[...]", attr_name(base)),
        Attr::ArraySlice { base, .. } => format!("{}[...]", attr_name(base)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::model::QueryValue;
    use crate::query::parse::parse;

    fn schema(pairs: &[(u64, &str)]) -> TestSchema {
        TestSchema {
            names: pairs.iter().map(|(id, n)| (*id, n.to_string())).collect(),
        }
    }

    fn pq(q: &crate::query::ast::Query) -> crate::query::plan::QueryPlan {
        crate::query::plan::plan_query(q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap()
    }

    #[test]
    fn exec_state_separates_finished_and_pending() {
        use crate::query::plan::Phase;
        let mut st = QueryExecState::new();
        st.push_finished(
            0,
            QueryResult {
                name: "q1".into(),
                oql: String::new(),
                columns: vec![],
                rows: vec![],
                row_count: 0,
                truncated: false,
                error: None,
                note: None,
                viz: None,
                elapsed_ms: None,
            },
        );
        let q = crate::query::parse::parse("SELECT @retainedHeapSize FROM C").unwrap();
        let plan = pq(&q);
        assert_eq!(plan.finalize_at, Phase::P3);
        let mut carry = crate::query::carry::Carry::index_only(100);
        carry.push_index(42);
        st.push_cross_phase(1, "q2".to_string(), plan.clone(), carry);
        assert_eq!(st.finished_len(), 1);
        assert_eq!(st.pending_len(), 1);
        assert_eq!(st.pending()[0].slot, 1);
        assert_eq!(st.pending()[0].carry.indices(), vec![42]);
    }

    #[test]
    fn carry_mode_carries_matched_indices_not_rows() {
        // A cross-phase query: carry mode collects matched dense indices instead
        // of building rows. `finish` is not used; `take_carry` extracts them.
        let q = parse("SELECT @objectId, @retainedHeapSize FROM com.acme.Foo").unwrap();
        let plan = pq(&q);
        assert!(plan.finalize_at == crate::query::plan::Phase::P3);
        let sc = schema(&[(10, "com.acme.Foo"), (20, "com.acme.Bar")]);
        let carry = crate::query::carry::Carry::index_only(100);
        let mut ex = SingleScanExecutor::new_carry(&q, &plan, &sc, carry);
        assert!(ex.is_carry());
        ex.visit_instance(3, 10, &[]); // Foo → carried
        ex.visit_instance(4, 20, &[]); // Bar → skipped (class mismatch)
        ex.visit_instance(7, 10, &[]); // Foo → carried
        let carry = ex.take_carry();
        assert_eq!(carry.indices(), vec![3, 7]);
    }

    #[test]
    fn carry_mode_skips_retained_where_terms() {
        // `WHERE @retainedHeapSize > 1000` cannot be evaluated during the scan
        // (retained size is unknown). Carry mode must NOT drop rows on it — all
        // class matches are carried; stage_runner applies the retained filter.
        let q = parse("SELECT @objectId FROM com.acme.Foo WHERE @retainedHeapSize > 1000").unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo")]);
        let carry = crate::query::carry::Carry::index_only(100);
        let mut ex = SingleScanExecutor::new_carry(&q, &plan, &sc, carry);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        let carry = ex.take_carry();
        assert_eq!(
            carry.indices(),
            vec![1, 2],
            "retained WHERE must not filter during scan"
        );
    }

    #[test]
    fn carry_mode_ignores_limit_during_scan() {
        // LIMIT applies AFTER the retained ORDER BY (in stage_runner), so carry
        // mode must carry every match regardless of the query's LIMIT.
        let q = parse("SELECT @objectId FROM com.acme.Foo ORDER BY @retainedHeapSize DESC LIMIT 1")
            .unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo")]);
        let carry = crate::query::carry::Carry::index_only(100);
        let mut ex = SingleScanExecutor::new_carry(&q, &plan, &sc, carry);
        for i in 1..=5u32 {
            ex.visit_instance(i as usize, 10, &[]);
        }
        let carry = ex.take_carry();
        assert_eq!(
            carry.indices(),
            vec![1, 2, 3, 4, 5],
            "LIMIT must be deferred to the late phase"
        );
    }

    #[test]
    fn carry_mode_still_applies_non_retained_where() {
        // A non-retained WHERE term (on a class match) still filters during the
        // scan; only retained terms are deferred. Here the class filter alone
        // decides membership (no field blob), so a Bar instance is excluded.
        let q = parse("SELECT @objectId FROM com.acme.Foo WHERE @retainedHeapSize > 0").unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo"), (20, "com.acme.Bar")]);
        let carry = crate::query::carry::Carry::index_only(100);
        let mut ex = SingleScanExecutor::new_carry(&q, &plan, &sc, carry);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 20, &[]); // wrong class → not carried
        let carry = ex.take_carry();
        assert_eq!(carry.indices(), vec![1]);
    }

    #[test]
    fn matches_exact_class_and_projects_object_id() {
        let q = parse("SELECT @objectId FROM com.acme.Foo").unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo"), (20, "com.acme.Bar")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(3, 10, &[]);
        ex.visit_instance(4, 20, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Int(3));
    }

    #[test]
    fn respects_limit() {
        let q = parse("SELECT @objectId FROM com.acme.Foo LIMIT 1").unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(3, 10, &[]);
        ex.visit_instance(4, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert!(res.truncated);
    }

    // --- Additional edge-case tests (beyond the plan's two) ---

    // --- MAT gap #5: quoted/regex FROM matcher unit tests ---

    fn regex_spec(src: &str) -> crate::query::ast::ClassSpec {
        crate::query::ast::ClassSpec {
            instanceof: false,
            class_name: src.into(),
            is_regex: true,
        }
    }
    fn glob_spec(src: &str) -> crate::query::ast::ClassSpec {
        crate::query::ast::ClassSpec {
            instanceof: false,
            class_name: src.into(),
            is_regex: false,
        }
    }

    #[test]
    fn matches_spec_regex_full_anchored_match() {
        let spec = regex_spec(r"java\.lang\..*");
        let re = compile_from_regex(&spec).unwrap();
        assert!(class_name_matches_spec(
            "java.lang.String",
            &spec,
            re.as_ref()
        ));
        // Full/anchored: a bare `lang` regex must NOT match the full name.
        let lang = regex_spec("lang");
        let lang_re = compile_from_regex(&lang).unwrap();
        assert!(
            !class_name_matches_spec("java.lang.String", &lang, lang_re.as_ref()),
            "regex must match the WHOLE class name (Pattern.matches), not a substring"
        );
    }

    #[test]
    fn matches_spec_regex_trailing_string() {
        let spec = regex_spec(".*String");
        let re = compile_from_regex(&spec).unwrap();
        assert!(class_name_matches_spec(
            "java.lang.String",
            &spec,
            re.as_ref()
        ));
        assert!(!class_name_matches_spec(
            "java.lang.Integer",
            &spec,
            re.as_ref()
        ));
    }

    #[test]
    fn matches_spec_regex_alternation() {
        let spec = regex_spec("java\\.lang\\.String|java\\.util\\.HashMap");
        let re = compile_from_regex(&spec).unwrap();
        assert!(class_name_matches_spec(
            "java.lang.String",
            &spec,
            re.as_ref()
        ));
        assert!(class_name_matches_spec(
            "java.util.HashMap",
            &spec,
            re.as_ref()
        ));
        assert!(!class_name_matches_spec(
            "java.lang.Integer",
            &spec,
            re.as_ref()
        ));
    }

    #[test]
    fn matches_spec_regex_dot_is_regex_metachar() {
        // In regex mode a `.` is any-char (regex semantics, NOT glob).
        let spec = regex_spec("java.lang.String");
        let re = compile_from_regex(&spec).unwrap();
        assert!(class_name_matches_spec(
            "java.lang.String",
            &spec,
            re.as_ref()
        ));
        // `.` matches any single char, so `javaXlangXString` matches too.
        assert!(class_name_matches_spec(
            "javaXlangXString",
            &spec,
            re.as_ref()
        ));
    }

    #[test]
    fn matches_spec_glob_falls_through_to_class_name_matches() {
        // A non-regex spec ignores from_regex and uses the exact/glob matcher.
        let spec = glob_spec("com.acme.*");
        assert!(compile_from_regex(&spec).unwrap().is_none());
        assert!(class_name_matches_spec("com.acme.Foo", &spec, None));
        assert!(!class_name_matches_spec("org.other.Foo", &spec, None));
    }

    #[test]
    fn compile_from_regex_bad_pattern_is_actionable_error() {
        let spec = regex_spec("[");
        let err = compile_from_regex(&spec).expect_err("unclosed class must be an error");
        assert!(
            err.0.contains("invalid regex") && err.0.contains('['),
            "error must name the regex problem; got: {}",
            err.0
        );
    }

    #[test]
    fn matches_spec_regex_matches_nothing_returns_false() {
        let spec = regex_spec("no\\.such\\.Class");
        let re = compile_from_regex(&spec).unwrap();
        assert!(!class_name_matches_spec(
            "java.lang.String",
            &spec,
            re.as_ref()
        ));
    }

    #[test]
    fn matches_spec_regex_none_when_uncompiled_is_false() {
        // Defensive: a regex spec with no compiled regex must be a no-match,
        // never a panic.
        let spec = regex_spec("java\\.lang\\..*");
        assert!(!class_name_matches_spec("java.lang.String", &spec, None));
    }

    #[test]
    fn class_name_matches_exact() {
        assert!(class_name_matches("com.acme.Foo", "com.acme.Foo"));
        assert!(!class_name_matches("com.acme.Foo", "com.acme.Bar"));
        assert!(!class_name_matches("com.acme.Foo", "com.acme"));
    }

    #[test]
    fn class_name_matches_glob() {
        // "com.acme.*" matches the package prefix itself and nested classes,
        // but must NOT match a differently-named sibling package.
        assert!(class_name_matches("com.acme.Foo", "com.acme.*"));
        assert!(class_name_matches("com.acme.sub.Bar", "com.acme.*"));
        assert!(class_name_matches("com.acme", "com.acme.*"));
        assert!(!class_name_matches("com.acmeX.Foo", "com.acme.*"));
        assert!(!class_name_matches("org.other.Foo", "com.acme.*"));
    }

    #[test]
    fn class_name_matches_separator_normalization() {
        // Stored JVM-internal slash names must match dotted patterns and vice-versa.
        assert!(class_name_matches("com/acme/Foo", "com.acme.Foo"));
        assert!(class_name_matches("com.acme.Foo", "com/acme/Foo"));
        assert!(class_name_matches("com/acme/Foo", "com.acme.*"));
        assert!(class_name_matches("com/acme/Foo", "com/acme/*"));
    }

    #[test]
    fn class_name_matches_arrays_and_edges() {
        // Array class names (dotted, with the `[]` suffix the resolver produces)
        // match exactly and are not spuriously matched by unrelated patterns.
        assert!(class_name_matches("char[]", "char[]"));
        assert!(class_name_matches(
            "java.lang.Object[]",
            "java.lang.Object[]"
        ));
        assert!(!class_name_matches("char[]", "byte[]"));
        // A shorter name than the wildcard prefix must not match (guards the
        // `get(..prefix.len())` slice returning None → false).
        assert!(!class_name_matches("com", "com.acme.*"));
        // Exact match of unequal lengths is rejected without allocation.
        assert!(!class_name_matches("com.acme.Foo", "com.acme.Foobar"));
    }

    #[test]
    fn column_name_for_star() {
        assert_eq!(column_name(&SelectItem::Star), "*");
    }

    #[test]
    fn column_name_for_at_attr() {
        assert_eq!(column_name(&SelectItem::Attr(Attr::ObjectId)), "@objectId");
        assert_eq!(
            column_name(&SelectItem::Attr(Attr::UsedHeapSize)),
            "@usedHeapSize"
        );
    }

    #[test]
    fn column_name_for_field() {
        assert_eq!(
            column_name(&SelectItem::Attr(Attr::Field("count".into()))),
            "count"
        );
    }

    #[test]
    fn column_name_for_aggregates() {
        let count_star = SelectItem::Aggregate {
            func: crate::query::ast::AggFunc::Count,
            arg: Box::new(SelectItem::Star),
        };
        assert_eq!(column_name(&count_star), "COUNT(*)");

        let sum_heap = SelectItem::Aggregate {
            func: crate::query::ast::AggFunc::Sum,
            arg: Box::new(SelectItem::Attr(Attr::UsedHeapSize)),
        };
        assert_eq!(column_name(&sum_heap), "SUM(@usedHeapSize)");
    }

    #[test]
    fn projects_star_as_objref() {
        let q = parse("SELECT * FROM com.acme.Foo").unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(7, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(
            res.rows[0][0],
            QueryValue::ObjRef {
                index: 7,
                class: "com.acme.Foo".into(),
                addr: None,
            }
        );
    }

    #[test]
    fn no_match_yields_empty_untruncated() {
        let q = parse("SELECT @objectId FROM com.acme.Missing").unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo"), (20, "com.acme.Bar")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(3, 10, &[]);
        ex.visit_instance(4, 20, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 0);
        assert!(!res.truncated);
    }

    #[test]
    fn glob_from_matches_multiple_classes() {
        let q = parse("SELECT @objectId FROM com.acme.*").unwrap();
        let plan = pq(&q);
        let sc = schema(&[
            (10, "com.acme.Foo"),
            (20, "com.acme.Bar"),
            (30, "org.other.Baz"),
        ]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 20, &[]);
        ex.visit_instance(3, 30, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 2);
        assert_eq!(res.rows[0][0], QueryValue::Int(1));
        assert_eq!(res.rows[1][0], QueryValue::Int(2));
        assert!(!res.truncated);
    }

    #[test]
    fn unknown_class_id_never_matches() {
        // A class_id absent from the resolver must not match any FROM.
        let q = parse("SELECT @objectId FROM com.acme.Foo").unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(3, 999, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 0);
    }

    // --- Task 9b: field decode + WHERE evaluation ---

    /// A fake resolver whose `field` lookup maps several field names to fixed
    /// (offset, type) pairs, so decode of each width/type can be exercised.
    struct FieldSchema {
        names: std::collections::HashMap<u64, String>,
        fields: std::collections::HashMap<String, (u32, crate::types::HprofType)>,
    }

    impl FieldSchema {
        /// The plan's canonical schema: class 10 = "C", field "count" @0:Int.
        fn count_only() -> Self {
            FieldSchema {
                names: std::iter::once((10u64, "C".to_string())).collect(),
                fields: std::iter::once((
                    "count".to_string(),
                    (0u32, crate::types::HprofType::Int),
                ))
                .collect(),
            }
        }
        fn with_fields(pairs: &[(&str, u32, crate::types::HprofType)]) -> Self {
            FieldSchema {
                names: std::iter::once((10u64, "C".to_string())).collect(),
                fields: pairs
                    .iter()
                    .map(|(n, o, t)| (n.to_string(), (*o, *t)))
                    .collect(),
            }
        }
    }

    impl ClassResolver for FieldSchema {
        fn class_name(&self, class_id: u64) -> Option<&str> {
            self.names.get(&class_id).map(|s| s.as_str())
        }
        fn field(&self, _class_id: u64, name: &str) -> Option<(u32, crate::types::HprofType)> {
            self.fields.get(name).copied()
        }
    }

    #[test]
    fn where_filters_on_scalar_field() {
        let q = crate::query::parse::parse("SELECT @objectId FROM C WHERE count > 5").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 3]); // count=3 fails >5
        ex.visit_instance(2, 10, &[0, 0, 0, 9]); // count=9 passes
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], crate::query::model::QueryValue::Int(2));
    }

    #[test]
    fn projects_scalar_field_value() {
        // NB: use field name "n" (not "count") for the projection: a bare
        // `SELECT count` collides with the COUNT aggregate keyword in the parser,
        // which is out of scope for this task. WHERE position parses `count`
        // fine (see where_filters_on_scalar_field).
        let q = crate::query::parse::parse("SELECT n FROM C").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::with_fields(&[("n", 0, crate::types::HprofType::Int)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 7]);
        let res = ex.finish("q1");
        assert_eq!(res.rows[0][0], crate::query::model::QueryValue::Int(7));
    }

    // --- read_be edge cases ---

    #[test]
    fn read_be_assembles_big_endian_widths() {
        assert_eq!(read_be(&[0x12, 0x34], 0, 2), Some(0x1234));
        assert_eq!(read_be(&[0xde, 0xad, 0xbe, 0xef], 0, 4), Some(0xdead_beef));
        assert_eq!(
            read_be(&[1, 2, 3, 4, 5, 6, 7, 8], 0, 8),
            Some(0x0102_0304_0506_0708)
        );
        // Reads honor the offset.
        assert_eq!(read_be(&[0xff, 0x12, 0x34], 1, 2), Some(0x1234));
    }

    #[test]
    fn read_be_out_of_range_is_none() {
        assert_eq!(read_be(&[0x12], 0, 2), None); // one byte short
        assert_eq!(read_be(&[], 0, 4), None);
        assert_eq!(read_be(&[1, 2, 3, 4], 2, 4), None); // off past tail
    }

    // --- per-type decode ---

    fn decode(field: &str, off: u32, ty: crate::types::HprofType, blob: &[u8]) -> QueryValue {
        let q = crate::query::parse::parse(&format!("SELECT {field} FROM C")).unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::with_fields(&[(field, off, ty)]);
        let ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.decode_field(10, field, blob)
    }

    #[test]
    fn decode_byte() {
        assert_eq!(
            decode("b", 0, crate::types::HprofType::Byte, &[0x2a]),
            QueryValue::Int(42)
        );
    }

    #[test]
    fn decode_short_sign_extends_negative() {
        // 0xFFFE as i16 == -2, must sign-extend to i64 -2 (not 65534).
        assert_eq!(
            decode("s", 0, crate::types::HprofType::Short, &[0xff, 0xfe]),
            QueryValue::Int(-2)
        );
    }

    #[test]
    fn decode_char_is_unsigned() {
        // Char is unsigned: 0xFFFF -> 65535, not -1.
        assert_eq!(
            decode("c", 0, crate::types::HprofType::Char, &[0xff, 0xff]),
            QueryValue::Int(65535)
        );
    }

    #[test]
    fn decode_int_sign_extends_negative() {
        // 0xFFFFFFFF as i32 == -1.
        assert_eq!(
            decode(
                "i",
                0,
                crate::types::HprofType::Int,
                &[0xff, 0xff, 0xff, 0xff]
            ),
            QueryValue::Int(-1)
        );
    }

    #[test]
    fn decode_long() {
        assert_eq!(
            decode(
                "l",
                0,
                crate::types::HprofType::Long,
                &[0, 0, 0, 0, 0, 0, 0x04, 0xd2]
            ),
            QueryValue::Int(1234)
        );
    }

    #[test]
    fn decode_float() {
        // 1.5f32 == bits 0x3FC00000.
        assert_eq!(
            decode(
                "f",
                0,
                crate::types::HprofType::Float,
                &[0x3f, 0xc0, 0x00, 0x00]
            ),
            QueryValue::Float(1.5)
        );
    }

    #[test]
    fn decode_double() {
        // 1.5f64 == bits 0x3FF8000000000000.
        assert_eq!(
            decode(
                "d",
                0,
                crate::types::HprofType::Double,
                &[0x3f, 0xf8, 0, 0, 0, 0, 0, 0]
            ),
            QueryValue::Float(1.5)
        );
    }

    #[test]
    fn decode_boolean_false_and_true() {
        assert_eq!(
            decode("bo", 0, crate::types::HprofType::Boolean, &[0]),
            QueryValue::Bool(false)
        );
        assert_eq!(
            decode("bo", 0, crate::types::HprofType::Boolean, &[1]),
            QueryValue::Bool(true)
        );
        assert_eq!(
            decode("bo", 0, crate::types::HprofType::Boolean, &[0x7f]),
            QueryValue::Bool(true)
        );
    }

    #[test]
    fn decode_object_field_is_null() {
        assert_eq!(
            decode(
                "o",
                0,
                crate::types::HprofType::Object,
                &[0, 0, 0, 0, 0, 0, 0, 1]
            ),
            QueryValue::Null
        );
    }

    #[test]
    fn decode_unknown_field_is_null() {
        // Resolver has no mapping for "missing" -> Null.
        let q = crate::query::parse::parse("SELECT missing FROM C").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let ex = SingleScanExecutor::new(&q, &plan, &sc);
        assert_eq!(
            ex.decode_field(10, "missing", &[0, 0, 0, 1]),
            QueryValue::Null
        );
    }

    #[test]
    fn decode_blob_too_short_is_null_no_panic() {
        // Int field @offset 4 but blob only 2 bytes: out of range -> Null.
        assert_eq!(
            decode("i", 4, crate::types::HprofType::Int, &[0, 1]),
            QueryValue::Null
        );
    }

    #[test]
    fn decode_honors_nonzero_offset() {
        // Int @offset 2 within an 8-byte blob.
        assert_eq!(
            decode(
                "i",
                2,
                crate::types::HprofType::Int,
                &[0xaa, 0xbb, 0, 0, 0, 0x05, 0xcc, 0xdd]
            ),
            QueryValue::Int(5)
        );
    }

    // --- compare_values ---

    #[test]
    fn compare_type_mismatch_eq_false_ne_true() {
        let lv = QueryValue::Int(1);
        let rhs = QueryValue::Str("x".into());
        assert!(!compare_values(
            &lv,
            crate::query::ast::CompareOp::Eq,
            &rhs,
            None
        ));
        assert!(compare_values(
            &lv,
            crate::query::ast::CompareOp::Ne,
            &rhs,
            None
        ));
        // Other ops on a mismatch are false.
        assert!(!compare_values(
            &lv,
            crate::query::ast::CompareOp::Lt,
            &rhs,
            None
        ));
        assert!(!compare_values(
            &lv,
            crate::query::ast::CompareOp::Gt,
            &rhs,
            None
        ));
    }

    #[test]
    fn compare_null_equals_null() {
        use crate::query::ast::CompareOp;
        assert!(compare_values(
            &QueryValue::Null,
            CompareOp::Eq,
            &QueryValue::Null,
            None
        ));
        assert!(!compare_values(
            &QueryValue::Null,
            CompareOp::Ne,
            &QueryValue::Null,
            None
        ));
    }

    #[test]
    fn compare_int_vs_float_cross() {
        use crate::query::ast::CompareOp;
        assert!(compare_values(
            &QueryValue::Int(2),
            CompareOp::Lt,
            &QueryValue::Float(2.5),
            None
        ));
        assert!(compare_values(
            &QueryValue::Float(2.5),
            CompareOp::Gt,
            &QueryValue::Int(2),
            None
        ));
        assert!(compare_values(
            &QueryValue::Int(3),
            CompareOp::Eq,
            &QueryValue::Float(3.0),
            None
        ));
    }

    #[test]
    fn compare_string_ordering() {
        use crate::query::ast::CompareOp;
        assert!(compare_values(
            &QueryValue::Str("abc".into()),
            CompareOp::Lt,
            &QueryValue::Str("abd".into()),
            None
        ));
        assert!(compare_values(
            &QueryValue::Str("abc".into()),
            CompareOp::Eq,
            &QueryValue::Str("abc".into()),
            None
        ));
    }

    #[test]
    fn compare_bool_ordering() {
        use crate::query::ast::CompareOp;
        assert!(compare_values(
            &QueryValue::Bool(false),
            CompareOp::Lt,
            &QueryValue::Bool(true),
            None
        ));
        assert!(compare_values(
            &QueryValue::Bool(true),
            CompareOp::Eq,
            &QueryValue::Bool(true),
            None
        ));
    }

    // --- LIKE / NOT LIKE (compare_values) ---
    // The compiled regex passed in is the anchored `^(?:<pat>)$` form the
    // executor/plan build once via `compile_like_regexes`; here we build it
    // inline to exercise `compare_values` in isolation.

    fn like_re(pat: &str) -> regex::Regex {
        regex::Regex::new(&format!("^(?:{pat})$")).unwrap()
    }

    #[test]
    fn like_full_match_true() {
        use crate::query::ast::CompareOp;
        let re = like_re("m.*");
        assert!(compare_values(
            &QueryValue::Str("main".into()),
            CompareOp::Like,
            &QueryValue::Str("m.*".into()),
            Some(&re)
        ));
    }

    #[test]
    fn like_non_match_false() {
        use crate::query::ast::CompareOp;
        let re = like_re("m.*");
        assert!(!compare_values(
            &QueryValue::Str("worker".into()),
            CompareOp::Like,
            &QueryValue::Str("m.*".into()),
            Some(&re)
        ));
    }

    #[test]
    fn like_is_anchored_full_match_not_substring() {
        use crate::query::ast::CompareOp;
        // "submaine" contains "m.*" as a substring but is NOT a full match of
        // `^(?:m.*)$`, so LIKE must be false (anchoring proof).
        let re = like_re("m.*");
        assert!(!compare_values(
            &QueryValue::Str("submaine".into()),
            CompareOp::Like,
            &QueryValue::Str("m.*".into()),
            Some(&re)
        ));
    }

    #[test]
    fn not_like_inverts_like() {
        use crate::query::ast::CompareOp;
        let re = like_re("m.*");
        // "main" matches → NOT LIKE is false.
        assert!(!compare_values(
            &QueryValue::Str("main".into()),
            CompareOp::NotLike,
            &QueryValue::Str("m.*".into()),
            Some(&re)
        ));
        // "worker" doesn't match → NOT LIKE is true.
        assert!(compare_values(
            &QueryValue::Str("worker".into()),
            CompareOp::NotLike,
            &QueryValue::Str("m.*".into()),
            Some(&re)
        ));
    }

    #[test]
    fn like_alternation_anchored() {
        use crate::query::ast::CompareOp;
        let re = like_re("foo|bar");
        // Both alternatives match fully.
        assert!(compare_values(
            &QueryValue::Str("foo".into()),
            CompareOp::Like,
            &QueryValue::Str("foo|bar".into()),
            Some(&re)
        ));
        assert!(compare_values(
            &QueryValue::Str("bar".into()),
            CompareOp::Like,
            &QueryValue::Str("foo|bar".into()),
            Some(&re)
        ));
        // "xfooy" is NOT a full match (anchoring wraps the whole alternation).
        assert!(!compare_values(
            &QueryValue::Str("xfooy".into()),
            CompareOp::Like,
            &QueryValue::Str("foo|bar".into()),
            Some(&re)
        ));
    }

    #[test]
    fn like_on_numeric_lhs_false_not_like_true() {
        use crate::query::ast::CompareOp;
        // A non-string LHS never matches a regex: LIKE is false, NOT LIKE true.
        let re = like_re("m.*");
        assert!(!compare_values(
            &QueryValue::Int(42),
            CompareOp::Like,
            &QueryValue::Str("m.*".into()),
            Some(&re)
        ));
        assert!(compare_values(
            &QueryValue::Int(42),
            CompareOp::NotLike,
            &QueryValue::Str("m.*".into()),
            Some(&re)
        ));
    }

    #[test]
    fn like_missing_compiled_regex_is_no_match() {
        use crate::query::ast::CompareOp;
        // A wiring bug (no compiled regex) is treated as no-match, never a panic.
        assert!(!compare_values(
            &QueryValue::Str("main".into()),
            CompareOp::Like,
            &QueryValue::Str("m.*".into()),
            None
        ));
    }

    #[test]
    fn compile_like_regexes_bad_pattern_is_actionable_error() {
        let q = crate::query::parse::parse(r#"SELECT * FROM C WHERE name LIKE "[""#).unwrap();
        let err = compile_like_regexes(&q).expect_err("unclosed class must be an error");
        let msg = err.to_string();
        assert!(msg.contains("invalid regex in LIKE"), "got: {msg}");
        assert!(msg.contains("Java-style regex"), "got: {msg}");
    }

    #[test]
    fn compile_like_regexes_collects_each_pattern_once() {
        let q = crate::query::parse::parse(
            r#"SELECT * FROM C WHERE name LIKE "a.*" AND other NOT LIKE "a.*""#,
        )
        .unwrap();
        let map = compile_like_regexes(&q).unwrap();
        // Two LIKE terms share one RHS pattern → compiled exactly once.
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("a.*"));
    }

    // --- WHERE combinators ---

    #[test]
    fn where_and_filters_both_bounds() {
        let q =
            crate::query::parse::parse("SELECT @objectId FROM C WHERE count > 5 AND count < 100")
                .unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 3]); // 3: fails >5
        ex.visit_instance(2, 10, &[0, 0, 0, 50]); // 50: passes both
        ex.visit_instance(3, 10, &[0, 0, 0, 200]); // 200: fails <100
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Int(2));
    }

    #[test]
    fn where_or_matches_either() {
        let q =
            crate::query::parse::parse("SELECT @objectId FROM C WHERE count < 5 OR count > 100")
                .unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 3]); // <5: passes
        ex.visit_instance(2, 10, &[0, 0, 0, 50]); // neither: fails
        ex.visit_instance(3, 10, &[0, 0, 0, 200]); // >100: passes
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 2);
        assert_eq!(res.rows[0][0], QueryValue::Int(1));
        assert_eq!(res.rows[1][0], QueryValue::Int(3));
    }

    #[test]
    fn where_not_negates() {
        let q = crate::query::parse::parse("SELECT @objectId FROM C WHERE NOT count = 7").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 7]); // ==7: excluded
        ex.visit_instance(2, 10, &[0, 0, 0, 8]); // !=7: included
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Int(2));
    }

    #[test]
    fn where_instanceof_matches_by_class_name() {
        let q = crate::query::parse::parse("SELECT @objectId FROM C WHERE x INSTANCEOF C").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 1]); // class 10 == "C" -> matches
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Int(1));
    }

    #[test]
    fn where_instanceof_excludes_other_class() {
        let q = crate::query::parse::parse("SELECT @objectId FROM C WHERE x INSTANCEOF D").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 1]); // class "C" is not "D"
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 0);
    }

    #[test]
    fn where_unknown_field_excludes_for_eq() {
        // Resolver has no "missing" -> decode Null; Null = 1 is a mismatch -> excluded.
        let q = crate::query::parse::parse("SELECT @objectId FROM C WHERE missing = 1").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 1]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 0);
    }

    // --- C2: alias-prefix stripping ---

    #[test]
    fn alias_prefixed_field_projects_same_as_bare() {
        // `FROM C c` binds alias `c`; `SELECT c.n` must resolve field `n`.
        let q = crate::query::parse::parse("SELECT c.n FROM C c").unwrap();
        assert_eq!(q.alias.as_deref(), Some("c"), "parser must bind alias `c`");
        let plan = pq(&q);
        let sc = FieldSchema::with_fields(&[("n", 0, crate::types::HprofType::Int)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 7]);
        let res = ex.finish("q1");
        assert_eq!(res.rows[0][0], QueryValue::Int(7));
    }

    #[test]
    fn alias_prefixed_field_filters_in_where() {
        // `WHERE c.count > 5` must strip the `c.` prefix and resolve `count`.
        let q = crate::query::parse::parse("SELECT @objectId FROM C c WHERE c.count > 5").unwrap();
        assert_eq!(q.alias.as_deref(), Some("c"));
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 3]); // count=3 fails >5
        ex.visit_instance(2, 10, &[0, 0, 0, 9]); // count=9 passes
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Int(2));
    }

    #[test]
    fn strip_alias_only_strips_matching_prefix() {
        // Directly exercise strip_alias: matching prefix stripped, others intact.
        let q = crate::query::parse::parse("SELECT n FROM C c").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let ex = SingleScanExecutor::new(&q, &plan, &sc);
        assert_eq!(ex.strip_alias("c.count"), "count");
        // Different leading identifier is left alone (not this query's alias).
        assert_eq!(ex.strip_alias("d.count"), "d.count");
        // A field whose name merely starts with the alias letters but no dot.
        assert_eq!(ex.strip_alias("count"), "count");
        // Nested dotted path only strips the first `<alias>.` segment.
        assert_eq!(ex.strip_alias("c.a.b"), "a.b");
    }

    #[test]
    fn strip_alias_noop_without_alias() {
        // No alias bound: field names pass through untouched.
        let q = crate::query::parse::parse("SELECT n FROM C").unwrap();
        assert!(q.alias.is_none());
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let ex = SingleScanExecutor::new(&q, &plan, &sc);
        assert_eq!(ex.strip_alias("c.count"), "c.count");
    }

    // --- Array path (@length projection + array WHERE) ---

    #[test]
    fn array_length_projects_real_count() {
        // `FROM char[]` matches the array class NAME passed to visit_array; the
        // `@length` column must project the element count as an Int.
        let q = crate::query::parse::parse("SELECT @length FROM char[]").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_array(1, "char[]", 42);
        ex.visit_array(2, "char[]", 7);
        // A non-matching array class is ignored.
        ex.visit_array(3, "int[]", 99);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 2);
        assert_eq!(res.rows[0][0], QueryValue::Int(42));
        assert_eq!(res.rows[1][0], QueryValue::Int(7));
    }

    #[test]
    fn array_length_filters_in_where() {
        // WHERE over @length filters array rows just like scalar fields.
        let q = crate::query::parse::parse("SELECT @length FROM char[] WHERE @length > 8").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_array(1, "char[]", 4); // fails >8
        ex.visit_array(2, "char[]", 16); // passes
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Int(16));
    }

    #[test]
    fn array_respects_limit() {
        // The LIMIT cap applies to array rows and sets truncated.
        let q = crate::query::parse::parse("SELECT @length FROM char[] LIMIT 2").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_array(1, "char[]", 1);
        ex.visit_array(2, "char[]", 2);
        ex.visit_array(3, "char[]", 3); // over the cap
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 2);
        assert!(res.truncated, "hitting the LIMIT cap must set truncated");
    }

    #[test]
    fn from_subquery_does_not_early_stop_on_limit() {
        // SW-6: a FROM-subquery source must NOT apply the scan-time LIMIT — the
        // semi-join (run.rs) filters afterward and applies the LIMIT post-join.
        // Early-stopping here would cap the scan before the semi-join sees the
        // rows, discarding matches. So all matched instances are collected even
        // though LIMIT 2 is present; the executor emits every scanned object and
        // does NOT set `truncated` from a scan cap.
        let q = crate::query::parse::parse("SELECT * FROM (SELECT * FROM C c) x LIMIT 2").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        ex.visit_instance(3, 10, &[]);
        ex.visit_instance(4, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(
            res.row_count, 4,
            "FROM-subquery scan must collect ALL matches (no scan-time LIMIT); \
             the LIMIT is applied post-semi-join in run.rs"
        );
        assert!(
            !res.truncated,
            "FROM-subquery scan must not set truncated from a LIMIT cap"
        );
    }

    #[test]
    fn from_subquery_order_by_does_not_truncate_in_finish() {
        // SW-6 + ORDER BY: a FROM-subquery still sorts in finish() (so the
        // semi-join preserves top-N order) but must NOT truncate to LIMIT there;
        // the post-join LIMIT in run.rs is the single cap. All matched rows come
        // back, sorted ascending by @usedHeapSize (served here as the src_idx).
        let q = crate::query::parse::parse(
            "SELECT @objectId, @usedHeapSize FROM (SELECT * FROM C c) x \
             ORDER BY @usedHeapSize ASC LIMIT 2",
        )
        .unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(3, 10, &[]);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(
            res.row_count, 3,
            "FROM-subquery + ORDER BY must not truncate to LIMIT in finish()"
        );
    }

    #[test]
    fn array_field_projection_is_null() {
        // A bare field has no meaning on an array element; project Null.
        let q = crate::query::parse::parse("SELECT n FROM char[]").unwrap();
        let plan = pq(&q);
        let sc = FieldSchema::with_fields(&[("n", 0, crate::types::HprofType::Int)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_array(1, "char[]", 5);
        let res = ex.finish("q1");
        assert_eq!(res.rows[0][0], QueryValue::Null);
    }

    // --- IN(<subquery>) membership evaluation (Task 23, Step 6) ---

    /// A resolver mapping class 10 = "C" and each dense index to a fixed address,
    /// so `@objectAddress IN (set)` can be exercised end-to-end in the executor.
    struct AddrResolver {
        names: std::collections::HashMap<u64, String>,
        addrs: std::collections::HashMap<usize, u64>,
    }
    impl ClassResolver for AddrResolver {
        fn class_name(&self, class_id: u64) -> Option<&str> {
            self.names.get(&class_id).map(|s| s.as_str())
        }
        fn addr_of(&self, src_idx: usize) -> Option<u64> {
            self.addrs.get(&src_idx).copied()
        }
    }

    #[test]
    fn in_subquery_keeps_only_members() {
        // `@objectAddress IN (<subquery>)` with an injected set {0x200, 0x400}:
        // only objects whose address is a member survive the scan.
        let q = crate::query::parse::parse(
            "SELECT @objectAddress FROM C WHERE @objectAddress IN \
             (SELECT @objectAddress FROM D)",
        )
        .unwrap();
        let plan = pq(&q);
        assert_eq!(plan.in_subplans.len(), 1);
        let sc = AddrResolver {
            names: std::iter::once((10u64, "C".to_string())).collect(),
            addrs: [(1usize, 0x100u64), (2, 0x200), (3, 0x400), (4, 0x800)]
                .into_iter()
                .collect(),
        };
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        let set: std::collections::HashSet<u64> = [0x200u64, 0x400].into_iter().collect();
        ex.set_in_subquery_sets(vec![InSet {
            lhs: Attr::ObjectAddress,
            set,
            truncated: false,
        }]);
        for i in 1..=4usize {
            ex.visit_instance(i, 10, &[]);
        }
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 2, "only the two member addresses survive");
        assert_eq!(res.rows[0][0], QueryValue::Int(0x200));
        assert_eq!(res.rows[1][0], QueryValue::Int(0x400));
        assert!(!res.truncated);
    }

    #[test]
    fn in_subquery_truncated_set_marks_result_truncated() {
        // A truncated inner membership set means membership is incomplete: the
        // outer result must be flagged truncated even if all scanned rows match.
        let q = crate::query::parse::parse(
            "SELECT @objectAddress FROM C WHERE @objectAddress IN \
             (SELECT @objectAddress FROM D)",
        )
        .unwrap();
        let plan = pq(&q);
        let sc = AddrResolver {
            names: std::iter::once((10u64, "C".to_string())).collect(),
            addrs: std::iter::once((1usize, 0x200u64)).collect(),
        };
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.set_in_subquery_sets(vec![InSet {
            lhs: Attr::ObjectAddress,
            set: std::iter::once(0x200u64).collect(),
            truncated: true,
        }]);
        ex.visit_instance(1, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert!(
            res.truncated,
            "a truncated membership set taints the outer result"
        );
    }

    #[test]
    fn from_subquery_matches_all_classes() {
        // A FROM-subquery source has no class pattern of its own, so the outer
        // executor considers every object (identity is later semi-joined). Here,
        // both class 10 and 20 objects are emitted (no WHERE, no injection).
        let q = crate::query::parse::parse("SELECT @objectId FROM (SELECT * FROM C c) x").unwrap();
        let plan = pq(&q);
        assert!(plan.from_subplan.is_some());
        let sc = schema(&[(10, "C"), (20, "D")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 20, &[]);
        let res = ex.finish("q1");
        assert_eq!(
            res.row_count, 2,
            "FROM-subquery outer matches all objects pre-semijoin"
        );
    }

    // ============================================================
    // Column alias (AS <name>) tests in execute
    // ============================================================

    #[test]
    fn alias_overrides_column_name_in_finish() {
        let q = crate::query::parse::parse("SELECT @objectId AS myid FROM com.acme.Foo").unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(5, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.columns.len(), 1);
        assert_eq!(
            res.columns[0].name, "myid",
            "alias must override derived @objectId name"
        );
    }

    #[test]
    fn no_alias_preserves_derived_column_name() {
        let q = crate::query::parse::parse("SELECT @usedHeapSize FROM com.acme.Foo").unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(5, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.columns[0].name, "@usedHeapSize");
    }

    #[test]
    fn multiple_aliases_applied_per_column() {
        let q = crate::query::parse::parse(
            "SELECT @objectId AS id, @usedHeapSize AS bytes FROM com.acme.Foo",
        )
        .unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(5, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.columns.len(), 2);
        assert_eq!(res.columns[0].name, "id");
        assert_eq!(res.columns[1].name, "bytes");
    }

    #[test]
    fn quoted_alias_applied_to_column() {
        let q = crate::query::parse::parse(r#"SELECT @usedHeapSize AS "size" FROM com.acme.Foo"#)
            .unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(5, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.columns[0].name, "size");
    }

    #[test]
    fn count_aggregate_alias() {
        let q = crate::query::parse::parse("SELECT COUNT(*) AS n FROM com.acme.Foo").unwrap();
        let plan = pq(&q);
        let sc = schema(&[(10, "com.acme.Foo")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(5, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.columns[0].name, "n");
    }

    #[test]
    fn query_columns_helper_respects_aliases() {
        use crate::query::ast::{Attr, Query, SelectItem};
        use crate::query::ast::{ClassSpec, FromSource};
        let q = Query {
            distinct: false,
            select: vec![SelectItem::Attr(Attr::ObjectId), SelectItem::Star],
            select_aliases: vec![Some("myid".to_string()), None],
            retained_set: false,
            from: FromSource::Class(ClassSpec {
                instanceof: false,
                class_name: "C".into(),
                is_regex: false,
            }),
            alias: None,
            where_: None,
            order_by: None,
            limit: None,
            offset: None,
            union_branches: Vec::new(),
            union_limit: None,
            group_by: Vec::new(),
            having: None,
            intersect_branches: Vec::new(),
            except_branches: Vec::new(),
        };
        let cols = query_columns(&q);
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].name, "myid");
        assert_eq!(cols[1].name, "*");
    }

    // ============================================================
    // Task 7: scan-time aggregate accumulator unit tests
    // ============================================================

    /// Resolver that supplies a fixed shallow size per dense index.
    struct ShallowSchema {
        name: &'static str,
        class_id: u64,
        /// Map from dense index → shallow size (bytes).
        sizes: std::collections::HashMap<usize, u32>,
    }
    impl ShallowSchema {
        fn new(name: &'static str, sizes: &[(usize, u32)]) -> Self {
            ShallowSchema {
                name,
                class_id: 10,
                sizes: sizes.iter().copied().collect(),
            }
        }
    }
    impl ClassResolver for ShallowSchema {
        fn class_name(&self, class_id: u64) -> Option<&str> {
            if class_id == self.class_id {
                Some(self.name)
            } else {
                None
            }
        }
        fn shallow_of(&self, src_idx: usize) -> Option<u32> {
            self.sizes.get(&src_idx).copied()
        }
    }

    /// `SELECT COUNT(*) FROM C` via SingleScan accumulator (no WHERE → but we
    /// also test with WHERE = absent so this routes to HistogramOnly; add a WHERE
    /// to force SingleScan path). Use `WHERE @usedHeapSize > 0` to route to scan.
    #[test]
    fn scan_acc_count_star_with_where() {
        let q =
            crate::query::parse::parse("SELECT COUNT(*) FROM C WHERE @usedHeapSize > 0").unwrap();
        let plan = pq(&q);
        let sc = ShallowSchema::new("C", &[(1, 24), (2, 24), (3, 24)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        ex.visit_instance(3, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1, "aggregate emits exactly 1 row");
        assert_eq!(res.rows[0][0], QueryValue::Int(3), "COUNT(*) = 3");
    }

    /// `SELECT SUM(@usedHeapSize * 2) FROM C WHERE @usedHeapSize > 0` —
    /// aggregate-over-expression routes to SingleScan; SUM must be 2 × sum-of-sizes.
    #[test]
    fn scan_acc_sum_over_expression() {
        // sizes: 10, 20, 30 → SUM(@usedHeapSize * 2) = (10+20+30)*2 = 120
        let q = crate::query::parse::parse(
            "SELECT SUM(@usedHeapSize * 2) FROM C WHERE @usedHeapSize > 0",
        )
        .unwrap();
        let plan = pq(&q);
        let sc = ShallowSchema::new("C", &[(1, 10), (2, 20), (3, 30)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        ex.visit_instance(3, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(
            res.rows[0][0],
            QueryValue::Int(120),
            "SUM(@usedHeapSize * 2) = 120"
        );
    }

    /// SUM ignores Null per-object values (object with unknown shallow size).
    #[test]
    fn scan_acc_sum_ignores_null_values() {
        // idx 2 has no shallow size → ShallowSchema returns None → project_attr returns Null
        // Null * 2 = Null; SUM should skip it.
        let _q = crate::query::parse::parse(
            "SELECT SUM(@usedHeapSize * 2) FROM C WHERE @usedHeapSize > 0 OR @usedHeapSize = 0",
        )
        .unwrap();
        // Force scan by having a WHERE clause; idx 2 missing from sizes map → shallow_of = None
        let q2 =
            crate::query::parse::parse("SELECT SUM(@usedHeapSize) FROM C WHERE @objectId >= 0")
                .unwrap();
        let plan = pq(&q2);
        let sc = ShallowSchema::new("C", &[(1, 100), (3, 50)]); // idx 2 missing
        let mut ex = SingleScanExecutor::new(&q2, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]); // shallow_of(2) = None → Null → skipped
        ex.visit_instance(3, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        // SUM of 100 + (Null skipped) + 50 = 150
        assert_eq!(res.rows[0][0], QueryValue::Int(150), "SUM skips Null");
    }

    /// AVG = sum / count, returned as Float.
    #[test]
    fn scan_acc_avg_is_float() {
        // sizes 10, 20, 30 → avg = 20.0
        let q =
            crate::query::parse::parse("SELECT AVG(@usedHeapSize) FROM C WHERE @usedHeapSize > 0")
                .unwrap();
        let plan = pq(&q);
        let sc = ShallowSchema::new("C", &[(1, 10), (2, 20), (3, 30)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        ex.visit_instance(3, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Float(20.0), "AVG = 20.0");
    }

    /// AVG of zero numeric values → Null (matches histogram behavior).
    #[test]
    fn scan_acc_avg_of_empty_is_null() {
        // No objects match (class 99 unknown) → count 0 → AVG = Null.
        let q = crate::query::parse::parse(
            "SELECT AVG(@usedHeapSize) FROM Missing WHERE @usedHeapSize > 0",
        )
        .unwrap();
        let plan = pq(&q);
        let sc = ShallowSchema::new("C", &[(1, 10)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]); // class 10 = "C", not "Missing"
        let res = ex.finish("q1");
        assert_eq!(
            res.row_count, 1,
            "aggregate always emits 1 row even with 0 matches"
        );
        assert_eq!(
            res.rows[0][0],
            QueryValue::Null,
            "AVG with no values → Null"
        );
    }

    /// MIN and MAX return correct bounds.
    #[test]
    fn scan_acc_min_max_correct() {
        let q = crate::query::parse::parse(
            "SELECT MIN(@usedHeapSize), MAX(@usedHeapSize) FROM C WHERE @usedHeapSize > 0",
        )
        .unwrap();
        let plan = pq(&q);
        let sc = ShallowSchema::new("C", &[(1, 8), (2, 32), (3, 16)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        ex.visit_instance(3, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Int(8), "MIN = 8");
        assert_eq!(res.rows[0][1], QueryValue::Int(32), "MAX = 32");
    }

    /// MIN/MAX with zero matched objects → Null.
    #[test]
    fn scan_acc_min_max_of_empty_is_null() {
        let q = crate::query::parse::parse(
            "SELECT MIN(@usedHeapSize), MAX(@usedHeapSize) FROM Missing WHERE @usedHeapSize > 0",
        )
        .unwrap();
        let plan = pq(&q);
        let sc = ShallowSchema::new("C", &[(1, 8)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]); // class 10 = "C", not "Missing"
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(
            res.rows[0][0],
            QueryValue::Null,
            "MIN with no values → Null"
        );
        assert_eq!(
            res.rows[0][1],
            QueryValue::Null,
            "MAX with no values → Null"
        );
    }

    /// SUM promotes to Float when any per-object value is Float.
    #[test]
    fn scan_acc_sum_promotes_to_float_when_float_present() {
        // Use @usedHeapSize / 2.0 to introduce a float (Int / Float = Float).
        let q = crate::query::parse::parse(
            "SELECT SUM(@usedHeapSize / 2.0) FROM C WHERE @usedHeapSize > 0",
        )
        .unwrap();
        let plan = pq(&q);
        // sizes: 10, 20 → each / 2.0 = 5.0, 10.0 → SUM = 15.0
        let sc = ShallowSchema::new("C", &[(1, 10), (2, 20)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        match &res.rows[0][0] {
            QueryValue::Float(f) => assert!(
                (f - 15.0).abs() < 1e-9,
                "SUM of floats must be 15.0, got {f}"
            ),
            other => panic!("expected Float(15.0), got {other:?}"),
        }
    }

    /// COUNT(expr) counts objects where the expr evaluates to non-Null.
    /// An object with no shallow size gives Null for @usedHeapSize; COUNT skips it.
    #[test]
    fn scan_acc_count_expr_skips_null() {
        let q =
            crate::query::parse::parse("SELECT COUNT(@usedHeapSize) FROM C WHERE @objectId >= 0")
                .unwrap();
        let plan = pq(&q);
        // idx 2 missing → shallow_of returns None → Null → not counted
        let sc = ShallowSchema::new("C", &[(1, 24), (3, 24)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]); // Null → skipped for COUNT(expr)
        ex.visit_instance(3, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(
            res.rows[0][0],
            QueryValue::Int(2),
            "COUNT(expr) must skip Null values, expected 2"
        );
    }

    /// Non-aggregate scan must be completely unaffected — no accumulator interference.
    #[test]
    fn scan_no_aggregate_is_unaffected_by_accumulator() {
        let q = crate::query::parse::parse("SELECT @usedHeapSize FROM C WHERE @usedHeapSize > 0")
            .unwrap();
        let plan = pq(&q);
        let sc = ShallowSchema::new("C", &[(1, 10), (2, 20), (3, 30)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        ex.visit_instance(3, 10, &[]);
        let res = ex.finish("q1");
        // Must produce 3 individual rows, NOT one aggregate row.
        assert_eq!(
            res.row_count, 3,
            "non-aggregate must produce per-object rows"
        );
        assert_eq!(res.rows[0][0], QueryValue::Int(10));
        assert_eq!(res.rows[1][0], QueryValue::Int(20));
        assert_eq!(res.rows[2][0], QueryValue::Int(30));
    }
}
