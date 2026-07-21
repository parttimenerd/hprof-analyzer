//! Needs analysis + planning for the supported OQL subset. Cost is per-need:
//! each flag arms exactly one piece of machinery. Deferred constructs are
//! rejected here (not in the parser) with a message naming the construct.

use crate::query::ast::{Attr, Predicate, Query, SelectItem, Value};
use crate::query::QueryError;

/// Per-need cost flags. Each flag independently arms exactly one piece of
/// machinery; an unset flag arms nothing. (Foundation subset — ref/retained/
/// dominator/edge needs are added in later slices.)
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QueryNeeds {
    pub histogram: bool,
    pub instance_scalar: bool,
    pub instance_string: bool,
    pub runtime_type: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    HistogramOnly,
    SingleScan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredCost {
    Type,
    Scalar,
    Str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conjunct {
    pub pred: Predicate,
    pub cost: PredCost,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryPlan {
    pub kind: StageKind,
    pub needs: QueryNeeds,
    pub where_terms: Vec<Conjunct>,
    pub limit: Option<u64>,
}

pub fn plan_query(q: &Query) -> Result<QueryPlan, QueryError> {
    if q.distinct {
        return Err(QueryError(
            "DISTINCT is deferred and not supported in this version; \
             remove DISTINCT and run the query without it"
                .into(),
        ));
    }

    let mut needs = QueryNeeds::default();
    let mut is_aggregate = false;

    for item in &q.select {
        match item {
            SelectItem::Aggregate { arg, .. } => {
                is_aggregate = true;
                note_attr_need(arg, &mut needs)?;
            }
            SelectItem::Star => {}
            SelectItem::Attr(a) => note_attr_need_attr(a, &mut needs),
        }
    }

    let mut where_terms = Vec::new();
    if let Some(pred) = &q.where_ {
        collect_pred_needs(pred, &mut needs)?;
        flatten_and(pred.clone(), &mut where_terms);
        where_terms.sort_by_key(|c| pred_cost_rank(c.cost));
    }

    let kind = if is_aggregate
        && !needs.instance_scalar
        && !needs.instance_string
        && where_terms.is_empty()
    {
        needs.histogram = true;
        StageKind::HistogramOnly
    } else {
        StageKind::SingleScan
    };

    Ok(QueryPlan { kind, needs, where_terms, limit: q.limit })
}

fn note_attr_need(item: &SelectItem, needs: &mut QueryNeeds) -> Result<(), QueryError> {
    match item {
        SelectItem::Star => Ok(()),
        SelectItem::Attr(a) => { note_attr_need_attr(a, needs); Ok(()) }
        SelectItem::Aggregate { .. } => Err(QueryError(
            "nested aggregate is deferred and not supported in this version; \
             an aggregate function may not take another aggregate as its argument"
                .into(),
        )),
    }
}

fn note_attr_need_attr(a: &Attr, needs: &mut QueryNeeds) {
    match a {
        Attr::DisplayName => needs.instance_string = true,
        Attr::ClassOf => needs.runtime_type = true,
        Attr::Field(_) => {
            needs.instance_scalar = true;
        }
        _ => {}
    }
}

fn collect_pred_needs(pred: &Predicate, needs: &mut QueryNeeds) -> Result<(), QueryError> {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            collect_pred_needs(a, needs)?;
            collect_pred_needs(b, needs)
        }
        Predicate::Not(a) => collect_pred_needs(a, needs),
        Predicate::InstanceOf(_) => { needs.runtime_type = true; Ok(()) }
        Predicate::Compare { lhs, rhs, .. } => {
            match lhs {
                Attr::Field(_) => {
                    if matches!(rhs, Value::Str(_)) {
                        needs.instance_string = true;
                    } else {
                        needs.instance_scalar = true;
                    }
                }
                Attr::DisplayName => needs.instance_string = true,
                Attr::ClassOf => needs.runtime_type = true,
                _ => {}
            }
            Ok(())
        }
    }
}

fn flatten_and(pred: Predicate, out: &mut Vec<Conjunct>) {
    match pred {
        Predicate::And(a, b) => {
            flatten_and(*a, out);
            flatten_and(*b, out);
        }
        other => {
            let cost = pred_cost(&other);
            out.push(Conjunct { pred: other, cost });
        }
    }
}

fn pred_cost(pred: &Predicate) -> PredCost {
    match pred {
        Predicate::InstanceOf(_) => PredCost::Type,
        Predicate::Not(a) => pred_cost(a),
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_cost(a).max_cost(pred_cost(b))
        }
        Predicate::Compare { lhs, rhs, .. } => match lhs {
            Attr::Field(_) if matches!(rhs, Value::Str(_)) => PredCost::Str,
            Attr::DisplayName => PredCost::Str,
            Attr::ClassOf => PredCost::Type,
            _ => PredCost::Scalar,
        },
    }
}

