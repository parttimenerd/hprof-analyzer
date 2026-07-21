//! Live `ClassResolver` over pass2's in-memory class metadata, plus a driver
//! that fans each per-object callback out to the active SingleScan executors.
//! Built and driven inside `Pass2::build` during the 2a heap scan.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::pass1::ClassInfo;
use crate::id_map::IdMap;
use crate::query::ast::Query;
use crate::query::execute::{ClassResolver, SingleScanExecutor};
use crate::query::model::QueryResult;
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
}

impl<'q, R: ClassResolver> ScanDriver<'q, R> {
    /// Construct a driver from `(slot, executor)` pairs. `slot` is the query's
    /// index in the caller's list.
    pub fn new(entries: Vec<(usize, SingleScanExecutor<'q, R>)>) -> Self {
        let mut execs = Vec::with_capacity(entries.len());
        let mut slots = Vec::with_capacity(entries.len());
        for (slot, ex) in entries {
            slots.push(slot);
            execs.push(ex);
        }
        Self { execs, slots }
    }
    pub fn is_empty(&self) -> bool {
        self.execs.is_empty()
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
}

impl<'q, R: ClassResolver> ObjectVisitor for ScanDriver<'q, R> {
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
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
pub fn run_single_dump(
    path: &str,
    queries: &[(Query, QueryPlan)],
) -> std::io::Result<Vec<QueryResult>> {
    let p1 = crate::pass1::Pass1::run(path)?;
    let opts = crate::AnalyzeOptions::default();
    let (.., state) =
        crate::pass2::Pass2::build(path, p1, crate::cvec::Codec::Zstd3, &opts, queries)?;
    // Query-only path: no retained sizes / dominators are computed, so cross-phase
    // (@retainedHeapSize) carries resolve to actionable errors rather than rows.
    let query_asts: Vec<Query> = queries.iter().map(|(q, _)| q.clone()).collect();
    Ok(crate::query::stage_runner::resume_without_late_ctx(state, &query_asts))
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
}

