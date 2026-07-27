//! Late-phase query runner. Consumes the cross-phase carries in a
//! QueryExecState after dominators + retained sizes exist, applies each plan's
//! late_ops, and reassembles all results in original query order.

use crate::query::PATH_FRONTIER_CAP;
use crate::query::ast::{Attr, CompareOp, Expr, Predicate, Query, SelectItem, SortDir};
use crate::query::execute::{CrossPhaseEntry, QueryExecState, arith, compare_values, unary, value_to_qv};
use crate::query::model::{QueryColumn, QueryResult, QueryValue};
use crate::query::plan::StageOp;
use crate::query::runflags::EdgeDir;

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

/// A shared empty gc-root-tags table, used as the default `gc_root_tags` borrow
/// when no `@GCRoots`/`@GCRootInfo`/`@info` query ran (the common case). Keeps
/// non-gcroot runs byte/RSS-identical: nothing is built or borrowed beyond this
/// zero-entry map. Mirrors `EMPTY_STRING_VALUES`.
pub static EMPTY_GC_ROOT_TAGS: std::sync::LazyLock<std::collections::HashMap<u32, u8>> =
    std::sync::LazyLock::new(std::collections::HashMap::new);

/// Returns the row cap to use when collecting rows in a late stage.
/// When OFFSET is set, the stage must collect `limit + offset` rows so
/// `collapse_union_results` can drain the first `offset` then re-apply LIMIT.
fn stage_limit(q: &Query) -> Option<u64> {
    match (q.limit, q.offset) {
        (Some(lim), Some(off)) => Some(lim.saturating_add(off)),
        (Some(lim), None) => Some(lim),
        (None, _) => None,
    }
}

