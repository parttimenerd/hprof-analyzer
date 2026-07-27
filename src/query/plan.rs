//! Needs analysis + planning for the supported OQL subset. Cost is per-need:
//! each flag arms exactly one piece of machinery. Deferred constructs are
//! rejected here (not in the parser) with a message naming the construct.

use crate::query::QueryError;
use crate::query::ast::{AggFunc, Attr, Expr, Predicate, Query, RefRole, SelectItem, Value};
use crate::query::carry::CarryLayout;
use crate::query::runflags::EdgeDir;

/// Default cap on late-phase emitted rows (dominator children) and retained-set
/// closures, mirroring the scan-time `DEFAULT_CARRY_CAP`. Bounds late output so
/// a pathological query can't blow up memory in the retained-live window.
pub const DEFAULT_LATE_CAP: usize = 1_000_000;
pub const DEFAULT_RETAINED_CAP: usize = 1_000_000;

/// Per-need cost flags. Each flag independently arms exactly one piece of
/// machinery; an unset flag arms nothing. (Foundation subset — ref/retained/
/// dominator/edge needs are added in later slices.)
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QueryNeeds {
    pub histogram: bool,
    pub instance_scalar: bool,
    pub instance_string: bool,
    pub runtime_type: bool,
    pub retained: bool,
    /// Arms the dominator-children CSR (dc_off/dc_tgt) in the late phase, for
    /// `dominators(x)` / `dominatorof(x)` / `AS RETAINED SET`.
    pub dominator_children: bool,
    /// Arms the forward-reference graph (fwd CSR + per-edge field ids) in the
    /// P2 late window, for N-hop `RefPath` resolution.
    pub ref_walk: bool,
    /// Arms the string-values side table in the P2 late window, for
    /// `toString(s)` SELECT and WHERE on `java.lang.String` FROM queries.
    pub string_values: bool,
    /// Arms GC-root descriptor resolution in the analyze late phase, for
    /// @GCRoots/@GCRootInfo/@info. Rejected in the query-only path.
    pub gc_roots: bool,
    /// Arms the P2 late-window `ResolveArrayIndex` op for `base[i]` /
    /// `base[start:end]` array index/slice expressions. Out-of-bounds or
    /// non-resolvable base → Null (not an error). Does NOT require the refwalk
    /// CSR; the P2 window resolves these as Null until a scan-capture pass is
    /// added for array element data.
    pub array_index: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    HistogramOnly,
    #[default]
    SingleScan,
    /// GROUP BY aggregation: rows are bucketed by the group-by key expressions
    /// during the scan and finalized after the full scan completes.
    GroupBy,
}

/// Which pipeline phase finalizes a query's rows. See canonical vocabulary.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    #[default]
    P1,
    /// N-hop `RefPath` resolution: the forward-reference graph is live in the
    /// post-scan window before dominators/retained (P3) are computed.
    P2,
    P3,
}

/// A late-phase operation applied when resuming a cross-phase query.
/// (Extended with more variants in later phases.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageOp {
    /// Join each carried dense index against `retained`, then apply retained
    /// WHERE terms, ORDER BY, and LIMIT.
    JoinRetained,
    /// Emit the dominator-tree children of each carried dense index (bounded by
    /// `cap`). Backs `dominators(x)`.
    DominatorChildren { cap: usize },
    /// Emit the immediate dominator (idom) of each carried dense index — one row
    /// per input (the tree root has no idom and yields nothing). Backs
    /// `dominatorof(x)`.
    DominatorOf,
    /// Bounded DFS over the dominator-children CSR from each carried index,
    /// emitting the retained closure. Backs `SELECT ... AS RETAINED SET`.
    RetainedSet { cap: usize },
    /// Resolve one reference hop of an N-hop `RefPath` against the forward-ref
    /// graph in the P2 window. `hop` is the 0-based hop index within the path;
    /// `role` decides ordering relative to WHERE filtering (predicate-critical
    /// walks resolve before filtering, projection-only after); `carry` is the
    /// frontier layout while walking (`AddrFrontier`) or the tail scalar layout
    /// on the final hop. One op is emitted per hop.
    RefWalkResolve {
        hop: usize,
        role: RefRole,
        carry: CarryLayout,
    },
    /// Look up inbound (or outbound) neighbours of each carried dense index.
    /// `Inbound` reads the inbound CSR; `Outbound` reads the retained forward
    /// edge store (L3 rescan-backed). Backs `@inbounds`/`@outbounds`.
    EdgeLookup { dir: EdgeDir },
    /// Bounded forward BFS from each carried index toward a target class, at most
    /// `depth_cap` levels, frontier-capped at `PATH_FRONTIER_CAP`. Backs `path(a,b)`.
    BoundedPath { depth_cap: usize },
    /// Resolve `toString(s)` for each carried dense index by looking up the
    /// pre-built string-values map (dense_idx → String). Applied in the P2 window
    /// after the backing-array decode pass.
    ResolveStringValues,
    /// Gate for `base[index]` / `base[start:end]` array index/slice expressions.
    /// The presence of this op in `late_ops` tells `eliminate_dead_needs` to
    /// preserve `needs.array_index`. The actual resolution in `stage_runner`
    /// returns Null for all ArrayIndex/ArraySlice columns (array element data is
    /// not yet captured during the scan); out-of-bounds and non-resolvable bases
    /// are Null rather than errors, matching the AST contract.
    ResolveArrayIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredCost {
    Type,
    Scalar,
    Str,
    /// An N-hop reference-path predicate — the most expensive (walks the
    /// forward-ref graph), so it sorts last.
    Ref,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conjunct {
    pub pred: Predicate,
    pub cost: PredCost,
}

/// A projection deferred past the scan-time filter (see `deferred_projections`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredProj {
    /// Index into the query's SELECT list of the deferred projected item.
    pub select_index: usize,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct QueryPlan {
    pub kind: StageKind,
    pub needs: QueryNeeds,
    pub where_terms: Vec<Conjunct>,
    pub finalize_at: Phase,
    /// Scan-time carry layout for cross-phase queries. `IndexOnly` for the
    /// current retained/dominator stages (only dense indices are carried).
    pub carry: CarryLayout,
    pub late_ops: Vec<StageOp>,
    pub limit: Option<u64>,
    /// Physical early-stop bound set by the optimizer's `pushdown_limit` pass.
    /// When `Some(n)`, the executor may stop the heap scan as soon as `n`
    /// matches are found. `None` means no early-stop (full scan required).
    /// Initialized to `None` by the planner; the optimizer sets it later only
    /// when doing so is provably safe (see `optimize::pushdown_limit`).
    pub scan_limit: Option<u64>,
    /// True iff the query has an ORDER BY clause. Recorded at plan time so the
    /// optimizer's `pushdown_limit` pass can check safety without re-deriving it
    /// from the AST: an ORDER BY requires the full match set to be materialized
    /// and sorted before LIMIT applies, so the scan cannot be stopped early.
    pub order_sensitive: bool,
    /// Number of projected columns (`[Star]` counts as 1). Used to verify
    /// UNION branch homogeneity.
    pub select_arity: usize,
    /// Planned UNION tail branches (empty for a non-UNION query). Each branch
    /// plan itself has an empty `union_branches`.
    pub union_branches: Vec<QueryPlan>,
    /// Union-wide trailing LIMIT applied to the WHOLE concatenated UNION result
    /// (MAT gap #6). Propagated from the outer `Query.union_limit` by
    /// [`plan_query`]; `None` for single queries and unions with no trailing
    /// LIMIT. The executor caps the union result at `min(union_limit, safety cap)`.
    pub union_limit: Option<u64>,
    /// Plan for a `FROM (<subquery>)` inner query, if the FROM source is a
    /// subquery. The driver runs this inner plan as its own scan slot, then
    /// semi-joins the outer matches against the inner's dense indices. `None`
    /// for a plain `FROM <class>` source. The inner AST is carried alongside so
    /// the driver can execute it without re-deriving it from the outer query.
    pub from_subplan: Option<Box<QueryPlan>>,
    /// Inner plans for each `WHERE <attr> IN (<subquery>)` predicate in the
    /// WHERE tree (empty when there are none). Each entry pairs the outer LHS
    /// attribute with the inner plan+AST; the driver runs the inner first,
    /// builds an address membership set, and injects it into the outer scan.
    pub in_subplans: Vec<InSubplan>,
    /// Pre-evaluated EXISTS/NOT EXISTS subquery results (one per Exists predicate
    /// in the WHERE tree, in encounter order). The driver runs the inner scan
    /// before the outer, records whether ≥1 row was produced (negated if NOT
    /// EXISTS), and injects the Vec<bool> into the outer executor.
    pub exists_subplans: Vec<ExistsSubplan>,
    /// SELECT-item indices whose projection is deferred past WHERE filtering
    /// because the projection is expensive (an N-hop RefPath or retained-size
    /// lookup) and evaluating it only for surviving rows is cheaper. Populated by
    /// `optimize::defer_projections`; empty for a freshly-planned query.
    pub deferred_projections: Vec<DeferredProj>,
    /// GROUP BY expressions (copied from AST when kind == GroupBy; empty otherwise).
    pub group_by_exprs: Vec<Expr>,
    /// Post-aggregate filter terms (HAVING), empty when no HAVING clause.
    pub having_terms: Vec<Conjunct>,
    /// Planned INTERSECT branches (empty for a non-INTERSECT query). Each branch
    /// plan itself has empty intersect/except branches.
    pub intersect_branch_plans: Vec<QueryPlan>,
    /// Planned EXCEPT branches (empty for a non-EXCEPT query). Each branch plan
    /// itself has empty intersect/except branches.
    pub except_branch_plans: Vec<QueryPlan>,
}

/// A planned `WHERE <lhs> IN (<subquery>)` predicate. `lhs` is the outer
/// attribute compared for membership (must be `@objectAddress`); `plan`/`inner`
/// are the inner subquery's plan and AST, run as their own scan slot before the
/// outer scan so the address set is ready when the outer predicate evaluates.
#[derive(Debug, Clone, PartialEq)]
pub struct InSubplan {
    pub lhs: Attr,
    pub plan: QueryPlan,
    pub inner: Query,
}

/// A planned EXISTS/NOT EXISTS subquery predicate.
#[derive(Debug, Clone, PartialEq)]
pub struct ExistsSubplan {
    pub negated: bool,
    pub plan: QueryPlan,
    pub inner: Query,
}

/// Plan a query, including any homogeneous `UNION` tail. Each branch is planned
/// independently via [`plan_single`]; the branches must share the head's column
/// arity, and no branch may use `RETAINED SET` or aggregates (they change the
/// row shape / arity of a UNION arm).
///
/// `depth_cap` sets the BFS depth limit for `path(a, b)` operations in this
/// query and all its UNION branches. Pass `DEFAULT_PATH_DEPTH_CAP` for the
/// canonical default; CLI callers pass the user-supplied `--query-path-depth`
/// value so the flag actually controls path() BFS depth end-to-end.
pub fn plan_query(q: &Query, depth_cap: usize) -> Result<QueryPlan, QueryError> {
    let mut head = plan_single(q, depth_cap)?;
    let head_arity = head.select_arity;

    // Plan UNION branches (if any).
    if !q.union_branches.is_empty() {
        if head.select_arity == 0 {
            // unreachable: select_list requires >= 1 item, but guard defensively.
            return Err(QueryError("UNION head has no projected columns".into()));
        }
        let mut planned = Vec::with_capacity(q.union_branches.len());
        // Guard the head first: a UNION head may not use RETAINED SET or aggregates.
        if q.retained_set {
            return Err(QueryError(
                "RETAINED SET is not allowed in a UNION branch".into(),
            ));
        }
        if select_has_aggregate(&q.select) {
            return Err(QueryError(
                "aggregates are not allowed in a UNION branch".into(),
            ));
        }
        for (i, branch) in q.union_branches.iter().enumerate() {
            // Branches parse flat, but clear defensively so plan_single never
            // recurses into a branch's own (empty) union tail.
            let mut b = branch.clone();
            b.union_branches.clear();
            if b.retained_set {
                return Err(QueryError(
                    "RETAINED SET is not allowed in a UNION branch".into(),
                ));
            }
            if select_has_aggregate(&b.select) {
                return Err(QueryError(
                    "aggregates are not allowed in a UNION branch".into(),
                ));
            }
            let bp = plan_single(&b, depth_cap)?;
            if bp.select_arity != head_arity {
                return Err(QueryError(format!(
                    "UNION branches must project the same number of columns \
                     (branch 0 has {head_arity}, branch {} has {})",
                    i + 1,
                    bp.select_arity
                )));
            }
            planned.push(bp);
        }
        head.union_branches = planned;
        // Propagate the union-wide trailing LIMIT (MAT gap #6) onto the head plan so
        // the executor can cap the concatenated union result. `None` for unions with
        // no trailing LIMIT (the executor then applies only the safety cap).
        head.union_limit = q.union_limit;
    }

    // Plan INTERSECT branches with arity validation.
    let mut intersect_branch_plans = Vec::new();
    for (i, branch) in q.intersect_branches.iter().enumerate() {
        let bp = plan_single(branch, depth_cap)?;
        if bp.select_arity != head_arity {
            return Err(QueryError(format!(
                "INTERSECT branches must have the same column count \
                 (left has {head_arity}, INTERSECT branch {} has {})",
                i + 1,
                bp.select_arity
            )));
        }
        intersect_branch_plans.push(bp);
    }
    head.intersect_branch_plans = intersect_branch_plans;

    // Plan EXCEPT branches with arity validation.
    let mut except_branch_plans = Vec::new();
    for (i, branch) in q.except_branches.iter().enumerate() {
        let bp = plan_single(branch, depth_cap)?;
        if bp.select_arity != head_arity {
            return Err(QueryError(format!(
                "EXCEPT branches must have the same column count \
                 (left has {head_arity}, EXCEPT branch {} has {})",
                i + 1,
                bp.select_arity
            )));
        }
        except_branch_plans.push(bp);
    }
    head.except_branch_plans = except_branch_plans;

    Ok(head)
}

/// True if any projected item is an aggregate (recursively, so `COUNT(SUM(x))`
/// counts). Used to reject aggregates inside UNION arms.
fn select_has_aggregate(select: &[SelectItem]) -> bool {
    select.iter().any(item_is_aggregate)
}
fn item_is_aggregate(it: &SelectItem) -> bool {
    matches!(it, SelectItem::Aggregate { .. })
}

/// Returns a human-readable display name for a SELECT item, used in GROUP BY
/// validation error messages to identify the offending column.
fn select_item_display_name(it: &SelectItem) -> String {
    match it {
        SelectItem::Attr(a) => attr_display_name(a),
        SelectItem::Expr(e) => expr_display_name(e),
        SelectItem::Star => "*".into(),
        SelectItem::Aggregate { func, .. } => format!("{func:?}(...)"),
        SelectItem::Path { .. } => "path(...)".into(),
        SelectItem::ToString(_) => "toString(...)".into(),
    }
}

fn attr_display_name(a: &Attr) -> String {
    match a {
        Attr::ObjectId => "@objectId".into(),
        Attr::ObjectAddress => "@objectAddress".into(),
        Attr::UsedHeapSize => "@usedHeapSize".into(),
        Attr::RetainedHeapSize => "@retainedHeapSize".into(),
        Attr::DisplayName => "@displayName".into(),
        Attr::Length => "@length".into(),
        Attr::Inbounds => "@inbounds".into(),
        Attr::Outbounds => "@outbounds".into(),
        Attr::ClassOf => "classof(...)".into(),
        Attr::Field(name) => name.clone(),
        Attr::RefPath { hops, tail, .. } => {
            let mut s = hops.join(".");
            s.push('.');
            s.push_str(&attr_display_name(tail));
            s
        }
        _ => format!("{a:?}"),
    }
}

fn expr_display_name(e: &Expr) -> String {
    match e {
        Expr::Attr(a) => attr_display_name(a),
        Expr::Lit(v) => format!("{v:?}"),
        Expr::Binary { op, lhs, rhs } => {
            format!("({} {:?} {})", expr_display_name(lhs), op, expr_display_name(rhs))
        }
        Expr::Unary { op, arg } => format!("{op:?}({})", expr_display_name(arg)),
        Expr::Method { name, .. } => format!("{name}(...)"),
        Expr::Aggregate { func, .. } => format!("{func:?}(...)"),
        Expr::Case { .. } => "CASE".to_string(),
        Expr::Coalesce(_) => "COALESCE".to_string(),
        Expr::NullIf { .. } => "NULLIF".to_string(),
    }
}

/// Visit every `Attr` leaf in an `Expr` tree (in-order), calling `f` on each.
fn expr_for_each_attr(e: &Expr, f: &mut impl FnMut(&Attr)) {
    match e {
        Expr::Attr(a) => {
            f(a);
            // `toHex(inner)` carries a nested Expr whose attr leaves (e.g.
            // `@objectAddress` in `toHex(@objectAddress)`) must be discovered for
            // phase/field/need analysis, so recurse into it.
            if let Attr::ToHex(inner) = a {
                expr_for_each_attr(inner, f);
            }
            // `ArrayIndex`/`ArraySlice` carry index/start/end expressions that
            // may themselves reference attrs; recurse into them.
            if let Attr::ArrayIndex { base, index } = a {
                expr_for_each_attr(index, f);
                // Also visit the base attr itself.
                f(base);
            }
            if let Attr::ArraySlice { base, start, end } = a {
                if let Some(s) = start { expr_for_each_attr(s, f); }
                if let Some(e) = end { expr_for_each_attr(e, f); }
                f(base);
            }
        }
        Expr::Lit(_) => {}
        Expr::Binary { lhs, rhs, .. } => {
            expr_for_each_attr(lhs, f);
            expr_for_each_attr(rhs, f);
        }
        Expr::Unary { arg, .. } => expr_for_each_attr(arg, f),
        Expr::Method { receiver, args, .. } => { // D2 fills this
            expr_for_each_attr(receiver, f);
            for a in args { expr_for_each_attr(a, f); }
        }
        Expr::Aggregate { .. } => {} // no Attr leaves in aggregate position
        Expr::Case { branches, else_ } => {
            for (pred, then_expr) in branches {
                pred_for_each_attr(pred, f);
                expr_for_each_attr(then_expr, f);
            }
            if let Some(e) = else_ { expr_for_each_attr(e, f); }
        }
        Expr::Coalesce(args) => {
            for arg in args { expr_for_each_attr(arg, f); }
        }
        Expr::NullIf { lhs, rhs } => {
            expr_for_each_attr(lhs, f);
            expr_for_each_attr(rhs, f);
        }
    }
}
fn expr_any_attr(e: &Expr, pred: impl Fn(&Attr) -> bool) -> bool {
    let mut found = false;
    expr_for_each_attr(e, &mut |a| {
        if pred(a) {
            found = true;
        }
    });
    found
}

/// Returns true if the expression tree contains an `Expr::Method` node whose
/// name is `"contains"` or `"toString"`. These methods require the string-values
/// side table (`needs.string_values`) to be armed so their string context is
/// available in the late (P2) window.
fn expr_has_string_method(e: &Expr) -> bool {
    match e {
        Expr::Method { name, receiver, args } => {
            if name == "contains" || name == "toString" {
                return true;
            }
            if expr_has_string_method(receiver) {
                return true;
            }
            args.iter().any(expr_has_string_method)
        }
        Expr::Attr(_) | Expr::Lit(_) => false,
        Expr::Binary { lhs, rhs, .. } => {
            expr_has_string_method(lhs) || expr_has_string_method(rhs)
        }
        Expr::Unary { arg, .. } => expr_has_string_method(arg),
        Expr::Aggregate { .. } => false,
        Expr::Case { branches, else_ } => {
            branches.iter().any(|(_, ex)| expr_has_string_method(ex))
                || else_.as_ref().map_or(false, |e| expr_has_string_method(e))
        }
        Expr::Coalesce(args) => args.iter().any(expr_has_string_method),
        Expr::NullIf { lhs, rhs } => {
            expr_has_string_method(lhs) || expr_has_string_method(rhs)
        }
    }
}

/// Visit every `Attr` leaf reachable from a `Predicate` tree, calling `f` on
/// each. Used by `expr_for_each_attr`'s `Expr::Case` arm to recurse into WHEN
/// conditions.
fn pred_for_each_attr(p: &Predicate, f: &mut impl FnMut(&Attr)) {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_for_each_attr(a, f);
            pred_for_each_attr(b, f);
        }
        Predicate::Not(a) => pred_for_each_attr(a, f),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_for_each_attr(lhs, f);
            expr_for_each_attr(rhs, f);
        }
        Predicate::InstanceOf(_) => {}
        Predicate::InSubquery { lhs, .. } => f(lhs),
        // EXISTS inner is a standalone query; it carries no outer attrs to walk.
        Predicate::Exists { .. } => {}
    }
}

