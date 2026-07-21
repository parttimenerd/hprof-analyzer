//! Late-phase query runner. Consumes the cross-phase carries in a
//! QueryExecState after dominators + retained sizes exist, applies each plan's
//! late_ops, and reassembles all results in original query order.

use crate::query::ast::{Attr, CompareOp, Predicate, Query, SelectItem, SortDir, Value};
use crate::query::execute::{CrossPhaseEntry, QueryExecState};
use crate::query::model::{QueryColumn, QueryResult, QueryValue};
use crate::query::plan::StageOp;

/// Maps a dense object index to its object address (and back, if needed) for
/// building result rows in the late phase.
pub struct IdMap<'a> {
    /// Object address per dense index. Borrowed from the pass2 id tables.
    addr_of: &'a [u64],
}
impl<'a> IdMap<'a> {
    pub fn new(addr_of: &'a [u64]) -> Self { Self { addr_of } }
    pub fn to_addr(&self, dense: u32) -> u64 { self.addr_of.get(dense as usize).copied().unwrap_or(0) }
    #[cfg(test)]
    pub fn identity(_n: usize) -> Self { Self { addr_of: &[] } }
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
    /// PLAN NOTE (Task 27): the production `Graph::fwd_offsets`/`fwd_targets`
    /// forward CSR is consumed by the inbound transpose (`build_from_fwd`) and
    /// freed before the dominator/retained window where `resume` runs — the RSS
    /// pipeline is tuned around freeing that ~6GB CSR early. Wiring RefWalk end
    /// to end therefore needs the forward CSR (and a parallel per-edge field-id
    /// column, which does NOT exist today) preserved into this window. That is a
    /// pipeline change tracked separately; these fields default to empty slices
    /// so the resolver degrades to "no edges" until the CSR is threaded in.
    pub fwd_off: &'a [u32],
    /// Forward-reference CSR targets (dense indices), parallel to `fwd_field`.
    pub fwd_tgt: &'a [u32],
    /// Per-edge field id, parallel to `fwd_tgt`: the interned field name of the
    /// reference that produced each out-edge. Used to follow a *named* hop.
    pub fwd_field: &'a [u32],
    /// Field-name → interned id table (name at index `id`). `field_id` scans it.
    pub field_names: &'a [String],
}

impl LateCtx<'_> {
    /// The interned id of a field name, or `None` if the name is unknown. Linear
    /// scan over the (small) interning table; RefWalk resolves one id per hop.
    pub fn field_id(&self, name: &str) -> Option<u32> {
        self.field_names.iter().position(|f| f == name).map(|p| p as u32)
    }
}

/// Resolve one reference hop: for each source dense index, emit the target dense
/// indices reachable via a forward-ref edge whose field name matches `field`.
/// An unknown field name (or an empty forward CSR) yields no targets.
pub fn resolve_hop(sources: &[u32], field: &str, ctx: &LateCtx) -> Vec<u32> {
    let Some(fid) = ctx.field_id(field) else { return Vec::new(); };
    let mut out = Vec::new();
    for &s in sources {
        let si = s as usize;
        if si + 1 >= ctx.fwd_off.len() { continue; }
        let (start, end) = (ctx.fwd_off[si] as usize, ctx.fwd_off[si + 1] as usize);
        for k in start..end {
            if ctx.fwd_field[k] == fid { out.push(ctx.fwd_tgt[k]); }
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
        slotted.push((
            entry.slot,
            QueryResult {
                name: entry.name.clone(),
                oql: String::new(),
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
                truncated: false,
                error: Some(
                    "@retainedHeapSize requires the full analysis pipeline; \
                     it is not available in the query-only path. Run the full \
                     report (drop --query-only) to use retained-size queries."
                        .to_string(),
                ),
                note: None,
            },
        ));
    }
    slotted.sort_by_key(|(slot, _)| *slot);
    slotted.into_iter().map(|(_, r)| r).collect()
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
            // Later phases add more StageOp variants; an unhandled op must fail
            // loudly rather than silently dropping the query's late work.
            #[allow(unreachable_patterns)]
            other => return QueryResult {
                name: entry.name.clone(), oql: String::new(), columns: Vec::new(),
                rows: Vec::new(), row_count: 0, truncated: false,
                error: Some(format!("stage op {other:?} not supported in this phase")),
                note: None,
            },
        }
    }
    join_retained(entry, q, ctx)
}

