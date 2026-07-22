//! Late-phase query runner. Consumes the cross-phase carries in a
//! QueryExecState after dominators + retained sizes exist, applies each plan's
//! late_ops, and reassembles all results in original query order.

use crate::query::ast::{Attr, CompareOp, Predicate, Query, SelectItem, SortDir, Value};
use crate::query::execute::{CrossPhaseEntry, QueryExecState};
use crate::query::model::{QueryColumn, QueryResult, QueryValue};
use crate::query::plan::StageOp;
use crate::query::runflags::EdgeDir;
use crate::query::PATH_FRONTIER_CAP;

/// A shared empty tail-scalar table, used as the default `refwalk_tails` borrow
/// when no RefWalk query ran (the common case). `LazyLock` derefs to a `'static`
/// `&HashMap`, so it can back any `LateCtx` lifetime.
pub(crate) static EMPTY_REFWALK_TAILS: std::sync::LazyLock<
    std::collections::HashMap<u32, QueryValue>,
> = std::sync::LazyLock::new(std::collections::HashMap::new);

/// A shared empty string-values table, used as the default `string_values` borrow
/// when no toString(s) query ran (the common case). Zero-cost for non-toString runs.
pub(crate) static EMPTY_STRING_VALUES: std::sync::LazyLock<std::collections::HashMap<u32, String>> =
    std::sync::LazyLock::new(std::collections::HashMap::new);

/// Maps a dense object index to its object address (and back, if needed) for
/// building result rows in the late phase.
pub struct IdMap<'a> {
    /// Object address per dense index. Borrowed from the pass2 id tables.
    addr_of: &'a [u64],
}
impl<'a> IdMap<'a> {
    pub fn new(addr_of: &'a [u64]) -> Self {
        Self { addr_of }
    }
    pub fn to_addr(&self, dense: u32) -> u64 {
        self.addr_of.get(dense as usize).copied().unwrap_or(0)
    }
    #[cfg(test)]
    pub fn identity(_n: usize) -> Self {
        Self { addr_of: &[] }
    }
}

/// Borrowed late-phase context. Lives only inside the `dc_*`/retained window in
/// main. Later stages grow this struct — never remove fields.
pub struct LateCtx<'a> {
    /// Retained size per dense object index (bytes).
    pub retained: &'a [u64],
    /// Immediate dominator per dense index (`u32::MAX` for roots).
    pub idom: &'a [u32],
    /// Dominator-children CSR offsets (len = n+1).
    pub dc_off: &'a [u32],
    /// Dominator-children CSR targets (dense indices).
    pub dc_tgt: &'a [u32],
    /// Shallow size per dense index (bytes).
    pub shallow: &'a [u32],
    /// Dense-index → address mapping for building result rows.
    pub id_map: &'a IdMap<'a>,
    /// Forward-reference CSR offsets (len = n+1): node `i`'s out-edges are
    /// `fwd_tgt[fwd_off[i]..fwd_off[i+1]]`. Empty when RefWalk is not armed.
    /// The production forward CSR is freed before this window; RefWalk instead
    /// preserves a small query-gated per-field CSR (built only when a RefWalk
    /// query ran) and threads it in here (see `main.rs` resume site).
    pub fwd_off: &'a [u32],
    /// Forward-reference CSR targets (dense indices), parallel to `fwd_field`.
    pub fwd_tgt: &'a [u32],
    /// Per-edge field id, parallel to `fwd_tgt`: the interned field name of the
    /// reference that produced each out-edge. Used to follow a *named* hop.
    pub fwd_field: &'a [u32],
    /// Field-name → interned id table (name at index `id`). `field_id` scans it.
    pub field_names: &'a [String],
    /// Scan-captured RefWalk *tail* scalars, keyed by the resolved-target dense
    /// index. Populated (query-gated) only when a RefWalk query with a primitive
    /// field tail ran; empty otherwise. The late window joins the walked-to dense
    /// index against this to project the real tail value (option (b)).
    pub refwalk_tails: &'a std::collections::HashMap<u32, QueryValue>,
    /// True when RefWalk edge or tail capture overflowed its cap during the scan,
    /// so the per-field CSR is incomplete and RefPath results may be partial.
    /// OR'd into each RefPath result's `truncated` flag.
    pub refwalk_truncated: bool,
    /// True when the toString(s) string-capture table overflowed its cap during
    /// the scan, meaning some String instances were not captured and toString(s)
    /// results may be partial. OR'd into each `ResolveStringValues` result's
    /// `truncated` flag.
    pub string_values_truncated: bool,
    /// Inbound-reference CSR offsets (len n+1): node i's referrers are
    /// `in_tgt[in_off[i]..in_off[i+1]]`. Empty when `@inbounds` is not armed.
    pub in_off: &'a [u32],
    /// Inbound-reference CSR targets (dense referrer indices), parallel to in_off.
    pub in_tgt: &'a [u32],
    /// Retained forward-edge store for `@outbounds`/`path` (L1+L2 compressed),
    /// or `None` when no forward-edge feature is armed.
    pub retained_edges: Option<&'a crate::query::retained_edges::RetainedEdges>,
    /// Decoded `toString(s)` values: `dense_idx → String`. Populated (query-gated)
    /// only when a toString(s) query ran; empty otherwise (non-toString runs keep
    /// the shared `EMPTY_STRING_VALUES` borrow, byte/RSS-identical to before).
    pub string_values: &'a std::collections::HashMap<u32, String>,
}

impl LateCtx<'_> {
    /// The interned id of a field name, or `None` if the name is unknown. Linear
    /// scan over the (small) interning table; RefWalk resolves one id per hop.
    pub fn field_id(&self, name: &str) -> Option<u32> {
        self.field_names
            .iter()
            .position(|f| f == name)
            .map(|p| p as u32)
    }

    /// The scan-captured tail scalar for a resolved-target dense index, if one
    /// was captured (primitive tail). `None` for object-ref tails or dead ends.
    pub fn refwalk_tail(&self, dense: u32) -> Option<&QueryValue> {
        self.refwalk_tails.get(&dense)
    }

    /// The decoded toString(s) text for a String instance at `dense`. `None`
    /// when no toString query ran or the instance was not captured (cap overflow).
    pub fn string_value(&self, dense: u32) -> Option<&str> {
        self.string_values.get(&dense).map(String::as_str)
    }
}

/// Resolve one reference hop: for each source dense index, emit the target dense
/// indices reachable via a forward-ref edge whose field name matches `field`.
/// An unknown field name (or an empty forward CSR) yields no targets.
pub fn resolve_hop(sources: &[u32], field: &str, ctx: &LateCtx) -> Vec<u32> {
    let Some(fid) = ctx.field_id(field) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &s in sources {
        let si = s as usize;
        if si + 1 >= ctx.fwd_off.len() {
            continue;
        }
        let (start, end) = (ctx.fwd_off[si] as usize, ctx.fwd_off[si + 1] as usize);
        for k in start..end {
            if ctx.fwd_field[k] == fid {
                out.push(ctx.fwd_tgt[k]);
            }
        }
    }
    out
}

/// Walk a full RefPath: fold `resolve_hop` over each hop, returning the final
/// frontier of resolved dense indices. An empty `hops` returns the seeds.
pub fn walk_refpath(seeds: &[u32], hops: &[String], ctx: &LateCtx) -> Vec<u32> {
    let mut frontier = seeds.to_vec();
    for h in hops {
        frontier = resolve_hop(&frontier, h, ctx);
    }
    frontier
}

/// Gather the neighbours of each row in `rows` in direction `dir`.
/// `Inbound`: referrers via the inbound CSR (`in_off`/`in_tgt`).
/// `Outbound`: forward targets via the retained edge store (`retained_edges`);
/// yields nothing if that store is absent.
/// Returns the concatenated neighbour dense indices (duplicates possible across
/// rows; dedup is the caller's concern). Bounds-checked against the CSR length.
pub fn edge_lookup(rows: &[u32], dir: EdgeDir, ctx: &LateCtx) -> Vec<u32> {
    let mut out = Vec::new();
    match dir {
        EdgeDir::Inbound => {
            for &r in rows {
                let ri = r as usize;
                // node ri's referrers live in in_tgt[in_off[ri]..in_off[ri+1]];
                // guard both offset reads against a too-short (or empty) CSR.
                if ri + 1 >= ctx.in_off.len() {
                    continue;
                }
                let (start, end) = (ctx.in_off[ri] as usize, ctx.in_off[ri + 1] as usize);
                out.extend_from_slice(&ctx.in_tgt[start..end]);
            }
        }
        EdgeDir::Outbound => {
            if let Some(re) = ctx.retained_edges {
                for &r in rows {
                    out.extend(re.targets_of(r));
                }
            }
        }
    }
    out
}