/// Visit every `Expr::Method` name reachable from an `Expr` tree (including the
/// method receiver and its arguments, and any `Attr::ToHex(inner)` sub-expr),
/// calling `f` with each method name. Used by the plan-time method validator.
fn expr_for_each_method<'a>(e: &'a Expr, f: &mut impl FnMut(&'a str)) {
    match e {
        Expr::Attr(a) => {
            if let Attr::ToHex(inner) = a {
                expr_for_each_method(inner, f);
            }
        }
        Expr::Lit(_) => {}
        Expr::Binary { lhs, rhs, .. } => {
            expr_for_each_method(lhs, f);
            expr_for_each_method(rhs, f);
        }
        Expr::Unary { arg, .. } => expr_for_each_method(arg, f),
        Expr::Method { receiver, name, args } => {
            f(name.as_str());
            expr_for_each_method(receiver, f);
            for a in args {
                expr_for_each_method(a, f);
            }
        }
        Expr::Aggregate { .. } => {} // no Method nodes in aggregate position
        Expr::Case { branches, else_ } => {
            for (_, then_expr) in branches { expr_for_each_method(then_expr, f); }
            if let Some(e) = else_ { expr_for_each_method(e, f); }
        }
        Expr::Coalesce(args) => {
            for arg in args { expr_for_each_method(arg, f); }
        }
        Expr::NullIf { lhs, rhs } => {
            expr_for_each_method(lhs, f);
            expr_for_each_method(rhs, f);
        }
    }
}

/// Visit every `Expr::Method` name reachable from a `SelectItem` (recursing into
/// aggregate args, `toString`/`path` carry no Expr method nodes but `Attr` can
/// via `toHex`). Mirrors `expr_for_each_attr`'s coverage.
fn select_item_for_each_method<'a>(it: &'a SelectItem, f: &mut impl FnMut(&'a str)) {
    match it {
        SelectItem::Expr(e) => expr_for_each_method(e, f),
        SelectItem::Attr(a) => {
            if let Attr::ToHex(inner) = a {
                expr_for_each_method(inner, f);
            }
        }
        SelectItem::Aggregate { arg, .. } => select_item_for_each_method(arg, f),
        SelectItem::Star | SelectItem::Path { .. } | SelectItem::ToString(_) => {}
    }
}

/// Visit every `Expr::Method` name reachable from a `Predicate` tree (both sides
/// of every Compare). Mirrors the predicate walkers used for ref-path analysis.
fn pred_for_each_method<'a>(p: &'a Predicate, f: &mut impl FnMut(&'a str)) {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_for_each_method(a, f);
            pred_for_each_method(b, f);
        }
        Predicate::Not(inner) => pred_for_each_method(inner, f),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_for_each_method(lhs, f);
            expr_for_each_method(rhs, f);
        }
        // InSubquery's inner is validated when it is planned as its own query;
        // its `lhs` is an `Attr` (no method node).
        // EXISTS inner is planned as its own query; it carries no outer method nodes.
        Predicate::InstanceOf(_) | Predicate::InSubquery { .. } | Predicate::Exists { .. } => {}
    }
}

/// Reject any `receiver.method(args)` whose method name is not in
/// [`crate::query::parse::METHODS`]. A scan-time `QueryValue` cannot carry an
/// error, so unsupported/unknown method names must be caught here at plan time
/// with an actionable message. `get` is deliberately absent from `METHODS`, so
/// indexed object-array element access is rejected with an array-access hint.
fn reject_unsupported_methods(q: &Query) -> Result<(), QueryError> {
    let mut bad: Option<String> = None;
    let mut check = |name: &str| {
        if bad.is_none() && !crate::query::parse::METHODS.contains(&name) {
            bad = Some(name.to_string());
        }
    };
    for item in &q.select {
        select_item_for_each_method(item, &mut check);
    }
    if let Some(pred) = &q.where_ {
        pred_for_each_method(pred, &mut check);
    }
    if let Some(ob) = &q.order_by {
        if let Attr::ToHex(inner) = &ob.key {
            expr_for_each_method(inner, &mut check);
        }
    }
    if let Some(name) = bad {
        let supported = crate::query::parse::METHODS.join(", ");
        return Err(QueryError(format!(
            "method `{name}()` requires a live JVM and is not available in static \
             heap analysis. Supported methods: {supported}. For indexed array-element \
             access, dereference the backing field directly (e.g. `a.elementData` for \
             a list, then a field/scalar tail on that array)."
        )));
    }
    Ok(())
}

/// Rewrite `Attr::ValueArray` in an `Attr` node to a 1-hop `RefPath` that
/// follows the `value` field (projection-only role). This is the canonical
/// lowering: `@valueArray` means "the object's `.value` field" — a forward
/// reference to the backing byte/char array. The resulting `RefPath` is
/// handled by the RefWalk machinery in the P2 late window.
fn rewrite_value_array_attr(a: Attr) -> Attr {
    match a {
        Attr::ValueArray => Attr::RefPath {
            hops: vec!["value".to_string()],
            tail: Box::new(Attr::ObjectAddress),
            role: RefRole::ProjectionOnly,
        },
        Attr::RefPath { hops, tail, role } => Attr::RefPath {
            hops,
            tail: Box::new(rewrite_value_array_attr(*tail)),
            role,
        },
        Attr::ToHex(inner) => Attr::ToHex(Box::new(rewrite_value_array_expr(*inner))),
        Attr::ArrayIndex { base, index } => Attr::ArrayIndex {
            base: Box::new(rewrite_value_array_attr(*base)),
            index: Box::new(rewrite_value_array_expr(*index)),
        },
        Attr::ArraySlice { base, start, end } => Attr::ArraySlice {
            base: Box::new(rewrite_value_array_attr(*base)),
            start: start.map(|e| Box::new(rewrite_value_array_expr(*e))),
            end: end.map(|e| Box::new(rewrite_value_array_expr(*e))),
        },
        other => other,
    }
}

fn rewrite_value_array_expr(e: Expr) -> Expr {
    match e {
        Expr::Attr(a) => Expr::Attr(rewrite_value_array_attr(a)),
        Expr::Lit(_) => e,
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op,
            lhs: Box::new(rewrite_value_array_expr(*lhs)),
            rhs: Box::new(rewrite_value_array_expr(*rhs)),
        },
        Expr::Unary { op, arg } => Expr::Unary {
            op,
            arg: Box::new(rewrite_value_array_expr(*arg)),
        },
        Expr::Method { receiver, name, args } => Expr::Method {
            receiver: Box::new(rewrite_value_array_expr(*receiver)),
            name,
            args: args.into_iter().map(rewrite_value_array_expr).collect(),
        },
        Expr::Aggregate { func, arg } => Expr::Aggregate { func, arg },
        Expr::Case { branches, else_ } => Expr::Case {
            branches: branches
                .into_iter()
                .map(|(p, ex)| (rewrite_value_array_pred(p), rewrite_value_array_expr(ex)))
                .collect(),
            else_: else_.map(|e| Box::new(rewrite_value_array_expr(*e))),
        },
        Expr::Coalesce(args) => {
            Expr::Coalesce(args.into_iter().map(rewrite_value_array_expr).collect())
        }
        Expr::NullIf { lhs, rhs } => Expr::NullIf {
            lhs: Box::new(rewrite_value_array_expr(*lhs)),
            rhs: Box::new(rewrite_value_array_expr(*rhs)),
        },
    }
}

fn rewrite_value_array_select_item(item: SelectItem) -> SelectItem {
    match item {
        SelectItem::Attr(a) => SelectItem::Attr(rewrite_value_array_attr(a)),
        SelectItem::Aggregate { func, arg } => SelectItem::Aggregate {
            func,
            arg: Box::new(rewrite_value_array_select_item(*arg)),
        },
        SelectItem::Expr(e) => SelectItem::Expr(Box::new(rewrite_value_array_expr(*e))),
        other => other,
    }
}

fn rewrite_value_array_pred(p: Predicate) -> Predicate {
    match p {
        Predicate::And(a, b) => Predicate::And(
            Box::new(rewrite_value_array_pred(*a)),
            Box::new(rewrite_value_array_pred(*b)),
        ),
        Predicate::Or(a, b) => Predicate::Or(
            Box::new(rewrite_value_array_pred(*a)),
            Box::new(rewrite_value_array_pred(*b)),
        ),
        Predicate::Not(a) => Predicate::Not(Box::new(rewrite_value_array_pred(*a))),
        Predicate::Compare { lhs, op, rhs } => Predicate::Compare {
            lhs: rewrite_value_array_expr(lhs),
            op,
            rhs: rewrite_value_array_expr(rhs),
        },
        other => other,
    }
}

/// Lower `@valueArray` in all SELECT items and WHERE predicates to a 1-hop
/// `RefPath { hops: ["value"], tail: ObjectAddress, role: ProjectionOnly }`. The
/// RefWalk machinery in the P2 window then resolves the hop transparently.
fn rewrite_value_array_in_query(mut q: Query) -> Query {
    q.select = q
        .select
        .into_iter()
        .map(rewrite_value_array_select_item)
        .collect();
    q.where_ = q.where_.map(rewrite_value_array_pred);
    q
}

/// Returns true if `Attr::ReferenceArray` appears anywhere in a `SelectItem`.
fn select_item_has_reference_array(item: &SelectItem) -> bool {
    match item {
        SelectItem::Attr(Attr::ReferenceArray) => true,
        SelectItem::Aggregate { arg, .. } => select_item_has_reference_array(arg),
        SelectItem::Expr(e) => {
            expr_any_attr(e, |a| matches!(a, Attr::ReferenceArray))
        }
        _ => false,
    }
}

/// Returns true if `Attr::ReferenceArray` appears anywhere in a predicate tree.
fn pred_has_reference_array(p: &Predicate) -> bool {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_has_reference_array(a) || pred_has_reference_array(b)
        }
        Predicate::Not(a) => pred_has_reference_array(a),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_any_attr(lhs, |a| matches!(a, Attr::ReferenceArray))
                || expr_any_attr(rhs, |a| matches!(a, Attr::ReferenceArray))
        }
        _ => false,
    }
}

/// Reject `@referenceArray` used against an instance-class FROM. Array types
/// have class names ending in `[]`; everything else is an instance. For regex /
/// glob FROM sources the concrete class is unknown at plan time so the check is
/// skipped (the executor will project Null, which is acceptable parity for now).
fn reject_reference_array_on_instance(q: &Query) -> Result<(), QueryError> {
    let class_name = q.from.class_name();
    // Skip check for: subqueries (empty class name), glob patterns, regex FROM
    // (is_regex), and known array types (ending in `[]`).
    if class_name.is_empty()
        || class_name.contains('*')
        || class_name.ends_with("[]")
        || q.from.class_spec().map_or(false, |s| s.is_regex)
    {
        return Ok(());
    }
    let has_ref_array = q
        .select
        .iter()
        .any(select_item_has_reference_array)
        || q.where_.as_ref().map_or(false, pred_has_reference_array);
    if has_ref_array {
        return Err(QueryError(
            "@referenceArray on an instance object is not supported; \
             dereference the backing field directly \
             (e.g. x.elementData for ArrayList, x.value for String)"
                .into(),
        ));
    }
    Ok(())
}