/// Human-readable label for an HPROF GC-root sub-tag (`types::heap::ROOT_*`), for
/// `@GCRoots`/`@GCRootInfo`/`@info` in analyze mode. Labels follow MAT's GC-root
/// naming (matching `report::format::gc_root_type_label`); an unrecognised code
/// falls back to `"root tag N"` so the value is never silently empty for a root.
pub fn root_tag_name(tag: u8) -> std::borrow::Cow<'static, str> {
    use crate::types::heap;
    let name = match tag {
        heap::ROOT_SYSTEM_CLASS => "System Class",
        heap::ROOT_JNI_GLOBAL => "JNI Global",
        heap::ROOT_JNI_LOCAL => "JNI Local",
        heap::ROOT_JAVA_FRAME => "Java Frame",
        heap::ROOT_NATIVE_STACK => "Native Stack",
        heap::ROOT_STICKY_CLASS => "Sticky Class",
        heap::ROOT_THREAD_BLOCK => "Thread Block",
        heap::ROOT_MONITOR_USED => "Busy Monitor",
        heap::ROOT_THREAD_OBJ => "Thread",
        heap::ROOT_UNKNOWN => "Unknown",
        // Any code outside the known HPROF sub-tag set: surface the numeric tag
        // rather than a misleading "Unknown" so the value stays diagnosable.
        other => return std::borrow::Cow::Owned(format!("root tag {other}")),
    };
    std::borrow::Cow::Borrowed(name)
}

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
    /// GC-root sub-tag per root object: `dense_idx → heap::ROOT_*`. Populated
    /// (query-gated) only when a `@GCRoots`/`@GCRootInfo`/`@info` query armed
    /// `needs.gc_roots`; empty otherwise (non-gcroot runs keep the shared
    /// `EMPTY_GC_ROOT_TAGS` borrow, byte/RSS-identical to before). A dense index
    /// absent from the map is a non-root object → `@GCRootInfo`/`@info` project
    /// `Null`. Built by zipping `g.gc_root_indices` with `g.gc_root_types` (the
    /// two are aligned 1:1 by construction — see `pass2::mod` and
    /// `report::build`), so no address→dense re-derivation is needed.
    pub gc_root_tags: &'a std::collections::HashMap<u32, u8>,
    /// Per-object class histogram row index, 1:1 with dense object indices.
    /// `class_names[class_idx[dense]]` gives the object's class name. Empty when
    /// not needed (non-classof queries) — `class_name_of` returns `None`.
    pub class_idx: &'a [u32],
    /// Class histogram row names, indexed by the values in `class_idx`.
    pub class_names: &'a [String],
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

    /// The GC-root sub-tag of the object at `dense`, or `None` if it is not a
    /// GC root (or no gcroot query armed the map). `@GCRootInfo`/`@GCRoots`
    /// project the tag's label for a root and `Null` for a non-root.
    pub fn gc_root_tag(&self, dense: u32) -> Option<u8> {
        self.gc_root_tags.get(&dense).copied()
    }

    /// The class name of the object at `dense`, or `None` when class data is
    /// not available (empty `class_idx` means not threaded into this window).
    pub fn class_name_of(&self, dense: u32) -> Option<&str> {
        let row = *self.class_idx.get(dense as usize)? as usize;
        self.class_names.get(row).map(String::as_str)
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
        } else if entry.plan.needs.gc_roots {
            "@GCRoots/@GCRootInfo/@info require the full analysis pipeline; \
             GC-root data is not collected in the query-only path. Run the full \
             report (drop --query-only) to use GC-root queries."
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
                viz: None,
                elapsed_ms: None,
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
                return dominator_rows(entry, q, &children, truncated);
            }
            StageOp::DominatorOf => {
                let idx: Vec<u32> = entry.carry.indices();
                let idoms = run_dominator_of(&idx, ctx);
                return dominator_rows(entry, q, &idoms, entry.carry.truncated());
            }
            StageOp::RetainedSet { cap } => {
                let seeds: Vec<u32> = entry.carry.indices();
                let (set, trunc) = run_retained_set(&seeds, *cap, ctx);
                let truncated = entry.carry.truncated() || trunc;
                return dominator_rows(entry, q, &set, truncated);
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
                return dominator_rows(entry, q, &neighbours, entry.carry.truncated());
            }
            StageOp::BoundedPath { depth_cap } => {
                // Bounded forward walk from each carried seed; concatenate reached
                // nodes. `target_rows` is empty by design (parity-lite): path(a, b)
                // emits the bounded forward-reachable subgraph from the FROM seeds.
                // `to`-operand early-stop needs a per-index class map we intentionally
                // don't keep (MEMORY-CRITICAL: no flat per-object Vec<u32>).
                let seeds: Vec<u32> = entry.carry.indices();
                let mut reached = Vec::new();
                let mut capped = false;
                for s in seeds {
                    let (nodes, c) = bounded_path(s, &[], *depth_cap, ctx);
                    reached.extend(nodes);
                    capped |= c;
                }
                return dominator_rows(entry, q, &reached, entry.carry.truncated() || capped);
            }
            StageOp::ResolveStringValues => {
                return string_values_rows(entry, q, ctx);
            }
            StageOp::ResolveArrayIndex => {
                return array_index_rows(entry, q, ctx);
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
                    viz: None,
                    elapsed_ms: None,
                };
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
) -> QueryResult {
    let mut indices = indices.to_vec();
    if let Some(limit) = stage_limit(q) {
        if indices.len() as u64 > limit {
            indices.truncate(limit as usize);
            truncated = true;
        }
    }
    let col = q
        .select_aliases
        .first()
        .and_then(|o| o.as_deref())
        .map(|s| s.to_string())
        .or_else(|| {
            q.select
                .first()
                .map(crate::query::execute::column_name)
        })
        .unwrap_or_else(|| "*".to_string());
    let rows: Vec<Vec<QueryValue>> = indices
        .iter()
        .map(|&i| {
            vec![QueryValue::ObjRef {
                // Dense index (see the string/scan `SELECT *` note): the late
                // id_map is empty, so `to_addr` would render every row as `@0`.
                index: i as u64,
                class: "?".to_string(),
                addr: None,
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
        viz: None,
        elapsed_ms: None,
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

    let columns: Vec<QueryColumn> = crate::query::execute::query_columns(q);
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
                SelectItem::Attr(Attr::ObjectAddress) => {
                    QueryValue::Int(ctx.id_map.to_addr(s) as i64)
                }
                SelectItem::Attr(Attr::UsedHeapSize) => QueryValue::Int(
                    ctx.shallow.get(s as usize).copied().unwrap_or(0) as i64,
                ),
                SelectItem::Attr(Attr::RetainedHeapSize) => QueryValue::Int(
                    ctx.retained.get(s as usize).copied().unwrap_or(0) as i64,
                ),
                SelectItem::Attr(Attr::ClassOf) | SelectItem::Attr(Attr::DisplayName) => {
                    match ctx.class_name_of(s) {
                        Some(name) => QueryValue::Str(name.to_string()),
                        None => QueryValue::Null,
                    }
                }
                SelectItem::Attr(Attr::GcRootInfo) | SelectItem::Attr(Attr::GcRoots) => {
                    match ctx.gc_root_tag(s) {
                        Some(tag) => QueryValue::Str(root_tag_name(tag).into_owned()),
                        None => QueryValue::Null,
                    }
                }
                SelectItem::Attr(Attr::ToHex(inner)) => {
                    let ret = ctx.retained.get(s as usize).copied().unwrap_or(0) as u64;
                    match eval_late_expr_multi(inner, s, ret, ctx, &like_regexes) {
                        QueryValue::Int(n) => QueryValue::Str(format!("0x{:x}", n as u64)),
                        _ => QueryValue::Null,
                    }
                }
                SelectItem::Expr(e) => {
                    let ret = ctx.retained.get(s as usize).copied().unwrap_or(0) as u64;
                    eval_late_expr_multi(e, s, ret, ctx, &like_regexes)
                }
                SelectItem::Star => QueryValue::ObjRef {
                    // Emit the DENSE object index (matching the scan path's
                    // `SELECT *`, execute.rs), not an address: the late id_map is
                    // intentionally empty (the dense address table is compressed
                    // away to protect the RSS peak), so `to_addr` would yield 0.
                    index: s as u64,
                    class: "?".to_string(),
                    addr: None,
                },
                _ => QueryValue::Null,
            })
            .collect();
        rows.push(row);
    }

    let mut truncated = entry.carry.truncated() || ctx.refwalk_truncated;
    if let Some(limit) = stage_limit(q) {
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
        viz: None,
        elapsed_ms: None,
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

    let columns: Vec<QueryColumn> = crate::query::execute::query_columns(q);

    // Aggregate over the toString-filtered set.
    if q.select.iter().any(|it| matches!(it, SelectItem::Aggregate { .. })) {
        let truncated = entry.carry.truncated() || ctx.string_values_truncated;

        // GROUP BY path: group by the toString(s) key, one row per distinct value.
        if !q.group_by.is_empty() {
            // Key: string value (None → Null group). Value: (key_val, accs).
            let mut group_map: std::collections::HashMap<
                Option<String>,
                (QueryValue, Vec<crate::query::execute::AggAcc>),
            > = std::collections::HashMap::new();
            for &idx in &kept {
                let key_val: QueryValue = ctx
                    .string_value(idx)
                    .map(|s| QueryValue::Str(s.to_string()))
                    .unwrap_or(QueryValue::Null);
                let key_opt: Option<String> = ctx.string_value(idx).map(|s| s.to_string());
                let entry_ref = group_map.entry(key_opt).or_insert_with(|| {
                    let init: Vec<crate::query::execute::AggAcc> =
                        q.select.iter().map(crate::query::execute::init_agg_acc).collect();
                    (key_val.clone(), init)
                });
                for (acc, item) in entry_ref.1.iter_mut().zip(q.select.iter()) {
                    if let SelectItem::Aggregate { arg, .. } = item {
                        let v = project_string_row_item(arg, idx, ctx, &like_regexes);
                        crate::query::execute::fold_agg_acc(acc, v);
                    }
                }
            }
            let mut out_rows: Vec<Vec<QueryValue>> = group_map
                .into_values()
                .map(|(key_val, accs)| {
                    let finalized: Vec<QueryValue> =
                        accs.into_iter().map(crate::query::execute::finalize_agg_acc).collect();
                    // Build one output row: aggregates from finalized accs, non-aggregates
                    // (i.e. the toString(s) GROUP BY key projection) from key_val.
                    q.select
                        .iter()
                        .enumerate()
                        .map(|(i, item)| match item {
                            SelectItem::Aggregate { .. } => {
                                finalized.get(i).cloned().unwrap_or(QueryValue::Null)
                            }
                            _ => key_val.clone(),
                        })
                        .collect()
                })
                .collect();
            // Apply HAVING filter post-aggregation.
            if !entry.plan.having_terms.is_empty() {
                out_rows.retain(|row| {
                    entry.plan.having_terms.iter().all(|term| {
                        crate::query::execute::eval_having_term(&term.pred, row, q, &columns, &like_regexes)
                    })
                });
            }
            if let Some(ob) = &q.order_by {
                if let Some(ci) =
                    crate::query::execute::order_by_column_index(q, &columns, &ob.key)
                {
                    crate::query::execute::sort_rows_by_column(&mut out_rows, ci, ob.dir);
                }
            }
            let mut truncated = truncated;
            if let Some(limit) = stage_limit(q) {
                if out_rows.len() as u64 > limit {
                    out_rows.truncate(limit as usize);
                    truncated = true;
                }
            }
            return QueryResult {
                name: entry.name.clone(),
                oql: String::new(),
                columns,
                row_count: out_rows.len() as u64,
                rows: out_rows,
                truncated,
                error: None,
                note: None,
                viz: None,
                elapsed_ms: None,
            };
        }

        // No GROUP BY: fold entire filtered set into one aggregate row.
        // COUNT(*) and COUNT(toString(s)) are the only supported aggregates here
        // (plan.rs gates everything else). Per-object arg value projected via
        // `project_string_row_item` (COUNT(*) ignores it; COUNT(toString(s)) sees
        // the decoded string).
        let mut accs: Vec<crate::query::execute::AggAcc> =
            q.select.iter().map(crate::query::execute::init_agg_acc).collect();
        for &idx in &kept {
            for (acc, item) in accs.iter_mut().zip(q.select.iter()) {
                if let SelectItem::Aggregate { arg, .. } = item {
                    let v = project_string_row_item(arg, idx, ctx, &like_regexes);
                    crate::query::execute::fold_agg_acc(acc, v);
                }
            }
        }
        let row: Vec<QueryValue> = accs
            .into_iter()
            .map(crate::query::execute::finalize_agg_acc)
            .collect();
        return QueryResult {
            name: entry.name.clone(),
            oql: String::new(),
            columns,
            row_count: 1,
            rows: vec![row],
            truncated,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
    }

    let out_rows: Vec<Vec<QueryValue>> = kept
        .iter()
        .map(|&idx| {
            q.select
                .iter()
                .map(|it| project_string_row_item(it, idx, ctx, &like_regexes))
                .collect()
        })
        .collect();

    let mut truncated = entry.carry.truncated() || ctx.string_values_truncated;
    let mut out_rows = out_rows;

    if let Some(ob) = &q.order_by {
        if let Some(ci) = crate::query::execute::order_by_column_index(q, &columns, &ob.key) {
            crate::query::execute::sort_rows_by_column(&mut out_rows, ci, ob.dir);
        }
    }

    if let Some(limit) = stage_limit(q) {
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
        viz: None,
        elapsed_ms: None,
    }
}

// ── Late-phase arithmetic helpers ────────────────────────────────────────────

/// Walk an Expr tree and return `true` if ANY `Attr` leaf satisfies `pred`.
/// Used to detect whether a Compare arm involves a specific late attr.
fn expr_has_attr(e: &Expr, pred: &impl Fn(&Attr) -> bool) -> bool {
    match e {
        Expr::Attr(a) => pred(a),
        Expr::Lit(_) => false,
        Expr::Binary { lhs, rhs, .. } => expr_has_attr(lhs, pred) || expr_has_attr(rhs, pred),
        Expr::Unary { arg, .. } => expr_has_attr(arg, pred),
        Expr::Method { receiver, args, .. } => // D2 fills this
            expr_has_attr(receiver, pred) || args.iter().any(|a| expr_has_attr(a, pred)),
        Expr::Aggregate { .. } => false,
        Expr::Case { branches, else_ } => {
            let pred: &dyn Fn(&Attr) -> bool = pred;
            branches.iter().any(|(cond, then_e)| {
                pred_has_attr_dyn(cond, pred) || expr_has_attr_dyn(then_e, pred)
            }) || else_.as_ref().map_or(false, |e| expr_has_attr_dyn(e, pred))
        }
        Expr::Coalesce(args) => args.iter().any(|a| expr_has_attr(a, pred)),
        Expr::NullIf { lhs, rhs } => expr_has_attr(lhs, pred) || expr_has_attr(rhs, pred),
    }
}
fn expr_has_attr_dyn(e: &Expr, pred: &dyn Fn(&Attr) -> bool) -> bool {
    match e {
        Expr::Attr(a) => pred(a),
        Expr::Lit(_) => false,
        Expr::Binary { lhs, rhs, .. } => expr_has_attr_dyn(lhs, pred) || expr_has_attr_dyn(rhs, pred),
        Expr::Unary { arg, .. } => expr_has_attr_dyn(arg, pred),
        Expr::Method { receiver, args, .. } =>
            expr_has_attr_dyn(receiver, pred) || args.iter().any(|a| expr_has_attr_dyn(a, pred)),
        Expr::Aggregate { .. } => false,
        Expr::Case { branches, else_ } => {
            branches.iter().any(|(cond, then_e)| {
                pred_has_attr_dyn(cond, pred) || expr_has_attr_dyn(then_e, pred)
            }) || else_.as_ref().map_or(false, |e| expr_has_attr_dyn(e, pred))
        }
        Expr::Coalesce(args) => args.iter().any(|a| expr_has_attr_dyn(a, pred)),
        Expr::NullIf { lhs, rhs } => expr_has_attr_dyn(lhs, pred) || expr_has_attr_dyn(rhs, pred),
    }
}
fn pred_has_attr_dyn(p: &Predicate, pred: &dyn Fn(&Attr) -> bool) -> bool {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_has_attr_dyn(a, pred) || pred_has_attr_dyn(b, pred)
        }
        Predicate::Not(a) => pred_has_attr_dyn(a, pred),
        Predicate::Compare { lhs, rhs, .. } => expr_has_attr_dyn(lhs, pred) || expr_has_attr_dyn(rhs, pred),
        Predicate::InstanceOf(_) | Predicate::InSubquery { .. } | Predicate::Exists { .. } => false,
    }
}
fn expr_find_attr<'e>(e: &'e Expr, pred: &impl Fn(&Attr) -> bool) -> Option<&'e Attr> {
    match e {
        Expr::Attr(a) if pred(a) => Some(a),
        Expr::Attr(_) | Expr::Lit(_) => None,
        Expr::Binary { lhs, rhs, .. } => {
            expr_find_attr(lhs, pred).or_else(|| expr_find_attr(rhs, pred))
        }
        Expr::Unary { arg, .. } => expr_find_attr(arg, pred),
        Expr::Method { receiver, args, .. } => // D2 fills this
            expr_find_attr(receiver, pred).or_else(|| args.iter().find_map(|a| expr_find_attr(a, pred))),
        Expr::Aggregate { .. } => None,
        Expr::Case { branches, else_ } => {
            let pred: &dyn Fn(&Attr) -> bool = pred;
            branches.iter().find_map(|(cond, then_e)| {
                pred_find_attr_dyn(cond, pred).or_else(|| expr_find_attr_dyn(then_e, pred))
            }).or_else(|| else_.as_ref().and_then(|e| expr_find_attr_dyn(e, pred)))
        }
        Expr::Coalesce(args) => args.iter().find_map(|a| expr_find_attr(a, pred)),
        Expr::NullIf { lhs, rhs } => expr_find_attr(lhs, pred).or_else(|| expr_find_attr(rhs, pred)),
    }
}
fn expr_find_attr_dyn<'e>(e: &'e Expr, pred: &dyn Fn(&Attr) -> bool) -> Option<&'e Attr> {
    match e {
        Expr::Attr(a) if pred(a) => Some(a),
        Expr::Attr(_) | Expr::Lit(_) => None,
        Expr::Binary { lhs, rhs, .. } => {
            expr_find_attr_dyn(lhs, pred).or_else(|| expr_find_attr_dyn(rhs, pred))
        }
        Expr::Unary { arg, .. } => expr_find_attr_dyn(arg, pred),
        Expr::Method { receiver, args, .. } =>
            expr_find_attr_dyn(receiver, pred).or_else(|| args.iter().find_map(|a| expr_find_attr_dyn(a, pred))),
        Expr::Aggregate { .. } => None,
        Expr::Case { branches, else_ } => {
            branches.iter().find_map(|(cond, then_e)| {
                pred_find_attr_dyn(cond, pred).or_else(|| expr_find_attr_dyn(then_e, pred))
            }).or_else(|| else_.as_ref().and_then(|e| expr_find_attr_dyn(e, pred)))
        }
        Expr::Coalesce(args) => args.iter().find_map(|a| expr_find_attr_dyn(a, pred)),
        Expr::NullIf { lhs, rhs } => expr_find_attr_dyn(lhs, pred).or_else(|| expr_find_attr_dyn(rhs, pred)),
    }
}
fn pred_find_attr_dyn<'e>(p: &'e Predicate, pred: &dyn Fn(&Attr) -> bool) -> Option<&'e Attr> {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_find_attr_dyn(a, pred).or_else(|| pred_find_attr_dyn(b, pred))
        }
        Predicate::Not(a) => pred_find_attr_dyn(a, pred),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_find_attr_dyn(lhs, pred).or_else(|| expr_find_attr_dyn(rhs, pred))
        }
        Predicate::InstanceOf(_) | Predicate::InSubquery { .. } => None,
        Predicate::Exists { .. } => None,
    }
}
/// "known" (its resolved value passed as `known`), identified by `is_known`.
/// Any other Attr leaf is unknown at late phase → `QueryValue::Null`. Literals
/// fold; Binary/Unary compose with Java arithmetic semantics (from execute.rs).
fn eval_late_expr(
    e: &Expr,
    is_known: &impl Fn(&Attr) -> bool,
    known: &QueryValue,
) -> QueryValue {
    match e {
        Expr::Attr(a) if is_known(a) => known.clone(),
        Expr::Attr(_) => QueryValue::Null, // unknown at late phase
        Expr::Lit(v) => value_to_qv(v),
        Expr::Binary { op, lhs, rhs } => arith(
            &eval_late_expr(lhs, is_known, known),
            *op,
            &eval_late_expr(rhs, is_known, known),
        ),
        Expr::Unary { op, arg } => unary(*op, &eval_late_expr(arg, is_known, known)),
        Expr::Method { .. } => QueryValue::Null, // D2 fills this
        Expr::Aggregate { .. } => QueryValue::Null, // evaluated in GROUP BY finalization, not late-phase
        Expr::Case { branches, else_ } => {
            for (cond, then_expr) in branches {
                if eval_late_pred(cond, is_known, known) {
                    return eval_late_expr(then_expr, is_known, known);
                }
            }
            match else_ {
                Some(e) => eval_late_expr(e, is_known, known),
                None => QueryValue::Null,
            }
        }
        Expr::Coalesce(args) => {
            for arg in args {
                let v = eval_late_expr(arg, is_known, known);
                if !matches!(v, QueryValue::Null) { return v; }
            }
            QueryValue::Null
        }
        Expr::NullIf { lhs, rhs } => {
            let lv = eval_late_expr(lhs, is_known, known);
            let rv = eval_late_expr(rhs, is_known, known);
            if lv == rv { QueryValue::Null } else { lv }
        }
    }
}