/// Bounded forward BFS from `from`, at most `depth_cap` levels, following the
/// retained forward edge store. Stops early if a node whose dense index is in
/// `target_rows` is reached. Frontier truncated at `PATH_FRONTIER_CAP` (returns
/// `capped=true` when hit). Returns `(reached_nodes, capped)` where reached_nodes
/// is the set of dense indices visited (BFS order, deduped). Never materializes
/// the whole graph — walks only the retained subgraph via `retained_edges`.
pub fn bounded_path(
    from: u32,
    target_rows: &[u32],
    depth_cap: usize,
    ctx: &LateCtx,
) -> (Vec<u32>, bool) {
    // No forward edge store armed: only the seed is "reached".
    let Some(re) = ctx.retained_edges else {
        return (vec![from], false);
    };
    let targets: std::collections::HashSet<u32> = target_rows.iter().copied().collect();

    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut reached: Vec<u32> = Vec::new();
    let mut capped = false;

    visited.insert(from);
    reached.push(from);
    // Early stop: the seed itself may already be a target.
    if targets.contains(&from) {
        return (reached, capped);
    }

    let mut frontier: Vec<u32> = vec![from];
    for _ in 0..depth_cap {
        if frontier.is_empty() {
            break;
        }
        let mut next: Vec<u32> = Vec::new();
        let mut hit_target = false;
        for &node in &frontier {
            for t in re.targets_of(node) {
                if visited.insert(t) {
                    reached.push(t);
                    if targets.contains(&t) {
                        hit_target = true;
                    }
                    next.push(t);
                    if next.len() > PATH_FRONTIER_CAP {
                        // Truncate the next frontier at the cap; further expansion
                        // is bounded so a pathological fan-out can't blow memory.
                        next.truncate(PATH_FRONTIER_CAP);
                        capped = true;
                        break;
                    }
                }
            }
            if capped {
                break;
            }
        }
        // A target was reached at this level: stop expanding further.
        if hit_target {
            break;
        }
        frontier = next;
    }
    (reached, capped)
}

/// Finalize a Phase-1 QueryExecState: run each pending carry through its
/// `late_ops`, merge with finished results in original slot order.
pub fn resume(state: QueryExecState, queries: &[Query], ctx: &LateCtx) -> Vec<QueryResult> {
    let (finished, pending) = state.into_parts();
    let mut slotted: Vec<(usize, QueryResult)> = finished;
    for entry in pending {
        let r = run_entry(&entry, &queries[entry.slot], ctx);
        slotted.push((entry.slot, r));
    }
    slotted.sort_by_key(|(slot, _)| *slot);
    slotted.into_iter().map(|(_, r)| r).collect()
}

/// Finalize a Phase-1 QueryExecState WITHOUT a late context: used by the
/// query-only fast path (`run_single_dump`) that never computes retained sizes
/// or dominators. Finished results pass through in slot order; any pending
/// cross-phase carry (a `@retainedHeapSize` query) cannot be answered here, so
/// it produces an actionable error result rather than silently empty rows.
pub fn resume_without_late_ctx(state: QueryExecState) -> Vec<QueryResult> {
    let (finished, pending) = state.into_parts();
    let mut slotted: Vec<(usize, QueryResult)> = finished;
    for entry in pending {
        // Tailor the error to what the entry actually needs so the message names
        // the real cause (a generic "@retainedHeapSize" message would mislead a
        // user running an edge or dominator query). Every one of these features
        // needs a structure built only in the full analyze scan (reference CSR,
        // inbound/forward edge store, dominator tree, or post-scan retained
        // sizes), none of which the query-only path builds. Classify by the plan
        // so the fix ("run the full report") is attached to the right feature.
        let error = if entry.plan.needs.ref_walk {
            "reference-path (N-hop `x.field.tail`) queries require the full \
             analysis pipeline; the reference graph is not built in the \
             query-only path. Run the full report (drop --query-only) to use \
             reference-path queries."
        } else if entry
            .plan
            .late_ops
            .iter()
            .any(|op| matches!(op, StageOp::EdgeLookup { .. } | StageOp::BoundedPath { .. }))
        {
            "edge queries (`@inbounds`/`@outbounds`/`path(a, b)`) require the \
             full analysis pipeline; the reference edge index is not built in \
             the query-only path. Run the full report (drop --query-only) to \
             use edge queries."
        } else if entry.plan.needs.dominator_children {
            "dominator queries (`dominators(x)`/`AS RETAINED SET`) require the \
             full analysis pipeline; the dominator tree is not built in the \
             query-only path. Run the full report (drop --query-only) to use \
             dominator queries."
        } else {
            "@retainedHeapSize requires the full analysis pipeline; \
             it is not available in the query-only path. Run the full \
             report (drop --query-only) to use retained-size queries."
        };
        slotted.push((
            entry.slot,
            QueryResult {
                name: entry.name.clone(),
                oql: String::new(),
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
                truncated: false,
                error: Some(error.to_string()),
                note: None,
            },
        ));
    }
    slotted.sort_by_key(|(slot, _)| *slot);
    slotted.into_iter().map(|(_, r)| r).collect()
}

/// Run a single cross-phase entry against the given late context.
/// Exposed `pub(crate)` for the query-only hybrid resume path.
pub(crate) fn run_entry_pub(entry: &CrossPhaseEntry, q: &Query, ctx: &LateCtx) -> QueryResult {
    run_entry(entry, q, ctx)
}

fn run_entry(entry: &CrossPhaseEntry, q: &Query, ctx: &LateCtx) -> QueryResult {
    // Dominator/retained-set ops each produce a one-column ObjRef result and
    // fully own row building; they never fall through to join_retained.
    for op in &entry.plan.late_ops {
        match op {
            StageOp::JoinRetained => {}
            StageOp::DominatorChildren { cap } => {
                let idx: Vec<u32> = entry.carry.indices();
                let children = run_dominator_children(&idx, *cap, ctx);
                let truncated = entry.carry.truncated() || children.len() >= *cap;
                return dominator_rows(entry, q, &children, truncated, ctx);
            }
            StageOp::DominatorOf => {
                let idx: Vec<u32> = entry.carry.indices();
                let idoms = run_dominator_of(&idx, ctx);
                return dominator_rows(entry, q, &idoms, entry.carry.truncated(), ctx);
            }
            StageOp::RetainedSet { cap } => {
                let seeds: Vec<u32> = entry.carry.indices();
                let (set, trunc) = run_retained_set(&seeds, *cap, ctx);
                let truncated = entry.carry.truncated() || trunc;
                return dominator_rows(entry, q, &set, truncated, ctx);
            }
            // The plan emits one RefWalkResolve op per hop, but the whole walk is
            // driven off the query's RefPath AST in one pass here — the first op
            // handles the entire path, so later per-hop ops are skipped.
            StageOp::RefWalkResolve { .. } => {
                return refpath_rows(entry, q, ctx);
            }
            StageOp::EdgeLookup { dir } => {
                let idx: Vec<u32> = entry.carry.indices();
                let neighbours = edge_lookup(&idx, *dir, ctx);
                return dominator_rows(entry, q, &neighbours, entry.carry.truncated(), ctx);
            }
            StageOp::BoundedPath { depth_cap } => {
                // Bounded walk from each carried seed; concatenate reached nodes.
                // `target_rows` is empty here (no early target stop): carry-level
                // target-class resolution lands in a later task (41/42).
                let seeds: Vec<u32> = entry.carry.indices();
                let mut reached = Vec::new();
                let mut capped = false;
                for s in seeds {
                    let (nodes, c) = bounded_path(s, &[], *depth_cap, ctx);
                    reached.extend(nodes);
                    capped |= c;
                }
                return dominator_rows(entry, q, &reached, entry.carry.truncated() || capped, ctx);
            }
            StageOp::ResolveStringValues => {
                return string_values_rows(entry, q, ctx);
            }
            // Later phases add more StageOp variants; an unhandled op must fail
            // loudly rather than silently dropping the query's late work.
            #[allow(unreachable_patterns)]
            other => {
                return QueryResult {
                    name: entry.name.clone(),
                    oql: String::new(),
                    columns: Vec::new(),
                    rows: Vec::new(),
                    row_count: 0,
                    truncated: false,
                    error: Some(format!("stage op {other:?} not supported in this phase")),
                    note: None,
                }
            }
        }
    }
    join_retained(entry, q, ctx)
}

/// Build a single-column result of object references from a set of dense
/// indices (dominator children / idoms / retained closure). LIMIT is applied
/// here since these ops don't route through join_retained.
fn dominator_rows(
    entry: &CrossPhaseEntry,
    q: &Query,
    indices: &[u32],
    mut truncated: bool,
    ctx: &LateCtx,
) -> QueryResult {
    let mut indices = indices.to_vec();
    if let Some(limit) = q.limit {
        if indices.len() as u64 > limit {
            indices.truncate(limit as usize);
            truncated = true;
        }
    }
    let col = q
        .select
        .first()
        .map(crate::query::execute::column_name)
        .unwrap_or_else(|| "*".to_string());
    let rows: Vec<Vec<QueryValue>> = indices
        .iter()
        .map(|&i| {
            vec![QueryValue::ObjRef {
                index: ctx.id_map.to_addr(i),
                class: "?".to_string(),
            }]
        })
        .collect();
    QueryResult {
        name: entry.name.clone(),
        oql: String::new(),
        columns: vec![QueryColumn { name: col }],
        row_count: rows.len() as u64,
        rows,
        truncated,
        error: None,
        note: None,
    }
}