impl PredCost {
    fn max_cost(self, other: PredCost) -> PredCost {
        if pred_cost_rank(self) >= pred_cost_rank(other) { self } else { other }
    }
}

fn pred_cost_rank(c: PredCost) -> u8 {
    match c {
        PredCost::Type => 0,
        PredCost::Scalar => 1,
        PredCost::Str => 2,
    }
}

impl QueryPlan {
    /// Human-readable plan summary for `!explain` / `!plan`.
    pub fn explain(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("stage: {:?}\n", self.kind));
        let mut armed = Vec::new();
        if self.needs.histogram { armed.push("histogram"); }
        if self.needs.instance_scalar { armed.push("instance_scalar"); }
        if self.needs.instance_string { armed.push("instance_string"); }
        if self.needs.runtime_type { armed.push("runtime_type"); }
        s.push_str(&format!(
            "needs (armed): {}\n",
            if armed.is_empty() { "none".into() } else { armed.join(", ") }
        ));
        if let Some(n) = self.limit {
            s.push_str(&format!("limit: {n}\n"));
        }
        if !self.where_terms.is_empty() {
            s.push_str("where (cheapest-first):\n");
            for c in &self.where_terms {
                s.push_str(&format!("  [{:?}] {:?}\n", c.cost, c.pred));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse::parse;

    #[test]
    fn histogram_only_needs() {
        let plan = plan_query(&parse("SELECT COUNT(*) FROM java.lang.String").unwrap()).unwrap();
        assert_eq!(plan.kind, StageKind::HistogramOnly);
        assert!(!plan.needs.instance_scalar);
        assert!(!plan.needs.instance_string);
    }

    #[test]
    fn single_scan_scalar_needs() {
        let plan = plan_query(
            &parse("SELECT @objectId FROM C WHERE count > 3").unwrap(),
        )
        .unwrap();
        assert_eq!(plan.kind, StageKind::SingleScan);
        assert!(plan.needs.instance_scalar);
        assert!(!plan.needs.instance_string);
    }

    #[test]
    fn string_projection_sets_string_need() {
        let plan = plan_query(&parse("SELECT @displayName FROM java.lang.String").unwrap()).unwrap();
        assert!(plan.needs.instance_string);
    }

    #[test]
    fn rejects_retained_heap() {
        // @retainedHeapSize is not a known @attr in this slice -> parser rejects it.
        assert!(parse("SELECT @retainedHeapSize FROM C").is_err());
    }

    #[test]
    fn rejects_distinct_for_now() {
        let err = plan_query(&parse("SELECT DISTINCT * FROM C").unwrap()).unwrap_err();
        assert!(err.0.to_lowercase().contains("distinct"), "got: {}", err.0);
    }

    #[test]
    fn predicates_ordered_cheapest_first() {
        let plan = plan_query(
            &parse("SELECT * FROM C WHERE name = \"x\" AND count > 1").unwrap(),
        )
        .unwrap();
        assert!(matches!(
            plan.where_terms.first(),
            Some(Conjunct { cost: PredCost::Scalar, .. })
        ));
    }

    // --- Additional tests requested by the user ---

    #[test]
    fn classof_projection_sets_runtime_type() {
        let plan =
            plan_query(&parse("SELECT classof(s) FROM java.lang.String s").unwrap()).unwrap();
        assert!(plan.needs.runtime_type);
        assert!(!plan.needs.instance_string);
        assert!(!plan.needs.instance_scalar);
    }

    #[test]
    fn instanceof_where_sets_runtime_type_and_type_cost() {
        let plan = plan_query(
            &parse("SELECT * FROM C WHERE s INSTANCEOF java.lang.String").unwrap(),
        )
        .unwrap();
        assert!(plan.needs.runtime_type);
        assert!(matches!(
            plan.where_terms.first(),
            Some(Conjunct { cost: PredCost::Type, .. })
        ));
    }

    #[test]
    fn displayname_compare_sets_string_need_and_str_cost() {
        let plan = plan_query(
            &parse("SELECT * FROM C WHERE @displayName = \"foo\"").unwrap(),
        )
        .unwrap();
        assert!(plan.needs.instance_string);
        assert!(matches!(
            plan.where_terms.first(),
            Some(Conjunct { cost: PredCost::Str, .. })
        ));
    }

    #[test]
    fn mixed_where_full_cheapest_first_order() {
        // Written worst-first on purpose: Str, Scalar, Type. Expect Type, Scalar, Str.
        let plan = plan_query(
            &parse(
                "SELECT * FROM C WHERE name = \"x\" AND count > 1 \
                 AND s INSTANCEOF java.lang.String",
            )
            .unwrap(),
        )
        .unwrap();
        let costs: Vec<PredCost> = plan.where_terms.iter().map(|c| c.cost).collect();
        assert_eq!(
            costs,
            vec![PredCost::Type, PredCost::Scalar, PredCost::Str],
            "got: {costs:?}"
        );
        assert!(plan.needs.instance_scalar);
        assert!(plan.needs.instance_string);
        assert!(plan.needs.runtime_type);
    }

    #[test]
    fn explain_lists_kind_and_needs() {
        let plan = plan_query(&parse("SELECT @objectId FROM C WHERE count > 3").unwrap()).unwrap();
        let text = plan.explain();
        assert!(text.contains("SingleScan"));
        assert!(text.contains("instance_scalar"));
        assert!(text.contains("cheapest-first"));
    }

    #[test]
    fn explain_histogram_only_no_where() {
        let plan =
            plan_query(&parse("SELECT COUNT(*) FROM java.lang.String").unwrap()).unwrap();
        let text = plan.explain();
        assert!(text.contains("HistogramOnly"), "got: {text}");
        assert!(text.contains("histogram"), "got: {text}");
        assert!(!text.contains("where (cheapest-first)"), "got: {text}");
        assert!(!text.contains("limit:"), "got: {text}");
    }

    #[test]
    fn explain_shows_limit() {
        let plan = plan_query(&parse("SELECT * FROM C LIMIT 10").unwrap()).unwrap();
        let text = plan.explain();
        assert!(text.contains("limit: 10"), "got: {text}");
    }

    #[test]
    fn explain_no_needs_shows_none() {
        // @objectId does not arm any field-decode need; no WHERE either.
        let plan = plan_query(&parse("SELECT @objectId FROM C").unwrap()).unwrap();
        let text = plan.explain();
        assert!(text.contains("needs (armed): none"), "got: {text}");
    }

    #[test]
    fn rejects_nested_aggregate() {
        let err = plan_query(&parse("SELECT COUNT(SUM(x)) FROM C").unwrap()).unwrap_err();
        assert!(
            err.0.to_lowercase().contains("aggregate"),
            "got: {}",
            err.0
        );
    }

    #[test]
    fn aggregate_with_where_is_single_scan() {
        // An aggregate that also filters cannot use the pre-built histogram.
        let plan = plan_query(
            &parse("SELECT COUNT(*) FROM C WHERE count > 1").unwrap(),
        )
        .unwrap();
        assert_eq!(plan.kind, StageKind::SingleScan);
        assert!(!plan.needs.histogram);
    }
}
