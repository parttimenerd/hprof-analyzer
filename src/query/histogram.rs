//! Aggregate-only query executor. Answers `SELECT COUNT(*), SUM(@usedHeapSize)
//! FROM <class>` style queries from a per-class summary (count + shallow total),
//! with no per-object heap rescan.

use crate::query::ast::{AggFunc, Attr, Query, SelectItem};
use crate::query::execute::{
    class_name_matches, class_name_matches_spec, column_name, compile_from_regex,
};
use crate::query::model::{QueryResult, QueryValue};
use crate::query::plan::QueryPlan;

/// Per-class summary rows the histogram executor reads. One entry per class in
/// the histogram: its normalized name, live instance count, and summed shallow
/// bytes across all instances.
pub struct ClassSummary<'a> {
    pub name: &'a str,
    pub count: u64,
    pub shallow_total: u64,
}

/// Run an aggregate-only query against the class summaries. Caller guarantees
/// `plan.kind == StageKind::HistogramOnly`.
pub fn run_histogram(q: &Query, plan: &QueryPlan, classes: &[ClassSummary]) -> QueryResult {
    let _ = plan; // WHERE on class handled via q.from / class name match
    let mut count = 0u64;
    let mut shallow = 0u64;
    // Compile the FROM regex ONCE (if the target is a quoted regex), then reuse
    // it across the per-class loop below — never recompile per class. The regex
    // was already validated at plan time, so `.ok().flatten()` cannot lose an
    // error here for a query that planned; a bare-ident/glob FROM yields `None`
    // and falls through to `class_name_matches`.
    let from_regex = q
        .from
        .class_spec()
        .and_then(|spec| compile_from_regex(spec).ok().flatten());
    for c in classes {
        let matches = match q.from.class_spec() {
            Some(spec) => class_name_matches_spec(c.name, spec, from_regex.as_ref()),
            None => class_name_matches(c.name, q.from.class_name()),
        };
        if matches {
            count += c.count;
            shallow += c.shallow_total;
        }
    }
    let mut cols = Vec::new();
    let mut row: Vec<QueryValue> = Vec::new();
    for item in &q.select {
        let (name, val) = eval_agg(item, count, shallow);
        cols.push(crate::query::model::QueryColumn { name });
        row.push(val);
    }
    QueryResult {
        name: String::new(),
        oql: String::new(),
        columns: cols,
        rows: vec![row],
        row_count: 1,
        truncated: false,
        error: None,
        note: None,
    }
}