/// Evaluate a predicate using the same late-window "known attr" semantics as
/// `eval_late_expr`. Used by `eval_late_expr`'s `Expr::Case` arm.
fn eval_late_pred(
    p: &Predicate,
    is_known: &impl Fn(&Attr) -> bool,
    known: &QueryValue,
) -> bool {
    use crate::query::ast::CompareOp;
    match p {
        Predicate::And(a, b) => eval_late_pred(a, is_known, known) && eval_late_pred(b, is_known, known),
        Predicate::Or(a, b) => eval_late_pred(a, is_known, known) || eval_late_pred(b, is_known, known),
        Predicate::Not(a) => !eval_late_pred(a, is_known, known),
        Predicate::Compare { lhs, op, rhs } => {
            let lv = eval_late_expr(lhs, is_known, known);
            let rv = eval_late_expr(rhs, is_known, known);
            // Build a simple like_regexes map on the fly for LIKE predicates.
            let mut like_re_map = std::collections::HashMap::new();
            if matches!(op, CompareOp::Like | CompareOp::NotLike) {
                if let QueryValue::Str(pattern) = &rv {
                    if let Ok(re) = regex::Regex::new(pattern) {
                        like_re_map.insert(pattern.clone(), re);
                    }
                }
            }
            cmp_late_qv(&lv, *op, &rv, &like_re_map)
        }
        // InstanceOf and InSubquery are not expected in CASE WHEN conditions
        // (no heap-type or subquery syntax used inside WHEN predicates).
        Predicate::InstanceOf(_) | Predicate::InSubquery { .. } | Predicate::Exists { .. } => false,
    }
}

/// Evaluate an expression in the late window where multiple late attrs are live:
/// @objectId, @retainedHeapSize, @usedHeapSize, @objectAddress, classof/displayName.
/// Blob-dependent field attrs (instance scalars, ref paths) remain Null.
fn eval_late_expr_multi(
    e: &Expr,
    idx: u32,
    ret: u64,
    ctx: &LateCtx,
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> QueryValue {
    use crate::query::ast::Attr;
    match e {
        Expr::Attr(a) => match a {
            Attr::ObjectId => QueryValue::Int(idx as i64),
            Attr::RetainedHeapSize => QueryValue::Int(ret as i64),
            Attr::UsedHeapSize => QueryValue::Int(
                ctx.shallow.get(idx as usize).copied().unwrap_or(0) as i64,
            ),
            Attr::ObjectAddress => QueryValue::Int(ctx.id_map.to_addr(idx) as i64),
            Attr::ClassOf | Attr::DisplayName => match ctx.class_name_of(idx) {
                Some(name) => QueryValue::Str(name.to_string()),
                None => QueryValue::Null,
            },
            Attr::GcRoots | Attr::GcRootInfo => match ctx.gc_root_tag(idx) {
                Some(tag) => QueryValue::Str(root_tag_name(tag).into_owned()),
                None => QueryValue::Null,
            },
            Attr::ToHex(inner) => {
                let inner_expr: &Expr = inner;
                match eval_late_expr_multi(inner_expr, idx, ret, ctx, like_regexes) {
                    QueryValue::Int(n) => QueryValue::Str(format!("0x{:x}", n as u64)),
                    _ => QueryValue::Null,
                }
            }
            _ => QueryValue::Null,
        },
        Expr::Lit(v) => value_to_qv(v),
        Expr::Binary { op, lhs, rhs } => arith(
            &eval_late_expr_multi(lhs, idx, ret, ctx, like_regexes),
            *op,
            &eval_late_expr_multi(rhs, idx, ret, ctx, like_regexes),
        ),
        Expr::Unary { op, arg } => {
            unary(*op, &eval_late_expr_multi(arg, idx, ret, ctx, like_regexes))
        }
        Expr::Case { branches, else_ } => {
            for (cond, then_expr) in branches {
                if eval_late_pred_multi(cond, idx, ret, ctx, like_regexes) {
                    return eval_late_expr_multi(then_expr, idx, ret, ctx, like_regexes);
                }
            }
            match else_ {
                Some(e) => eval_late_expr_multi(e, idx, ret, ctx, like_regexes),
                None => QueryValue::Null,
            }
        }
        Expr::Coalesce(args) => {
            for arg in args {
                let v = eval_late_expr_multi(arg, idx, ret, ctx, like_regexes);
                if !matches!(v, QueryValue::Null) { return v; }
            }
            QueryValue::Null
        }
        Expr::NullIf { lhs, rhs } => {
            let lv = eval_late_expr_multi(lhs, idx, ret, ctx, like_regexes);
            let rv = eval_late_expr_multi(rhs, idx, ret, ctx, like_regexes);
            if lv == rv { QueryValue::Null } else { lv }
        }
        Expr::Aggregate { .. } | Expr::Method { .. } => QueryValue::Null,
    }
}

fn eval_late_pred_multi(
    p: &Predicate,
    idx: u32,
    ret: u64,
    ctx: &LateCtx,
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> bool {
    match p {
        Predicate::And(a, b) => {
            eval_late_pred_multi(a, idx, ret, ctx, like_regexes)
                && eval_late_pred_multi(b, idx, ret, ctx, like_regexes)
        }
        Predicate::Or(a, b) => {
            eval_late_pred_multi(a, idx, ret, ctx, like_regexes)
                || eval_late_pred_multi(b, idx, ret, ctx, like_regexes)
        }
        Predicate::Not(a) => !eval_late_pred_multi(a, idx, ret, ctx, like_regexes),
        Predicate::Compare { lhs, op, rhs } => {
            let lv = eval_late_expr_multi(lhs, idx, ret, ctx, like_regexes);
            let rv = eval_late_expr_multi(rhs, idx, ret, ctx, like_regexes);
            cmp_late_qv(&lv, *op, &rv, like_regexes)
        }
        _ => true,
    }
}


/// the RHS is expected to be a `QueryValue::Str` containing the pattern, and
/// `like_regexes` is consulted. Null operands: only `Ne` and `NotLike` are true
/// (type-mismatch behaviour consistent with `cmp_query_value`).
fn cmp_late_qv(
    lv: &QueryValue,
    op: CompareOp,
    rv: &QueryValue,
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> bool {
    let like_re: Option<&regex::Regex> = if matches!(op, CompareOp::Like | CompareOp::NotLike) {
        if let QueryValue::Str(pattern) = rv {
            like_regexes.get(pattern.as_str())
        } else {
            None
        }
    } else {
        None
    };
    compare_values(lv, op, rv, like_re)
}

/// True if the predicate tree contains any `Attr::ToString` comparison.
/// Widened: the ToString attr may appear anywhere inside the Compare's lhs or
/// rhs expression (e.g. `toString(s) + 1 = 5` has it buried in a Binary).
fn has_to_string_pred(p: &Predicate) -> bool {
    fn is_tostring(a: &Attr) -> bool { matches!(a, Attr::ToString(_)) }
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            has_to_string_pred(a) || has_to_string_pred(b)
        }
        Predicate::Not(a) => has_to_string_pred(a),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_has_attr(lhs, &is_tostring) || expr_has_attr(rhs, &is_tostring)
        }
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
        Predicate::Compare { lhs, op, rhs } => {
            // Detect whether this Compare involves a ToString attr anywhere in
            // lhs or rhs. If not, it was applied in Phase 1 and passes here.
            let is_tostring = |a: &Attr| matches!(a, Attr::ToString(_));
            if !expr_has_attr(lhs, &is_tostring) && !expr_has_attr(rhs, &is_tostring) {
                return true;
            }
            // Resolve the toString attr for this dense index.
            let string_qv = match ctx.string_value(dense) {
                Some(s) => QueryValue::Str(s.to_string()),
                None => return false, // not captured → no match
            };
            let lv = eval_late_expr(lhs, &is_tostring, &string_qv);
            let rv = eval_late_expr(rhs, &is_tostring, &string_qv);
            cmp_late_qv(&lv, *op, &rv, like_regexes)
        }
        // Non-toString predicates were already applied at scan time (Phase 1).
        _ => true,
    }
}

