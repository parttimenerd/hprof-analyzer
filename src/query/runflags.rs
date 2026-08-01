//! Per-run edge-retention gating.
//!
//! Edge-using OQL features (`@inbounds`, `@outbounds`, `path(a, b)`) are
//! MEMORY-CRITICAL: retaining the forward-reference CSR and/or an inbound index
//! costs multiple GB of RSS. So retention is opt-in *per run*: before pass2 we
//! scan every query (and every UNION branch) and compute a [`RunFlags`] that
//! records exactly which edge structures must be built and which class rows
//! must be kept. A run whose queries use no edge feature produces the default
//! all-off flags and is byte-for-byte / RSS-identical to a no-query run.

// The public API here is consumed by main.rs (Task 41) and the edge executor
// (Task 40); until those land, the items are dead from the binary's view.
#![allow(dead_code)]

use crate::query::QueryError;
use crate::query::ast::{Attr, Expr, Predicate, Query, SelectItem};

/// Direction of an edge feature. Kept as a small typed enum so callers that
/// walk edges do not thread bare bools around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum EdgeDir {
    Inbound,
    Outbound,
}

/// A dense-class-index membership set, sized to the class universe. Reuses the
/// project's compact [`crate::bitset::Bitset`] (1 bit per class).
pub type ClassBitset = crate::bitset::Bitset;

/// The edge structures a run must build, plus which class rows to retain.
///
/// All fields default to "retain nothing" so a run with no edge-using query is
/// identical to today. Flags are the OR across every query and UNION branch;
/// [`retain_rows`] is the union of the FROM-class bits of *only* the queries
/// that actually use an edge feature.
#[derive(Debug, Clone, Default)]
pub struct RunFlags {
    /// Build the inbound reference index (`@inbounds`).
    pub retain_inbound: bool,
    /// Retain the forward-reference CSR for bounded subgraph walks (`path(a, b)`).
    pub retain_forward: bool,
    /// Dense class-index bits whose rows must be retained for edge resolution.
    /// `None` means no query used any edge feature — retain nothing.
    pub retain_rows: Option<ClassBitset>,
    /// `@outbounds` is answered by a targeted rescan (L3) rather than by keeping
    /// the inbound index; this flag alone does NOT force `retain_inbound`.
    pub outbounds_by_rescan: bool,
}

impl RunFlags {
    /// A one-line, human-readable disclosure of which edge structures this run
    /// retains (and roughly the memory cost implied), or `None` when no edge
    /// feature is used (the zero-cost path). Surfaced to the user so an edge query
    /// makes its added RSS cost visible.
    pub fn retention_note(&self) -> Option<String> {
        let mut frags: Vec<&str> = Vec::new();
        if self.retain_inbound {
            frags.push("inbound reference index");
        }
        if self.retain_forward {
            frags.push("forward reference graph rows");
        }
        if self.outbounds_by_rescan {
            frags.push("outbound edges via rescan");
        }
        if frags.is_empty() {
            return None;
        }
        Some(format!(
            "edge query: retaining {} (added heap cost)",
            frags.join(", ")
        ))
    }
}

/// Resolves a FROM class name/pattern (possibly a glob such as `com.acme.*`,
/// and possibly `INSTANCEOF`) to the dense class-index bits it matches, and
/// reports the total class count for sizing the bitset. Abstracts over the real
/// class table so planning can be unit-tested with a fake.
pub trait ClassIndexResolver {
    /// Dense class indices matched by `from_class_pattern`. `instanceof` selects
    /// subclass-inclusive matching. Unknown names yield an empty vec (the query
    /// simply matches nothing, which is not an edge-retention error).
    fn class_bits(&self, from_class_pattern: &str, instanceof: bool) -> Vec<usize>;
    /// Total number of dense classes — the bitset length.
    fn universe_len(&self) -> usize;
}

/// Compute the edge-retention flags for a whole run (all top-level queries).
///
/// Scans each query and its UNION branches for edge features and unions the
/// results. Only edge-using queries contribute their FROM-class bits to
/// [`RunFlags::retain_rows`]; a run with no edge feature returns the default
/// flags (`retain_rows: None`).
pub fn plan_run(
    queries: &[Query],
    class_index: &dyn ClassIndexResolver,
    path_depth_cap: usize,
) -> Result<RunFlags, QueryError> {
    if path_depth_cap == 0 {
        return Err(QueryError(
            "--query-path-depth must be > 0 (bounded path walks need at least one hop)".into(),
        ));
    }

    let mut flags = RunFlags::default();
    for q in queries {
        accumulate_query(q, class_index, &mut flags);
        // A UNION branch can independently use edges, so scan each branch as its
        // own query (branches are flat: their own `union_branches` is empty).
        for branch in &q.union_branches {
            accumulate_query(branch, class_index, &mut flags);
        }
    }
    Ok(flags)
}

