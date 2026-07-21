//! Needs analysis + planning for the supported OQL subset. Cost is per-need:
//! each flag arms exactly one piece of machinery. Deferred constructs are
//! rejected here (not in the parser) with a message naming the construct.

use crate::query::ast::{Attr, Predicate, Query, SelectItem, Value};
use crate::query::carry::CarryLayout;
use crate::query::QueryError;

/// Default cap on late-phase emitted rows (dominator children) and retained-set
/// closures, mirroring the scan-time `DEFAULT_CARRY_CAP`. Bounds late output so
/// a pathological query can't blow up memory in the retained-live window.
pub const DEFAULT_LATE_CAP: usize = 1_000_000;
pub const DEFAULT_RETAINED_CAP: usize = 1_000_000;

/// Per-need cost flags. Each flag independently arms exactly one piece of
/// machinery; an unset flag arms nothing. (Foundation subset — ref/retained/
/// dominator/edge needs are added in later slices.)
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QueryNeeds {
    pub histogram: bool,
    pub instance_scalar: bool,
    pub instance_string: bool,
    pub runtime_type: bool,
    pub retained: bool,
    /// Arms the dominator-children CSR (dc_off/dc_tgt) in the late phase, for
    /// `dominators(x)` / `dominatorof(x)` / `AS RETAINED SET`.
    pub dominator_children: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    HistogramOnly,
    SingleScan,
}

/// Which pipeline phase finalizes a query's rows. See canonical vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    P1,
    // P2 is constructed by later rollout phases (RefWalk); reserved here.
    #[allow(dead_code)]
    P2,
    P3,
}

/// A late-phase operation applied when resuming a cross-phase query.
/// (Extended with more variants in later phases.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageOp {
    /// Join each carried dense index against `retained`, then apply retained
    /// WHERE terms, ORDER BY, and LIMIT.
    JoinRetained,
    /// Emit the dominator-tree children of each carried dense index (bounded by
    /// `cap`). Backs `dominators(x)`.
    DominatorChildren { cap: usize },
    /// Emit the immediate dominator (idom) of each carried dense index — one row
    /// per input (the tree root has no idom and yields nothing). Backs
    /// `dominatorof(x)`.
    DominatorOf,
    /// Bounded DFS over the dominator-children CSR from each carried index,
    /// emitting the retained closure. Backs `SELECT ... AS RETAINED SET`.
    RetainedSet { cap: usize },
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
    pub finalize_at: Phase,
    /// Scan-time carry layout for cross-phase queries. `IndexOnly` for the
    /// current retained/dominator stages (only dense indices are carried).
    pub carry: CarryLayout,
    pub late_ops: Vec<StageOp>,
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

    // `AS RETAINED SET`: expand each match to its dominator-retained closure.
    // Incompatible with aggregates (there is no closure of an aggregate scalar).
    if q.retained_set {
        if is_aggregate {
            return Err(QueryError(
                "RETAINED SET cannot be combined with aggregate functions; \
                 SELECT the objects (e.g. SELECT s AS RETAINED SET FROM ... s), \
                 not an aggregate over them"
                    .into(),
            ));
        }
        needs.dominator_children = true;
        return Ok(QueryPlan {
            kind: StageKind::SingleScan,
            needs,
            where_terms,
            finalize_at: Phase::P3,
            carry: CarryLayout::IndexOnly,
            late_ops: vec![StageOp::RetainedSet { cap: DEFAULT_RETAINED_CAP }],
            limit: q.limit,
        });
    }

    // `dominators(alias)`: dominator-tree children of each matched object. The
    // sole argument must name the FROM alias; anything else is a hard error.
    if let [SelectItem::Attr(Attr::Dominators(a) | Attr::DominatorOf(a))] = q.select.as_slice() {
        if Some(a.as_str()) != q.alias.as_deref() {
            return Err(QueryError(format!(
                "unknown alias '{a}'; the FROM clause binds {}",
                match &q.alias {
                    Some(al) => format!("alias '{al}'"),
                    None => "no alias".to_string(),
                }
            )));
        }
        needs.dominator_children = true;
        let op = match &q.select[0] {
            SelectItem::Attr(Attr::DominatorOf(_)) => StageOp::DominatorOf,
            _ => StageOp::DominatorChildren { cap: DEFAULT_LATE_CAP },
        };
        return Ok(QueryPlan {
            kind: StageKind::SingleScan,
            needs,
            where_terms,
            finalize_at: Phase::P3,
            carry: CarryLayout::IndexOnly,
            late_ops: vec![op],
            limit: q.limit,
        });
    }

    let cross_phase = uses_retained(q);
    if cross_phase {
        needs.retained = true;
    }
    let (finalize_at, late_ops) = if cross_phase {
        (Phase::P3, vec![StageOp::JoinRetained])
    } else {
        (Phase::P1, Vec::new())
    };

    Ok(QueryPlan {
        kind,
        needs,
        where_terms,
        finalize_at,
        carry: CarryLayout::IndexOnly,
        late_ops,
        limit: q.limit,
    })
}

