//! Explicit query-optimizer pass. Pure building blocks consumed by later
//! optimizer stages (predicate reordering, limit pushdown, projection
//! deferral). This module holds no query state — it operates on `QueryPlan`s
//! and a `SchemaStats` snapshot of per-class instance counts.

use std::collections::HashMap;

use crate::query::ast::{Attr, Query, SelectItem};
use crate::query::carry::CarryLayout;
use crate::query::plan::DeferredProj;
use crate::query::plan::{PredCost, QueryPlan, StageOp};

/// Per-class instance counts sampled from the heap, used by the optimizer to
/// estimate predicate selectivity. Built once and shared read-only across the
/// optimization of every query.
#[derive(Debug, Default, Clone)]
pub struct SchemaStats {
    pub instance_counts: HashMap<String, u64>,
}

impl SchemaStats {
    /// Instance count for a class by name, or 0 if the class was never seen
    /// (an absent class has no live instances, so 0 is the correct estimate).
    pub fn count_of(&self, class: &str) -> u64 {
        self.instance_counts.get(class).copied().unwrap_or(0)
    }
}

/// Cost rank of a predicate class: lower = cheaper = evaluate earlier.
/// Type checks are near-free; scalar field compares need a decode; string
/// compares need a decode + UTF-8 materialization; ref-path predicates walk
/// the forward-ref graph and sort last.
pub fn pred_cost_rank(c: PredCost) -> u8 {
    match c {
        PredCost::Type => 0,
        PredCost::Scalar => 1,
        PredCost::Str => 2,
        PredCost::Ref => 3,
    }
}

/// Reorder a plan's WHERE conjuncts cheap-first so inexpensive predicates
/// filter rows before expensive ones evaluate. Stable within a cost class, so
/// user-written order is preserved among equally-cheap predicates. Idempotent.
pub fn reorder_predicates(plan: &mut QueryPlan) {
    plan.where_terms.sort_by_key(|c| pred_cost_rank(c.cost));
}

/// Push a query's LIMIT down to the physical scan as an early-stop bound, but
/// ONLY when doing so cannot change results. Unsafe when: there is no LIMIT;
/// the query is order-sensitive (ORDER BY must see the full match set before
/// truncating); or any late-phase op / UNION / subquery needs the complete
/// match set (dominators, retained joins, refwalk, semi-joins all force a full
/// scan). In those cases `scan_limit` stays `None` and the full semantic LIMIT
/// is applied later.
pub fn pushdown_limit(plan: &mut QueryPlan) {
    let safe = plan.limit.is_some()
        && !plan.order_sensitive
        && plan.late_ops.is_empty()
        && plan.union_branches.is_empty()
        && plan.from_subplan.is_none()
        && plan.in_subplans.is_empty();
    plan.scan_limit = if safe { plan.limit } else { None };
}

/// True if a SELECT item (or the attribute it contains) is deferrable:
/// N-hop `RefPath` and `@retainedHeapSize` are expensive projection-only
/// attributes. Recurses into `Aggregate` to inspect the wrapped argument.
fn select_item_is_deferrable(item: &SelectItem) -> bool {
    match item {
        SelectItem::Attr(Attr::RefPath { .. }) | SelectItem::Attr(Attr::RetainedHeapSize) => true,
        SelectItem::Aggregate { arg, .. } => select_item_is_deferrable(arg),
        _ => false,
    }
}

/// Mark projection-only expensive attributes (N-hop RefPath, `@retainedHeapSize`)
/// for deferral past the WHERE filter: they are evaluated only for surviving
/// rows. Idempotent — clears and recomputes `deferred_projections` each call.
pub fn defer_projections(plan: &mut QueryPlan, query: &Query) {
    plan.deferred_projections.clear();
    for (i, item) in query.select.iter().enumerate() {
        if select_item_is_deferrable(item) {
            plan.deferred_projections
                .push(DeferredProj { select_index: i });
        }
    }
}

