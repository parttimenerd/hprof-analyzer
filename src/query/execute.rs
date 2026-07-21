//! Executor for the supported OQL subset. SingleScanExecutor implements
//! ObjectVisitor and accumulates bounded rows during the pass2 2a scan.
//! HistogramExecutor answers aggregate-only queries from per-class stats.

use crate::query::ast::{Attr, CompareOp, Query, SelectItem, Value};
use crate::query::carry::Carry;
use crate::query::model::{QueryColumn, QueryResult, QueryValue};
use crate::query::plan::QueryPlan;
use crate::query::ObjectVisitor;

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
}

impl QueryExecState {
    pub fn new() -> Self { Self::default() }
    pub fn push_finished(&mut self, slot: usize, r: QueryResult) { self.finished.push((slot, r)); }
    pub fn push_cross_phase(&mut self, slot: usize, name: String, plan: QueryPlan, carry: Carry) {
        self.pending.push(CrossPhaseEntry { slot, name, plan, carry });
    }
    pub fn finished_len(&self) -> usize { self.finished.len() }
    pub fn pending_len(&self) -> usize { self.pending.len() }
    pub fn pending(&self) -> &[CrossPhaseEntry] { &self.pending }
    pub fn has_pending(&self) -> bool { !self.pending.is_empty() }
    /// Consume into (finished slots, pending entries) for the stage runner.
    pub fn into_parts(self) -> (Vec<(usize, QueryResult)>, Vec<CrossPhaseEntry>) {
        (self.finished, self.pending)
    }
}

/// Abstracts class-name resolution + field-offset lookup so the executor can be
/// unit-tested against a fake and run against the real pass2 schema in prod.
pub trait ClassResolver {
    fn class_name(&self, class_id: u64) -> Option<&str>;
    fn field(&self, _class_id: u64, _name: &str) -> Option<(u32, crate::types::HprofType)> { None }
    fn addr_of(&self, _src_idx: usize) -> Option<u64> { None }
    fn shallow_of(&self, _src_idx: usize) -> Option<u32> { None }
}

#[cfg(test)]
pub struct TestSchema { pub names: std::collections::HashMap<u64, String> }

#[cfg(test)]
impl ClassResolver for TestSchema {
    fn class_name(&self, class_id: u64) -> Option<&str> { self.names.get(&class_id).map(|s| s.as_str()) }
}

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
}

impl<'a, R: ClassResolver> SingleScanExecutor<'a, R> {
    pub fn new(query: &'a Query, plan: &'a QueryPlan, resolver: &'a R) -> Self {
        Self { query, plan, resolver, rows: Vec::new(), matched: 0, truncated: false, carry: None }
    }

    /// Construct a cross-phase carry executor. `carry` should be an
    /// `index-only` carry sized by the caller's cap; matched indices are pushed
    /// into it during the scan and extracted with `take_carry` at scan end.
    pub fn new_carry(query: &'a Query, plan: &'a QueryPlan, resolver: &'a R, carry: Carry) -> Self {
        Self { query, plan, resolver, rows: Vec::new(), matched: 0, truncated: false, carry: Some(carry) }
    }

    /// True if this executor is carrying indices for a later phase.
    pub fn is_carry(&self) -> bool { self.carry.is_some() }

    /// The plan this executor runs (borrowed). Used by the driver to tag a
    /// carried query with its plan for the late phase.
    pub fn plan(&self) -> &QueryPlan { self.plan }

