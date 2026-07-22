//! Executor for the supported OQL subset. SingleScanExecutor implements
//! ObjectVisitor and accumulates bounded rows during the pass2 2a scan.
//! HistogramExecutor answers aggregate-only queries from per-class stats.

use crate::query::ObjectVisitor;
use crate::query::ast::{Attr, CompareOp, Query, SelectItem, Value};
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
}

impl QueryExecState {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push_finished(&mut self, slot: usize, r: QueryResult) {
        self.finished.push((slot, r));
    }
    pub fn push_cross_phase(&mut self, slot: usize, name: String, plan: QueryPlan, carry: Carry) {
        self.pending.push(CrossPhaseEntry {
            slot,
            name,
            plan,
            carry,
        });
    }
    pub fn finished_len(&self) -> usize {
        self.finished.len()
    }
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
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
        }
    }

    /// Construct a cross-phase carry executor. `carry` should be an
    /// `index-only` carry sized by the caller's cap; matched indices are pushed
    /// into it during the scan and extracted with `take_carry` at scan end.
    pub fn new_carry(query: &'a Query, plan: &'a QueryPlan, resolver: &'a R, carry: Carry) -> Self {
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

    /// True if this executor is carrying indices for a later phase.
    pub fn is_carry(&self) -> bool {
        self.carry.is_some()
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
            },
            SelectItem::Aggregate { .. } => QueryValue::Null,
            SelectItem::Attr(a) => self.project_attr(a, src_idx, class_id, blob),
            // path(a, b) is cross-phase (needs the ref graph); filled later, not here.
            SelectItem::Path { .. } => QueryValue::Null,
            // toString(s) is cross-phase: resolved post-scan by the stage runner.
            SelectItem::ToString(_) => QueryValue::Null,
            SelectItem::Expr(_) => unreachable!("Expr select item reached before arithmetic wiring"),
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
            // toString(s) is cross-phase: resolved post-scan by the stage runner.
            Attr::ToString(_) => QueryValue::Null,
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
            },
            SelectItem::Aggregate { .. } => QueryValue::Null,
            SelectItem::Attr(a) => self.project_array_attr(a, src_idx, class_name, length),
            // path(a, b) is cross-phase (needs the ref graph); filled later, not here.
            SelectItem::Path { .. } => QueryValue::Null,
            // toString(s) is cross-phase: resolved post-scan by the stage runner.
            SelectItem::ToString(_) => QueryValue::Null,
            SelectItem::Expr(_) => unreachable!("Expr select item reached before arithmetic wiring"),
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
            // toString(s) is cross-phase: resolved post-scan by the stage runner.
            Attr::ToString(_) => QueryValue::Null,
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
        for term in &self.plan.where_terms {
            // In carry mode, @retainedHeapSize WHERE terms can't be evaluated
            // during the scan (retained size is unknown); they are applied late
            // in stage_runner. Skip them here so a retained predicate doesn't
            // spuriously compare against Null and drop every row.
            if self.carry.is_some() && crate::query::plan::pred_uses_retained(&term.pred) {
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
            P::InstanceOf(cname) => self
                .resolver
                .class_name(class_id)
                .map(|n| class_name_matches(n, cname))
                .unwrap_or(false),
            P::InSubquery { lhs, .. } => self.eval_in_subquery(lhs, src_idx),
            P::Compare { lhs, op, rhs } => {
                // Pass the real `src_idx` so object-identity LHS attrs
                // (@objectAddress/@objectId) compare against the actual object,
                // not a placeholder. Blob-scalar and class/type LHS attrs ignore
                // the index, so this is a no-op for them.
                let lhs_attr = lhs.as_attr().expect("Compare lhs is a single attr before arithmetic wiring");
                let rhs_val = rhs.as_lit().expect("Compare rhs is a single literal before arithmetic wiring");
                let lv = self.project_attr(lhs_attr, src_idx, class_id, blob);
                compare_values(&lv, *op, rhs_val, self.like_re_for(rhs_val))
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
        for term in &self.plan.where_terms {
            // See `where_passes`: skip retained terms in carry mode.
            if self.carry.is_some() && crate::query::plan::pred_uses_retained(&term.pred) {
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
            P::Compare { lhs, op, rhs } => {
                let lhs_attr = lhs.as_attr().expect("Compare lhs is a single attr before arithmetic wiring");
                let rhs_val = rhs.as_lit().expect("Compare rhs is a single literal before arithmetic wiring");
                let lv = self.project_array_attr(lhs_attr, src_idx, class_name, length);
                compare_values(&lv, *op, rhs_val, self.like_re_for(rhs_val))
            }
        }
    }

    pub fn finish(self, name: &str) -> QueryResult {
        let columns = query_columns(self.query);
        QueryResult {
            name: name.to_string(),
            oql: String::new(),
            columns,
            row_count: self.rows.len() as u64,
            rows: self.rows,
            truncated: self.truncated,
            error: None,
            note: None,
        }
    }
}

impl<'a, R: ClassResolver> ObjectVisitor for SingleScanExecutor<'a, R> {
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
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
        if let Some(limit) = self.plan.limit {
            if self.matched >= limit {
                self.truncated = true;
                return;
            }
        }
        self.matched += 1;
        let row = self.project_row(src_idx, class_id, blob);
        self.rows.push(row);
    }

    fn visit_array(&mut self, src_idx: usize, class_name: &str, length: u32) {
        // A FROM-subquery source matches every object (identity is constrained
        // by the outer semi-join), so it considers arrays too.
        let class_ok = self.query.from.as_subquery().is_some()
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
        if let Some(limit) = self.plan.limit {
            if self.matched >= limit {
                self.truncated = true;
                return;
            }
        }
        self.matched += 1;
        let row = self.project_array_row(src_idx, class_name, length);
        self.rows.push(row);
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

/// Evaluate a WHERE comparison of a projected LHS `QueryValue` against a literal
/// RHS `Value`. For `LIKE`/`NOT LIKE`, `like_re` MUST be the pre-compiled,
/// anchored regex for the RHS pattern (compiled ONCE at plan/executor-construction
/// time via [`compile_like_regexes`] — NEVER on this per-object hot path). LIKE is
/// meaningful only for a string LHS; a non-string LHS never matches, so `Like` is
/// `false` and `NotLike` is `true` ("not like" holds for anything that isn't a
/// matching string). A missing `like_re` for a LIKE op (a wiring bug — the
/// actionable compile error is raised earlier at plan time) is treated as no
/// match rather than a panic.
fn compare_values(
    lv: &QueryValue,
    op: CompareOp,
    rhs: &Value,
    like_re: Option<&regex::Regex>,
) -> bool {
    if matches!(op, CompareOp::Like | CompareOp::NotLike) {
        let is_like = match (lv, rhs) {
            (QueryValue::Str(s), Value::Str(_)) => {
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
pub fn compile_like_regexes(
    query: &Query,
) -> Result<std::collections::HashMap<String, regex::Regex>, crate::query::QueryError> {
    let mut out = std::collections::HashMap::new();
    if let Some(pred) = &query.where_ {
        collect_like_regexes(pred, &mut out)?;
    }
    Ok(out)
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
        // Non-LIKE compares, InstanceOf, and IN-subqueries carry no LIKE pattern.
        P::Compare { .. } | P::InstanceOf(_) | P::InSubquery { .. } => {}
    }
    Ok(())
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
        SelectItem::Expr(_) => unreachable!("Expr select item reached before arithmetic wiring"),
    }
}

/// Build output columns for a query, applying per-item AS aliases where present.
pub(crate) fn query_columns(q: &Query) -> Vec<QueryColumn> {
    debug_assert_eq!(q.select.len(), q.select_aliases.len(), "select and select_aliases must stay parallel");
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

fn path_operand_name(p: &crate::query::ast::PathOperand) -> String {
    use crate::query::ast::PathOperand;
    match p {
        PathOperand::Alias(s) | PathOperand::Class(s) => s.clone(),
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
        Attr::Inbounds => "@inbounds".into(),
        Attr::Outbounds => "@outbounds".into(),
        Attr::ClassOf => "classof".into(),
        Attr::Dominators(a) => format!("dominators({a})"),
        Attr::DominatorOf(a) => format!("dominatorof({a})"),
        Attr::ToString(a) => format!("toString({a})"),
        Attr::Field(f) => f.clone(),
        Attr::RefPath { hops, tail, .. } => {
            let mut s = hops.join(".");
            s.push('.');
            s.push_str(&attr_name(tail));
            s
        }
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
                class: "com.acme.Foo".into()
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
        let rhs = crate::query::ast::Value::Str("x".into());
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
        use crate::query::ast::{CompareOp, Value};
        assert!(compare_values(
            &QueryValue::Null,
            CompareOp::Eq,
            &Value::Null,
            None
        ));
        assert!(!compare_values(
            &QueryValue::Null,
            CompareOp::Ne,
            &Value::Null,
            None
        ));
    }

    #[test]
    fn compare_int_vs_float_cross() {
        use crate::query::ast::{CompareOp, Value};
        assert!(compare_values(
            &QueryValue::Int(2),
            CompareOp::Lt,
            &Value::Float(2.5),
            None
        ));
        assert!(compare_values(
            &QueryValue::Float(2.5),
            CompareOp::Gt,
            &Value::Int(2),
            None
        ));
        assert!(compare_values(
            &QueryValue::Int(3),
            CompareOp::Eq,
            &Value::Float(3.0),
            None
        ));
    }

    #[test]
    fn compare_string_ordering() {
        use crate::query::ast::{CompareOp, Value};
        assert!(compare_values(
            &QueryValue::Str("abc".into()),
            CompareOp::Lt,
            &Value::Str("abd".into()),
            None
        ));
        assert!(compare_values(
            &QueryValue::Str("abc".into()),
            CompareOp::Eq,
            &Value::Str("abc".into()),
            None
        ));
    }

    #[test]
    fn compare_bool_ordering() {
        use crate::query::ast::{CompareOp, Value};
        assert!(compare_values(
            &QueryValue::Bool(false),
            CompareOp::Lt,
            &Value::Bool(true),
            None
        ));
        assert!(compare_values(
            &QueryValue::Bool(true),
            CompareOp::Eq,
            &Value::Bool(true),
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
        use crate::query::ast::{CompareOp, Value};
        let re = like_re("m.*");
        assert!(compare_values(
            &QueryValue::Str("main".into()),
            CompareOp::Like,
            &Value::Str("m.*".into()),
            Some(&re)
        ));
    }

    #[test]
    fn like_non_match_false() {
        use crate::query::ast::{CompareOp, Value};
        let re = like_re("m.*");
        assert!(!compare_values(
            &QueryValue::Str("worker".into()),
            CompareOp::Like,
            &Value::Str("m.*".into()),
            Some(&re)
        ));
    }

    #[test]
    fn like_is_anchored_full_match_not_substring() {
        use crate::query::ast::{CompareOp, Value};
        // "submaine" contains "m.*" as a substring but is NOT a full match of
        // `^(?:m.*)$`, so LIKE must be false (anchoring proof).
        let re = like_re("m.*");
        assert!(!compare_values(
            &QueryValue::Str("submaine".into()),
            CompareOp::Like,
            &Value::Str("m.*".into()),
            Some(&re)
        ));
    }

    #[test]
    fn not_like_inverts_like() {
        use crate::query::ast::{CompareOp, Value};
        let re = like_re("m.*");
        // "main" matches → NOT LIKE is false.
        assert!(!compare_values(
            &QueryValue::Str("main".into()),
            CompareOp::NotLike,
            &Value::Str("m.*".into()),
            Some(&re)
        ));
        // "worker" doesn't match → NOT LIKE is true.
        assert!(compare_values(
            &QueryValue::Str("worker".into()),
            CompareOp::NotLike,
            &Value::Str("m.*".into()),
            Some(&re)
        ));
    }

    #[test]
    fn like_alternation_anchored() {
        use crate::query::ast::{CompareOp, Value};
        let re = like_re("foo|bar");
        // Both alternatives match fully.
        assert!(compare_values(
            &QueryValue::Str("foo".into()),
            CompareOp::Like,
            &Value::Str("foo|bar".into()),
            Some(&re)
        ));
        assert!(compare_values(
            &QueryValue::Str("bar".into()),
            CompareOp::Like,
            &Value::Str("foo|bar".into()),
            Some(&re)
        ));
        // "xfooy" is NOT a full match (anchoring wraps the whole alternation).
        assert!(!compare_values(
            &QueryValue::Str("xfooy".into()),
            CompareOp::Like,
            &Value::Str("foo|bar".into()),
            Some(&re)
        ));
    }

    #[test]
    fn like_on_numeric_lhs_false_not_like_true() {
        use crate::query::ast::{CompareOp, Value};
        // A non-string LHS never matches a regex: LIKE is false, NOT LIKE true.
        let re = like_re("m.*");
        assert!(!compare_values(
            &QueryValue::Int(42),
            CompareOp::Like,
            &Value::Str("m.*".into()),
            Some(&re)
        ));
        assert!(compare_values(
            &QueryValue::Int(42),
            CompareOp::NotLike,
            &Value::Str("m.*".into()),
            Some(&re)
        ));
    }

    #[test]
    fn like_missing_compiled_regex_is_no_match() {
        use crate::query::ast::{CompareOp, Value};
        // A wiring bug (no compiled regex) is treated as no-match, never a panic.
        assert!(!compare_values(
            &QueryValue::Str("main".into()),
            CompareOp::Like,
            &Value::Str("m.*".into()),
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
        let q = crate::query::parse::parse(
            "SELECT @objectId AS myid FROM com.acme.Foo",
        )
        .unwrap();
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
        let q =
            crate::query::parse::parse("SELECT @usedHeapSize FROM com.acme.Foo").unwrap();
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
        let q = crate::query::parse::parse(
            r#"SELECT @usedHeapSize AS "size" FROM com.acme.Foo"#,
        )
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
        let q = crate::query::parse::parse(
            "SELECT COUNT(*) AS n FROM com.acme.Foo",
        )
        .unwrap();
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
            union_branches: Vec::new(),
            union_limit: None,
        };
        let cols = query_columns(&q);
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].name, "myid");
        assert_eq!(cols[1].name, "*");
    }
}