/// Clear late-phase `QueryNeeds` flags that no surviving `late_ops` op
/// requires. Only the late-armed needs (`retained`, `dominator_children`,
/// `ref_walk`) are recomputed from `late_ops`; scan-time needs (histogram,
/// instance_*, runtime_type) are left untouched because they derive from the
/// SELECT/WHERE that this plan-only view cannot re-inspect. Idempotent.
pub fn eliminate_dead_needs(plan: &mut QueryPlan) {
    let mut retained = false;
    let mut dominator_children = false;
    let mut ref_walk = false;
    for op in &plan.late_ops {
        match op {
            StageOp::JoinRetained => retained = true,
            StageOp::RetainedSet { .. } => {
                retained = true;
                dominator_children = true;
            }
            StageOp::DominatorChildren { .. } | StageOp::DominatorOf => {
                dominator_children = true;
            }
            StageOp::RefWalkResolve { .. } => ref_walk = true,
            // Edge-retention ops (`@inbounds`/`@outbounds`/`path`) are gated by
            // per-run RunFlags, not by a QueryNeeds field, so there is no
            // late-armed need to recompute for them here.
            StageOp::EdgeLookup { .. } | StageOp::BoundedPath { .. } => {}
            // ResolveStringValues arms `string_values` which is handled separately
            // (tracked in QueryNeeds but not cleared here; it's already minimal).
            StageOp::ResolveStringValues => {}
        }
    }
    // Only DOWNGRADE (clear) — never set a need true here (setting is the
    // planner's job). AND with the recomputed referent so a need survives only
    // if both the planner set it AND a late op still references it.
    plan.needs.retained &= retained;
    plan.needs.dominator_children &= dominator_children;
    plan.needs.ref_walk &= ref_walk;
}

/// Narrow the scan-time carry layout to the minimum needed downstream. An
/// `IndexOnly` carry is already minimal (no-op). For an `IndexPlusScalars`
/// carry, if no downstream op consumes any carried scalar, downgrade to
/// `IndexOnly`. (Column-level width pruning is a future refinement; today the
/// planner only ever emits `IndexOnly`, so this conservatively downgrades an
/// all-unused scalar carry and otherwise leaves the layout intact.) Idempotent.
pub fn narrow_carry(plan: &mut QueryPlan) {
    if let CarryLayout::IndexPlusScalars { widths } = &plan.carry {
        if widths.is_empty() {
            plan.carry = CarryLayout::IndexOnly;
        }
    }
}

/// Reorder candidate class scans by estimated selectivity (smallest instance
/// count first), so the narrower scan drives a semi-join. Currently a no-op:
/// the planner commits to a single scan source, leaving no choice to reorder.
/// Kept as the extension point for subquery-driven scan selection; consults
/// `stats.count_of` once multiple candidates are recorded on the plan.
pub fn order_by_selectivity(_plan: &mut QueryPlan, _stats: &SchemaStats) {
    // No multi-candidate scan choice is recorded on QueryPlan yet. When one is
    // added, sort candidates ascending by stats.count_of(class).
}