/// Project a single SELECT item for a toString(s) result row.
fn project_string_row_item(
    it: &SelectItem,
    dense: u32,
    ctx: &LateCtx,
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> QueryValue {
    match it {
        SelectItem::ToString(_) | SelectItem::Attr(Attr::ToString(_)) => ctx
            .string_value(dense)
            .map(|s| QueryValue::Str(s.to_string()))
            .unwrap_or(QueryValue::Null),
        SelectItem::Attr(Attr::ObjectId) => QueryValue::Int(dense as i64),
        SelectItem::Attr(Attr::ObjectAddress) => QueryValue::Int(ctx.id_map.to_addr(dense) as i64),
        SelectItem::Attr(Attr::UsedHeapSize) => QueryValue::Int(
            ctx.shallow.get(dense as usize).copied().unwrap_or(0) as i64,
        ),
        SelectItem::Attr(Attr::RetainedHeapSize) => QueryValue::Int(
            ctx.retained.get(dense as usize).copied().unwrap_or(0) as i64,
        ),
        SelectItem::Attr(Attr::ClassOf) | SelectItem::Attr(Attr::DisplayName) => {
            match ctx.class_name_of(dense) {
                Some(name) => QueryValue::Str(name.to_string()),
                None => QueryValue::Null,
            }
        }
        SelectItem::Attr(Attr::GcRootInfo) | SelectItem::Attr(Attr::GcRoots) => {
            match ctx.gc_root_tag(dense) {
                Some(tag) => QueryValue::Str(root_tag_name(tag).into_owned()),
                None => QueryValue::Null,
            }
        }
        SelectItem::Attr(Attr::ToHex(inner)) => {
            let ret = ctx.retained.get(dense as usize).copied().unwrap_or(0) as u64;
            match eval_late_expr_multi(inner, dense, ret, ctx, like_regexes) {
                QueryValue::Int(n) => QueryValue::Str(format!("0x{:x}", n as u64)),
                _ => QueryValue::Null,
            }
        }
        SelectItem::Expr(e) => {
            let ret = ctx.retained.get(dense as usize).copied().unwrap_or(0) as u64;
            eval_late_expr_multi(e, dense, ret, ctx, like_regexes)
        }
        SelectItem::Star => QueryValue::ObjRef {
            // Dense index, matching the scan-path `SELECT *` convention: the late
            // id_map is empty (address table compressed away), so `to_addr` here
            // would yield a misleading `@0` for every row.
            index: dense as u64,
            class: "java.lang.String".to_string(),
            addr: None,
        },
        _ => QueryValue::Null,
    }
}

/// Project a single SELECT item for an `array_index_rows` row from a dense object
/// index. `ArrayIndex`/`ArraySlice` items are handled by the caller (they always
/// project `Null` at this stage). All other items are resolved here using the
/// same late-phase data available in `LateCtx`. `class_name` is the query's
/// FROM class name, used to resolve `@displayName`/`@classOf` — every row in an
/// array-index result came from the same FROM class, mirroring how `execute.rs`
/// resolves these attrs for array rows at scan time.
fn project_array_index_item(
    it: &SelectItem,
    dense: u32,
    ctx: &LateCtx,
    class_name: &str,
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> QueryValue {
    match it {
        SelectItem::Attr(Attr::ObjectId) => QueryValue::Int(dense as i64),
        SelectItem::Attr(Attr::ObjectAddress) => {
            QueryValue::Int(ctx.id_map.to_addr(dense) as i64)
        }
        SelectItem::Attr(Attr::RetainedHeapSize) => QueryValue::Int(
            ctx.retained.get(dense as usize).copied().unwrap_or(0) as i64,
        ),
        SelectItem::Attr(Attr::UsedHeapSize) => QueryValue::Int(
            ctx.shallow.get(dense as usize).copied().unwrap_or(0) as i64,
        ),
        SelectItem::Attr(Attr::GcRootInfo) | SelectItem::Attr(Attr::GcRoots) => {
            match ctx.gc_root_tag(dense) {
                Some(tag) => QueryValue::Str(root_tag_name(tag).into_owned()),
                None => QueryValue::Null,
            }
        }
        // `@displayName` and `@classOf` both return the class name of the matched
        // object. Every row in an array-index result came from the query's FROM
        // class, so the FROM class name is the correct value here — consistent with
        // how `execute.rs` resolves these attrs for array rows during the scan.
        SelectItem::Attr(Attr::DisplayName) | SelectItem::Attr(Attr::ClassOf) => {
            QueryValue::Str(class_name.to_string())
        }
        SelectItem::Attr(Attr::ToHex(inner)) => {
            let ret = ctx.retained.get(dense as usize).copied().unwrap_or(0) as u64;
            match eval_late_expr_multi(inner, dense, ret, ctx, like_regexes) {
                QueryValue::Int(n) => QueryValue::Str(format!("0x{:x}", n as u64)),
                _ => QueryValue::Null,
            }
        }
        SelectItem::Expr(e) => {
            let ret = ctx.retained.get(dense as usize).copied().unwrap_or(0) as u64;
            eval_late_expr_multi(e, dense, ret, ctx, like_regexes)
        }
        SelectItem::Star => QueryValue::ObjRef {
            index: dense as u64,
            class: class_name.to_string(),
            addr: None,
        },
        _ => QueryValue::Null,
    }
}

/// Produce result rows for a query with `needs.array_index` (contains at least one
/// `base[index]` or `base[start:end]` expression). In this release, array element
/// data is not captured during the scan, so all `ArrayIndex`/`ArraySlice` columns
/// project `Null`. Other columns (`@objectId`, `*`, etc.) are projected normally
/// from the carried dense indices. Out-of-bounds and non-resolvable bases both
/// yield `Null` without error, matching the AST contract. Limit is applied.
fn array_index_rows(entry: &CrossPhaseEntry, q: &Query, ctx: &LateCtx) -> QueryResult {
    let like_regexes = crate::query::execute::compile_like_regexes(q).unwrap_or_default();
    let seeds: Vec<u32> = entry.carry.indices();
    let columns: Vec<QueryColumn> = crate::query::execute::query_columns(q);
    let class_name = q.from.class_name();
    let mut rows: Vec<Vec<QueryValue>> = Vec::new();
    for &s in &seeds {
        let row: Vec<QueryValue> = q
            .select
            .iter()
            .map(|it| match it {
                // ArrayIndex and ArraySlice: element data not yet captured → Null.
                SelectItem::Attr(Attr::ArrayIndex { .. })
                | SelectItem::Attr(Attr::ArraySlice { .. }) => QueryValue::Null,
                // All other columns: resolve normally from the dense index using
                // the same late-phase projection as other stage entry functions.
                _ => project_array_index_item(it, s, ctx, class_name, &like_regexes),
            })
            .collect();
        rows.push(row);
    }
    let mut truncated = entry.carry.truncated();
    if let Some(limit) = stage_limit(q) {
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
        note: Some(
            "array element data is not yet captured during the scan; \
             ArrayIndex/ArraySlice columns project Null in this release."
                .to_string(),
        ),
        viz: None,
        elapsed_ms: None,
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
        // `@objectAddress` tail (e.g. `e.getKey()` → `RefPath{tail:ObjectAddress}`).
        // The dense→address table is compressed away before the late window
        // (`id_map` is empty in both the report and query paths), so the walked-to
        // object's address is captured at scan time keyed by its own dense index
        // (see `capture_refwalk`) and read back from the tail table here. Fall back
        // to `id_map.to_addr` for contexts that do populate it (unit tests).
        Attr::ObjectAddress => match ctx.refwalk_tail(dense) {
            Some(v) => Some(v.clone()),
            None => Some(QueryValue::Int(ctx.id_map.to_addr(dense) as i64)),
        },
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
        // `@length` tail (e.g. `s.value.@length`): the resolved target is the
        // backing array, whose element count was captured at scan time keyed by
        // its dense index (the late window has no per-object length array). A
        // miss means the walk landed on a non-array or an uncaptured object.
        Attr::Length => match ctx.refwalk_tail(dense) {
            Some(v) => Some(v.clone()),
            None => {
                note.get_or_insert_with(|| {
                    "a reference-path @length tail resolved to an object whose \
                     length was not captured during the scan (the walked-to \
                     object is not an array); such tails project Null."
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
/// Widened: finds the RefPath attr wherever it appears in the Compare's
/// lhs or rhs expression (e.g. `x.parent.hash * 2 > 100`).
fn find_pred_refpath(p: &Predicate) -> Option<Attr> {
    let is_refpath = |a: &Attr| matches!(a, Attr::RefPath { .. });
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            find_pred_refpath(a).or_else(|| find_pred_refpath(b))
        }
        Predicate::Not(a) => find_pred_refpath(a),
        Predicate::Compare { lhs, rhs, .. } => {
            expr_find_attr(lhs, &is_refpath)
                .or_else(|| expr_find_attr(rhs, &is_refpath))
                .cloned()
        }
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
    let is_refpath = |a: &Attr| matches!(a, Attr::RefPath { .. });
    match p {
        Predicate::And(a, b) => {
            eval_refpath_pred(a, val, like_regexes) && eval_refpath_pred(b, val, like_regexes)
        }
        Predicate::Or(a, b) => {
            eval_refpath_pred(a, val, like_regexes) || eval_refpath_pred(b, val, like_regexes)
        }
        Predicate::Not(a) => !eval_refpath_pred(a, val, like_regexes),
        Predicate::Compare { lhs, op, rhs } => {
            // Only handle this Compare if it involves a RefPath attr.
            if !expr_has_attr(lhs, &is_refpath) && !expr_has_attr(rhs, &is_refpath) {
                return true;
            }
            let known = match val {
                Some(v) => v.clone(),
                None => return false, // dead end / uncaptured tail → no match
            };
            let lv = eval_late_expr(lhs, &is_refpath, &known);
            let rv = eval_late_expr(rhs, &is_refpath, &known);
            cmp_late_qv(&lv, *op, &rv, like_regexes)
        }
        _ => true,
    }
}
// cmp_query_value / cmp_i64 / cmp_f64 removed: callers now use cmp_late_qv.

/// Total ordering for two `QueryValue`s, used in GROUP BY late aggregation for
/// MIN/MAX and ORDER BY. Null sorts last. Mixed types fall back to Equal.
fn qv_ord(a: &QueryValue, b: &QueryValue) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (QueryValue::Null, QueryValue::Null) => Ordering::Equal,
        (QueryValue::Null, _) => Ordering::Greater,
        (_, QueryValue::Null) => Ordering::Less,
        (QueryValue::Int(x), QueryValue::Int(y)) => x.cmp(y),
        (QueryValue::Float(x), QueryValue::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (QueryValue::Int(x), QueryValue::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
        (QueryValue::Float(x), QueryValue::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
        (QueryValue::Str(x), QueryValue::Str(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

/// Evaluate a single GROUP BY key expression in the late phase.
/// Only attributes available in the late window are supported; anything else
/// returns Null (non-resolvable in the retained join).
fn eval_late_gb_key(expr: &Expr, idx: u32, ret: u64, ctx: &LateCtx) -> QueryValue {
    // GROUP BY expressions don't contain LIKE; use an empty regex map.
    static EMPTY_LIKE: std::sync::LazyLock<std::collections::HashMap<String, regex::Regex>> =
        std::sync::LazyLock::new(std::collections::HashMap::new);
    eval_late_expr_multi(expr, idx, ret, ctx, &EMPTY_LIKE)
}

/// GROUP BY + @retainedHeapSize: re-aggregate in the late phase using real
/// retained sizes and class names now available via `ctx`. The carry holds
/// individual dense indices (IndexOnly). We iterate them, compute the GROUP BY
/// key, and accumulate per-group SUM/COUNT/MIN/MAX.
fn join_retained_group_by(entry: &CrossPhaseEntry, q: &Query, ctx: &LateCtx) -> QueryResult {
    use crate::query::ast::{AggFunc, SortDir};
    use crate::query::execute::query_columns;
    use std::collections::HashMap;

    let like_regexes = crate::query::execute::compile_like_regexes(q).unwrap_or_default();

    // Per-group state: key_str -> (key_values, per-select-item accumulators).
    // Accumulator is (running: QueryValue, count: u64) for each SELECT aggregate.
    type GroupState = (Vec<QueryValue>, Vec<(QueryValue, u64)>);
    let mut groups: HashMap<String, GroupState> = HashMap::new();

    for idx in entry.carry.indices() {
        let ret = *ctx.retained.get(idx as usize).unwrap_or(&0);
        if !retained_where_passes(q, ret) {
            continue;
        }
        // Compute the GROUP BY key vector.
        let key: Vec<QueryValue> = entry
            .plan
            .group_by_exprs
            .iter()
            .map(|ge| eval_late_gb_key(ge, idx, ret, ctx))
            .collect();
        // Stable string representation for grouping.
        let key_str = format!("{key:?}");
        let accs = groups.entry(key_str).or_insert_with(|| {
            let init: Vec<(QueryValue, u64)> = q
                .select
                .iter()
                .map(|it| match it {
                    SelectItem::Aggregate { func, .. } => {
                        let start = match func {
                            AggFunc::Count => QueryValue::Int(0),
                            AggFunc::Sum => QueryValue::Int(0),
                            AggFunc::Min => QueryValue::Null,
                            AggFunc::Max => QueryValue::Null,
                            AggFunc::Avg | AggFunc::Percentile(_) | AggFunc::Median => QueryValue::Int(0),
                        };
                        (start, 0u64)
                    }
                    _ => (QueryValue::Null, 0),
                })
                .collect();
            (key.clone(), init)
        });
        // Accumulate each SELECT item's aggregate.
        for (i, it) in q.select.iter().enumerate() {
            let SelectItem::Aggregate { func, arg } = it else {
                continue;
            };
            let val = match arg.as_ref() {
                SelectItem::Attr(Attr::RetainedHeapSize) => QueryValue::Int(ret as i64),
                SelectItem::Attr(Attr::UsedHeapSize) => QueryValue::Int(
                    ctx.shallow.get(idx as usize).copied().unwrap_or(0) as i64,
                ),
                SelectItem::Attr(Attr::ObjectId) => QueryValue::Int(idx as i64),
                SelectItem::Attr(Attr::ObjectAddress) => {
                    QueryValue::Int(ctx.id_map.to_addr(idx) as i64)
                }
                SelectItem::Expr(e) => eval_late_expr_multi(e, idx, ret as u64, ctx, &like_regexes),
                SelectItem::Star => QueryValue::Int(1),
                _ => QueryValue::Null,
            };
            let (acc, count) = &mut accs.1[i];
            match func {
                AggFunc::Count => {
                    if let QueryValue::Int(n) = acc { *n += 1; }
                }
                AggFunc::Sum => {
                    if val != QueryValue::Null {
                        *acc = match (&*acc, &val) {
                            (QueryValue::Int(a), QueryValue::Int(b)) => QueryValue::Int(*a + *b),
                            (QueryValue::Float(a), QueryValue::Float(b)) => QueryValue::Float(*a + *b),
                            (QueryValue::Int(a), QueryValue::Float(b)) => QueryValue::Float(*a as f64 + *b),
                            (QueryValue::Float(a), QueryValue::Int(b)) => QueryValue::Float(*a + *b as f64),
                            _ => acc.clone(),
                        };
                    }
                }
                AggFunc::Min => {
                    if val != QueryValue::Null {
                        *acc = if *acc == QueryValue::Null {
                            val.clone()
                        } else {
                            match qv_ord(&val, acc) {
                                std::cmp::Ordering::Less => val.clone(),
                                _ => acc.clone(),
                            }
                        };
                    }
                }
                AggFunc::Max => {
                    if val != QueryValue::Null {
                        *acc = if *acc == QueryValue::Null {
                            val.clone()
                        } else {
                            match qv_ord(&val, acc) {
                                std::cmp::Ordering::Greater => val.clone(),
                                _ => acc.clone(),
                            }
                        };
                    }
                }
                AggFunc::Avg => {
                    if val != QueryValue::Null {
                        *count += 1;
                        *acc = match (&*acc, &val) {
                            (QueryValue::Int(a), QueryValue::Int(b)) => QueryValue::Int(*a + *b),
                            (QueryValue::Float(a), QueryValue::Float(b)) => QueryValue::Float(*a + *b),
                            (QueryValue::Int(a), QueryValue::Float(b)) => QueryValue::Float(*a as f64 + *b),
                            (QueryValue::Float(a), QueryValue::Int(b)) => QueryValue::Float(*a + *b as f64),
                            _ => acc.clone(),
                        };
                    }
                }
                AggFunc::Percentile(_) | AggFunc::Median => {} // not supported in late phase
            }
        }
    }

    // Finalize: build output rows from group accumulators.
    let group_by_exprs = &entry.plan.group_by_exprs;
    let mut rows: Vec<Vec<QueryValue>> = groups
        .into_values()
        .map(|(key, accs)| {
            q.select
                .iter()
                .enumerate()
                .map(|(i, it)| match it {
                    SelectItem::Aggregate { func, .. } => {
                        let (acc, count) = &accs[i];
                        match func {
                            AggFunc::Avg if *count > 0 => match acc {
                                QueryValue::Int(s) => QueryValue::Float(*s as f64 / *count as f64),
                                QueryValue::Float(s) => QueryValue::Float(*s / *count as f64),
                                _ => QueryValue::Null,
                            },
                            _ => acc.clone(),
                        }
                    }
                    _ => {
                        // Non-aggregate: find this item's position in the GROUP BY keys.
                        let col_name = crate::query::execute::column_name(it);
                        let gb_match = group_by_exprs.iter().enumerate().find(|(_, ge)| {
                            let ge_name = crate::query::execute::expr_name(ge);
                            ge_name == col_name
                                || match (ge, it) {
                                    (Expr::Attr(ga), SelectItem::Attr(a)) => ga == a,
                                    _ => false,
                                }
                        });
                        match gb_match {
                            Some((j, _)) => key.get(j).cloned().unwrap_or(QueryValue::Null),
                            None => QueryValue::Null,
                        }
                    }
                })
                .collect()
        })
        .collect();

    // Apply HAVING filter post-aggregation (mirrors the scan-time GROUP BY path).
    if !entry.plan.having_terms.is_empty() {
        let columns = query_columns(q);
        rows.retain(|row| {
            entry.plan.having_terms.iter().all(|term| {
                crate::query::execute::eval_having_term(&term.pred, row, q, &columns, &like_regexes)
            })
        });
    }

    // ORDER BY: match by ORDER BY attribute or alias name.
    if let Some(ob) = &q.order_by {
        let ob_name = crate::query::execute::attr_name(&ob.key);
        let cols = query_columns(q);
        let col_idx = cols.iter().position(|c| c.name == ob_name)
            .or_else(|| q.select.iter().position(|it| match it {
                SelectItem::Attr(a) => *a == ob.key,
                SelectItem::Aggregate { arg, .. } => matches!(arg.as_ref(), SelectItem::Attr(a) if *a == ob.key),
                _ => false,
            }));
        if let Some(col_idx) = col_idx {
            rows.sort_by(|a, b| {
                qv_ord(
                    a.get(col_idx).unwrap_or(&QueryValue::Null),
                    b.get(col_idx).unwrap_or(&QueryValue::Null),
                )
            });
            if ob.dir == SortDir::Desc {
                rows.reverse();
            }
        }
    }

    let mut truncated = entry.carry.truncated();
    if let Some(limit) = stage_limit(q) {
        if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            truncated = true;
        }
    }

    let row_count = rows.len() as u64;
    QueryResult {
        name: entry.name.clone(),
        oql: String::new(),
        columns: query_columns(q),
        row_count,
        rows,
        truncated,
        error: None,
        note: None,
        viz: None,
        elapsed_ms: None,
    }
}

fn join_retained(entry: &CrossPhaseEntry, q: &Query, ctx: &LateCtx) -> QueryResult {
    if entry.plan.kind == crate::query::plan::StageKind::GroupBy {
        return join_retained_group_by(entry, q, ctx);
    }
    let like_regexes = crate::query::execute::compile_like_regexes(q).unwrap_or_default();

    // Ungrouped aggregate path: SELECT COUNT/SUM/MIN/MAX/AVG over retained attrs
    // without a GROUP BY clause. Fold all matched objects into a single row.
    // The scan-time plan correctly carries all indices (StageKind::SingleScan),
    // but join_retained normally projects 1:1 — aggregation must happen here.
    let is_aggregate = q.select.iter().any(|it| matches!(it, SelectItem::Aggregate { .. }));
    if is_aggregate && q.group_by.is_empty() {
        use crate::query::ast::AggFunc;
        let columns = crate::query::execute::query_columns(q);
        let mut accs: Vec<(QueryValue, u64)> = q.select.iter().map(|it| match it {
            SelectItem::Aggregate { func, .. } => {
                let start = match func {
                    AggFunc::Count => QueryValue::Int(0),
                    AggFunc::Sum => QueryValue::Int(0),
                    AggFunc::Min | AggFunc::Max => QueryValue::Null,
                    AggFunc::Avg | AggFunc::Percentile(_) | AggFunc::Median => QueryValue::Int(0),
                };
                (start, 0u64)
            }
            _ => (QueryValue::Null, 0),
        }).collect();

        for idx in entry.carry.indices() {
            let ret = *ctx.retained.get(idx as usize).unwrap_or(&0);
            if !retained_where_passes(q, ret) {
                continue;
            }
            for (i, it) in q.select.iter().enumerate() {
                let SelectItem::Aggregate { func, arg } = it else { continue };
                let val = match arg.as_ref() {
                    SelectItem::Attr(Attr::RetainedHeapSize) => QueryValue::Int(ret as i64),
                    SelectItem::Attr(Attr::UsedHeapSize) => QueryValue::Int(
                        ctx.shallow.get(idx as usize).copied().unwrap_or(0) as i64,
                    ),
                    SelectItem::Attr(Attr::ObjectId) => QueryValue::Int(idx as i64),
                    SelectItem::Attr(Attr::ObjectAddress) => {
                        QueryValue::Int(ctx.id_map.to_addr(idx) as i64)
                    }
                    SelectItem::Expr(e) => eval_late_expr_multi(e, idx, ret as u64, ctx, &like_regexes),
                    SelectItem::Star => QueryValue::Int(1),
                    _ => QueryValue::Null,
                };
                let (acc, count) = &mut accs[i];
                match func {
                    AggFunc::Count => { if let QueryValue::Int(n) = acc { *n += 1; } }
                    AggFunc::Sum => {
                        if val != QueryValue::Null {
                            *acc = match (&*acc, &val) {
                                (QueryValue::Int(a), QueryValue::Int(b)) => QueryValue::Int(*a + *b),
                                (QueryValue::Float(a), QueryValue::Float(b)) => QueryValue::Float(*a + *b),
                                (QueryValue::Int(a), QueryValue::Float(b)) => QueryValue::Float(*a as f64 + *b),
                                (QueryValue::Float(a), QueryValue::Int(b)) => QueryValue::Float(*a + *b as f64),
                                _ => acc.clone(),
                            };
                        }
                    }
                    AggFunc::Min => {
                        if val != QueryValue::Null {
                            *acc = if *acc == QueryValue::Null { val.clone() } else {
                                match qv_ord(&val, acc) {
                                    std::cmp::Ordering::Less => val.clone(),
                                    _ => acc.clone(),
                                }
                            };
                        }
                    }
                    AggFunc::Max => {
                        if val != QueryValue::Null {
                            *acc = if *acc == QueryValue::Null { val.clone() } else {
                                match qv_ord(&val, acc) {
                                    std::cmp::Ordering::Greater => val.clone(),
                                    _ => acc.clone(),
                                }
                            };
                        }
                    }
                    AggFunc::Avg => {
                        if val != QueryValue::Null {
                            *count += 1;
                            *acc = match (&*acc, &val) {
                                (QueryValue::Int(a), QueryValue::Int(b)) => QueryValue::Int(*a + *b),
                                (QueryValue::Float(a), QueryValue::Float(b)) => QueryValue::Float(*a + *b),
                                (QueryValue::Int(a), QueryValue::Float(b)) => QueryValue::Float(*a as f64 + *b),
                                (QueryValue::Float(a), QueryValue::Int(b)) => QueryValue::Float(*a + *b as f64),
                                _ => acc.clone(),
                            };
                        }
                    }
                    AggFunc::Percentile(_) | AggFunc::Median => {} // unsupported in late phase
                }
            }
        }

        // Finalize: Count/Sum/Min/Max return acc directly; Avg divides by count.
        let row: Vec<QueryValue> = q.select.iter().enumerate().map(|(i, it)| match it {
            SelectItem::Aggregate { func, .. } => {
                let (acc, count) = &accs[i];
                match func {
                    AggFunc::Avg if *count > 0 => match acc {
                        QueryValue::Int(s) => QueryValue::Float(*s as f64 / *count as f64),
                        QueryValue::Float(s) => QueryValue::Float(*s / *count as f64),
                        _ => QueryValue::Null,
                    },
                    _ => acc.clone(),
                }
            }
            _ => QueryValue::Null,
        }).collect();

        // Apply HAVING filter on the single aggregate row.
        let passes = if entry.plan.having_terms.is_empty() {
            true
        } else {
            entry.plan.having_terms.iter().all(|term| {
                crate::query::execute::eval_having_term(&term.pred, &row, q, &columns, &like_regexes)
            })
        };
        let (rows, row_count) = if passes { (vec![row], 1u64) } else { (vec![], 0u64) };
        return QueryResult {
            name: entry.name.clone(),
            oql: String::new(),
            columns,
            row_count,
            rows,
            truncated: entry.carry.truncated(),
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
    }

    let mut rows: Vec<(u32, u64)> = Vec::new();
    for idx in entry.carry.indices() {
        let ret = *ctx.retained.get(idx as usize).unwrap_or(&0);
        if retained_where_passes(q, ret) {
            rows.push((idx, ret));
        }
    }

    let order_by_retained = q.order_by.as_ref()
        .is_some_and(|ob| ob.key == Attr::RetainedHeapSize);

    if order_by_retained {
        // Fast path: sort (idx,ret) pairs before projection, then cap.
        let dir = q.order_by.as_ref().unwrap().dir;
        rows.sort_by_key(|(_, r)| *r);
        if dir == SortDir::Desc {
            rows.reverse();
        }
        let mut truncated = entry.carry.truncated();
        if let Some(limit) = stage_limit(q) {
            if rows.len() as u64 > limit {
                rows.truncate(limit as usize);
                truncated = true;
            }
        }
        let columns: Vec<QueryColumn> = crate::query::execute::query_columns(q);
        let out_rows: Vec<Vec<QueryValue>> = rows
            .iter()
            .map(|(idx, ret)| project_late_row(q, *idx, *ret, ctx, &like_regexes))
            .collect();
        return QueryResult {
            name: entry.name.clone(),
            oql: String::new(),
            columns,
            row_count: out_rows.len() as u64,
            rows: out_rows,
            truncated,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
    }

    // Generic path: project all rows, sort by any column alias or attr, then cap.
    let columns: Vec<QueryColumn> = crate::query::execute::query_columns(q);
    let mut out_rows: Vec<Vec<QueryValue>> = rows
        .iter()
        .map(|(idx, ret)| project_late_row(q, *idx, *ret, ctx, &like_regexes))
        .collect();
    let mut truncated = entry.carry.truncated();
    if let Some(ob) = &q.order_by {
        if let Some(col_idx) =
            crate::query::execute::order_by_column_index(q, &columns, &ob.key)
        {
            crate::query::execute::sort_rows_by_column(&mut out_rows, col_idx, ob.dir);
        }
    }
    if let Some(limit) = stage_limit(q) {
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
        viz: None,
        elapsed_ms: None,
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
    let is_retained = |a: &Attr| matches!(a, Attr::RetainedHeapSize);
    // The retained size is represented as an i64 (safe: Java heap sizes fit in
    // 63 bits for any JVM process that doesn't OOM before analysis) for arithmetic
    // with the standard `arith` helper which returns Int/Float/Null.
    let retained_qv = QueryValue::Int(ret as i64);
    match p {
        Predicate::And(a, b) => eval_retained_pred(a, ret) && eval_retained_pred(b, ret),
        Predicate::Or(a, b) => eval_retained_pred(a, ret) || eval_retained_pred(b, ret),
        Predicate::Not(a) => !eval_retained_pred(a, ret),
        Predicate::Compare { lhs, op, rhs } => {
            // Only handle this Compare if it involves @retainedHeapSize.
            // Non-retained terms were applied in Phase 1 and pass here.
            if !expr_has_attr(lhs, &is_retained) && !expr_has_attr(rhs, &is_retained) {
                return true;
            }
            let lv = eval_late_expr(lhs, &is_retained, &retained_qv);
            let rv = eval_late_expr(rhs, &is_retained, &retained_qv);
            // No LIKE regex map for retained-size comparisons (numeric only).
            cmp_late_qv(&lv, *op, &rv, &std::collections::HashMap::new())
        }
        _ => true,
    }
}
// cmp_u64 removed: callers now use cmp_late_qv via eval_retained_pred.

/// Project a late row from a dense index + retained size. Handles attrs that are
/// available in the late window: @objectId, @retainedHeapSize, @usedHeapSize (from
/// ctx.shallow), @objectAddress (from ctx.id_map; returns 0 when the id_map was
/// compressed away on the analyze path), @classOf/@displayName (from ctx.class_idx /
/// ctx.class_names), @GCRoots/@GCRootInfo/@info (from ctx.gc_root_tags), and SELECT *.
/// Arithmetic and CASE expressions over late attrs are evaluated via eval_late_expr_multi.
/// Blob-dependent field attrs need an IndexPlusScalars carry (later step) and are Null.
fn project_late_row(
    q: &Query,
    idx: u32,
    ret: u64,
    ctx: &LateCtx,
    like_regexes: &std::collections::HashMap<String, regex::Regex>,
) -> Vec<QueryValue> {
    q.select
        .iter()
        .map(|it| match it {
            SelectItem::Attr(Attr::ObjectId) => QueryValue::Int(idx as i64),
            SelectItem::Attr(Attr::RetainedHeapSize) => QueryValue::Int(ret as i64),
            SelectItem::Attr(Attr::UsedHeapSize) => QueryValue::Int(
                ctx.shallow.get(idx as usize).copied().unwrap_or(0) as i64,
            ),
            SelectItem::Attr(Attr::ObjectAddress) => {
                QueryValue::Int(ctx.id_map.to_addr(idx) as i64)
            }
            SelectItem::Attr(Attr::GcRootInfo) | SelectItem::Attr(Attr::GcRoots) => {
                match ctx.gc_root_tag(idx) {
                    Some(tag) => QueryValue::Str(root_tag_name(tag).into_owned()),
                    None => QueryValue::Null,
                }
            }
            SelectItem::Attr(Attr::ClassOf) | SelectItem::Attr(Attr::DisplayName) => {
                match ctx.class_name_of(idx) {
                    Some(name) => QueryValue::Str(name.to_string()),
                    None => QueryValue::Null,
                }
            }
            SelectItem::Attr(Attr::ToHex(inner)) => {
                match eval_late_expr_multi(inner, idx, ret, ctx, like_regexes) {
                    QueryValue::Int(n) => QueryValue::Str(format!("0x{:x}", n as u64)),
                    _ => QueryValue::Null,
                }
            }
            SelectItem::Star => QueryValue::ObjRef {
                index: idx as u64,
                class: ctx.class_name_of(idx).unwrap_or("?").to_string(),
                // Late window: the dense-address table is compressed away, so
                // resolving an address here would yield a misleading @0.
                addr: None,
            },
            SelectItem::Expr(e) => eval_late_expr_multi(e, idx, ret, ctx, like_regexes),
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


    fn pq(q: &crate::query::ast::Query) -> crate::query::plan::QueryPlan {
        crate::query::plan::plan_query(q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap()
    }
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
        }
    }

    static EMPTY_ID_MAP: IdMap<'static> = IdMap { addr_of: &[] };

    fn q_slice(q: &crate::query::ast::Query) -> Vec<crate::query::ast::Query> {
        vec![q.clone(), q.clone()]
    }

    #[test]
    fn root_tag_name_table() {
        use crate::types::heap;
        assert_eq!(root_tag_name(heap::ROOT_SYSTEM_CLASS), "System Class");
        assert_eq!(root_tag_name(heap::ROOT_JNI_GLOBAL), "JNI Global");
        assert_eq!(root_tag_name(heap::ROOT_JNI_LOCAL), "JNI Local");
        assert_eq!(root_tag_name(heap::ROOT_JAVA_FRAME), "Java Frame");
        assert_eq!(root_tag_name(heap::ROOT_NATIVE_STACK), "Native Stack");
        assert_eq!(root_tag_name(heap::ROOT_STICKY_CLASS), "Sticky Class");
        assert_eq!(root_tag_name(heap::ROOT_THREAD_BLOCK), "Thread Block");
        assert_eq!(root_tag_name(heap::ROOT_MONITOR_USED), "Busy Monitor");
        assert_eq!(root_tag_name(heap::ROOT_THREAD_OBJ), "Thread");
        assert_eq!(root_tag_name(heap::ROOT_UNKNOWN), "Unknown");
        // An out-of-range code surfaces the numeric tag, never a silent empty.
        assert_eq!(root_tag_name(0x42), "root tag 66");
    }

    /// A `@GCRootInfo` SELECT projects the root-tag label for a dense index that
    /// is in the gc-root-tags map, and `Null` for a non-root index. `@GCRoots`
    /// returns the same descriptor. Exercises `project_late_row` in analyze mode.
    #[test]
    fn gcroot_attrs_project_from_tag_map() {
        use crate::types::heap;
        let q = crate::query::parse::parse("SELECT @GCRootInfo, @GCRoots FROM C").unwrap();
        let plan = pq(&q);
        assert!(plan.needs.gc_roots, "query must arm needs.gc_roots");
        let mut carry = crate::query::carry::Carry::index_only(10);
        carry.push_index(3); // a root (Thread)
        carry.push_index(5); // a non-root
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".to_string(), plan, carry);
        let retained = vec![0u64; 10];
        let tags: std::collections::HashMap<u32, u8> =
            [(3u32, heap::ROOT_THREAD_OBJ)].into_iter().collect();
        let base = ctx(&retained);
        let ctx = LateCtx {
            gc_root_tags: &tags,
            ..base
        };
        let out = resume(st, &q_slice(&q), &ctx);
        let r = &out[0];
        // Row order follows carry order (no ORDER BY on a late gc-root attr).
        assert_eq!(r.rows[0][0], QueryValue::Str("Thread".to_string()));
        assert_eq!(r.rows[0][1], QueryValue::Str("Thread".to_string()));
        assert_eq!(r.rows[1][0], QueryValue::Null);
        assert_eq!(r.rows[1][1], QueryValue::Null);
    }

    #[test]
    fn join_retained_projects_and_orders_desc() {
        let q = crate::query::parse::parse(
            "SELECT @objectId, @retainedHeapSize FROM C ORDER BY @retainedHeapSize DESC",
        )
        .unwrap();
        let plan = pq(&q);
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
        let plan = pq(&q);
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
        let plan = pq(&q);
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
                viz: None,
                elapsed_ms: None,
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
        let plan = pq(&q);
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
        let plan = pq(&q);
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
        let plan = pq(&q);
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
mod classof_late_tests {
    use super::*;
    use crate::query::execute::QueryExecState;
    use crate::query::model::QueryValue;

    static EMPTY_ID_MAP: IdMap<'static> = IdMap { addr_of: &[] };

    fn classof_ctx<'a>(
        retained: &'a [u64],
        class_idx: &'a [u32],
        class_names: &'a [String],
    ) -> LateCtx<'a> {
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx,
            class_names,
        }
    }

    #[test]
    fn classof_projects_class_name_in_retained_join() {
        let class_names: Vec<String> = vec!["java.lang.String".into(), "java.lang.Object".into()];
        // Object at dense 0 is a String (row 0), object at dense 1 is an Object (row 1).
        let class_idx = vec![0u32, 1];
        let retained = vec![100u64, 200];
        let ctx = classof_ctx(&retained, &class_idx, &class_names);
        let q = crate::query::parse::parse(
            "SELECT classof(x), @retainedHeapSize FROM C",
        ).unwrap();
        let plan = crate::query::plan::plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(10);
        carry.push_index(0);
        carry.push_index(1);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".into(), plan, carry);
        let out = resume(st, &[q.clone(), q], &ctx);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.rows[0][0], QueryValue::Str("java.lang.String".into()));
        assert_eq!(r.rows[0][1], QueryValue::Int(100));
        assert_eq!(r.rows[1][0], QueryValue::Str("java.lang.Object".into()));
        assert_eq!(r.rows[1][1], QueryValue::Int(200));
    }

    #[test]
    fn classof_returns_null_when_class_data_absent() {
        // Empty class_idx: simulates a context without class data threaded in.
        let ctx = classof_ctx(&[500u64], &[], &[]);
        let q = crate::query::parse::parse("SELECT classof(x) FROM C").unwrap();
        let plan = crate::query::plan::plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(10);
        carry.push_index(0);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".into(), plan, carry);
        let out = resume(st, &[q.clone(), q], &ctx);
        assert_eq!(out[0].rows[0][0], QueryValue::Null);
    }

    #[test]
    fn group_by_classof_sum_retained_aggregates_in_late_phase() {
        // Two classes; three objects (2 String at 100 each, 1 Object at 200).
        // GROUP BY classof(x) should yield two groups with correct SUM values.
        let class_names: Vec<String> = vec!["java.lang.String".into(), "java.lang.Object".into()];
        let class_idx = vec![0u32, 0, 1]; // dense 0,1 = String; dense 2 = Object
        let retained = vec![100u64, 100, 200];
        let ctx = classof_ctx(&retained, &class_idx, &class_names);
        let q = crate::query::parse::parse(
            "SELECT classof(x) AS class, SUM(@retainedHeapSize) AS total \
             FROM C GROUP BY classof(x) ORDER BY total DESC",
        ).unwrap();
        let plan = crate::query::plan::plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap();
        assert_eq!(plan.kind, crate::query::plan::StageKind::GroupBy);
        let mut carry = crate::query::carry::Carry::index_only(10);
        carry.push_index(0);
        carry.push_index(1);
        carry.push_index(2);
        let mut st = crate::query::execute::QueryExecState::new();
        st.push_cross_phase(0, "q1".into(), plan, carry);
        let out = resume(st, &[q.clone(), q], &ctx);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.rows.len(), 2, "should have 2 groups");
        // First row (DESC) = String with SUM=200; second = Object with SUM=200.
        // Both SUMs are non-null integers.
        for row in &r.rows {
            assert!(matches!(row[1], QueryValue::Int(_)), "SUM should be Int, got {:?}", row[1]);
            assert!(matches!(row[0], QueryValue::Str(_)), "classof should be Str, got {:?}", row[0]);
        }
        // DESC order: total[0] >= total[1]
        let s0 = if let QueryValue::Int(n) = r.rows[0][1] { n } else { panic!() };
        let s1 = if let QueryValue::Int(n) = r.rows[1][1] { n } else { panic!() };
        assert!(s0 >= s1, "should be sorted DESC: {s0} >= {s1}");
        // Verify the SUM values: String total=200, Object total=200 (equal).
        assert_eq!(s0 + s1, 400, "total retained should be 400");
    }
    #[test]
    fn used_heap_size_projects_shallow_in_retained_join() {
        // Three objects: shallow sizes 40, 80, 120.
        let shallow = vec![40u32, 80, 120];
        let retained = vec![40u64, 80, 120];
        let ctx = LateCtx {
            retained: &retained,
            shallow: &shallow,
            idom: &[],
            dc_off: &[],
            dc_tgt: &[],
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
        };
        let q = crate::query::parse::parse(
            "SELECT @usedHeapSize, @retainedHeapSize FROM C",
        ).unwrap();
        let plan = crate::query::plan::plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(10);
        carry.push_index(0);
        carry.push_index(1);
        carry.push_index(2);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".into(), plan, carry);
        let out = resume(st, &[q.clone(), q], &ctx);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.rows[0][0], QueryValue::Int(40));
        assert_eq!(r.rows[1][0], QueryValue::Int(80));
        assert_eq!(r.rows[2][0], QueryValue::Int(120));
    }

    #[test]
    fn group_by_sum_used_heap_size_aggregates_in_late_phase() {
        // Two objects with shallow 40 and 80; GROUP BY @retainedHeapSize bucket,
        // SUM(@usedHeapSize) should be non-null.
        let shallow = vec![40u32, 80];
        let retained = vec![40u64, 80];
        let class_names: Vec<String> = vec!["Foo".into()];
        let class_idx = vec![0u32, 0];
        let ctx = LateCtx {
            retained: &retained,
            shallow: &shallow,
            class_idx: &class_idx,
            class_names: &class_names,
            idom: &[],
            dc_off: &[],
            dc_tgt: &[],
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
        };
        let q = crate::query::parse::parse(
            "SELECT classof(x) AS c, SUM(@usedHeapSize) AS sh FROM C GROUP BY classof(x) ORDER BY sh DESC",
        ).unwrap();
        let plan = crate::query::plan::plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap();
        assert_eq!(plan.kind, crate::query::plan::StageKind::GroupBy);
        let mut carry = crate::query::carry::Carry::index_only(10);
        carry.push_index(0);
        carry.push_index(1);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".into(), plan, carry);
        let out = resume(st, &[q.clone(), q], &ctx);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.rows.len(), 1, "one group (all Foo)");
        assert_eq!(r.rows[0][1], QueryValue::Int(120), "SUM(@usedHeapSize) = 40+80 = 120");
    }

    #[test]
    fn group_by_having_filters_groups_in_late_phase() {
        // Two objects: String (retained=100, shallow=40) and Object (retained=200, shallow=80).
        // HAVING SUM(@retainedHeapSize) > 150 should keep only the Object group.
        let class_names: Vec<String> = vec!["java.lang.String".into(), "java.lang.Object".into()];
        let class_idx = vec![0u32, 1]; // dense 0=String, dense 1=Object
        let retained = vec![100u64, 200];
        let ctx = classof_ctx(&retained, &class_idx, &class_names);
        let q = crate::query::parse::parse(
            "SELECT classof(x) AS c, SUM(@retainedHeapSize) AS ret \
             FROM C GROUP BY classof(x) HAVING ret > 150",
        ).unwrap();
        let plan = crate::query::plan::plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap();
        assert_eq!(plan.kind, crate::query::plan::StageKind::GroupBy);
        let mut carry = crate::query::carry::Carry::index_only(10);
        carry.push_index(0);
        carry.push_index(1);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".into(), plan, carry);
        let out = resume(st, &[q.clone(), q], &ctx);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.rows.len(), 1, "HAVING should keep only Object group (ret=200 > 150)");
        assert_eq!(r.rows[0][0], QueryValue::Str("java.lang.Object".into()));
        assert_eq!(r.rows[0][1], QueryValue::Int(200));
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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

    fn pq(q: &crate::query::ast::Query) -> crate::query::plan::QueryPlan {
        crate::query::plan::plan_query(q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap()
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
        };
        let q = crate::query::parse::parse("SELECT dominators(s) FROM C s").unwrap();
        let plan = pq(&q);
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
        };
        let q = crate::query::parse::parse("SELECT dominatorof(s) FROM C s").unwrap();
        let plan = pq(&q);
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
        };
        let q = crate::query::parse::parse("SELECT s AS RETAINED SET FROM C s").unwrap();
        let plan = pq(&q);
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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

    fn pq(q: &crate::query::ast::Query) -> crate::query::plan::QueryPlan {
        crate::query::plan::plan_query(q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap()
    }

    /// Build a QueryExecState with one carried seed frontier for `oql`, seeded
    /// with the given dense indices.
    fn refwalk_state(oql: &str, seeds: &[u32]) -> (QueryExecState, crate::query::ast::Query) {
        let q = crate::query::parse::parse(oql).unwrap();
        let plan = pq(&q);
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
        }
    }

    fn pq(q: &crate::query::ast::Query) -> crate::query::plan::QueryPlan {
        crate::query::plan::plan_query(q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap()
    }

    /// Helper: build a QueryExecState with one `ResolveStringValues` pending entry
    /// carrying the given dense indices.
    fn string_state(oql: &str, seeds: &[u32]) -> (QueryExecState, crate::query::ast::Query) {
        let q = crate::query::parse::parse(oql).unwrap();
        let plan = pq(&q);
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
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

#[cfg(test)]
mod arith_late_tests {
    //! Unit tests for the late-phase arithmetic-expression evaluator introduced in
    //! Task 6. These tests cover:
    //!   - `eval_late_expr` directly (the building-block helper)
    //!   - Case A: arithmetic on the RHS only (literal-only RHS folds at late time)
    //!   - Case B: arithmetic on the LHS (the late attr is buried inside a Binary)
    //!   - RHS containing a non-late Attr (yields Null → comparison false, no panic)
    //!   - Non-arithmetic queries still work exactly as before (regression guard)
    //!   - Detector widening: `has_to_string_pred` and `find_pred_refpath` find the
    //!     late attr when it is buried inside a Binary/Unary expression.

    use super::*;
    use crate::query::ast::{Attr, CompareOp, Expr, ArithOp, UnaryOp, Predicate, Value};
    use crate::query::model::QueryValue;

    // ── eval_late_expr ────────────────────────────────────────────────────────

    /// A plain Attr that IS the known attr resolves to the supplied known value.
    #[test]
    fn eval_late_expr_known_attr_resolves_to_known() {
        let e = Expr::Attr(Attr::RetainedHeapSize);
        let known = QueryValue::Int(50);
        let result = eval_late_expr(
            &e,
            &|a| matches!(a, Attr::RetainedHeapSize),
            &known,
        );
        assert_eq!(result, QueryValue::Int(50));
    }

    /// An unknown Attr resolves to Null.
    #[test]
    fn eval_late_expr_unknown_attr_is_null() {
        let e = Expr::Attr(Attr::UsedHeapSize);
        let known = QueryValue::Int(999);
        let result = eval_late_expr(
            &e,
            &|a| matches!(a, Attr::RetainedHeapSize),
            &known,
        );
        assert_eq!(result, QueryValue::Null);
    }

    /// A literal folds to the corresponding QueryValue.
    #[test]
    fn eval_late_expr_lit_folds() {
        let e = Expr::Lit(Value::Int(42));
        let result = eval_late_expr(
            &e,
            &|a| matches!(a, Attr::RetainedHeapSize),
            &QueryValue::Null,
        );
        assert_eq!(result, QueryValue::Int(42));
    }

    /// `@retainedHeapSize * 2` with known=Int(50) → Int(100).
    #[test]
    fn eval_late_expr_retained_mul_2() {
        let e = Expr::Binary {
            op: ArithOp::Mul,
            lhs: Box::new(Expr::Attr(Attr::RetainedHeapSize)),
            rhs: Box::new(Expr::Lit(Value::Int(2))),
        };
        let result = eval_late_expr(
            &e,
            &|a| matches!(a, Attr::RetainedHeapSize),
            &QueryValue::Int(50),
        );
        assert_eq!(result, QueryValue::Int(100));
    }

    /// Unary negation: `-@retainedHeapSize` with known=Int(5) → Int(-5).
    #[test]
    fn eval_late_expr_unary_neg() {
        let e = Expr::Unary {
            op: UnaryOp::Neg,
            arg: Box::new(Expr::Attr(Attr::RetainedHeapSize)),
        };
        let result = eval_late_expr(
            &e,
            &|a| matches!(a, Attr::RetainedHeapSize),
            &QueryValue::Int(5),
        );
        assert_eq!(result, QueryValue::Int(-5));
    }

    /// Unary pos: `+@retainedHeapSize` is identity.
    #[test]
    fn eval_late_expr_unary_pos_is_identity() {
        let e = Expr::Unary {
            op: UnaryOp::Pos,
            arg: Box::new(Expr::Attr(Attr::RetainedHeapSize)),
        };
        let result = eval_late_expr(
            &e,
            &|a| matches!(a, Attr::RetainedHeapSize),
            &QueryValue::Int(7),
        );
        assert_eq!(result, QueryValue::Int(7));
    }

    // ── cmp_late_qv (QueryValue vs QueryValue comparator) ────────────────────

    /// Int vs Int ordered compare.
    #[test]
    fn cmp_late_qv_int_gt() {
        assert!(cmp_late_qv(&QueryValue::Int(101), CompareOp::Gt, &QueryValue::Int(100), &std::collections::HashMap::new()));
        assert!(!cmp_late_qv(&QueryValue::Int(99), CompareOp::Gt, &QueryValue::Int(100), &std::collections::HashMap::new()));
    }

    /// Int vs Float cross-type compare.
    #[test]
    fn cmp_late_qv_int_vs_float() {
        assert!(cmp_late_qv(&QueryValue::Int(3), CompareOp::Lt, &QueryValue::Float(3.5), &std::collections::HashMap::new()));
        assert!(!cmp_late_qv(&QueryValue::Int(4), CompareOp::Lt, &QueryValue::Float(3.5), &std::collections::HashMap::new()));
    }

    /// Null on either side → only Ne is true (mismatch behavior).
    #[test]
    fn cmp_late_qv_null_is_not_equal() {
        let no_re = std::collections::HashMap::new();
        assert!(!cmp_late_qv(&QueryValue::Null, CompareOp::Eq, &QueryValue::Int(1), &no_re));
        assert!(cmp_late_qv(&QueryValue::Null, CompareOp::Ne, &QueryValue::Int(1), &no_re));
        assert!(!cmp_late_qv(&QueryValue::Null, CompareOp::Gt, &QueryValue::Int(1), &no_re));
    }

    // ── Case A: arithmetic RHS — retained_where_passes ───────────────────────

    /// `@retainedHeapSize > 40 * 2` with ret=100 → passes (RHS folds to 80).
    #[test]
    fn retained_where_case_a_rhs_arith_passes() {
        let q = crate::query::parse::parse(
            "SELECT @objectId FROM C WHERE @retainedHeapSize > 40 * 2"
        ).unwrap();
        assert!(retained_where_passes(&q, 100));
    }

    /// `@retainedHeapSize > 40 * 2` with ret=50 → fails (50 ≤ 80).
    #[test]
    fn retained_where_case_a_rhs_arith_fails() {
        let q = crate::query::parse::parse(
            "SELECT @objectId FROM C WHERE @retainedHeapSize > 40 * 2"
        ).unwrap();
        assert!(!retained_where_passes(&q, 50));
    }

    // ── Case B: arithmetic LHS — retained_where_passes ───────────────────────

    /// `@retainedHeapSize * 2 > 100` with ret=60 → 120 > 100 → passes.
    #[test]
    fn retained_where_case_b_lhs_arith_passes() {
        let q = crate::query::parse::parse(
            "SELECT @objectId FROM C WHERE @retainedHeapSize * 2 > 100"
        ).unwrap();
        assert!(retained_where_passes(&q, 60));
    }

    /// `@retainedHeapSize * 2 > 100` with ret=40 → 80 > 100 → fails.
    #[test]
    fn retained_where_case_b_lhs_arith_fails() {
        let q = crate::query::parse::parse(
            "SELECT @objectId FROM C WHERE @retainedHeapSize * 2 > 100"
        ).unwrap();
        assert!(!retained_where_passes(&q, 40));
    }

    // ── RHS containing a non-late Attr — no panic, returns false ─────────────

    /// Build a Compare with a non-late Attr on the RHS manually (parser can't do
    /// this, but the evaluator must not panic and must return false).
    #[test]
    fn retained_where_rhs_non_late_attr_no_panic_returns_false() {
        // Construct: @retainedHeapSize > @usedHeapSize (manually)
        let pred = Predicate::Compare {
            lhs: Expr::Attr(Attr::RetainedHeapSize),
            op: CompareOp::Gt,
            rhs: Expr::Attr(Attr::UsedHeapSize), // non-late attr, unknown at late phase
        };
        // Must not panic; the unknown RHS attr becomes Null, so comparison is false.
        assert!(!eval_retained_pred(&pred, 999));
    }

    // ── Non-arithmetic regression guard ──────────────────────────────────────

    /// A plain `@retainedHeapSize > 100` (no arithmetic) still works exactly as
    /// before. This is the folded-literal fast path that must be byte-identical.
    #[test]
    fn retained_where_plain_literal_still_works() {
        let q = crate::query::parse::parse(
            "SELECT @objectId FROM C WHERE @retainedHeapSize > 100"
        ).unwrap();
        assert!(retained_where_passes(&q, 200));
        assert!(!retained_where_passes(&q, 50));
        assert!(!retained_where_passes(&q, 100)); // strict >
    }

    // ── has_to_string_pred — detector widening ────────────────────────────────

    /// A normal `toString(s) = "foo"` is detected.
    #[test]
    fn has_to_string_pred_detects_plain_compare() {
        let p = Predicate::Compare {
            lhs: Expr::Attr(Attr::ToString("s".to_string())),
            op: CompareOp::Eq,
            rhs: Expr::Lit(Value::Str("foo".to_string())),
        };
        assert!(has_to_string_pred(&p));
    }

    /// `toString(s) + 1 = 5` — toString buried in Binary on LHS. Must be detected
    /// so the late phase runs (even though arith on a String→Null, the phase still
    /// needs to evaluate to avoid silently passing all rows).
    #[test]
    fn has_to_string_pred_detects_buried_in_binary_lhs() {
        let p = Predicate::Compare {
            lhs: Expr::Binary {
                op: ArithOp::Add,
                lhs: Box::new(Expr::Attr(Attr::ToString("s".to_string()))),
                rhs: Box::new(Expr::Lit(Value::Int(1))),
            },
            op: CompareOp::Eq,
            rhs: Expr::Lit(Value::Int(5)),
        };
        assert!(has_to_string_pred(&p));
    }

    /// A plain non-toString compare is not detected.
    #[test]
    fn has_to_string_pred_does_not_detect_plain_attr() {
        let p = Predicate::Compare {
            lhs: Expr::Attr(Attr::RetainedHeapSize),
            op: CompareOp::Gt,
            rhs: Expr::Lit(Value::Int(100)),
        };
        assert!(!has_to_string_pred(&p));
    }

    // ── find_pred_refpath — detector widening ─────────────────────────────────

    /// A normal `x.parent.name > 100` is found.
    #[test]
    fn find_pred_refpath_detects_plain_refpath() {
        let refpath = Attr::RefPath {
            hops: vec!["parent".to_string()],
            tail: Box::new(Attr::Field("name".to_string())),
            role: crate::query::ast::RefRole::PredicateCritical,
        };
        let p = Predicate::Compare {
            lhs: Expr::Attr(refpath.clone()),
            op: CompareOp::Gt,
            rhs: Expr::Lit(Value::Int(100)),
        };
        let found = find_pred_refpath(&p);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), refpath);
    }

    /// `x.parent.name * 2 > 100` — RefPath buried in Binary on LHS. Must be found.
    #[test]
    fn find_pred_refpath_detects_buried_in_binary() {
        let refpath = Attr::RefPath {
            hops: vec!["parent".to_string()],
            tail: Box::new(Attr::Field("hash".to_string())),
            role: crate::query::ast::RefRole::PredicateCritical,
        };
        let p = Predicate::Compare {
            lhs: Expr::Binary {
                op: ArithOp::Mul,
                lhs: Box::new(Expr::Attr(refpath.clone())),
                rhs: Box::new(Expr::Lit(Value::Int(2))),
            },
            op: CompareOp::Gt,
            rhs: Expr::Lit(Value::Int(100)),
        };
        let found = find_pred_refpath(&p);
        assert!(found.is_some(), "should find refpath buried in binary lhs");
        assert_eq!(found.unwrap(), refpath);
    }

    /// A non-refpath compare returns None.
    #[test]
    fn find_pred_refpath_returns_none_for_plain_retained() {
        let p = Predicate::Compare {
            lhs: Expr::Attr(Attr::RetainedHeapSize),
            op: CompareOp::Gt,
            rhs: Expr::Lit(Value::Int(100)),
        };
        assert!(find_pred_refpath(&p).is_none());
    }

    // ── End-to-end via resume(): retained arithmetic ──────────────────────────

    static EMPTY_ID_MAP: IdMap<'static> = IdMap { addr_of: &[] };

    fn ctx_for(retained: &[u64]) -> LateCtx<'_> {
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
            gc_root_tags: &EMPTY_GC_ROOT_TAGS,
            class_idx: &[],
            class_names: &[],
        }
    }

    fn pq(q: &crate::query::ast::Query) -> crate::query::plan::QueryPlan {
        crate::query::plan::plan_query(q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap()
    }

    /// End-to-end: `@retainedHeapSize * 2 > 100` applied through resume().
    /// Idx 1 (retained=60, 60*2=120>100 passes), idx 2 (retained=40, 40*2=80 fails).
    #[test]
    fn e2e_retained_lhs_arith_filters_correctly() {
        let q = crate::query::parse::parse(
            "SELECT @objectId FROM C WHERE @retainedHeapSize * 2 > 100"
        ).unwrap();
        let plan = pq(&q);
        let mut carry = crate::query::carry::Carry::index_only(100);
        carry.push_index(1);
        carry.push_index(2);
        let mut st = crate::query::execute::QueryExecState::new();
        st.push_cross_phase(0, "q_arith".to_string(), plan, carry);
        let retained = {
            let mut v = vec![0u64; 10];
            v[1] = 60;
            v[2] = 40;
            v
        };
        let out = resume(st, &[q.clone(), q], &ctx_for(&retained));
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(r.row_count, 1, "only idx 1 (retained=60, 60*2=120>100) passes");
        assert_eq!(r.rows[0][0], QueryValue::Int(1));
    }

    /// End-to-end: `@retainedHeapSize > 40 * 2` (RHS arithmetic). Same expectation.
    #[test]
    fn e2e_retained_rhs_arith_filters_correctly() {
        let q = crate::query::parse::parse(
            "SELECT @objectId FROM C WHERE @retainedHeapSize > 40 * 2"
        ).unwrap();
        let plan = pq(&q);
        let mut carry = crate::query::carry::Carry::index_only(100);
        carry.push_index(1);
        carry.push_index(2);
        let mut st = crate::query::execute::QueryExecState::new();
        st.push_cross_phase(0, "q_rhs".to_string(), plan, carry);
        let retained = {
            let mut v = vec![0u64; 10];
            v[1] = 100; // 100 > 80 → passes
            v[2] = 50;  // 50 > 80 → fails
            v
        };
        let out = resume(st, &[q.clone(), q], &ctx_for(&retained));
        let r = &out[0];
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(r.row_count, 1, "only idx 1 (retained=100 > 80) passes");
        assert_eq!(r.rows[0][0], QueryValue::Int(1));
    }
}