/// Build a single-column result of object references from a set of dense
/// indices (dominator children / idoms / retained closure). LIMIT is applied
/// here since these ops don't route through join_retained.
fn dominator_rows(
    entry: &CrossPhaseEntry, q: &Query, indices: &[u32], mut truncated: bool, ctx: &LateCtx,
) -> QueryResult {
    let mut indices = indices.to_vec();
    if let Some(limit) = q.limit {
        if indices.len() as u64 > limit { indices.truncate(limit as usize); truncated = true; }
    }
    let col = q.select.first().map(crate::query::execute::column_name)
        .unwrap_or_else(|| "*".to_string());
    let rows: Vec<Vec<QueryValue>> = indices.iter().map(|&i| {
        vec![QueryValue::ObjRef { index: ctx.id_map.to_addr(i), class: "?".to_string() }]
    }).collect();
    QueryResult {
        name: entry.name.clone(), oql: String::new(),
        columns: vec![QueryColumn { name: col }],
        row_count: rows.len() as u64, rows, truncated, error: None,
        note: None,
    }
}

fn join_retained(entry: &CrossPhaseEntry, q: &Query, ctx: &LateCtx) -> QueryResult {
    let mut rows: Vec<(u32, u64)> = Vec::new();
    for idx in entry.carry.indices() {
        let ret = *ctx.retained.get(idx as usize).unwrap_or(&0);
        if retained_where_passes(q, ret) { rows.push((idx, ret)); }
    }
    if let Some(ob) = &q.order_by {
        if ob.key == Attr::RetainedHeapSize {
            rows.sort_by_key(|(_, r)| *r);
            if ob.dir == SortDir::Desc { rows.reverse(); }
        }
    }
    let mut truncated = entry.carry.truncated();
    if let Some(limit) = q.limit {
        if rows.len() as u64 > limit { rows.truncate(limit as usize); truncated = true; }
    }
    let columns: Vec<QueryColumn> = q.select.iter()
        .map(|it| QueryColumn { name: crate::query::execute::column_name(it) })
        .collect();
    let out_rows: Vec<Vec<QueryValue>> = rows.iter()
        .map(|(idx, ret)| project_late_row(q, *idx, *ret))
        .collect();
    QueryResult {
        name: entry.name.clone(), oql: String::new(), columns,
        row_count: out_rows.len() as u64, rows: out_rows, truncated, error: None,
        note: None,
    }
}

/// Evaluate only the retained-size WHERE terms; non-retained terms were already
/// applied in Phase 1, so they pass here.
fn retained_where_passes(q: &Query, ret: u64) -> bool {
    match &q.where_ { None => true, Some(p) => eval_retained_pred(p, ret) }
}
fn eval_retained_pred(p: &Predicate, ret: u64) -> bool {
    match p {
        Predicate::And(a, b) => eval_retained_pred(a, ret) && eval_retained_pred(b, ret),
        Predicate::Or(a, b) => eval_retained_pred(a, ret) || eval_retained_pred(b, ret),
        Predicate::Not(a) => !eval_retained_pred(a, ret),
        Predicate::Compare { lhs: Attr::RetainedHeapSize, op, rhs } => cmp_u64(ret, *op, rhs),
        _ => true,
    }
}
fn cmp_u64(lv: u64, op: CompareOp, rhs: &Value) -> bool {
    let rv = match rhs { Value::Int(i) => *i as f64, Value::Float(f) => *f, _ => return matches!(op, CompareOp::Ne) };
    let l = lv as f64;
    match op {
        CompareOp::Eq => l == rv, CompareOp::Ne => l != rv,
        CompareOp::Lt => l < rv, CompareOp::Le => l <= rv,
        CompareOp::Gt => l > rv, CompareOp::Ge => l >= rv,
    }
}

/// Project a late row. IndexOnly carries answer only @objectId / @retainedHeapSize;
/// blob-dependent attrs need an IndexPlusScalars carry (later step) and are Null.
fn project_late_row(q: &Query, idx: u32, ret: u64) -> Vec<QueryValue> {
    q.select.iter().map(|it| match it {
        SelectItem::Attr(Attr::ObjectId) => QueryValue::Int(idx as i64),
        SelectItem::Attr(Attr::RetainedHeapSize) => QueryValue::Int(ret as i64),
        SelectItem::Star => QueryValue::ObjRef { index: idx as u64, class: "?".to_string() },
        _ => QueryValue::Null,
    }).collect()
}

