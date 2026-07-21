//! Explicit query-optimizer pass. Pure building blocks consumed by later
//! optimizer stages (predicate reordering, limit pushdown, projection
//! deferral). This module holds no query state — it operates on `QueryPlan`s
//! and a `SchemaStats` snapshot of per-class instance counts.

use std::collections::HashMap;

use crate::query::plan::{PredCost, QueryPlan};

/// Per-class instance counts sampled from the heap, used by the optimizer to
/// estimate predicate selectivity. Built once and shared read-only across the
/// optimization of every query.
#[derive(Debug, Default, Clone)]
pub struct SchemaStats {
    pub instance_counts: HashMap<String, u64>,
}

impl SchemaStats {
    /// Instance count for a class by name, or 0 if the class was never seen
    /// (an absent class has no live instances, so 0 is the correct estimate).
    pub fn count_of(&self, class: &str) -> u64 {
        self.instance_counts.get(class).copied().unwrap_or(0)
    }
}

/// Cost rank of a predicate class: lower = cheaper = evaluate earlier.
/// Type checks are near-free; scalar field compares need a decode; string
/// compares need a decode + UTF-8 materialization; ref-path predicates walk
/// the forward-ref graph and sort last.
pub fn pred_cost_rank(c: PredCost) -> u8 {
    match c {
        PredCost::Type => 0,
        PredCost::Scalar => 1,
        PredCost::Str => 2,
        PredCost::Ref => 3,
    }
}