/// Resolve an N-hop `RefPath` projection in the P2 late window. For each carried
/// seed, walk the hop fields to the resolved target's dense index and project the
/// tail: an identity attr (`@objectId`/`@objectAddress`) answered directly from
/// the dense index; a scalar field tail looked up in the scan-captured tail table
/// (`ctx.refwalk_tail`). Dead ends and object-ref/absent tails project `Null`; an
/// absent tail attaches an advisory note. A predicate-critical RefPath in WHERE
/// filters seeds by comparing the resolved tail against the predicate RHS.
fn refpath_rows(entry: &CrossPhaseEntry, q: &Query, ctx: &LateCtx) -> QueryResult {
    // A predicate-critical RefPath in WHERE filters seeds before projection.
    let where_refpath = q.where_.as_ref().and_then(find_pred_refpath);

    // Compile LIKE/NOT LIKE regexes for this query's WHERE predicates. They were
    // already validated at plan time, so compilation here is infallible for
    // well-formed queries; errors fall back to an empty map (LIKE never matches).
    // This is called once per query entry (not per row), so it does not hot-path.
    let like_regexes = crate::query::execute::compile_like_regexes(q).unwrap_or_default();

    let seeds: Vec<u32> = entry.carry.indices();
    let mut note: Option<String> = None;

    // Predicate-critical filter: keep seeds whose resolved tail passes the WHERE
    // comparison. Only the RefPath term is evaluated here (other terms were
    // applied in Phase 1); a seed with no comparison keeps all.
    let kept: Vec<u32> = if let (Some(Attr::RefPath { hops, tail, .. }), Some(pred)) =
        (where_refpath.as_ref(), q.where_.as_ref())
    {
        seeds
            .iter()
            .copied()
            .filter(|&s| {
                let resolved = walk_refpath(&[s], hops, ctx);
                let val = resolved
                    .first()
                    .and_then(|&d| project_tail(tail, d, ctx, &mut note));
                eval_refpath_pred(pred, val.as_ref(), &like_regexes)
            })
            .collect()
    } else {
        seeds.clone()
    };

    let columns: Vec<QueryColumn> = q
        .select
        .iter()
        .map(|it| QueryColumn {
            name: crate::query::execute::column_name(it),
        })
        .collect();

    let mut rows: Vec<Vec<QueryValue>> = Vec::new();
    for &s in &kept {
        let row: Vec<QueryValue> = q
            .select
            .iter()
            .map(|it| match it {
                SelectItem::Attr(Attr::RefPath { hops, tail, .. }) => {
                    let resolved = walk_refpath(&[s], hops, ctx);
                    match resolved.first() {
                        Some(&d) => {
                            project_tail(tail, d, ctx, &mut note).unwrap_or(QueryValue::Null)
                        }
                        None => QueryValue::Null,
                    }
                }
                SelectItem::Attr(Attr::ObjectId) => QueryValue::Int(s as i64),
                SelectItem::Star => QueryValue::ObjRef {
                    index: ctx.id_map.to_addr(s),
                    class: "?".to_string(),
                },
                _ => QueryValue::Null,
            })
            .collect();
        rows.push(row);
    }

    let mut truncated = entry.carry.truncated() || ctx.refwalk_truncated;
    if let Some(limit) = q.limit {
        if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            truncated = true;
        }
    }

    QueryResult {
        name: entry.name.clone(),
        oql: String::new(),
        columns,
        row_count: rows.len() as u64,
        rows,
        truncated,
        error: None,
        note,
    }
}

/// Resolve toString(s) values for each carried dense index. For every String
/// instance carried from the scan, look up its decoded text in `ctx.string_values`,
/// apply any WHERE `toString(s)` predicates, and project result rows with all
/// toString(s) columns filled in. Non-toString SELECT columns (like `*`,
/// `@objectId`, etc.) are projected as far as possible from the dense index.
fn string_values_rows(entry: &CrossPhaseEntry, q: &Query, ctx: &LateCtx) -> QueryResult {
    // Compile LIKE regexes once for the query — reuse the shared helper so that
    // patterns are anchored `^(?:...)$` and validated at plan time. On a bad
    // pattern (cannot happen for a planned query), the empty map causes LIKE to
    // never match, which is the correct "no compiled regex → no match" semantics.
    let like_regexes = crate::query::execute::compile_like_regexes(q).unwrap_or_default();

    let seeds: Vec<u32> = entry.carry.indices();

    // Apply toString(s) WHERE predicates.
    let kept: Vec<u32> = if q.where_.as_ref().is_some_and(has_to_string_pred) {
        seeds
            .iter()
            .copied()
            .filter(|&s| eval_tostring_pred(q.where_.as_ref().unwrap(), s, ctx, &like_regexes))
            .collect()
    } else {
        seeds
    };

    let columns: Vec<QueryColumn> = q
        .select
        .iter()
        .map(|it| QueryColumn {
            name: crate::query::execute::column_name(it),
        })
        .collect();

    let out_rows: Vec<Vec<QueryValue>> = kept
        .iter()
        .map(|&idx| {
            q.select
                .iter()
                .map(|it| project_string_row_item(it, idx, ctx))
                .collect()
        })
        .collect();

    let mut truncated = entry.carry.truncated() || ctx.string_values_truncated;
    let mut out_rows = out_rows;
    if let Some(limit) = q.limit {
        if out_rows.len() as u64 > limit {
            out_rows.truncate(limit as usize);
            truncated = true;
        }
    }

    QueryResult {
        name: entry.name.clone(),
        oql: String::new(),
        columns,
        row_count: out_rows.len() as u64,
        rows: out_rows,
        truncated,
        error: None,
        note: None,
    }
}

/// True if the predicate tree contains any `Attr::ToString` comparison.
fn has_to_string_pred(p: &Predicate) -> bool {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => has_to_string_pred(a) || has_to_string_pred(b),
        Predicate::Not(a) => has_to_string_pred(a),
        Predicate::Compare {
            lhs: Attr::ToString(_),
            ..
        } => true,
        _ => false,
    }
}

/// Evaluate a predicate tree for a String instance at `dense`, resolving
/// `toString(s)` comparisons against `ctx.string_values`.
fn eval_tostring_pred(
    p: &Predicate,
    dense: u32,
    ctx: &LateCtx,
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> bool {
    match p {
        Predicate::And(a, b) => {
            eval_tostring_pred(a, dense, ctx, like_regexes)
                && eval_tostring_pred(b, dense, ctx, like_regexes)
        }
        Predicate::Or(a, b) => {
            eval_tostring_pred(a, dense, ctx, like_regexes)
                || eval_tostring_pred(b, dense, ctx, like_regexes)
        }
        Predicate::Not(a) => !eval_tostring_pred(a, dense, ctx, like_regexes),
        Predicate::Compare {
            lhs: Attr::ToString(_),
            op,
            rhs,
        } => {
            match ctx.string_value(dense) {
                Some(s) => cmp_query_value(&QueryValue::Str(s.to_string()), *op, rhs, like_regexes),
                None => false, // String instance not in capture (cap overflow) → no match
            }
        }
        // Non-toString predicates were already applied at scan time (Phase 1).
        _ => true,
    }
}

/// Project a single SELECT item for a toString(s) result row.
fn project_string_row_item(it: &SelectItem, dense: u32, ctx: &LateCtx) -> QueryValue {
    match it {
        SelectItem::ToString(_) | SelectItem::Attr(Attr::ToString(_)) => ctx
            .string_value(dense)
            .map(|s| QueryValue::Str(s.to_string()))
            .unwrap_or(QueryValue::Null),
        SelectItem::Attr(Attr::ObjectId) => QueryValue::Int(dense as i64),
        SelectItem::Attr(Attr::ObjectAddress) => QueryValue::Int(ctx.id_map.to_addr(dense) as i64),
        SelectItem::Star => QueryValue::ObjRef {
            index: ctx.id_map.to_addr(dense),
            class: "java.lang.String".to_string(),
        },
        _ => QueryValue::Null,
    }
}

/// Project a RefPath tail on a resolved-target dense index. Identity attrs answer
/// directly from the dense index; a scalar field tail is looked up in the
/// scan-captured tail table. Returns `None` (→ caller projects `Null` + note)
/// when a field tail has no captured value (object-ref tail or not decoded).
fn project_tail(
    tail: &Attr,
    dense: u32,
    ctx: &LateCtx,
    note: &mut Option<String>,
) -> Option<QueryValue> {
    match tail {
        Attr::ObjectId => Some(QueryValue::Int(dense as i64)),
        Attr::ObjectAddress => Some(QueryValue::Int(ctx.id_map.to_addr(dense) as i64)),
        Attr::Field(_) => match ctx.refwalk_tail(dense) {
            Some(v) => Some(v.clone()),
            None => {
                note.get_or_insert_with(|| {
                    "a reference-path tail resolved to an object reference (or a \
                     field not captured during the scan); such tails project Null \
                     in this release."
                        .to_string()
                });
                None
            }
        },
        // Nested RefPath tails are folded into `hops` by the parser; any other
        // tail attr is not projectable on a walked-to object here.
        _ => None,
    }
}

/// The first `Attr::RefPath` referenced by a predicate, if any.
fn find_pred_refpath(p: &Predicate) -> Option<Attr> {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            find_pred_refpath(a).or_else(|| find_pred_refpath(b))
        }
        Predicate::Not(a) => find_pred_refpath(a),
        Predicate::Compare {
            lhs: a @ Attr::RefPath { .. },
            ..
        } => Some(a.clone()),
        _ => None,
    }
}