/// What edge features a single query (one branch) uses.
#[derive(Default)]
struct BranchUse {
    inbound: bool,
    outbound: bool,
    forward: bool,
}

impl BranchUse {
    fn any(&self) -> bool {
        self.inbound || self.outbound || self.forward
    }
}

/// Scan one query/branch, OR its flags into `flags`, and — if it uses any edge
/// feature — union its FROM-class bits into `flags.retain_rows`.
fn accumulate_query(q: &Query, class_index: &dyn ClassIndexResolver, flags: &mut RunFlags) {
    let mut used = BranchUse::default();

    for item in &q.select {
        scan_select_item(item, &mut used);
    }
    if let Some(pred) = &q.where_ {
        scan_predicate(pred, &mut used);
    }

    flags.retain_inbound |= used.inbound;
    flags.outbounds_by_rescan |= used.outbound;
    flags.retain_forward |= used.forward;

    // L1: only edge-using queries keep their FROM rows.
    if used.any() {
        let bits = class_index.class_bits(q.from.class_name(), q.from.instanceof());
        if !bits.is_empty() || flags.retain_rows.is_some() {
            let set = flags
                .retain_rows
                .get_or_insert_with(|| ClassBitset::with_len(class_index.universe_len()));
            for b in bits {
                set.set(b);
            }
        } else {
            // No matched bits and no prior set: still mark that an edge query ran
            // by allocating the (empty) bitset, so callers can distinguish
            // "edges requested but class unknown" from "no edges at all".
            flags
                .retain_rows
                .get_or_insert_with(|| ClassBitset::with_len(class_index.universe_len()));
        }
    }
}

/// Inspect a SELECT item for edge features. Recurses into aggregate arguments.
fn scan_select_item(item: &SelectItem, used: &mut BranchUse) {
    match item {
        SelectItem::Attr(a) => scan_attr(a, used),
        SelectItem::Aggregate { arg, .. } => scan_select_item(arg, used),
        // A bounded forward subgraph walk needs the forward-reference CSR.
        // FROM-seed rows suffice; no `to`-class retention is needed because
        // `to`-operand early-stop is not armed (parity-lite).
        SelectItem::Path { .. } => used.forward = true,
        SelectItem::Star => {}
        // toString(s) uses the string-values side table, not edge CSRs.
        SelectItem::ToString(_) => {}
        // An arithmetic/method-call column: recurse into the expression so a
        // buried @inbounds/@outbounds still arms its edge index.
        SelectItem::Expr(e) => scan_expr(e, used),
    }
}

/// Inspect an arithmetic expression tree for edge-feature attrs on any leaf.
fn scan_expr(e: &Expr, used: &mut BranchUse) {
    match e {
        Expr::Attr(a) => scan_attr(a, used),
        Expr::Lit(_) => {}
        Expr::Binary { lhs, rhs, .. } => {
            scan_expr(lhs, used);
            scan_expr(rhs, used);
        }
        Expr::Unary { arg, .. } => scan_expr(arg, used),
        Expr::Method { receiver, args, .. } => {
            scan_expr(receiver, used);
            for a in args {
                scan_expr(a, used);
            }
        }
        Expr::Aggregate { .. } => {} // no edge-feature attrs in aggregate
        Expr::Case { branches, else_ } => {
            for (pred, then_e) in branches {
                scan_predicate(pred, used);
                scan_expr(then_e, used);
            }
            if let Some(e) = else_ {
                scan_expr(e, used);
            }
        }
        Expr::Coalesce(args) => {
            for arg in args {
                scan_expr(arg, used);
            }
        }
        Expr::NullIf { lhs, rhs } => {
            scan_expr(lhs, used);
            scan_expr(rhs, used);
        }
    }
}

/// Inspect a WHERE predicate tree for edge features on its comparison LHS attrs.
fn scan_predicate(pred: &Predicate, used: &mut BranchUse) {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            scan_predicate(a, used);
            scan_predicate(b, used);
        }
        Predicate::Not(a) => scan_predicate(a, used),
        Predicate::Compare { lhs, rhs, .. } => {
            scan_expr(lhs, used);
            scan_expr(rhs, used);
        }
        // Edge usage inside an IN-subquery's inner query targets a *different*
        // FROM class; scoping that correctly is out of scope for this task, so
        // we only inspect the outer `lhs` here.
        Predicate::InSubquery { lhs, .. } => scan_attr(lhs, used),
        Predicate::InstanceOf(_) => {}
        // EXISTS inner is a standalone query; no outer edge attrs to scan.
        Predicate::Exists { .. } => {}
    }
}