/// Reorder a plan's WHERE conjuncts cheap-first so inexpensive predicates
/// filter rows before expensive ones evaluate. Stable within a cost class, so
/// user-written order is preserved among equally-cheap predicates. Idempotent.
pub fn reorder_predicates(plan: &mut QueryPlan) {
    plan.where_terms.sort_by_key(|c| pred_cost_rank(c.cost));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::plan::{Conjunct, PredCost};
    use crate::query::ast::{Attr, CompareOp, Predicate, Value};
    use crate::query::parse::parse;
    use crate::query::plan::plan_query;

    // ---------- helpers ----------

    /// Build a minimal Conjunct with a scalar integer-compare predicate (cost = `cost`).
    /// The field name is embedded in the predicate so two conjuncts at the same cost
    /// but different field names can be told apart.
    fn scalar_conjunct(field: &str, cost: PredCost) -> Conjunct {
        Conjunct {
            pred: Predicate::Compare {
                lhs: Attr::Field(field.to_string()),
                op: CompareOp::Gt,
                rhs: Value::Int(0),
            },
            cost,
        }
    }

    // ---------- reorder_predicates tests ----------

    /// reorder_predicates must sort where_terms by pred_cost_rank, cheapest first.
    /// We hand-build the conjuncts (Ref, Str, Scalar, Type in that worst-first order)
    /// to make the test independent of parse+plan_query's own internal sort.
    #[test]
    fn reorder_sorts_cheap_first() {
        // Build a plan whose where_terms are deliberately in worst-first order.
        let mut plan = plan_query(&parse("SELECT @objectId FROM java.lang.String").unwrap()).unwrap();
        plan.where_terms = vec![
            scalar_conjunct("d", PredCost::Ref),
            scalar_conjunct("c", PredCost::Str),
            scalar_conjunct("b", PredCost::Scalar),
            scalar_conjunct("a", PredCost::Type),
        ];

        reorder_predicates(&mut plan);

        let ranks: Vec<u8> = plan.where_terms.iter().map(|c| pred_cost_rank(c.cost)).collect();
        assert!(
            ranks.windows(2).all(|w| w[0] <= w[1]),
            "expected non-decreasing ranks, got: {:?}",
            ranks
        );
        // Cheapest first: rank 0 first, rank 3 last.
        assert_eq!(ranks.first().copied(), Some(0), "first conjunct must be cheapest");
        assert_eq!(ranks.last().copied(), Some(3), "last conjunct must be most expensive");
    }

    /// Within the same cost class, the relative user-written order must be preserved
    /// (stable sort). We put two Scalar conjuncts ('first', 'second') in known order
    /// and verify reorder_predicates preserves that order.
    #[test]
    fn reorder_is_stable_within_cost_class() {
        let mut plan = plan_query(&parse("SELECT @objectId FROM java.lang.String").unwrap()).unwrap();
        plan.where_terms = vec![
            scalar_conjunct("first", PredCost::Scalar),
            scalar_conjunct("second", PredCost::Scalar),
        ];

        reorder_predicates(&mut plan);

        assert_eq!(plan.where_terms.len(), 2);
        match &plan.where_terms[0].pred {
            Predicate::Compare { lhs: Attr::Field(name), .. } => {
                assert_eq!(name, "first", "stable sort must preserve first conjunct's position");
            }
            other => panic!("unexpected predicate: {:?}", other),
        }
        match &plan.where_terms[1].pred {
            Predicate::Compare { lhs: Attr::Field(name), .. } => {
                assert_eq!(name, "second", "stable sort must preserve second conjunct's position");
            }
            other => panic!("unexpected predicate: {:?}", other),
        }
    }

    /// Calling reorder_predicates twice must produce the same result as calling
    /// it once (idempotent).
    #[test]
    fn reorder_is_idempotent() {
        let mut plan = plan_query(&parse("SELECT @objectId FROM java.lang.String").unwrap()).unwrap();
        plan.where_terms = vec![
            scalar_conjunct("d", PredCost::Ref),
            scalar_conjunct("c", PredCost::Str),
            scalar_conjunct("b", PredCost::Scalar),
            scalar_conjunct("a", PredCost::Type),
        ];

        reorder_predicates(&mut plan);
        let after_first: Vec<PredCost> = plan.where_terms.iter().map(|c| c.cost).collect();

        reorder_predicates(&mut plan);
        let after_second: Vec<PredCost> = plan.where_terms.iter().map(|c| c.cost).collect();

        assert_eq!(after_first, after_second, "reorder_predicates must be idempotent");
    }

    /// A plan with no WHERE clause has an empty where_terms; reorder_predicates
    /// must leave it empty and not panic.
    #[test]
    fn reorder_empty_where_is_noop() {
        let mut plan = plan_query(&parse("SELECT * FROM java.lang.String").unwrap()).unwrap();
        assert!(plan.where_terms.is_empty(), "precondition: no WHERE → empty where_terms");

        reorder_predicates(&mut plan); // must not panic

        assert!(plan.where_terms.is_empty(), "where_terms must remain empty after reorder");
    }

    #[test]
    fn pred_cost_orders_cheap_first() {
        assert!(pred_cost_rank(PredCost::Type) < pred_cost_rank(PredCost::Scalar));
        assert!(pred_cost_rank(PredCost::Scalar) < pred_cost_rank(PredCost::Str));
        assert!(pred_cost_rank(PredCost::Str) < pred_cost_rank(PredCost::Ref));
    }

    #[test]
    fn pred_cost_type_is_zero() {
        assert_eq!(pred_cost_rank(PredCost::Type), 0);
    }

    #[test]
    fn schema_stats_count_of_defaults_zero() {
        let stats = SchemaStats::default();
        assert_eq!(stats.count_of("java.lang.String"), 0);
        assert_eq!(stats.count_of("com.example.Foo"), 0);
        assert_eq!(stats.count_of(""), 0);
    }

    #[test]
    fn schema_stats_count_of_returns_inserted() {
        let mut stats = SchemaStats::default();
        stats.instance_counts.insert("java.lang.String".to_string(), 42);
        assert_eq!(stats.count_of("java.lang.String"), 42);
        assert_eq!(stats.count_of("java.lang.Object"), 0);
    }

    #[test]
    fn schema_stats_default_is_empty() {
        assert!(SchemaStats::default().instance_counts.is_empty());
    }
}