/// Evaluate only the RefPath comparison term(s) of a predicate against the
/// resolved tail value. Non-RefPath terms pass (they were applied in Phase 1).
/// A `None` resolved value fails any comparison (dead end / uncaptured tail).
fn eval_refpath_pred(
    p: &Predicate,
    val: Option<&QueryValue>,
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> bool {
    match p {
        Predicate::And(a, b) => {
            eval_refpath_pred(a, val, like_regexes) && eval_refpath_pred(b, val, like_regexes)
        }
        Predicate::Or(a, b) => {
            eval_refpath_pred(a, val, like_regexes) || eval_refpath_pred(b, val, like_regexes)
        }
        Predicate::Not(a) => !eval_refpath_pred(a, val, like_regexes),
        Predicate::Compare {
            lhs: Attr::RefPath { .. },
            op,
            rhs,
        } => match val {
            Some(v) => cmp_query_value(v, *op, rhs, like_regexes),
            None => false,
        },
        _ => true,
    }
}

/// Compare a resolved tail `QueryValue` against a literal RHS per the operator.
/// `like_regexes` backs LIKE/NOT LIKE for string tails — looked up by pattern
/// string, never compiled per row.
fn cmp_query_value(
    v: &QueryValue,
    op: CompareOp,
    rhs: &Value,
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> bool {
    match (v, rhs) {
        (QueryValue::Int(l), Value::Int(r)) => cmp_i64(*l, op, *r),
        (QueryValue::Int(l), Value::Float(r)) => cmp_f64(*l as f64, op, *r),
        (QueryValue::Float(l), Value::Float(r)) => cmp_f64(*l, op, *r),
        (QueryValue::Float(l), Value::Int(r)) => cmp_f64(*l, op, *r as f64),
        (QueryValue::Str(l), Value::Str(r)) => match op {
            CompareOp::Eq => l == r,
            CompareOp::Ne => l != r,
            CompareOp::Like => like_regexes
                .get(r.as_str())
                .is_some_and(|re| re.is_match(l)),
            CompareOp::NotLike => like_regexes
                .get(r.as_str())
                .is_none_or(|re| !re.is_match(l)),
            _ => false,
        },
        (QueryValue::Bool(l), Value::Bool(r)) => match op {
            CompareOp::Eq => l == r,
            CompareOp::Ne => l != r,
            _ => false,
        },
        // Type mismatch: only Ne is (trivially) true. Non-string LHS with LIKE
        // never matches; NOT LIKE on non-string is trivially true.
        _ => matches!(op, CompareOp::Ne | CompareOp::NotLike),
    }
}
fn cmp_i64(l: i64, op: CompareOp, r: i64) -> bool {
    match op {
        CompareOp::Eq => l == r,
        CompareOp::Ne => l != r,
        CompareOp::Lt => l < r,
        CompareOp::Le => l <= r,
        CompareOp::Gt => l > r,
        CompareOp::Ge => l >= r,
        // LIKE/NOT LIKE are string-only; a numeric LHS never matches a regex.
        CompareOp::Like => false,
        CompareOp::NotLike => true,
    }
}
fn cmp_f64(l: f64, op: CompareOp, r: f64) -> bool {
    match op {
        CompareOp::Eq => l == r,
        CompareOp::Ne => l != r,
        CompareOp::Lt => l < r,
        CompareOp::Le => l <= r,
        CompareOp::Gt => l > r,
        CompareOp::Ge => l >= r,
        // LIKE/NOT LIKE are string-only; a numeric LHS never matches a regex.
        CompareOp::Like => false,
        CompareOp::NotLike => true,
    }
}

fn join_retained(entry: &CrossPhaseEntry, q: &Query, ctx: &LateCtx) -> QueryResult {
    let mut rows: Vec<(u32, u64)> = Vec::new();
    for idx in entry.carry.indices() {
        let ret = *ctx.retained.get(idx as usize).unwrap_or(&0);
        if retained_where_passes(q, ret) {
            rows.push((idx, ret));
        }
    }
    if let Some(ob) = &q.order_by {
        if ob.key == Attr::RetainedHeapSize {
            rows.sort_by_key(|(_, r)| *r);
            if ob.dir == SortDir::Desc {
                rows.reverse();
            }
        }
    }
    let mut truncated = entry.carry.truncated();
    if let Some(limit) = q.limit {
        if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            truncated = true;
        }
    }
    let columns: Vec<QueryColumn> = q
        .select
        .iter()
        .map(|it| QueryColumn {
            name: crate::query::execute::column_name(it),
        })
        .collect();
    let out_rows: Vec<Vec<QueryValue>> = rows
        .iter()
        .map(|(idx, ret)| project_late_row(q, *idx, *ret))
        .collect();
    QueryResult {
        name: entry.name.clone(),
        oql: String::new(),
        columns,
        row_count: out_rows.len() as u64,
        rows: out_rows,
        truncated,
        error: None,
        note: None,
    }
}

/// Evaluate only the retained-size WHERE terms; non-retained terms were already
/// applied in Phase 1, so they pass here.
fn retained_where_passes(q: &Query, ret: u64) -> bool {
    match &q.where_ {
        None => true,
        Some(p) => eval_retained_pred(p, ret),
    }
}
fn eval_retained_pred(p: &Predicate, ret: u64) -> bool {
    match p {
        Predicate::And(a, b) => eval_retained_pred(a, ret) && eval_retained_pred(b, ret),
        Predicate::Or(a, b) => eval_retained_pred(a, ret) || eval_retained_pred(b, ret),
        Predicate::Not(a) => !eval_retained_pred(a, ret),
        Predicate::Compare {
            lhs: Attr::RetainedHeapSize,
            op,
            rhs,
        } => cmp_u64(ret, *op, rhs),
        _ => true,
    }
}
fn cmp_u64(lv: u64, op: CompareOp, rhs: &Value) -> bool {
    let rv = match rhs {
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
        _ => return matches!(op, CompareOp::Ne),
    };
    let l = lv as f64;
    match op {
        CompareOp::Eq => l == rv,
        CompareOp::Ne => l != rv,
        CompareOp::Lt => l < rv,
        CompareOp::Le => l <= rv,
        CompareOp::Gt => l > rv,
        CompareOp::Ge => l >= rv,
        // LIKE/NOT LIKE are string-only; a numeric retained-size LHS never
        // matches a regex, so LIKE is false and NOT LIKE is true.
        CompareOp::Like => false,
        CompareOp::NotLike => true,
    }
}

/// Project a late row. IndexOnly carries answer only @objectId / @retainedHeapSize;
/// blob-dependent attrs need an IndexPlusScalars carry (later step) and are Null.
fn project_late_row(q: &Query, idx: u32, ret: u64) -> Vec<QueryValue> {
    q.select
        .iter()
        .map(|it| match it {
            SelectItem::Attr(Attr::ObjectId) => QueryValue::Int(idx as i64),
            SelectItem::Attr(Attr::RetainedHeapSize) => QueryValue::Int(ret as i64),
            SelectItem::Star => QueryValue::ObjRef {
                index: idx as u64,
                class: "?".to_string(),
            },
            _ => QueryValue::Null,
        })
        .collect()
}

/// Dominator-tree children of each matched dense index, in match order, bounded
/// by `cap`. The dominator tree gives each node one parent, so child lists are
/// disjoint (no dedup needed).
pub(crate) fn run_dominator_children(matches: &[u32], cap: usize, ctx: &LateCtx) -> Vec<u32> {
    let mut out = Vec::new();
    for &i in matches {
        let i = i as usize;
        if i + 1 >= ctx.dc_off.len() {
            continue;
        }
        let (start, end) = (ctx.dc_off[i] as usize, ctx.dc_off[i + 1] as usize);
        for &child in &ctx.dc_tgt[start..end] {
            if out.len() >= cap {
                return out;
            }
            out.push(child);
        }
    }
    out
}

/// Immediate dominator (idom) of each matched dense index, in match order. Tree
/// roots (`idom == u32::MAX`) have no dominator and emit nothing.
pub(crate) fn run_dominator_of(matches: &[u32], ctx: &LateCtx) -> Vec<u32> {
    let mut out = Vec::new();
    for &i in matches {
        if let Some(&d) = ctx.idom.get(i as usize) {
            if d != u32::MAX {
                out.push(d);
            }
        }
    }
    out
}

/// Bounded DFS over the dominator-children CSR from each seed. Returns
/// (closure, truncated); `truncated` iff `cap` was hit before full exploration.
pub(crate) fn run_retained_set(seeds: &[u32], cap: usize, ctx: &LateCtx) -> (Vec<u32>, bool) {
    let n = ctx.dc_off.len().saturating_sub(1);
    let mut visited = vec![false; n];
    let mut out = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    for &s in seeds {
        if (s as usize) < n && !visited[s as usize] {
            stack.push(s);
            while let Some(node) = stack.pop() {
                let ni = node as usize;
                if visited[ni] {
                    continue;
                }
                if out.len() >= cap {
                    return (out, true);
                }
                visited[ni] = true;
                out.push(node);
                let (start, end) = (ctx.dc_off[ni] as usize, ctx.dc_off[ni + 1] as usize);
                for &child in &ctx.dc_tgt[start..end] {
                    if !visited[child as usize] {
                        stack.push(child);
                    }
                }
            }
        }
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::execute::QueryExecState;
    use crate::query::model::{QueryResult, QueryValue};

    fn ctx(retained: &[u64]) -> LateCtx<'_> {
        // Dominator/shallow/id_map fields are unread by the retained-join tests;
        // populate with empty slices and an identity IdMap.
        LateCtx {
            retained,
            idom: &[],
            dc_off: &[],
            dc_tgt: &[],
            shallow: &[],
            id_map: &EMPTY_ID_MAP,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        }
    }

    static EMPTY_ID_MAP: IdMap<'static> = IdMap { addr_of: &[] };

    fn q_slice(q: &crate::query::ast::Query) -> Vec<crate::query::ast::Query> {
        vec![q.clone(), q.clone()]
    }

    #[test]
    fn join_retained_projects_and_orders_desc() {
        let q = crate::query::parse::parse(
            "SELECT @objectId, @retainedHeapSize FROM C ORDER BY @retainedHeapSize DESC",
        )
        .unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        carry.push_index(42);
        carry.push_index(7);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".to_string(), plan, carry);
        let retained = {
            let mut v = vec![0u64; 100];
            v[42] = 1000;
            v[7] = 5000;
            v
        };
        let out = resume(st, &q_slice(&q), &ctx(&retained));
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.rows[0][0], QueryValue::Int(7));
        assert_eq!(r.rows[0][1], QueryValue::Int(5000));
        assert_eq!(r.rows[1][0], QueryValue::Int(42));
        assert_eq!(r.rows[1][1], QueryValue::Int(1000));
    }

    #[test]
    fn join_retained_filters_where_and_limit() {
        let q = crate::query::parse::parse(
            "SELECT @objectId FROM C WHERE @retainedHeapSize > 1500 ORDER BY @retainedHeapSize DESC LIMIT 1").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        for i in [1u32, 2, 3] {
            carry.push_index(i);
        }
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".to_string(), plan, carry);
        let retained = {
            let mut v = vec![0u64; 10];
            v[1] = 1000;
            v[2] = 2000;
            v[3] = 3000;
            v
        };
        let out = resume(st, &q_slice(&q), &ctx(&retained));
        let r = &out[0];
        assert_eq!(r.row_count, 1);
        assert_eq!(r.rows[0][0], QueryValue::Int(3));
        assert!(r.truncated, "LIMIT cap must set truncated");
    }

    #[test]
    fn finished_and_pending_reassemble_in_slot_order() {
        let q = crate::query::parse::parse("SELECT @retainedHeapSize FROM C").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        carry.push_index(5);
        let mut st = QueryExecState::new();
        st.push_finished(
            1,
            QueryResult {
                name: "q_hist".into(),
                oql: String::new(),
                columns: vec![],
                rows: vec![vec![QueryValue::Int(99)]],
                row_count: 1,
                truncated: false,
                error: None,
                note: None,
            },
        );
        st.push_cross_phase(0, "q_ret".to_string(), plan, carry);
        let retained = {
            let mut v = vec![0u64; 10];
            v[5] = 777;
            v
        };
        let out = resume(st, &q_slice(&q), &ctx(&retained));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "q_ret");
        assert_eq!(out[1].name, "q_hist");
    }

    // --- Extra tests (exceed the plan's list) ---

    #[test]
    fn no_where_passes_all() {
        // No WHERE, no ORDER BY, no LIMIT: every carried index is projected.
        let q = crate::query::parse::parse("SELECT @objectId, @retainedHeapSize FROM C").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        for i in [3u32, 8, 1] {
            carry.push_index(i);
        }
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".to_string(), plan, carry);
        let retained = {
            let mut v = vec![0u64; 10];
            v[3] = 30;
            v[8] = 80;
            v[1] = 10;
            v
        };
        let out = resume(st, &q_slice(&q), &ctx(&retained));
        let r = &out[0];
        assert_eq!(r.row_count, 3);
        // Order preserved (push order) since no ORDER BY.
        assert_eq!(r.rows[0][0], QueryValue::Int(3));
        assert_eq!(r.rows[0][1], QueryValue::Int(30));
        assert_eq!(r.rows[1][0], QueryValue::Int(8));
        assert_eq!(r.rows[1][1], QueryValue::Int(80));
        assert_eq!(r.rows[2][0], QueryValue::Int(1));
        assert_eq!(r.rows[2][1], QueryValue::Int(10));
        assert!(!r.truncated);
    }

    #[test]
    fn where_only_filters_on_retained() {
        // WHERE @retainedHeapSize > 100, no ORDER BY: keep only indices above the
        // threshold, preserving push order.
        let q = crate::query::parse::parse("SELECT @objectId FROM C WHERE @retainedHeapSize > 100")
            .unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        for i in [1u32, 2, 3, 4] {
            carry.push_index(i);
        }
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".to_string(), plan, carry);
        let retained = {
            let mut v = vec![0u64; 10];
            v[1] = 50;
            v[2] = 150;
            v[3] = 100;
            v[4] = 200;
            v
        };
        let out = resume(st, &q_slice(&q), &ctx(&retained));
        let r = &out[0];
        assert_eq!(
            r.row_count, 2,
            "only idx 2 (150) and idx 4 (200) exceed 100"
        );
        assert_eq!(r.rows[0][0], QueryValue::Int(2));
        assert_eq!(r.rows[1][0], QueryValue::Int(4));
        assert!(!r.truncated);
    }

    #[test]
    fn empty_carry_yields_empty_result() {
        let q = crate::query::parse::parse("SELECT @objectId, @retainedHeapSize FROM C").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let carry = crate::query::carry::Carry::index_only(100);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q_empty".to_string(), plan, carry);
        let retained = vec![0u64; 10];
        let out = resume(st, &q_slice(&q), &ctx(&retained));
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.row_count, 0);
        assert!(r.rows.is_empty());
        assert!(!r.truncated);
        assert!(r.error.is_none());
        // Columns are still projected even with no rows.
        assert_eq!(r.columns.len(), 2);
    }
}