fn uses_retained(q: &Query) -> bool {
    let in_select = q.select.iter().any(select_uses_retained);
    let in_where = q.where_.as_ref().map(pred_uses_retained).unwrap_or(false);
    let in_order = matches!(&q.order_by, Some(ob) if ob.key == Attr::RetainedHeapSize);
    in_select || in_where || in_order
}
fn select_uses_retained(it: &SelectItem) -> bool {
    match it {
        SelectItem::Attr(Attr::RetainedHeapSize) => true,
        SelectItem::Aggregate { arg, .. } => select_uses_retained(arg),
        _ => false,
    }
}
/// True if a predicate references `@retainedHeapSize` anywhere. Reused by the
/// scan-time carry executor to skip retained WHERE terms (retained size is
/// unknown during the pass2 scan; those terms are applied late in stage_runner).
pub(crate) fn pred_uses_retained(p: &Predicate) -> bool {
    match p {
        Predicate::And(a, b) | Predicate::Or(a, b) => pred_uses_retained(a) || pred_uses_retained(b),
        Predicate::Not(a) => pred_uses_retained(a),
        Predicate::Compare { lhs: Attr::RetainedHeapSize, .. } => true,
        _ => false,
    }
}

/// Schema lookup used for plan-time field validation. Implemented by the live
/// pass2 resolver; a fake version backs the unit tests. `class_field_names`
/// returns the full super-chain field-name set for an EXACT (non-glob) class
/// name, or `None` when the class is unknown or the name is a glob pattern
/// (in which case field validation is skipped, since the concrete runtime
/// classes vary per instance).
pub trait FieldSchema {
    fn class_field_names(&self, exact_class_name: &str) -> Option<Vec<String>>;
}

/// Reject any bare field referenced in SELECT/WHERE/ORDER BY that is absent
/// from the FROM class's field set. Skipped for glob FROM patterns and for
/// classes the schema can't resolve (validation is best-effort: an unresolvable
/// class means we can't prove a field is missing, so we let the scan proceed).
pub fn validate_fields(q: &Query, schema: &dyn FieldSchema) -> Result<(), QueryError> {
    let class = &q.from.class_name;
    if class.contains('*') {
        return Ok(());
    }
    let Some(known) = schema.class_field_names(class) else {
        return Ok(());
    };

    let mut referenced = Vec::new();
    for item in &q.select {
        collect_select_fields(item, &mut referenced);
    }
    if let Some(pred) = &q.where_ {
        collect_pred_fields(pred, &mut referenced);
    }
    if let Some(ob) = &q.order_by {
        if let Attr::Field(name) = &ob.key {
            referenced.push(name.clone());
        }
    }

    for name in referenced {
        let bare = strip_alias(&name, q.alias.as_deref());
        if !known.iter().any(|f| f == bare) {
            return Err(QueryError(format!(
                "unknown field `{bare}` on {class}; \
                 known fields: {}",
                if known.is_empty() {
                    "(none)".to_string()
                } else {
                    known.join(", ")
                }
            )));
        }
    }
    Ok(())
}

fn strip_alias<'n>(name: &'n str, alias: Option<&str>) -> &'n str {
    if let Some(a) = alias {
        if let Some(rest) = name.strip_prefix(a) {
            if let Some(field) = rest.strip_prefix('.') {
                return field;
            }
        }
    }
    name
}

