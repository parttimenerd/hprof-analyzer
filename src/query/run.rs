//! Live `ClassResolver` over pass2's in-memory class metadata, plus a driver
//! that fans each per-object callback out to the active SingleScan executors.
//! Built and driven inside `Pass2::build` during the 2a heap scan.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::pass1::ClassInfo;
use crate::query::ast::Query;
use crate::query::execute::{ClassResolver, SingleScanExecutor};
use crate::query::model::QueryResult;
use crate::query::plan::QueryPlan;
use crate::query::ObjectVisitor;
use crate::types::HprofType;

/// Resolves a class-object address (`class_id`) to its dotted class name and,
/// for named fields, to the `(offset, type)` within an INSTANCE_DUMP blob.
/// Borrows pass2's live `class_map`/`strings` immutably for the scan's lifetime.
pub struct LiveResolver<'a> {
    class_map: &'a HashMap<u64, ClassInfo>,
    strings: &'a HashMap<u64, String>,
    id_size: usize,
    names: HashMap<u64, String>,
    field_cache: RefCell<HashMap<(u64, String), Option<(u32, HprofType)>>>,
}

impl<'a> LiveResolver<'a> {
    pub fn new(
        class_map: &'a HashMap<u64, ClassInfo>,
        strings: &'a HashMap<u64, String>,
        id_size: usize,
    ) -> Self {
        let mut names = HashMap::with_capacity(class_map.len());
        for (&addr, ci) in class_map {
            if let Some(raw) = strings.get(&ci.name_id) {
                names.insert(addr, raw.replace('/', "."));
            }
        }
        Self { class_map, strings, id_size, names, field_cache: RefCell::new(HashMap::new()) }
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
}

/// Fans each `visit_instance` out to every active SingleScan executor.
pub struct ScanDriver<'q, R: ClassResolver> {
    execs: Vec<SingleScanExecutor<'q, R>>,
}

impl<'q, R: ClassResolver> ScanDriver<'q, R> {
    pub fn new(execs: Vec<SingleScanExecutor<'q, R>>) -> Self {
        Self { execs }
    }
    pub fn is_empty(&self) -> bool {
        self.execs.is_empty()
    }
    /// Finalize every executor into a `QueryResult`, tagging each with its
    /// label (`names[i]`) and original OQL text (`oqls[i]`).
    pub fn finish(self, names: &[String], oqls: &[String]) -> Vec<QueryResult> {
        self.execs
            .into_iter()
            .enumerate()
            .map(|(i, ex)| {
                let mut r = ex.finish(names.get(i).map(String::as_str).unwrap_or(""));
                if let Some(oql) = oqls.get(i) {
                    r.oql = oql.clone();
                }
                r
            })
            .collect()
    }
}

impl<'q, R: ClassResolver> ObjectVisitor for ScanDriver<'q, R> {
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
        for ex in &mut self.execs {
            ex.visit_instance(src_idx, class_id, blob);
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
    let (.., results) =
        crate::pass2::Pass2::build(path, p1, crate::cvec::Codec::Zstd3, &opts, queries)?;
    Ok(results)
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

        let execs = vec![
            SingleScanExecutor::new(&q_foo, &p_foo, &resolver),
            SingleScanExecutor::new(&q_bar, &p_bar, &resolver),
        ];
        let mut driver = ScanDriver::new(execs);
        assert!(!driver.is_empty());

        // Drive a few objects: two Foo (class 10), one Bar (class 20).
        driver.visit_instance(1, 10, &[]);
        driver.visit_instance(2, 20, &[]);
        driver.visit_instance(3, 10, &[]);

        let names = vec!["foo-query".to_string(), "bar-query".to_string()];
        let oqls = vec![
            "SELECT @objectId FROM com.acme.Foo".to_string(),
            "SELECT @objectId FROM com.acme.Bar".to_string(),
        ];
        let results = driver.finish(&names, &oqls);

        assert_eq!(results.len(), 2);

        assert_eq!(results[0].name, "foo-query");
        assert_eq!(results[0].oql, "SELECT @objectId FROM com.acme.Foo");
        assert_eq!(results[0].row_count, 2, "two Foo instances matched");

        assert_eq!(results[1].name, "bar-query");
        assert_eq!(results[1].oql, "SELECT @objectId FROM com.acme.Bar");
        assert_eq!(results[1].row_count, 1, "one Bar instance matched");
    }

    #[test]
    fn empty_driver_is_empty() {
        let driver: ScanDriver<'_, FakeResolver> = ScanDriver::new(Vec::new());
        assert!(driver.is_empty());
    }
}