#[cfg(test)]
mod dom_ctx_tests {
    use super::*;
    /// Dominator tree: 0->{1,2}, 1->{3}. CSR dc_off=[0,2,3,3,3], dc_tgt=[1,2,3].
    pub(super) fn tiny_ctx_parts() -> (Vec<u32>, Vec<u32>, Vec<u32>, Vec<u64>, Vec<u32>) {
        (
            vec![u32::MAX, 0, 0, 1],
            vec![0u32, 2, 3, 3, 3],
            vec![1u32, 2, 3],
            vec![100u64, 40, 10, 20],
            vec![10u32, 10, 10, 20],
        )
    }
    #[test]
    fn late_ctx_exposes_dominator_fields() {
        let (idom, dc_off, dc_tgt, retained, shallow) = tiny_ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx {
            retained: &retained,
            idom: &idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &shallow,
            id_map: &id_map,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        };
        assert_eq!(ctx.dc_off.len(), 5);
        assert_eq!(ctx.id_map.to_addr(0), id_map.to_addr(0));
    }
}

#[cfg(test)]
mod dom_run_tests {
    use super::*;

    fn ctx_parts() -> (Vec<u32>, Vec<u32>, Vec<u32>, Vec<u64>, Vec<u32>) {
        super::dom_ctx_tests::tiny_ctx_parts()
    }

    #[test]
    fn dominator_children_emits_direct_children() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx {
            retained: &retained,
            idom: &idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &shallow,
            id_map: &id_map,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        };
        assert_eq!(
            run_dominator_children(&[0u32], usize::MAX, &ctx),
            vec![1u32, 2]
        );
        assert_eq!(
            run_dominator_children(&[1u32], usize::MAX, &ctx),
            vec![3u32]
        );
        assert!(run_dominator_children(&[2u32], usize::MAX, &ctx).is_empty());
    }
    #[test]
    fn dominator_children_respects_cap() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx {
            retained: &retained,
            idom: &idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &shallow,
            id_map: &id_map,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        };
        assert_eq!(run_dominator_children(&[0u32], 1, &ctx).len(), 1);
    }
    #[test]
    fn dominator_of_emits_idom() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx {
            retained: &retained,
            idom: &idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &shallow,
            id_map: &id_map,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        };
        // idom = [MAX,0,0,1]: node 3's idom is 1, node 1's idom is 0, root 0 yields nothing.
        assert_eq!(run_dominator_of(&[3u32], &ctx), vec![1u32]);
        assert_eq!(run_dominator_of(&[1u32, 2u32], &ctx), vec![0u32, 0u32]);
        assert!(run_dominator_of(&[0u32], &ctx).is_empty());
    }
    #[test]
    fn retained_set_emits_bounded_closure() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx {
            retained: &retained,
            idom: &idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &shallow,
            id_map: &id_map,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        };
        let (mut set, truncated) = run_retained_set(&[0u32], usize::MAX, &ctx);
        set.sort_unstable();
        assert_eq!(set, vec![0u32, 1, 2, 3]);
        assert!(!truncated);
    }
    #[test]
    fn retained_set_overflow_marks_truncated() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx {
            retained: &retained,
            idom: &idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &shallow,
            id_map: &id_map,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        };
        let (set, truncated) = run_retained_set(&[0u32], 2, &ctx);
        assert_eq!(set.len(), 2);
        assert!(truncated);
    }
    #[test]
    fn retained_set_dedups_shared_roots() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx {
            retained: &retained,
            idom: &idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &shallow,
            id_map: &id_map,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        };
        let (mut set, _t) = run_retained_set(&[1u32, 0u32], usize::MAX, &ctx);
        set.sort_unstable();
        assert_eq!(set, vec![0u32, 1, 2, 3]);
    }

    #[test]
    fn resume_dominator_children_builds_rows() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx {
            retained: &retained,
            idom: &idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &shallow,
            id_map: &id_map,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        };
        let q = crate::query::parse::parse("SELECT dominators(s) FROM C s").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        carry.push_index(0);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q_dom".to_string(), plan, carry);
        let out = resume(st, &[q.clone(), q], &ctx);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.row_count, 2, "node 0 has children {{1,2}}");
        assert_eq!(r.columns.len(), 1);
        assert!(r.error.is_none());
    }

    #[test]
    fn resume_dominator_of_builds_single_row() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx {
            retained: &retained,
            idom: &idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &shallow,
            id_map: &id_map,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        };
        let q = crate::query::parse::parse("SELECT dominatorof(s) FROM C s").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        carry.push_index(3);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q_domof".to_string(), plan, carry);
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert_eq!(r.row_count, 1, "node 3's idom is node 1");
    }

    #[test]
    fn resume_retained_set_builds_closure_rows() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx {
            retained: &retained,
            idom: &idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &shallow,
            id_map: &id_map,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        };
        let q = crate::query::parse::parse("SELECT s AS RETAINED SET FROM C s").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        carry.push_index(0);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q_rset".to_string(), plan, carry);
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert_eq!(r.row_count, 4, "closure of node 0 is {{0,1,2,3}}");
    }
}