/// Dominator-tree children of each matched dense index, in match order, bounded
/// by `cap`. The dominator tree gives each node one parent, so child lists are
/// disjoint (no dedup needed).
pub(crate) fn run_dominator_children(matches: &[u32], cap: usize, ctx: &LateCtx) -> Vec<u32> {
    let mut out = Vec::new();
    for &i in matches {
        let i = i as usize;
        if i + 1 >= ctx.dc_off.len() { continue; }
        let (start, end) = (ctx.dc_off[i] as usize, ctx.dc_off[i + 1] as usize);
        for &child in &ctx.dc_tgt[start..end] {
            if out.len() >= cap { return out; }
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
            if d != u32::MAX { out.push(d); }
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
                if visited[ni] { continue; }
                if out.len() >= cap { return (out, true); }
                visited[ni] = true;
                out.push(node);
                let (start, end) = (ctx.dc_off[ni] as usize, ctx.dc_off[ni + 1] as usize);
                for &child in &ctx.dc_tgt[start..end] {
                    if !visited[child as usize] { stack.push(child); }
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
        }
    }

    static EMPTY_ID_MAP: IdMap<'static> = IdMap { addr_of: &[] };

    fn q_slice(q: &crate::query::ast::Query) -> Vec<crate::query::ast::Query> {
        vec![q.clone(), q.clone()]
    }

    #[test]
    fn join_retained_projects_and_orders_desc() {
        let q = crate::query::parse::parse(
            "SELECT @objectId, @retainedHeapSize FROM C ORDER BY @retainedHeapSize DESC").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        carry.push_index(42);
        carry.push_index(7);
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".to_string(), plan, carry);
        let retained = { let mut v = vec![0u64; 100]; v[42] = 1000; v[7] = 5000; v };
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
        for i in [1u32, 2, 3] { carry.push_index(i); }
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".to_string(), plan, carry);
        let retained = { let mut v = vec![0u64; 10]; v[1]=1000; v[2]=2000; v[3]=3000; v };
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
        st.push_finished(1, QueryResult {
            name: "q_hist".into(), oql: String::new(), columns: vec![],
            rows: vec![vec![QueryValue::Int(99)]], row_count: 1, truncated: false, error: None,
            note: None,
        });
        st.push_cross_phase(0, "q_ret".to_string(), plan, carry);
        let retained = { let mut v = vec![0u64; 10]; v[5]=777; v };
        let out = resume(st, &q_slice(&q), &ctx(&retained));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "q_ret");
        assert_eq!(out[1].name, "q_hist");
    }

    // --- Extra tests (exceed the plan's list) ---

    #[test]
    fn no_where_passes_all() {
        // No WHERE, no ORDER BY, no LIMIT: every carried index is projected.
        let q = crate::query::parse::parse(
            "SELECT @objectId, @retainedHeapSize FROM C").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        for i in [3u32, 8, 1] { carry.push_index(i); }
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".to_string(), plan, carry);
        let retained = { let mut v = vec![0u64; 10]; v[3]=30; v[8]=80; v[1]=10; v };
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
        let q = crate::query::parse::parse(
            "SELECT @objectId FROM C WHERE @retainedHeapSize > 100").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let mut carry = crate::query::carry::Carry::index_only(100);
        for i in [1u32, 2, 3, 4] { carry.push_index(i); }
        let mut st = QueryExecState::new();
        st.push_cross_phase(0, "q1".to_string(), plan, carry);
        let retained = { let mut v = vec![0u64; 10]; v[1]=50; v[2]=150; v[3]=100; v[4]=200; v };
        let out = resume(st, &q_slice(&q), &ctx(&retained));
        let r = &out[0];
        assert_eq!(r.row_count, 2, "only idx 2 (150) and idx 4 (200) exceed 100");
        assert_eq!(r.rows[0][0], QueryValue::Int(2));
        assert_eq!(r.rows[1][0], QueryValue::Int(4));
        assert!(!r.truncated);
    }

    #[test]
    fn empty_carry_yields_empty_result() {
        let q = crate::query::parse::parse(
            "SELECT @objectId, @retainedHeapSize FROM C").unwrap();
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
        (vec![u32::MAX,0,0,1], vec![0u32,2,3,3,3], vec![1u32,2,3],
         vec![100u64,40,10,20], vec![10u32,10,10,20])
    }
    #[test]
    fn late_ctx_exposes_dominator_fields() {
        let (idom, dc_off, dc_tgt, retained, shallow) = tiny_ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx { retained: &retained, idom: &idom, dc_off: &dc_off,
                            dc_tgt: &dc_tgt, shallow: &shallow, id_map: &id_map,
                            fwd_off: &[], fwd_tgt: &[], fwd_field: &[], field_names: &[] };
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
        let ctx = LateCtx { retained:&retained, idom:&idom, dc_off:&dc_off, dc_tgt:&dc_tgt, shallow:&shallow, id_map:&id_map, fwd_off:&[], fwd_tgt:&[], fwd_field:&[], field_names:&[] };
        assert_eq!(run_dominator_children(&[0u32], usize::MAX, &ctx), vec![1u32, 2]);
        assert_eq!(run_dominator_children(&[1u32], usize::MAX, &ctx), vec![3u32]);
        assert!(run_dominator_children(&[2u32], usize::MAX, &ctx).is_empty());
    }
    #[test]
    fn dominator_children_respects_cap() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx { retained:&retained, idom:&idom, dc_off:&dc_off, dc_tgt:&dc_tgt, shallow:&shallow, id_map:&id_map, fwd_off:&[], fwd_tgt:&[], fwd_field:&[], field_names:&[] };
        assert_eq!(run_dominator_children(&[0u32], 1, &ctx).len(), 1);
    }
    #[test]
    fn dominator_of_emits_idom() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx { retained:&retained, idom:&idom, dc_off:&dc_off, dc_tgt:&dc_tgt, shallow:&shallow, id_map:&id_map, fwd_off:&[], fwd_tgt:&[], fwd_field:&[], field_names:&[] };
        // idom = [MAX,0,0,1]: node 3's idom is 1, node 1's idom is 0, root 0 yields nothing.
        assert_eq!(run_dominator_of(&[3u32], &ctx), vec![1u32]);
        assert_eq!(run_dominator_of(&[1u32, 2u32], &ctx), vec![0u32, 0u32]);
        assert!(run_dominator_of(&[0u32], &ctx).is_empty());
    }
    #[test]
    fn retained_set_emits_bounded_closure() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx { retained:&retained, idom:&idom, dc_off:&dc_off, dc_tgt:&dc_tgt, shallow:&shallow, id_map:&id_map, fwd_off:&[], fwd_tgt:&[], fwd_field:&[], field_names:&[] };
        let (mut set, truncated) = run_retained_set(&[0u32], usize::MAX, &ctx);
        set.sort_unstable();
        assert_eq!(set, vec![0u32, 1, 2, 3]);
        assert!(!truncated);
    }
    #[test]
    fn retained_set_overflow_marks_truncated() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx { retained:&retained, idom:&idom, dc_off:&dc_off, dc_tgt:&dc_tgt, shallow:&shallow, id_map:&id_map, fwd_off:&[], fwd_tgt:&[], fwd_field:&[], field_names:&[] };
        let (set, truncated) = run_retained_set(&[0u32], 2, &ctx);
        assert_eq!(set.len(), 2);
        assert!(truncated);
    }
    #[test]
    fn retained_set_dedups_shared_roots() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx { retained:&retained, idom:&idom, dc_off:&dc_off, dc_tgt:&dc_tgt, shallow:&shallow, id_map:&id_map, fwd_off:&[], fwd_tgt:&[], fwd_field:&[], field_names:&[] };
        let (mut set, _t) = run_retained_set(&[1u32, 0u32], usize::MAX, &ctx);
        set.sort_unstable();
        assert_eq!(set, vec![0u32, 1, 2, 3]);
    }

    #[test]
    fn resume_dominator_children_builds_rows() {
        let (idom, dc_off, dc_tgt, retained, shallow) = ctx_parts();
        let id_map = IdMap::identity(4);
        let ctx = LateCtx { retained:&retained, idom:&idom, dc_off:&dc_off, dc_tgt:&dc_tgt, shallow:&shallow, id_map:&id_map, fwd_off:&[], fwd_tgt:&[], fwd_field:&[], field_names:&[] };
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
        let ctx = LateCtx { retained:&retained, idom:&idom, dc_off:&dc_off, dc_tgt:&dc_tgt, shallow:&shallow, id_map:&id_map, fwd_off:&[], fwd_tgt:&[], fwd_field:&[], field_names:&[] };
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
        let ctx = LateCtx { retained:&retained, idom:&idom, dc_off:&dc_off, dc_tgt:&dc_tgt, shallow:&shallow, id_map:&id_map, fwd_off:&[], fwd_tgt:&[], fwd_field:&[], field_names:&[] };
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
            retained: &[], idom: &[], dc_off: &[], dc_tgt: &[], shallow: &[],
            id_map,
            fwd_off, fwd_tgt, fwd_field, field_names,
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
}