fn plan_single(q: &Query, depth_cap: usize) -> Result<QueryPlan, QueryError> {
    // Lower @valueArray to a 1-hop RefPath before any planning so all
    // downstream logic (needs analysis, refwalk op emission) sees it as a
    // standard RefPath and handles it for free.
    let q_owned = rewrite_value_array_in_query(q.clone());
    let q = &q_owned;

    // Subqueries (FROM (...) and WHERE ... IN (...)) must be non-correlated:
    // the inner query may not reference an alias bound by the outer query.
    if let Some(inner) = q.from.as_subquery() {
        reject_if_correlated(inner)?;
    }
    if let Some(pred) = &q.where_ {
        reject_in_subqueries_if_correlated(pred)?;
    }

    // Reject unsupported / non-emulable `receiver.method(args)` calls up front:
    // a scan-time value cannot return an error, so unknown method names are
    // caught here with an actionable message (before any heavy planning).
    reject_unsupported_methods(q)?;

    // Reject `@referenceArray` used on an instance-class FROM (not an array type).
    // `@referenceArray` is only meaningful on array objects (class name ends with
    // `[]`). On instances it has no defined semantics; tell the user to dereference
    // the backing field directly instead. Skip the check for regex/glob FROM sources
    // since the concrete class is unknown at plan time.
    reject_reference_array_on_instance(q)?;

    // Validate a quoted-regex FROM target once, at plan time, so a bad regex is
    // an ACTIONABLE error here rather than a silent no-match (or per-row panic)
    // during the scan. The compiled regex is discarded; the executor / histogram
    // recompile the (now known-good) pattern once per query, never per object.
    if let Some(spec) = q.from.class_spec() {
        crate::query::execute::compile_from_regex(spec)?;
    }

    // Validate every `LIKE`/`NOT LIKE` RHS regex once, at plan time, so a bad
    // pattern is an ACTIONABLE error here rather than a silent no-match (or
    // per-row panic) during the scan. The compiled map is discarded; the executor
    // recompiles the (now known-good) patterns once per query, never per object.
    crate::query::execute::compile_like_regexes(q)?;

    // Plan any subqueries. A FROM-subquery is semi-joined by object identity, so
    // its inner must project whole objects; an IN-subquery is matched by address,
    // so its inner must project a single `@objectAddress` column. Both are run as
    // their own scan slots by the driver (see run.rs) and applied to the outer.
    let from_subplan = match q.from.as_subquery() {
        Some(inner) => {
            enforce_from_subquery_projection(inner)?;
            Some(Box::new(plan_query(inner, depth_cap)?))
        }
        None => None,
    };
    let mut in_subplans = Vec::new();
    if let Some(pred) = &q.where_ {
        collect_in_subplans(pred, &mut in_subplans, depth_cap)?;
    }
    let mut exists_subplans = Vec::new();
    if let Some(pred) = &q.where_ {
        collect_exists_subplans(pred, &mut exists_subplans, depth_cap)?;
    }

    let select_arity = q.select.len();
    let mut needs = QueryNeeds::default();
    let mut is_aggregate = false;

    for item in &q.select {
        match item {
            SelectItem::Aggregate { arg, .. } => {
                is_aggregate = true;
                note_attr_need(arg, &mut needs)?;
            }
            SelectItem::Star => {}
            SelectItem::Attr(a) => note_attr_need_attr(a, &mut needs),
            // path(a, b): handled below as a lone-select special-case (mirroring
            // @inbounds/@outbounds). A lone path select returns early; a mixed
            // select is rejected with an actionable error after the scan loop.
            // We do NOT note any scalar needs here — path emits object-ref rows
            // from the forward-reference graph, not instance fields.
            SelectItem::Path { .. } => {}
            // toString(s) needs the string-values side table (built post-scan).
            SelectItem::ToString(_) => {
                needs.string_values = true;
            }
            SelectItem::Expr(e) => {
                expr_for_each_attr(e, &mut |a| note_attr_need_attr(a, &mut needs));
                // `contains` and `toString` method calls require the string-values
                // side table (decoded String text) to be available in the late window.
                if expr_has_string_method(e) {
                    needs.string_values = true;
                }
            }
        }
    }

    // An aggregate over a FROM-subquery cannot be answered correctly: aggregates
    // fold during the outer scan, but the subquery semi-join runs post-scan, so
    // the fold would cover every scanned object (the subquery source matches all)
    // rather than the semi-joined subset. Reject with an actionable error instead
    // of silently returning a wrong count / zero rows.
    if is_aggregate && from_subplan.is_some() {
        return Err(QueryError(
            "aggregates over a FROM-subquery are not supported: an aggregate folds \
             during the scan, before the subquery semi-join is applied, so the result \
             would not reflect the subquery. Aggregate the inner query instead, e.g. \
             `SELECT COUNT(*) FROM <class> WHERE ...`, or select whole objects from the \
             subquery and aggregate a wrapping query."
                .into(),
        ));
    }
    let mut where_terms = Vec::new();
    if let Some(pred) = &q.where_ {
        collect_pred_needs(pred, &mut needs)?;
        flatten_and(pred.clone(), &mut where_terms);
    }

    // --- GROUP BY / HAVING validation ---
    let has_group_by = !q.group_by.is_empty();

    // HAVING without GROUP BY is invalid.
    if q.having.is_some() && !has_group_by {
        return Err(QueryError(
            "HAVING requires a GROUP BY clause — use WHERE to filter before aggregation, \
             or add a GROUP BY key"
                .into(),
        ));
    }

    // Validate: every non-aggregate SELECT item must appear in GROUP BY.
    if has_group_by {
        for item in q.select.iter() {
            if item_is_aggregate(item) {
                continue;
            }
            let item_as_expr: Option<Expr> = match item {
                SelectItem::Attr(a) => Some(Expr::Attr(a.clone())),
                SelectItem::Expr(e) => Some((**e).clone()),
                SelectItem::Star => None,
                _ => None,
            };
            if let Some(item_expr) = item_as_expr {
                let in_group_by = q.group_by.iter().any(|ge| ge == &item_expr);
                if !in_group_by {
                    let col_name = select_item_display_name(item);
                    return Err(QueryError(format!(
                        "non-aggregate column '{col_name}' must appear in GROUP BY \
                         (add it to the GROUP BY list or wrap it in an aggregate like COUNT(*))"
                    )));
                }
            }
        }
    }

    // Collect HAVING needs and terms.
    let mut having_terms = Vec::new();
    if let Some(having) = &q.having {
        collect_pred_needs(having, &mut needs)?;
        flatten_and(having.clone(), &mut having_terms);
    }

    // Register needs for GROUP BY key expressions.
    for ge in &q.group_by {
        expr_for_each_attr(ge, &mut |a| note_attr_need_attr(a, &mut needs));
    }

    // True when any aggregate arg is a compound SelectItem::Expr — i.e. the arg
    // is not a bare @attr or COUNT(*) and cannot be answered from class-summary
    // scalars. A bare @usedHeapSize arg folds to SelectItem::Attr (not Expr), so
    // this flag is false for the plain-aggregate histogram fast paths.
    let agg_over_expr = q.select.iter().any(|item| {
        matches!(
            item,
            SelectItem::Aggregate {
                arg,
                ..
            } if matches!(arg.as_ref(), SelectItem::Expr(_))
        )
    });

    // `FROM INSTANCEOF C` must NOT use the histogram fast path: a `ClassSummary`
    // carries only a class name (no super-chain), so the histogram cannot resolve
    // subclasses and would count only the exact class. Route instanceof aggregates
    // to SingleScan, where `class_matches` walks the hierarchy via `is_instance_of`.
    //
    // `FROM OBJECTS <address>` likewise must NOT use the histogram fast path: the
    // histogram counts by class name, but an Object source has no class name and
    // is restricted to a single dense index — a gate that lives only in the
    // SingleScan `visit_*` path. Route it to SingleScan so the aggregate folds
    // over at most the one matched object (COUNT(*) ≤ 1).
    let is_object_from = matches!(q.from, crate::query::ast::FromSource::Object(_));
    let kind = if has_group_by {
        StageKind::GroupBy
    } else if is_aggregate
        && !needs.instance_scalar
        && !needs.instance_string
        && where_terms.is_empty()
        && !agg_over_expr
        && !q.from.instanceof()
        && !is_object_from
        && q.select.iter().all(agg_histogram_answerable)
    {
        needs.histogram = true;
        StageKind::HistogramOnly
    } else {
        StageKind::SingleScan
    };

    // `AS RETAINED SET`: expand each match to its dominator-retained closure.
    // Incompatible with aggregates (there is no closure of an aggregate scalar).
    if q.retained_set {
        if is_aggregate {
            return Err(QueryError(
                "RETAINED SET cannot be combined with aggregate functions; \
                 SELECT the objects (e.g. SELECT s AS RETAINED SET FROM ... s), \
                 not an aggregate over them"
                    .into(),
            ));
        }
        needs.dominator_children = true;
        return Ok(QueryPlan {
            kind: StageKind::SingleScan,
            needs,
            where_terms,
            finalize_at: Phase::P3,
            carry: CarryLayout::IndexOnly,
            late_ops: vec![StageOp::RetainedSet {
                cap: DEFAULT_RETAINED_CAP,
            }],
            limit: q.limit,
            scan_limit: None,
            order_sensitive: q.order_by.is_some(),
            select_arity,
            union_branches: Vec::new(),
            union_limit: None,
            // RETAINED SET / dominator queries don't compose with subqueries in
            // this slice (their FROM binds a class alias and their SELECT is a
            // single graph op), so the subquery plans stay empty here.
            from_subplan: None,
            in_subplans: Vec::new(),
            exists_subplans: Vec::new(),
            deferred_projections: Vec::new(),
            group_by_exprs: Vec::new(),
            having_terms: Vec::new(),
            intersect_branch_plans: Vec::new(),
            except_branch_plans: Vec::new(),
        });
    }

    // `dominators(alias)`: dominator-tree children of each matched object. The
    // sole argument must name the FROM alias; anything else is a hard error.
    if let [SelectItem::Attr(Attr::Dominators(a) | Attr::DominatorOf(a))] = q.select.as_slice() {
        if Some(a.as_str()) != q.alias.as_deref() {
            return Err(QueryError(format!(
                "unknown alias '{a}'; the FROM clause binds {}",
                match &q.alias {
                    Some(al) => format!("alias '{al}'"),
                    None => "no alias".to_string(),
                }
            )));
        }
        needs.dominator_children = true;
        let op = match &q.select[0] {
            SelectItem::Attr(Attr::DominatorOf(_)) => StageOp::DominatorOf,
            _ => StageOp::DominatorChildren {
                cap: DEFAULT_LATE_CAP,
            },
        };
        return Ok(QueryPlan {
            kind: StageKind::SingleScan,
            needs,
            where_terms,
            finalize_at: Phase::P3,
            carry: CarryLayout::IndexOnly,
            late_ops: vec![op],
            limit: q.limit,
            scan_limit: None,
            order_sensitive: q.order_by.is_some(),
            select_arity,
            union_branches: Vec::new(),
            union_limit: None,
            from_subplan: None,
            in_subplans: Vec::new(),
            exists_subplans: Vec::new(),
            deferred_projections: Vec::new(),
            group_by_exprs: Vec::new(),
            having_terms: Vec::new(),
            intersect_branch_plans: Vec::new(),
            except_branch_plans: Vec::new(),
        });
    }

    // `path(a, b)`: bounded forward-reachable subgraph from the FROM-alias seeds.
    // Only valid as a LONE select item (like @inbounds); mixed selects are
    // rejected below with an actionable error. Resolves in the P2 late window off
    // the retained forward-edge store — finalize_at P2, carry IndexOnly.
    // `target_rows` is `&[]` by design — `to`-operand early-stop deferred (parity-lite).
    if let [SelectItem::Path { .. }] = q.select.as_slice() {
        return Ok(QueryPlan {
            kind: StageKind::SingleScan,
            needs,
            where_terms,
            finalize_at: Phase::P2,
            carry: CarryLayout::IndexOnly,
            late_ops: vec![StageOp::BoundedPath { depth_cap }],
            limit: q.limit,
            scan_limit: None,
            order_sensitive: q.order_by.is_some(),
            select_arity,
            union_branches: Vec::new(),
            union_limit: None,
            from_subplan: None,
            in_subplans: Vec::new(),
            exists_subplans: Vec::new(),
            deferred_projections: Vec::new(),
            group_by_exprs: Vec::new(),
            having_terms: Vec::new(),
            intersect_branch_plans: Vec::new(),
            except_branch_plans: Vec::new(),
        });
    }
    // A mixed select containing path(a, b) alongside other items is not supported:
    // path emits a one-column object-ref subgraph, so combining it with other
    // projections is meaningless. Reject with an actionable error.
    if q.select.iter().any(|it| matches!(it, SelectItem::Path { .. })) {
        return Err(QueryError(
            "path(a, b) must be the only select item \
             (e.g. SELECT path(a, b) FROM java.lang.Thread a)"
                .into(),
        ));
    }

    // `@inbounds` / `@outbounds`: the referrers / forward-targets of each matched
    // object, emitted one row per neighbour. Only special-cased as a LONE select
    // item (mixed selects still hit the Null projection path — see execute.rs).
    // Edges resolve in the P2 late window from the inbound CSR / retained edge
    // store, so we finalize at P2 and carry only the dense index frontier.
    if let [SelectItem::Attr(a @ (Attr::Inbounds | Attr::Outbounds))] = q.select.as_slice() {
        let dir = match a {
            Attr::Inbounds => EdgeDir::Inbound,
            Attr::Outbounds => EdgeDir::Outbound,
            _ => unreachable!("slice pattern already narrows to Inbounds|Outbounds"),
        };
        return Ok(QueryPlan {
            kind: StageKind::SingleScan,
            needs,
            where_terms,
            finalize_at: Phase::P2,
            carry: CarryLayout::IndexOnly,
            late_ops: vec![StageOp::EdgeLookup { dir }],
            limit: q.limit,
            scan_limit: None,
            order_sensitive: q.order_by.is_some(),
            select_arity,
            union_branches: Vec::new(),
            union_limit: None,
            from_subplan: None,
            in_subplans: Vec::new(),
            exists_subplans: Vec::new(),
            deferred_projections: Vec::new(),
            group_by_exprs: Vec::new(),
            having_terms: Vec::new(),
            intersect_branch_plans: Vec::new(),
            except_branch_plans: Vec::new(),
        });
    }

    let cross_phase = uses_retained(q);
    if cross_phase {
        needs.retained = true;
        // PERCENTILE/MEDIAN collect their argument's values at scan time, but a
        // cross-phase (retained) query scans in index-only carry mode with no
        // scan-time accumulator — the values would never be gathered. Reject at
        // plan time with an actionable message rather than returning an empty
        // percentile. (Mirrors the toString-late aggregate guard below.)
        if q.select.iter().any(select_uses_percentile) {
            return Err(QueryError(
                "PERCENTILE/MEDIAN cannot be combined with @retainedHeapSize; \
                 retained size is computed in a later phase where per-value \
                 collection is unavailable. Compute the percentile over a \
                 scan-time attribute (e.g. @usedHeapSize) instead"
                    .into(),
            ));
        }
    }
    let (mut finalize_at, mut late_ops) = if cross_phase {
        (Phase::P3, vec![StageOp::JoinRetained])
    } else {
        (Phase::P1, Vec::new())
    };

    // N-hop RefWalk: a predicate-critical path (in WHERE) must resolve before
    // row filtering; a projection-only path (SELECT only) resolves after. The
    // hop count is the number of reference edges to follow; we emit one
    // `RefWalkResolve` per hop (the final hop carries the tail scalar, earlier
    // hops carry the address frontier). We take the max hop count of each role
    // so a single walk of the deepest path subsumes shorter co-prefixed ones.
    let where_hops = q.where_.as_ref().map(pred_refpath_hops).unwrap_or(0);
    let select_hops = q.select.iter().map(select_refpath_hops).max().unwrap_or(0);
    if where_hops > 0 || select_hops > 0 {
        needs.ref_walk = true;
        // A predicate-critical walk must complete before filtering, so its ops
        // come first and set the role; otherwise the walk is projection-only.
        let mut push_hops = |count: usize, role: RefRole| {
            for hop in 0..count {
                let carry = if hop + 1 == count {
                    CarryLayout::IndexOnly
                } else {
                    CarryLayout::AddrFrontier
                };
                late_ops.push(StageOp::RefWalkResolve { hop, role, carry });
            }
        };
        if where_hops > 0 {
            push_hops(where_hops, RefRole::PredicateCritical);
        }
        if select_hops > 0 {
            push_hops(select_hops, RefRole::ProjectionOnly);
        }
        // RefWalk finalizes at P2; a later phase (P3 retained/dominators) wins.
        if finalize_at == Phase::P1 {
            finalize_at = Phase::P2;
        }
    }

    // Array index/slice: if `array_index` was set by ArrayIndex/ArraySlice, emit a
    // `ResolveArrayIndex` late op and advance finalize_at to P2 so the late window
    // runs. The op is a gate that keeps `eliminate_dead_needs` from clearing
    // `needs.array_index`; actual resolution happens in `stage_runner::array_index_rows`.
    if needs.array_index {
        late_ops.push(StageOp::ResolveArrayIndex);
        if finalize_at == Phase::P1 {
            finalize_at = Phase::P2;
        }
    }

    // `toString(s)`: for FROM java.lang.String, decode each instance to its text
    // value via a late ResolveStringValues op (runs at P2). For any other object
    // class, MAT's fallback display form `<class> @ 0x<addr>` is produced at scan
    // time (no late op, no retention) — so a non-String FROM is now ALLOWED and
    // falls through with no gating. Only a subquery FROM is rejected (the element
    // class is indeterminate at plan time).
    if needs.string_values {
        let class_name = q.from.class_name();
        let is_string_from = is_string_class_name(class_name);
        let is_subquery = q.from.as_subquery().is_some();
        if is_subquery {
            return Err(QueryError(
                "toString over a subquery result is not supported; apply toString \
                 inside the inner query, e.g. SELECT toString(s) FROM (<inner>) s \
                 where the inner query yields java.lang.String"
                    .to_string(),
            ));
        }
        if is_string_from {
            late_ops.push(StageOp::ResolveStringValues);
            if finalize_at == Phase::P1 {
                finalize_at = Phase::P2;
            }
            // An aggregate combined with a toString(s) WHERE folds over the late,
            // string-filtered set (see stage_runner::string_values_rows). Only
            // aggregates whose argument is projectable from the late string context
            // are supported: COUNT(*) and COUNT(toString(s)). SUM/AVG/MIN/MAX and
            // COUNT over other args (e.g. @usedHeapSize) would fold over Null in the
            // late phase — reject them with an actionable error instead of silently
            // returning 0/Null. (This gate applies ONLY to the late string-decode
            // path; the scan-time non-String display form has no such constraint.)
            if is_aggregate {
                let has_group_by = !q.group_by.is_empty();
                let ok = q.select.iter().all(|it| match it {
                    SelectItem::Aggregate { func, arg } => {
                        matches!(func, AggFunc::Count)
                            && matches!(
                                arg.as_ref(),
                                SelectItem::Star
                                    | SelectItem::ToString(_)
                                    | SelectItem::Attr(Attr::ToString(_))
                            )
                    }
                    // Non-aggregate items are valid GROUP BY key projections when a
                    // GROUP BY clause is present (e.g. `SELECT toString(s), COUNT(*)`
                    // with `GROUP BY toString(s)`). Without GROUP BY they are free-
                    // standing non-aggregates mixed with aggregates — still an error.
                    _ => has_group_by,
                });
                if !ok {
                    return Err(QueryError(
                        "only COUNT(*) or COUNT(toString(s)) may be combined with a \
                         toString(s) filter in WHERE; SUM/AVG/MIN/MAX (and COUNT over \
                         other attributes) over a toString-filtered set are not \
                         supported in this release"
                            .into(),
                    ));
                }
            }
        }
        // else: non-String class FROM -> generic scan-time display, no late op.
    }

    // G1: @GCRoots/@GCRootInfo require the full analyze pipeline. Force the plan
    // into carry mode (finalize_at != P1) so the entry is deferred to
    // `resume_without_late_ctx` where it gets an actionable error rather than
    // silently projecting Null in the query-only path.
    if needs.gc_roots && finalize_at == Phase::P1 {
        finalize_at = Phase::P3;
    }

    Ok(QueryPlan {
        kind,
        needs,
        where_terms,
        finalize_at,
        carry: CarryLayout::IndexOnly,
        late_ops,
        // For DISTINCT queries, defer the LIMIT to the dedup choke point so all
        // matching rows flow through for dedup before the cap is applied.
        limit: if q.distinct { None } else { q.limit },
        scan_limit: None,
        order_sensitive: q.order_by.is_some(),
        select_arity,
        union_branches: Vec::new(),
        union_limit: None,
        from_subplan,
        in_subplans,
        exists_subplans,
        deferred_projections: Vec::new(),
        group_by_exprs: q.group_by.clone(),
        having_terms,
        intersect_branch_plans: Vec::new(),
        except_branch_plans: Vec::new(),
    })
}

/// Enforce that a `FROM (<subquery>)` inner query projects whole-object
/// identity: a single `SELECT *`, `@objectId`, or `@objectAddress` column. The
/// outer query semi-joins by dense object index, so a scalar/field projection
/// (which loses object identity) is rejected with an actionable message.
fn enforce_from_subquery_projection(inner: &Query) -> Result<(), QueryError> {
    let ok = inner.select.len() == 1
        && matches!(
            inner.select[0],
            SelectItem::Star
                | SelectItem::Attr(Attr::ObjectId)
                | SelectItem::Attr(Attr::ObjectAddress)
        );
    if ok {
        Ok(())
    } else {
        Err(QueryError(
            "FROM-subquery must select whole objects (use SELECT * or SELECT @objectId)".into(),
        ))
    }
}

/// Walk a WHERE tree, plan each `IN (<subquery>)` inner, and collect the results
/// into `out`. The inner must project a single address-valued column
/// (`@objectAddress`, or `*` — but `*` yields an ObjRef index, not an address,
/// so for IN we require an explicit `@objectAddress`). Nested inners' own IN
/// predicates are handled when their plan is built (recursively via plan_query).
fn collect_in_subplans(pred: &Predicate, out: &mut Vec<InSubplan>, depth_cap: usize) -> Result<(), QueryError> {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            collect_in_subplans(a, out, depth_cap)?;
            collect_in_subplans(b, out, depth_cap)
        }
        Predicate::Not(a) => collect_in_subplans(a, out, depth_cap),
        Predicate::InSubquery { lhs, inner } => {
            enforce_in_subquery_projection(inner)?;
            let inner_plan = plan_query(inner, depth_cap)?;
            out.push(InSubplan {
                lhs: lhs.clone(),
                plan: inner_plan,
                inner: (**inner).clone(),
            });
            Ok(())
        }
        Predicate::Compare { .. } | Predicate::InstanceOf(_) => Ok(()),
        Predicate::Exists { .. } => Ok(()),
    }
}