#[cfg(test)]
mod refwalk_tests {
    use super::*;

    /// Build a LateCtx over a tiny forward-ref graph. Field names are interned
    /// by position: `field_names[id]` is the name of edge-field id `id`.
    fn fwd_ctx<'a>(
        fwd_off: &'a [u32],
        fwd_tgt: &'a [u32],
        fwd_field: &'a [u32],
        field_names: &'a [String],
        id_map: &'a IdMap<'a>,
    ) -> LateCtx<'a> {
        LateCtx {
            retained: &[],
            idom: &[],
            dc_off: &[],
            dc_tgt: &[],
            shallow: &[],
            id_map,
            fwd_off,
            fwd_tgt,
            fwd_field,
            field_names,
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        }
    }

    /// Like `fwd_ctx`, but with a caller-supplied tail-scalar table so the
    /// RefWalkResolve projection tests can join the walked-to dense index against
    /// a captured tail value.
    #[allow(clippy::too_many_arguments)]
    fn fwd_ctx_tails<'a>(
        fwd_off: &'a [u32],
        fwd_tgt: &'a [u32],
        fwd_field: &'a [u32],
        field_names: &'a [String],
        id_map: &'a IdMap<'a>,
        tails: &'a std::collections::HashMap<u32, QueryValue>,
    ) -> LateCtx<'a> {
        LateCtx {
            retained: &[],
            idom: &[],
            dc_off: &[],
            dc_tgt: &[],
            shallow: &[],
            id_map,
            fwd_off,
            fwd_tgt,
            fwd_field,
            field_names,
            refwalk_tails: tails,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        }
    }

    #[test]
    fn field_id_interns_by_position() {
        let names = vec!["parent".to_string(), "next".to_string()];
        let id_map = IdMap::identity(0);
        let ctx = fwd_ctx(&[], &[], &[], &names, &id_map);
        assert_eq!(ctx.field_id("parent"), Some(0));
        assert_eq!(ctx.field_id("next"), Some(1));
        assert_eq!(ctx.field_id("missing"), None);
    }

    #[test]
    fn resolve_hop_follows_named_field() {
        // node 0 --"parent"--> 2 ; node 1 --"parent"--> 2. CSR: each of 0,1 has
        // one out-edge; nodes 2,3 have none. fwd_off len = n+1 = 5.
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(4);
        let ctx = fwd_ctx(
            &[0, 1, 2, 2, 2], // out-edge ranges for nodes 0..3
            &[2, 2],          // targets
            &[0, 0],          // both edges are field "parent" (id 0)
            &names,
            &id_map,
        );
        assert_eq!(resolve_hop(&[0, 1], "parent", &ctx), vec![2, 2]);
    }

    #[test]
    fn resolve_hop_filters_by_field_name() {
        // node 0 has two out-edges: --"parent"--> 5, --"next"--> 9.
        let names = vec!["parent".to_string(), "next".to_string()];
        let id_map = IdMap::identity(10);
        let ctx = fwd_ctx(
            &[0, 2, 2], // node 0 -> edges [0,2); node 1 -> none
            &[5, 9],
            &[0, 1], // parent, next
            &names,
            &id_map,
        );
        assert_eq!(resolve_hop(&[0], "parent", &ctx), vec![5]);
        assert_eq!(resolve_hop(&[0], "next", &ctx), vec![9]);
        // Unknown field name yields nothing (no crash).
        assert!(resolve_hop(&[0], "bogus", &ctx).is_empty());
    }

    #[test]
    fn resolve_hop_empty_csr_is_noop() {
        // The production default: RefWalk not threaded, all slices empty.
        let names: Vec<String> = Vec::new();
        let id_map = IdMap::identity(0);
        let ctx = fwd_ctx(&[], &[], &[], &names, &id_map);
        assert!(resolve_hop(&[0, 1, 2], "parent", &ctx).is_empty());
    }

    #[test]
    fn walk_refpath_folds_two_hops() {
        // 0 --"parent"--> 1 --"parent"--> 2 (chain). Two-hop walk from 0 -> [2].
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(3);
        let ctx = fwd_ctx(
            &[0, 1, 2, 2], // node0->[0,1) node1->[1,2) node2->none
            &[1, 2],
            &[0, 0],
            &names,
            &id_map,
        );
        let hops = vec!["parent".to_string(), "parent".to_string()];
        assert_eq!(walk_refpath(&[0], &hops, &ctx), vec![2]);
    }

    #[test]
    fn walk_refpath_empty_hops_returns_seeds() {
        let names: Vec<String> = Vec::new();
        let id_map = IdMap::identity(0);
        let ctx = fwd_ctx(&[], &[], &[], &names, &id_map);
        assert_eq!(walk_refpath(&[3, 4], &[], &ctx), vec![3, 4]);
    }

    #[test]
    fn walk_refpath_dead_end_yields_empty() {
        // 0 --"parent"--> 1, but 1 has no "parent" edge: second hop is empty.
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(2);
        let ctx = fwd_ctx(&[0, 1, 1], &[1], &[0], &names, &id_map);
        let hops = vec!["parent".to_string(), "parent".to_string()];
        assert!(walk_refpath(&[0], &hops, &ctx).is_empty());
    }

    // --- RefWalkResolve end-to-end projection (Task 4) ---

    use crate::query::execute::QueryExecState;

    /// Build a QueryExecState with one carried seed frontier for `oql`, seeded
    /// with the given dense indices.
    fn refwalk_state(oql: &str, seeds: &[u32]) -> (QueryExecState, crate::query::ast::Query) {
        let q = crate::query::parse::parse(oql).unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        assert!(plan.needs.ref_walk, "query must arm ref_walk: {oql}");
        let mut carry = crate::query::carry::Carry::index_only(1000);
        for &s in seeds {
            carry.push_index(s);
        }
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q_rw".to_string(), plan, carry);
        (st, q)
    }

    #[test]
    fn refwalk_resolve_projects_primitive_tail() {
        // SELECT x.parent.name FROM C x — one hop "parent", tail field "name".
        // seed 0 --parent--> 3; tail table maps 3 -> Int(42).
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(4);
        let mut tails = std::collections::HashMap::new();
        tails.insert(3u32, QueryValue::Int(42));
        let ctx = fwd_ctx_tails(
            &[0, 1, 1, 1, 1], // node 0 -> edge [0,1); nodes 1..3 none
            &[3],
            &[0],
            &names,
            &id_map,
            &tails,
        );
        let (st, q) = refwalk_state("SELECT x.parent.name FROM C x", &[0]);
        let out = resume(st, &[q.clone(), q], &ctx);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(r.row_count, 1, "one seed → one resolved row");
        assert_eq!(r.rows[0][0], QueryValue::Int(42), "tail scalar projected");
    }

    #[test]
    fn refwalk_resolve_dead_end_yields_null() {
        // seed 0 has NO "parent" edge → the walk resolves to nothing. Per spec a
        // dead-end projects Null (the row is kept, cell is Null).
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(2);
        let tails = std::collections::HashMap::new();
        let ctx = fwd_ctx_tails(&[0, 0, 0], &[], &[], &names, &id_map, &tails);
        let (st, q) = refwalk_state("SELECT x.parent.name FROM C x", &[0]);
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none());
        assert_eq!(r.rows[0][0], QueryValue::Null, "dead-end tail is Null");
    }

    #[test]
    fn refwalk_resolve_empty_carry_is_empty() {
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(1);
        let tails = std::collections::HashMap::new();
        let ctx = fwd_ctx_tails(&[0, 0], &[], &[], &names, &id_map, &tails);
        let (st, q) = refwalk_state("SELECT x.parent.name FROM C x", &[]);
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none());
        assert_eq!(r.row_count, 0);
    }

    #[test]
    fn refwalk_resolve_object_ref_tail_is_null_with_note() {
        // Resolved target 3 has NO captured tail (object-ref tail, not decoded):
        // the cell is Null and the result carries an advisory note.
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(4);
        let tails = std::collections::HashMap::new(); // no tail captured for 3
        let ctx = fwd_ctx_tails(&[0, 1, 1, 1, 1], &[3], &[0], &names, &id_map, &tails);
        let (st, q) = refwalk_state("SELECT x.parent.name FROM C x", &[0]);
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none());
        assert_eq!(r.rows[0][0], QueryValue::Null);
        assert!(
            r.note.is_some(),
            "object-ref/absent tail should attach a note"
        );
    }

    #[test]
    fn refwalk_resolve_predicate_critical_filters_by_tail() {
        // SELECT x.parent.hash FROM C x WHERE x.parent.hash > 100.
        // seed 0 --parent--> 3 (hash 150, passes); seed 1 --parent--> 4 (hash 50,
        // filtered out). Only the passing seed's row is emitted.
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(5);
        let mut tails = std::collections::HashMap::new();
        tails.insert(3u32, QueryValue::Int(150));
        tails.insert(4u32, QueryValue::Int(50));
        let ctx = fwd_ctx_tails(
            // node0->[0,1)=3, node1->[1,2)=4; nodes 2..4 none
            &[0, 1, 2, 2, 2, 2],
            &[3, 4],
            &[0, 0],
            &names,
            &id_map,
            &tails,
        );
        let (st, q) = refwalk_state(
            "SELECT x.parent.hash FROM C x WHERE x.parent.hash > 100",
            &[0, 1],
        );
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(r.row_count, 1, "only the seed whose tail > 100 survives");
        assert_eq!(r.rows[0][0], QueryValue::Int(150));
    }

    #[test]
    fn refwalk_resolve_identity_tail_projects_object_id() {
        // A `@objectId` tail is answered directly from the resolved dense index —
        // no scan-captured value needed. (The parser can't emit @-tails via
        // RefPath today, but project_tail supports it for forward-compat.)
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(4);
        let tails = std::collections::HashMap::new();
        let ctx = fwd_ctx_tails(&[0, 1, 1, 1, 1], &[3], &[0], &names, &id_map, &tails);
        // Drive project_tail directly (no OQL surface for @-tails yet).
        let mut note = None;
        assert_eq!(
            project_tail(&Attr::ObjectId, 3, &ctx, &mut note),
            Some(QueryValue::Int(3))
        );
        assert!(note.is_none(), "identity tail needs no note");
    }

    // --- LIKE / NOT LIKE on RefPath string tail ---

    #[test]
    fn refwalk_like_on_string_tail_matches_correctly() {
        // SELECT x.parent.name FROM C x WHERE x.parent.name LIKE "foo.*"
        // seed 0 --parent--> 3 (name "foobar", matches); seed 1 --parent--> 4
        // (name "bar", doesn't match). Only seed 0's row survives the filter.
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(5);
        let mut tails = std::collections::HashMap::new();
        tails.insert(3u32, QueryValue::Str("foobar".to_string()));
        tails.insert(4u32, QueryValue::Str("bar".to_string()));
        let ctx = fwd_ctx_tails(
            &[0, 1, 2, 2, 2, 2],
            &[3, 4],
            &[0, 0],
            &names,
            &id_map,
            &tails,
        );
        let (st, q) = refwalk_state(
            "SELECT x.parent.name FROM C x WHERE x.parent.name LIKE \"foo.*\"",
            &[0, 1],
        );
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(
            r.row_count, 1,
            "only seed 0 (name 'foobar') matches LIKE 'foo.*'"
        );
        assert_eq!(r.rows[0][0], QueryValue::Str("foobar".to_string()));
    }

    #[test]
    fn refwalk_not_like_on_string_tail_negates_correctly() {
        // SELECT x.parent.name FROM C x WHERE x.parent.name NOT LIKE "foo.*"
        // seed 0 -> name "foobar" (filtered out); seed 1 -> name "bar" (survives).
        let names = vec!["parent".to_string()];
        let id_map = IdMap::identity(5);
        let mut tails = std::collections::HashMap::new();
        tails.insert(3u32, QueryValue::Str("foobar".to_string()));
        tails.insert(4u32, QueryValue::Str("bar".to_string()));
        let ctx = fwd_ctx_tails(
            &[0, 1, 2, 2, 2, 2],
            &[3, 4],
            &[0, 0],
            &names,
            &id_map,
            &tails,
        );
        let (st, q) = refwalk_state(
            "SELECT x.parent.name FROM C x WHERE x.parent.name NOT LIKE \"foo.*\"",
            &[0, 1],
        );
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(
            r.row_count, 1,
            "only seed 1 (name 'bar') passes NOT LIKE 'foo.*'"
        );
        assert_eq!(r.rows[0][0], QueryValue::Str("bar".to_string()));
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;
    use crate::query::retained_edges::{RetainedEdges, RetainedEdgesBuilder};

    static EMPTY_ID_MAP: IdMap<'static> = IdMap { addr_of: &[] };

    /// Build a LateCtx wired for edge lookups: an inbound CSR (`in_off`/`in_tgt`)
    /// and an optional retained forward-edge store. All other fields are empty.
    fn edge_ctx<'a>(
        in_off: &'a [u32],
        in_tgt: &'a [u32],
        retained_edges: Option<&'a RetainedEdges>,
    ) -> LateCtx<'a> {
        LateCtx {
            retained: &[],
            idom: &[],
            dc_off: &[],
            dc_tgt: &[],
            shallow: &[],
            id_map: &EMPTY_ID_MAP,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off,
            in_tgt,
            retained_edges,
            string_values: &EMPTY_STRING_VALUES,
            string_values_truncated: false,
        }
    }

    // ---------- edge_lookup ----------

    #[test]
    fn edge_lookup_inbound_returns_sources() {
        // Inbound CSR sized for nodes 0..=5. Node 5's referrers are [2,3].
        // in_off has len n+1 = 7; in_off[5]..in_off[6] = [0,2) into in_tgt.
        let in_off = [0u32, 0, 0, 0, 0, 0, 2];
        let in_tgt = [2u32, 3];
        let ctx = edge_ctx(&in_off, &in_tgt, None);
        assert_eq!(edge_lookup(&[5], EdgeDir::Inbound, &ctx), vec![2, 3]);
    }

    #[test]
    fn edge_lookup_inbound_empty_for_leaf() {
        // Node 0 has no referrers: in_off[0]..in_off[1] is empty.
        let in_off = [0u32, 0, 2];
        let in_tgt = [7u32, 9];
        let ctx = edge_ctx(&in_off, &in_tgt, None);
        assert!(edge_lookup(&[0], EdgeDir::Inbound, &ctx).is_empty());
        // Node 1 does have referrers [7,9].
        assert_eq!(edge_lookup(&[1], EdgeDir::Inbound, &ctx), vec![7, 9]);
    }

    #[test]
    fn edge_lookup_inbound_out_of_range_is_empty() {
        // A row beyond the CSR length must not panic — it yields nothing.
        let in_off = [0u32, 1, 1];
        let in_tgt = [4u32];
        let ctx = edge_ctx(&in_off, &in_tgt, None);
        assert!(edge_lookup(&[99], EdgeDir::Inbound, &ctx).is_empty());
        // Empty inbound CSR is also a no-op.
        let empty = edge_ctx(&[], &[], None);
        assert!(edge_lookup(&[0, 1], EdgeDir::Inbound, &empty).is_empty());
    }

    #[test]
    fn edge_lookup_outbound_uses_retained_edges() {
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(0, &[3, 7]);
        let re = b.finish();
        let ctx = edge_ctx(&[], &[], Some(&re));
        assert_eq!(edge_lookup(&[0], EdgeDir::Outbound, &ctx), vec![3, 7]);
    }

    #[test]
    fn edge_lookup_outbound_none_store_is_empty() {
        let ctx = edge_ctx(&[], &[], None);
        assert!(edge_lookup(&[0, 1, 2], EdgeDir::Outbound, &ctx).is_empty());
    }

    #[test]
    fn edge_lookup_multi_row_concatenates() {
        // Node 1 referrers [4]; node 2 referrers [5,6]. Two rows concatenate.
        let in_off = [0u32, 0, 1, 3];
        let in_tgt = [4u32, 5, 6];
        let ctx = edge_ctx(&in_off, &in_tgt, None);
        assert_eq!(edge_lookup(&[1, 2], EdgeDir::Inbound, &ctx), vec![4, 5, 6]);
    }

    #[test]
    fn edge_lookup_outbound_multi_row_concatenates() {
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(0, &[1, 2]);
        b.push_row(1, &[3]);
        let re = b.finish();
        let ctx = edge_ctx(&[], &[], Some(&re));
        assert_eq!(edge_lookup(&[0, 1], EdgeDir::Outbound, &ctx), vec![1, 2, 3]);
    }

    // ---------- bounded_path ----------

    /// Line graph 0->1->2->...->n (each node points to its successor).
    fn line_graph(n: u32) -> RetainedEdges {
        let mut b = RetainedEdgesBuilder::new();
        for i in 0..n {
            b.push_row(i, &[i + 1]);
        }
        b.finish()
    }

    #[test]
    fn bounded_path_respects_depth_cap() {
        let re = line_graph(60);
        let ctx = edge_ctx(&[], &[], Some(&re));
        let (reached, capped) = bounded_path(0, &[], 3, &ctx);
        assert!(!capped);
        // At most seed + 3 hops = {0,1,2,3}.
        assert!(reached.len() <= 4, "reached too far: {reached:?}");
        let mut sorted = reached.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3]);
        assert!(
            !reached.contains(&50),
            "must not reach a far node at depth 3"
        );
    }

    #[test]
    fn bounded_path_frontier_capped() {
        // One node fans out to > PATH_FRONTIER_CAP targets.
        let mut b = RetainedEdgesBuilder::new();
        let targets: Vec<u32> = (1..=(PATH_FRONTIER_CAP as u32 + 100)).collect();
        b.push_row(0, &targets);
        let re = b.finish();
        let ctx = edge_ctx(&[], &[], Some(&re));
        let (_reached, capped) = bounded_path(0, &[], 5, &ctx);
        assert!(capped, "fan-out beyond PATH_FRONTIER_CAP must set capped");
    }

    #[test]
    fn bounded_path_none_store_returns_seed_only() {
        let ctx = edge_ctx(&[], &[], None);
        let (reached, capped) = bounded_path(0, &[], 10, &ctx);
        assert_eq!(reached, vec![0]);
        assert!(!capped);
    }

    #[test]
    fn bounded_path_early_stop_on_target() {
        let re = line_graph(10);
        let ctx = edge_ctx(&[], &[], Some(&re));
        let (reached, capped) = bounded_path(0, &[2], 10, &ctx);
        assert!(!capped);
        assert!(reached.contains(&2), "must reach the target node 2");
        // Early exit: expansion stops once 2 is reached, so 3 is not visited.
        assert!(
            !reached.contains(&3),
            "must not expand past the target: {reached:?}"
        );
    }

    #[test]
    fn bounded_path_seed_is_target_returns_seed_only() {
        let re = line_graph(10);
        let ctx = edge_ctx(&[], &[], Some(&re));
        let (reached, capped) = bounded_path(0, &[0], 10, &ctx);
        assert_eq!(reached, vec![0], "seed already a target: no expansion");
        assert!(!capped);
    }

    #[test]
    fn bounded_path_dedups_reached() {
        // Diamond: 0->{1,2}, 1->{3}, 2->{3}. Node 3 is reachable via two paths
        // but must appear once in reached.
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(0, &[1, 2]);
        b.push_row(1, &[3]);
        b.push_row(2, &[3]);
        let re = b.finish();
        let ctx = edge_ctx(&[], &[], Some(&re));
        let (reached, _capped) = bounded_path(0, &[], 5, &ctx);
        let threes = reached.iter().filter(|&&r| r == 3).count();
        assert_eq!(threes, 1, "node 3 deduped: {reached:?}");
        let mut sorted = reached.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3]);
    }

    #[test]
    fn bounded_path_zero_depth_returns_seed_only() {
        let re = line_graph(10);
        let ctx = edge_ctx(&[], &[], Some(&re));
        let (reached, capped) = bounded_path(0, &[], 0, &ctx);
        assert_eq!(reached, vec![0], "depth_cap 0 walks no edges");
        assert!(!capped);
    }

    // ---------- run_entry wiring ----------

    #[test]
    fn resume_bounded_path_builds_rows() {
        // path(s, ...) plans a BoundedPath op; wire it through resume() end to end.
        let re = line_graph(5);
        let ctx = edge_ctx(&[], &[], Some(&re));
        let plan = crate::query::plan::QueryPlan {
            late_ops: vec![StageOp::BoundedPath { depth_cap: 2 }],
            ..Default::default()
        };
        let q = crate::query::parse::parse("SELECT * FROM C s").unwrap();
        let mut st = crate::query::execute::QueryExecState::new();
        st.push_cross_phase(0, "q_path".to_string(), plan, {
            let mut c = crate::query::carry::Carry::index_only(100);
            c.push_index(0);
            c
        });
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        // Seed 0 with depth 2 reaches {0,1,2} in a line graph.
        assert_eq!(r.row_count, 3, "bounded walk of depth 2 from 0 → {{0,1,2}}");
    }

    #[test]
    fn resume_edge_lookup_inbound_builds_rows() {
        // @inbounds plans an EdgeLookup{Inbound}; wire through resume().
        let in_off = [0u32, 0, 0, 0, 0, 0, 2];
        let in_tgt = [2u32, 3];
        let ctx = edge_ctx(&in_off, &in_tgt, None);
        let plan = crate::query::plan::QueryPlan {
            late_ops: vec![StageOp::EdgeLookup {
                dir: EdgeDir::Inbound,
            }],
            ..Default::default()
        };
        let q = crate::query::parse::parse("SELECT * FROM C s").unwrap();
        let mut st = crate::query::execute::QueryExecState::new();
        st.push_cross_phase(0, "q_in".to_string(), plan, {
            let mut c = crate::query::carry::Carry::index_only(100);
            c.push_index(5);
            c
        });
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(r.row_count, 2, "node 5's referrers are {{2,3}}");
    }
}