/// Flag the edge feature (if any) that this attribute uses.
fn scan_attr(a: &Attr, used: &mut BranchUse) {
    match a {
        Attr::Inbounds => used.inbound = true,
        Attr::Outbounds => used.outbound = true,
        // RefPath is a separate mechanism (captured hop fields), not edge
        // retention; ignore it here.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse::parse;

    /// Deterministic fake class table for planning tests.
    ///
    /// Direct-match bits: `java.lang.String` -> 0, `java.lang.Integer` -> 1.
    /// INSTANCEOF `java.lang.Object` -> {0, 1}. A glob `java.lang.*` -> {0, 1}.
    /// Universe is 8 classes.
    struct FakeClassIndex;

    impl ClassIndexResolver for FakeClassIndex {
        fn class_bits(&self, from_class_pattern: &str, instanceof: bool) -> Vec<usize> {
            if instanceof && from_class_pattern == "java.lang.Object" {
                return vec![0, 1];
            }
            match from_class_pattern {
                "java.lang.String" => vec![0],
                "java.lang.Integer" => vec![1],
                "java.lang.*" => vec![0, 1],
                _ => vec![],
            }
        }
        fn universe_len(&self) -> usize {
            8
        }
    }

    fn fake_class_index() -> FakeClassIndex {
        FakeClassIndex
    }

    fn plan(src: &str) -> RunFlags {
        let q = parse(src).unwrap();
        plan_run(&[q], &fake_class_index(), 8).unwrap()
    }

    #[test]
    fn no_edge_query_yields_empty_flags() {
        let f = plan("SELECT * FROM java.lang.String");
        assert!(!f.retain_inbound);
        assert!(!f.retain_forward);
        assert!(!f.outbounds_by_rescan);
        assert!(f.retain_rows.is_none());
    }

    #[test]
    fn outbounds_only_uses_rescan_not_inbound() {
        let f = plan("SELECT @outbounds FROM java.lang.String");
        assert!(f.outbounds_by_rescan);
        assert!(!f.retain_inbound);
        assert!(!f.retain_forward);
        // Edge-using query -> its FROM row is retained.
        assert!(f.retain_rows.is_some());
        assert!(f.retain_rows.as_ref().unwrap().get(0));
    }

    #[test]
    fn inbounds_retains_inbound_index() {
        let f = plan("SELECT @inbounds FROM java.lang.String");
        assert!(f.retain_inbound);
        assert!(!f.outbounds_by_rescan);
        assert!(f.retain_rows.is_some());
        assert!(f.retain_rows.as_ref().unwrap().get(0));
    }

    #[test]
    fn path_sets_retain_forward() {
        let f = plan("SELECT path(s, java.lang.Integer) FROM java.lang.String s");
        assert!(f.retain_forward);
        assert!(!f.retain_inbound);
        assert!(f.retain_rows.is_some());
        assert!(f.retain_rows.as_ref().unwrap().get(0));
    }

    #[test]
    fn flags_union_across_queries() {
        let q1 = parse("SELECT @inbounds FROM java.lang.String").unwrap();
        let q2 = parse("SELECT @outbounds FROM java.lang.Integer").unwrap();
        let f = plan_run(&[q1, q2], &fake_class_index(), 8).unwrap();
        assert!(f.retain_inbound);
        assert!(f.outbounds_by_rescan);
        let rows = f.retain_rows.expect("edge queries must retain rows");
        assert!(rows.get(0), "String bit set");
        assert!(rows.get(1), "Integer bit set");
    }

    #[test]
    fn inbounds_in_where_counts() {
        let f = plan("SELECT * FROM java.lang.String WHERE @inbounds > 3");
        assert!(f.retain_inbound, "WHERE traversal must detect @inbounds");
        assert!(f.retain_rows.is_some());
        assert!(f.retain_rows.as_ref().unwrap().get(0));
    }

    #[test]
    fn union_branch_edge_usage_counts() {
        let f =
            plan("SELECT * FROM java.lang.String UNION SELECT @inbounds FROM java.lang.Integer");
        assert!(
            f.retain_inbound,
            "UNION branch traversal must detect @inbounds"
        );
        let rows = f.retain_rows.expect("edge branch must retain rows");
        // The non-edge lead branch (String, bit 0) must NOT be retained.
        assert!(!rows.get(0), "non-edge lead branch must not retain rows");
        assert!(rows.get(1), "edge UNION branch retains its FROM (Integer)");
    }

    #[test]
    fn path_depth_zero_is_error() {
        let q = parse("SELECT * FROM java.lang.String").unwrap();
        let err = plan_run(&[q], &fake_class_index(), 0).unwrap_err();
        assert!(!err.0.is_empty());
        assert!(
            err.0.contains("depth"),
            "error should mention depth, got: {}",
            err.0
        );
    }

    #[test]
    fn no_edge_run_retains_no_rows() {
        let f = plan("SELECT @displayName FROM java.lang.String WHERE count > 3");
        assert!(f.retain_rows.is_none());
        assert!(!f.retain_inbound);
        assert!(!f.retain_forward);
        assert!(!f.outbounds_by_rescan);
    }

    #[test]
    fn instanceof_from_unions_all_matched_bits() {
        let f = plan("SELECT @inbounds FROM INSTANCEOF java.lang.Object");
        assert!(f.retain_inbound);
        let rows = f.retain_rows.expect("edge query retains rows");
        assert!(rows.get(0));
        assert!(rows.get(1));
    }

    #[test]
    fn retention_note_describes_what_is_kept() {
        let f = plan("SELECT @inbounds FROM java.lang.String");
        let note = f.retention_note().expect("edge query discloses retention");
        assert!(
            note.contains("inbound"),
            "note should mention inbound: {note}"
        );

        let none = plan("SELECT * FROM java.lang.String");
        assert!(
            none.retention_note().is_none(),
            "no-edge run discloses nothing"
        );
    }

    #[test]
    fn retention_note_none_for_no_edge_run() {
        let f = plan("SELECT @displayName FROM java.lang.String WHERE count > 3");
        assert!(f.retention_note().is_none());
    }

    #[test]
    fn retention_note_mentions_forward_for_path() {
        let f = plan("SELECT path(s, java.lang.Integer) FROM java.lang.String s");
        let note = f.retention_note().expect("path query discloses retention");
        assert!(
            note.contains("forward"),
            "note should mention forward: {note}"
        );
    }

    #[test]
    fn retention_note_mentions_outbound() {
        let f = plan("SELECT @outbounds FROM java.lang.String");
        let note = f
            .retention_note()
            .expect("outbound query discloses retention");
        assert!(
            note.contains("outbound"),
            "note should mention outbound: {note}"
        );
    }

    #[test]
    fn retention_note_is_single_line() {
        let f = plan("SELECT @inbounds FROM java.lang.String");
        let note = f.retention_note().expect("edge query discloses retention");
        assert!(!note.is_empty(), "note must be non-empty");
        assert!(!note.contains('\n'), "note must be a single line: {note:?}");
    }

    #[test]
    fn retention_note_combines_multiple() {
        let q1 = parse("SELECT @inbounds FROM java.lang.String").unwrap();
        let q2 = parse("SELECT @outbounds FROM java.lang.Integer").unwrap();
        let f = plan_run(&[q1, q2], &fake_class_index(), 8).unwrap();
        let note = f.retention_note().expect("edge queries disclose retention");
        assert!(
            note.contains("inbound"),
            "note should mention inbound: {note}"
        );
        assert!(
            note.contains("outbound"),
            "note should mention outbound: {note}"
        );
    }

    #[test]
    fn edgedir_is_copy() {
        // Compile-time check that EdgeDir is a trivially-copyable typed enum.
        let d = EdgeDir::Inbound;
        let d2 = d;
        assert_eq!(d, d2);
        assert_ne!(EdgeDir::Inbound, EdgeDir::Outbound);
    }

    #[test]
    fn method_call_select_item_does_not_panic() {
        // A method-call expression as a SELECT item parses to SelectItem::Expr;
        // scanning it for edge features must not panic (regression: this hit an
        // `unreachable!` and crashed the full analyze pipeline).
        let f = plan("SELECT s.name.toString() FROM java.lang.String s");
        // No edge feature -> no retention.
        assert!(!f.retain_inbound);
        assert!(!f.outbounds_by_rescan);
        assert!(!f.retain_forward);
    }

    #[test]
    fn arithmetic_select_item_does_not_panic() {
        let f = plan("SELECT @usedHeapSize * 2 FROM java.lang.String");
        assert!(!f.retain_inbound);
        assert!(!f.outbounds_by_rescan);
    }

    #[test]
    fn edge_attr_inside_expr_select_item_counts() {
        // @inbounds buried inside an arithmetic SELECT expression must still arm
        // the inbound index and retain the FROM row.
        let f = plan("SELECT @inbounds + 1 FROM java.lang.String");
        assert!(f.retain_inbound, "expr-wrapped @inbounds must arm inbound");
        assert!(f.retain_rows.is_some());
        assert!(f.retain_rows.as_ref().unwrap().get(0));
    }

    #[test]
    fn edge_attr_in_wrapped_where_compare_counts() {
        // @outbounds inside an arithmetic WHERE comparison operand (not a bare
        // Expr::Attr) must still be detected on both sides of the compare.
        let f = plan("SELECT * FROM java.lang.String WHERE @outbounds + 0 > 3");
        assert!(
            f.outbounds_by_rescan,
            "wrapped @outbounds in WHERE must arm rescan"
        );
    }
}
