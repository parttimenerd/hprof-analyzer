//! Late-phase query runner. Consumes the cross-phase carries in a
//! QueryExecState after dominators + retained sizes exist, applies each plan's
//! late_ops, and reassembles all results in original query order.

use crate::query::ast::{Attr, CompareOp, Predicate, Query, SelectItem, SortDir, Value};
use crate::query::execute::{CrossPhaseEntry, QueryExecState};
use crate::query::model::{QueryColumn, QueryResult, QueryValue};
use crate::query::plan::StageOp;

/// Borrowed late-phase context. Lives only inside the `dc_*`/retained window in
/// main. This step reads only `retained`; later stages grow this struct —
/// never remove fields.
pub struct LateCtx<'a> {
    /// Retained size per dense object index (bytes).
    pub retained: &'a [u64],
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
pub fn resume_without_late_ctx(state: QueryExecState, _queries: &[Query]) -> Vec<QueryResult> {
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
            },
        ));
    }
    slotted.sort_by_key(|(slot, _)| *slot);
    slotted.into_iter().map(|(_, r)| r).collect()
}

fn run_entry(entry: &CrossPhaseEntry, q: &Query, ctx: &LateCtx) -> QueryResult {
    for op in &entry.plan.late_ops {
        match op {
            StageOp::JoinRetained => {}
            // Later phases add more StageOp variants; an unhandled op must fail
            // loudly rather than silently dropping the query's late work.
            #[allow(unreachable_patterns)]
            other => return QueryResult {
                name: entry.name.clone(), oql: String::new(), columns: Vec::new(),
                rows: Vec::new(), row_count: 0, truncated: false,
                error: Some(format!("stage op {other:?} not supported in this phase")),
            },
        }
    }
    join_retained(entry, q, ctx)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::execute::QueryExecState;
    use crate::query::model::{QueryResult, QueryValue};

    fn ctx(retained: &[u64]) -> LateCtx<'_> { LateCtx { retained } }

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