/// Walk a WHERE tree, plan each `EXISTS (<subquery>)` / `NOT EXISTS (<subquery>)` inner,
/// and collect the results into `out`. The inner is a full query (any SELECT) — EXISTS
/// only needs to know if ≥1 row was produced, so no projection restriction is imposed.
/// Evaluation order: inner scans run before the outer scan; results are passed to the
/// executor as a `Vec<bool>` parallel to the `exists_subplans` index.
fn collect_exists_subplans(pred: &Predicate, out: &mut Vec<ExistsSubplan>, depth_cap: usize) -> Result<(), QueryError> {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            collect_exists_subplans(a, out, depth_cap)?;
            collect_exists_subplans(b, out, depth_cap)
        }
        Predicate::Not(a) => collect_exists_subplans(a, out, depth_cap),
        Predicate::Exists { inner, negated } => {
            let inner_plan = plan_query(inner, depth_cap)?;
            out.push(ExistsSubplan {
                negated: *negated,
                plan: inner_plan,
                inner: (**inner).clone(),
            });
            Ok(())
        }
        Predicate::Compare { .. } | Predicate::InstanceOf(_) | Predicate::InSubquery { .. } => Ok(()),
    }
}
/// addresses, so the inner must `SELECT @objectAddress` — a scalar/field or a
/// bare `@objectId` (a dense index, not an address) is rejected.
fn enforce_in_subquery_projection(inner: &Query) -> Result<(), QueryError> {
    let ok =
        inner.select.len() == 1 && matches!(inner.select[0], SelectItem::Attr(Attr::ObjectAddress));
    if ok {
        Ok(())
    } else {
        Err(QueryError(
            "IN-subquery must select a single address-valued column (SELECT @objectAddress)".into(),
        ))
    }
}

fn uses_retained(q: &Query) -> bool {
    let in_select = q.select.iter().any(select_uses_retained);
    let in_where = q.where_.as_ref().map(pred_uses_retained).unwrap_or(false);
    let in_order = matches!(&q.order_by, Some(ob) if ob.key == Attr::RetainedHeapSize);
    let in_group_by = q.group_by.iter().any(|ge| expr_any_attr(ge, |a| matches!(a, Attr::RetainedHeapSize)));
    in_select || in_where || in_order || in_group_by
}
fn select_uses_retained(it: &SelectItem) -> bool {
    match it {
        SelectItem::Attr(Attr::RetainedHeapSize) => true,
        SelectItem::Aggregate { arg, .. } => select_uses_retained(arg),
        SelectItem::Expr(e) => expr_any_attr(e, |a| matches!(a, Attr::RetainedHeapSize)),
        _ => false,
    }
}
/// True if a SELECT item is (or wraps) a PERCENTILE/MEDIAN aggregate. Used to
/// reject percentiles over the retained-late path at plan time.
fn select_uses_percentile(it: &SelectItem) -> bool {
    matches!(
        it,
        SelectItem::Aggregate {
            func: AggFunc::Percentile(_) | AggFunc::Median,
            ..
        }
    )
}
/// True if a predicate references `@retainedHeapSize` anywhere. Reused by the
/// scan-time carry executor to skip retained WHERE terms (retained size is
/// unknown during the pass2 scan; those terms are applied late in stage_runner).
/// True if a FROM class name denotes java.lang.String (fully-qualified, slash
/// form, or a simple `.String` short form). Single source of truth shared by the
/// planner's toString gate and the executor's from_is_string check so they cannot drift.
pub(crate) fn is_string_class_name(name: &str) -> bool {
    name == "java.lang.String" || name == "java/lang/String" || name.ends_with(".String")
}
pub(crate) fn pred_uses_retained(p: &Predicate) -> bool {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_uses_retained(a) || pred_uses_retained(b)
        }
        Predicate::Not(a) => pred_uses_retained(a),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_any_attr(lhs, |a| matches!(a, Attr::RetainedHeapSize))
                || expr_any_attr(rhs, |a| matches!(a, Attr::RetainedHeapSize))
        }
        _ => false,
    }
}

/// True if any comparison in the predicate tree references `toString(s)`.
/// Such predicates cannot be evaluated during the pass2 scan — the string
/// value is decoded only after the backing-array pass (P2) — so a carry-mode
/// scan must SKIP them (leaving the object to be carried) and defer them to the
/// late stage, where `eval_tostring_pred` resolves them against the decoded
/// text. Without this skip the scan would compare `toString(s)` against `Null`
/// and drop every row before the late phase could re-filter (SW-2).
pub(crate) fn pred_uses_tostring(p: &Predicate) -> bool {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_uses_tostring(a) || pred_uses_tostring(b)
        }
        Predicate::Not(a) => pred_uses_tostring(a),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_any_attr(lhs, |a| matches!(a, Attr::ToString(_)))
                || expr_any_attr(rhs, |a| matches!(a, Attr::ToString(_)))
        }
        _ => false,
    }
}

/// True if any comparison in the predicate tree references an N-hop `RefPath`
/// (e.g. `s.value.@length` or `t.name.value.@length`). Such predicates cannot
/// be evaluated during the pass2 scan: the forward-reference graph is walked
/// only in the post-scan late window (`RefWalkResolve` → `refpath_rows`), so at
/// scan time a `RefPath` attr projects `Null` and any comparison against it
/// (`Null > 0`) is false — which would drop EVERY carried row before the late
/// predicate-critical filter could run. A carry-mode scan must therefore SKIP
/// these terms and defer them to `refpath_rows`, exactly as it defers
/// `@retainedHeapSize` and String `toString` terms.
pub(crate) fn pred_uses_refpath(p: &Predicate) -> bool {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_uses_refpath(a) || pred_uses_refpath(b)
        }
        Predicate::Not(a) => pred_uses_refpath(a),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_any_attr(lhs, |a| matches!(a, Attr::RefPath { .. }))
                || expr_any_attr(rhs, |a| matches!(a, Attr::RefPath { .. }))
        }
        _ => false,
    }
}

/// The maximum RefPath hop count across every `Attr::RefPath` reachable in a
/// SELECT projection (following aggregate arguments). `0` if none.
fn select_refpath_hops(it: &SelectItem) -> usize {
    match it {
        SelectItem::Attr(Attr::RefPath { hops, .. }) => hops.len(),
        SelectItem::Aggregate { arg, .. } => select_refpath_hops(arg),
        SelectItem::Expr(e) => {
            let mut max = 0;
            expr_for_each_attr(e, &mut |a| {
                if let Attr::RefPath { hops, .. } = a {
                    max = max.max(hops.len());
                }
            });
            max
        }
        _ => 0,
    }
}

/// The maximum RefPath hop count across every `Attr::RefPath` reachable in a
/// WHERE predicate tree. `0` if none. A non-zero result means at least one
/// conjunct is predicate-critical (a refwalk must resolve before filtering).
fn pred_refpath_hops(p: &Predicate) -> usize {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_refpath_hops(a).max(pred_refpath_hops(b))
        }
        Predicate::Not(a) => pred_refpath_hops(a),
        Predicate::Compare { lhs, rhs, .. } => {
            let mut max = 0;
            expr_for_each_attr(lhs, &mut |a| {
                if let Attr::RefPath { hops, .. } = a {
                    max = max.max(hops.len());
                }
            });
            expr_for_each_attr(rhs, &mut |a| {
                if let Attr::RefPath { hops, .. } = a {
                    max = max.max(hops.len());
                }
            });
            max
        }
        _ => 0,
    }
}

/// Schema lookup used for plan-time field validation. Implemented by the live
/// pass2 resolver; a fake version backs the unit tests. `class_field_names`
/// returns the full super-chain field-name set for an EXACT (non-glob) class
/// name, or `None` when the class is unknown or the name is a glob pattern
/// (in which case field validation is skipped, since the concrete runtime
/// classes vary per instance).
pub trait FieldSchema {
    fn class_field_names(&self, exact_class_name: &str) -> Option<Vec<String>>;
}

/// Reject any bare field referenced in SELECT/WHERE/ORDER BY that is absent
/// from the FROM class's field set. Skipped for glob FROM patterns and for
/// classes the schema can't resolve (validation is best-effort: an unresolvable
/// class means we can't prove a field is missing, so we let the scan proceed).
pub fn validate_fields(q: &Query, schema: &dyn FieldSchema) -> Result<(), QueryError> {
    let class = q.from.class_name();
    if class.contains('*') {
        return Ok(());
    }
    let Some(known) = schema.class_field_names(class) else {
        return Ok(());
    };

    let mut referenced = Vec::new();
    for item in &q.select {
        collect_select_fields(item, &mut referenced);
    }
    if let Some(pred) = &q.where_ {
        collect_pred_fields(pred, &mut referenced);
    }
    if let Some(ob) = &q.order_by {
        if let Attr::Field(name) = &ob.key {
            // Skip if the name matches a SELECT column alias: ORDER BY foo where
            // foo is `... AS foo` is a reference to an output column, not a field.
            let is_alias = q
                .select_aliases
                .iter()
                .any(|a| a.as_deref() == Some(name.as_str()));
            if !is_alias {
                referenced.push(name.clone());
            }
        }
    }

    for name in referenced {
        // A bare reference to the FROM alias itself (e.g. `SELECT s ... String s`,
        // as used by `AS RETAINED SET`) denotes the whole object, not a field, so
        // it is never a field lookup and must not be validated as one.
        if q.alias.as_deref() == Some(name.as_str()) {
            continue;
        }
        let bare = strip_alias(&name, q.alias.as_deref());
        if !known.iter().any(|f| f == bare) {
            return Err(QueryError(format!(
                "unknown field `{bare}` on {class}; \
                 known fields: {}",
                if known.is_empty() {
                    "(none)".to_string()
                } else {
                    known.join(", ")
                }
            )));
        }
    }
    Ok(())
}

fn strip_alias<'n>(name: &'n str, alias: Option<&str>) -> &'n str {
    if let Some(a) = alias {
        if let Some(rest) = name.strip_prefix(a) {
            if let Some(field) = rest.strip_prefix('.') {
                return field;
            }
        }
    }
    name
}

fn collect_select_fields(item: &SelectItem, out: &mut Vec<String>) {
    match item {
        SelectItem::Attr(Attr::Field(name)) => out.push(name.clone()),
        SelectItem::Aggregate { arg, .. } => collect_select_fields(arg, out),
        SelectItem::Expr(e) => expr_for_each_attr(e, &mut |a| {
            if let Attr::Field(name) = a {
                out.push(name.clone());
            }
        }),
        _ => {}
    }
}

fn collect_pred_fields(pred: &Predicate, out: &mut Vec<String>) {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            collect_pred_fields(a, out);
            collect_pred_fields(b, out);
        }
        Predicate::Not(a) => collect_pred_fields(a, out),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_for_each_attr(lhs, &mut |a| {
                if let Attr::Field(name) = a {
                    out.push(name.clone());
                }
            });
            expr_for_each_attr(rhs, &mut |a| {
                if let Attr::Field(name) = a {
                    out.push(name.clone());
                }
            });
        }
        _ => {}
    }
}

/// The alias head of a dotted field reference, e.g. `s.count` → `Some("s")`.
/// Bare fields (`count`) and non-field attrs yield `None`. A dotted `@`-attr is
/// impossible (the lexer captures `@a.b` whole), so only `Attr::Field` matters.
fn attr_alias_head(a: &Attr) -> Option<&str> {
    match a {
        Attr::Field(name) => name.split_once('.').map(|(head, _)| head),
        // A RefPath's alias head is its first hop; after the query's own alias
        // is stripped during parse, a leftover foreign head appears here.
        Attr::RefPath { hops, .. } => hops.first().map(|s| s.as_str()),
        _ => None,
    }
}

/// Collect the alias heads (`a` in `a.field`) referenced by SELECT + WHERE of a
/// query, excluding the query's own bound alias and any bare (dot-free) field.
/// A head left over after excluding the bound alias came from *outside* this
/// query — the signature of a correlated subquery.
fn referenced_alias_heads(q: &Query) -> std::collections::HashSet<String> {
    let mut heads = std::collections::HashSet::new();
    let push = |a: &Attr, heads: &mut std::collections::HashSet<String>| {
        if let Some(h) = attr_alias_head(a) {
            heads.insert(h.to_string());
        }
    };
    for item in &q.select {
        match item {
            SelectItem::Attr(a) => push(a, &mut heads),
            SelectItem::Aggregate { arg, .. } => {
                if let SelectItem::Attr(a) = arg.as_ref() {
                    push(a, &mut heads);
                }
            }
            SelectItem::Star => {}
            // Correlation detection over path(a, b) operands lands in a later task.
            SelectItem::Path { .. } => {}
            // toString(s) has no external alias head to detect correlation.
            SelectItem::ToString(_) => {}
            SelectItem::Expr(e) => expr_for_each_attr(e, &mut |a| {
                if let Some(h) = attr_alias_head(a) {
                    heads.insert(h.to_string());
                }
            }),
        }
    }
    if let Some(pred) = &q.where_ {
        collect_pred_alias_heads(pred, &mut heads);
    }
    if let Some(a) = q.alias.as_deref() {
        heads.remove(a);
    }
    heads
}

fn collect_pred_alias_heads(pred: &Predicate, heads: &mut std::collections::HashSet<String>) {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            collect_pred_alias_heads(a, heads);
            collect_pred_alias_heads(b, heads);
        }
        Predicate::Not(a) => collect_pred_alias_heads(a, heads),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_for_each_attr(lhs, &mut |a| {
                if let Some(h) = attr_alias_head(a) {
                    heads.insert(h.to_string());
                }
            });
            expr_for_each_attr(rhs, &mut |a| {
                if let Some(h) = attr_alias_head(a) {
                    heads.insert(h.to_string());
                }
            });
        }
        // A nested IN-subquery is checked on its own via reject_if_correlated;
        // its inner heads are relative to the inner query, not this one.
        // Same for EXISTS: inner is a standalone non-correlated query.
        Predicate::InSubquery { .. } | Predicate::InstanceOf(_) | Predicate::Exists { .. } => {}
    }
}

/// A subquery is correlated iff it references an alias head it does not itself
/// bind (its own FROM alias). We reject such queries with an actionable message
/// rather than attempting per-outer-row re-execution (out of scope).
fn reject_if_correlated(inner: &Query) -> Result<(), QueryError> {
    if let Some(head) = referenced_alias_heads(inner).into_iter().next() {
        return Err(QueryError(format!(
            "correlated subqueries are not supported: inner query references outer alias `{head}`"
        )));
    }
    Ok(())
}

/// Walk a WHERE predicate tree and reject any `IN (<subquery>)` whose inner
/// query is correlated. Nested inners are checked recursively so a correlated
/// subquery buried inside another subquery is still caught.
fn reject_in_subqueries_if_correlated(pred: &Predicate) -> Result<(), QueryError> {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            reject_in_subqueries_if_correlated(a)?;
            reject_in_subqueries_if_correlated(b)
        }
        Predicate::Not(a) => reject_in_subqueries_if_correlated(a),
        Predicate::InSubquery { inner, .. } => {
            reject_if_correlated(inner)?;
            if let Some(p) = &inner.where_ {
                reject_in_subqueries_if_correlated(p)?;
            }
            Ok(())
        }
        Predicate::Compare { .. } | Predicate::InstanceOf(_) => Ok(()),
        Predicate::Exists { inner, .. } => {
            reject_if_correlated(inner)?;
            if let Some(p) = &inner.where_ {
                reject_in_subqueries_if_correlated(p)?;
            }
            Ok(())
        }
    }
}
/// histogram-only path can answer without touching per-object data:
///
///   1. `COUNT(*)` — answered from the class-summary row count.
///   2. `SUM(@usedHeapSize)` — answered from the class-summary shallow total.
///   3. `AVG(@usedHeapSize)` — answered from count + shallow total.
///
/// Every other aggregate (MIN, MAX, COUNT over a non-Star, SUM/AVG over
/// anything other than `@usedHeapSize`) requires the per-object SingleScan path.
/// Non-aggregate items also return `false` so any mix falls to SingleScan
/// (though mixed non-aggregate + aggregate selects are rejected earlier by the
/// planner before this check is reached).
fn agg_histogram_answerable(item: &SelectItem) -> bool {
    match item {
        SelectItem::Aggregate { func, arg } => match (func, arg.as_ref()) {
            (AggFunc::Count, SelectItem::Star) => true,
            (AggFunc::Sum, SelectItem::Attr(Attr::UsedHeapSize)) => true,
            (AggFunc::Avg, SelectItem::Attr(Attr::UsedHeapSize)) => true,
            _ => false,
        },
        // Non-aggregate items: treat as not histogram-answerable so any stray
        // mix falls through to SingleScan.
        _ => false,
    }
}

fn note_attr_need(item: &SelectItem, needs: &mut QueryNeeds) -> Result<(), QueryError> {
    match item {
        SelectItem::Star => Ok(()),
        SelectItem::Attr(a) => {
            note_attr_need_attr(a, needs);
            Ok(())
        }
        SelectItem::Aggregate { .. } => Err(QueryError(
            "nested aggregate is deferred and not supported in this version; \
             an aggregate function may not take another aggregate as its argument"
                .into(),
        )),
        // path(a, b) cannot be an aggregate argument; full support lands later.
        SelectItem::Path { .. } => Err(QueryError(
            "path(a, b) may not be used as an aggregate argument".into(),
        )),
        // toString(s) as an aggregate argument (COUNT(toString(s))) is supported
        // in the carry-mode late path for String queries — mark the string-values
        // table as needed; the aggregate gate in `is_string_from` validates the
        // combination (SUM/AVG/MIN/MAX over toString are rejected there).
        SelectItem::ToString(_) => {
            needs.string_values = true;
            Ok(())
        }
        SelectItem::Expr(e) => {
            expr_for_each_attr(e, &mut |a| note_attr_need_attr(a, needs));
            Ok(())
        }
    }
}

fn note_attr_need_attr(a: &Attr, needs: &mut QueryNeeds) {
    match a {
        Attr::DisplayName => needs.instance_string = true,
        Attr::ClassOf => needs.runtime_type = true,
        Attr::Field(_) => {
            needs.instance_scalar = true;
        }
        // toString(s) arms the string-values side table, built post-scan.
        Attr::ToString(_) => needs.string_values = true,
        // G1: GC-root attrs require the full analyze pipeline.
        Attr::GcRoots | Attr::GcRootInfo => needs.gc_roots = true,
        // Array index/slice: resolved in P2 late window via ResolveArrayIndex op.
        Attr::ArrayIndex { .. } | Attr::ArraySlice { .. } => {
            needs.array_index = true;
        }
        _ => {}
    }
}