#[cfg(test)]
mod tostring_tests {
    use super::*;
    use crate::query::execute::QueryExecState;

    static EMPTY_ID_MAP: IdMap<'static> = IdMap { addr_of: &[] };

    /// Build a LateCtx with a caller-supplied string-values map.
    fn string_ctx<'a>(sv: &'a std::collections::HashMap<u32, String>) -> LateCtx<'a> {
        LateCtx {
            retained: &[],
            idom: &[],
            dc_off: &[],
            dc_tgt: &[],
            shallow: &[],
            id_map: &EMPTY_ID_MAP,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: sv,
            string_values_truncated: false,
        }
    }

    /// Helper: build a QueryExecState with one `ResolveStringValues` pending entry
    /// carrying the given dense indices.
    fn string_state(oql: &str, seeds: &[u32]) -> (QueryExecState, crate::query::ast::Query) {
        let q = crate::query::parse::parse(oql).unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        assert!(
            plan.needs.string_values,
            "query must arm string_values for this test: {oql}"
        );
        let mut carry = crate::query::carry::Carry::index_only(1000);
        for &s in seeds {
            carry.push_index(s);
        }
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q_sv".to_string(), plan, carry);
        (st, q)
    }

    #[test]
    fn string_values_rows_projects_decoded_text() {
        // dense 0 → "hello", dense 1 → "world". SELECT toString(s).
        let mut sv = std::collections::HashMap::new();
        sv.insert(0u32, "hello".to_string());
        sv.insert(1u32, "world".to_string());
        let ctx = string_ctx(&sv);

        let (st, q) = string_state("SELECT toString(s) FROM java.lang.String s", &[0, 1]);
        let out = resume(st, &[q.clone(), q], &ctx);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(r.row_count, 2);
        assert_eq!(
            r.rows[0][0],
            crate::query::model::QueryValue::Str("hello".to_string())
        );
        assert_eq!(
            r.rows[1][0],
            crate::query::model::QueryValue::Str("world".to_string())
        );
    }

    #[test]
    fn string_values_rows_uncaptured_is_null() {
        // dense 0 is NOT in the string-values map (cap overflow or absent).
        let sv: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let ctx = string_ctx(&sv);

        let (st, q) = string_state("SELECT toString(s) FROM java.lang.String s", &[0]);
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none());
        // An uncaptured String still produces a row with a Null cell (not an error).
        assert_eq!(r.row_count, 1, "uncaptured String must still produce a row");
        assert_eq!(r.rows[0][0], crate::query::model::QueryValue::Null);
    }

    #[test]
    fn string_values_rows_empty_carry_is_empty() {
        let sv: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let ctx = string_ctx(&sv);

        let (st, q) = string_state("SELECT toString(s) FROM java.lang.String s", &[]);
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none());
        assert_eq!(r.row_count, 0);
    }

    #[test]
    fn string_values_where_like_filters() {
        // dense 0 → "java.lang.String", dense 1 → "hello". Filter: LIKE "java\..*".
        // Only dense 0 should survive.
        let mut sv = std::collections::HashMap::new();
        sv.insert(0u32, "java.lang.String".to_string());
        sv.insert(1u32, "hello".to_string());
        let ctx = string_ctx(&sv);

        let (st, q) = string_state(
            r#"SELECT toString(s) FROM java.lang.String s WHERE toString(s) LIKE "java\..*""#,
            &[0, 1],
        );
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(
            r.row_count, 1,
            "only the java.lang.String value should pass"
        );
        assert_eq!(
            r.rows[0][0],
            crate::query::model::QueryValue::Str("java.lang.String".to_string())
        );
    }

    #[test]
    fn string_values_where_not_like_inverts() {
        // dense 0 → "java.lang.Object", dense 1 → "hello".
        // NOT LIKE "java\..*" passes only "hello".
        let mut sv = std::collections::HashMap::new();
        sv.insert(0u32, "java.lang.Object".to_string());
        sv.insert(1u32, "hello".to_string());
        let ctx = string_ctx(&sv);

        let (st, q) = string_state(
            r#"SELECT toString(s) FROM java.lang.String s WHERE toString(s) NOT LIKE "java\..*""#,
            &[0, 1],
        );
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(r.row_count, 1, "only 'hello' passes NOT LIKE 'java\\..*'");
        assert_eq!(
            r.rows[0][0],
            crate::query::model::QueryValue::Str("hello".to_string())
        );
    }

    #[test]
    fn string_values_limit_is_applied() {
        let mut sv = std::collections::HashMap::new();
        for i in 0u32..10 {
            sv.insert(i, format!("s{i}"));
        }
        let ctx = string_ctx(&sv);

        let (st, q) = string_state(
            "SELECT toString(s) FROM java.lang.String s LIMIT 3",
            &(0u32..10).collect::<Vec<_>>(),
        );
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none());
        assert_eq!(r.row_count, 3, "LIMIT 3 must cap at 3 rows");
        assert!(r.truncated, "exceeding LIMIT must mark truncated");
    }

    #[test]
    fn string_values_capture_truncated_sets_result_truncated() {
        // When the string-capture table overflowed during the scan,
        // ctx.string_values_truncated is true. The result must surface this
        // as QueryResult.truncated even when LIMIT is not hit, so the caller
        // knows the results are partial.
        let mut sv = std::collections::HashMap::new();
        sv.insert(0u32, "hello".to_string());
        // Build ctx with string_values_truncated = true to simulate cap overflow.
        let ctx = LateCtx {
            retained: &[],
            idom: &[],
            dc_off: &[],
            dc_tgt: &[],
            shallow: &[],
            id_map: &EMPTY_ID_MAP,
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
            refwalk_tails: &EMPTY_REFWALK_TAILS,
            refwalk_truncated: false,
            in_off: &[],
            in_tgt: &[],
            retained_edges: None,
            string_values: &sv,
            string_values_truncated: true,
        };

        let (st, q) = string_state("SELECT toString(s) FROM java.lang.String s", &[0]);
        let out = resume(st, &[q.clone(), q], &ctx);
        let r = &out[0];
        assert!(r.error.is_none());
        assert_eq!(r.row_count, 1, "one seed still produces one row");
        assert!(
            r.truncated,
            "cap-overflow during scan must set QueryResult.truncated"
        );
    }
}