    /// Consume a carry executor, returning the accumulated carry. Panics if this
    /// executor is not in carry mode (caller must check `is_carry`).
    pub fn take_carry(self) -> Carry {
        self.carry.expect("take_carry on a non-carry executor")
    }
    fn class_matches(&self, class_id: u64) -> bool {
        let want = &self.query.from.class_name;
        match self.resolver.class_name(class_id) { None => false, Some(name) => class_name_matches(name, want) }
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
        self.query.select.iter().map(|item| self.project_item(item, src_idx, class_id, blob)).collect()
    }
    fn project_item(&self, item: &SelectItem, src_idx: usize, class_id: u64, blob: &[u8]) -> QueryValue {
        match item {
            SelectItem::Star => QueryValue::ObjRef { index: src_idx as u64, class: self.resolver.class_name(class_id).unwrap_or("?").to_string() },
            SelectItem::Aggregate { .. } => QueryValue::Null,
            SelectItem::Attr(a) => self.project_attr(a, src_idx, class_id, blob),
        }
    }
    fn project_attr(&self, a: &Attr, src_idx: usize, class_id: u64, blob: &[u8]) -> QueryValue {
        match a {
            Attr::ObjectId => QueryValue::Int(src_idx as i64),
            Attr::ObjectAddress => self.resolver.addr_of(src_idx).map(|x| QueryValue::Int(x as i64)).unwrap_or(QueryValue::Null),
            Attr::UsedHeapSize => self.resolver.shallow_of(src_idx).map(|x| QueryValue::Int(x as i64)).unwrap_or(QueryValue::Null),
            // filled cross-phase (stage runner) — retained size is unknown during the pass2 scan.
            Attr::RetainedHeapSize => QueryValue::Null,
            Attr::ClassOf | Attr::DisplayName => QueryValue::Str(self.resolver.class_name(class_id).unwrap_or("?").to_string()),
            Attr::Length => QueryValue::Null,
            Attr::Field(name) => self.decode_field(class_id, name, blob),
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
    fn project_array_item(&self, item: &SelectItem, src_idx: usize, class_name: &str, length: u32) -> QueryValue {
        match item {
            SelectItem::Star => QueryValue::ObjRef { index: src_idx as u64, class: class_name.to_string() },
            SelectItem::Aggregate { .. } => QueryValue::Null,
            SelectItem::Attr(a) => self.project_array_attr(a, src_idx, class_name, length),
        }
    }
    fn project_array_attr(&self, a: &Attr, src_idx: usize, class_name: &str, length: u32) -> QueryValue {
        match a {
            Attr::ObjectId => QueryValue::Int(src_idx as i64),
            Attr::ObjectAddress => self.resolver.addr_of(src_idx).map(|x| QueryValue::Int(x as i64)).unwrap_or(QueryValue::Null),
            Attr::UsedHeapSize => self.resolver.shallow_of(src_idx).map(|x| QueryValue::Int(x as i64)).unwrap_or(QueryValue::Null),
            // filled cross-phase (stage runner) — retained size is unknown during the pass2 scan.
            Attr::RetainedHeapSize => QueryValue::Null,
            Attr::ClassOf | Attr::DisplayName => QueryValue::Str(class_name.to_string()),
            Attr::Length => QueryValue::Int(length as i64),
            // Arrays have no named fields; a field reference resolves to Null.
            Attr::Field(_) => QueryValue::Null,
        }
    }
    fn decode_field(&self, class_id: u64, name: &str, blob: &[u8]) -> QueryValue {
        use crate::types::HprofType;
        let name = self.strip_alias(name);
        let Some((off, ty)) = self.resolver.field(class_id, name) else { return QueryValue::Null; };
        let o = off as usize;
        match ty {
            HprofType::Boolean | HprofType::Byte => blob.get(o).map(|&b| {
                if ty == HprofType::Boolean { QueryValue::Bool(b != 0) } else { QueryValue::Int(b as i64) }
            }).unwrap_or(QueryValue::Null),
            HprofType::Short => read_be(blob, o, 2).map(|v| QueryValue::Int(v as i16 as i64)).unwrap_or(QueryValue::Null),
            HprofType::Char => read_be(blob, o, 2).map(|v| QueryValue::Int(v as i64)).unwrap_or(QueryValue::Null),
            HprofType::Int => read_be(blob, o, 4).map(|v| QueryValue::Int(v as i32 as i64)).unwrap_or(QueryValue::Null),
            HprofType::Long => read_be(blob, o, 8).map(|v| QueryValue::Int(v as i64)).unwrap_or(QueryValue::Null),
            HprofType::Float => read_be(blob, o, 4).map(|v| QueryValue::Float(f32::from_bits(v as u32) as f64)).unwrap_or(QueryValue::Null),
            HprofType::Double => read_be(blob, o, 8).map(|v| QueryValue::Float(f64::from_bits(v))).unwrap_or(QueryValue::Null),
            HprofType::Object => QueryValue::Null,
        }
    }
    fn where_passes(&self, class_id: u64, blob: &[u8]) -> bool {
        for term in &self.plan.where_terms {
            // In carry mode, @retainedHeapSize WHERE terms can't be evaluated
            // during the scan (retained size is unknown); they are applied late
            // in stage_runner. Skip them here so a retained predicate doesn't
            // spuriously compare against Null and drop every row.
            if self.carry.is_some() && crate::query::plan::pred_uses_retained(&term.pred) {
                continue;
            }
            if !self.eval_pred(&term.pred, class_id, blob) { return false; }
        }
        true
    }
    fn eval_pred(&self, pred: &crate::query::ast::Predicate, class_id: u64, blob: &[u8]) -> bool {
        use crate::query::ast::Predicate as P;
        match pred {
            P::And(a, b) => self.eval_pred(a, class_id, blob) && self.eval_pred(b, class_id, blob),
            P::Or(a, b) => self.eval_pred(a, class_id, blob) || self.eval_pred(b, class_id, blob),
            P::Not(a) => !self.eval_pred(a, class_id, blob),
            P::InstanceOf(cname) => self.resolver.class_name(class_id).map(|n| class_name_matches(n, cname)).unwrap_or(false),
            P::Compare { lhs, op, rhs } => {
                // src_idx=0 is a placeholder: WHERE in this slice only references
                // blob-scalar fields and class/type data, none of which use the
                // object index. Object-identity attrs (@objectId/@objectAddress) in
                // WHERE are out of scope and would spuriously compare against 0.
                let lv = self.project_attr(lhs, 0, class_id, blob);
                compare_values(&lv, *op, rhs)
            }
        }
    }

    /// WHERE evaluation for array objects: only `@length`, `@objectId`, and
    /// `INSTANCEOF` are meaningful; named-field compares resolve to Null (and
    /// thus behave like the instance path's unknown-field handling).
    fn array_where_passes(&self, class_name: &str, length: u32) -> bool {
        for term in &self.plan.where_terms {
            // See `where_passes`: skip retained terms in carry mode.
            if self.carry.is_some() && crate::query::plan::pred_uses_retained(&term.pred) {
                continue;
            }
            if !self.array_eval_pred(&term.pred, class_name, length) { return false; }
        }
        true
    }
    fn array_eval_pred(&self, pred: &crate::query::ast::Predicate, class_name: &str, length: u32) -> bool {
        use crate::query::ast::Predicate as P;
        match pred {
            P::And(a, b) => self.array_eval_pred(a, class_name, length) && self.array_eval_pred(b, class_name, length),
            P::Or(a, b) => self.array_eval_pred(a, class_name, length) || self.array_eval_pred(b, class_name, length),
            P::Not(a) => !self.array_eval_pred(a, class_name, length),
            P::InstanceOf(cname) => class_name_matches(class_name, cname),
            P::Compare { lhs, op, rhs } => {
                let lv = self.project_array_attr(lhs, 0, class_name, length);
                compare_values(&lv, *op, rhs)
            }
        }
    }

    pub fn finish(self, name: &str) -> QueryResult {
        let columns = self.query.select.iter().map(|it| QueryColumn { name: column_name(it) }).collect();
        QueryResult { name: name.to_string(), oql: String::new(), columns, row_count: self.rows.len() as u64, rows: self.rows, truncated: self.truncated, error: None }
    }
}

impl<'a, R: ClassResolver> ObjectVisitor for SingleScanExecutor<'a, R> {
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
        if !self.class_matches(class_id) { return; }
        if !self.where_passes(class_id, blob) { return; }
        if let Some(carry) = &mut self.carry {
            // Carry mode: no LIMIT here (retained ORDER BY + LIMIT run late);
            // the carry's own cap bounds memory and sets its truncated flag.
            carry.push_index(src_idx as u32);
            return;
        }
        if let Some(limit) = self.plan.limit { if self.matched >= limit { self.truncated = true; return; } }
        self.matched += 1;
        let row = self.project_row(src_idx, class_id, blob);
        self.rows.push(row);
    }

    fn visit_array(&mut self, src_idx: usize, class_name: &str, length: u32) {
        if !class_name_matches(class_name, &self.query.from.class_name) { return; }
        if !self.array_where_passes(class_name, length) { return; }
        if let Some(carry) = &mut self.carry {
            carry.push_index(src_idx as u32);
            return;
        }
        if let Some(limit) = self.plan.limit { if self.matched >= limit { self.truncated = true; return; } }
        self.matched += 1;
        let row = self.project_array_row(src_idx, class_name, length);
        self.rows.push(row);
    }
}

pub fn class_name_matches(name_dotted: &str, pattern: &str) -> bool {
    let name = name_dotted.replace('/', ".");
    let pat = pattern.replace('/', ".");
    if let Some(prefix) = pat.strip_suffix(".*") { name == prefix || name.starts_with(&format!("{prefix}.")) } else { name == pat }
}

/// Read `n` big-endian bytes at `off` as a u64. None if out of range.
fn read_be(blob: &[u8], off: usize, n: usize) -> Option<u64> {
    if off + n > blob.len() { return None; }
    let mut v = 0u64;
    for i in 0..n { v = (v << 8) | blob[off + i] as u64; }
    Some(v)
}

fn compare_values(lv: &QueryValue, op: CompareOp, rhs: &Value) -> bool {
    let ord = match (lv, rhs) {
        (QueryValue::Int(a), Value::Int(b)) => (*a).partial_cmp(b),
        (QueryValue::Int(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
        (QueryValue::Float(a), Value::Int(b)) => a.partial_cmp(&(*b as f64)),
        (QueryValue::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (QueryValue::Str(a), Value::Str(b)) => Some(a.as_str().cmp(b.as_str())),
        (QueryValue::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (QueryValue::Null, Value::Null) => Some(std::cmp::Ordering::Equal),
        _ => None,
    };
    match ord {
        None => matches!(op, CompareOp::Ne),
        Some(o) => match op {
            CompareOp::Eq => o.is_eq(), CompareOp::Ne => o.is_ne(),
            CompareOp::Lt => o.is_lt(), CompareOp::Le => o.is_le(),
            CompareOp::Gt => o.is_gt(), CompareOp::Ge => o.is_ge(),
        },
    }
}

pub fn column_name(it: &SelectItem) -> String {
    match it {
        SelectItem::Star => "*".to_string(),
        SelectItem::Attr(a) => attr_name(a),
        SelectItem::Aggregate { func, arg } => { let f = format!("{func:?}").to_uppercase(); format!("{f}({})", column_name(arg)) }
    }
}

fn attr_name(a: &Attr) -> String {
    match a {
        Attr::ObjectId => "@objectId".into(),
        Attr::ObjectAddress => "@objectAddress".into(),
        Attr::UsedHeapSize => "@usedHeapSize".into(),
        Attr::RetainedHeapSize => "@retainedHeapSize".into(),
        Attr::DisplayName => "@displayName".into(),
        Attr::Length => "@length".into(),
        Attr::ClassOf => "classof".into(),
        Attr::Field(f) => f.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::model::QueryValue;
    use crate::query::parse::parse;
    use crate::query::plan::plan_query;

    fn schema(pairs: &[(u64, &str)]) -> TestSchema {
        TestSchema {
            names: pairs.iter().map(|(id, n)| (*id, n.to_string())).collect(),
        }
    }

    #[test]
    fn exec_state_separates_finished_and_pending() {
        use crate::query::plan::Phase;
        let mut st = QueryExecState::new();
        st.push_finished(0, QueryResult {
            name: "q1".into(), oql: String::new(), columns: vec![],
            rows: vec![], row_count: 0, truncated: false, error: None,
        });
        let q = crate::query::parse::parse("SELECT @retainedHeapSize FROM C").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = plan_query(&q).unwrap();
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
        let plan = plan_query(&q).unwrap();
        let sc = schema(&[(10, "com.acme.Foo")]);
        let carry = crate::query::carry::Carry::index_only(100);
        let mut ex = SingleScanExecutor::new_carry(&q, &plan, &sc, carry);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        let carry = ex.take_carry();
        assert_eq!(carry.indices(), vec![1, 2], "retained WHERE must not filter during scan");
    }

    #[test]
    fn carry_mode_ignores_limit_during_scan() {
        // LIMIT applies AFTER the retained ORDER BY (in stage_runner), so carry
        // mode must carry every match regardless of the query's LIMIT.
        let q = parse(
            "SELECT @objectId FROM com.acme.Foo ORDER BY @retainedHeapSize DESC LIMIT 1").unwrap();
        let plan = plan_query(&q).unwrap();
        let sc = schema(&[(10, "com.acme.Foo")]);
        let carry = crate::query::carry::Carry::index_only(100);
        let mut ex = SingleScanExecutor::new_carry(&q, &plan, &sc, carry);
        for i in 1..=5u32 { ex.visit_instance(i as usize, 10, &[]); }
        let carry = ex.take_carry();
        assert_eq!(carry.indices(), vec![1, 2, 3, 4, 5], "LIMIT must be deferred to the late phase");
    }

    #[test]
    fn carry_mode_still_applies_non_retained_where() {
        // A non-retained WHERE term (on a class match) still filters during the
        // scan; only retained terms are deferred. Here the class filter alone
        // decides membership (no field blob), so a Bar instance is excluded.
        let q = parse("SELECT @objectId FROM com.acme.Foo WHERE @retainedHeapSize > 0").unwrap();
        let plan = plan_query(&q).unwrap();
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
        let plan = plan_query(&q).unwrap();
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
        let plan = plan_query(&q).unwrap();
        let sc = schema(&[(10, "com.acme.Foo")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(3, 10, &[]);
        ex.visit_instance(4, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert!(res.truncated);
    }

    // --- Additional edge-case tests (beyond the plan's two) ---

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
        let plan = plan_query(&q).unwrap();
        let sc = schema(&[(10, "com.acme.Foo")]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(7, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(
            res.rows[0][0],
            QueryValue::ObjRef {
                index: 7,
                class: "com.acme.Foo".into()
            }
        );
    }

    #[test]
    fn no_match_yields_empty_untruncated() {
        let q = parse("SELECT @objectId FROM com.acme.Missing").unwrap();
        let plan = plan_query(&q).unwrap();
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
        let plan = plan_query(&q).unwrap();
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
        let plan = plan_query(&q).unwrap();
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
                fields: std::iter::once(("count".to_string(), (0u32, crate::types::HprofType::Int)))
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
            decode("i", 0, crate::types::HprofType::Int, &[0xff, 0xff, 0xff, 0xff]),
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
            decode("f", 0, crate::types::HprofType::Float, &[0x3f, 0xc0, 0x00, 0x00]),
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let sc = FieldSchema::count_only();
        let ex = SingleScanExecutor::new(&q, &plan, &sc);
        assert_eq!(ex.decode_field(10, "missing", &[0, 0, 0, 1]), QueryValue::Null);
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
        let rhs = crate::query::ast::Value::Str("x".into());
        assert!(!compare_values(&lv, crate::query::ast::CompareOp::Eq, &rhs));
        assert!(compare_values(&lv, crate::query::ast::CompareOp::Ne, &rhs));
        // Other ops on a mismatch are false.
        assert!(!compare_values(&lv, crate::query::ast::CompareOp::Lt, &rhs));
        assert!(!compare_values(&lv, crate::query::ast::CompareOp::Gt, &rhs));
    }

    #[test]
    fn compare_null_equals_null() {
        use crate::query::ast::{CompareOp, Value};
        assert!(compare_values(&QueryValue::Null, CompareOp::Eq, &Value::Null));
        assert!(!compare_values(&QueryValue::Null, CompareOp::Ne, &Value::Null));
    }

    #[test]
    fn compare_int_vs_float_cross() {
        use crate::query::ast::{CompareOp, Value};
        assert!(compare_values(&QueryValue::Int(2), CompareOp::Lt, &Value::Float(2.5)));
        assert!(compare_values(&QueryValue::Float(2.5), CompareOp::Gt, &Value::Int(2)));
        assert!(compare_values(&QueryValue::Int(3), CompareOp::Eq, &Value::Float(3.0)));
    }

    #[test]
    fn compare_string_ordering() {
        use crate::query::ast::{CompareOp, Value};
        assert!(compare_values(
            &QueryValue::Str("abc".into()),
            CompareOp::Lt,
            &Value::Str("abd".into())
        ));
        assert!(compare_values(
            &QueryValue::Str("abc".into()),
            CompareOp::Eq,
            &Value::Str("abc".into())
        ));
    }

    #[test]
    fn compare_bool_ordering() {
        use crate::query::ast::{CompareOp, Value};
        assert!(compare_values(&QueryValue::Bool(false), CompareOp::Lt, &Value::Bool(true)));
        assert!(compare_values(&QueryValue::Bool(true), CompareOp::Eq, &Value::Bool(true)));
    }

    // --- WHERE combinators ---

    #[test]
    fn where_and_filters_both_bounds() {
        let q = crate::query::parse::parse("SELECT @objectId FROM C WHERE count > 5 AND count < 100")
            .unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let q = crate::query::parse::parse("SELECT @objectId FROM C WHERE count < 5 OR count > 100")
            .unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let q =
            crate::query::parse::parse("SELECT @objectId FROM C WHERE x INSTANCEOF C").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let sc = FieldSchema::count_only();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 1]); // class "C" is not "D"
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 0);
    }

    #[test]
    fn where_unknown_field_excludes_for_eq() {
        // Resolver has no "missing" -> decode Null; Null = 1 is a mismatch -> excluded.
        let q =
            crate::query::parse::parse("SELECT @objectId FROM C WHERE missing = 1").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
        let plan = crate::query::plan::plan_query(&q).unwrap();
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
    fn array_field_projection_is_null() {
        // A bare field has no meaning on an array element; project Null.
        let q = crate::query::parse::parse("SELECT n FROM char[]").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let sc = FieldSchema::with_fields(&[("n", 0, crate::types::HprofType::Int)]);
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_array(1, "char[]", 5);
        let res = ex.finish("q1");
        assert_eq!(res.rows[0][0], QueryValue::Null);
    }
}