/// Evaluate one aggregate SELECT item against the count/shallow accumulators.
/// Foundation slice supports COUNT(*) and SUM/AVG over @usedHeapSize only (the
/// two scalars a class summary carries). Other aggregate args resolve to Null —
/// the planner keeps such queries as SingleScan when they need per-object data;
/// anything that still reaches here degrades to Null rather than panicking.
fn eval_agg(item: &SelectItem, count: u64, shallow: u64) -> (String, QueryValue) {
    match item {
        SelectItem::Aggregate { func, arg } => {
            let label = column_name(item);
            let arg_is_shallow = matches!(arg.as_ref(), SelectItem::Attr(Attr::UsedHeapSize));
            let v = match func {
                AggFunc::Count => QueryValue::Int(count as i64),
                AggFunc::Sum if arg_is_shallow => QueryValue::Int(shallow as i64),
                AggFunc::Avg if arg_is_shallow && count > 0 => {
                    QueryValue::Float(shallow as f64 / count as f64)
                }
                _ => QueryValue::Null,
            };
            (label, v)
        }
        _ => (column_name(item), QueryValue::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ast::{AggFunc, Attr, SelectItem};

    fn summaries() -> Vec<ClassSummary<'static>> {
        vec![
            ClassSummary {
                name: "java.lang.String",
                count: 100,
                shallow_total: 2400,
            },
            ClassSummary {
                name: "java.util.HashMap",
                count: 10,
                shallow_total: 480,
            },
        ]
    }

    // --- Plan-3 required tests ---

    #[test]
    fn count_star_of_one_class() {
        let q = crate::query::parse::parse("SELECT COUNT(*) FROM java.lang.String").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.rows[0][0], QueryValue::Int(100));
    }

    #[test]
    fn sum_shallow_of_one_class() {
        let q =
            crate::query::parse::parse("SELECT SUM(@usedHeapSize) FROM java.lang.String").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.rows[0][0], QueryValue::Int(2400));
    }

    #[test]
    fn glob_matches_multiple_classes() {
        let q = crate::query::parse::parse("SELECT COUNT(*) FROM java.util.*").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.rows[0][0], QueryValue::Int(10));
    }

    // --- Extra edge-case tests ---

    // MAT gap #5: quoted-regex FROM matches across classes in the histogram path.
    #[test]
    fn regex_from_matches_multiple_java_classes() {
        // Both summary classes are under java.*, so a `java\..*` regex counts both.
        let q = crate::query::parse::parse(r#"SELECT COUNT(*) FROM "java\..*""#).unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let res = run_histogram(&q, &plan, &summaries());
        assert_eq!(res.rows[0][0], QueryValue::Int(110));
    }

    #[test]
    fn regex_from_trailing_string_matches_one_class() {
        let q = crate::query::parse::parse(r#"SELECT COUNT(*) FROM ".*String""#).unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let res = run_histogram(&q, &plan, &summaries());
        assert_eq!(res.rows[0][0], QueryValue::Int(100));
    }

    #[test]
    fn regex_from_matches_nothing_is_zero() {
        let q = crate::query::parse::parse(r#"SELECT COUNT(*) FROM "no\.such\..*""#).unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let res = run_histogram(&q, &plan, &summaries());
        assert_eq!(res.rows[0][0], QueryValue::Int(0));
    }

    /// AVG(@usedHeapSize) plans as HistogramOnly (arg is UsedHeapSize, no
    /// instance_scalar need is set). Verifies 2400/100 == 24.0.
    #[test]
    fn avg_shallow_is_histogram_only_and_correct() {
        let q =
            crate::query::parse::parse("SELECT AVG(@usedHeapSize) FROM java.lang.String").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        assert_eq!(
            plan.kind,
            crate::query::plan::StageKind::HistogramOnly,
            "AVG(@usedHeapSize) must plan as HistogramOnly, got {:?}",
            plan.kind
        );
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.rows[0][0], QueryValue::Float(24.0));
    }

    /// AVG on a class that matches nothing: count=0 hits the `count > 0` guard
    /// and falls to `_ => Null`.
    #[test]
    fn avg_no_match_is_null() {
        let q = crate::query::parse::parse("SELECT AVG(@usedHeapSize) FROM com.nonexistent.Class")
            .unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Null);
    }

    /// COUNT(*) on a class that matches nothing → Int(0).
    #[test]
    fn count_no_match_is_zero() {
        let q = crate::query::parse::parse("SELECT COUNT(*) FROM com.nonexistent.Class").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Int(0));
    }

    /// SUM on a class that matches nothing → Int(0).
    #[test]
    fn sum_no_match_is_zero() {
        let q = crate::query::parse::parse("SELECT SUM(@usedHeapSize) FROM com.nonexistent.Class")
            .unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], QueryValue::Int(0));
    }

    /// Multiple aggregates in one SELECT. Verifies column count, values, and
    /// column labels emitted by column_name.
    #[test]
    fn multiple_aggregates_in_one_select() {
        let q =
            crate::query::parse::parse("SELECT COUNT(*), SUM(@usedHeapSize) FROM java.lang.String")
                .unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.rows[0].len(), 2, "must have two columns");
        assert_eq!(res.rows[0][0], QueryValue::Int(100), "COUNT(*)");
        assert_eq!(res.rows[0][1], QueryValue::Int(2400), "SUM(@usedHeapSize)");
        assert_eq!(res.columns[0].name, "COUNT(*)");
        assert_eq!(res.columns[1].name, "SUM(@usedHeapSize)");
    }

    /// Glob spanning both classes: COUNT(*) = 110, SUM = 2880.
    #[test]
    fn glob_java_star_sums_both_classes() {
        let q =
            crate::query::parse::parse("SELECT COUNT(*), SUM(@usedHeapSize) FROM java.*").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.rows[0][0], QueryValue::Int(110), "COUNT(*)");
        assert_eq!(res.rows[0][1], QueryValue::Int(2880), "SUM(@usedHeapSize)");
    }

    /// SUM over a non-shallow arg (e.g. @objectId) → Null. Tested directly via
    /// eval_agg (private but accessible inside the module) to avoid relying on
    /// the planner routing such a query here.
    #[test]
    fn sum_non_shallow_arg_is_null() {
        let item = SelectItem::Aggregate {
            func: AggFunc::Sum,
            arg: Box::new(SelectItem::Attr(Attr::ObjectId)),
        };
        let (_label, val) = eval_agg(&item, 50, 1200);
        assert_eq!(val, QueryValue::Null);
    }

    /// MIN is not derivable from a class summary → Null.
    #[test]
    fn min_is_null() {
        let item = SelectItem::Aggregate {
            func: AggFunc::Min,
            arg: Box::new(SelectItem::Attr(Attr::UsedHeapSize)),
        };
        let (_label, val) = eval_agg(&item, 50, 1200);
        assert_eq!(val, QueryValue::Null);
    }

    /// MAX is not derivable from a class summary → Null.
    #[test]
    fn max_is_null() {
        let item = SelectItem::Aggregate {
            func: AggFunc::Max,
            arg: Box::new(SelectItem::Attr(Attr::UsedHeapSize)),
        };
        let (_label, val) = eval_agg(&item, 50, 1200);
        assert_eq!(val, QueryValue::Null);
    }

    /// A non-aggregate item in the SELECT (e.g. Star) → Null from the fallback
    /// arm in eval_agg. Exercises the `_ =>` branch.
    #[test]
    fn non_aggregate_select_item_is_null() {
        let item = SelectItem::Star;
        let (_label, val) = eval_agg(&item, 10, 100);
        assert_eq!(val, QueryValue::Null);
    }

    /// AVG with zero count: count=0, `count > 0` guard fails → Null even for
    /// a shallow arg. Directly calls eval_agg.
    #[test]
    fn avg_zero_count_is_null() {
        let item = SelectItem::Aggregate {
            func: AggFunc::Avg,
            arg: Box::new(SelectItem::Attr(Attr::UsedHeapSize)),
        };
        let (_label, val) = eval_agg(&item, 0, 0);
        assert_eq!(val, QueryValue::Null);
    }

    /// run_histogram always emits exactly one row regardless of match count.
    #[test]
    fn result_always_has_exactly_one_row() {
        // Match case
        let q = crate::query::parse::parse("SELECT COUNT(*) FROM java.lang.String").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let res = run_histogram(&q, &plan, &summaries());
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows.len(), 1);

        // No-match case
        let q2 = crate::query::parse::parse("SELECT COUNT(*) FROM no.such.Class").unwrap();
        let plan2 = crate::query::plan::plan_query(&q2).unwrap();
        let res2 = run_histogram(&q2, &plan2, &summaries());
        assert_eq!(res2.row_count, 1);
        assert_eq!(res2.rows.len(), 1);
    }

    /// MAT-parity / normalization invariant: aggregate COUNT(*) over a primitive
    /// array class matches ONLY when the class summary carries the pretty/dotted
    /// name (`char[]`) that queries use — the same normalized form the scan path
    /// projects. A summary built with the pretty name matches and returns its
    /// count. This pins the pass2 contract: build summaries from pretty names.
    #[test]
    fn count_star_of_prim_array_matches_pretty_name() {
        let q = crate::query::parse::parse("SELECT COUNT(*) FROM char[]").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = vec![ClassSummary {
            name: "char[]",
            count: 225,
            shallow_total: 9000,
        }];
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.rows[0][0], QueryValue::Int(225));
    }

    /// Regression pin for the root cause: a summary keyed by the RAW JVM
    /// descriptor `[C` (what pass2 held before the fix) does NOT match a
    /// `FROM char[]` pattern (2 vs 6 bytes), so COUNT would be 0. This is exactly
    /// why the histogram path silently returned 0 while the scan path (which sees
    /// the pretty name) returned rows. `class_name_matches` is the shared matcher;
    /// asserting it directly documents that pass2 MUST normalize before building
    /// summaries — the raw descriptor can never match the query pattern.
    #[test]
    fn raw_descriptor_does_not_match_pretty_pattern() {
        // The raw-vs-pretty asymmetry that caused COUNT(*) FROM char[] == 0.
        assert!(
            !class_name_matches("[C", "char[]"),
            "raw descriptor `[C` must NOT match `char[]` — this is the bug: pass2 \
             fed summaries the raw name, so the histogram path never matched"
        );
        // And the fix's normalized form DOES match.
        assert!(
            class_name_matches("char[]", "char[]"),
            "pretty name `char[]` must match `char[]`"
        );
        // Sanity: a whole summary keyed by the raw name yields COUNT 0.
        let q = crate::query::parse::parse("SELECT COUNT(*) FROM char[]").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let raw = vec![ClassSummary {
            name: "[C",
            count: 225,
            shallow_total: 9000,
        }];
        let res = run_histogram(&q, &plan, &raw);
        assert_eq!(
            res.rows[0][0],
            QueryValue::Int(0),
            "raw-named summary must (wrongly) count 0 — the exact bug"
        );
    }

    /// Verify result metadata: name, oql, truncated, error are defaults.
    #[test]
    fn result_metadata_defaults() {
        let q = crate::query::parse::parse("SELECT COUNT(*) FROM java.lang.String").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let res = run_histogram(&q, &plan, &summaries());
        assert!(res.name.is_empty(), "name must be empty");
        assert!(res.oql.is_empty(), "oql must be empty");
        assert!(!res.truncated);
        assert!(res.error.is_none());
    }
}
