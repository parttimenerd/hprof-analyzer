//! Explicit query-optimizer pass. Pure building blocks consumed by later
//! optimizer stages (predicate reordering, limit pushdown, projection
//! deferral). This module holds no query state — it operates on `QueryPlan`s
//! and a `SchemaStats` snapshot of per-class instance counts.

use std::collections::HashMap;

use crate::query::plan::PredCost;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::plan::PredCost;

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