fn collect_select_fields(item: &SelectItem, out: &mut Vec<String>) {
    match item {
        SelectItem::Attr(Attr::Field(name)) => out.push(name.clone()),
        SelectItem::Aggregate { arg, .. } => collect_select_fields(arg, out),
        _ => {}
    }
}

fn collect_pred_fields(pred: &Predicate, out: &mut Vec<String>) {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            collect_pred_fields(a, out);
            collect_pred_fields(b, out);
        }
        Predicate::Not(a) => collect_pred_fields(a, out),
        Predicate::Compare { lhs: Attr::Field(name), .. } => out.push(name.clone()),
        _ => {}
    }
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
        if self.needs.retained { armed.push("retained"); }
        s.push_str(&format!(
            "needs (armed): {}\n",
            if armed.is_empty() { "none".into() } else { armed.join(", ") }
        ));
        s.push_str(&format!("finalize: {:?}\n", self.finalize_at));
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
    fn retained_heap_size_now_parses() {
        assert!(parse("SELECT @retainedHeapSize FROM C").is_ok());
    }

    #[test]
    fn retained_in_select_sets_retained_need_and_p3_finalize() {
        let plan = plan_query(&parse("SELECT @retainedHeapSize FROM C").unwrap()).unwrap();
        assert!(plan.needs.retained, "SELECT @retainedHeapSize must arm the retained need");
        assert_eq!(plan.finalize_at, Phase::P3);
        assert_eq!(plan.late_ops, vec![StageOp::JoinRetained]);
    }
    #[test]
    fn retained_in_where_is_cross_phase() {
        let plan = plan_query(&parse("SELECT @objectId FROM C WHERE @retainedHeapSize > 1024").unwrap()).unwrap();
        assert!(plan.needs.retained);
        assert_eq!(plan.finalize_at, Phase::P3);
    }
    #[test]
    fn retained_in_order_by_is_cross_phase() {
        let plan = plan_query(&parse("SELECT @objectId FROM C ORDER BY @retainedHeapSize DESC").unwrap()).unwrap();
        assert!(plan.needs.retained);
        assert_eq!(plan.finalize_at, Phase::P3);
        assert_eq!(plan.late_ops, vec![StageOp::JoinRetained]);
    }
    #[test]
    fn non_retained_query_finalizes_in_p1() {
        let plan = plan_query(&parse("SELECT @objectId FROM C WHERE count > 3").unwrap()).unwrap();
        assert!(!plan.needs.retained);
        assert_eq!(plan.finalize_at, Phase::P1);
        assert!(plan.late_ops.is_empty());
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
    fn plan_dominators_emits_dominator_children_stage() {
        let plan = plan_query(&parse("SELECT dominators(s) FROM java.lang.String s").unwrap()).unwrap();
        assert!(matches!(plan.carry, CarryLayout::IndexOnly));
        assert_eq!(plan.late_ops.len(), 1);
        assert!(matches!(plan.late_ops[0], StageOp::DominatorChildren { .. }));
        assert_eq!(plan.finalize_at, Phase::P3);
        assert!(plan.needs.dominator_children);
    }
    #[test]
    fn plan_dominators_unknown_alias_rejected() {
        let err = plan_query(&parse("SELECT dominators(x) FROM java.lang.String s").unwrap()).unwrap_err();
        assert!(err.to_string().contains("unknown alias 'x'"), "got: {err}");
    }
    #[test]
    fn plan_dominatorof_emits_dominator_of_stage() {
        let plan = plan_query(&parse("SELECT dominatorof(s) FROM java.lang.String s").unwrap()).unwrap();
        assert_eq!(plan.late_ops.len(), 1);
        assert!(matches!(plan.late_ops[0], StageOp::DominatorOf));
        assert_eq!(plan.finalize_at, Phase::P3);
        assert!(plan.needs.dominator_children);
    }
    #[test]
    fn plan_retained_set_emits_retained_set_stage() {
        let plan = plan_query(&parse("SELECT s AS RETAINED SET FROM java.lang.String s").unwrap()).unwrap();
        assert!(matches!(plan.late_ops[0], StageOp::RetainedSet { .. }));
        assert_eq!(plan.finalize_at, Phase::P3);
        assert!(plan.needs.dominator_children);
    }
    #[test]
    fn plan_retained_set_with_aggregate_rejected() {
        let err = plan_query(&parse("SELECT count(s) AS RETAINED SET FROM java.lang.String s").unwrap()).unwrap_err();
        assert!(err.to_string().contains("RETAINED SET cannot be combined with aggregate"), "got: {err}");
    }

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

    // --- Field validation (unknown-field rejection) ---

    struct FakeSchema {
        class: &'static str,
        fields: Vec<&'static str>,
    }
    impl FieldSchema for FakeSchema {
        fn class_field_names(&self, exact_class_name: &str) -> Option<Vec<String>> {
            if exact_class_name.replace('/', ".") == self.class {
                Some(self.fields.iter().map(|s| s.to_string()).collect())
            } else {
                None
            }
        }
    }

    #[test]
    fn validate_accepts_known_field() {
        let schema = FakeSchema { class: "java.lang.String", fields: vec!["count", "hash", "value"] };
        let q = parse("SELECT count FROM java.lang.String WHERE hash > 0").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_select_field() {
        let schema = FakeSchema { class: "java.lang.String", fields: vec!["count", "hash"] };
        let q = parse("SELECT bogusfield FROM java.lang.String").unwrap();
        let err = validate_fields(&q, &schema).unwrap_err();
        assert!(err.0.contains("unknown field"), "got: {}", err.0);
        assert!(err.0.contains("bogusfield"), "got: {}", err.0);
        assert!(err.0.contains("java.lang.String"), "got: {}", err.0);
        // Actionable: lists the known fields.
        assert!(err.0.contains("count"), "should list known fields: {}", err.0);
    }

    #[test]
    fn validate_rejects_unknown_where_field() {
        let schema = FakeSchema { class: "java.lang.String", fields: vec!["count"] };
        let q = parse("SELECT * FROM java.lang.String WHERE nope > 3").unwrap();
        let err = validate_fields(&q, &schema).unwrap_err();
        assert!(err.0.contains("unknown field"), "got: {}", err.0);
        assert!(err.0.contains("nope"), "got: {}", err.0);
    }

    #[test]
    fn validate_strips_alias_before_lookup() {
        let schema = FakeSchema { class: "java.lang.String", fields: vec!["count", "hash"] };
        // `s.count`/`s.hash` must resolve as bare `count`/`hash`.
        let q = parse("SELECT s.count FROM java.lang.String s WHERE s.hash > 0").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
        // Alias-stripped unknown field is still rejected, reported bare.
        let q2 = parse("SELECT s.bogus FROM java.lang.String s").unwrap();
        let err = validate_fields(&q2, &schema).unwrap_err();
        assert!(err.0.contains("unknown field `bogus`"), "got: {}", err.0);
    }

    #[test]
    fn validate_rejects_unknown_order_by_field() {
        let schema = FakeSchema { class: "java.lang.String", fields: vec!["count", "hash"] };
        let q = parse("SELECT * FROM java.lang.String ORDER BY bogus").unwrap();
        let err = validate_fields(&q, &schema).unwrap_err();
        assert!(err.0.contains("unknown field"), "got: {}", err.0);
        assert!(err.0.contains("bogus"), "got: {}", err.0);
    }

    #[test]
    fn validate_accepts_known_order_by_field() {
        let schema = FakeSchema { class: "java.lang.String", fields: vec!["count", "hash"] };
        let q = parse("SELECT * FROM java.lang.String ORDER BY count DESC").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
    }

    #[test]
    fn validate_skips_glob_from() {
        // Glob FROM classes vary per instance; field validation is skipped.
        let schema = FakeSchema { class: "irrelevant", fields: vec![] };
        let q = parse("SELECT anything FROM com.acme.*").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
    }

    #[test]
    fn validate_skips_unresolvable_class() {
        // Unknown class → schema returns None → we can't prove a field missing.
        let schema = FakeSchema { class: "java.lang.String", fields: vec!["count"] };
        let q = parse("SELECT whatever FROM com.other.Unknown").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
    }

    #[test]
    fn validate_ignores_builtin_attrs() {
        // @-attrs are not bare fields and must never be flagged.
        let schema = FakeSchema { class: "java.lang.String", fields: vec![] };
        let q = parse("SELECT @objectId, @usedHeapSize, @displayName FROM java.lang.String").unwrap();
        assert!(validate_fields(&q, &schema).is_ok());
    }
}
