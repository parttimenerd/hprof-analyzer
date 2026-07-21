//! Live `ClassResolver` over pass2's in-memory class metadata, plus a driver
//! that fans each per-object callback out to the active SingleScan executors.
//! Built and driven inside `Pass2::build` during the 2a heap scan.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::pass1::ClassInfo;
use crate::id_map::IdMap;
use crate::query::ast::Query;
use crate::query::execute::{ClassResolver, SingleScanExecutor};
use crate::query::model::{QueryResult, QueryValue};
use crate::query::plan::QueryPlan;
use crate::query::ObjectVisitor;
use crate::types::HprofType;

/// Resolves a class-object address (`class_id`) to its dotted class name and,
/// for named fields, to the `(offset, type)` within an INSTANCE_DUMP blob. Also
/// serves per-object `@objectAddress` (via `id_map`) and `@usedHeapSize` (via
/// the dense `shallow` size array). Borrows pass2's live tables immutably for
/// the scan's lifetime.
pub struct LiveResolver<'a> {
    class_map: &'a HashMap<u64, ClassInfo>,
    strings: &'a HashMap<u64, String>,
    id_size: usize,
    names: HashMap<u64, String>,
    id_map: &'a IdMap,
    shallow: &'a [u32],
    field_cache: RefCell<HashMap<(u64, String), Option<(u32, HprofType)>>>,
}

impl<'a> LiveResolver<'a> {
    pub fn new(
        class_map: &'a HashMap<u64, ClassInfo>,
        strings: &'a HashMap<u64, String>,
        id_size: usize,
        id_map: &'a IdMap,
        shallow: &'a [u32],
    ) -> Self {
        let mut names = HashMap::with_capacity(class_map.len());
        for (&addr, ci) in class_map {
            if let Some(raw) = strings.get(&ci.name_id) {
                names.insert(addr, raw.replace('/', "."));
            }
        }
        Self {
            class_map,
            strings,
            id_size,
            names,
            id_map,
            shallow,
            field_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Walk the super-chain from `class_id`, returning the SLASH-form name of the
    /// first class that declares a field named `name` (so `field_offset`'s
    /// `owner_class` filter selects the right declaring class, not a shadowing
    /// subclass field of the same simple name).
    fn owner_of(&self, class_id: u64, name: &str) -> Option<String> {
        let mut cur = class_id;
        loop {
            let ci = self.class_map.get(&cur)?;
            for &(fname_id, _t) in &ci.fields {
                if self.strings.get(&fname_id).map(String::as_str) == Some(name) {
                    return self.strings.get(&ci.name_id).cloned();
                }
            }
            if ci.super_id == 0 {
                return None;
            }
            cur = ci.super_id;
        }
    }
}

impl<'a> ClassResolver for LiveResolver<'a> {
    fn class_name(&self, class_id: u64) -> Option<&str> {
        self.names.get(&class_id).map(String::as_str)
    }

    fn field(&self, class_id: u64, name: &str) -> Option<(u32, HprofType)> {
        let key = (class_id, name.to_string());
        if let Some(cached) = self.field_cache.borrow().get(&key) {
            return *cached;
        }
        let resolved = self.owner_of(class_id, name).and_then(|owner_slash| {
            crate::pass2::sizing::field_offset(
                class_id, name, &owner_slash, self.class_map, self.strings, self.id_size,
            )
        });
        self.field_cache.borrow_mut().insert(key, resolved);
        resolved
    }

    fn addr_of(&self, src_idx: usize) -> Option<u64> {
        (src_idx < self.id_map.len()).then(|| self.id_map.addr_at(src_idx))
    }

    fn shallow_of(&self, src_idx: usize) -> Option<u32> {
        self.shallow.get(src_idx).copied()
    }

    fn index_of_addr(&self, addr: u64) -> Option<usize> {
        self.id_map.index_of(addr)
    }

    fn ref_width(&self) -> usize {
        self.id_size
    }
}

impl<'a> crate::query::plan::FieldSchema for LiveResolver<'a> {
    fn class_field_names(&self, exact_class_name: &str) -> Option<Vec<String>> {
        // `names` is dot-form; the FROM class as written may be dot- or
        // slash-form, so normalize both to dots before matching.
        let want = exact_class_name.replace('/', ".");
        let (&class_id, _) = self.names.iter().find(|(_, n)| **n == want)?;

        let mut fields = Vec::new();
        let mut cur = class_id;
        while let Some(ci) = self.class_map.get(&cur) {
            for &(fname_id, _t) in &ci.fields {
                if let Some(name) = self.strings.get(&fname_id) {
                    if !fields.iter().any(|f| f == name) {
                        fields.push(name.clone());
                    }
                }
            }
            if ci.super_id == 0 {
                break;
            }
            cur = ci.super_id;
        }
        Some(fields)
    }
}

/// Fans each `visit_instance` out to every active SingleScan executor. Each
/// executor is tagged with its `slot` (the original index in the caller's query
/// list) so `finish_state` can reassemble results in input order and route
/// cross-phase (carry) executors to the late stage.
pub struct ScanDriver<'q, R: ClassResolver> {
    execs: Vec<SingleScanExecutor<'q, R>>,
    slots: Vec<usize>,
    /// Armed only when at least one exec's plan has `needs.ref_walk`. Holds the
    /// interned hop-field table + the resolver used to decode ref fields from
    /// each instance blob. `None` on non-RefWalk runs → zero capture cost.
    refwalk: Option<RefWalkState<'q, R>>,
}

/// Sidecar edge-capture state for RefWalk queries (see `refwalk.rs`).
struct RefWalkState<'q, R: ClassResolver> {
    edges: crate::query::refwalk::RefWalkEdges,
    /// Interned hop field names; `field_id` is the index into this table.
    field_names: Vec<String>,
    /// Tail (projected) field names captured per resolved-target object.
    tail_names: Vec<String>,
    /// `dense_idx -> tail field value`, decoded at scan time (blob is gone in
    /// the late window). Keyed by the object that OWNS the tail field.
    tails: crate::query::refwalk::RefWalkTails,
    resolver: &'q R,
}