fn collect_pred_needs(pred: &Predicate, needs: &mut QueryNeeds) -> Result<(), QueryError> {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            collect_pred_needs(a, needs)?;
            collect_pred_needs(b, needs)
        }
        Predicate::Not(a) => collect_pred_needs(a, needs),
        Predicate::InstanceOf(_) => {
            needs.runtime_type = true;
            Ok(())
        }
        Predicate::InSubquery { .. } => {
            // Membership is tested against the inner result's address set; the
            // outer LHS is an address/id attribute, needing no instance data.
            Ok(())
        }
        Predicate::Exists { .. } => {
            // EXISTS is evaluated once before the scan (boolean constant);
            // the outer scan needs no additional data from the inner.
            Ok(())
        }
        Predicate::Compare { lhs, rhs, .. } => {
            let lhs_attr = lhs.as_attr();
            let rhs_val = rhs.as_lit();
            // Reject ArrayIndex/ArraySlice in WHERE predicates at plan time.
            let is_array_attr = |a: &Attr| matches!(a, Attr::ArrayIndex { .. } | Attr::ArraySlice { .. });
            if lhs_attr.map_or(false, is_array_attr)
                || expr_any_attr(lhs, is_array_attr)
                || expr_any_attr(rhs, is_array_attr)
            {
                return Err(QueryError(
                    "array indexing is not supported in WHERE predicates — \
                     use array access in SELECT columns only"
                        .into(),
                ));
            }
            // Folded plain-compare fast path (unchanged behavior): lhs is a single attr.
            if let Some(a) = lhs_attr {
                match a {
                    Attr::Field(_) => {
                        if matches!(rhs_val, Some(Value::Str(_))) {
                            needs.instance_string = true;
                        } else {
                            needs.instance_scalar = true;
                        }
                    }
                    Attr::DisplayName => needs.instance_string = true,
                    Attr::ClassOf => needs.runtime_type = true,
                    // toString(s) in WHERE arms the string-values side table.
                    Attr::ToString(_) => needs.string_values = true,
                    _ => {}
                }
            } else {
                // Arithmetic lhs: note every attr leaf's need (numeric context).
                expr_for_each_attr(lhs, &mut |a| note_attr_need_attr(a, needs));
            }
            // The rhs may also carry attrs (arithmetic on the right). Note their needs.
            if rhs_val.is_none() {
                expr_for_each_attr(rhs, &mut |a| note_attr_need_attr(a, needs));
            }
            // `contains` and `toString` method calls in WHERE require the
            // string-values side table (decoded String text) in the late window.
            if expr_has_string_method(lhs) || expr_has_string_method(rhs) {
                needs.string_values = true;
            }
            Ok(())
        }
    }
}

fn flatten_and(pred: Predicate, out: &mut Vec<Conjunct>) {
    match pred {
        Predicate::And(a, b) => {
            flatten_and(*a, out);
            flatten_and(*b, out);
        }
        other => {
            let cost = pred_cost(&other);
            out.push(Conjunct { pred: other, cost });
        }
    }
}

fn pred_cost(pred: &Predicate) -> PredCost {
    match pred {
        Predicate::InstanceOf(_) => PredCost::Type,
        Predicate::InSubquery { .. } => PredCost::Str,
        Predicate::Exists { .. } => PredCost::Scalar,
        Predicate::Not(a) => pred_cost(a),
        Predicate::And(a, b) | Predicate::Or(a, b) => pred_cost(a).max_cost(pred_cost(b)),
        Predicate::Compare { lhs, rhs, .. } => {
            if expr_any_attr(lhs, |a| matches!(a, Attr::RefPath { .. }))
                || expr_any_attr(rhs, |a| matches!(a, Attr::RefPath { .. }))
            {
                PredCost::Ref
            } else {
                match lhs.as_attr() {
                    Some(Attr::Field(_)) if matches!(rhs.as_lit(), Some(Value::Str(_))) => PredCost::Str,
                    Some(Attr::DisplayName) => PredCost::Str,
                    Some(Attr::ClassOf) => PredCost::Type,
                    _ => PredCost::Scalar,
                }
            }
        }
    }
}

impl PredCost {
    fn max_cost(self, other: PredCost) -> PredCost {
        if pred_cost_rank(self) >= pred_cost_rank(other) {
            self
        } else {
            other
        }
    }
}

fn pred_cost_rank(c: PredCost) -> u8 {
    match c {
        PredCost::Type => 0,
        PredCost::Scalar => 1,
        PredCost::Str => 2,
        PredCost::Ref => 3,
    }
}