/// The full optimizer pass: reorder predicates, push LIMIT to the scan when
/// safe, defer expensive projections past the filter, prune dead needs, narrow
/// the carry layout, then recurse into FROM-subplans, IN-subplans, and UNION
/// branches so nested plans are optimized too. Idempotent: every rewrite is a
/// no-op on already-optimized input (stable sorts, clear-then-recompute,
/// downgrade-only), so `optimize(optimize(p)) == optimize(p)`.
pub fn optimize(mut plan: QueryPlan, query: &Query, stats: &SchemaStats) -> QueryPlan {
    reorder_predicates(&mut plan);
    order_by_selectivity(&mut plan, stats);
    pushdown_limit(&mut plan);
    defer_projections(&mut plan, query);
    eliminate_dead_needs(&mut plan);
    narrow_carry(&mut plan);

    // Recurse into the FROM-subplan, if any. We take the subplan out first to
    // avoid a simultaneous mutable/immutable borrow on `plan`.
    if let Some(sub) = plan.from_subplan.take() {
        plan.from_subplan = Some(Box::new(match query.from.as_subquery() {
            Some(sub_ast) => optimize(*sub, sub_ast, stats),
            // Defensive: from_subplan is Some but FROM is not a subquery — shouldn't
            // happen (planner builds them in lockstep) but we must not drop the plan.
            None => *sub,
        }));
    }

    // Recurse into each IN-subplan. Clone the inner AST to avoid a split borrow
    // (`isp.inner` immutable while `isp.plan` is taken mutably).
    for isp in &mut plan.in_subplans {
        let inner = isp.inner.clone();
        let sub = std::mem::take(&mut isp.plan);
        isp.plan = optimize(sub, &inner, stats);
    }

    // Recurse into UNION branches; positionally matched with query.union_branches.
    let branch_asts = &query.union_branches;
    plan.union_branches = plan
        .union_branches
        .into_iter()
        .enumerate()
        .map(|(i, b)| match branch_asts.get(i) {
            Some(bast) => optimize(b, bast, stats),
            // Defensive: no matching AST branch — leave as-is rather than dropping.
            None => b,
        })
        .collect();

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ast::{Attr, CompareOp, Predicate, Value};
    use crate::query::carry::CarryLayout;
    use crate::query::parse::parse;
    use crate::query::plan::StageOp;
    use crate::query::plan::plan_query;
    use crate::query::plan::{Conjunct, Phase, PredCost, StageKind};

    // ---------- helpers ----------

    fn pq(q: &crate::query::ast::Query) -> QueryPlan {
        plan_query(q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap()
    }

    /// Build a minimal Conjunct with a scalar integer-compare predicate (cost = `cost`).
    /// The field name is embedded in the predicate so two conjuncts at the same cost
    /// but different field names can be told apart.
    fn scalar_conjunct(field: &str, cost: PredCost) -> Conjunct {
        Conjunct {
            pred: Predicate::Compare {
                lhs: Attr::Field(field.to_string()),
                op: CompareOp::Gt,
                rhs: Value::Int(0),
            },
            cost,
        }
    }

    // ---------- reorder_predicates tests ----------

    /// reorder_predicates must sort where_terms by pred_cost_rank, cheapest first.
    /// We hand-build the conjuncts (Ref, Str, Scalar, Type in that worst-first order)
    /// to make the test independent of parse+plan_query's own internal sort.
    #[test]
    fn reorder_sorts_cheap_first() {
        // Build a plan whose where_terms are deliberately in worst-first order.
        let mut plan =
            pq(&parse("SELECT @objectId FROM java.lang.String").unwrap());
        plan.where_terms = vec![
            scalar_conjunct("d", PredCost::Ref),
            scalar_conjunct("c", PredCost::Str),
            scalar_conjunct("b", PredCost::Scalar),
            scalar_conjunct("a", PredCost::Type),
        ];

        reorder_predicates(&mut plan);

        let ranks: Vec<u8> = plan
            .where_terms
            .iter()
            .map(|c| pred_cost_rank(c.cost))
            .collect();
        assert!(
            ranks.windows(2).all(|w| w[0] <= w[1]),
            "expected non-decreasing ranks, got: {:?}",
            ranks
        );
        // Cheapest first: rank 0 first, rank 3 last.
        assert_eq!(
            ranks.first().copied(),
            Some(0),
            "first conjunct must be cheapest"
        );
        assert_eq!(
            ranks.last().copied(),
            Some(3),
            "last conjunct must be most expensive"
        );
    }

    /// Within the same cost class, the relative user-written order must be preserved
    /// (stable sort). We put two Scalar conjuncts ('first', 'second') in known order
    /// and verify reorder_predicates preserves that order.
    #[test]
    fn reorder_is_stable_within_cost_class() {
        let mut plan =
            pq(&parse("SELECT @objectId FROM java.lang.String").unwrap());
        plan.where_terms = vec![
            scalar_conjunct("first", PredCost::Scalar),
            scalar_conjunct("second", PredCost::Scalar),
        ];

        reorder_predicates(&mut plan);

        assert_eq!(plan.where_terms.len(), 2);
        match &plan.where_terms[0].pred {
            Predicate::Compare {
                lhs: Attr::Field(name),
                ..
            } => {
                assert_eq!(
                    name, "first",
                    "stable sort must preserve first conjunct's position"
                );
            }
            other => panic!("unexpected predicate: {:?}", other),
        }
        match &plan.where_terms[1].pred {
            Predicate::Compare {
                lhs: Attr::Field(name),
                ..
            } => {
                assert_eq!(
                    name, "second",
                    "stable sort must preserve second conjunct's position"
                );
            }
            other => panic!("unexpected predicate: {:?}", other),
        }
    }

    /// Calling reorder_predicates twice must produce the same result as calling
    /// it once (idempotent).
    #[test]
    fn reorder_is_idempotent() {
        let mut plan =
            pq(&parse("SELECT @objectId FROM java.lang.String").unwrap());
        plan.where_terms = vec![
            scalar_conjunct("d", PredCost::Ref),
            scalar_conjunct("c", PredCost::Str),
            scalar_conjunct("b", PredCost::Scalar),
            scalar_conjunct("a", PredCost::Type),
        ];

        reorder_predicates(&mut plan);
        let after_first: Vec<PredCost> = plan.where_terms.iter().map(|c| c.cost).collect();

        reorder_predicates(&mut plan);
        let after_second: Vec<PredCost> = plan.where_terms.iter().map(|c| c.cost).collect();

        assert_eq!(
            after_first, after_second,
            "reorder_predicates must be idempotent"
        );
    }

    /// A plan with no WHERE clause has an empty where_terms; reorder_predicates
    /// must leave it empty and not panic.
    #[test]
    fn reorder_empty_where_is_noop() {
        let mut plan = pq(&parse("SELECT * FROM java.lang.String").unwrap());
        assert!(
            plan.where_terms.is_empty(),
            "precondition: no WHERE → empty where_terms"
        );

        reorder_predicates(&mut plan); // must not panic

        assert!(
            plan.where_terms.is_empty(),
            "where_terms must remain empty after reorder"
        );
    }

    #[test]
    fn pred_cost_orders_cheap_first() {
        assert!(pred_cost_rank(PredCost::Type) < pred_cost_rank(PredCost::Scalar));
        assert!(pred_cost_rank(PredCost::Scalar) < pred_cost_rank(PredCost::Str));
        assert!(pred_cost_rank(PredCost::Str) < pred_cost_rank(PredCost::Ref));
    }

    #[test]
    fn pred_cost_type_is_zero() {
        assert_eq!(pred_cost_rank(PredCost::Type), 0);
    }

    #[test]
    fn schema_stats_count_of_defaults_zero() {
        let stats = SchemaStats::default();
        assert_eq!(stats.count_of("java.lang.String"), 0);
        assert_eq!(stats.count_of("com.example.Foo"), 0);
        assert_eq!(stats.count_of(""), 0);
    }

    #[test]
    fn schema_stats_count_of_returns_inserted() {
        let mut stats = SchemaStats::default();
        stats
            .instance_counts
            .insert("java.lang.String".to_string(), 42);
        assert_eq!(stats.count_of("java.lang.String"), 42);
        assert_eq!(stats.count_of("java.lang.Object"), 0);
    }

    #[test]
    fn schema_stats_default_is_empty() {
        assert!(SchemaStats::default().instance_counts.is_empty());
    }

    // ---------- pushdown_limit tests (Task 31) ----------

    /// A simple LIMIT query with no ORDER BY and no late ops: pushdown_limit must
    /// set scan_limit to the same value as limit.
    #[test]
    fn limit_pushed_to_scan_when_safe() {
        let mut plan =
            pq(&parse("SELECT @objectId FROM java.lang.String LIMIT 10").unwrap());
        pushdown_limit(&mut plan);
        assert_eq!(
            plan.scan_limit,
            Some(10),
            "scan_limit must equal limit when pushdown is safe"
        );
    }

    /// ORDER BY @retainedHeapSize triggers a JoinRetained late op AND sets
    /// order_sensitive. Either condition alone blocks pushdown; this tests both.
    #[test]
    fn limit_not_pushed_with_order_by() {
        let mut plan = pq(&parse("SELECT @objectId FROM java.lang.String ORDER BY @retainedHeapSize LIMIT 10")
                .unwrap());
        pushdown_limit(&mut plan);
        assert_eq!(
            plan.scan_limit, None,
            "ORDER BY @retainedHeapSize must block limit pushdown (order_sensitive + late_ops)"
        );
    }

    /// ORDER BY a plain scalar (@usedHeapSize) parses, does NOT produce a late op
    /// (no cross-phase), but sets order_sensitive = true. That alone must be
    /// enough to block pushdown. This exercises the order_sensitive guard in
    /// isolation (no late_ops are present).
    #[test]
    fn limit_not_pushed_with_scalar_order_by() {
        let mut plan = pq(&parse("SELECT @objectId FROM java.lang.String ORDER BY @usedHeapSize LIMIT 10")
                .unwrap());
        // Verify precondition: no late ops (so order_sensitive is the ONLY blocker).
        assert!(
            plan.late_ops.is_empty(),
            "precondition: ORDER BY @usedHeapSize must not produce late ops, got {:?}",
            plan.late_ops
        );
        assert!(
            plan.order_sensitive,
            "precondition: ORDER BY must set order_sensitive"
        );
        pushdown_limit(&mut plan);
        assert_eq!(
            plan.scan_limit, None,
            "order_sensitive alone (no late ops) must block limit pushdown"
        );
    }

    /// A query with no LIMIT produces no scan_limit regardless.
    #[test]
    fn no_limit_means_no_scan_limit() {
        let mut plan =
            pq(&parse("SELECT @objectId FROM java.lang.String").unwrap());
        pushdown_limit(&mut plan);
        assert_eq!(
            plan.scan_limit, None,
            "absent LIMIT must leave scan_limit None"
        );
    }

    /// A query with late ops (JoinRetained due to @retainedHeapSize in SELECT)
    /// must not have its limit pushed down even though there is no ORDER BY on
    /// a retained key (we add a LIMIT but no ORDER BY so order_sensitive is
    /// false, but late_ops is non-empty).
    #[test]
    fn limit_not_pushed_with_late_ops() {
        // @retainedHeapSize in SELECT → JoinRetained late op; no ORDER BY so
        // order_sensitive is false; LIMIT 5 is present. The late op blocks pushdown.
        let mut plan =
            pq(&parse("SELECT @retainedHeapSize FROM java.lang.String LIMIT 5").unwrap());
        // Verify precondition: late ops non-empty and order_sensitive false.
        assert!(
            !plan.late_ops.is_empty(),
            "precondition: @retainedHeapSize in SELECT must produce late ops, got {:?}",
            plan.late_ops
        );
        assert!(
            !plan.order_sensitive,
            "precondition: no ORDER BY → order_sensitive must be false"
        );
        pushdown_limit(&mut plan);
        assert_eq!(
            plan.scan_limit, None,
            "non-empty late_ops must block limit pushdown"
        );
    }

    /// Calling pushdown_limit twice on a safe plan must leave scan_limit
    /// unchanged (idempotent).
    #[test]
    fn pushdown_is_idempotent() {
        let mut plan =
            pq(&parse("SELECT @objectId FROM java.lang.String LIMIT 10").unwrap());
        pushdown_limit(&mut plan);
        assert_eq!(plan.scan_limit, Some(10), "first call must set scan_limit");
        pushdown_limit(&mut plan);
        assert_eq!(
            plan.scan_limit,
            Some(10),
            "second call must not change scan_limit"
        );
    }

    // ---------- defer_projections tests (Task 32) ----------

    /// A SELECT with a projection-only N-hop RefPath (`x.parent.name`) is
    /// deferrable (expensive). After `defer_projections`, `deferred_projections`
    /// must be non-empty and contain select_index 0.
    #[test]
    fn projection_only_refpath_deferred() {
        let query = parse("SELECT x.parent.name FROM Node x").unwrap();
        let mut plan = pq(&query);
        defer_projections(&mut plan, &query);
        assert!(
            !plan.deferred_projections.is_empty(),
            "RefPath SELECT item must be marked deferrable; got {:?}",
            plan.deferred_projections
        );
        assert_eq!(
            plan.deferred_projections[0].select_index, 0,
            "first (only) SELECT item is at index 0"
        );
    }

    /// `SELECT @retainedHeapSize FROM java.lang.String` — the retained-heap-size
    /// attribute is expensive (cross-phase); it must be marked deferrable.
    #[test]
    fn retained_projection_deferred() {
        let query = parse("SELECT @retainedHeapSize FROM java.lang.String").unwrap();
        let mut plan = pq(&query);
        defer_projections(&mut plan, &query);
        assert!(
            !plan.deferred_projections.is_empty(),
            "@retainedHeapSize SELECT item must be marked deferrable; got {:?}",
            plan.deferred_projections
        );
        assert_eq!(plan.deferred_projections[0].select_index, 0);
    }

    /// `SELECT @objectId FROM java.lang.String` — a cheap built-in attr is NOT
    /// deferrable; `deferred_projections` must remain empty.
    #[test]
    fn plain_scalar_projection_not_deferred() {
        let query = parse("SELECT @objectId FROM java.lang.String").unwrap();
        let mut plan = pq(&query);
        defer_projections(&mut plan, &query);
        assert!(
            plan.deferred_projections.is_empty(),
            "@objectId is cheap — must NOT be marked deferrable; got {:?}",
            plan.deferred_projections
        );
    }

    /// `defer_projections` must be idempotent: calling it twice must produce the
    /// same `deferred_projections` as calling it once (the `.clear()` prevents
    /// duplication).
    #[test]
    fn defer_is_idempotent() {
        let query = parse("SELECT @retainedHeapSize FROM java.lang.String").unwrap();
        let mut plan = pq(&query);
        defer_projections(&mut plan, &query);
        let first = plan.deferred_projections.clone();
        defer_projections(&mut plan, &query);
        let second = plan.deferred_projections.clone();
        assert_eq!(
            first, second,
            "calling defer_projections twice must be idempotent"
        );
    }

    // ---------- eliminate_dead_needs tests (Task 32) ----------

    /// A stale `needs.retained = true` with no `JoinRetained` late op must be
    /// cleared by `eliminate_dead_needs`.
    #[test]
    fn dead_retained_need_eliminated() {
        // Build a plan that has no late ops, then manually arm needs.retained.
        let mut plan =
            pq(&parse("SELECT @usedHeapSize FROM java.lang.String").unwrap());
        // Ensure no late ops (precondition).
        plan.late_ops.clear();
        plan.needs.retained = true; // stale: no referent late op
        eliminate_dead_needs(&mut plan);
        assert!(
            !plan.needs.retained,
            "needs.retained must be cleared when no late op references it"
        );
    }

    /// A plan that genuinely uses retained (`SELECT @retainedHeapSize`) has a
    /// `JoinRetained` late op — `eliminate_dead_needs` must PRESERVE `needs.retained`.
    #[test]
    fn live_retained_need_preserved() {
        let mut plan =
            pq(&parse("SELECT @retainedHeapSize FROM java.lang.String").unwrap());
        // Confirm precondition: the planner armed JoinRetained and needs.retained.
        assert!(
            plan.late_ops
                .iter()
                .any(|op| matches!(op, StageOp::JoinRetained)),
            "precondition: @retainedHeapSize SELECT must produce JoinRetained, got {:?}",
            plan.late_ops
        );
        assert!(
            plan.needs.retained,
            "precondition: needs.retained must be set by planner"
        );
        eliminate_dead_needs(&mut plan);
        assert!(
            plan.needs.retained,
            "needs.retained must stay true when JoinRetained late op is present"
        );
    }

    /// `eliminate_dead_needs` must NOT touch scan-time needs
    /// (`instance_scalar`, `instance_string`, `runtime_type`). We build a plan
    /// that arms all three via WHERE, record the flags, call
    /// `eliminate_dead_needs`, and assert they are unchanged.
    #[test]
    fn eliminate_does_not_touch_scan_needs() {
        // @displayName = "foo" → instance_string; count > 1 → instance_scalar;
        // s INSTANCEOF java.lang.Object → runtime_type.
        let mut plan = pq(&parse(
            "SELECT * FROM C s WHERE @displayName = \"foo\" \
             AND count > 1 AND s INSTANCEOF java.lang.Object",
        )
        .unwrap());
        // Confirm preconditions.
        assert!(plan.needs.instance_string, "precondition: instance_string");
        assert!(plan.needs.instance_scalar, "precondition: instance_scalar");
        assert!(plan.needs.runtime_type, "precondition: runtime_type");
        let before_scalar = plan.needs.instance_scalar;
        let before_string = plan.needs.instance_string;
        let before_rt = plan.needs.runtime_type;
        eliminate_dead_needs(&mut plan);
        assert_eq!(
            plan.needs.instance_scalar, before_scalar,
            "instance_scalar must not change"
        );
        assert_eq!(
            plan.needs.instance_string, before_string,
            "instance_string must not change"
        );
        assert_eq!(
            plan.needs.runtime_type, before_rt,
            "runtime_type must not change"
        );
    }

    /// Calling `eliminate_dead_needs` twice must leave `needs` identical to
    /// calling it once (idempotent).
    #[test]
    fn eliminate_is_idempotent() {
        let mut plan =
            pq(&parse("SELECT @retainedHeapSize FROM java.lang.String").unwrap());
        eliminate_dead_needs(&mut plan);
        let needs_after_first = plan.needs.clone();
        eliminate_dead_needs(&mut plan);
        assert_eq!(
            plan.needs, needs_after_first,
            "eliminate_dead_needs must be idempotent"
        );
    }

    // ---------- optimize (Task 33) tests ----------

    /// Calling optimize twice must produce the same plan as calling it once.
    /// Also verifies that a simple LIMIT query has scan_limit pushed down.
    #[test]
    fn optimize_is_idempotent_and_composes() {
        let src = "SELECT @objectId FROM java.lang.String LIMIT 3";
        let q = parse(src).unwrap();
        let plan = pq(&q);
        let once = optimize(plan.clone(), &q, &SchemaStats::default());
        let twice = optimize(once.clone(), &q, &SchemaStats::default());
        assert_eq!(once, twice, "optimize must be idempotent");
        assert_eq!(once.scan_limit, Some(3), "scan_limit must be pushed down");
    }

    /// After optimize, where_terms must be sorted cheapest-first and scan_limit set.
    #[test]
    fn optimize_reorders_and_pushes_limit() {
        // Build a plan from a LIMIT query, then manually inject worst-first predicates.
        let src = "SELECT @objectId FROM java.lang.String LIMIT 5";
        let q = parse(src).unwrap();
        let mut plan = pq(&q);
        // Inject Ref > Str > Scalar > Type (worst-first) to verify reorder.
        plan.where_terms = vec![
            scalar_conjunct("d", PredCost::Ref),
            scalar_conjunct("c", PredCost::Str),
            scalar_conjunct("b", PredCost::Scalar),
            scalar_conjunct("a", PredCost::Type),
        ];
        // With where_terms injected, pushdown_limit sees a non-empty plan; but
        // late_ops, union_branches, from_subplan and in_subplans are empty for
        // this query, so pushdown is still safe (order_sensitive is false too).
        let optimized = optimize(plan, &q, &SchemaStats::default());
        let ranks: Vec<u8> = optimized
            .where_terms
            .iter()
            .map(|c| pred_cost_rank(c.cost))
            .collect();
        assert!(
            ranks.windows(2).all(|w| w[0] <= w[1]),
            "where_terms must be sorted cheapest-first after optimize, got ranks: {:?}",
            ranks
        );
        assert_eq!(
            optimized.scan_limit,
            Some(5),
            "scan_limit must be pushed down by optimize"
        );
    }

    /// optimize must recurse into union_branches so each branch is also optimized.
    #[test]
    fn optimize_recurses_into_union_branches() {
        let src =
            "SELECT @objectId FROM java.lang.String UNION SELECT @objectId FROM java.lang.Object";
        let q = parse(src).unwrap();
        let plan = pq(&q);
        assert_eq!(
            plan.union_branches.len(),
            1,
            "precondition: one union branch"
        );
        let once = optimize(plan.clone(), &q, &SchemaStats::default());
        let twice = optimize(once.clone(), &q, &SchemaStats::default());
        // Idempotence holds across the full UNION plan.
        assert_eq!(once, twice, "optimize must be idempotent for UNION queries");
        // Branch count is preserved.
        assert_eq!(
            once.union_branches.len(),
            1,
            "union branch must be preserved after optimize"
        );
    }

    /// QueryPlan::default() must compile and produce sensible zero/empty values.
    #[test]
    fn optimize_default_queryplan_constructs() {
        let d = QueryPlan::default();
        assert_eq!(
            d.kind,
            StageKind::SingleScan,
            "default kind must be SingleScan"
        );
        assert_eq!(
            d.carry,
            CarryLayout::IndexOnly,
            "default carry must be IndexOnly"
        );
        assert_eq!(d.finalize_at, Phase::P1, "default finalize_at must be P1");
        assert!(
            d.where_terms.is_empty(),
            "default where_terms must be empty"
        );
        assert!(d.late_ops.is_empty(), "default late_ops must be empty");
        assert!(
            d.union_branches.is_empty(),
            "default union_branches must be empty"
        );
        assert!(
            d.in_subplans.is_empty(),
            "default in_subplans must be empty"
        );
        assert!(
            d.deferred_projections.is_empty(),
            "default deferred_projections must be empty"
        );
        assert!(
            d.from_subplan.is_none(),
            "default from_subplan must be None"
        );
        assert!(d.limit.is_none(), "default limit must be None");
        assert!(d.scan_limit.is_none(), "default scan_limit must be None");
        assert!(!d.order_sensitive, "default order_sensitive must be false");
        assert_eq!(d.select_arity, 0, "default select_arity must be 0");
    }

    /// order_by_selectivity must be a no-op: calling it must not change the plan.
    #[test]
    fn order_by_selectivity_is_noop() {
        let q = parse("SELECT @objectId FROM java.lang.String").unwrap();
        let mut plan = pq(&q);
        let snapshot = plan.clone();
        order_by_selectivity(&mut plan, &SchemaStats::default());
        assert_eq!(
            plan, snapshot,
            "order_by_selectivity must not change the plan"
        );
    }

    /// optimize must leave an empty WHERE and absent LIMIT unchanged.
    #[test]
    fn optimize_empty_where_and_no_limit() {
        let src = "SELECT @objectId FROM java.lang.String";
        let q = parse(src).unwrap();
        let plan = pq(&q);
        let once = optimize(plan.clone(), &q, &SchemaStats::default());
        assert!(
            once.where_terms.is_empty(),
            "empty WHERE must remain empty after optimize"
        );
        assert_eq!(
            once.scan_limit, None,
            "absent LIMIT must leave scan_limit None after optimize"
        );
        let twice = optimize(once.clone(), &q, &SchemaStats::default());
        assert_eq!(
            once, twice,
            "optimize must be idempotent on a simple no-WHERE no-LIMIT plan"
        );
    }

    // ---------- narrow_carry tests (Task 32) ----------

    /// A plan whose carry is `IndexOnly` (the default) must be untouched by
    /// `narrow_carry`.
    #[test]
    fn narrow_carry_indexonly_is_noop() {
        let mut plan =
            pq(&parse("SELECT @objectId FROM java.lang.String").unwrap());
        assert!(
            matches!(plan.carry, CarryLayout::IndexOnly),
            "precondition: default carry is IndexOnly"
        );
        narrow_carry(&mut plan);
        assert!(
            matches!(plan.carry, CarryLayout::IndexOnly),
            "narrow_carry must leave IndexOnly unchanged"
        );
    }

    /// An `IndexPlusScalars` carry with an empty widths list carries no actual
    /// scalar data, so `narrow_carry` must downgrade it to `IndexOnly`.
    #[test]
    fn narrow_carry_empty_scalars_downgrades() {
        let mut plan =
            pq(&parse("SELECT @objectId FROM java.lang.String").unwrap());
        plan.carry = CarryLayout::IndexPlusScalars { widths: vec![] };
        narrow_carry(&mut plan);
        assert!(
            matches!(plan.carry, CarryLayout::IndexOnly),
            "IndexPlusScalars with empty widths must downgrade to IndexOnly"
        );
    }
}