impl<'q, R: ClassResolver> ScanDriver<'q, R> {
    /// Construct a driver from `(slot, executor)` pairs. `slot` is the query's
    /// index in the caller's list. Arms RefWalk edge capture iff any executor's
    /// plan requests it.
    pub fn new(entries: Vec<(usize, SingleScanExecutor<'q, R>)>) -> Self {
        let mut execs = Vec::with_capacity(entries.len());
        let mut slots = Vec::with_capacity(entries.len());
        for (slot, ex) in entries {
            slots.push(slot);
            execs.push(ex);
        }
        let refwalk = Self::arm_refwalk(&execs);
        Self { execs, slots, refwalk }
    }

    /// Build the RefWalk sidecar if any exec needs it: intern the union of hop
    /// field names across all RefWalk queries, and grab a resolver reference for
    /// blob decoding. Returns `None` (no capture) when no query walks references.
    fn arm_refwalk(execs: &[SingleScanExecutor<'q, R>]) -> Option<RefWalkState<'q, R>> {
        let mut per_query_hops: Vec<Vec<String>> = Vec::new();
        let mut per_query_tails: Vec<Vec<String>> = Vec::new();
        for ex in execs {
            if ex.plan().needs.ref_walk {
                per_query_hops.push(crate::query::refwalk::refwalk_field_names(ex.query()));
                per_query_tails.push(crate::query::refwalk::refwalk_tail_field_names(ex.query()));
            }
        }
        if per_query_hops.is_empty() {
            return None;
        }
        let field_names = crate::query::refwalk::intern_hop_fields(&per_query_hops);
        let tail_names = crate::query::refwalk::intern_hop_fields(&per_query_tails);
        let resolver = execs.first().map(|e| e.resolver())?;
        Some(RefWalkState {
            edges: crate::query::refwalk::RefWalkEdges::new(
                crate::query::refwalk::REFWALK_EDGE_CAP,
            ),
            field_names,
            tail_names,
            tails: crate::query::refwalk::RefWalkTails::new(
                crate::query::refwalk::REFWALK_EDGE_CAP,
            ),
            resolver,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.execs.is_empty()
    }
    /// True if any armed executor's FROM pattern can match an array class. Lets
    /// the pass2 scan skip per-array class-name construction entirely when no
    /// query targets arrays (the common case), keeping the multi-GB array path
    /// allocation-free for instance-only query sets.
    pub fn wants_arrays(&self) -> bool {
        self.execs.iter().any(|e| e.wants_arrays())
    }
    /// Finalize every executor into a `QueryExecState`, each tagged with its
    /// original `slot` (the query's index in the caller's list): row-mode
    /// executors push a finished `QueryResult`; carry-mode (cross-phase)
    /// executors push their carried indices as a pending entry for the late
    /// stage. Name/OQL labels are filled by the caller once results are
    /// reassembled in slot order, so they are left empty here.
    pub fn finish_state(self) -> crate::query::execute::QueryExecState {
        let mut state = crate::query::execute::QueryExecState::new();
        let slots = self.slots;
        for (i, ex) in self.execs.into_iter().enumerate() {
            let slot = slots[i];
            if ex.is_carry() {
                let plan = ex.plan().clone();
                let carry = ex.take_carry();
                state.push_cross_phase(slot, String::new(), plan, carry);
            } else {
                let r = ex.finish("");
                state.push_finished(slot, r);
            }
        }
        state
    }

    /// True if RefWalk edge OR tail capture overflowed its cap (owning query
    /// results must be marked truncated — the per-field CSR / tail table is
    /// incomplete so N-hop projections may miss rows).
    pub fn refwalk_truncated(&self) -> bool {
        self.refwalk
            .as_ref()
            .map(|s| s.edges.truncated() || s.tails.truncated())
            .unwrap_or(false)
    }

    /// Fold the captured field-labeled edges into a per-src forward CSR over `n`
    /// nodes: `(fwd_off[n+1], fwd_tgt, fwd_field)`. Returns `None` when RefWalk
    /// was never armed (non-RefWalk run → the late window keeps empty slices).
    /// Takes `&mut self` so it can run before `finish_state` consumes the driver.
    pub fn take_refwalk_csr(&mut self, n: usize) -> Option<(Vec<u32>, Vec<u32>, Vec<u32>)> {
        let edges = std::mem::replace(
            &mut self.refwalk.as_mut()?.edges,
            crate::query::refwalk::RefWalkEdges::new(0),
        );
        Some(edges.into_csr(n))
    }

    /// Take the captured tail-value side table (`dense_idx -> QueryValue`), or
    /// `None` when RefWalk was never armed. Takes `&mut self` for the same
    /// ordering reason as `take_refwalk_csr`.
    pub fn take_refwalk_tails(
        &mut self,
    ) -> Option<std::collections::HashMap<u32, crate::query::model::QueryValue>> {
        let tails = std::mem::replace(
            &mut self.refwalk.as_mut()?.tails,
            crate::query::refwalk::RefWalkTails::new(0),
        );
        Some(tails.into_map())
    }

    /// Take the interned hop field-name table (`field_id` → name), parallel to
    /// the CSR's `fwd_field` column. `None` when RefWalk was never armed. Takes
    /// `&mut self` for the same ordering reason as `take_refwalk_csr`.
    pub fn take_refwalk_field_names(&mut self) -> Option<Vec<String>> {
        Some(std::mem::take(&mut self.refwalk.as_mut()?.field_names))
    }

    /// Decode the needed reference fields from one instance blob and record their
    /// edges; also capture any tail field this object owns. No-op when unarmed.
    fn capture_refwalk(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
        let Some(state) = self.refwalk.as_mut() else { return };
        let width = state.resolver.ref_width();
        for (field_id, name) in state.field_names.iter().enumerate() {
            let Some((off, ty)) = state.resolver.field(class_id, name) else { continue };
            if ty != HprofType::Object {
                continue;
            }
            let start = off as usize;
            let end = start + width;
            if end > blob.len() {
                continue;
            }
            let mut addr: u64 = 0;
            for &b in &blob[start..end] {
                addr = (addr << 8) | b as u64;
            }
            if addr == 0 {
                continue; // null reference → no edge
            }
            if let Some(dst) = state.resolver.index_of_addr(addr) {
                state.edges.push(src_idx as u32, field_id as u32, dst as u32);
            }
        }
        // Capture tail field values owned by THIS object (keyed by its own dense
        // index — the walk resolves to it, then the late window looks it up).
        for name in &state.tail_names {
            let Some((off, ty)) = state.resolver.field(class_id, name) else { continue };
            if let Some(v) = crate::query::refwalk::decode_primitive_tail(off, ty, blob) {
                state.tails.insert(src_idx as u32, v);
            }
        }
    }
}

impl<'q, R: ClassResolver> ObjectVisitor for ScanDriver<'q, R> {
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
        self.capture_refwalk(src_idx, class_id, blob);
        for ex in &mut self.execs {
            ex.visit_instance(src_idx, class_id, blob);
        }
    }
    fn visit_array(&mut self, src_idx: usize, class_name: &str, length: u32) {
        for ex in &mut self.execs {
            ex.visit_array(src_idx, class_name, length);
        }
    }
}

/// Run the full pass1+pass2 pipeline against `path` for the given planned
/// queries and return their results. Used by the REPL (and available to any
/// one-shot caller). Does not build or render the full report.
///
/// Execution architecture (subqueries): the outer scan needs each
/// `IN (<subquery>)` membership set to be known DURING the scan (the predicate
/// is evaluated per object), so a single pass cannot serve it. When any query
/// uses a subquery we run a TWO-PASS scan over the same dump: an inner pass runs
/// every inner subquery as its own slot and materializes results; we then build
/// the IN-membership sets and FROM-subquery dense-index sets, inject the IN sets
/// into the outer executors, run the outer pass, and finally semi-join each
/// FROM-subquery's outer rows against its inner dense-index set. Queries without
/// subqueries take the ordinary single-pass path (no inner scan).
pub fn run_single_dump(
    path: &str,
    queries: &[(Query, QueryPlan)],
) -> std::io::Result<Vec<QueryResult>> {
    let (flat, groups) = expand_union_queries(queries);
    let opts = crate::AnalyzeOptions::default();

    // Collect the inner subqueries needing an earlier pass, tagged with their
    // outer flat-slot and role (FROM identity vs IN membership on some LHS).
    let inners = collect_subquery_inners(&flat);

    if inners.is_empty() {
        // Fast path: no subqueries — one scan, no injection.
        let p1 = crate::pass1::Pass1::run(path)?;
        let mut empty = std::collections::HashMap::new();
        let (.., state, _refwalk_csr) = crate::pass2::Pass2::build(
            path, p1, crate::cvec::Codec::Zstd3, &opts, &flat, &mut empty,
        )?;
        let flat_results = crate::query::stage_runner::resume_without_late_ctx(state);
        return Ok(collapse_union_results(flat_results, &groups));
    }

    // ── Inner pass: scan the dump once for all inner subqueries ──────────────
    let inner_queries: Vec<(Query, QueryPlan)> =
        inners.iter().map(|i| (i.inner.clone(), i.plan.clone())).collect();
    let p1_inner = crate::pass1::Pass1::run(path)?;
    let mut empty = std::collections::HashMap::new();
    let (.., inner_state, _inner_refwalk_csr) = crate::pass2::Pass2::build(
        path, p1_inner, crate::cvec::Codec::Zstd3, &opts, &inner_queries, &mut empty,
    )?;
    let inner_results = crate::query::stage_runner::resume_without_late_ctx(inner_state);

    // ── Materialize inner results into injectable sets ───────────────────────
    // IN-subqueries → per-outer-slot address membership sets (injected into the
    // outer executors). FROM-subqueries → per-outer-slot sorted dense-index
    // sets (applied as a post-scan semi-join).
    let mut in_sets_by_slot: std::collections::HashMap<
        usize,
        Vec<crate::query::execute::InSet>,
    > = std::collections::HashMap::new();
    // outer_slot → (sorted inner dense indices, inner truncated)
    let mut from_index_by_slot: std::collections::HashMap<usize, (Vec<u32>, bool)> =
        std::collections::HashMap::new();
    for (inner_idx, meta) in inners.iter().enumerate() {
        let res = &inner_results[inner_idx];
        match &meta.role {
            SubqueryRole::In { lhs } => {
                let addrs: Vec<u64> = res.rows.iter().filter_map(|r| row_address(r)).collect();
                let (set, cap_trunc) =
                    build_in_subquery_set(&addrs, crate::query::SUBQUERY_SET_CAP);
                in_sets_by_slot.entry(meta.outer_slot).or_default().push(
                    crate::query::execute::InSet {
                        lhs: lhs.clone(),
                        set,
                        truncated: cap_trunc || res.truncated,
                    },
                );
            }
            SubqueryRole::From => {
                let mut idx: Vec<u32> = res.rows.iter().filter_map(|r| row_dense_index(r)).collect();
                idx.sort_unstable();
                from_index_by_slot.insert(meta.outer_slot, (idx, res.truncated));
            }
        }
    }

    // ── Outer pass: scan again with IN sets injected ─────────────────────────
    let p1_outer = crate::pass1::Pass1::run(path)?;
    let (.., outer_state, _outer_refwalk_csr) = crate::pass2::Pass2::build(
        path, p1_outer, crate::cvec::Codec::Zstd3, &opts, &flat, &mut in_sets_by_slot,
    )?;
    let mut flat_results = crate::query::stage_runner::resume_without_late_ctx(outer_state);

    // ── FROM-subquery semi-join: keep only outer rows whose dense index is in
    //    the inner result set (matched by dense index). ───────────────────────
    for (slot, (inner_idx_sorted, inner_trunc)) in &from_index_by_slot {
        let r = &mut flat_results[*slot];
        // Extract this outer result's own row dense indices, sorted, then
        // intersect. `intersect_from_subquery` returns the kept indices; we use
        // membership to filter the rows in place (preserving row order/shape).
        let keep: std::collections::HashSet<u32> = {
            let mut outer_idx: Vec<u32> = r.rows.iter().filter_map(|r| row_dense_index(r)).collect();
            outer_idx.sort_unstable();
            let (kept, _t) = intersect_from_subquery(inner_idx_sorted, *inner_trunc, &outer_idx);
            kept.into_iter().collect()
        };
        r.rows.retain(|row| row_dense_index(row).map(|i| keep.contains(&i)).unwrap_or(false));
        r.row_count = r.rows.len() as u64;
        if *inner_trunc {
            r.truncated = true;
        }
    }

    Ok(collapse_union_results(flat_results, &groups))
}

/// The role an inner subquery plays for its outer query: a FROM source (semi-
/// joined by object identity) or an IN-predicate membership set (on some LHS).
enum SubqueryRole {
    From,
    In { lhs: crate::query::ast::Attr },
}

/// One inner subquery to run in the earlier pass, tagged with the outer flat-
/// slot it belongs to and its role.
struct SubqueryInner {
    outer_slot: usize,
    role: SubqueryRole,
    inner: Query,
    plan: QueryPlan,
}

/// Gather every inner subquery across the flattened outer queries. Only one
/// level deep is materialized here: a nested inner's own subqueries are planned
/// but this query subset does not run doubly-nested subqueries (the planner
/// still rejects correlation at every level).
fn collect_subquery_inners(flat: &[(Query, QueryPlan)]) -> Vec<SubqueryInner> {
    let mut out = Vec::new();
    for (slot, (_q, plan)) in flat.iter().enumerate() {
        if let Some(fp) = &plan.from_subplan {
            // The inner FROM AST lives on the outer query's FromSource; recover it.
            if let Some(inner) = _q.from.as_subquery() {
                out.push(SubqueryInner {
                    outer_slot: slot,
                    role: SubqueryRole::From,
                    inner: inner.clone(),
                    plan: (**fp).clone(),
                });
            }
        }
        for isp in &plan.in_subplans {
            out.push(SubqueryInner {
                outer_slot: slot,
                role: SubqueryRole::In { lhs: isp.lhs.clone() },
                inner: isp.inner.clone(),
                plan: isp.plan.clone(),
            });
        }
    }
    out
}

/// Extract the dense object index a result row identifies: `SELECT *` yields an
/// `ObjRef { index }`, `SELECT @objectId` an `Int(index)`. Rows that carry
/// neither (e.g. a scalar projection) yield `None` and never join.
fn row_dense_index(row: &[QueryValue]) -> Option<u32> {
    match row.first()? {
        QueryValue::ObjRef { index, .. } => Some(*index as u32),
        QueryValue::Int(i) if *i >= 0 => Some(*i as u32),
        _ => None,
    }
}

/// Extract the object address a result row identifies for IN-membership:
/// `SELECT @objectAddress` yields an `Int(addr)`. Non-address rows yield `None`.
fn row_address(row: &[QueryValue]) -> Option<u64> {
    match row.first()? {
        QueryValue::Int(i) => Some(*i as u64),
        _ => None,
    }
}

/// Concatenate the results of homogeneous `UNION` branches (UNION ALL: no
/// dedup). The first result supplies the column headers; every branch's rows
/// are appended in branch order up to `overall_cap`, past which `truncated` is
/// set. `truncated` also propagates if any individual branch was truncated.
pub fn concat_union(mut branches: Vec<QueryResult>, overall_cap: usize) -> QueryResult {
    let mut out = branches.remove(0);
    for b in branches {
        out.truncated |= b.truncated;
        for row in b.rows {
            if out.rows.len() >= overall_cap {
                out.truncated = true;
                return finalize(out);
            }
            out.rows.push(row);
        }
    }
    finalize(out)
}

fn finalize(mut r: QueryResult) -> QueryResult {
    r.row_count = r.rows.len() as u64;
    r
}

/// Semi-join by dense object index for a `FROM (<subquery>)` source: keep outer
/// rows whose dense index appears in the inner query's result set. Both inputs
/// must be sorted ascending. Inner truncation propagates: a truncated inner set
/// means the membership test is incomplete, so the outer result is truncated too.
pub fn intersect_from_subquery(
    inner_sorted: &[u32],
    inner_truncated: bool,
    outer_sorted: &[u32],
) -> (Vec<u32>, bool) {
    let (mut i, mut j) = (0usize, 0usize);
    let mut out = Vec::new();
    while i < inner_sorted.len() && j < outer_sorted.len() {
        match inner_sorted[i].cmp(&outer_sorted[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(outer_sorted[j]);
                i += 1;
                j += 1;
            }
        }
    }
    (out, inner_truncated)
}

/// Build an object-address membership set from an `IN (<subquery>)` inner
/// query's projected addresses, capped at `cap` distinct entries. Returns the
/// set and whether the cap was hit (truncated — membership is then incomplete).
pub fn build_in_subquery_set(addrs: &[u64], cap: usize) -> (std::collections::HashSet<u64>, bool) {
    let mut set = std::collections::HashSet::with_capacity(addrs.len().min(cap));
    let mut truncated = false;
    for &a in addrs {
        if set.len() >= cap {
            truncated = true;
            break;
        }
        set.insert(a);
    }
    (set, truncated)
}

/// Membership test for an `IN (<subquery>)` predicate: is the outer row's LHS
/// address present in the inner result's address set?
pub fn in_subquery_contains(set: &std::collections::HashSet<u64>, lhs_addr: u64) -> bool {
    set.contains(&lhs_addr)
}

/// One original query's footprint in the flattened scan list: `count`
/// consecutive slots starting at `head`. `count == 1` for a plain query;
/// `1 + N` when the query has N `UNION` branches (head slot followed by one
/// slot per branch, in branch order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnionGroup {
    pub head: usize,
    pub count: usize,
}

/// Flatten a caller's query list so every `UNION` branch becomes its own scan
/// slot (head first, then each branch). Branches are cloned with their own
/// `union_branches` cleared (both AST and plan) so each runs as an ordinary
/// single query through the normal execute/carry/histogram path. Returns the
/// flat `(Query, QueryPlan)` list plus, in caller order, the `UnionGroup`
/// describing how to re-collapse each original query's slots.
pub fn expand_union_queries(
    queries: &[(Query, QueryPlan)],
) -> (Vec<(Query, QueryPlan)>, Vec<UnionGroup>) {
    let mut flat: Vec<(Query, QueryPlan)> = Vec::with_capacity(queries.len());
    let mut groups: Vec<UnionGroup> = Vec::with_capacity(queries.len());
    for (q, plan) in queries {
        let head = flat.len();
        // Head slot: same query/plan but without the branch tail (branches run
        // as their own slots below).
        let mut head_q = q.clone();
        head_q.union_branches.clear();
        let mut head_plan = plan.clone();
        let branch_plans = std::mem::take(&mut head_plan.union_branches);
        flat.push((head_q, head_plan));
        // One slot per branch, AST paired with its pre-planned counterpart.
        for (bq, bplan) in q.union_branches.iter().zip(branch_plans.into_iter()) {
            let mut bq = bq.clone();
            bq.union_branches.clear();
            flat.push((bq, bplan));
        }
        groups.push(UnionGroup { head, count: 1 + q.union_branches.len() });
    }
    (flat, groups)
}

/// Re-collapse flat scan results (in flattened-slot order) back to one result
/// per original query, applying `concat_union` to each `UnionGroup` that spans
/// more than one slot. `results` must be exactly the `flat` list produced by
/// [`expand_union_queries`], in the same order.
pub fn collapse_union_results(
    mut results: Vec<QueryResult>,
    groups: &[UnionGroup],
) -> Vec<QueryResult> {
    // Drain by group so slot indices stay valid regardless of per-group counts.
    let mut it = results.drain(..);
    let mut out: Vec<QueryResult> = Vec::with_capacity(groups.len());
    for g in groups {
        let branch_results: Vec<QueryResult> = (0..g.count)
            .map(|_| it.next().expect("flat results shorter than groups describe"))
            .collect();
        if g.count == 1 {
            out.push(branch_results.into_iter().next().unwrap());
        } else {
            out.push(concat_union(branch_results, crate::query::OVERALL_UNION_CAP));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse::parse;
    use crate::query::plan::plan_query;

    /// Minimal `ClassResolver` mapping a couple of class addresses to names;
    /// fields are unused by these class-only queries.
    struct FakeResolver {
        names: HashMap<u64, String>,
    }

    impl ClassResolver for FakeResolver {
        fn class_name(&self, class_id: u64) -> Option<&str> {
            self.names.get(&class_id).map(String::as_str)
        }
    }

    #[test]
    fn scan_driver_fans_out_and_finish_tags_name_and_oql() {
        let resolver = FakeResolver {
            names: [
                (10u64, "com.acme.Foo".to_string()),
                (20u64, "com.acme.Bar".to_string()),
            ]
            .into_iter()
            .collect(),
        };

        // Two independent SingleScan queries over the same fake resolver.
        let q_foo = parse("SELECT @objectId FROM com.acme.Foo").unwrap();
        let p_foo = plan_query(&q_foo).unwrap();
        let q_bar = parse("SELECT @objectId FROM com.acme.Bar").unwrap();
        let p_bar = plan_query(&q_bar).unwrap();

        let entries = vec![
            (0usize, SingleScanExecutor::new(&q_foo, &p_foo, &resolver)),
            (1usize, SingleScanExecutor::new(&q_bar, &p_bar, &resolver)),
        ];
        let mut driver = ScanDriver::new(entries);
        assert!(!driver.is_empty());

        // Drive a few objects: two Foo (class 10), one Bar (class 20).
        driver.visit_instance(1, 10, &[]);
        driver.visit_instance(2, 20, &[]);
        driver.visit_instance(3, 10, &[]);

        let state = driver.finish_state();

        // Both queries are Phase-1 (no @retainedHeapSize), so both finish.
        assert_eq!(state.finished_len(), 2);
        assert_eq!(state.pending_len(), 0);

        let (finished, _pending) = state.into_parts();
        // finished is (slot, QueryResult); slots preserve input order 0,1.
        let by_slot: std::collections::HashMap<usize, &QueryResult> =
            finished.iter().map(|(s, r)| (*s, r)).collect();

        let foo = by_slot[&0];
        assert_eq!(foo.row_count, 2, "two Foo instances matched");

        let bar = by_slot[&1];
        assert_eq!(bar.row_count, 1, "one Bar instance matched");
    }

    #[test]
    fn empty_driver_is_empty() {
        let driver: ScanDriver<'_, FakeResolver> = ScanDriver::new(Vec::new());
        assert!(driver.is_empty());
    }

    #[test]
    fn index_of_addr_default_is_none() {
        struct Bare;
        impl crate::query::execute::ClassResolver for Bare {
            fn class_name(&self, _c: u64) -> Option<&str> {
                None
            }
        }
        assert_eq!(Bare.index_of_addr(0x1000), None);
    }

    /// Resolver for RefWalk edge-capture tests: class 1 is "C" with a "parent"
    /// object field at offset 0 (ref width 8); addresses map to dense indices.
    struct RefFakeResolver {
        names: HashMap<u64, String>,
        addr_to_idx: HashMap<u64, usize>,
    }
    impl ClassResolver for RefFakeResolver {
        fn class_name(&self, class_id: u64) -> Option<&str> {
            self.names.get(&class_id).map(String::as_str)
        }
        fn field(&self, _class_id: u64, name: &str) -> Option<(u32, HprofType)> {
            if name == "parent" {
                Some((0, HprofType::Object))
            } else {
                None
            }
        }
        fn index_of_addr(&self, addr: u64) -> Option<usize> {
            self.addr_to_idx.get(&addr).copied()
        }
        fn ref_width(&self) -> usize {
            8
        }
    }

    fn be8(v: u64) -> [u8; 8] {
        v.to_be_bytes()
    }

    #[test]
    fn scan_driver_captures_refwalk_edges() {
        let resolver = RefFakeResolver {
            names: [(1u64, "C".to_string())].into_iter().collect(),
            addr_to_idx: [(0x100u64, 5usize), (0x200u64, 6usize)]
                .into_iter()
                .collect(),
        };
        let q = parse("SELECT x.parent.name FROM C x").unwrap();
        let p = plan_query(&q).unwrap();
        assert!(p.needs.ref_walk, "query must arm ref_walk");

        let entries = vec![(0usize, SingleScanExecutor::new(&q, &p, &resolver))];
        let mut driver = ScanDriver::new(entries);

        driver.visit_instance(0, 1, &be8(0x100));
        driver.visit_instance(1, 1, &be8(0x200));

        let csr = driver.take_refwalk_csr(8);
        assert!(csr.is_some(), "armed driver yields a CSR");
        let (_off, tgt, fid) = csr.unwrap();
        assert_eq!(tgt, vec![5, 6]);
        assert_eq!(fid, vec![0, 0]);
    }

    #[test]
    fn scan_driver_null_ref_and_absent_field_and_unarmed() {
        // null ref (addr 0) → no edge; absent field → no edge (no panic).
        let resolver = RefFakeResolver {
            names: [(1u64, "C".to_string())].into_iter().collect(),
            addr_to_idx: [(0x100u64, 5usize)].into_iter().collect(),
        };
        let q = parse("SELECT x.parent.name FROM C x").unwrap();
        let p = plan_query(&q).unwrap();
        let entries = vec![(0usize, SingleScanExecutor::new(&q, &p, &resolver))];
        let mut driver = ScanDriver::new(entries);
        driver.visit_instance(0, 1, &be8(0)); // null → no edge
        driver.visit_instance(1, 1, &be8(0x100)); // real → dense 5
        let (_off, tgt, _fid) = driver.take_refwalk_csr(8).unwrap();
        assert_eq!(tgt, vec![5], "only the non-null ref becomes an edge");

        // Unarmed (no RefWalk query) → take_refwalk_csr is None.
        let q2 = parse("SELECT @objectId FROM C").unwrap();
        let p2 = plan_query(&q2).unwrap();
        let entries2 = vec![(0usize, SingleScanExecutor::new(&q2, &p2, &resolver))];
        let mut driver2 = ScanDriver::new(entries2);
        driver2.visit_instance(0, 1, &be8(0x100));
        assert!(driver2.take_refwalk_csr(8).is_none());
    }

    #[test]
    fn concat_union_appends_and_caps() {
        use crate::query::model::{QueryColumn, QueryValue};
        let col = || vec![QueryColumn { name: "c".into() }];
        let a = QueryResult {
            name: "q".into(),
            oql: "".into(),
            columns: col(),
            rows: vec![vec![QueryValue::Int(1)], vec![QueryValue::Int(2)]],
            row_count: 2,
            truncated: false,
            error: None,
            note: None,
        };
        let b = QueryResult {
            name: "q".into(),
            oql: "".into(),
            columns: col(),
            rows: vec![vec![QueryValue::Int(3)]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
        };
        let out = concat_union(vec![a, b], 10);
        assert_eq!(out.row_count, 3);
        assert_eq!(out.rows.len(), 3);
        assert!(!out.truncated);
        assert_eq!(out.columns.len(), 1, "headers come from the head branch");

        let big = concat_union(
            vec![
                QueryResult {
                    name: "q".into(),
                    oql: "".into(),
                    columns: col(),
                    rows: (0..8).map(|i| vec![QueryValue::Int(i)]).collect(),
                    row_count: 8,
                    truncated: false,
                    error: None,
                    note: None,
                },
                QueryResult {
                    name: "q".into(),
                    oql: "".into(),
                    columns: col(),
                    rows: (0..8).map(|i| vec![QueryValue::Int(i)]).collect(),
                    row_count: 8,
                    truncated: false,
                    error: None,
                    note: None,
                },
            ],
            10,
        );
        assert_eq!(big.rows.len(), 10);
        assert!(big.truncated, "cap exceeded sets truncated");
    }

    #[test]
    fn concat_union_propagates_branch_truncation() {        use crate::query::model::{QueryColumn, QueryValue};
        let col = || vec![QueryColumn { name: "c".into() }];
        let a = QueryResult {
            name: "q".into(),
            oql: "".into(),
            columns: col(),
            rows: vec![vec![QueryValue::Int(1)]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
        };
        // Second branch was itself truncated at scan time; UNION must carry that.
        let b = QueryResult {
            name: "q".into(),
            oql: "".into(),
            columns: col(),
            rows: vec![vec![QueryValue::Int(2)]],
            row_count: 1,
            truncated: true,
            error: None,
            note: None,
        };
        let out = concat_union(vec![a, b], 100);
        assert_eq!(out.rows.len(), 2);
        assert!(out.truncated, "a truncated branch taints the union even under cap");
    }

    fn one_col_result(vals: &[i64]) -> QueryResult {
        use crate::query::model::{QueryColumn, QueryValue};
        QueryResult {
            name: String::new(),
            oql: String::new(),
            columns: vec![QueryColumn { name: "c".into() }],
            rows: vals.iter().map(|&v| vec![QueryValue::Int(v)]).collect(),
            row_count: vals.len() as u64,
            truncated: false,
            error: None,
            note: None,
        }
    }

    #[test]
    fn expand_union_flattens_head_then_branches() {
        // q0: plain; q1: 2 UNION branches (head + 2). Grouping must record
        // 1 slot for q0 and 3 consecutive slots for q1.
        let q_plain = parse("SELECT * FROM com.acme.Foo").unwrap();
        let p_plain = plan_query(&q_plain).unwrap();
        let q_union = parse(
            "SELECT * FROM com.acme.Foo UNION SELECT * FROM com.acme.Bar UNION SELECT * FROM com.acme.Baz",
        )
        .unwrap();
        let p_union = plan_query(&q_union).unwrap();

        let (flat, groups) = expand_union_queries(&[(q_plain, p_plain), (q_union, p_union)]);
        assert_eq!(flat.len(), 4, "1 plain + 3 union slots");
        // Every flat entry must carry no residual branch tail.
        for (q, p) in &flat {
            assert!(q.union_branches.is_empty(), "flattened AST keeps no branch tail");
            assert!(p.union_branches.is_empty(), "flattened plan keeps no branch tail");
        }
        assert_eq!(groups, vec![
            UnionGroup { head: 0, count: 1 },
            UnionGroup { head: 1, count: 3 },
        ]);
    }

    #[test]
    fn collapse_union_merges_branch_slots_only() {
        // Flat results for the layout above: slot0 plain (1 row), slots1-3 the
        // union branches (2 + 1 + 3 rows). After collapse: q0 untouched (1 row),
        // q1 concatenated (6 rows).
        let flat = vec![
            one_col_result(&[10]),
            one_col_result(&[1, 2]),
            one_col_result(&[3]),
            one_col_result(&[4, 5, 6]),
        ];
        let groups = vec![
            UnionGroup { head: 0, count: 1 },
            UnionGroup { head: 1, count: 3 },
        ];
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out.len(), 2, "one result per original query");
        assert_eq!(out[0].row_count, 1);
        assert_eq!(out[1].row_count, 6, "2 + 1 + 3 rows concatenated");
        assert!(!out[1].truncated);
    }

    #[test]
    fn expand_collapse_roundtrip_preserves_plain_query_order() {
        // Two plain queries: flatten is a no-op grouping and collapse returns
        // them in the same order with contents intact.
        let flat = vec![one_col_result(&[1]), one_col_result(&[2, 3])];
        let groups = vec![
            UnionGroup { head: 0, count: 1 },
            UnionGroup { head: 1, count: 1 },
        ];
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].row_count, 1);
        assert_eq!(out[1].row_count, 2);
    }

    // ---------- subquery helpers (Task 23) ----------

    #[test]
    fn intersect_from_subquery_semijoin() {
        // outer scan produced dense idx [1,2,3,5]; inner produced [2,3,4]
        let (kept, trunc) = intersect_from_subquery(&[2, 3, 4], false, &[1, 2, 3, 5]);
        assert_eq!(kept, vec![2, 3]);
        assert!(!trunc);
    }

    #[test]
    fn intersect_from_subquery_propagates_truncation() {
        let (_k, trunc) = intersect_from_subquery(&[2, 3], true, &[2, 3]);
        assert!(trunc, "inner truncation must propagate — result is incomplete");
    }

    #[test]
    fn intersect_from_subquery_disjoint_is_empty() {
        let (kept, _t) = intersect_from_subquery(&[10, 11], false, &[1, 2, 3]);
        assert!(kept.is_empty());
    }

    #[test]
    fn in_subquery_set_membership() {
        let (set, trunc) = build_in_subquery_set(&[10, 20, 30], 100);
        assert!(!trunc);
        assert!(in_subquery_contains(&set, 20));
        assert!(!in_subquery_contains(&set, 99));
        let (_s, t) = build_in_subquery_set(&[1, 2, 3, 4], 2);
        assert!(t, "cap exceeded sets truncated");
    }

    #[test]
    fn in_subquery_set_dedups() {
        // Duplicate addresses collapse; membership unaffected, cap counts uniques.
        let (set, trunc) = build_in_subquery_set(&[5, 5, 5], 100);
        assert!(!trunc);
        assert_eq!(set.len(), 1);
        assert!(in_subquery_contains(&set, 5));
    }
}