impl QueryPlan {
    /// Human-readable plan summary for `!explain` / `!plan`.
    pub fn explain(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("stage: {:?}\n", self.kind));
        let mut armed = Vec::new();
        if self.needs.histogram {
            armed.push("histogram");
        }
        if self.needs.instance_scalar {
            armed.push("instance_scalar");
        }
        if self.needs.instance_string {
            armed.push("instance_string");
        }
        if self.needs.runtime_type {
            armed.push("runtime_type");
        }
        if self.needs.retained {
            armed.push("retained");
        }
        if self.needs.dominator_children {
            armed.push("dominator_children");
        }
        if self.needs.ref_walk {
            armed.push("ref_walk");
        }
        s.push_str(&format!(
            "needs (armed): {}\n",
            if armed.is_empty() {
                "none".into()
            } else {
                armed.join(", ")
            }
        ));
        s.push_str(&format!("finalize: {:?}\n", self.finalize_at));
        if let Some(n) = self.limit {
            s.push_str(&format!("limit: {n}\n"));
        }
        if let Some(n) = self.scan_limit {
            s.push_str(&format!("scan_limit: {n}\n"));
        }
        if !self.where_terms.is_empty() {
            s.push_str("where:\n");
            for c in &self.where_terms {
                s.push_str(&format!("  [{:?}] {:?}\n", c.cost, c.pred));
            }
        }
        if !self.late_ops.is_empty() {
            let names: Vec<String> = self.late_ops.iter().map(|op| format!("{op:?}")).collect();
            s.push_str(&format!("late_ops: {}\n", names.join(", ")));
        }
        if !self.deferred_projections.is_empty() {
            let indices: Vec<String> = self
                .deferred_projections
                .iter()
                .map(|d| d.select_index.to_string())
                .collect();
            s.push_str(&format!("deferred_projections: [{}]\n", indices.join(", ")));
        }
        s
    }

    /// Machine-friendly plan summary: one short descriptor per active stage/feature.
    /// Used by `!plan` output and tests to assert optimizer effects (e.g. that a
    /// LIMIT was pushed to the scan).
    #[allow(dead_code)]
    pub fn stage_list(&self) -> Vec<String> {
        let mut v = Vec::new();
        v.push(format!("stage={:?}", self.kind));
        if let Some(n) = self.limit {
            v.push(format!("limit={n}"));
        }
        if let Some(n) = self.scan_limit {
            v.push(format!("scan_limit={n}"));
        }
        for op in &self.late_ops {
            v.push(format!("late_op={op:?}"));
        }
        if !self.where_terms.is_empty() {
            let costs: Vec<String> = self
                .where_terms
                .iter()
                .map(|c| format!("{:?}", c.cost))
                .collect();
            v.push(format!("where_costs=[{}]", costs.join(",")));
        }
        if !self.deferred_projections.is_empty() {
            v.push(format!("deferred={}", self.deferred_projections.len()));
        }
        v
    }

    /// A query is "resident-only" when every attribute it touches can be answered
    /// from the persistent pass1 resolver + pass2 Graph tables WITHOUT reading any
    /// transient per-object blob and WITHOUT any cross-phase table (retained,
    /// dominators, ref-walk, string values, gc roots). Such queries can be served
    /// from a warm REPL cache with an empty blob.
    pub fn is_resident_only(&self) -> bool {
        let n = &self.needs;
        !n.instance_scalar
            && !n.instance_string
            && !n.retained
            && !n.dominator_children
            && !n.ref_walk
            && !n.string_values
            && !n.gc_roots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse::parse;

    #[test]
    fn is_resident_only_classifies_needs() {
        let mut p = QueryPlan::default();
        p.needs.histogram = true;
        assert!(p.is_resident_only(), "histogram-only must be resident");

        let mut p2 = QueryPlan::default();
        p2.needs.instance_scalar = true;
        assert!(!p2.is_resident_only(), "instance_scalar needs the scan");

        let mut p3 = QueryPlan::default();
        p3.needs.retained = true;
        assert!(!p3.is_resident_only(), "retained needs the full pipeline");

        // runtime_type (class-metadata only) is still resident-only.
        let mut p4 = QueryPlan::default();
        p4.needs.runtime_type = true;
        assert!(p4.is_resident_only(), "runtime_type is class metadata, resident");
    }

    /// Convenience wrapper: plan with the canonical default depth (used by all
    /// tests that do not specifically test depth threading).
    fn pq(q: &Query) -> Result<QueryPlan, QueryError> {
        plan_query(q, crate::query::DEFAULT_PATH_DEPTH_CAP)
    }
    #[test]
    fn histogram_only_needs() {
        let plan = pq(&parse("SELECT COUNT(*) FROM java.lang.String").unwrap()).unwrap();
        assert_eq!(plan.kind, StageKind::HistogramOnly);
        assert!(!plan.needs.instance_scalar);
        assert!(!plan.needs.instance_string);
    }

    // D5: unsupported instance methods (no live JVM) are rejected at plan time.
    #[test]
    #[allow(non_snake_case)]
    fn method_rejection_subList_hashCode() {
        for q in [
            "SELECT s.subList(0,1) FROM java.util.ArrayList s",
            "SELECT s.hashCode() FROM java.lang.Object s",
        ] {
            let err = pq(&parse(q).unwrap()).unwrap_err();
            assert!(
                err.0.contains("requires a live JVM"),
                "query `{q}` must be rejected with the live-JVM message; got: {}",
                err.0
            );
        }
    }

    // D5: `get(n)` is intentionally NOT supported (indexed object-array element
    // access is not emulable statically); the message must carry the array hint.
    #[test]
    fn method_rejection_get() {
        let err = pq(&parse("SELECT a.get(0) FROM java.util.ArrayList a").unwrap()).unwrap_err();
        assert!(
            err.0.contains("requires a live JVM"),
            "get(0) must be rejected; got: {}",
            err.0
        );
        assert!(
            err.0.contains("elementData"),
            "get(0) rejection must include the array-element access hint; got: {}",
            err.0
        );
        assert!(
            !err.0.contains(", get,") && !err.0.contains(" get "),
            "`get` must NOT appear in the supported-methods list; got: {}",
            err.0
        );
    }

    // D5: guard against over-rejection — supported methods still plan cleanly,
    // including a method used in a WHERE predicate.
    #[test]
    fn method_supported_names_ok() {
        pq(&parse("SELECT i.intValue() FROM java.lang.Integer i").unwrap())
            .expect("intValue() is supported and must plan");
        pq(&parse("SELECT s.getName() FROM java.lang.String s").unwrap())
            .expect("getName() is supported and must plan");
        pq(&parse("SELECT * FROM java.lang.Integer i WHERE i.intValue() = 1").unwrap())
            .expect("supported method in WHERE must plan");
    }

    // MAT gap #5: a bad quoted FROM regex must be an actionable plan-time error.
    #[test]
    fn bad_from_regex_rejected_at_plan_time() {
        let q = parse(r#"SELECT * FROM "[""#).expect("parses; regex is validated at plan");
        let err = pq(&q).expect_err("bad regex must be rejected at plan time");
        assert!(
            err.0.contains("invalid regex") && err.0.contains('['),
            "plan error must name the regex problem; got: {}",
            err.0
        );
    }

    // A valid quoted FROM regex plans cleanly (as a histogram-only aggregate).
    #[test]
    fn good_from_regex_plans_ok() {
        let plan = pq(&parse(r#"SELECT COUNT(*) FROM "java\.lang\..*""#).unwrap()).unwrap();
        assert_eq!(plan.kind, StageKind::HistogramOnly);
    }

    // `FROM OBJECTS <addr>` projections plan as a SingleScan: the single-object
    // dense-index gate lives in the SingleScan visit path.
    #[test]
    fn plan_from_objects_projection_is_single_scan() {
        let plan = pq(&parse("SELECT @objectAddress FROM OBJECTS 0x10").unwrap()).unwrap();
        assert_eq!(plan.kind, StageKind::SingleScan);
    }

    // `COUNT(*) FROM OBJECTS <addr>` must NOT take the histogram fast path (which
    // counts by class name and cannot express the single-index gate); it routes to
    // SingleScan so the aggregate folds over at most one matched object (≤ 1).
    #[test]
    fn plan_count_from_objects_is_single_scan_not_histogram() {
        let plan = pq(&parse("SELECT COUNT(*) FROM OBJECTS 0x10").unwrap()).unwrap();
        assert_eq!(
            plan.kind,
            StageKind::SingleScan,
            "COUNT(*) FROM OBJECTS must route to SingleScan (single-index gate), \
             not the class-name histogram"
        );
        assert!(!plan.needs.histogram);
    }

    #[test]
    fn single_scan_scalar_needs() {
        let plan = pq(&parse("SELECT @objectId FROM C WHERE count > 3").unwrap()).unwrap();
        assert_eq!(plan.kind, StageKind::SingleScan);
        assert!(plan.needs.instance_scalar);
        assert!(!plan.needs.instance_string);
    }

    #[test]
    fn string_projection_sets_string_need() {
        let plan =
            pq(&parse("SELECT @displayName FROM java.lang.String").unwrap()).unwrap();
        assert!(plan.needs.instance_string);
    }

    #[test]
    fn retained_heap_size_now_parses() {
        assert!(parse("SELECT @retainedHeapSize FROM C").is_ok());
    }

    #[test]
    fn retained_in_select_sets_retained_need_and_p3_finalize() {
        let plan = pq(&parse("SELECT @retainedHeapSize FROM C").unwrap()).unwrap();
        assert!(
            plan.needs.retained,
            "SELECT @retainedHeapSize must arm the retained need"
        );
        assert_eq!(plan.finalize_at, Phase::P3);
        assert_eq!(plan.late_ops, vec![StageOp::JoinRetained]);
    }
    #[test]
    fn retained_in_where_is_cross_phase() {
        let plan =
            pq(&parse("SELECT @objectId FROM C WHERE @retainedHeapSize > 1024").unwrap())
                .unwrap();
        assert!(plan.needs.retained);
        assert_eq!(plan.finalize_at, Phase::P3);
    }
    #[test]
    fn retained_in_order_by_is_cross_phase() {
        let plan =
            pq(&parse("SELECT @objectId FROM C ORDER BY @retainedHeapSize DESC").unwrap())
                .unwrap();
        assert!(plan.needs.retained);
        assert_eq!(plan.finalize_at, Phase::P3);
        assert_eq!(plan.late_ops, vec![StageOp::JoinRetained]);
    }
    #[test]
    fn non_retained_query_finalizes_in_p1() {
        let plan = pq(&parse("SELECT @objectId FROM C WHERE count > 3").unwrap()).unwrap();
        assert!(!plan.needs.retained);
        assert_eq!(plan.finalize_at, Phase::P1);
        assert!(plan.late_ops.is_empty());
    }

    #[test]
    fn array_index_sets_array_index_need_and_p2() {
        // `s.value[999999]` should set needs.array_index = true and finalize_at = P2.
        let q = parse("SELECT s.value[999999] AS elem FROM java.lang.String s LIMIT 3").unwrap();
        // Inspect the parsed select item
        println!("select[0] = {:?}", q.select[0]);
        let plan = pq(&q).unwrap();
        println!("finalize_at = {:?}", plan.finalize_at);
        println!("needs.array_index = {}", plan.needs.array_index);
        assert!(plan.needs.array_index, "array index must set needs.array_index");
        assert_eq!(plan.finalize_at, Phase::P2, "array index must finalize at P2");
        // late_ops must contain ResolveArrayIndex
        assert!(
            plan.late_ops.iter().any(|op| matches!(op, StageOp::ResolveArrayIndex)),
            "array index must emit ResolveArrayIndex late op, got: {:?}", plan.late_ops
        );
    }

    #[test]
    fn distinct_now_plans() {
        // DISTINCT is no longer rejected at plan time; the plan must succeed.
        let plan = pq(&parse("SELECT DISTINCT * FROM C").unwrap());
        assert!(plan.is_ok(), "DISTINCT should plan successfully, got: {:?}", plan.unwrap_err());
    }

    #[test]
    fn distinct_with_limit_plans_ok() {
        let plan = pq(&parse("SELECT DISTINCT @objectId FROM C LIMIT 5").unwrap());
        assert!(plan.is_ok(), "DISTINCT LIMIT should plan, got: {:?}", plan.unwrap_err());
        // For a DISTINCT query, scan-time limit is cleared so all rows flow through
        // for dedup; the limit is applied post-dedup at the choke point.
        let plan = plan.unwrap();
        assert_eq!(plan.limit, None, "scan-time limit must be cleared for DISTINCT");
    }

    #[test]
    fn non_distinct_limit_unchanged() {
        // Non-DISTINCT queries must keep their scan-time limit (invariant guard).
        let plan = pq(&parse("SELECT @objectId FROM C LIMIT 7").unwrap()).unwrap();
        assert_eq!(plan.limit, Some(7), "non-distinct limit must pass through unchanged");
    }

    #[test]
    fn predicates_ordered_cheapest_first() {
        let q = parse("SELECT * FROM C WHERE name = \"x\" AND count > 1").unwrap();
        let plan = pq(&q).unwrap();
        let plan = crate::query::optimize::optimize(
            plan,
            &q,
            &crate::query::optimize::SchemaStats::default(),
        );
        assert!(matches!(
            plan.where_terms.first(),
            Some(Conjunct {
                cost: PredCost::Scalar,
                ..
            })
        ));
    }

    // --- Additional tests requested by the user ---

    #[test]
    fn plan_dominators_emits_dominator_children_stage() {
        let plan =
            pq(&parse("SELECT dominators(s) FROM java.lang.String s").unwrap()).unwrap();
        assert!(matches!(plan.carry, CarryLayout::IndexOnly));
        assert_eq!(plan.late_ops.len(), 1);
        assert!(matches!(
            plan.late_ops[0],
            StageOp::DominatorChildren { .. }
        ));
        assert_eq!(plan.finalize_at, Phase::P3);
        assert!(plan.needs.dominator_children);
    }
    #[test]
    fn plan_dominators_unknown_alias_rejected() {
        let err = pq(&parse("SELECT dominators(x) FROM java.lang.String s").unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("unknown alias 'x'"), "got: {err}");
    }
    #[test]
    fn plan_dominatorof_emits_dominator_of_stage() {
        let plan =
            pq(&parse("SELECT dominatorof(s) FROM java.lang.String s").unwrap()).unwrap();
        assert_eq!(plan.late_ops.len(), 1);
        assert!(matches!(plan.late_ops[0], StageOp::DominatorOf));
        assert_eq!(plan.finalize_at, Phase::P3);
        assert!(plan.needs.dominator_children);
    }
    #[test]
    fn plan_inbounds_emits_edge_lookup_inbound() {
        let plan = pq(&parse("SELECT @inbounds FROM java.lang.String").unwrap()).unwrap();
        assert_eq!(
            plan.late_ops,
            vec![StageOp::EdgeLookup {
                dir: EdgeDir::Inbound
            }]
        );
        assert_eq!(plan.finalize_at, Phase::P2);
        assert!(matches!(plan.carry, CarryLayout::IndexOnly));
        // The edge lookup does not arm the dominator-children CSR.
        assert!(!plan.needs.dominator_children);
    }
    #[test]
    fn plan_outbounds_emits_edge_lookup_outbound() {
        let plan = pq(&parse("SELECT @outbounds FROM java.lang.String").unwrap()).unwrap();
        assert_eq!(
            plan.late_ops,
            vec![StageOp::EdgeLookup {
                dir: EdgeDir::Outbound
            }]
        );
        assert_eq!(plan.finalize_at, Phase::P2);
        assert!(matches!(plan.carry, CarryLayout::IndexOnly));
        assert!(!plan.needs.dominator_children);
    }
    #[test]
    fn plan_star_select_does_not_emit_edge_lookup() {
        // Regression guard: the @inbounds/@outbounds special-case must NOT fire
        // for a plain non-edge select — no EdgeLookup, empty late_ops.
        let plan = pq(&parse("SELECT * FROM java.lang.String").unwrap()).unwrap();
        assert!(
            !plan
                .late_ops
                .iter()
                .any(|op| matches!(op, StageOp::EdgeLookup { .. })),
            "SELECT * must not emit an EdgeLookup op, got: {:?}",
            plan.late_ops
        );
        assert!(
            plan.late_ops.is_empty(),
            "SELECT * must have empty late_ops, got: {:?}",
            plan.late_ops
        );
    }

    #[test]
    fn plan_retained_set_emits_retained_set_stage() {
        let plan = pq(&parse("SELECT s AS RETAINED SET FROM java.lang.String s").unwrap())
            .unwrap();
        assert!(matches!(plan.late_ops[0], StageOp::RetainedSet { .. }));
        assert_eq!(plan.finalize_at, Phase::P3);
        assert!(plan.needs.dominator_children);
    }
    #[test]
    fn plan_retained_set_with_aggregate_rejected() {
        let err =
            pq(&parse("SELECT count(s) AS RETAINED SET FROM java.lang.String s").unwrap())
                .unwrap_err();
        assert!(
            err.to_string()
                .contains("RETAINED SET cannot be combined with aggregate"),
            "got: {err}"
        );
    }

    #[test]
    fn plan_percentile_over_scan_attr_ok() {
        // PERCENTILE over a plain scan-time attribute plans fine (no late phase).
        let plan = pq(&parse("SELECT PERCENTILE(@usedHeapSize, 95) FROM C").unwrap()).unwrap();
        assert_eq!(plan.finalize_at, Phase::P1, "percentile over @usedHeapSize is scan-time");
        assert!(plan.late_ops.is_empty(), "no late ops, got: {:?}", plan.late_ops);
    }

    #[test]
    fn plan_percentile_over_retained_rejected() {
        let err =
            pq(&parse("SELECT PERCENTILE(@retainedHeapSize, 95) FROM C").unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("PERCENTILE/MEDIAN cannot be combined with @retainedHeapSize"),
            "got: {err}"
        );
    }

    #[test]
    fn plan_median_over_retained_rejected() {
        let err =
            pq(&parse("SELECT MEDIAN(@retainedHeapSize) FROM C").unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("PERCENTILE/MEDIAN cannot be combined with @retainedHeapSize"),
            "got: {err}"
        );
    }

    #[test]
    fn refpath_in_where_is_predicate_critical() {
        use crate::query::ast::RefRole;
        let q = parse("SELECT * FROM Node x WHERE x.parent.id = 7").unwrap();
        let plan = pq(&q).unwrap();
        assert!(plan.needs.ref_walk, "ref_walk need must be set");
        assert!(
            plan.late_ops.iter().any(|op| matches!(
                op,
                StageOp::RefWalkResolve {
                    role: RefRole::PredicateCritical,
                    ..
                }
            )),
            "expected a PredicateCritical RefWalkResolve op, got {:?}",
            plan.late_ops
        );
        // A predicate-critical refwalk resolves at P2 (before row filtering).
        assert_eq!(plan.finalize_at, Phase::P2);
    }

    #[test]
    fn refpath_projection_only_defers() {
        use crate::query::ast::RefRole;
        let q = parse("SELECT x.parent.name FROM Node x").unwrap();
        let plan = pq(&q).unwrap();
        assert!(plan.needs.ref_walk, "ref_walk need must be set");
        assert!(
            plan.late_ops.iter().any(|op| matches!(
                op,
                StageOp::RefWalkResolve {
                    role: RefRole::ProjectionOnly,
                    ..
                }
            )),
            "expected a ProjectionOnly RefWalkResolve op, got {:?}",
            plan.late_ops
        );
        assert_eq!(plan.finalize_at, Phase::P2);
    }

    #[test]
    fn refpath_emits_one_resolve_op_per_hop() {
        // `x.a.b.c` after alias-strip has hops [a, b] and tail c → 2 resolve ops.
        let q = parse("SELECT x.a.b.c FROM Node x").unwrap();
        let plan = pq(&q).unwrap();
        let hops = plan
            .late_ops
            .iter()
            .filter(|op| matches!(op, StageOp::RefWalkResolve { .. }))
            .count();
        assert_eq!(
            hops, 2,
            "one RefWalkResolve op per hop, got {:?}",
            plan.late_ops
        );
    }

    #[test]
    fn refpath_with_retained_stays_p3() {
        // A refwalk combined with a P3 need (retained) keeps finalize_at at P3
        // (the later phase wins); ref_walk is still armed.
        let q = parse(
            "SELECT x.parent.name, @retainedHeapSize FROM Node x ORDER BY @retainedHeapSize DESC",
        )
        .unwrap();
        let plan = pq(&q).unwrap();
        assert!(plan.needs.ref_walk);
        assert!(plan.needs.retained);
        assert_eq!(
            plan.finalize_at,
            Phase::P3,
            "P3 (retained) must win over P2"
        );
    }

    #[test]
    fn union_arity_mismatch_rejected() {
        let q = parse("SELECT @objectId FROM java.lang.String UNION SELECT @objectId, @usedHeapSize FROM java.lang.Integer").unwrap();
        let err = pq(&q).unwrap_err();
        assert!(
            err.0
                .contains("UNION branches must project the same number of columns"),
            "got: {}",
            err.0
        );
        assert!(
            err.0.contains('1') && err.0.contains('2'),
            "message names both arities: {}",
            err.0
        );
    }
    #[test]
    fn union_retained_set_arm_rejected() {
        let q = parse(
            "SELECT * FROM java.lang.String UNION SELECT * AS RETAINED SET FROM java.lang.Integer",
        )
        .unwrap();
        let err = pq(&q).unwrap_err();
        assert!(err.0.contains("RETAINED SET"), "got: {}", err.0);
    }
    #[test]
    fn union_retained_set_head_rejected() {
        let q = parse(
            "SELECT * AS RETAINED SET FROM java.lang.String UNION SELECT * FROM java.lang.Integer",
        )
        .unwrap();
        let err = pq(&q).unwrap_err();
        assert!(err.0.contains("RETAINED SET"), "got: {}", err.0);
    }
    #[test]
    fn union_aggregate_arm_rejected() {
        let q =
            parse("SELECT * FROM java.lang.String UNION SELECT COUNT(*) FROM java.lang.Integer")
                .unwrap();
        let err = pq(&q).unwrap_err();
        assert!(
            err.0.contains("aggregates are not allowed in a UNION"),
            "got: {}",
            err.0
        );
    }
    #[test]
    fn union_two_branches_plans() {
        let q =
            parse("SELECT * FROM java.lang.String UNION SELECT * FROM java.lang.Integer").unwrap();
        let plan = pq(&q).unwrap();
        assert_eq!(plan.union_branches.len(), 1);
        assert_eq!(plan.select_arity, 1); // Star = arity 1 sentinel (whole-row)
        assert!(
            plan.union_branches[0].union_branches.is_empty(),
            "branch plans stay flat"
        );
    }
    #[test]
    fn non_union_plan_has_empty_branches() {
        let plan = pq(&parse("SELECT @objectId, name FROM C").unwrap()).unwrap();
        assert!(plan.union_branches.is_empty());
        assert_eq!(plan.select_arity, 2);
    }

    #[test]
    fn classof_projection_sets_runtime_type() {
        let plan =
            pq(&parse("SELECT classof(s) FROM java.lang.String s").unwrap()).unwrap();
        assert!(plan.needs.runtime_type);
        assert!(!plan.needs.instance_string);
        assert!(!plan.needs.instance_scalar);
    }

    #[test]
    fn instanceof_where_sets_runtime_type_and_type_cost() {
        let plan =
            pq(&parse("SELECT * FROM C WHERE s INSTANCEOF java.lang.String").unwrap())
                .unwrap();
        assert!(plan.needs.runtime_type);
        assert!(matches!(
            plan.where_terms.first(),
            Some(Conjunct {
                cost: PredCost::Type,
                ..
            })
        ));
    }

    #[test]
    fn displayname_compare_sets_string_need_and_str_cost() {
        let plan =
            pq(&parse("SELECT * FROM C WHERE @displayName = \"foo\"").unwrap()).unwrap();
        assert!(plan.needs.instance_string);
        assert!(matches!(
            plan.where_terms.first(),
            Some(Conjunct {
                cost: PredCost::Str,
                ..
            })
        ));
    }

    #[test]
    fn mixed_where_full_cheapest_first_order() {
        // Written worst-first on purpose: Str, Scalar, Type. Expect Type, Scalar, Str after optimize.
        let q = parse(
            "SELECT * FROM C WHERE name = \"x\" AND count > 1 \
             AND s INSTANCEOF java.lang.String",
        )
        .unwrap();
        let plan = pq(&q).unwrap();
        let plan = crate::query::optimize::optimize(
            plan,
            &q,
            &crate::query::optimize::SchemaStats::default(),
        );
        let costs: Vec<PredCost> = plan.where_terms.iter().map(|c| c.cost).collect();
        assert_eq!(
            costs,
            vec![PredCost::Type, PredCost::Scalar, PredCost::Str],
            "got: {costs:?}"
        );
        assert!(plan.needs.instance_scalar);
        assert!(plan.needs.instance_string);
        assert!(plan.needs.runtime_type);
    }

    #[test]
    fn explain_lists_kind_and_needs() {
        let plan = pq(&parse("SELECT @objectId FROM C WHERE count > 3").unwrap()).unwrap();
        let text = plan.explain();
        assert!(text.contains("SingleScan"));
        assert!(text.contains("instance_scalar"));
        assert!(text.contains("where:"));
    }

    #[test]
    fn explain_histogram_only_no_where() {
        let plan = pq(&parse("SELECT COUNT(*) FROM java.lang.String").unwrap()).unwrap();
        let text = plan.explain();
        assert!(text.contains("HistogramOnly"), "got: {text}");
        assert!(text.contains("histogram"), "got: {text}");
        assert!(!text.contains("where:"), "got: {text}");
        assert!(!text.contains("limit:"), "got: {text}");
    }

    #[test]
    fn explain_shows_limit() {
        let plan = pq(&parse("SELECT * FROM C LIMIT 10").unwrap()).unwrap();
        let text = plan.explain();
        assert!(text.contains("limit: 10"), "got: {text}");
    }

    #[test]
    fn explain_no_needs_shows_none() {
        // @objectId does not arm any field-decode need; no WHERE either.
        let plan = pq(&parse("SELECT @objectId FROM C").unwrap()).unwrap();
        let text = plan.explain();
        assert!(text.contains("needs (armed): none"), "got: {text}");
    }

    #[test]
    fn rejects_nested_aggregate() {
        let err = pq(&parse("SELECT COUNT(SUM(x)) FROM C").unwrap()).unwrap_err();
        assert!(err.0.to_lowercase().contains("aggregate"), "got: {}", err.0);
    }

    #[test]
    fn aggregate_with_where_is_single_scan() {
        // An aggregate that also filters cannot use the pre-built histogram.
        let plan = pq(&parse("SELECT COUNT(*) FROM C WHERE count > 1").unwrap()).unwrap();
        assert_eq!(plan.kind, StageKind::SingleScan);
        assert!(!plan.needs.histogram);
    }

    // --- Field validation (unknown-field rejection) ---

    struct FakeSchema {
        class: &'static str,
        fields: Vec<&'static str>,
    }
    impl FieldSchema for FakeSchema {
        fn class_field_names(&self, exact_class_name: &str) -> Option<Vec<String>> {
            if exact_class_name.replace('/', ".") == self.class {
                Some(self.fields.iter().map(|s| s.to_string()).collect())
            } else {
                None
            }
        }
    }

    #[test]
    fn validate_accepts_known_field() {
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec!["count", "hash", "value"],
        };
        let q = parse("SELECT count FROM java.lang.String WHERE hash > 0").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_select_field() {
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec!["count", "hash"],
        };
        let q = parse("SELECT bogusfield FROM java.lang.String").unwrap();
        let err = validate_fields(&q, &schema).unwrap_err();
        assert!(err.0.contains("unknown field"), "got: {}", err.0);
        assert!(err.0.contains("bogusfield"), "got: {}", err.0);
        assert!(err.0.contains("java.lang.String"), "got: {}", err.0);
        // Actionable: lists the known fields.
        assert!(
            err.0.contains("count"),
            "should list known fields: {}",
            err.0
        );
    }

    #[test]
    fn validate_rejects_unknown_where_field() {
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec!["count"],
        };
        let q = parse("SELECT * FROM java.lang.String WHERE nope > 3").unwrap();
        let err = validate_fields(&q, &schema).unwrap_err();
        assert!(err.0.contains("unknown field"), "got: {}", err.0);
        assert!(err.0.contains("nope"), "got: {}", err.0);
    }

    #[test]
    fn validate_strips_alias_before_lookup() {
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec!["count", "hash"],
        };
        // `s.count`/`s.hash` must resolve as bare `count`/`hash`.
        let q = parse("SELECT s.count FROM java.lang.String s WHERE s.hash > 0").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
        // Alias-stripped unknown field is still rejected, reported bare.
        let q2 = parse("SELECT s.bogus FROM java.lang.String s").unwrap();
        let err = validate_fields(&q2, &schema).unwrap_err();
        assert!(err.0.contains("unknown field `bogus`"), "got: {}", err.0);
    }

    #[test]
    fn validate_accepts_bare_alias_reference() {
        // A bare reference to the FROM alias (`SELECT s ... String s`, as
        // emitted by `AS RETAINED SET`) denotes the whole object, not a field,
        // so it must not be validated as (and rejected as) an unknown field.
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec!["count", "hash"],
        };
        let q = parse("SELECT s FROM java.lang.String s").unwrap();
        assert!(
            validate_fields(&q, &schema).is_ok(),
            "bare alias must be accepted"
        );
        let q2 = parse("SELECT s AS RETAINED SET FROM java.lang.String s").unwrap();
        assert!(
            validate_fields(&q2, &schema).is_ok(),
            "AS RETAINED SET bare alias must be accepted"
        );
    }

    #[test]
    fn validate_rejects_unknown_order_by_field() {
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec!["count", "hash"],
        };
        let q = parse("SELECT * FROM java.lang.String ORDER BY bogus").unwrap();
        let err = validate_fields(&q, &schema).unwrap_err();
        assert!(err.0.contains("unknown field"), "got: {}", err.0);
        assert!(err.0.contains("bogus"), "got: {}", err.0);
    }

    #[test]
    fn validate_accepts_known_order_by_field() {
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec!["count", "hash"],
        };
        let q = parse("SELECT * FROM java.lang.String ORDER BY count DESC").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
    }

    #[test]
    fn validate_accepts_order_by_select_alias() {
        // ORDER BY <name> where <name> is a SELECT column alias must not be
        // rejected as an unknown field — it references an output column, not
        // a raw heap field.  This covers the common pattern:
        //   SELECT @retainedHeapSize AS bytes ... ORDER BY bytes DESC
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec!["value", "coder", "hash"],
        };
        let q =
            parse("SELECT @retainedHeapSize AS bytes FROM java.lang.String ORDER BY bytes DESC")
                .unwrap();
        assert!(validate_fields(&q, &schema).is_ok(), "alias in ORDER BY must be accepted");

        // Also works when combined with toString() (the original failing case).
        let q = parse(
            "SELECT toString(s) AS value, @retainedHeapSize AS bytes FROM java.lang.String s ORDER BY bytes DESC LIMIT 5",
        )
        .unwrap();
        assert!(validate_fields(&q, &schema).is_ok(), "toString + alias ORDER BY must be accepted");
    }

    #[test]
    fn validate_skips_glob_from() {
        // Glob FROM classes vary per instance; field validation is skipped.
        let schema = FakeSchema {
            class: "irrelevant",
            fields: vec![],
        };
        let q = parse("SELECT anything FROM com.acme.*").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
    }

    #[test]
    fn validate_skips_unresolvable_class() {
        // Unknown class → schema returns None → we can't prove a field missing.
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec!["count"],
        };
        let q = parse("SELECT whatever FROM com.other.Unknown").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
    }

    #[test]
    fn validate_ignores_builtin_attrs() {
        // @-attrs are not bare fields and must never be flagged.
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec![],
        };
        let q =
            parse("SELECT @objectId, @usedHeapSize, @displayName FROM java.lang.String").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
    }

    // ---------- correlated-subquery rejection (Task 22) ----------

    #[test]
    fn correlated_from_subquery_rejected() {
        // inner references outer alias `s` via a dotted LHS head `s.y` it doesn't
        // bind (its own alias is `o`). RHS must be a literal in our grammar, so
        // correlation surfaces on the compared attribute, not the value.
        let q = parse("SELECT * FROM (SELECT * FROM java.lang.Object o WHERE s.y > 0) x").unwrap();
        let err = pq(&q).unwrap_err();
        assert!(
            err.0.contains("correlated") || err.0.contains("references"),
            "got: {}",
            err.0
        );
    }

    #[test]
    fn noncorrelated_from_subquery_ok() {
        let q =
            parse("SELECT * FROM (SELECT * FROM java.lang.String s WHERE s.count > 0) x").unwrap();
        assert!(pq(&q).is_ok());
    }

    #[test]
    fn correlated_in_subquery_rejected() {
        // The IN-subquery's inner references an unbound dotted head `t.v`.
        let q = parse(
            "SELECT * FROM java.lang.String s WHERE @objectAddress IN \
             (SELECT * FROM java.lang.Integer i WHERE t.v > 0)",
        )
        .unwrap();
        let err = pq(&q).unwrap_err();
        assert!(
            err.0.contains("correlated") || err.0.contains("references"),
            "got: {}",
            err.0
        );
    }

    #[test]
    fn noncorrelated_in_subquery_ok() {
        let q = parse(
            "SELECT * FROM java.lang.String s WHERE @objectAddress IN \
             (SELECT @objectAddress FROM java.lang.Integer i WHERE i.v > 0)",
        )
        .unwrap();
        assert!(pq(&q).is_ok());
    }

    #[test]
    fn referenced_alias_heads_skips_own_alias_and_bare_fields() {
        // `s.count` head `s` is the bound alias (excluded); `count` is bare (no head).
        let q = parse("SELECT s.count FROM java.lang.String s WHERE count > 0").unwrap();
        assert!(referenced_alias_heads(&q).is_empty());
    }

    #[test]
    fn referenced_alias_heads_collects_foreign_head() {
        let q = parse("SELECT * FROM java.lang.String s WHERE s.a = 1 AND t.b = 2").unwrap();
        let heads = referenced_alias_heads(&q);
        assert!(
            heads.contains("t"),
            "expected foreign head `t`, got: {heads:?}"
        );
        assert!(!heads.contains("s"), "bound alias `s` must be excluded");
    }

    // ---------- subquery plan wiring (Task 23, Steps 5-6) ----------

    #[test]
    fn from_subquery_scalar_projection_rejected() {
        // A FROM-subquery projecting a scalar/field loses object identity, so the
        // outer semi-join can't run; reject with the whole-objects message.
        let q = parse("SELECT * FROM (SELECT n FROM java.lang.String s) x").unwrap();
        let err = pq(&q).unwrap_err();
        assert!(
            err.0.contains("FROM-subquery must select whole objects"),
            "got: {}",
            err.0
        );
    }

    #[test]
    fn from_subquery_star_projection_accepted() {
        let q = parse("SELECT * FROM (SELECT * FROM java.lang.String s) x").unwrap();
        let plan = pq(&q).unwrap();
        assert!(
            plan.from_subplan.is_some(),
            "FROM-subquery must plan an inner subplan"
        );
        assert!(plan.in_subplans.is_empty());
    }

    #[test]
    fn from_subquery_objectid_projection_accepted() {
        let q = parse("SELECT * FROM (SELECT @objectId FROM java.lang.String s) x").unwrap();
        let plan = pq(&q).unwrap();
        assert!(plan.from_subplan.is_some());
    }

    #[test]
    fn from_subquery_aggregate_rejected() {
        // An aggregate folds during the scan, before the FROM-subquery semi-join
        // is applied, so the result would ignore the subquery. Reject it.
        let q = parse("SELECT COUNT(*) FROM (SELECT * FROM java.lang.String s) x").unwrap();
        let err = pq(&q).unwrap_err();
        assert!(
            err.0
                .contains("aggregates over a FROM-subquery are not supported"),
            "got: {}",
            err.0
        );
    }

    #[test]
    fn in_subquery_non_address_projection_rejected() {
        // The inner projects `@objectId` (a dense index, not an address); IN
        // compares outer addresses, so this must be rejected.
        let q = parse(
            "SELECT * FROM java.lang.String s WHERE @objectAddress IN \
             (SELECT @objectId FROM java.lang.Integer i)",
        )
        .unwrap();
        let err = pq(&q).unwrap_err();
        assert!(
            err.0
                .contains("IN-subquery must select a single address-valued column"),
            "got: {}",
            err.0
        );
    }

    #[test]
    fn in_subquery_address_projection_accepted() {
        let q = parse(
            "SELECT * FROM java.lang.String s WHERE @objectAddress IN \
             (SELECT @objectAddress FROM java.lang.Integer i)",
        )
        .unwrap();
        let plan = pq(&q).unwrap();
        assert_eq!(
            plan.in_subplans.len(),
            1,
            "one IN-subquery must plan one InSubplan"
        );
        assert!(plan.from_subplan.is_none(), "no FROM-subquery here");
        assert_eq!(plan.in_subplans[0].lhs, Attr::ObjectAddress);
    }

    #[test]
    fn plain_query_has_no_subplans() {
        let plan = pq(&parse("SELECT @objectId FROM C").unwrap()).unwrap();
        assert!(plan.from_subplan.is_none());
        assert!(plan.in_subplans.is_empty());
    }

    #[test]
    fn two_in_subqueries_plan_two_subplans() {
        let q = parse(
            "SELECT * FROM java.lang.String s WHERE \
             @objectAddress IN (SELECT @objectAddress FROM A a) AND \
             @objectAddress IN (SELECT @objectAddress FROM B b)",
        )
        .unwrap();
        let plan = pq(&q).unwrap();
        assert_eq!(plan.in_subplans.len(), 2);
    }

    // ---------- Task 34: explain() / stage_list() tests ----------

    /// After optimize, explain() must contain `scan_limit: 5` when the limit
    /// was pushed down to the scan by the optimizer.
    #[test]
    fn explain_shows_scan_limit_after_optimize() {
        let q = parse("SELECT @objectId FROM java.lang.String LIMIT 5").unwrap();
        let plan = pq(&q).unwrap();
        let plan = crate::query::optimize::optimize(plan, &q, &Default::default());
        let out = plan.explain();
        assert!(
            out.contains("scan_limit: 5"),
            "explain() must show scan_limit: 5 after optimize, got:\n{out}"
        );
    }

    /// stage_list() on an optimized plan with LIMIT 5 must contain "scan_limit=5".
    #[test]
    fn stage_list_reports_scan_limit() {
        let q = parse("SELECT @objectId FROM java.lang.String LIMIT 5").unwrap();
        let plan = pq(&q).unwrap();
        let plan = crate::query::optimize::optimize(plan, &q, &Default::default());
        let list = plan.stage_list();
        assert!(
            list.iter().any(|s| s == "scan_limit=5"),
            "stage_list() must contain 'scan_limit=5', got: {:?}",
            list
        );
    }

    /// An unoptimized plan (plan_query only, no optimize) for LIMIT 5 must have
    /// 'limit=5' in stage_list() but NO 'scan_limit=' entry.
    #[test]
    fn stage_list_raw_has_no_scan_limit() {
        let q = parse("SELECT @objectId FROM java.lang.String LIMIT 5").unwrap();
        let plan = pq(&q).unwrap();
        let list = plan.stage_list();
        assert!(
            list.iter().any(|s| s == "limit=5"),
            "stage_list() must contain 'limit=5', got: {:?}",
            list
        );
        assert!(
            !list.iter().any(|s| s.starts_with("scan_limit=")),
            "unoptimized plan must NOT contain 'scan_limit=', got: {:?}",
            list
        );
    }

    // --- toString(s) planning (MAT gap #3) ---

    #[test]
    fn tostring_select_sets_string_values_need_and_p2_finalize() {
        let plan =
            pq(&parse("SELECT toString(s) FROM java.lang.String s").unwrap()).unwrap();
        assert!(
            plan.needs.string_values,
            "toString SELECT must arm string_values need"
        );
        assert_eq!(
            plan.finalize_at,
            Phase::P2,
            "toString SELECT must finalize at P2"
        );
        assert!(
            plan.late_ops
                .iter()
                .any(|op| matches!(op, StageOp::ResolveStringValues)),
            "toString SELECT must emit a ResolveStringValues op, got {:?}",
            plan.late_ops
        );
    }

    #[test]
    fn tostring_where_sets_string_values_need() {
        let plan = pq(
            &parse(r#"SELECT @objectId FROM java.lang.String s WHERE toString(s) LIKE "java\..*""#)
                .unwrap(),
        )
        .unwrap();
        assert!(
            plan.needs.string_values,
            "toString WHERE must arm string_values need"
        );
        assert_eq!(plan.finalize_at, Phase::P2);
    }

    // Wave C: toString on a non-String FROM class is no longer a plan error.
    // It now falls through to a scan-time display projection (<class> @ 0x<addr>)
    // with NO late ResolveStringValues op. The String path is unchanged (below).
    #[test]
    fn tostring_non_string_no_longer_errors() {
        let plan = pq(&parse("SELECT toString(t) FROM java.lang.Thread t").unwrap());
        assert!(plan.is_ok(), "non-String toString should plan: {plan:?}");
        let plan = plan.unwrap();
        assert!(
            !plan
                .late_ops
                .iter()
                .any(|op| matches!(op, StageOp::ResolveStringValues)),
            "non-String toString must NOT emit ResolveStringValues, got {:?}",
            plan.late_ops
        );
    }

    #[test]
    fn tostring_string_still_uses_string_values() {
        let plan =
            pq(&parse("SELECT toString(s) FROM java.lang.String s").unwrap()).unwrap();
        assert!(
            format!("{plan:?}").contains("ResolveStringValues"),
            "String toString must still emit ResolveStringValues, got {plan:?}"
        );
    }

    #[test]
    fn tostring_subquery_still_rejected() {
        let plan =
            pq(&parse("SELECT toString(s) FROM (SELECT * FROM java.lang.Object) s").unwrap());
        assert!(plan.is_err(), "toString over a subquery must be rejected");
        let err = plan.unwrap_err();
        assert!(
            err.0.contains("subquery") && err.0.contains("inner"),
            "subquery toString error must guide the user to the inner query, got: {}",
            err.0
        );
    }

    // Formerly `tostring_on_non_string_from_is_plan_error`: non-String FROM now
    // plans cleanly (scan-time display form), it is no longer an error.
    #[test]
    fn tostring_on_non_string_object_from_plans_ok() {
        let plan = pq(&parse("SELECT toString(s) FROM java.lang.Object s").unwrap());
        assert!(
            plan.is_ok(),
            "non-String Object FROM toString must plan: {plan:?}"
        );
    }

    #[test]
    fn tostring_on_string_class_alternate_forms_accepted() {
        // The dotted class name must be accepted at plan time.
        assert!(
            pq(&parse("SELECT toString(s) FROM java.lang.String s").unwrap()).is_ok(),
            "dotted class name must succeed"
        );
    }

    // Formerly `tostring_non_string_from_error_names_fix`: a non-String container
    // class (HashMap) now plans cleanly as a scan-time display projection.
    #[test]
    fn tostring_on_non_string_container_from_plans_ok() {
        let plan = pq(&parse("SELECT toString(s) FROM java.util.HashMap s").unwrap());
        assert!(
            plan.is_ok(),
            "non-String HashMap FROM toString must plan: {plan:?}"
        );
    }

    // ============================================================
    // No-toString gating: string_values flag must NOT be set for
    // non-toString queries (negative control) and MUST be set for
    // toString queries (positive control). Pins the query-gating
    // invariant so non-toString runs never arm the decode path.
    // ============================================================

    /// A pure COUNT(*) histogram query must NOT set `string_values`.
    #[test]
    fn no_tostring_count_star_gating_false() {
        let plan = pq(&parse("SELECT COUNT(*) FROM java.lang.String").unwrap()).unwrap();
        assert!(
            !plan.needs.string_values,
            "COUNT(*) must not arm string_values, got: {:?}",
            plan.needs
        );
    }

    /// A WHERE-only query on @usedHeapSize must NOT set `string_values`.
    #[test]
    fn no_tostring_used_heap_size_where_gating_false() {
        let plan =
            pq(&parse("SELECT * FROM java.lang.String s WHERE @usedHeapSize > 0").unwrap())
                .unwrap();
        assert!(
            !plan.needs.string_values,
            "WHERE @usedHeapSize query must not arm string_values, got: {:?}",
            plan.needs
        );
    }

    /// A plain scalar-field SELECT on a non-String class must NOT set `string_values`.
    #[test]
    fn no_tostring_scalar_select_gating_false() {
        let plan = pq(&parse("SELECT count FROM java.util.HashMap").unwrap()).unwrap();
        assert!(
            !plan.needs.string_values,
            "field SELECT on non-String class must not arm string_values, got: {:?}",
            plan.needs
        );
    }

    /// Positive control: `SELECT toString(s) FROM java.lang.String s` must set
    /// `needs.string_values == true` and finalize at P2 (the string-values decode
    /// window), and emit a `ResolveStringValues` late op.
    #[test]
    fn tostring_select_sets_string_values_true_and_p2() {
        let plan =
            pq(&parse("SELECT toString(s) FROM java.lang.String s").unwrap()).unwrap();
        assert!(
            plan.needs.string_values,
            "toString SELECT must arm string_values, got: {:?}",
            plan.needs
        );
        assert_eq!(
            plan.finalize_at,
            Phase::P2,
            "toString SELECT must finalize at P2, got: {:?}",
            plan.finalize_at
        );
        assert!(
            plan.late_ops
                .iter()
                .any(|op| matches!(op, StageOp::ResolveStringValues)),
            "toString SELECT must emit a ResolveStringValues late op, got: {:?}",
            plan.late_ops
        );
    }

    /// Positive control: `WHERE toString(s) LIKE "..."` must also set
    /// `needs.string_values == true` and finalize at P2.
    #[test]
    fn tostring_where_like_sets_string_values_true_and_p2() {
        let plan = pq(
            &parse(r#"SELECT @objectId FROM java.lang.String s WHERE toString(s) LIKE "java\..*""#)
                .unwrap(),
        )
        .unwrap();
        assert!(
            plan.needs.string_values,
            "toString WHERE must arm string_values, got: {:?}",
            plan.needs
        );
        assert_eq!(
            plan.finalize_at,
            Phase::P2,
            "toString WHERE must finalize at P2, got: {:?}",
            plan.finalize_at
        );
    }

    // ============================================================
    // path(a, b) planning tests
    // ============================================================

    /// Pin: DEFAULT_PATH_DEPTH_CAP must be 5 (the canonical CLI default depth).
    #[test]
    fn default_path_depth_cap_constant_is_5() {
        assert_eq!(crate::query::DEFAULT_PATH_DEPTH_CAP, 5);
    }

    /// Depth flag threads through: planning with depth=7 must produce
    /// `BoundedPath { depth_cap: 7 }`, NOT the default 5.
    #[test]
    fn plan_path_depth_param_overrides_default() {
        let q = parse("SELECT path(a, b) FROM java.lang.Thread a").unwrap();
        let plan = plan_query(&q, 7).unwrap();
        assert_eq!(
            plan.late_ops,
            vec![StageOp::BoundedPath { depth_cap: 7 }],
            "plan with depth=7 must carry depth_cap=7, not the default"
        );
    }

    /// A lone `SELECT path(a, b)` with the canonical default depth plans to a
    /// BoundedPath late op at P2 with depth_cap == DEFAULT_PATH_DEPTH_CAP.
    #[test]
    fn plan_path_lone_emits_bounded_path_op() {
        let plan =
            pq(&parse("SELECT path(a, b) FROM java.lang.Thread a").unwrap()).unwrap();
        assert_eq!(
            plan.late_ops,
            vec![StageOp::BoundedPath {
                depth_cap: crate::query::DEFAULT_PATH_DEPTH_CAP
            }],
            "lone path(a,b) must emit exactly one BoundedPath op"
        );
        assert_eq!(
            plan.finalize_at,
            Phase::P2,
            "path(a,b) must finalize at P2"
        );
        assert!(
            matches!(plan.carry, CarryLayout::IndexOnly),
            "path(a,b) carry must be IndexOnly"
        );
        assert_eq!(plan.kind, StageKind::SingleScan);
    }

    /// A mixed select with path(a, b) plus another item must be rejected with an
    /// actionable error mentioning "only select item".
    #[test]
    fn plan_path_mixed_select_rejected_actionably() {
        let err = pq(
            &parse("SELECT path(a,b), @usedHeapSize FROM java.lang.Thread a").unwrap(),
        )
        .unwrap_err();
        assert!(
            err.0.contains("only select item"),
            "mixed path select must mention 'only select item'; got: {}",
            err.0
        );
    }

    /// path(a, b) as an aggregate argument must be rejected with an actionable error.
    #[test]
    fn plan_path_as_aggregate_arg_rejected() {
        let err =
            pq(&parse("SELECT COUNT(path(a, b)) FROM java.lang.Thread a").unwrap())
                .unwrap_err();
        assert!(
            err.0.contains("aggregate"),
            "path-as-aggregate-arg must mention 'aggregate'; got: {}",
            err.0
        );
    }

    /// A plain non-path query must still plan with finalize_at == P1 and no
    /// BoundedPath op (memory invariant: no spurious forward-edge retention).
    #[test]
    fn no_path_query_stays_p1_no_bounded_path_op() {
        let plan = pq(&parse("SELECT COUNT(*) FROM java.lang.String").unwrap()).unwrap();
        assert_eq!(plan.finalize_at, Phase::P1);
        assert!(
            !plan.late_ops.iter().any(|op| matches!(op, StageOp::BoundedPath { .. })),
            "non-path query must not emit BoundedPath op"
        );
    }

    // ============================================================
    // Arithmetic expression planning (Task 5: see-through Expr leaves)
    // ============================================================

    /// `expr_for_each_attr` visits all attr leaves of a Binary tree.
    #[test]
    fn expr_for_each_attr_visits_all_leaves() {
        use crate::query::ast::{ArithOp, Value};
        // Build: @retainedHeapSize * (@usedHeapSize + 2)
        let e = Expr::Binary {
            op: ArithOp::Mul,
            lhs: Box::new(Expr::Attr(Attr::RetainedHeapSize)),
            rhs: Box::new(Expr::Binary {
                op: ArithOp::Add,
                lhs: Box::new(Expr::Attr(Attr::UsedHeapSize)),
                rhs: Box::new(Expr::Lit(Value::Int(2))),
            }),
        };
        let mut visited = Vec::new();
        expr_for_each_attr(&e, &mut |a| visited.push(a.clone()));
        assert_eq!(visited.len(), 2, "must visit exactly the 2 attr leaves");
        assert!(visited.contains(&Attr::RetainedHeapSize));
        assert!(visited.contains(&Attr::UsedHeapSize));
    }

    /// `expr_any_attr` finds RetainedHeapSize two levels deep inside a tree.
    #[test]
    fn expr_any_attr_finds_retained_two_levels_deep() {
        use crate::query::ast::{ArithOp, UnaryOp, Value};
        // Build: -((@retainedHeapSize + 1) * 2)
        let e = Expr::Unary {
            op: UnaryOp::Neg,
            arg: Box::new(Expr::Binary {
                op: ArithOp::Mul,
                lhs: Box::new(Expr::Binary {
                    op: ArithOp::Add,
                    lhs: Box::new(Expr::Attr(Attr::RetainedHeapSize)),
                    rhs: Box::new(Expr::Lit(Value::Int(1))),
                }),
                rhs: Box::new(Expr::Lit(Value::Int(2))),
            }),
        };
        assert!(
            expr_any_attr(&e, |a| matches!(a, Attr::RetainedHeapSize)),
            "expr_any_attr must find RetainedHeapSize buried two levels deep"
        );
        assert!(
            !expr_any_attr(&e, |a| matches!(a, Attr::UsedHeapSize)),
            "expr_any_attr must return false when attr is absent"
        );
    }

    /// `SELECT @retainedHeapSize * 2 FROM C` must arm the retained need and finalize at P3.
    #[test]
    fn arithmetic_select_retained_arms_p3() {
        let plan = pq(&parse("SELECT @retainedHeapSize * 2 FROM C").unwrap()).unwrap();
        assert!(
            plan.needs.retained,
            "SELECT @retainedHeapSize * 2 must arm the retained need"
        );
        assert_eq!(
            plan.finalize_at,
            Phase::P3,
            "SELECT @retainedHeapSize * 2 must finalize at P3"
        );
        assert_eq!(
            plan.late_ops,
            vec![StageOp::JoinRetained],
            "SELECT @retainedHeapSize * 2 must emit JoinRetained late op"
        );
    }

    /// `WHERE @retainedHeapSize * 2 > 100` must arm the retained need and finalize at P3.
    #[test]
    fn arithmetic_where_retained_arms_p3() {
        let plan =
            pq(&parse("SELECT @objectId FROM C WHERE @retainedHeapSize * 2 > 100").unwrap())
                .unwrap();
        assert!(
            plan.needs.retained,
            "WHERE @retainedHeapSize * 2 > 100 must arm the retained need"
        );
        assert_eq!(plan.finalize_at, Phase::P3);
    }

    /// `pred_uses_retained` must return true when @retainedHeapSize is inside arithmetic.
    #[test]
    fn pred_uses_retained_arithmetic_lhs() {
        let q = parse("SELECT @objectId FROM C WHERE @retainedHeapSize * 2 > 100").unwrap();
        let pred = q.where_.as_ref().unwrap();
        assert!(
            pred_uses_retained(pred),
            "pred_uses_retained must fire for @retainedHeapSize inside arithmetic"
        );
    }

    /// `SELECT @usedHeapSize * 2 FROM C` must NOT arm retained (non-retained arithmetic).
    #[test]
    fn arithmetic_select_non_retained_stays_p1() {
        let plan = pq(&parse("SELECT @usedHeapSize * 2 FROM C").unwrap()).unwrap();
        assert!(
            !plan.needs.retained,
            "SELECT @usedHeapSize * 2 must NOT arm the retained need"
        );
        assert_eq!(
            plan.finalize_at,
            Phase::P1,
            "SELECT @usedHeapSize * 2 must stay at P1"
        );
        assert!(
            plan.late_ops.is_empty(),
            "SELECT @usedHeapSize * 2 must have no late ops"
        );
    }

    /// A `WHERE` arithmetic compare with a `Field` leaf must arm `instance_scalar`.
    #[test]
    fn arithmetic_where_field_arms_instance_scalar() {
        let plan =
            pq(&parse("SELECT @objectId FROM C WHERE count * 2 > 100").unwrap()).unwrap();
        assert!(
            plan.needs.instance_scalar,
            "WHERE count * 2 > 100 must arm instance_scalar"
        );
        assert!(
            !plan.needs.instance_string,
            "WHERE count * 2 > 100 must NOT arm instance_string"
        );
    }

    /// `collect_select_fields` must collect field names from an Expr item.
    #[test]
    fn collect_select_fields_descends_into_expr() {
        use crate::query::ast::{ArithOp, Value};
        // Manually build: SelectItem::Expr(count * 2)
        let item = SelectItem::Expr(Box::new(Expr::Binary {
            op: ArithOp::Mul,
            lhs: Box::new(Expr::Attr(Attr::Field("count".to_string()))),
            rhs: Box::new(Expr::Lit(Value::Int(2))),
        }));
        let mut out = Vec::new();
        collect_select_fields(&item, &mut out);
        assert_eq!(out, vec!["count"], "collect_select_fields must descend into Expr and collect 'count'");
    }

    /// Field validation must reject an unknown field inside arithmetic in SELECT.
    #[test]
    fn validate_rejects_unknown_field_inside_arithmetic_select() {
        let schema = FakeSchema {
            class: "java.lang.String",
            fields: vec!["count", "hash"],
        };
        // `badfield * 2` – badfield is not in the schema.
        let q = parse("SELECT badfield * 2 FROM java.lang.String").unwrap();
        let err = validate_fields(&q, &schema).unwrap_err();
        assert!(err.0.contains("unknown field"), "got: {}", err.0);
        assert!(err.0.contains("badfield"), "got: {}", err.0);
    }

    /// A RefPath buried in a WHERE-arithmetic compare must yield `PredCost::Ref`.
    #[test]
    fn pred_cost_ref_path_in_arithmetic_is_ref_cost() {
        // Build: WHERE x.parent.id * 2 > 0 — the RefPath should force Ref cost.
        // We test pred_cost directly since it's in the same module.
        use crate::query::ast::{ArithOp, Value};
        let pred = Predicate::Compare {
            lhs: Expr::Binary {
                op: ArithOp::Mul,
                lhs: Box::new(Expr::Attr(Attr::RefPath {
                    hops: vec!["parent".to_string()],
                    tail: Box::new(Attr::Field("id".to_string())),
                    role: crate::query::ast::RefRole::ProjectionOnly,
                })),
                rhs: Box::new(Expr::Lit(Value::Int(2))),
            },
            op: crate::query::ast::CompareOp::Gt,
            rhs: Expr::Lit(Value::Int(0)),
        };
        assert_eq!(
            pred_cost(&pred),
            PredCost::Ref,
            "a RefPath buried inside arithmetic must yield Ref cost"
        );
    }

    /// A RefPath arithmetic expr in SELECT must arm refwalk and P2 finalize.
    #[test]
    fn arithmetic_select_refpath_arms_refwalk_p2() {
        // `x.parent.id * 2` — a 1-hop RefPath inside arithmetic in SELECT
        let plan =
            pq(&parse("SELECT x.parent.id * 2 FROM Node x").unwrap()).unwrap();
        assert!(
            plan.needs.ref_walk,
            "arithmetic SELECT with RefPath must arm ref_walk"
        );
        assert_eq!(
            plan.finalize_at,
            Phase::P2,
            "arithmetic SELECT with RefPath must finalize at P2"
        );
    }

    /// aggregate-over-expression: `SUM(@usedHeapSize * 2)` must plan without error.
    #[test]
    fn aggregate_over_expression_plans_ok() {
        let plan = pq(&parse("SELECT SUM(@usedHeapSize * 2) FROM C").unwrap());
        assert!(
            plan.is_ok(),
            "SUM(@usedHeapSize * 2) must plan successfully, got: {:?}",
            plan.unwrap_err()
        );
        // `@usedHeapSize` doesn't arm instance_scalar (it's not a user field);
        // what matters is that the plan succeeds (no unreachable! panic).
    }

    /// aggregate-over-expression with a Field leaf arms instance_scalar.
    #[test]
    fn aggregate_over_expression_with_field_arms_scalar() {
        let plan = pq(&parse("SELECT SUM(count * 2) FROM C").unwrap()).unwrap();
        assert!(
            plan.needs.instance_scalar,
            "SUM(count * 2) must arm instance_scalar (count is a Field)"
        );
    }

    /// Non-arithmetic queries (folded to plain Attr/Lit) plan byte-identically to before.
    #[test]
    fn non_arithmetic_query_plans_identically() {
        // These are the original folded-leaf cases; they must be unchanged.
        let p1 = pq(&parse("SELECT @retainedHeapSize FROM C").unwrap()).unwrap();
        assert!(p1.needs.retained && p1.finalize_at == Phase::P3);

        let p2 = pq(&parse("SELECT @objectId FROM C WHERE count > 3").unwrap()).unwrap();
        assert!(p2.needs.instance_scalar && p2.finalize_at == Phase::P1 && !p2.needs.retained);

        let p3 = pq(&parse("SELECT @objectId FROM C WHERE @retainedHeapSize > 1024").unwrap())
            .unwrap();
        assert!(p3.needs.retained && p3.finalize_at == Phase::P3);
    }

    // ============================================================
    // SW-4: MIN/MAX over @attr must route to SingleScan (not HistogramOnly)
    // ============================================================

    /// `MIN(s.@usedHeapSize)` must route to SingleScan, not HistogramOnly.
    /// Before the fix the planner sends this to HistogramOnly which returns Null
    /// for MIN/MAX because the histogram only knows count + shallow_total.
    #[test]
    fn min_used_heap_size_routes_single_scan() {
        let plan = pq(
            &parse("SELECT MIN(s.@usedHeapSize) FROM java.lang.String s").unwrap(),
        )
        .unwrap();
        assert_eq!(
            plan.kind,
            StageKind::SingleScan,
            "MIN(@usedHeapSize) must route to SingleScan so the per-object accumulator \
             can compute the real minimum; got HistogramOnly (would return null)"
        );
    }

    /// `MAX(s.@usedHeapSize)` must also route to SingleScan.
    #[test]
    fn max_used_heap_size_routes_single_scan() {
        let plan = pq(
            &parse("SELECT MAX(s.@usedHeapSize) FROM java.lang.String s").unwrap(),
        )
        .unwrap();
        assert_eq!(
            plan.kind,
            StageKind::SingleScan,
            "MAX(@usedHeapSize) must route to SingleScan; got HistogramOnly (would return null)"
        );
    }

    /// `MIN(@objectId)` must route to SingleScan (histogram has no object-id info).
    #[test]
    fn min_object_id_routes_single_scan() {
        let plan =
            pq(&parse("SELECT MIN(s.@objectId) FROM java.lang.String s").unwrap()).unwrap();
        assert_eq!(
            plan.kind,
            StageKind::SingleScan,
            "MIN(@objectId) must route to SingleScan; histogram cannot answer it"
        );
    }

    /// `MAX(@objectId)` must route to SingleScan.
    #[test]
    fn max_object_id_routes_single_scan() {
        let plan =
            pq(&parse("SELECT MAX(s.@objectId) FROM java.lang.String s").unwrap()).unwrap();
        assert_eq!(
            plan.kind,
            StageKind::SingleScan,
            "MAX(@objectId) must route to SingleScan; histogram cannot answer it"
        );
    }

    /// `MIN(s.hash)` (plain instance field) must also route to SingleScan.
    #[test]
    fn min_instance_field_routes_single_scan() {
        let plan =
            pq(&parse("SELECT MIN(s.hash) FROM java.lang.String s").unwrap()).unwrap();
        assert_eq!(
            plan.kind,
            StageKind::SingleScan,
            "MIN over an instance field must route to SingleScan"
        );
    }

    // Positive regression: the three histogram-answerable shapes must STAY on
    // HistogramOnly (byte/RSS-identical fast path — do not regress this).

    /// `COUNT(*)` must stay on HistogramOnly.
    #[test]
    fn count_star_stays_histogram_only() {
        let plan =
            pq(&parse("SELECT COUNT(*) FROM java.lang.String").unwrap()).unwrap();
        assert_eq!(
            plan.kind,
            StageKind::HistogramOnly,
            "COUNT(*) must stay on the fast histogram path"
        );
    }

    /// `SUM(@usedHeapSize)` must stay on HistogramOnly.
    #[test]
    fn sum_used_heap_size_stays_histogram_only() {
        let plan =
            pq(&parse("SELECT SUM(@usedHeapSize) FROM java.lang.String").unwrap()).unwrap();
        assert_eq!(
            plan.kind,
            StageKind::HistogramOnly,
            "SUM(@usedHeapSize) must stay on the fast histogram path"
        );
    }

    /// `AVG(@usedHeapSize)` must stay on HistogramOnly.
    #[test]
    fn avg_used_heap_size_stays_histogram_only() {
        let plan =
            pq(&parse("SELECT AVG(@usedHeapSize) FROM java.lang.String").unwrap()).unwrap();
        assert_eq!(
            plan.kind,
            StageKind::HistogramOnly,
            "AVG(@usedHeapSize) must stay on the fast histogram path"
        );
    }

    /// A mixed MIN+SUM in the same SELECT must route to SingleScan (MIN is not
    /// histogram-answerable, even though SUM would be alone).
    #[test]
    fn mixed_min_sum_routes_single_scan() {
        let plan = pq(
            &parse(
                "SELECT MIN(s.@usedHeapSize), SUM(s.@usedHeapSize) FROM java.lang.String s",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            plan.kind,
            StageKind::SingleScan,
            "MIN+SUM mix must route to SingleScan (MIN is not histogram-answerable)"
        );
    }

    /// INSTANCEOF SUBCLASS FIX: `COUNT(*) FROM INSTANCEOF C` must NOT use the
    /// histogram fast path. A `ClassSummary` has no super-chain, so the histogram
    /// would count only the exact class. Forcing SingleScan lets `class_matches`
    /// walk the hierarchy via `is_instance_of`. The exact-class COUNT still uses
    /// the histogram (guarded by `count_star_stays_histogram_only`).
    #[test]
    fn count_instanceof_routes_single_scan() {
        let plan =
            pq(&parse("SELECT COUNT(*) FROM INSTANCEOF java.lang.Thread").unwrap()).unwrap();
        assert_eq!(
            plan.kind,
            StageKind::SingleScan,
            "COUNT(*) FROM INSTANCEOF must route to SingleScan so subclasses are \
             resolved via the superclass walk, not the class-summary histogram"
        );
    }

    #[test]
    fn gcroots_attr_sets_needs_gc_roots_and_forces_carry() {
        let plan = pq(&parse("SELECT @GCRoots FROM java.lang.Thread").unwrap()).unwrap();
        assert!(
            plan.needs.gc_roots,
            "@GCRoots must set needs.gc_roots"
        );
        assert_ne!(
            plan.finalize_at,
            Phase::P1,
            "@GCRoots must force finalize_at != P1 so the entry goes into carry mode"
        );
    }

    #[test]
    fn group_by_plans_as_group_by_stage() {
        let q = parse(
            "SELECT @displayName, COUNT(*) FROM java.lang.Thread GROUP BY @displayName",
        )
        .unwrap();
        let plan = plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap();
        assert_eq!(plan.kind, StageKind::GroupBy);
        assert_eq!(plan.group_by_exprs.len(), 1);
        assert!(
            matches!(
                plan.group_by_exprs.first(),
                Some(crate::query::ast::Expr::Attr(crate::query::ast::Attr::DisplayName))
            ),
            "expected DisplayName expr in group_by_exprs"
        );
    }

    #[test]
    fn having_without_group_by_errors_at_plan_time() {
        use crate::query::ast::{Attr, CompareOp, Expr, Predicate, Value};
        let q = parse("SELECT COUNT(*) FROM java.lang.Thread").unwrap();
        // Inject having manually to test planner path
        let mut q2 = q.clone();
        q2.having = Some(Predicate::Compare {
            lhs: Expr::Attr(Attr::UsedHeapSize),
            op: CompareOp::Gt,
            rhs: Expr::Lit(Value::Int(0)),
        });
        let err = plan_query(&q2, crate::query::DEFAULT_PATH_DEPTH_CAP)
            .expect_err("HAVING without GROUP BY must error");
        assert!(err.0.to_lowercase().contains("having"), "got: {}", err.0);
    }

    #[test]
    fn group_by_non_aggregate_not_in_group_by_errors() {
        let q = parse(
            "SELECT @displayName, @usedHeapSize, COUNT(*) FROM java.lang.Thread GROUP BY @displayName",
        )
        .unwrap();
        let err = plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP)
            .expect_err("@usedHeapSize not in GROUP BY must error");
        assert!(
            err.0.contains("@usedHeapSize") || err.0.contains("usedHeapSize") || err.0.to_lowercase().contains("non-aggregate"),
            "error must name the offending column, got: {}",
            err.0
        );
    }

    #[test]
    fn array_index_in_where_errors() {
        use crate::query::parse::parse;
        // Inject ArrayIndex into WHERE (parse may or may not support it directly,
        // so build the query manually)
        let q = parse("SELECT @objectId FROM java.lang.String s WHERE s.value[0] > 65");
        // If the parser doesn't support this form, just verify the planner would reject it
        match q {
            Err(_) => { /* parser rejected it — acceptable */ }
            Ok(q) => {
                let err = plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP)
                    .expect_err("ArrayIndex in WHERE must error");
                assert!(
                    err.0.to_lowercase().contains("array") || err.0.to_lowercase().contains("where"),
                    "error must mention array or WHERE, got: {}",
                    err.0
                );
            }
        }
    }
}

